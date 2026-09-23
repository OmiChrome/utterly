//! Persistent config: mic name, hotkey string, AI Studio API key.
//! Fixed-size, stack-friendly struct. Stored at ~/.config/utterly/config.json (0600).

use std::{fs, io, path::PathBuf};

pub const MAX_CUSTOM_VOCABULARY: usize = 1000;
const MAX_CUSTOM_TERM_CHARS: usize = 120;

#[cfg(target_os = "windows")]
use base64::Engine as _;

#[cfg(target_os = "windows")]
const PROTECTED_KEY_PREFIX: &str = "dpapi:v1:";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    /// Substring match for cpal input device. Empty = default mic.
    #[serde(default)]
    pub mic: String,
    /// Human-readable hotkey preset (see hotkey::PRESETS).
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    /// Gemini API key pasted from Google AI Studio (https://aistudio.google.com/apikey).
    #[serde(default)]
    pub api_key: String,
    /// BCP-47 hints, e.g. ["en-US"]. Empty = auto-detect (85+ langs).
    #[serde(default)]
    pub language_codes: Vec<String>,
    /// Transcription mode: "smart" (disfluency removal, formatting) or
    /// "verbatim" (exact words + timestamps/diarization-compatible).
    /// serde(default) keeps pre-mode config files loading.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Speech-biasing phrases sent with the next Gemini Live session.
    #[serde(default)]
    pub custom_vocabulary: Vec<String>,
    /// Last pill position (physical px, top-left). None = center-bottom.
    /// serde(default) keeps pre-position config files loading.
    #[serde(default)]
    pub pill_x: Option<i32>,
    #[serde(default)]
    pub pill_y: Option<i32>,
}

fn default_hotkey() -> String {
    "Alt+Space".to_string()
}

fn default_mode() -> String {
    "smart".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mic: String::new(),
            hotkey: "Alt+Space".to_string(),
            api_key: String::new(),
            language_codes: Vec::new(),
            mode: default_mode(),
            custom_vocabulary: Vec::new(),
            pill_x: None,
            pill_y: None,
        }
    }
}

/// Config dir, cross-platform: XDG on Linux, %APPDATA% on Windows,
/// ~/.config everywhere else (incl. macOS).
pub fn config_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("Utterly").join("config.json");
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("utterly").join("config.json");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("utterly")
            .join("config.json");
    }
    #[cfg(target_os = "windows")]
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Roaming")
            .join("Utterly")
            .join("config.json");
    }
    PathBuf::from(".").join("utterly-config.json")
}

pub fn load() -> Config {
    let path = config_path();
    let Ok(text) = fs::read_to_string(path) else {
        return Config::default();
    };
    let Ok(mut cfg) = serde_json::from_str::<Config>(&text) else {
        return Config::default();
    };
    cfg.custom_vocabulary = normalize_custom_vocabulary(cfg.custom_vocabulary);

    #[cfg(target_os = "windows")]
    if let Some(encoded) = cfg.api_key.strip_prefix(PROTECTED_KEY_PREFIX) {
        cfg.api_key = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(io::Error::other)
            .and_then(|bytes| unprotect(&bytes))
            .and_then(|bytes| String::from_utf8(bytes).map_err(io::Error::other))
            .unwrap_or_else(|error| {
                eprintln!("[utterly] couldn't unlock the saved API key: {error}");
                String::new()
            });
    } else if !cfg.api_key.is_empty() {
        // Migrate older plaintext config files to current-user DPAPI storage.
        if let Err(error) = save(&cfg) {
            eprintln!("[utterly] couldn't protect the saved API key: {error}");
        }
    }

    cfg
}

pub fn normalize_custom_term(input: &str) -> Option<String> {
    let term = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if term.is_empty()
        || term.chars().count() > MAX_CUSTOM_TERM_CHARS
        || term.chars().any(char::is_control)
    {
        None
    } else {
        Some(term)
    }
}

pub fn normalize_custom_vocabulary(words: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::with_capacity(words.len().min(MAX_CUSTOM_VOCABULARY));
    for word in words {
        let Some(word) = normalize_custom_term(&word) else {
            continue;
        };
        if normalized
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&word))
        {
            continue;
        }
        normalized.push(word);
        if normalized.len() == MAX_CUSTOM_VOCABULARY {
            break;
        }
    }
    normalized
}

pub fn add_custom_term(cfg: &mut Config, input: &str) -> Result<(), &'static str> {
    let term = normalize_custom_term(input).ok_or("Enter a phrase under 120 characters.")?;
    if cfg
        .custom_vocabulary
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(&term))
    {
        return Err("That phrase is already listed.");
    }
    if cfg.custom_vocabulary.len() >= MAX_CUSTOM_VOCABULARY {
        return Err("The dictionary is limited to 1,000 phrases.");
    }
    cfg.custom_vocabulary.push(term);
    Ok(())
}

