//! Output: commit finalized SMART text to the focused textarea.
//! Strategy: clipboard + synthetic paste (Ctrl/Cmd+V) so it works in ANY app's
//! text area without per-app plugins. Also returns the text for the pill window.

/// Copy `text` to system clipboard. Best-effort; returns whether it worked.
pub fn to_clipboard(text: &str) -> bool {
    match arboard::Clipboard::new() {
        Ok(mut cb) => cb.set_text(text.to_string()).is_ok(),
        Err(_) => false,
    }
}

/// Paste clipboard into the focused field via synthetic keystroke
/// (enigo 0.2: `Keyboard::key` + `Direction`). Ctrl+V (Linux/Windows),
/// Cmd+V (macOS).
pub fn paste_into_focused() -> bool {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut e = match Enigo::new(&Settings::default()) {
        Ok(e) => e,
        Err(_) => return false,
    };
    #[cfg(target_os = "macos")]
    let held = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let held = Key::Control;
    e.key(held, Direction::Press).is_ok()
        && e.key(Key::Unicode('v'), Direction::Click).is_ok()
        && e.key(held, Direction::Release).is_ok()
}

/// Full commit: clipboard first (reliable), then synthetic paste (convenient).
/// Sleeps 30 ms between so the OS clipboard settles before Ctrl+V.
pub fn commit(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    if !to_clipboard(text) {
        return false;
    }
    std::thread::sleep(std::time::Duration::from_millis(30));
    paste_into_focused();
    true
}

/// Read a pasted AI Studio key from the clipboard (tray menu:
/// "Paste API key from clipboard"). Accepts anything that looks like a key:
/// ≥12 chars, no internal whitespace. Returns None otherwise.
pub fn read_key_from_clipboard() -> Option<String> {
    let text = arboard::Clipboard::new().ok()?.get_text().ok()?;
    let key = text.trim().to_string();
    if key.len() >= 12 && !key.chars().any(|c| c.is_whitespace()) {
        Some(key)
    } else {
        None
    }
}
