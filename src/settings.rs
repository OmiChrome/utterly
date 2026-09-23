//! Small native settings window for Windows.

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub mode: String,
    pub hotkey: String,
    pub vocabulary: Vec<String>,
}

#[cfg(target_os = "windows")]
mod windows {
    use super::Snapshot;
    use crate::{config, hotkey, tray::MenuCmd};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    const CLASS: &str = "UtterlyNativeSettings";
    const WM_CREATE: u32 = 0x0001;
    const WM_CLOSE: u32 = 0x0010;
    const WM_COMMAND: u32 = 0x0111;
    const WM_CTLCOLORBTN: u32 = 0x0135;
    const WM_CTLCOLORSTATIC: u32 = 0x0138;
    const WM_SETFONT: u32 = 0x0030;
    const WM_APP_SHOW: u32 = 0x8001;
    const WM_APP_REFRESH: u32 = 0x8002;
    const GWLP_USERDATA: i32 = -21;
    const WS_CHILD: u32 = 0x4000_0000;
    const WS_VISIBLE: u32 = 0x1000_0000;
    const WS_TABSTOP: u32 = 0x0001_0000;
    const WS_BORDER: u32 = 0x0080_0000;
    const WS_VSCROLL: u32 = 0x0020_0000;
    const WS_CLIPCHILDREN: u32 = 0x0200_0000;
    const WS_CAPTION_SYSMENU_MIN: u32 = 0x00CA_0000;
    const BS_AUTORADIOBUTTON: u32 = 0x0009;
    const BS_PUSHBUTTON: u32 = 0x0000;
    const CBS_DROPDOWNLIST: u32 = 0x0003;
    const LBS_NOTIFY: u32 = 0x0001;
    const SS_LEFT: u32 = 0x0000;
    const SW_HIDE: i32 = 0;
    const SW_RESTORE: i32 = 9;
    const BN_CLICKED: u16 = 0;
    const CBN_SELCHANGE: u16 = 1;
    const LBN_SELCHANGE: u16 = 1;
    const CB_ADDSTRING: u32 = 0x0143;
    const CB_SETCURSEL: u32 = 0x014E;
    const CB_GETCURSEL: u32 = 0x0147;
    const LB_RESETCONTENT: u32 = 0x0184;
    const LB_ADDSTRING: u32 = 0x0180;
    const LB_GETCURSEL: u32 = 0x0188;
    const LB_GETTEXTLEN: u32 = 0x018A;
    const LB_GETTEXT: u32 = 0x0189;
    const BM_SETCHECK: u32 = 0x00F1;
    const BST_CHECKED: usize = 1;
    const EM_SETLIMITTEXT: u32 = 0x00C5;
    const DEFAULT_GUI_FONT: i32 = 17;
    const COLOR_WINDOW: i32 = 5;
    const MAX_WORD_CHARS: usize = 120;
    const ID_SMART: i32 = 101;
    const ID_VERBATIM: i32 = 102;
    const ID_HOTKEY: i32 = 103;
    const ID_WORD: i32 = 104;
    const ID_ADD: i32 = 105;
    const ID_WORDS: i32 = 106;
    const ID_REMOVE: i32 = 107;
    const ID_COUNT: i32 = 108;
    const ID_STATUS: i32 = 109;

    type Hwnd = *mut c_void;
    type WndProc = unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct Message {
        hwnd: Hwnd,
        message: u32,
        w_param: usize,
        l_param: isize,
        time: u32,
        point: Point,
        private: u32,
    }

    #[repr(C)]
    struct WindowClass {
        style: u32,
        wnd_proc: Option<WndProc>,
        cls_extra: i32,
        wnd_extra: i32,
        instance: Hwnd,
        icon: Hwnd,
        cursor: Hwnd,
        background: Hwnd,
        menu_name: *const u16,
        class_name: *const u16,
    }

    #[repr(C)]
    struct CreateStruct {
        create_params: *mut c_void,
        instance: Hwnd,
        menu: Hwnd,
        parent: Hwnd,
        height: i32,
        width: i32,
        y: i32,
        x: i32,
        style: i32,
        name: *const u16,
        class: *const u16,
        ex_style: u32,
    }

