//! Utterly — minimal push-to-talk dictation pill.
//!
//! Hold Alt+Space: listen (manual VAD `activityStart`, stream 16 kHz PCM).
//! Release: `audioStreamEnd`, collect SMART-cleaned finals, paste into the
//! focused text area (clipboard + Ctrl/Cmd+V).
//!
//! Constraints honoured:
//! - No async runtime (std threads only), fixed audio buffers and WS frames.
//! - Release profile opt-z + LTO + strip keeps the Windows bundle under 5 MB.

// No console window on double-click exe (Windows GUI subsystem).
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod audio;
mod config;
mod hotkey;
mod output;
mod settings;
mod transcribe;
mod tray;
mod ui;

use std::sync::mpsc;
use std::time::{Duration, Instant};

struct HotkeySession {
    events: mpsc::Receiver<hotkey::KeyEvent>,
    requests: mpsc::Sender<String>,
    results: mpsc::Receiver<(String, Result<(), String>)>,
    initial_error: Option<String>,
}

fn print_help() {
    println!(
        "Utterly — Gemini 3.5 Transcribe Live dictation pill\n\
         \n\
         Usage:\n  \
           utterly [--list-mics] [--set-mic NAME] [--set-hotkey HOTKEY]\n  \
                    [--set-key] [--set-mode smart|verbatim] [--help]\n\
         \n\
         Run with no flags: pill window + tray icon. Hold Alt+Space to dictate,\n  \
         release to transcribe into the focused text area.\n\
         Mic, hotkey, transcription mode (smart/verbatim) and API key can also\n  \
         be changed live from the tray-icon menu.\n\
         \n\
         Setup:\n  \
           1. Get a key at https://aistudio.google.com/apikey\n  \
           2. Copy the key, then run utterly --set-key\n  \
           3. utterly --list-mics / --set-mic \"MacBook Pro Microphone\"\n\
         \n\
         Config: ~/.config/utterly/config.json (0600 on Unix)\n  \
         (Windows: %APPDATA%\\Utterly\\config.json; API key protected with DPAPI)"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }
    if args.iter().any(|a| a == "--list-mics") {
        for m in audio::list_mics() {
            println!("{m}");
        }
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--set-mic") {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        let mut c = config::load();
        c.mic = v;
        config::save(&c).expect("save config");
        println!("mic saved");
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--set-hotkey") {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        let mut c = config::load();
        c.hotkey = hotkey::normalize(&v).to_string();
        config::save(&c).expect("save config");
        println!(
            "hotkey saved: {} (presets: {})",
            c.hotkey,
            hotkey::PRESETS.join(", ")
        );
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--set-key") {
        if args.get(i + 1).is_some_and(|value| !value.starts_with('-')) {
            eprintln!(
                "[utterly] don't pass API keys as command-line arguments; copy the key and run `utterly --set-key`"
            );
            std::process::exit(2);
        }
        let Some(key) = output::read_key_from_clipboard() else {
            eprintln!("[utterly] clipboard has no valid API key; copy it from AI Studio first");
            std::process::exit(2);
        };
        let mut c = config::load();
        c.api_key = key;
        if let Err(error) = config::save(&c) {
            eprintln!("[utterly] couldn't save API key: {error}");
            std::process::exit(1);
        }
        println!("API key saved");
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--set-mode") {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        let mut c = config::load();
        c.mode = transcribe::normalize_mode(&v).to_string();
        config::save(&c).expect("save config");
        println!(
            "mode saved: {} (smart|verbatim; applies to the next utterance)",
            c.mode
        );
        return;
    }

    let cfg = config::load();

    // Instant cold-start: initiate background Gemini Live WebSocket connection immediately
    // at startup while audio capture, tray icon, and window creation run in parallel.
    let (warm_tx, warm_rx) = mpsc::channel::<(transcribe::Ws, Instant)>();
    if !cfg.api_key.trim().is_empty() {
        spawn_prewarm(
            cfg.api_key.clone(),
            cfg.language_codes.clone(),
            cfg.mode.clone(),
            cfg.custom_vocabulary.clone(),
            warm_tx.clone(),
        );
    }

    // Single instance: a second copy would silently lose the global-hotkey
    // grab race (X11 reports BadAccess asynchronously) and look dead while
    // the older copy eats every press. Refuse loudly instead.
    if !claim_instance() {
        eprintln!("[utterly] another Utterly instance is already running — quitting.");
        std::process::exit(1);
    }

    // Process-lifetime heartbeat: session_loop also retouches every ~30 s,
    // but its mic/hotkey early-returns never reach that tick — without this
    // a live error-state pill would go stale and become stealable after 90 s
    // on Windows (pid_alive falls back to lock age off-Linux). One tiny
    // write per 30 s, ~0% idle CPU.
    let _heartbeat = std::thread::Builder::new()
        .name("utterly-instance-heartbeat".into())
        .stack_size(256 * 1024)
        .spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(30));
            touch_instance();
        });

    // UI channel: session thread -> pill window (main thread).
    let (pill_tx, pill_rx) = mpsc::channel::<ui::PillUpdate>();
    // UI command channel: pill window -> session thread (mic click-to-talk,
    // hide-to-tray notices).
    let (ui_cmd_tx, ui_cmd_rx) = mpsc::channel::<ui::UiCmd>();
    // Menu channel: tray menu thread -> session thread.
    let (menu_tx, menu_rx) = mpsc::channel::<tray::MenuCmd>();
    #[cfg(target_os = "windows")]
    let initial_settings = settings::Snapshot {
        mode: cfg.mode.clone(),
        hotkey: cfg.hotkey.clone(),
        vocabulary: cfg.custom_vocabulary.clone(),
    };
    #[cfg(target_os = "windows")]
    let settings_window = match settings::SettingsWindow::spawn(initial_settings, menu_tx.clone()) {
        Ok(window) => Some(window),
        Err(error) => {
            eprintln!("[utterly] settings window: {error}");
            None
        }
    };
    #[cfg(not(target_os = "windows"))]
    let settings_window: Option<settings::SettingsWindow> = None;
    // Menu-sync channel: session -> main thread (radio checkmarks; muda items
    // are !Send/!Sync so only the main thread touches them).
    let (sync_tx, sync_rx) = mpsc::channel::<(String, String, String)>();
    let (hotkey_events_tx, hotkey_events_rx) = mpsc::channel();
    let (hotkey_requests_tx, hotkey_requests_rx) = mpsc::channel();
    let (hotkey_results_tx, hotkey_results_rx) = mpsc::channel();

    // macOS needs its manager on the main event-loop thread; the Windows
    // backend owns its own message-pumping thread for RegisterHotKey events.
    let (hotkey, hotkey_error) = match hotkey::Hotkey::register(&cfg.hotkey) {
        Ok(manager) => (Some(manager), None),
        Err(error) => {
            eprintln!("[utterly] hotkey: {error}");
            (None, Some(error))
        }
    };

    // Tray + options menu are built AND owned on the MAIN thread.
    // Linux tray menus are GTK-based: init GTK first. Without a display
    // (headless/SSH) gtk::init fails and we run pill-only — never panicking
    // inside gtk::Menu::new. Other OSes always build the menu.
    #[cfg(target_os = "linux")]
    let gtk_ready: bool = gtk::init().is_ok();
    #[cfg(not(target_os = "linux"))]
    let gtk_ready: bool = true;
    let (tray, menu) = tray::build_tray(&cfg.mic, &cfg.hotkey, &cfg.mode, gtk_ready);
    let _menu_thread = tray::spawn_menu_listener(menu.id_table(), menu_tx);

    let initial_hotkey = cfg.hotkey.clone();
    let initial = format!("Utterly — Hold {initial_hotkey} to dictate");
    let _ = sync_tx.send((cfg.mic.clone(), cfg.hotkey.clone(), cfg.mode.clone()));
    let settings_for_session = settings_window.clone();

    // ---- session thread (audio + hotkey + websocket) ----
    std::thread::Builder::new()
        .name("utterly-session".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            session_loop(
                cfg,
                pill_tx,
                menu_rx,
                sync_tx,
                ui_cmd_rx,
                settings_for_session,
                HotkeySession {
                    events: hotkey_events_rx,
                    requests: hotkey_requests_tx,
                    results: hotkey_results_rx,
                    initial_error: hotkey_error,
                },
                warm_tx,
                warm_rx,
            )
        })
        .expect("spawn session");

    // ---- main thread: pill window (winit must own the main thread) ----
    let services = ui::UiServices {
        hotkey,
        hotkey_events: hotkey_events_tx,
        hotkey_requests: hotkey_requests_rx,
        hotkey_results: hotkey_results_tx,
        tray,
        menu,
        sync_rx,
        gtk_pump: gtk_ready,
        ui_cmd_tx,
        settings: settings_window,
    };
    if let Err(e) = ui::run_pill(pill_rx, initial, services) {
        eprintln!("[utterly] pill: {e}");
    }
    release_instance();
}

