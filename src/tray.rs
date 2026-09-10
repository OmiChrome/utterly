//! Tray area icon + options menu (the in-app settings surface).
//!
//! - Icon: idle (grey) / listening (red) / transcribing (green), generated
//!   PROCEDURALLY (16×16 RGBA, 1 KiB each) — no image files, no `image` crate.
//!   Matches assets/*.svg source art.
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

/// Commands from the tray menu to the session thread.
#[derive(Debug, Clone)]
pub enum MenuCmd {
    /// Switch mic ("" = system default). Session reopens cpal capture.
    Mic(String),
    /// Switch push-to-talk preset. Session re-registers the global hotkey.
    Hotkey(String),
    /// Switch transcription mode (smart/verbatim). Takes effect on the next
    /// utterance (the model is chosen per Live API session).
    Mode(String),
    /// Read the API key from the clipboard (copied from AI Studio) and save it.
    PasteKey,
    Quit,
}

/// Owned menu handles. Must stay alive for the app lifetime (muda items are
/// referenced by id); owned by the main-thread pill loop next to the TrayIcon.
pub struct TrayMenu {
    mic_items: Vec<(String, CheckMenuItem)>,
    hotkey_items: Vec<(String, CheckMenuItem)>,
    mode_items: Vec<(String, CheckMenuItem)>,
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
            paste_key: None,
            quit: None,
        }
    }
}

fn rgba_dot(r: u8, g: u8, b: u8) -> Vec<u8> {
    // 16×16: transparent bg, filled circle r=6 centered, 1px soft edge.
    let mut px = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16i32 {
        for x in 0..16i32 {
            let dx = x - 8;
            let dy = y - 8;
            let d2 = dx * dx + dy * dy;
            let (rr, gg, bb, aa) = if d2 <= 30 {
                (r, g, b, 255)
            } else if d2 <= 42 {
                (r, g, b, 120)
            } else {
                (0, 0, 0, 0)
            };
            px.extend_from_slice(&[rr, gg, bb, aa]);
        }
    }
    px
}

pub fn icon_for(mode: crate::ui::Mode) -> Icon {
    let (r, g, b) = match mode {
        crate::ui::Mode::Idle => (0x8E, 0x8E, 0x93),
        crate::ui::Mode::Listening => (0xFF, 0x45, 0x3A),
        crate::ui::Mode::Transcribing => (0x30, 0xD1, 0x58),
    };
    Icon::from_rgba(rgba_dot(r, g, b), 16, 16).expect("16x16 RGBA icon")
}

/// Build tray icon + options menu. `with_menu=false` (Linux without a display)
/// skips all GTK construction and returns an empty menu — the pill keeps
/// working headless instead of panicking in `gtk::Menu::new`.
pub fn build_tray(
    current_mic: &str,
    current_hotkey: &str,
    current_mode: &str,
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

    let paste_key = MenuItem::new("Paste API key from clipboard", true, None);
    let quit = MenuItem::new("Quit Utterly", true, None);

    menu.append(&mic_sub);
    menu.append(&hk_sub);
    menu.append(&mode_sub);
    menu.append(&PredefinedMenuItem::separator());
    menu.append(&paste_key);
    menu.append(&PredefinedMenuItem::separator());
    menu.append(&quit);

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Utterly — hold Ctrl+Space to dictate")
        .with_icon(icon_for(crate::ui::Mode::Idle))
        .build()
        .ok();

    (
        tray,
        TrayMenu {
            mic_items,
            hotkey_items,
            mode_items,
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
    pub fn sync(&self, mic: &str, hotkey: &str, mode: &str) {
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

pub fn set_mode(tray: &mut Option<TrayIcon>, mode: crate::ui::Mode, text_preview: &str) {
    if let Some(t) = tray.as_mut() {
        let _ = t.set_icon(Some(icon_for(mode)));
        let tip = match mode {
            crate::ui::Mode::Idle => "Utterly — hold Ctrl+Space to dictate".to_string(),
            crate::ui::Mode::Listening => format!("● Listening… {text_preview}"),
            crate::ui::Mode::Transcribing => format!("… Transcribing {text_preview}"),
        };
        let short: String = tip.chars().take(120).collect();
        let _ = t.set_tooltip(Some(short));
    }
}