    #[derive(Default)]
    struct Controls {
        smart: Hwnd,
        verbatim: Hwnd,
        hotkey: Hwnd,
        word: Hwnd,
        add: Hwnd,
        words: Hwnd,
        remove: Hwnd,
        count: Hwnd,
        status: Hwnd,
    }

    struct WindowState {
        snapshot: Arc<Mutex<Snapshot>>,
        actions: Sender<MenuCmd>,
        controls: Controls,
        font: Hwnd,
        instance: Hwnd,
        scale: f32,
    }

    #[derive(Clone)]
    pub struct SettingsWindow {
        hwnd: Arc<AtomicIsize>,
        snapshot: Arc<Mutex<Snapshot>>,
    }

    impl SettingsWindow {
        pub fn spawn(snapshot: Snapshot, actions: Sender<MenuCmd>) -> Result<Self, String> {
            let snapshot = Arc::new(Mutex::new(snapshot));
            let hwnd = Arc::new(AtomicIsize::new(0));
            let thread_snapshot = Arc::clone(&snapshot);
            let thread_hwnd = Arc::clone(&hwnd);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            thread::Builder::new()
                .name("utterly-settings".into())
                .stack_size(512 * 1024)
                .spawn(move || window_thread(thread_snapshot, thread_hwnd, actions, ready_tx))
                .map_err(|error| error.to_string())?;
            ready_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| format!("settings window startup timed out: {error}"))??;
            Ok(Self { hwnd, snapshot })
        }

        pub fn show(&self) {
            post(&self.hwnd, WM_APP_SHOW);
        }

