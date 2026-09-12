//! Global push-to-talk hotkey: hold Ctrl+Space to listen, release to transcribe.
//! Thin wrapper over the `global-hotkey` crate (winit-compatible, ~100 KiB).
//! Emits Pressed/Released over std mpsc — no polling thread, ~0% idle CPU.

use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
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

fn to_hotkey(preset: &str) -> HotKey {
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

pub struct Hotkey {
    _manager: GlobalHotKeyManager,
    _key: HotKey,
    pub rx: Receiver<KeyEvent>,
}

impl Hotkey {
    /// Register a preset (see PRESETS). Unknown strings fall back to default.
    pub fn register(want: &str) -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
        let key = to_hotkey(want);
        manager.register(key).map_err(|e| {
            #[cfg(target_os = "macos")]
            let hint = "need accessibility/input permission on macOS";
            #[cfg(target_os = "windows")]
            let hint = "another app may hold Ctrl+Space — try another preset";
            #[cfg(target_os = "linux")]
            let hint = "another app may hold this combo — try another preset";
            #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
            let hint = "check input permission / conflicting hotkeys";
            format!("hotkey register ({hint}): {e}")
        })?;
        let (tx, rx) = mpsc::channel::<KeyEvent>();
        let global_rx = GlobalHotKeyEvent::receiver();
        std::thread::Builder::new()
            .name("utterly-hotkey".into())
            .stack_size(256 * 1024) // tiny stack: single-digit RAM budget
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
        Ok(Self {
            _manager: manager,
            _key: key,
            rx,
        })
    }

    /// Switch preset at runtime (tray menu). On failure the old hotkey is
    /// restored so push-to-talk never silently dies.
    pub fn set(&mut self, preset: &str) -> Result<(), String> {
        let new_key = to_hotkey(preset);
        let _ = self._manager.unregister(self._key);
        match self._manager.register(new_key) {
            Ok(()) => {
                self._key = new_key;
                Ok(())
            }
            Err(e) => {
                let _ = self._manager.register(self._key);
                Err(e.to_string())
            }
        }
    }

    pub fn try_event(&self) -> Option<KeyEvent> {
        self.rx.try_recv().ok()
    }
}

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
