//! Global push-to-talk hotkey. Emits Pressed/Released over std mpsc.
//!
//! Windows registers the whole chord with `RegisterHotKey`, then polls only
//! while it is held using a 10 ms window timer. Other platforms use
//! `global-hotkey` directly.

#[cfg(not(target_os = "windows"))]
use std::sync::mpsc::{self, Receiver};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Pressed,
    Released,
}

/// Push-to-talk presets shown in the tray menu. All use Space (hard to
/// fat-finger while typing); modifiers differ. Unknown strings fall back
/// to the default so the app never boots without push-to-talk.
pub const PRESETS: &[&str] = &["Ctrl+Space", "Alt+Space", "Ctrl+Shift+Space"];

/// Normalize user input ("ctrl + shift + space") to a canonical preset.
pub fn normalize(want: &str) -> &'static str {
    let w: String = want.chars().filter(|c| !c.is_whitespace()).collect();
    let w = w.to_ascii_lowercase();
    let has = |s: &str| w.contains(s);
    if has("space") {
        if has("ctrl") && has("shift") {
            return PRESETS[2];
        }
        if has("alt") && !has("ctrl") {
            return PRESETS[1];
        }
        if has("ctrl") {
            return PRESETS[0];
        }
    }
    PRESETS[0]
}

#[cfg(not(target_os = "windows"))]
fn to_hotkey(preset: &str) -> global_hotkey::hotkey::HotKey {
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};

    let p = normalize(preset);
    let mut mods = Modifiers::empty();
    let lower = p.to_ascii_lowercase();
    if lower.contains("ctrl") {
        mods |= Modifiers::CONTROL;
    }
    if lower.contains("alt") {
        mods |= Modifiers::ALT;
    }
    if lower.contains("shift") {
        mods |= Modifiers::SHIFT;
    }
    HotKey::new(Some(mods), Code::Space)
}

#[cfg(not(target_os = "windows"))]
pub struct Hotkey {
    manager: global_hotkey::GlobalHotKeyManager,
    key: global_hotkey::hotkey::HotKey,
    pub rx: Receiver<KeyEvent>,
}

#[cfg(not(target_os = "windows"))]
impl Hotkey {
    /// Register a preset (see PRESETS). Unknown strings fall back to default.
    pub fn register(want: &str) -> Result<Self, String> {
        use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};

        let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
        let key = to_hotkey(want);
        manager.register(key).map_err(|e| {
            #[cfg(target_os = "macos")]
            let hint = "need accessibility/input permission on macOS";
            #[cfg(target_os = "linux")]
            let hint = "another app may hold this combo — try another preset";
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            let hint = "check input permission / conflicting hotkeys";
            format!("hotkey register ({hint}): {e}")
        })?;
        let (tx, rx) = mpsc::channel::<KeyEvent>();
        let global_rx = GlobalHotKeyEvent::receiver();
        std::thread::Builder::new()
            .name("utterly-hotkey".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                use global_hotkey::HotKeyState;
                while let Ok(ev) = global_rx.recv() {
                    let _ = tx.send(match ev.state {
                        HotKeyState::Pressed => KeyEvent::Pressed,
                        HotKeyState::Released => KeyEvent::Released,
                    });
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { manager, key, rx })
    }

    /// Switch preset at runtime. On failure, restore the old hotkey.
    pub fn set(&mut self, preset: &str) -> Result<(), String> {
        let new_key = to_hotkey(preset);
        let _ = self.manager.unregister(self.key);
        match self.manager.register(new_key) {
            Ok(()) => {
                self.key = new_key;
                Ok(())
            }
            Err(e) => {
                let _ = self.manager.register(self.key);
                Err(e.to_string())
            }
        }
    }

