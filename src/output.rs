//! Output: commit finalized SMART text to the focused textarea.
//! Strategy: clipboard + synthetic paste (Ctrl/Cmd+V) so it works in ANY app's
//! text area without per-app plugins. Also returns the text for the pill window.

/// Google AI Studio page where the user creates a Gemini API key.
/// Shown in the first-run pill title and opened automatically (best-effort)
/// when no key is configured.
pub const AI_STUDIO_URL: &str = "https://aistudio.google.com/apikey";

/// Pure key-shape check shared by the tray `PasteKey` flow and the
/// first-run clipboard poll: ≥12 chars, no internal whitespace.
pub fn is_valid_key(key: &str) -> bool {
    let k = key.trim();
    k.len() >= 12 && !k.chars().any(|c| c.is_whitespace())
}

/// Open `url` in the default browser, best-effort and never blocking:
/// spawned on a tiny thread, all failures ignored. std-only (no `open`/`rfd` crate).
pub fn open_browser(url: &str) {
    let url = url.to_string();
    let _ = std::thread::Builder::new()
        .name("utterly-open-browser".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", &url])
                    .status();
            }
            #[cfg(target_os = "macos")]
            {
                let _ = std::process::Command::new("open").arg(&url).status();
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            {
                let _ = std::process::Command::new("xdg-open").arg(&url).status();
            }
        });
}
pub fn to_clipboard(text: &str) -> bool {
    // Retry up to 3 times with brief backoff on Windows if clipboard is momentarily locked
    for attempt in 0..3 {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if cb.set_text(text.to_string()).is_ok() {
                return true;
            }
        }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    false
}

#[cfg(target_os = "windows")]
mod windows_input {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetAsyncKeyState(v_key: i32) -> i16;
    }

    pub fn modifiers_released() -> bool {
        // Never synthesize key-up for a key the user is still physically holding.
        for _ in 0..50 {
            if [0x10, 0x11, 0x12, 0x5b, 0x5c]
                .iter()
                .all(|key| unsafe { GetAsyncKeyState(*key) >= 0 })
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }
}

/// Paste clipboard into the focused field via synthetic keystroke
/// (enigo 0.2: `Keyboard::key` + `Direction`). Ctrl+V (Linux/Windows),
/// Cmd+V (macOS).
#[allow(dead_code)] // Public helper for callers without a captured FocusTarget.
pub fn paste_into_focused() -> bool {
    #[cfg(target_os = "windows")]
    if !windows_input::modifiers_released() {
        return false;
    }

    paste_key_chord()
}

/// Call only after the caller has checked modifiers and target identity.
fn paste_key_chord() -> bool {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut e = match Enigo::new(&Settings::default()) {
        Ok(e) => e,
        Err(_) => return false,
    };
    #[cfg(target_os = "macos")]
    let held = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let held = Key::Control;
    if e.key(held, Direction::Press).is_err() {
        return false;
    }
    // Windows shortcuts are physical virtual-key events; Unicode input sends
    // text characters and does not reliably form the Ctrl+V chord.
    #[cfg(target_os = "windows")]
    let pasted = e.key(Key::V, Direction::Click).is_ok();
    #[cfg(not(target_os = "windows"))]
    let pasted = e.key(Key::Unicode('v'), Direction::Click).is_ok();
    // Always release the modifier, even if the paste keystroke failed.
    let released = e.key(held, Direction::Release).is_ok();
    pasted && released
}

#[derive(Debug, PartialEq, Eq)]
pub enum CommitError {
    ClipboardUnavailable,
    PasteFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    Pasted,
    ClipboardOnly,
}

/// Identity and limited caret context only. No COM objects cross threads.
#[derive(Clone, Default)]
pub struct FocusTarget {
    window: isize,
    runtime_id: Vec<i32>,
    before: String,
    after: String,
    context: String,
    context_known: bool,
}

impl FocusTarget {
    pub fn capture(context: bool) -> Self {
        #[cfg(target_os = "windows")]
        return native_output::capture(context).unwrap_or_default();
        #[cfg(not(target_os = "windows"))]
        {
            let _ = context;
            Self::default()
        }
    }

    pub fn context(&self) -> &str {
        &self.context
    }

    fn still_focused(&self) -> bool {
        if self.window == 0 || self.runtime_id.is_empty() {
            return false;
        }
        let now = Self::capture(false);
        same_target(self.window, &self.runtime_id, now.window, &now.runtime_id)
    }
}

fn same_target(window: isize, identity: &[i32], current: isize, current_id: &[i32]) -> bool {
    window != 0 && window == current && !identity.is_empty() && identity == current_id
}