pub fn remove_custom_term(cfg: &mut Config, phrase: &str) {
    cfg.custom_vocabulary
        .retain(|existing| !existing.eq_ignore_ascii_case(phrase));
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut stored = cfg.clone();
    #[cfg(target_os = "windows")]
    if !stored.api_key.is_empty() && !stored.api_key.starts_with(PROTECTED_KEY_PREFIX) {
        let encrypted = protect(stored.api_key.as_bytes())?;
        stored.api_key = format!(
            "{PROTECTED_KEY_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(encrypted)
        );
    }
    let text = serde_json::to_string_pretty(&stored).map_err(io::Error::other)?;
    fs::write(&path, text)?;
    // Best-effort 0600 so the API key isn't world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct DataBlob {
    size: u32,
    data: *mut u8,
}

#[cfg(target_os = "windows")]
#[link(name = "crypt32")]
unsafe extern "system" {
    fn CryptProtectData(
        input: *const DataBlob,
        description: *const u16,
        entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void,
        prompt: *const std::ffi::c_void,
        flags: u32,
        output: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        input: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void,
        prompt: *const std::ffi::c_void,
        flags: u32,
        output: *mut DataBlob,
    ) -> i32;
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

#[cfg(target_os = "windows")]
fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    crypt(bytes, true)
}

#[cfg(target_os = "windows")]
fn unprotect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    crypt(bytes, false)
}

#[cfg(target_os = "windows")]
fn crypt(bytes: &[u8], protect: bool) -> io::Result<Vec<u8>> {
    use std::{ptr, slice};

    let size = u32::try_from(bytes.len())
        .map_err(|_| io::Error::other("API key is too large to protect"))?;
    let input = DataBlob {
        size,
        data: bytes.as_ptr() as *mut u8,
    };
    let mut output = DataBlob {
        size: 0,
        data: ptr::null_mut(),
    };
    // DPAPI binds this value to the current Windows user profile.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                0x1, // CRYPTPROTECT_UI_FORBIDDEN
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                0x1, // CRYPTPROTECT_UI_FORBIDDEN
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if output.data.is_null() {
        return Err(io::Error::other(
            "Windows returned an empty protected value",
        ));
    }

    let result = unsafe { slice::from_raw_parts(output.data, output.size as usize).to_vec() };
    unsafe {
        LocalFree(output.data.cast());
    }
    Ok(result)
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn api_key_protection_round_trips_for_current_user() {
        let key = b"AQ.test-key-for-dpapi";
        let protected = protect(key).expect("protect key");
        assert_ne!(protected, key);
        assert_eq!(unprotect(&protected).expect("unprotect key"), key);
    }

    #[test]
    fn save_does_not_double_encrypt_already_protected_key() {
        let key = b"AQ.test-key-for-dpapi";
        let protected = protect(key).expect("protect key");
        let encrypted_str = format!(
            "{PROTECTED_KEY_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(protected)
        );
        let cfg = Config {
            api_key: encrypted_str.clone(),
            ..Config::default()
        };
        let mut stored = cfg.clone();
        if !stored.api_key.is_empty() && !stored.api_key.starts_with(PROTECTED_KEY_PREFIX) {
            let encrypted = protect(stored.api_key.as_bytes()).expect("protect");
            stored.api_key = format!(
                "{PROTECTED_KEY_PREFIX}{}",
                base64::engine::general_purpose::STANDARD.encode(encrypted)
            );
        }
        assert_eq!(stored.api_key, encrypted_str);
    }
}

#[cfg(test)]
mod dictionary_tests {
    use super::*;

    #[test]
    fn terms_collapse_whitespace_and_reject_empty_or_oversized_input() {
        assert_eq!(
            normalize_custom_term("  Ada   Lovelace "),
            Some("Ada Lovelace".into())
        );
        assert_eq!(normalize_custom_term(" \t "), None);
        assert_eq!(normalize_custom_term(&"x".repeat(121)), None);
    }

    #[test]
    fn dictionary_deduplicates_case_insensitively_and_caps_at_1000() {
        let mut cfg = Config::default();
        add_custom_term(&mut cfg, "Gemini").unwrap();
        assert!(add_custom_term(&mut cfg, " gemini ").is_err());
        for index in 0..MAX_CUSTOM_VOCABULARY - 1 {
            add_custom_term(&mut cfg, &format!("term-{index}")).unwrap();
        }
        assert_eq!(cfg.custom_vocabulary.len(), 1000);
        assert!(add_custom_term(&mut cfg, "last phrase").is_err());
        remove_custom_term(&mut cfg, "GEMINI");
        assert_eq!(cfg.custom_vocabulary.len(), 999);
    }

    #[test]
    fn loading_a_legacy_config_defaults_the_dictionary_to_empty() {
        let legacy =
            r#"{"mic":"","hotkey":"Ctrl+Space","api_key":"","language_codes":[],"mode":"smart"}"#;
        let cfg: Config = serde_json::from_str(legacy).unwrap();
        assert!(cfg.custom_vocabulary.is_empty());
    }
}