/// Feed the capture watchdog from a successful take: any nonzero energy
/// proves the stream is alive (real mics always have a noise floor).
fn note_take(
    rms: f32,
    silent_takes: &mut u32,
    last_take_ok: &mut Instant,
    reopen_wait: &mut Duration,
) {
    *last_take_ok = Instant::now();
    if rms == 0.0 {
        *silent_takes += 1;
    } else {
        *silent_takes = 0;
        *reopen_wait = Duration::from_secs(5);
    }
}

fn push_pill(tx: &mpsc::Sender<ui::PillUpdate>, mode: ui::Mode, level: f32, title: &str) {
    push_pill_verbatim(tx, mode, level, title, false);
}

/// `verbatim_live` marks live-verbatim sessions (finals stream in over the
/// websocket, so the pill shows a meter instead of the Transcribing
/// spinner). Callers in `session_loop` pass `cfg.mode == "verbatim"`;
/// plain `push_pill` defaults to false (smart/idle).
fn push_pill_verbatim(
    tx: &mpsc::Sender<ui::PillUpdate>,
    mode: ui::Mode,
    level: f32,
    title: &str,
    verbatim_live: bool,
) {
    let _ = tx.send(ui::PillUpdate {
        mode,
        level,
        title: title.to_string(),
        verbatim_live,
    });
}

fn refresh_settings(window: &Option<settings::SettingsWindow>, cfg: &config::Config) {
    if let Some(window) = window {
        window.update(settings::Snapshot {
            mode: cfg.mode.clone(),
            hotkey: cfg.hotkey.clone(),
            vocabulary: cfg.custom_vocabulary.clone(),
        });
    }
}