        pub fn update(&self, snapshot: Snapshot) {
            if let Ok(mut current) = self.snapshot.lock() {
                *current = snapshot;
            }
            post(&self.hwnd, WM_APP_REFRESH);
        }
    }

    fn post(hwnd: &AtomicIsize, message: u32) {
        let hwnd = hwnd.load(Ordering::Acquire);
        if hwnd != 0 {
            unsafe {
                PostMessageW(hwnd as Hwnd, message, 0, 0);
            }
        }
    }

    fn window_thread(
        snapshot: Arc<Mutex<Snapshot>>,
        hwnd_slot: Arc<AtomicIsize>,
        actions: Sender<MenuCmd>,
        ready: mpsc::SyncSender<Result<(), String>>,
    ) {
        let class = wide(CLASS);
        let title = wide("Utterly Settings");
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let cursor = unsafe { LoadCursorW(std::ptr::null_mut(), 32512usize as *const u16) };
        let wc = WindowClass {
            style: 0,
            wnd_proc: Some(window_proc),
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: std::ptr::null_mut(),
            cursor,
            background: 6usize as Hwnd, // COLOR_WINDOW + 1
            menu_name: std::ptr::null(),
            class_name: class.as_ptr(),
        };
        if unsafe { RegisterClassW(&wc) } == 0 {
            let error = std::io::Error::last_os_error();
            let _ = ready.send(Err(format!("register settings window: {error}")));
            return;
        }

        let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
        let dpi = unsafe { GetDpiForSystem() }.max(96) as f32;
        let mut state = Box::new(WindowState {
            snapshot,
            actions,
            controls: Controls::default(),
            font,
            instance,
            scale: dpi / 96.0,
        });
        let state_ptr = (&mut *state) as *mut WindowState;
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_CAPTION_SYSMENU_MIN | WS_CLIPCHILDREN,
                i32::MIN,
                i32::MIN,
                scale(414, state.scale),
                scale(500, state.scale),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                state_ptr.cast(),
            )
        };
        if hwnd.is_null() {
            let error = std::io::Error::last_os_error();
            let _ = ready.send(Err(format!("create settings window: {error}")));
            return;
        }
        hwnd_slot.store(hwnd as isize, Ordering::Release);
        let _ = ready.send(Ok(()));

        let mut message: Message = unsafe { std::mem::zeroed() };
        loop {
            let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
            if result <= 0 {
                break;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        let _ = state; // Keep the window state alive for the full message loop.
    }

    unsafe extern "system" fn window_proc(
        hwnd: Hwnd,
        message: u32,
        w_param: usize,
        l_param: isize,
    ) -> isize {
        if message == 0x0081 {
            let create = &*(l_param as *const CreateStruct);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.create_params as isize);
            return DefWindowProcW(hwnd, message, w_param, l_param);
        }
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
        if state_ptr.is_null() {
            return DefWindowProcW(hwnd, message, w_param, l_param);
        }
        let state = &mut *state_ptr;
        match message {
            WM_CREATE => {
                create_controls(hwnd, state);
                refresh(state);
                0
            }
            WM_APP_SHOW => {
                refresh(state);
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
                0
            }
            WM_APP_REFRESH => {
                refresh(state);
                0
            }
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_COMMAND => {
                command(state, w_param, l_param);
                0
            }
            WM_CTLCOLORBTN | WM_CTLCOLORSTATIC => {
                SetBkMode(w_param as Hwnd, 1); // TRANSPARENT
                SetBkColor(w_param as Hwnd, 0x00FF_FFFF);
                GetSysColorBrush(COLOR_WINDOW) as isize
            }
            _ => DefWindowProcW(hwnd, message, w_param, l_param),
        }
    }

    unsafe fn create_controls(hwnd: Hwnd, state: &mut WindowState) {
        label(
            hwnd,
            state,
            (20, 18, 370, 20),
            "Which transcription style do you want?",
            1,
        );
        let controls = Controls {
            smart: control(
                hwnd,
                state,
                "BUTTON",
                "Smart  ·  cleans up and formats",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTORADIOBUTTON,
                (22, 42, 370, 24),
                ID_SMART,
            ),
            verbatim: control(
                hwnd,
                state,
                "BUTTON",
                "Verbatim  ·  keeps words as spoken",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTORADIOBUTTON,
                (22, 68, 370, 24),
                ID_VERBATIM,
            ),
            hotkey: control(
                hwnd,
                state,
                "COMBOBOX",
                "",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | WS_VSCROLL | CBS_DROPDOWNLIST,
                (22, 130, 370, 140),
                ID_HOTKEY,
            ),
            count: label(
                hwnd,
                state,
                (20, 199, 370, 18),
                "0 / 1,000 entries",
                ID_COUNT,
            ),
            word: control(
                hwnd,
                state,
                "EDIT",
                "",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER,
                (22, 224, 282, 26),
                ID_WORD,
            ),
            add: control(
                hwnd,
                state,
                "BUTTON",
                "Add",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON,
                (312, 224, 80, 26),
                ID_ADD,
            ),
            words: control(
                hwnd,
                state,
                "LISTBOX",
                "",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | WS_VSCROLL | LBS_NOTIFY,
                (22, 260, 370, 132),
                ID_WORDS,
            ),
            remove: control(
                hwnd,
                state,
                "BUTTON",
                "Remove selected phrase",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON,
                (22, 402, 370, 26),
                ID_REMOVE,
            ),
            status: label(
                hwnd,
                state,
                (20, 440, 370, 20),
                "Up to 1,000 phrases; best results typically use 100 or fewer.",
                ID_STATUS,
            ),
        };
        label(
            hwnd,
            state,
            (20, 105, 370, 20),
            "Which keybind do you want to use?",
            2,
        );
        for preset in hotkey::PRESETS {
            let text = wide(preset);
            SendMessageW(controls.hotkey, CB_ADDSTRING, 0, text.as_ptr() as isize);
        }
        label(hwnd, state, (20, 177, 370, 20), "Personal dictionary", 3);
        SendMessageW(controls.word, EM_SETLIMITTEXT, MAX_WORD_CHARS, 0);
        state.controls = controls;
    }

    unsafe fn label(
        parent: Hwnd,
        state: &WindowState,
        bounds: (i32, i32, i32, i32),
        text: &str,
        id: i32,
    ) -> Hwnd {
        control(
            parent,
            state,
            "STATIC",
            text,
            WS_CHILD | WS_VISIBLE | SS_LEFT,
            bounds,
            id,
        )
    }

    unsafe fn control(
        parent: Hwnd,
        state: &WindowState,
        class: &str,
        text: &str,
        style: u32,
        bounds: (i32, i32, i32, i32),
        id: i32,
    ) -> Hwnd {
        let class = wide(class);
        let text = wide(text);
        let (x, y, width, height) = bounds;
        let child = CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            style,
            scale(x, state.scale),
            scale(y, state.scale),
            scale(width, state.scale),
            scale(height, state.scale),
            parent,
            id as usize as Hwnd,
            state.instance,
            std::ptr::null_mut(),
        );
        if !child.is_null() {
            SendMessageW(child, WM_SETFONT, state.font as usize, 1);
        }
        child
    }

    unsafe fn refresh(state: &WindowState) {
        let Ok(snapshot) = state.snapshot.lock() else {
            return;
        };
        let c = &state.controls;
        SendMessageW(
            c.smart,
            BM_SETCHECK,
            usize::from(snapshot.mode != "verbatim") * BST_CHECKED,
            0,
        );
        SendMessageW(
            c.verbatim,
            BM_SETCHECK,
            usize::from(snapshot.mode == "verbatim") * BST_CHECKED,
            0,
        );
        let selected = hotkey::PRESETS
            .iter()
            .position(|preset| *preset == hotkey::normalize(&snapshot.hotkey))
            .unwrap_or(0);
        SendMessageW(c.hotkey, CB_SETCURSEL, selected, 0);
        SendMessageW(c.words, LB_RESETCONTENT, 0, 0);
        for phrase in &snapshot.vocabulary {
            let phrase = wide(phrase);
            SendMessageW(c.words, LB_ADDSTRING, 0, phrase.as_ptr() as isize);
        }
        let count = format!(
            "{} / 1,000 entries  ·  best results typically use 100 or fewer",
            snapshot.vocabulary.len()
        );
        SetWindowTextW(c.count, wide(&count).as_ptr());
        EnableWindow(c.remove, i32::from(!snapshot.vocabulary.is_empty()));
        EnableWindow(
            c.add,
            i32::from(snapshot.vocabulary.len() < config::MAX_CUSTOM_VOCABULARY),
        );
    }

    unsafe fn command(state: &WindowState, w_param: usize, _l_param: isize) {
        let id = (w_param & 0xFFFF) as i32;
        let notification = ((w_param >> 16) & 0xFFFF) as u16;
        let send = |cmd| {
            let _ = state.actions.send(cmd);
        };
        match (id, notification) {
            (ID_SMART, BN_CLICKED) => send(MenuCmd::Mode("smart".into())),
            (ID_VERBATIM, BN_CLICKED) => send(MenuCmd::Mode("verbatim".into())),
            (ID_HOTKEY, CBN_SELCHANGE) => {
                let index = SendMessageW(state.controls.hotkey, CB_GETCURSEL, 0, 0);
                if let Some(preset) = hotkey::PRESETS.get(index as usize) {
                    send(MenuCmd::Hotkey((*preset).into()));
                }
            }
            (ID_ADD, BN_CLICKED) => add_phrase(state),
            (ID_REMOVE, BN_CLICKED) => remove_phrase(state),
            (ID_WORDS, LBN_SELCHANGE) => {
                SetWindowTextW(state.controls.status, wide("").as_ptr());
            }
            _ => {}
        }
    }

    unsafe fn add_phrase(state: &WindowState) {
        let edit = state.controls.word;
        let count = GetWindowTextLengthW(edit).max(0) as usize;
        if count == 0 {
            SetWindowTextW(
                state.controls.status,
                wide("Enter a word or phrase first.").as_ptr(),
            );
            return;
        }
        let mut text = vec![0u16; count + 1];
        GetWindowTextW(edit, text.as_mut_ptr(), text.len() as i32);
        let phrase = String::from_utf16_lossy(&text[..count]);
        let normalized = config::normalize_custom_term(&phrase);
        if normalized.is_none() {
            SetWindowTextW(
                state.controls.status,
                wide("Use a phrase under 120 characters.").as_ptr(),
            );
            return;
        }
        let normalized = normalized.unwrap();
        let Ok(snapshot) = state.snapshot.lock() else {
            return;
        };
        if snapshot.vocabulary.len() >= config::MAX_CUSTOM_VOCABULARY {
            SetWindowTextW(
                state.controls.status,
                wide("The 1,000-entry limit is full.").as_ptr(),
            );
        } else if snapshot
            .vocabulary
            .iter()
            .any(|item| item.eq_ignore_ascii_case(&normalized))
        {
            SetWindowTextW(
                state.controls.status,
                wide("That phrase is already listed.").as_ptr(),
            );
        } else {
            drop(snapshot);
            if state
                .actions
                .send(MenuCmd::DictionaryAdd(normalized))
                .is_ok()
            {
                SetWindowTextW(edit, wide("").as_ptr());
                SetWindowTextW(
                    state.controls.status,
                    wide("Added for the next utterance.").as_ptr(),
                );
            } else {
                SetWindowTextW(
                    state.controls.status,
                    wide("Utterly is not responding.").as_ptr(),
                );
            }
        }
    }

    unsafe fn remove_phrase(state: &WindowState) {
        let index = SendMessageW(state.controls.words, LB_GETCURSEL, 0, 0);
        if index < 0 {
            return;
        }
        let len = SendMessageW(state.controls.words, LB_GETTEXTLEN, index as usize, 0);
        if len < 0 || len as usize > MAX_WORD_CHARS {
            return;
        }
        let mut text = vec![0u16; len as usize + 1];
        SendMessageW(
            state.controls.words,
            LB_GETTEXT,
            index as usize,
            text.as_mut_ptr() as isize,
        );
        let phrase = String::from_utf16_lossy(&text[..len as usize]);
        let _ = state.actions.send(MenuCmd::DictionaryRemove(phrase));
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    fn scale(value: i32, factor: f32) -> i32 {
        (value as f32 * factor).round() as i32
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassW(class: *const WindowClass) -> u16;
        fn CreateWindowExW(
            ex_style: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: Hwnd,
            menu: Hwnd,
            instance: Hwnd,
            param: *mut c_void,
        ) -> Hwnd;
        fn DefWindowProcW(hwnd: Hwnd, message: u32, w_param: usize, l_param: isize) -> isize;
        fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
        fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> isize;
        fn GetMessageW(message: *mut Message, hwnd: Hwnd, first: u32, last: u32) -> i32;
        fn TranslateMessage(message: *const Message) -> i32;
        fn DispatchMessageW(message: *const Message) -> isize;
        fn PostMessageW(hwnd: Hwnd, message: u32, w_param: usize, l_param: isize) -> i32;
        fn ShowWindow(hwnd: Hwnd, command: i32) -> i32;
        fn SetForegroundWindow(hwnd: Hwnd) -> i32;
        fn SendMessageW(hwnd: Hwnd, message: u32, w_param: usize, l_param: isize) -> isize;
        fn GetWindowTextLengthW(hwnd: Hwnd) -> i32;
        fn GetWindowTextW(hwnd: Hwnd, text: *mut u16, max: i32) -> i32;
        fn SetWindowTextW(hwnd: Hwnd, text: *const u16) -> i32;
        fn EnableWindow(hwnd: Hwnd, enable: i32) -> i32;
        fn GetModuleHandleW(name: *const u16) -> Hwnd;
        fn LoadCursorW(instance: Hwnd, cursor: *const u16) -> Hwnd;
        fn GetDpiForSystem() -> u32;
        fn GetSysColorBrush(index: i32) -> Hwnd;
        fn SetBkMode(dc: Hwnd, mode: i32) -> i32;
        fn SetBkColor(dc: Hwnd, color: u32) -> u32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn GetStockObject(index: i32) -> Hwnd;
    }
}

#[cfg(target_os = "windows")]
pub use windows::SettingsWindow;

#[cfg(not(target_os = "windows"))]
#[derive(Clone)]
pub struct SettingsWindow;

#[cfg(not(target_os = "windows"))]
impl SettingsWindow {
    pub fn spawn(
        _snapshot: Snapshot,
        _actions: std::sync::mpsc::Sender<crate::tray::MenuCmd>,
    ) -> Result<Self, String> {
        Ok(Self)
    }

    pub fn show(&self) {}
    pub fn update(&self, _snapshot: Snapshot) {}
}