    pub fn try_event(&self) -> Option<KeyEvent> {
        self.rx.try_recv().ok()
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::{KeyEvent, PRESETS};
    use std::ffi::c_void;
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    const WM_QUIT: u32 = 0x0012;
    const WM_TIMER: u32 = 0x0113;
    const WM_HOTKEY: u32 = 0x0312;
    const WM_APP_SET_HOTKEY: u32 = 0x8001;
    const HOTKEY_ID: i32 = 1;
    const TIMER_ID: usize = 1;
    const MOD_ALT: u8 = 0x0001;
    const MOD_CONTROL: u8 = 0x0002;
    const MOD_SHIFT: u8 = 0x0004;
    const MOD_NOREPEAT: u32 = 0x4000;
    const VK_SPACE: u32 = 0x20;
    const PM_NOREMOVE: u32 = 0;

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct Message {
        hwnd: *mut c_void,
        message: u32,
        w_param: usize,
        l_param: isize,
        time: u32,
        point: Point,
        private: u32,
    }

    struct SetRequest {
        modifiers: u8,
        result: SyncSender<Result<(), String>>,
    }

    type PendingSet = Arc<Mutex<Option<SetRequest>>>;

    fn modifier_flags(preset: &str) -> u8 {
        match super::normalize(preset) {
            p if p == PRESETS[1] => MOD_ALT,
            p if p == PRESETS[2] => MOD_CONTROL | MOD_SHIFT,
            _ => MOD_CONTROL,
        }
    }

    fn register(modifiers: u8) -> Result<(), String> {
        // `MOD_NOREPEAT` prevents repeated WM_HOTKEY messages while Space is
        // held. The timer below is enabled only until the matching key-up.
        let flags = u32::from(modifiers) | MOD_NOREPEAT;
        // SAFETY: A null window associates this hotkey with the current thread.
        let ok = unsafe { RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, flags, VK_SPACE) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            Err(format!(
                "another app may already hold this shortcut (Windows error {error})"
            ))
        } else {
            Ok(())
        }
    }

