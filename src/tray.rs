//! Tray area icon + options menu (the in-app settings surface).
//!
//! - Icon: generated Utterly microphone artwork with a small state-color badge.
//!   A raw 32×32 RGBA asset keeps the runtime dependency-free.
//! - Menu (via `tray_icon::menu`, i.e. muda — already in the tree, +0 new deps):
//!   Microphone ▸ (system default + cpal devices), Hold-to-talk hotkey ▸
//!   (Space presets), Transcription mode ▸ (Smart / Verbatim),
//!   "Paste API key from clipboard", Quit.
//!
//! Menu events are forwarded to the session thread as `MenuCmd`; radio
//! checkmarks are applied on the main thread (muda items are !Send/!Sync).

use tray_icon::{
    icon::Icon,
    menu::{menu_event_receiver, CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    TrayIcon, TrayIconBuilder,
};

/// Take-history submenu: retention radio choices plus maintenance actions.
/// (Windows users get the richer History page in the settings window; this
/// keeps the tray on parity for every platform.)
struct HistoryItems {
    retention: Vec<(String, CheckMenuItem)>,
    purge: MenuItem,
    clear_all: MenuItem,
    open_folder: MenuItem,
}

/// Commands from the tray menu to the session thread.
#[derive(Debug, Clone)]
pub enum MenuCmd {
    Preferences(crate::config::Preferences),
    /// Open the native settings window on Windows.
    Settings,
    /// Switch mic ("" = system default). Session reopens cpal capture.
    Mic(String),
    /// Switch push-to-talk preset. Session re-registers the global hotkey.
    Hotkey(String),
    /// Switch transcription mode (smart/verbatim). Takes effect on the next
    /// utterance (the model is chosen per Live API session).
    Mode(String),
    /// Add or remove a Gemini speech-biasing phrase.
    DictionaryAdd(String),
    DictionaryRemove(String),
    /// Read the API key from the clipboard (copied from AI Studio) and save it.
    PasteKey,
    /// Set how much take history is kept ("day" | "week" | "month" | "year").
    HistoryRetention(String),
    /// Delete all takes older than the currently selected retention period.
    /// The confirmation dialog lives in the menu handler; the session thread
    /// only performs the (idempotent) purge.
    HistoryPurge,
    /// Delete every saved take (folder contents untouched otherwise).
    HistoryClearAll,
    /// Open the history folder in the platform file manager.
    HistoryOpenFolder,
    /// Play a saved take's audio on the default output device.
    HistoryPlay(String),
    /// Re-run transcription on a saved take's audio (SMART mode) and update
    /// its transcript.txt.
    HistoryRetranscribe(String),
    /// Delete exactly one saved take by directory name.
    HistoryDeleteTake(String),
    Quit,
}

/// Owned menu handles. Must stay alive for the app lifetime (muda items are
/// referenced by id); owned by the main-thread pill loop next to the TrayIcon.
pub struct TrayMenu {
    mic_items: Vec<(String, CheckMenuItem)>,
    hotkey_items: Vec<(String, CheckMenuItem)>,
    mode_items: Vec<(String, CheckMenuItem)>,
    history: HistoryItems,
    settings: Option<MenuItem>,
    paste_key: Option<MenuItem>,
    quit: Option<MenuItem>,
}

impl TrayMenu {
    /// Empty menu for headless/no-display runs: building real GTK items would
    /// panic without `gtk::init`, so we build nothing and the pill runs solo.
    pub fn empty() -> Self {
        Self {
            mic_items: Vec::new(),
            hotkey_items: Vec::new(),
            mode_items: Vec::new(),
            history: HistoryItems {
                retention: Vec::new(),
                purge: MenuItem::new("", false, None),
                clear_all: MenuItem::new("", false, None),
                open_folder: MenuItem::new("", false, None),
            },
            settings: None,
            paste_key: None,
            quit: None,
        }
    }
}

pub fn icon_for(mode: crate::ui::Mode) -> Icon {
    const SIZE: i32 = 32;
    let mut rgba = include_bytes!("../assets/utterly-tray-32.rgba").to_vec();
    let (r, g, b) = match mode {
        crate::ui::Mode::Idle => (0x8E, 0x8E, 0x93),
        crate::ui::Mode::Listening => (0xFF, 0x45, 0x3A),
        crate::ui::Mode::Transcribing => (0x30, 0xD1, 0x58),
    };
    // Status dot in the artwork's lower-right padding.
    for y in 25..31 {
        for x in 25..31 {
            let dx = x - 28;
            let dy = y - 28;
            if dx * dx + dy * dy <= 9 {
                let i = ((y * SIZE + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[r, g, b, 255]);
            }
        }
    }
    Icon::from_rgba(rgba, SIZE as u32, SIZE as u32).expect("32x32 RGBA icon")
}

/// Build tray icon + options menu. `with_menu=false` (Linux without a display)
/// skips all GTK construction and returns an empty menu — the pill keeps
/// working headless instead of panicking in `gtk::Menu::new`.
pub fn build_tray(
    current_mic: &str,
    current_hotkey: &str,
    current_mode: &str,
    current_retention: &str,
    with_menu: bool,
) -> (Option<TrayIcon>, TrayMenu) {
    if !with_menu {
        return (None, TrayMenu::empty());
    }
    let menu = Menu::new();

    // --- Microphone submenu ---
    let mic_sub = Submenu::new("Microphone", true);
    let mut mic_items = Vec::new();
    let mut add_mic = |value: String, label: String, checked: bool| {
        let item = CheckMenuItem::new(label, true, checked, None);
        mic_sub.append(&item);
        mic_items.push((value, item));
    };
    add_mic(
        String::new(),
        "System default".to_string(),
        current_mic.is_empty(),
    );
    for dev in crate::audio::list_mics() {
        let checked = dev == current_mic;
        let label = dev.clone();
        add_mic(dev, label, checked);
    }

    // --- Hotkey submenu ---
    let hk_sub = Submenu::new("Hold-to-talk hotkey", true);
    let mut hotkey_items = Vec::new();
    for preset in crate::hotkey::PRESETS {
        let checked = *preset == crate::hotkey::normalize(current_hotkey);
        let item = CheckMenuItem::new(*preset, true, checked, None);
        hk_sub.append(&item);
        hotkey_items.push((preset.to_string(), item));
    }

    // --- Transcription-mode submenu (Smart cleans ums/ahs; Verbatim is exact) ---
    let mode_sub = Submenu::new("Transcription mode", true);
    let mut mode_items = Vec::new();
    for preset in crate::transcribe::MODES {
        let checked = *preset == crate::transcribe::normalize_mode(current_mode);
        let label = match *preset {
            "smart" => "Smart (clean ums/ahs)".to_string(),
            _ => "Verbatim (exact words)".to_string(),
        };
        let item = CheckMenuItem::new(label, true, checked, None);
        mode_sub.append(&item);
        mode_items.push((preset.to_string(), item));
    }

    let settings = if cfg!(target_os = "windows") {
        Some(MenuItem::new("Settings…", true, None))
    } else {
        None
    };
    let paste_key = MenuItem::new("Paste API key from clipboard", true, None);
    let quit = MenuItem::new("Quit Utterly", true, None);

    // --- History submenu (retention + maintenance) ---
    let hist_sub = Submenu::new("History", true);
    let keep_sub = Submenu::new("Keep history", true);
    let mut retention_items = Vec::new();
    let current = crate::history::Retention::from_key(current_retention);
    for preset in crate::history::Retention::ALL {
        let checked = preset == current;
        let item = CheckMenuItem::new(preset.label(), true, checked, None);
        keep_sub.append(&item);
        retention_items.push((preset.key().to_string(), item));
    }
    hist_sub.append(&keep_sub);
    hist_sub.append(&PredefinedMenuItem::separator());
    let purge = MenuItem::new("Delete history past this period…", true, None);
    let clear_all = MenuItem::new("Delete all history…", true, None);
    let open_folder = MenuItem::new("Open history folder", true, None);
    hist_sub.append(&purge);
    hist_sub.append(&clear_all);
    hist_sub.append(&open_folder);

    if let Some(item) = &settings {
        menu.append(item);
        menu.append(&PredefinedMenuItem::separator());
    }
    menu.append(&mic_sub);
    menu.append(&hk_sub);
    menu.append(&mode_sub);
    menu.append(&hist_sub);
    menu.append(&PredefinedMenuItem::separator());
    menu.append(&paste_key);
    menu.append(&PredefinedMenuItem::separator());
    menu.append(&quit);

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(format!("Utterly — hold {current_hotkey} to dictate"))
        .with_icon(icon_for(crate::ui::Mode::Idle))
        .build()
        .ok();

    (
        tray,
        TrayMenu {
            mic_items,
            hotkey_items,
            mode_items,
            history: HistoryItems {
                retention: retention_items,
                purge,
                clear_all,
                open_folder,
            },
            settings,
            paste_key: Some(paste_key),
            quit: Some(quit),
        },
    )
}

impl TrayMenu {
    /// Snapshot the id→command table. Plain data (u32 + MenuCmd), safe to
    /// move to the listener thread. Muda items themselves are !Sync and stay
    /// on the main thread; checkmarks are applied there via [`TrayMenu::sync`].
    pub fn id_table(&self) -> Vec<(u32, MenuCmd)> {
        let mut table = Vec::new();
        for (value, item) in &self.mic_items {
            table.push((item.id(), MenuCmd::Mic(value.clone())));
        }
        for (preset, item) in &self.hotkey_items {
            table.push((item.id(), MenuCmd::Hotkey(preset.clone())));
        }
        for (preset, item) in &self.mode_items {
            table.push((item.id(), MenuCmd::Mode(preset.clone())));
        }
        for (preset, item) in &self.history.retention {
            table.push((item.id(), MenuCmd::HistoryRetention(preset.clone())));
        }
        table.push((self.history.purge.id(), MenuCmd::HistoryPurge));
        table.push((self.history.clear_all.id(), MenuCmd::HistoryClearAll));
        table.push((self.history.open_folder.id(), MenuCmd::HistoryOpenFolder));
        if let Some(item) = &self.settings {
            table.push((item.id(), MenuCmd::Settings));
        }
        if let Some(p) = &self.paste_key {
            table.push((p.id(), MenuCmd::PasteKey));
        }
        if let Some(q) = &self.quit {
            table.push((q.id(), MenuCmd::Quit));
        }
        table
    }

    /// Radio-checkmark update. Call ONLY on the thread that created the menu
    /// (main thread) — muda items are neither Send nor Sync.
    pub fn sync(&self, mic: &str, hotkey: &str, mode: &str, current_retention: &str) {
        let hk = crate::hotkey::normalize(hotkey);
        let md = crate::transcribe::normalize_mode(mode);
        for (value, item) in &self.mic_items {
            item.set_checked(value == mic);
        }
        for (preset, item) in &self.hotkey_items {
            item.set_checked(*preset == hk);
        }
        for (preset, item) in &self.mode_items {
            item.set_checked(*preset == md);
        }
        let current = crate::history::Retention::from_key(current_retention);
        for (preset, item) in &self.history.retention {
            item.set_checked(crate::history::Retention::from_key(preset) == current);
        }
    }
}

/// Park a tiny thread on the muda event channel; forwards MenuCmd to session.
/// Only the id table (plain data) crosses threads.
pub fn spawn_menu_listener(
    table: Vec<(u32, MenuCmd)>,
    tx: std::sync::mpsc::Sender<MenuCmd>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("utterly-menu".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            while let Ok(ev) = menu_event_receiver().recv() {
                if let Some((_, cmd)) = table.iter().find(|(id, _)| *id == ev.id) {
                    let quit = matches!(cmd, MenuCmd::Quit);
                    let _ = tx.send(cmd.clone());
                    if quit {
                        break;
                    }
                }
            }
        })
}

pub fn set_mode(
    tray: &mut Option<TrayIcon>,
    mode: crate::ui::Mode,
    text_preview: &str,
    hotkey: &str,
) {
    if let Some(t) = tray.as_mut() {
        let _ = t.set_icon(Some(icon_for(mode)));
        let tip = match mode {
            crate::ui::Mode::Idle => format!("Utterly — hold {hotkey} to dictate"),
            crate::ui::Mode::Listening => format!("● Listening… {text_preview}"),
            crate::ui::Mode::Transcribing => format!("… Transcribing {text_preview}"),
        };
        let short: String = tip.chars().take(120).collect();
        let _ = t.set_tooltip(Some(short));
    }
}
