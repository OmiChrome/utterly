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
/// spawned on a tiny thread, all failures ignored. std-only (keeps the
/// <3 MB size goal — no `open`/`rfd` crate).
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
}