    fn dismiss_alt_space_menu() {
        use enigo::{Direction, Enigo, Key, Keyboard, Settings};

        if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
            let _ = enigo.key(Key::Escape, Direction::Click);
        }
    }

    pub struct Hotkey {
        thread_id: u32,
        thread: Option<JoinHandle<()>>,
        pending_set: PendingSet,
        pub rx: Receiver<KeyEvent>,
    }

    impl Hotkey {
        /// Register a preset (see PRESETS). Unknown strings fall back to default.
        pub fn register(preset: &str) -> Result<Self, String> {
            let (events, rx) = mpsc::channel();
            let pending_set = Arc::new(Mutex::new(None));
            let thread_pending_set = Arc::clone(&pending_set);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let modifiers = modifier_flags(preset);

            let thread = thread::Builder::new()
                .name("utterly-hotkey".into())
                .stack_size(256 * 1024)
                .spawn(move || hotkey_thread(modifiers, events, thread_pending_set, ready_tx))
                .map_err(|e| e.to_string())?;

            match ready_rx.recv() {
                Ok(Ok(thread_id)) => Ok(Self {
                    thread_id,
                    thread: Some(thread),
                    pending_set,
                    rx,
                }),
                Ok(Err(error)) => {
                    let _ = thread.join();
                    Err(error)
                }
                Err(error) => {
                    let _ = thread.join();
                    Err(format!("hotkey thread stopped during startup: {error}"))
                }
            }
        }

        /// Switch preset at runtime. On failure, the old shortcut is restored.
        pub fn set(&mut self, preset: &str) -> Result<(), String> {
            let (result, response) = mpsc::sync_channel(1);
            let mut pending = self
                .pending_set
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if pending.is_some() {
                return Err("another hotkey change is already in progress".into());
            }
            *pending = Some(SetRequest {
                modifiers: modifier_flags(preset),
                result,
            });
            drop(pending);

            // Wake the hotkey thread so it can unregister/register on the same
            // thread that owns the WM_HOTKEY message queue.
            let posted = unsafe { PostThreadMessageW(self.thread_id, WM_APP_SET_HOTKEY, 0, 0) };
            if posted == 0 {
                self.pending_set
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                return Err("couldn't reach the hotkey message thread".into());
            }

            response
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "hotkey change timed out".to_string())?
        }

        pub fn try_event(&self) -> Option<KeyEvent> {
            self.rx.try_recv().ok()
        }
    }

    impl Drop for Hotkey {
        fn drop(&mut self) {
            // SAFETY: thread_id belongs to our live message-pumping thread.
            unsafe {
                PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn hotkey_thread(
        mut modifiers: u8,
        events: mpsc::Sender<KeyEvent>,
        pending_set: PendingSet,
        ready: SyncSender<Result<u32, String>>,
    ) {
        // Create the message queue before registering thread-owned hotkeys.
        let mut message = unsafe { std::mem::zeroed::<Message>() };
        unsafe {
            PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
        }
        let thread_id = unsafe { GetCurrentThreadId() };
        if let Err(error) = register(modifiers) {
            let _ = ready.send(Err(error));
            return;
        }
        if ready.send(Ok(thread_id)).is_err() {
            unsafe {
                UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
            }
            return;
        }

        let mut active = false;
        let mut timer_id = 0;
        let mut active_modifiers = modifiers;
        loop {
            let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
            if result <= 0 {
                if result < 0 {
                    eprintln!("[utterly] hotkey message loop failed: {}", unsafe {
                        GetLastError()
                    });
                }
                break;
            }
            match message.message {
                WM_HOTKEY if message.w_param == HOTKEY_ID as usize && !active => {
                    // Poll only while held. This replaces global-hotkey's
                    // Windows busy-spin release watcher with a 10 ms timer.
                    let timer = unsafe { SetTimer(std::ptr::null_mut(), TIMER_ID, 10, None) };
                    if timer != 0 {
                        active = true;
                        timer_id = timer;
                        active_modifiers = modifiers;
                        let _ = events.send(KeyEvent::Pressed);
                    }
                }
                WM_TIMER if message.w_param == timer_id && active => {
                    let held = unsafe { GetAsyncKeyState(VK_SPACE as i32) < 0 };
                    let alt_held = unsafe { GetAsyncKeyState(0x12) < 0 };
                    let chord_released = !held && (active_modifiers & MOD_ALT == 0 || !alt_held);
                    if chord_released {
                        active = false;
                        timer_id = 0;
                        unsafe {
                            KillTimer(std::ptr::null_mut(), message.w_param);
                        }
                        if active_modifiers & MOD_ALT != 0 {
                            // Alt+Space is also the Windows system-menu
                            // shortcut. Close that menu after Alt is released
                            // so paste returns to the original text field.
                            dismiss_alt_space_menu();
                        }
                        let _ = events.send(KeyEvent::Released);
                    }
                }
                WM_APP_SET_HOTKEY => {
                    let request = pending_set
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take();
                    if let Some(request) = request {
                        unsafe {
                            UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
                        }
                        match register(request.modifiers) {
                            Ok(()) => {
                                modifiers = request.modifiers;
                                let _ = request.result.send(Ok(()));
                            }
                            Err(error) => {
                                let restore = register(modifiers);
                                let result = match restore {
                                    Ok(()) => Err(error),
                                    Err(restore_error) => Err(format!(
                                        "{error}; couldn't restore previous shortcut: {restore_error}"
                                    )),
                                };
                                let _ = request.result.send(result);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        unsafe {
            if timer_id != 0 {
                KillTimer(std::ptr::null_mut(), timer_id);
            }
            UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
        }
        if active {
            let _ = events.send(KeyEvent::Released);
        }
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetAsyncKeyState(key: i32) -> i16;
        fn GetMessageW(message: *mut Message, window: *mut c_void, min: u32, max: u32) -> i32;
        fn KillTimer(window: *mut c_void, timer_id: usize) -> i32;
        fn PeekMessageW(
            message: *mut Message,
            window: *mut c_void,
            min: u32,
            max: u32,
            remove: u32,
        ) -> i32;
        fn PostThreadMessageW(thread_id: u32, message: u32, w_param: usize, l_param: isize) -> i32;
        fn RegisterHotKey(window: *mut c_void, id: i32, modifiers: u32, key: u32) -> i32;
        fn SetTimer(
            window: *mut c_void,
            timer_id: usize,
            interval: u32,
            callback: Option<unsafe extern "system" fn(*mut c_void, u32, usize, u32)>,
        ) -> usize;
        fn UnregisterHotKey(window: *mut c_void, id: i32) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThreadId() -> u32;
        fn GetLastError() -> u32;
    }
}

#[cfg(target_os = "windows")]
pub use windows::Hotkey;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_normalize() {
        assert_eq!(normalize("Ctrl+Space"), "Ctrl+Space");
        assert_eq!(normalize("ctrl + shift + space"), "Ctrl+Shift+Space");
        assert_eq!(normalize("ALT+space"), "Alt+Space");
        assert_eq!(normalize("garbage"), "Ctrl+Space");
    }
}