/// Always preserve the result on the clipboard; paste only into the captured edit.
pub fn commit_to_target(
    text: &str,
    target: &FocusTarget,
    smart: bool,
) -> Result<CommitOutcome, CommitError> {
    if text.trim().is_empty() {
        return Err(CommitError::ClipboardUnavailable);
    }
    let same = target.still_focused();
    let text = if smart && same && target.context_known {
        smart_text(text, &target.before, &target.after)
    } else {
        text.to_string()
    };
    if !to_clipboard(&text) {
        return Err(CommitError::ClipboardUnavailable);
    }
    #[cfg(not(target_os = "windows"))]
    return if paste_into_focused() {
        Ok(CommitOutcome::Pasted)
    } else {
        Err(CommitError::PasteFailed)
    };
    #[cfg(target_os = "windows")]
    {
        if !same {
            return Ok(CommitOutcome::ClipboardOnly);
        }
        if !windows_input::modifiers_released() {
            return Ok(CommitOutcome::ClipboardOnly);
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        // Recheck after clipboard/modifier waits, immediately before synthetic input.
        if !target.still_focused() {
            return Ok(CommitOutcome::ClipboardOnly);
        }
        if paste_key_chord() {
            Ok(CommitOutcome::Pasted)
        } else {
            Err(CommitError::PasteFailed)
        }
    }
}

fn smart_text(text: &str, before: &str, after: &str) -> String {
    let mut text = text.trim().to_string();
    let previous = before.chars().next_back();
    let next = after.chars().next();
    if before.trim_end().is_empty()
        || before.trim_end().ends_with(['.', '!', '?', '\n'])
        || before.ends_with('\n')
    {
        if let Some(first) = text.chars().next() {
            text.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
        }
    }
    if let Some(n) = next {
        if ".,!?;:".contains(n) && text.ends_with(n) {
            text.pop();
        }
    }
    if previous
        .is_some_and(|c| !c.is_whitespace() && (c.is_alphanumeric() || ".,!?;:)\"".contains(c)))
        && text.chars().next().is_some_and(|c| c.is_alphanumeric())
        && !previous.is_some_and(is_unspaced_script)
        && !text.chars().next().is_some_and(is_unspaced_script)
    {
        text.insert(0, ' ');
    }
    if next.is_some_and(|c| c.is_alphanumeric())
        && text.chars().next_back().is_some_and(|c| !c.is_whitespace())
        && !next.is_some_and(is_unspaced_script)
        && !text.chars().next_back().is_some_and(is_unspaced_script)
    {
        text.push(' ');
    }
    text
}

fn is_unspaced_script(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af)
}

/// Local, conservative candidates; never learns URLs, numbers or sentence starters.
pub fn learned_terms(context: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut sentence_start = true;
    for raw in context.split_whitespace().take(160) {
        let token = raw.trim_matches(|c: char| !c.is_alphabetic());
        if !sentence_start
            && (3..=32).contains(&token.chars().count())
            && token
                .chars()
                .all(|c| c.is_alphabetic() || c == '-' || c == '\'')
            && !raw.contains(['@', '/', ':'])
            && !raw.chars().any(|c| c.is_ascii_digit())
            && token.chars().next().is_some_and(|c| c.is_uppercase())
            && token.chars().any(|c| c.is_lowercase())
            && ![
                "The", "This", "That", "These", "Those", "And", "For", "With", "From", "Your",
                "You", "They", "Their", "Our", "But",
            ]
            .contains(&token)
            && !result
                .iter()
                .any(|s: &String| s.eq_ignore_ascii_case(token))
        {
            result.push(token.to_string());
            if result.len() == 16 {
                break;
            }
        }
        sentence_start = raw.ends_with(['.', '!', '?']);
    }
    result
}

pub fn focused_app_icon() -> Option<Vec<u8>> {
    #[cfg(target_os = "windows")]
    return native_output::focused_app_icon();
    #[cfg(not(target_os = "windows"))]
    None
}

#[cfg(target_os = "windows")]
#[path = "native_output.rs"]
mod native_output;