fn spawn_prewarm(
    key: String,
    langs: Vec<String>,
    mode: String,
    vocab: Vec<String>,
    tx: mpsc::Sender<(transcribe::Ws, Instant)>,
) {
    let _ = std::thread::Builder::new()
        .name("utterly-prewarm".into())
        .stack_size(512 * 1024)
        .spawn(move || {
            if let Ok(w) = transcribe::connect_live(&key, &langs, &mode, &vocab) {
                let _ = tx.send((w, Instant::now()));
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn session_loop(
    mut cfg: config::Config,
    pill_tx: mpsc::Sender<ui::PillUpdate>,
    menu_rx: mpsc::Receiver<tray::MenuCmd>,
    sync_tx: mpsc::Sender<(String, String, String)>,
    ui_cmd_rx: mpsc::Receiver<ui::UiCmd>,
    settings_window: Option<settings::SettingsWindow>,
    hotkeys: HotkeySession,
    warm_tx: mpsc::Sender<(transcribe::Ws, Instant)>,
    warm_rx: mpsc::Receiver<(transcribe::Ws, Instant)>,
) {
    let HotkeySession {
        events: hotkey_rx,
        requests: hotkey_request_tx,
        results: hotkey_result_rx,
        initial_error: hotkey_init_error,
    } = hotkeys;
    if cfg.api_key.trim().is_empty() {
        push_pill(
            &pill_tx,
            ui::Mode::Idle,
            0.0,
            "Utterly — copy your AI Studio key, then run utterly --set-key",
        );
        eprintln!("[utterly] no API key. Copy one from AI Studio, then run: utterly --set-key");
        // First-run onboarding (std-only, best-effort, never blocking):
        // auto-open the AI Studio key page, then tell the user exactly what
        // to do. A clipboard poll below picks the key up without a restart.
        output::open_browser(output::AI_STUDIO_URL);
        eprintln!(
            "[utterly] Get a key at {} then copy it; paste via the tray menu \
             \"Paste API key from clipboard\" or just copy — it is picked up automatically.",
            output::AI_STUDIO_URL
        );
    }

    let mut cap = match audio::Capture::open(&cfg.mic) {
        Ok(c) => c,
        Err(e) => {
            push_pill(
                &pill_tx,
                ui::Mode::Idle,
                0.0,
                &format!("Utterly — mic error: {e}"),
            );
            eprintln!("[utterly] mic: {e}");
            return;
        }
    };

    println!(
        "[utterly] ready. Hold {} to dictate ({} mode).",
        cfg.hotkey, cfg.mode
    );
    // When the key is missing the onboarding pill above stays up (plus the
    // clipboard poll below) instead of being clobbered by the ready message.
    if cfg.api_key.trim().is_empty() {
        push_pill(
            &pill_tx,
            ui::Mode::Idle,
            0.0,
            "Utterly — no API key: copy one from https://aistudio.google.com/apikey, it is picked up automatically",
        );
    } else {
        push_pill(
            &pill_tx,
            ui::Mode::Idle,
            0.0,
            &format!("Utterly — Hold {} to dictate", cfg.hotkey),
        );
    }
    if let Some(error) = hotkey_init_error {
        push_pill(
            &pill_tx,
            ui::Mode::Idle,
            0.0,
            &format!("Utterly — hotkey error: {error}"),
        );
    }

    // UI preview hook (also handy for screenshots): UTTERLY_DEMO=listening
    // or =transcribing repaints that pill state every ~2 s, overriding idle
    // meter and watchdog notes. Normal operation is unaffected.
    let demo_mode: Option<ui::Mode> = match std::env::var("UTTERLY_DEMO").as_deref() {
        Ok("listening") => Some(ui::Mode::Listening),
        Ok("transcribing") => Some(ui::Mode::Transcribing),
        _ => None,
    };
    let mut last_demo = Instant::now() - Duration::from_secs(10);

    let mut recording = false;
    let mut ws: Option<transcribe::Ws> = None;
    let mut ws_fresh = Instant::now() - Duration::from_secs(3600);
    let mut last_ping = Instant::now();
    let mut finals: Vec<String> = Vec::new();
    let mut interim = String::new();
    let mut last_idle_push = Instant::now();
    let mut last_prewarm = Instant::now();
    // Capture watchdog: a live mic always has a noise floor, so sustained
    // BIT-EXACT digital silence (rms == 0.0) means the stream went stale
    // (observed on PipeWire/ALSA bridges: audio flows, then zeros forever
    // with no error, while fresh opens work). Reopen with backoff.
    let mut silent_takes: u32 = 0;
    let mut last_take_ok = Instant::now();
    let mut cap_age = Instant::now();
    let mut last_reopen = Instant::now() - Duration::from_secs(60);
    let mut reopen_wait = Duration::from_secs(5);
    let mut last_touch = Instant::now();
    // First-run key poll: check the clipboard every 2 s until a key appears
    // (sleep-based, ~0% idle). Armed in the past so the first tick fires
    // immediately after the mic/hotkey setup below.
    let mut last_key_poll = Instant::now() - Duration::from_secs(10);
    // Reused per-tick event buffer (hotkey + pill toggles); cleared each tick
    // so the 10 ms loop never allocates when idle.
    enum Src {
        Hk(hotkey::KeyEvent),
        Toggle,
    }
    let mut evs: Vec<Src> = Vec::new();

    loop {
        // First-run onboarding: pick up a copied AI Studio key without a
        // restart. Same >=12-chars/no-whitespace rule as the tray PasteKey
        // flow; --set-key and tray flows are untouched.
        if cfg.api_key.trim().is_empty() && last_key_poll.elapsed() >= Duration::from_secs(2) {
            last_key_poll = Instant::now();
            if let Some(k) = output::read_key_from_clipboard() {
                cfg.api_key = k;
                let _ = config::save(&cfg);
                // Key changed: drop any spare/kept socket built with no key.
                ws = None;
                while warm_rx.try_recv().is_ok() {}
                spawn_prewarm(
                    cfg.api_key.clone(),
                    cfg.language_codes.clone(),
                    cfg.mode.clone(),
                    cfg.custom_vocabulary.clone(),
                    warm_tx.clone(),
                );
                push_pill(
                    &pill_tx,
                    ui::Mode::Idle,
                    0.0,
                    &format!("Utterly — API key saved, hold {} to dictate", cfg.hotkey),
                );
                println!("[utterly] API key saved from clipboard");
            }
        }
        // Instance heartbeat (see pid_alive): retouch the lock every ~30 s.
        if last_touch.elapsed() >= Duration::from_secs(30) {
            touch_instance();
            last_touch = Instant::now();
        }
        // Demo ticker (see above).
        if let Some(dm) = demo_mode {
            if last_demo.elapsed() >= Duration::from_millis(500) {
                last_demo = Instant::now();
                match dm {
                    ui::Mode::Listening => push_pill_verbatim(
                        &pill_tx,
                        ui::Mode::Listening,
                        3000.0,
                        "Utterly ● Listening… (demo preview)",
                        cfg.mode == "verbatim",
                    ),
                    _ => push_pill_verbatim(
                        &pill_tx,
                        ui::Mode::Transcribing,
                        500.0,
                        "Utterly … transcribing Transcribing… (demo preview)",
                        cfg.mode == "verbatim",
                    ),
                }
            }
        }
        // --- capture watchdog: reopen a stale-silent stream (see above) ---
        let stream_old_enough = cap_age.elapsed() >= Duration::from_secs(5);
        let starved = last_take_ok.elapsed() >= Duration::from_secs(3) && stream_old_enough;
        if demo_mode.is_none() && last_reopen.elapsed() >= reopen_wait && (silent_takes >= 30 || starved) {
            match audio::Capture::open(&cfg.mic) {
                Ok(c) => {
                    eprintln!(
                        "[utterly] mic stream reopened ({}) after digital silence (watchdog)",
                        c.fmt_desc
                    );
                    cap = c;
                    cap_age = Instant::now();
                    silent_takes = 0;
                    last_take_ok = Instant::now();
                    last_reopen = Instant::now();
                    reopen_wait = (reopen_wait * 2).min(Duration::from_secs(60));
                    push_pill(
                        &pill_tx,
                        ui::Mode::Idle,
                        0.0,
                        &format!(
                            "Utterly — mic stream recovered, hold {} to dictate",
                            cfg.hotkey
                        ),
                    );
                }
                Err(e) => {
                    last_reopen = Instant::now();
                    eprintln!("[utterly] mic reopen failed: {e}");
                }
            }
        }
        // --- tray menu commands (mic / hotkey / API key / quit) ---
        while let Ok(cmd) = menu_rx.try_recv() {
            match cmd {
                tray::MenuCmd::Settings => {
                    if let Some(window) = &settings_window {
                        window.show();
                    }
                }
                tray::MenuCmd::Mic(name) => {
                    cfg.mic = name.clone();
                    match audio::Capture::open(&cfg.mic) {
                        Ok(c) => {
                            cap = c;
                            let _ = config::save(&cfg);
                            let _ = sync_tx.send((
                                cfg.mic.clone(),
                                cfg.hotkey.clone(),
                                cfg.mode.clone(),
                            ));
                            let shown = if name.is_empty() {
                                "System default".to_string()
                            } else {
                                name
                            };
                            push_pill(
                                &pill_tx,
                                ui::Mode::Idle,
                                0.0,
                                &format!("Utterly — mic: {shown}"),
                            );
                            println!("[utterly] mic: {shown}");
                        }
                        Err(e) => {
                            push_pill(
                                &pill_tx,
                                ui::Mode::Idle,
                                0.0,
                                &format!("Utterly — mic error: {e}"),
                            );
                        }
                    }
                }
                tray::MenuCmd::Hotkey(preset) => {
                    if hotkey_request_tx.send(preset).is_err() {
                        push_pill(
                            &pill_tx,
                            ui::Mode::Idle,
                            0.0,
                            "Utterly — couldn't reach the hotkey manager",
                        );
                    }
                }
                tray::MenuCmd::Mode(mode) => {
                    cfg.mode = transcribe::normalize_mode(&mode).to_string();
                    let _ = config::save(&cfg);
                    // Model/wire format changed: drop the kept-alive socket
                    // and any spare so the next press connects fresh.
                    ws = None;
                    while warm_rx.try_recv().is_ok() {}
                    let _ = sync_tx.send((cfg.mic.clone(), cfg.hotkey.clone(), cfg.mode.clone()));
                    refresh_settings(&settings_window, &cfg);
                    let what = if cfg.mode == "verbatim" {
                        "Verbatim — exact words"
                    } else {
                        "Smart — ums/ahs removed, formatted"
                    };
                    push_pill(
                        &pill_tx,
                        ui::Mode::Idle,
                        0.0,
                        &format!("Utterly — mode: {what} (next utterance)"),
                    );
                    println!("[utterly] mode: {}", cfg.mode);
                }
                tray::MenuCmd::DictionaryAdd(phrase) => {
                    match config::add_custom_term(&mut cfg, &phrase) {
                        Ok(()) => {
                            if let Err(error) = config::save(&cfg) {
                                eprintln!("[utterly] couldn't save dictionary: {error}");
                            }
                            // Vocabulary is baked at setup: drop sockets.
                            ws = None;
                            while warm_rx.try_recv().is_ok() {}
                            refresh_settings(&settings_window, &cfg);
                            push_pill(
                                &pill_tx,
                                ui::Mode::Idle,
                                0.0,
                                "Utterly — dictionary phrase added for the next utterance",
                            );
                        }
                        Err(error) => {
                            push_pill(&pill_tx, ui::Mode::Idle, 0.0, &format!("Utterly — {error}"));
                        }
                    }
                }
                tray::MenuCmd::DictionaryRemove(phrase) => {
                    config::remove_custom_term(&mut cfg, &phrase);
                    if let Err(error) = config::save(&cfg) {
                        eprintln!("[utterly] couldn't save dictionary: {error}");
                    }
                    ws = None;
                    while warm_rx.try_recv().is_ok() {}
                    refresh_settings(&settings_window, &cfg);
                }
                tray::MenuCmd::PasteKey => match output::read_key_from_clipboard() {
                    Some(k) => {
                        cfg.api_key = k;
                        let _ = config::save(&cfg);
                        ws = None;
                        while warm_rx.try_recv().is_ok() {}
                        spawn_prewarm(
                            cfg.api_key.clone(),
                            cfg.language_codes.clone(),
                            cfg.mode.clone(),
                            cfg.custom_vocabulary.clone(),
                            warm_tx.clone(),
                        );
                        push_pill(
                            &pill_tx,
                            ui::Mode::Idle,
                            0.0,
                            &format!("Utterly — API key saved, hold {} to dictate", cfg.hotkey),
                        );
                        println!("[utterly] API key saved from clipboard");
                    }
                    None => {
                        push_pill(
                            &pill_tx,
                            ui::Mode::Idle,
                            0.0,
                            "Utterly — clipboard has no key (copy it from AI Studio first)",
                        );
                    }
                },
                tray::MenuCmd::Quit => {
                    release_instance();
                    std::process::exit(0);
                }
            }
        }
        while let Ok((preset, result)) = hotkey_result_rx.try_recv() {
            match result {
                Ok(()) => {
                    cfg.hotkey = preset;
                    if let Err(error) = config::save(&cfg) {
                        eprintln!("[utterly] couldn't save hotkey: {error}");
                    }
                    let _ = sync_tx.send((cfg.mic.clone(), cfg.hotkey.clone(), cfg.mode.clone()));
                    refresh_settings(&settings_window, &cfg);
                    push_pill(
                        &pill_tx,
                        ui::Mode::Idle,
                        0.0,
                        &format!("Utterly — hold {} to dictate ({})", cfg.hotkey, cfg.mode),
                    );
                    println!("[utterly] hotkey: {}", cfg.hotkey);
                }
                Err(error) => {
                    push_pill(
                        &pill_tx,
                        ui::Mode::Idle,
                        0.0,
                        &format!("Utterly — hotkey error: {error}"),
                    );
                }
            }
        }
        // --- hotkey events (press/release) + pill mic-click toggles ---
        // MicToggle feeds the SAME match arms below (single code path, no
        // behavior drift): idle -> Pressed, recording -> Released. The mapping
        // is resolved per event so queued toggles track `recording` exactly.
        // Hide is already applied in the UI thread; the session ignores it.
        evs.clear();
        while let Ok(ev) = hotkey_rx.try_recv() {
            evs.push(Src::Hk(ev));
        }
        while let Ok(cmd) = ui_cmd_rx.try_recv() {
            match cmd {
                ui::UiCmd::Hide => {}
                ui::UiCmd::MicToggle => evs.push(Src::Toggle),
            }
        }
        for src in evs.drain(..) {
            let ev = match src {
                Src::Hk(ev) => ev,
                Src::Toggle if recording => hotkey::KeyEvent::Released,
                Src::Toggle => hotkey::KeyEvent::Pressed,
            };
            match ev {
                hotkey::KeyEvent::Pressed => {
                    if recording || cfg.api_key.trim().is_empty() {
                        continue;
                    }
                    // Fresh utterance: drop pre-roll so old audio can't leak in.
                    if let Ok(mut r) = cap.ring.lock() {
                        r.clear();
                    }
                    // Drain stale chunk signals.
                    while cap.ready_rx.try_recv().is_ok() {}
                    // Drop a kept-alive socket older than 5 min (server caps
                    // sessions at ~10 min; a dead-idle socket costs a press).
                    if ws.is_some() && ws_fresh.elapsed() > Duration::from_secs(300) {
                        ws = None;
                    }
                    // Prefer the pre-warmed socket when fresh (<5 min);
                    // otherwise connect on the press path (existing behavior).
                    let mut warm = None;
                    if ws.is_none() {
                        while let Ok((w, created)) = warm_rx.try_recv() {
                            if created.elapsed() <= Duration::from_secs(300) {
                                warm = Some(w);
                            }
                        }
                    }
                    let connected = if let Some(w) = warm.take() {
                        ws = Some(w);
                        ws_fresh = Instant::now();
                        last_ping = Instant::now();
                        // Re-arm the spare for the next press.
                        spawn_prewarm(
                            cfg.api_key.clone(),
                            cfg.language_codes.clone(),
                            cfg.mode.clone(),
                            cfg.custom_vocabulary.clone(),
                            warm_tx.clone(),
                        );
                        true
                    } else if ws.is_some() {
                        // Reuse the kept-alive socket across utterances.
                        true
                    } else {
                        match transcribe::connect_live(
                            &cfg.api_key,
                            &cfg.language_codes,
                            &cfg.mode,
                            &cfg.custom_vocabulary,
                        ) {
                            Ok(w) => {
                                ws = Some(w);
                                ws_fresh = Instant::now();
                                last_ping = Instant::now();
                                true
                            }
                            Err(e) => {
                                push_pill(
                                    &pill_tx,
                                    ui::Mode::Idle,
                                    0.0,
                                    &format!("Utterly — connect failed: {e}"),
                                );
                                eprintln!("[utterly] connect: {e}");
                                false
                            }
                        }
                    };
                    if !connected {
                        continue;
                    }
                    if let Some(w) = ws.as_mut() {
                        // Manual turn bracketing, nested inside realtimeInput
                        // (a top-level activityStart gets the session closed
                        // with "Unknown name activityStart").
                        if let Err(e) = transcribe::send_activity_start(w) {
                            eprintln!("[utterly] activityStart failed on cached socket: {e}; retrying fresh connection");
                            ws = None;
                            let mut warm = None;
                            while let Ok((w_sub, created)) = warm_rx.try_recv() {
                                if created.elapsed() <= Duration::from_secs(300) {
                                    warm = Some(w_sub);
                                }
                            }
                            let reconnected = if let Some(w_sub) = warm.take() {
                                spawn_prewarm(
                                    cfg.api_key.clone(),
                                    cfg.language_codes.clone(),
                                    cfg.mode.clone(),
                                    cfg.custom_vocabulary.clone(),
                                    warm_tx.clone(),
                                );
                                Some(w_sub)
                            } else {
                                match transcribe::connect_live(
                                    &cfg.api_key,
                                    &cfg.language_codes,
                                    &cfg.mode,
                                    &cfg.custom_vocabulary,
                                ) {
                                    Ok(w_new) => Some(w_new),
                                    Err(err) => {
                                        push_pill(
                                            &pill_tx,
                                            ui::Mode::Idle,
                                            0.0,
                                            &format!("Utterly — connect retry failed: {err}"),
                                        );
                                        eprintln!("[utterly] reconnect: {err}");
                                        None
                                    }
                                }
                            };
                            if let Some(mut fresh_ws) = reconnected {
                                if let Err(err2) = transcribe::send_activity_start(&mut fresh_ws) {
                                    push_pill(
                                        &pill_tx,
                                        ui::Mode::Idle,
                                        0.0,
                                        &format!("Utterly — couldn't start transcription: {err2}"),
                                    );
                                    eprintln!("[utterly] retry activityStart: {err2}");
                                    ws = None;
                                    continue;
                                }
                                ws = Some(fresh_ws);
                                ws_fresh = Instant::now();
                                last_ping = Instant::now();
                            } else {
                                continue;
                            }
                        }
                        recording = true;
                        finals.clear();
                        interim.clear();
                        push_pill_verbatim(
                            &pill_tx,
                            ui::Mode::Listening,
                            0.0,
                            "Utterly ●",
                            cfg.mode == "verbatim",
                        );
                    }
                }
                hotkey::KeyEvent::Released => {
                    if !recording {
                        continue;
                    }
                    recording = false;
                    push_pill_verbatim(
                        &pill_tx,
                        ui::Mode::Transcribing,
                        0.0,
                        &format!("Utterly … transcribing {}", interim_short(&interim)),
                        cfg.mode == "verbatim",
                    );
                    // Flush remaining buffered audio, then end the turn + stream.
                    let mut session_closed = false;
                    if let Some(w) = ws.as_mut() {
                        while let Some((chunk, rms)) = cap.take_chunk() {
                            note_take(rms, &mut silent_takes, &mut last_take_ok, &mut reopen_wait);
                            if rms >= audio::SILENCE_RMS {
                                let _ = transcribe::send_pcm(w, &chunk);
                            }
                        }
                        // End the manual turn, then end the audio stream.
                        let _ = transcribe::send_activity_end(w);
                        let _ = transcribe::send_audio_end(w);
                        // Grace period: SMART finals arrive after the turn end.
                        // Early-exit on server close or 500ms quiet after the
                        // first final; 4000ms hard cap preserves old behavior.
                        // Live preview keeps streaming here, on-change only.
                        let grace_start = Instant::now();
                        let mut last_activity = grace_start;
                        let mut last_preview = String::new();
                        while grace_start.elapsed() < Duration::from_millis(4000) {
                            if let Some(ev) = transcribe::recv_timeout(w, 100) {
                                let closed = ev.closed;
                                if closed {
                                    session_closed = true;
                                }
                                if let Some(t) = ev.interim {
                                    interim = t;
                                    last_activity = Instant::now();
                                }
                                if let Some(t) = ev.finalized {
                                    push_final(&mut finals, t);
                                    last_activity = Instant::now();
                                }
                                let preview = combine_transcript(&finals, &interim);
                                if preview != last_preview {
                                    last_preview = preview.clone();
                                    let short: String = preview.chars().take(80).collect();
                                    push_pill_verbatim(
                                        &pill_tx,
                                        ui::Mode::Transcribing,
                                        0.0,
                                        &format!("Utterly … transcribing {short}"),
                                        cfg.mode == "verbatim",
                                    );
                                }
                                let elapsed_ms = grace_start.elapsed().as_millis() as u64;
                                let quiet_ms = last_activity.elapsed().as_millis() as u64;
                                if should_stop_grace(
                                    closed,
                                    !finals.is_empty(),
                                    quiet_ms,
                                    elapsed_ms,
                                ) {
                                    break;
                                }
                            } else if should_stop_grace(
                                false,
                                !finals.is_empty(),
                                last_activity.elapsed().as_millis() as u64,
                                grace_start.elapsed().as_millis() as u64,
                            ) {
                                break;
                            }
                        }
                    }
                    // Keep the socket across utterances for instant next press;
                    // drop only when the server closed it.
                    if session_closed {
                        ws = None;
                    }
                    let text = combine_transcript(&finals, &interim);
                    let text = clean_tags(&text);
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        push_pill(
                            &pill_tx,
                            ui::Mode::Idle,
                            0.0,
                            "Utterly — heard nothing, try again",
                        );
                    } else {
                        let shown: String = text.chars().take(80).collect();
                        let title = match output::commit(&text) {
                            Ok(()) => format!("Utterly — {shown}"),
                            Err(output::CommitError::ClipboardUnavailable) => {
                                "Utterly — couldn't access the clipboard".to_string()
                            }
                            Err(output::CommitError::PasteFailed) => {
                                format!(
                                    "Utterly — paste failed; transcript is on clipboard: {shown}"
                                )
                            }
                        };
                        push_pill(&pill_tx, ui::Mode::Idle, 0.0, &title);
                        println!("[utterly] {text}");
                    }
                    interim.clear();
                }
            }
        }

        if recording {
            let mut level: f32 = 0.0;
            // Drain ALL ready chunks (10 msgs/sec steady state), skip silence.
            if let Some(w) = ws.as_mut() {
                let mut sent = 0;
                while let Some((chunk, rms)) = cap.take_chunk() {
                    note_take(rms, &mut silent_takes, &mut last_take_ok, &mut reopen_wait);
                    level = rms;
                    if rms >= audio::SILENCE_RMS {
                        if transcribe::send_pcm(w, &chunk).is_err() {
                            break; // server went away; release path finalizes
                        }
                        sent += 1;
                    }
                    if sent >= 4 {
                        break; // never hog the tick; keeps UI @30fps
                    }
                }
                // Poll server without blocking the audio path.
                let mut got_update = false;
                for _ in 0..3 {
                    match transcribe::recv_timeout(w, 5) {
                        Some(ev) => {
                            if let Some(t) = ev.interim {
                                interim = t;
                                got_update = true;
                            }
                            if let Some(t) = ev.finalized {
                                push_final(&mut finals, t);
                                got_update = true;
                            }
                            if ev.closed {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                if got_update || level > 0.0 {
                    let preview = combine_transcript(&finals, &interim);
                    let short: String = preview.chars().take(80).collect();
                    push_pill_verbatim(
                        &pill_tx,
                        ui::Mode::Listening,
                        level,
                        &format!("Utterly ● {short}"),
                        cfg.mode == "verbatim",
                    );
                }
            }
            std::thread::sleep(tick_interval(recording));
        } else {
            // Idle keepalive ping every 20s to prevent Google WebSocket timeout
            if let Some(w) = ws.as_mut() {
                if last_ping.elapsed() >= Duration::from_secs(20) {
                    last_ping = Instant::now();
                    if let Err(e) = transcribe::send_ping(w) {
                        eprintln!("[utterly] keepalive ping failed: {e}; dropping stale socket");
                        ws = None;
                    }
                }
            }
            // Idle: drop stale WebSocket (>4 minutes) so background prewarm refreshes it before next press
            if ws.is_some() && ws_fresh.elapsed() > Duration::from_secs(240) {
                ws = None;
            }
            // Idle: pick up the pre-warmed socket if we don't have one.
            if ws.is_none() {
                while let Ok((w, created)) = warm_rx.try_recv() {
                    if created.elapsed() <= Duration::from_secs(240) {
                        ws = Some(w);
                        ws_fresh = Instant::now();
                        last_ping = Instant::now();
                    }
                }
                // Proactively reconnect in background if missing
                if ws.is_none()
                    && !cfg.api_key.trim().is_empty()
                    && last_prewarm.elapsed() >= Duration::from_secs(5)
                {
                    last_prewarm = Instant::now();
                    spawn_prewarm(
                        cfg.api_key.clone(),
                        cfg.language_codes.clone(),
                        cfg.mode.clone(),
                        cfg.custom_vocabulary.clone(),
                        warm_tx.clone(),
                    );
                }
            }
            // Idle: animate the meter at ~10 Hz so mic choice is verifiable,
            // ring stays bounded (overwrite-oldest) even if never drained.
            if last_idle_push.elapsed() >= Duration::from_millis(100) {
                let level = match cap.take_chunk() {
                    Some((_, rms)) => {
                        note_take(rms, &mut silent_takes, &mut last_take_ok, &mut reopen_wait);
                        rms
                    }
                    None => 0.0,
                };
                // Only push when there's something to show or every 2 s (clock).
                // While the API key is missing the onboarding pill stays up —
                // idle meter pushes would clobber it.
                if demo_mode.is_none() && level > 200.0 && !cfg.api_key.trim().is_empty() {
                    push_pill(
                        &pill_tx,
                        ui::Mode::Idle,
                        level,
                        &format!("Utterly — hold {} to dictate", cfg.hotkey),
                    );
                    last_idle_push = Instant::now();
                } else if last_idle_push.elapsed() >= Duration::from_secs(2) {
                    last_idle_push = Instant::now();
                }
            }
            std::thread::sleep(tick_interval(recording));
        }
    }
}

fn interim_short(s: &str) -> String {
    let t: String = s.chars().take(60).collect();
    if t.is_empty() {
        String::new()
    } else {
        format!("“{t}”")
    }
}

/// Session-loop tick: 10ms while recording, 50ms idle.
/// Idle meter is already gated to 100ms (last_idle_push), so the slower
/// idle tick saves ~80 wakeups/sec with zero behavior change.
fn tick_interval(recording: bool) -> Duration {
    if recording {
        Duration::from_millis(10)
    } else {
        Duration::from_millis(50)
    }
}

/// Grace-drain stop predicate: early-exit on server close or 500ms quiet
/// after the first final, with a 4000ms hard cap. Pure for testability;
/// `quiet_ms` is time since last interim/final, `elapsed_ms` since grace start.
fn should_stop_grace(closed: bool, got_final: bool, quiet_ms: u64, elapsed_ms: u64) -> bool {
    if closed {
        return true;
    }
    if elapsed_ms >= 4000 {
        return true;
    }
    if got_final && quiet_ms >= 500 {
        return true;
    }
    false
}

/// Clean speech artifact tags, noise annotations, and markdown brackets from transcript text.
/// Preserves mathematical comparisons (x < y) and programming array indexing (arr[0]).
pub fn clean_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            let valid_start = if i + 1 < chars.len() {
                let next = chars[i + 1];
                next.is_ascii_alphabetic()
                    || (next == '/' && i + 2 < chars.len() && chars[i + 2].is_ascii_alphabetic())
            } else {
                false
            };
            if valid_start {
                let mut j = i + 1;
                let mut found = false;
                while j < chars.len() && j <= i + 32 {
                    if chars[j] == '\n' {
                        break;
                    }
                    if chars[j] == '>' {
                        found = true;
                        break;
                    }
                    j += 1;
                }
                if found {
                    let inner: String = chars[i + 1..j].iter().collect();
                    let inner_trim = inner.trim();
                    let tag_content = inner_trim.trim_end_matches('/').trim();
                    let tag_name = tag_content
                        .trim_start_matches('/')
                        .split_whitespace()
                        .next()
                        .unwrap_or("");
                    let is_tag = !tag_name.is_empty()
                        && tag_name
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
                    if is_tag {
                        i = j + 1;
                        continue;
                    }
                }
            }
            out.push(c);
            i += 1;
        } else if c == '[' {
            let mut j = i + 1;
            let mut found = false;
            while j < chars.len() && j <= i + 32 {
                if chars[j] == '\n' {
                    break;
                }
                if chars[j] == ']' {
                    found = true;
                    break;
                }
                j += 1;
            }
            if found {
                let inner: String = chars[i + 1..j].iter().collect();
                let inner_trim = inner.trim();
                let tag_content = inner_trim.trim_end_matches('/').trim();
                let is_speech_tag = !tag_content.is_empty()
                    && tag_content
                        .chars()
                        .all(|ch| ch.is_ascii_alphabetic() || ch == ' ' || ch == '-')
                    && (tag_content.eq_ignore_ascii_case("laughter")
                        || tag_content.eq_ignore_ascii_case("applause")
                        || tag_content.eq_ignore_ascii_case("music")
                        || tag_content.eq_ignore_ascii_case("noise")
                        || tag_content.eq_ignore_ascii_case("silence")
                        || tag_content.eq_ignore_ascii_case("whisper")
                        || tag_content.eq_ignore_ascii_case("cough")
                        || tag_content.eq_ignore_ascii_case("sigh")
                        || tag_content.eq_ignore_ascii_case("gasp")
                        || tag_content.eq_ignore_ascii_case("groan")
                        || tag_content.eq_ignore_ascii_case("cheering")
                        || tag_content.eq_ignore_ascii_case("chuckle")
                        || tag_content.eq_ignore_ascii_case("crying")
                        || tag_content.eq_ignore_ascii_case("snort")
                        || tag_content.eq_ignore_ascii_case("yawn")
                        || tag_content.eq_ignore_ascii_case("throat-clearing")
                        || tag_content.eq_ignore_ascii_case("throat clearing")
                        || tag_content.eq_ignore_ascii_case("inaudible")
                        || tag_content.eq_ignore_ascii_case("unintelligible")
                        || tag_content.eq_ignore_ascii_case("speech"));
                if is_speech_tag {
                    i = j + 1;
                    continue;
                }
            }
            out.push(c);
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    // Preserve intentional newlines while normalizing intra-line whitespace.
    let lines: Vec<String> = out
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    lines.join("\n")
}

/// Combine finalized transcript segments with the current streaming interim segment.
/// Resolves overlaps and fixes mismatches during live streaming transcription.
pub fn combine_transcript(finals: &[String], interim: &str) -> String {
    let clean_interim = clean_tags(interim);
    let interim = clean_interim.trim();
    if finals.is_empty() {
        return interim.to_string();
    }
    let finals_joined = finals.join(" ");
    if interim.is_empty() {
        return finals_joined;
    }
    if interim.eq_ignore_ascii_case(&finals_joined) {
        return interim.to_string();
    }
    if interim
        .to_lowercase()
        .starts_with(&finals_joined.to_lowercase())
    {
        return interim.to_string();
    }
    if finals_joined
        .to_lowercase()
        .starts_with(&interim.to_lowercase())
    {
        return finals_joined;
    }

    let pw: Vec<&str> = finals_joined.split_whitespace().collect();
    let iw: Vec<&str> = interim.split_whitespace().collect();

    let max_k = pw.len().min(iw.len()).min(8);
    for k in (1..=max_k).rev() {
        let pw_tail = &pw[pw.len() - k..];
        let iw_head = &iw[..k];
        let matches = pw_tail.iter().zip(iw_head.iter()).all(|(a, b)| {
            a.eq_ignore_ascii_case(b)
                || a.trim_matches(|c: char| !c.is_alphanumeric())
                    .eq_ignore_ascii_case(b.trim_matches(|c: char| !c.is_alphanumeric()))
        });
        if matches {
            let mut prefix = pw[..pw.len() - k].join(" ");
            if !prefix.is_empty() {
                prefix.push(' ');
            }
            prefix.push_str(interim);
            return prefix;
        }
    }

    // Shared anchor matching
    let pw_search_start = pw.len().saturating_sub(8);
    for p_idx in pw_search_start..pw.len() {
        for i_idx in 0..iw.len().min(4) {
            let mut match_len = 0;
            while p_idx + match_len < pw.len()
                && i_idx + match_len < iw.len()
                && pw[p_idx + match_len]
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .eq_ignore_ascii_case(
                        iw[i_idx + match_len].trim_matches(|c: char| !c.is_alphanumeric()),
                    )
            {
                match_len += 1;
            }
            if match_len >= 2 {
                if p_idx == 0 || (p_idx <= 2 && i_idx > 0) {
                    return interim.to_string();
                }
                let mut prefix = pw[..p_idx].join(" ");
                if !prefix.is_empty() {
                    prefix.push(' ');
                }
                prefix.push_str(interim);
                return prefix;
            }
        }
    }

    format!("{finals_joined} {interim}")
}

/// Streaming overlap repair & mismatch fixing: when a new final or generation arrives,
/// replace instead of appending if it extends, fixes, or corrects previous text.
pub fn push_final(finals: &mut Vec<String>, text: String) {
    let cleaned = clean_tags(&text);
    let t = cleaned.trim();
    if t.is_empty() {
        return;
    }
    if finals.is_empty() {
        finals.push(t.to_string());
        return;
    }

    let full_prev = finals.join(" ");
    let prev_trim = full_prev.trim();
    if prev_trim == t || prev_trim.eq_ignore_ascii_case(t) {
        return; // exact duplicate
    }
    if t.to_lowercase().starts_with(&prev_trim.to_lowercase()) {
        finals.clear();
        finals.push(t.to_string());
        return;
    }
    if prev_trim.to_lowercase().starts_with(&t.to_lowercase()) {
        return; // already covered
    }

    let pw: Vec<&str> = prev_trim.split_whitespace().collect();
    let tw: Vec<&str> = t.split_whitespace().collect();

    // 1. Direct word-level tail-overlap splice: pw ends with first k words of tw (k >= 2)
    let max_k = pw.len().min(tw.len()).min(8);
    for k in (2..=max_k).rev() {
        let pw_tail = &pw[pw.len() - k..];
        let tw_head = &tw[..k];
        let matches = pw_tail.iter().zip(tw_head.iter()).all(|(a, b)| {
            a.eq_ignore_ascii_case(b)
                || a.trim_matches(|c: char| !c.is_alphanumeric())
                    .eq_ignore_ascii_case(b.trim_matches(|c: char| !c.is_alphanumeric()))
        });
        if matches {
            let mut merged = pw[..pw.len() - k].join(" ");
            if !merged.is_empty() {
                merged.push(' ');
            }
            merged.push_str(t);
            finals.clear();
            finals.push(merged);
            return;
        }
    }

    // 2. Mismatch fixing / re-generation: look for shared word anchor of length >= 2
    let pw_search_start = pw.len().saturating_sub(8);
    let mut best_match: Option<(usize, usize, usize)> = None; // (pw_idx, tw_idx, len)
    for p_idx in pw_search_start..pw.len() {
        for t_idx in 0..tw.len().min(4) {
            let mut match_len = 0;
            while p_idx + match_len < pw.len()
                && t_idx + match_len < tw.len()
                && pw[p_idx + match_len]
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .eq_ignore_ascii_case(
                        tw[t_idx + match_len].trim_matches(|c: char| !c.is_alphanumeric()),
                    )
            {
                match_len += 1;
            }
            if match_len >= 2 {
                if let Some((_, _, best_len)) = best_match {
                    if match_len > best_len {
                        best_match = Some((p_idx, t_idx, match_len));
                    }
                } else {
                    best_match = Some((p_idx, t_idx, match_len));
                }
            }
        }
    }

    if let Some((p_idx, t_idx, _len)) = best_match {
        if p_idx == 0 || (p_idx <= 2 && t_idx > 0) {
            finals.clear();
            finals.push(t.to_string());
            return;
        }
        let mut prefix = pw[..p_idx].join(" ");
        if !prefix.is_empty() {
            prefix.push(' ');
        }
        prefix.push_str(t);
        finals.clear();
        finals.push(prefix);
        return;
    }

    finals.push(t.to_string());
}

/// Claim the single-instance lock (atomic create + stale-pid steal).
/// The file is intentionally never deleted: a dead pid means stale.
fn claim_instance() -> bool {
    let dir = config::config_path();
    let Some(dir) = dir.parent() else {
        return true; // nowhere to lock; don't block startup
    };
    let _ = std::fs::create_dir_all(dir);
    let lock = dir.join("instance.lock");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = writeln!(f, "{}", std::process::id());
            std::mem::forget(f); // existence == claim, held for process life
            true
        }
        Err(_) => {
            // Possibly stale (crash / SIGKILL). Steal iff the pid is gone
            // (and, on Linux, iff it isn't another live utterly).
            if let Ok(text) = std::fs::read_to_string(&lock) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    if !pid_alive(pid) {
                        let _ = std::fs::remove_file(&lock);
                        return claim_instance();
                    }
                }
            }
            false
        }
    }
}

fn release_instance() {
    let dir = config::config_path();
    if let Some(parent) = dir.parent() {
        let _ = std::fs::remove_file(parent.join("instance.lock"));
    }
}

#[cfg(target_os = "windows")]
fn win_pid_alive(pid: u32) -> bool {
    use std::ffi::c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0x0000_0000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(object: *mut c_void) -> i32;
        fn K32GetProcessImageFileNameW(
            process: *mut c_void,
            image_file_name: *mut u16,
            size: u32,
        ) -> u32;
    }

    unsafe {
        let handle = OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            let q_handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if q_handle.is_null() {
                return false;
            }
            CloseHandle(q_handle);
            return false;
        }

        let wait_res = WaitForSingleObject(handle, 0);
        if wait_res == WAIT_OBJECT_0 {
            CloseHandle(handle);
            return false;
        }

        let mut buf = [0u16; 512];
        let len = K32GetProcessImageFileNameW(handle, buf.as_mut_ptr(), buf.len() as u32);
        CloseHandle(handle);

        if len > 0 {
            let name = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
            name.contains("utterly")
        } else {
            true
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid == std::process::id() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/cmdline")) {
            Ok(cmd) => cmd.contains("utterly"),
            Err(_) => false, // no /proc entry => dead
        }
    }
    #[cfg(target_os = "windows")]
    {
        win_pid_alive(pid)
    }
    #[cfg(all(not(target_os = "linux"), not(target_os = "windows")))]
    {
        let _ = pid;
        lock_age_secs() < 90
    }
}

