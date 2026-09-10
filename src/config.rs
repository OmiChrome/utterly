//! Persistent config: mic name, hotkey string, AI Studio API key.
//! Fixed-size, stack-friendly struct. Stored at ~/.config/utterly/config.json (0600).

use std::{fs, path::PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    /// Substring match for cpal input device. Empty = default mic.
    pub mic: String,
    /// Human-readable hotkey preset (see hotkey::PRESETS).
    pub hotkey: String,
    /// Gemini API key pasted from Google AI Studio (https://aistudio.google.com/apikey).
    pub api_key: String,
    /// BCP-47 hints, e.g. ["en-US"]. Empty = auto-detect (85+ langs).
    pub language_codes: Vec<String>,
    /// Transcription mode: "smart" (disfluency removal, formatting) or
    /// "verbatim" (exact words + timestamps/diarization-compatible).
    /// serde(default) keeps pre-mode config files loading.
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_mode() -> String {
    "smart".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mic: String::new(),
            hotkey: "Ctrl+Space".to_string(),
            api_key: String::new(),
            language_codes: Vec::new(),
            mode: default_mode(),
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
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(cfg).unwrap_or_else(|_| "{}".to_string());
    fs::write(&path, text)?;
    // Best-effort 0600 so the API key isn't world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}