/// Read a pasted AI Studio key from the clipboard (tray menu:
/// "Paste API key from clipboard"). Accepts anything that looks like a key:
/// ≥12 chars, no internal whitespace. Returns None otherwise.
pub fn read_key_from_clipboard() -> Option<String> {
    let text = arboard::Clipboard::new().ok()?.get_text().ok()?;
    let key = text.trim().to_string();
    if is_valid_key(&key) {
        Some(key)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_spacing_respects_caret_context() {
        assert_eq!(smart_text("world", "Hello", ""), " world");
        assert_eq!(smart_text("hello", "Done. ", ""), "Hello");
        assert_eq!(smart_text("hello", "", ""), "Hello");
        assert_eq!(smart_text("world", "Hello ", ", next"), "world");
        assert_eq!(smart_text("new", "", "word"), "New ");
        assert_eq!(smart_text("élise", "Done. ", ""), "Élise");
        assert_eq!(smart_text("hello.", "", ". Next"), "Hello");
        assert_eq!(smart_text("世界", "你好", "。"), "世界");
        assert_eq!(smart_text("日本語", "", "です"), "日本語");
    }

    #[test]
    fn dictionary_candidates_are_bounded_and_conservative() {
        let terms = learned_terms("Hello there. Meet Alice and OpenAI at Acme. alice@example.com https://foo.com SECRET123");
        assert!(terms.contains(&"Alice".to_string()));
        assert!(terms.contains(&"OpenAI".to_string()));
        assert!(!terms.contains(&"Hello".to_string()));
        assert!(!terms.contains(&"Meet".to_string()));
        assert!(!terms
            .iter()
            .any(|t| t.contains('@') || t.chars().any(|c| c.is_ascii_digit())));
        assert!(learned_terms(&" Alice".repeat(100)).len() <= 16);
    }

    #[test]
    fn target_identity_refuses_changed_or_unknown_elements() {
        assert!(same_target(10, &[1, 2], 10, &[1, 2]));
        assert!(!same_target(10, &[1, 2], 11, &[1, 2]));
        assert!(!same_target(10, &[1, 2], 10, &[1, 3]));
        assert!(!same_target(10, &[], 10, &[]));
        assert!(!same_target(0, &[1], 0, &[1]));
    }

    #[test]
    fn valid_key_accepts_plain_token() {
        assert!(is_valid_key("AIzaSyD-example-key-123"));
    }

    #[test]
    fn valid_key_trims_surrounding_whitespace() {
        assert!(is_valid_key("  AIzaSyD-example-key-123\n"));
    }

    #[test]
    fn valid_key_rejects_short_or_blank() {
        assert!(!is_valid_key(""));
        assert!(!is_valid_key("   "));
        assert!(!is_valid_key("short-key"));
    }

    #[test]
    fn valid_key_rejects_internal_whitespace() {
        assert!(!is_valid_key("AIzaSyD example key 123"));
        assert!(!is_valid_key("key-with\ttab-inside-123"));
    }

    #[test]
    fn test_to_clipboard_roundtrip() {
        let test_text = "utterly_unit_test_clipboard_12345";
        assert!(to_clipboard(test_text));
        let read = arboard::Clipboard::new().unwrap().get_text().unwrap();
        assert_eq!(read, test_text);
    }

    #[test]
    fn test_commit_empty_rejects() {
        let target = FocusTarget::default();
        assert_eq!(
            commit_to_target("", &target, true),
            Err(CommitError::ClipboardUnavailable)
        );
        assert_eq!(
            commit_to_target("   \n", &target, true),
            Err(CommitError::ClipboardUnavailable)
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "opens temporary Win32 edit windows and changes foreground focus/clipboard"]
    fn native_focus_and_paste_smoke() {
        use std::ffi::c_void;
        use std::sync::mpsc;
        use std::time::Duration;

        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
        };
        #[link(name = "user32")]
        #[allow(clashing_extern_declarations)]
        unsafe extern "system" {
            fn CreateWindowExW(
                ex: u32,
                class: *const u16,
                title: *const u16,
                style: u32,
                x: i32,
                y: i32,
                width: i32,
                height: i32,
                parent: *mut c_void,
                menu: *mut c_void,
                instance: *mut c_void,
                param: *mut c_void,
            ) -> *mut c_void;
            fn ShowWindow(hwnd: *mut c_void, cmd: i32) -> i32;
            fn SetForegroundWindow(hwnd: *mut c_void) -> i32;
            fn SetFocus(hwnd: *mut c_void) -> *mut c_void;
            fn GetForegroundWindow() -> *mut c_void;
            fn GetFocus() -> *mut c_void;
            fn DestroyWindow(hwnd: *mut c_void) -> i32;
            fn GetWindowTextW(hwnd: *mut c_void, text: *mut u16, count: i32) -> i32;
        }
        fn wide(text: &str) -> Vec<u16> {
            text.encode_utf16().chain(std::iter::once(0)).collect()
        }
        fn field_text(hwnd: isize) -> String {
            let mut buf = [0u16; 128];
            let len =
                unsafe { GetWindowTextW(hwnd as *mut c_void, buf.as_mut_ptr(), buf.len() as i32) };
            String::from_utf16_lossy(&buf[..len.max(0) as usize])
        }
        struct ClipboardRestore(Option<String>);
        impl Drop for ClipboardRestore {
            fn drop(&mut self) {
                if let Some(text) = &self.0 {
                    let _ = to_clipboard(text);
                }
            }
        }
        enum Cmd {
            Focus(usize, mpsc::Sender<(bool, bool)>),
            Quit,
        }
        struct Fixture {
            tx: mpsc::Sender<Cmd>,
            join: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = self.tx.send(Cmd::Quit);
                if let Some(join) = self.join.take() {
                    let _ = join.join();
                }
            }
        }

        let clipboard = ClipboardRestore(
            arboard::Clipboard::new()
                .ok()
                .and_then(|mut cb| cb.get_text().ok()),
        );
        let _ = &clipboard;
        let (ready_tx, ready_rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let join = std::thread::spawn(move || unsafe {
            let static_class = wide("STATIC");
            let edit_class = wide("EDIT");
            let parent = CreateWindowExW(
                0,
                static_class.as_ptr(),
                wide("Utterly focus smoke").as_ptr(),
                0x00cf_0000 | 0x1000_0000,
                80,
                80,
                300,
                190,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if parent.is_null() {
                let _ = ready_tx.send([0isize; 4]);
                return;
            }
            let mut fields = [std::ptr::null_mut(); 3];
            for (index, field) in fields.iter_mut().enumerate() {
                *field = CreateWindowExW(
                    0,
                    edit_class.as_ptr(),
                    wide("").as_ptr(),
                    0x4000_0000 | 0x1000_0000 | 0x0001_0000 | if index == 2 { 0x20 } else { 0 },
                    20,
                    20 + index as i32 * 45,
                    240,
                    28,
                    parent,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
            ShowWindow(parent, 5);
            SetForegroundWindow(parent);
            SetFocus(fields[0]);
            let _ = ready_tx.send([
                parent as isize,
                fields[0] as isize,
                fields[1] as isize,
                fields[2] as isize,
            ]);
            'pump: loop {
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        Cmd::Focus(index, ack) => {
                            SetForegroundWindow(parent);
                            SetFocus(fields[index]);
                            let _ = ack.send((
                                GetForegroundWindow() == parent,
                                GetFocus() == fields[index],
                            ));
                        }
                        Cmd::Quit => break 'pump,
                    }
                }
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, HWND(0), 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            DestroyWindow(parent);
        });
        let fixture = Fixture {
            tx: cmd_tx,
            join: Some(join),
        };
        let handles = ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("fixture windows");
        assert!(handles.iter().all(|&handle| handle != 0));
        let fields = &handles[1..];
        let focus = |index| {
            let (ack_tx, ack_rx) = mpsc::channel();
            fixture.tx.send(Cmd::Focus(index, ack_tx)).unwrap();
            let state = ack_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            std::thread::sleep(Duration::from_millis(80));
            assert!(state.1, "fixture edit did not receive focus: {state:?}");
            state.0
        };
        if std::env::var_os("UTTERLY_FOCUS_TEST_INTERACTIVE").is_some() {
            eprintln!("Activate the temporary 'Utterly focus smoke' window within 30 seconds");
            let until = std::time::Instant::now() + Duration::from_secs(30);
            while unsafe { GetForegroundWindow() } as isize != handles[0]
                && std::time::Instant::now() < until
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            assert_eq!(
                unsafe { GetForegroundWindow() } as isize,
                handles[0],
                "fixture was not activated"
            );
        }
        if !focus(0) {
            eprintln!("Skipping native focus smoke: Windows denied foreground activation to the test process");
            return;
        }
        let first = FocusTarget::capture(true);
        assert!(
            !first.runtime_id.is_empty(),
            "UI Automation must capture the owned edit"
        );
        assert_eq!(
            commit_to_target("Utterly smoke text", &first, false),
            Ok(CommitOutcome::Pasted)
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(field_text(fields[0]), "Utterly smoke text");
        assert!(focus(1));
        assert_eq!(
            commit_to_target("must stay on clipboard", &first, false),
            Ok(CommitOutcome::ClipboardOnly)
        );
        assert_eq!(field_text(fields[1]), "");
        assert!(focus(2));
        let password = FocusTarget::capture(true);
        assert!(password.context().is_empty());
        assert_eq!(
            commit_to_target("never paste here", &password, false),
            Ok(CommitOutcome::ClipboardOnly)
        );
        assert_eq!(field_text(fields[2]), "");
    }
}