/// Seconds since instance.lock was last (re)touched. u64::MAX if unknown.
/// Only needed off-Linux (Linux uses /proc instead).
#[cfg(all(not(target_os = "linux"), not(target_os = "windows")))]
fn lock_age_secs() -> u64 {
    let dir = config::config_path();
    let path = dir.parent().map(|d| d.join("instance.lock"));
    let age = path
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs());
    age.unwrap_or(u64::MAX)
}

/// Heartbeat: refresh instance.lock so a later copy knows we're alive.
/// Cheap (one tiny write); called every ~30 s from the session loop.
fn touch_instance() {
    let dir = config::config_path();
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
        let _ = std::fs::write(
            parent.join("instance.lock"),
            format!("{}\n", std::process::id()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_interval_recording_vs_idle() {
        assert_eq!(tick_interval(true), Duration::from_millis(10));
        assert_eq!(tick_interval(false), Duration::from_millis(50));
    }

    #[test]
    fn should_stop_grace_stops_on_closed_quiet_and_cap() {
        // Closed server side always stops immediately.
        assert!(should_stop_grace(true, false, 0, 0));
        assert!(should_stop_grace(true, true, 0, 0));
        // Quiet 500ms after a final stops early.
        assert!(should_stop_grace(false, true, 500, 1000));
        assert!(should_stop_grace(false, true, 600, 1000));
        // Otherwise continues.
        assert!(!should_stop_grace(false, false, 0, 0));
        assert!(!should_stop_grace(false, true, 0, 1000));
        assert!(!should_stop_grace(false, true, 499, 1000));
        assert!(!should_stop_grace(false, false, 1000, 1000));
        // Hard cap at 4000ms never exceeded.
        assert!(should_stop_grace(false, false, 0, 4000));
        assert!(should_stop_grace(false, true, 0, 4000));
        assert!(should_stop_grace(false, false, 0, 4001));
    }

    #[test]
    fn push_final_repairs_overlaps() {
        let mut v = Vec::new();
        push_final(&mut v, "hello".into());
        assert_eq!(v, vec!["hello"]);
        push_final(&mut v, "hello".into());
        assert_eq!(v, vec!["hello"], "exact dup skips");
        push_final(&mut v, "hello world".into());
        assert_eq!(v, vec!["hello world"], "extension replaces");
        push_final(&mut v, "world".into());
        assert_eq!(v, vec!["hello world", "world"], "disjoint pushes");
        let mut w = vec!["the quick brown fox".to_string()];
        push_final(&mut w, "brown fox jumps".into());
        assert_eq!(w, vec!["the quick brown fox jumps"], "tail overlap splices");
        push_final(&mut w, "  ".into());
        assert_eq!(w.len(), 1, "blank ignored");
        let mut s = vec!["or".to_string()];
        push_final(&mut s, "world".into());
        assert_eq!(s, vec!["or", "world"], "substring must not replace");
        // Tag stripping test
        let mut t = Vec::new();
        push_final(&mut t, "hello <speech> world [laughter]".into());
        assert_eq!(t, vec!["hello world"]);

        // Multi-segment re-generation replacing whole transcript
        let mut multi = vec!["The quick".to_string(), "brown fox".to_string()];
        push_final(
            &mut multi,
            "The quick brown fox jumps over the lazy dog".into(),
        );
        assert_eq!(
            multi,
            vec!["The quick brown fox jumps over the lazy dog"],
            "full sentence generation replaces multi-segment finals"
        );

        // Mismatch repair via anchor matching
        let mut mismatch = vec!["I am going to".to_string()];
        push_final(&mut mismatch, "I'm going to the store".into());
        assert_eq!(
            mismatch,
            vec!["I'm going to the store"],
            "mismatch anchor fixes previous text without duplication"
        );

        // Short sentence continuation must not be wiped out
        let mut short_sent = vec!["I love this cat".to_string()];
        push_final(&mut short_sent, "this cat is black".into());
        assert_eq!(
            short_sent,
            vec!["I love this cat is black"],
            "short sentence tail overlap must preserve prefix"
        );
    }

    #[test]
    fn clean_tags_removes_brackets_and_xml() {
        assert_eq!(clean_tags("hello <noise> world"), "hello world");
        assert_eq!(clean_tags("hey [laughter] there"), "hey there");
        assert_eq!(clean_tags("hello <laughter/> world"), "hello world");
        assert_eq!(clean_tags("hello <laughter /> world"), "hello world");
        assert_eq!(clean_tags("sound [laughter/] effect"), "sound effect");
        assert_eq!(
            clean_tags("sigh [sigh] and chuckle [chuckle]"),
            "sigh and chuckle"
        );
        assert_eq!(clean_tags("clean text"), "clean text");
        // Must preserve mathematical comparisons and array indexing
        assert_eq!(clean_tags("if x < 10 then y > 20"), "if x < 10 then y > 20");
        assert_eq!(clean_tags("let val = arr[0];"), "let val = arr[0];");
        assert_eq!(clean_tags("if x < 5"), "if x < 5");
        // Must not swallow text on unclosed delimiter
        assert_eq!(clean_tags("test <unclosed"), "test <unclosed");
        assert_eq!(clean_tags("test [unclosed"), "test [unclosed");
        // Must preserve intentional line breaks and paragraphs
        assert_eq!(
            clean_tags("First paragraph.\n\nSecond paragraph."),
            "First paragraph.\n\nSecond paragraph."
        );
    }

    #[test]
    fn combine_transcript_handles_interim_streaming() {
        assert_eq!(combine_transcript(&[], "hello"), "hello");
        assert_eq!(combine_transcript(&["hello".into()], ""), "hello");
        assert_eq!(
            combine_transcript(&["hello".into()], "world"),
            "hello world"
        );
        assert_eq!(
            combine_transcript(&["hello".into()], "hello world"),
            "hello world"
        );
        assert_eq!(
            combine_transcript(&["the quick".into()], "quick brown fox"),
            "the quick brown fox"
        );
        assert_eq!(
            combine_transcript(&["Please send the email".into()], "the email today"),
            "Please send the email today"
        );
        // Multi-segment finals with interim extension
        assert_eq!(
            combine_transcript(
                &["the quick".into(), "brown fox".into()],
                "the quick brown fox jumps"
            ),
            "the quick brown fox jumps"
        );
    }
}
