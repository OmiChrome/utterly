//! Utterly — minimal push-to-talk dictation pill.
//!
//! Hold Ctrl+Space: listen (manual VAD `activityStart`, stream 16 kHz PCM).
//! Release: `audioStreamEnd`, collect SMART-cleaned finals, paste into the
//! focused text area (clipboard + Ctrl/Cmd+V).
//!
//! Constraints honoured:
//! - No async runtime (std threads only), fixed 320 KiB audio ring, 95 KiB
//!   framebuffer, 8 KiB ws frames => single-digit MB RSS, idle CPU ~0%.
//! - Release profile opt-z + LTO + strip targets a <3 MB installer.

mod audio;
mod config;
mod hotkey;
mod output;
mod transcribe;
mod tray;
mod ui;

use std::sync::mpsc;
use std::time::{Duration, Instant};

fn print_help() {
    println!(
        "Utterly — Gemini 3.5 Transcribe Live dictation pill\n\
         \n\
         Usage:\n  \
           utterly [--list-mics] [--set-mic NAME] [--set-hotkey HOTKEY]\n  \
                    [--set-key API_KEY] [--set-mode smart|verbatim] [--help]\n\
         \n\
         Run with no flags: pill window + tray icon. Hold Ctrl+Space to dictate,\n  \
         release to transcribe into the focused text area.\n\
         Mic, hotkey, transcription mode (smart/verbatim) and API key can also\n  \
         be changed live from the tray-icon menu.\n\
         \n\
         Setup:\n  \
           1. Get a key at https://aistudio.google.com/apikey\n  \
           2. utterly --set-key YOUR_KEY\n  \
           3. utterly --list-mics / --set-mic \"MacBook Pro Microphone\"\n\
         \n\
         Config: ~/.config/utterly/config.json (0600, API key not world-readable)\n  \
         (Windows: %APPDATA%\\Utterly\\config.json)"
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
        let v = args.get(i + 1).cloned().unwrap_or_default();
        let mut c = config::load();
        c.api_key = v.trim().to_string();
        config::save(&c).expect("save config");
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
    // Menu-sync channel: session -> main thread (radio checkmarks; muda items
    // are !Send/!Sync so only the main thread touches them).
    let (sync_tx, sync_rx) = mpsc::channel::<(String, String, String)>();

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

    // ---- session thread (audio + hotkey + websocket) ----
    std::thread::Builder::new()
        .name("utterly-session".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || session_loop(cfg, pill_tx, menu_rx, sync_tx, ui_cmd_rx))
        .expect("spawn session");

    // ---- main thread: pill window (winit must own the main thread) ----
    let initial = "Utterly — hold Ctrl+Space to dictate".to_string();
    if let Err(e) = ui::run_pill(pill_rx, initial, tray, menu, sync_rx, gtk_ready, ui_cmd_tx) {
        eprintln!("[utterly] pill: {e}");
    }
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

fn session_loop(
    mut cfg: config::Config,
    pill_tx: mpsc::Sender<ui::PillUpdate>,
    menu_rx: mpsc::Receiver<tray::MenuCmd>,
    sync_tx: mpsc::Sender<(String, String, String)>,
    ui_cmd_rx: mpsc::Receiver<ui::UiCmd>,
) {
    if cfg.api_key.trim().is_empty() {
        push_pill(
            &pill_tx, ui::Mode::Idle, 0.0,
            "Utterly — paste your AI Studio key: utterly --set-key KEY (https://aistudio.google.com/apikey)",
        );
        eprintln!("[utterly] no API key. Run: utterly --set-key KEY");
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

    let mut hk = match hotkey::Hotkey::register(&cfg.hotkey) {
        Ok(h) => h,
        Err(e) => {
            push_pill(
                &pill_tx,
                ui::Mode::Idle,
                0.0,
                &format!("Utterly — hotkey error: {e}"),
            );
            eprintln!("[utterly] hotkey: {e}");
            return;
        }
    };

    println!("[utterly] ready. Hold Ctrl+Space to dictate (SMART mode).");
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
            "Utterly — hold Ctrl+Space to dictate",
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
    let mut finals: Vec<String> = Vec::new();
    let mut interim = String::new();
    let mut last_idle_push = Instant::now();
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
            if last_demo.elapsed() >= Duration::from_secs(2) {
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
                        "Utterly … transcribing (demo preview)",
                        cfg.mode == "verbatim",
                    ),
                }
            }
        }
        // --- capture watchdog: reopen a stale-silent stream (see above) ---
        let stream_old_enough = cap_age.elapsed() >= Duration::from_secs(5);
        let starved = last_take_ok.elapsed() >= Duration::from_secs(3) && stream_old_enough;
        if last_reopen.elapsed() >= reopen_wait && (silent_takes >= 30 || starved) {
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
                        "Utterly — mic stream recovered, hold Ctrl+Space to dictate",
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
                tray::MenuCmd::Hotkey(preset) => match hk.set(&preset) {
                    Ok(()) => {
                        cfg.hotkey = preset.clone();
                        let _ = config::save(&cfg);
                        let _ =
                            sync_tx.send((cfg.mic.clone(), cfg.hotkey.clone(), cfg.mode.clone()));
                        push_pill(
                            &pill_tx,
                            ui::Mode::Idle,
                            0.0,
                            &format!("Utterly — hold {preset} to dictate ({})", cfg.mode),
                        );
                        println!("[utterly] hotkey: {preset}");
                    }
                    Err(e) => {
                        push_pill(
                            &pill_tx,
                            ui::Mode::Idle,
                            0.0,
                            &format!("Utterly — hotkey error: {e}"),
                        );
                    }
                },
                tray::MenuCmd::Mode(mode) => {
                    cfg.mode = transcribe::normalize_mode(&mode).to_string();
                    let _ = config::save(&cfg);
                    let _ = sync_tx.send((cfg.mic.clone(), cfg.hotkey.clone(), cfg.mode.clone()));
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
                tray::MenuCmd::PasteKey => match output::read_key_from_clipboard() {
                    Some(k) => {
                        cfg.api_key = k;
                        let _ = config::save(&cfg);
                        push_pill(
                            &pill_tx,
                            ui::Mode::Idle,
                            0.0,
                            "Utterly — API key saved, hold Ctrl+Space to dictate",
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
                    std::process::exit(0);
                }
            }
        }
        // --- hotkey events (press/release) + pill mic-click toggles ---
        // MicToggle feeds the SAME match arms below (single code path, no
        // behavior drift): idle -> Pressed, recording -> Released. The mapping
        // is resolved per event so queued toggles track `recording` exactly.
        // Hide is already applied in the UI thread; the session ignores it.
        evs.clear();
        while let Some(ev) = hk.try_event() {
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
                    match transcribe::connect_live(&cfg.api_key, &cfg.language_codes, &cfg.mode) {
                        Ok(mut w) => {
                            // Manual turn bracketing, nested inside realtimeInput
                            // (a top-level activityStart gets the session closed
                            // with "Unknown name activityStart").
                            let _ = transcribe::send_activity_start(&mut w);
                            ws = Some(w);
                            recording = true;
                            finals.clear();
                            interim.clear();
                            // Verify the setup while the failure is still loud:
                            // surface rejections/closes instead of streaming
                            // into a dead session.
                            if let Some(w2) = ws.as_mut() {
                                let raw = transcribe::recv_raw(w2, 400);
                                match classify_setup_response(raw.as_deref()) {
                                    SetupVerify::Ok => {}
                                    SetupVerify::Unexpected => {
                                        let short: String = raw
                                            .as_deref()
                                            .unwrap_or_default()
                                            .chars()
                                            .take(300)
                                            .collect();
                                        eprintln!("[utterly] unexpected setup response: {short}");
                                    }
                                    SetupVerify::Timeout => {
                                        eprintln!("[utterly] no setup response (still proceeding)");
                                    }
                                }
                            }
                            push_pill_verbatim(
                                &pill_tx,
                                ui::Mode::Listening,
                                0.0,
                                "Utterly ● Listening… (release Ctrl+Space to transcribe)",
                                cfg.mode == "verbatim",
                            );
                        }
                        Err(e) => {
                            push_pill(
                                &pill_tx,
                                ui::Mode::Idle,
                                0.0,
                                &format!("Utterly — connect failed: {e}"),
                            );
                            eprintln!("[utterly] connect: {e}");
                        }
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
                        let grace_start = Instant::now();
                        let mut last_activity = grace_start;
                        while grace_start.elapsed() < Duration::from_millis(4000) {
                            if let Some(ev) = transcribe::recv_timeout(w, 100) {
                                let closed = ev.closed;
                                if let Some(t) = ev.interim {
                                    interim = t;
                                    last_activity = Instant::now();
                                }
                                if let Some(t) = ev.finalized {
                                    finals.push(t);
                                    last_activity = Instant::now();
                                }
                                let elapsed_ms = grace_start.elapsed().as_millis() as u64;
                                let quiet_ms = last_activity.elapsed().as_millis() as u64;
                                if should_stop_grace(closed, !finals.is_empty(), quiet_ms, elapsed_ms)
                                {
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
                    ws = None;
                    let text = if finals.is_empty() {
                        interim.clone()
                    } else {
                        finals.join(" ")
                    };
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        push_pill(
                            &pill_tx,
                            ui::Mode::Idle,
                            0.0,
                            "Utterly — heard nothing, try again",
                        );
                    } else {
                        output::commit(&text);
                        let shown: String = text.chars().take(80).collect();
                        push_pill(&pill_tx, ui::Mode::Idle, 0.0, &format!("Utterly — {shown}"));
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
                                finals.push(t);
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
                    let preview = if finals.is_empty() {
                        interim.clone()
                    } else {
                        finals.join(" ")
                    };
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
            std::thread::sleep(Duration::from_millis(10));
        } else {
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
                if level > 200.0 && !cfg.api_key.trim().is_empty() {
                    push_pill(
                        &pill_tx,
                        ui::Mode::Idle,
                        level,
                        "Utterly — hold Ctrl+Space to dictate",
                    );
                    last_idle_push = Instant::now();
                } else if last_idle_push.elapsed() >= Duration::from_secs(2) {
                    last_idle_push = Instant::now();
                }
            }
            std::thread::sleep(Duration::from_millis(10));
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

/// Setup-handshake verdict: pure for testability.
#[derive(Debug, PartialEq, Eq)]
enum SetupVerify {
    Ok,
    Unexpected,
    Timeout,
}

/// Classify the raw setup response: `setupComplete` => Ok, any other
/// payload (CLOSE / read-error string) => Unexpected, no frame => Timeout.
fn classify_setup_response(raw: Option<&str>) -> SetupVerify {
    match raw {
        Some(s) if s.contains("setupComplete") => SetupVerify::Ok,
        Some(_) => SetupVerify::Unexpected,
        None => SetupVerify::Timeout,
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
    #[cfg(not(target_os = "linux"))]
    {
        // No portable kill(pid,0) in std-only code: treat the lock as live
        // iff its heartbeat is fresh (the holder retouches it every 30 s).
        // A crashed holder stops touching => stealable after 90 s.
        let _ = pid;
        lock_age_secs() < 90
    }
}

/// Seconds since instance.lock was last (re)touched. u64::MAX if unknown.
/// Only needed off-Linux (Linux uses /proc instead).
#[cfg(not(target_os = "linux"))]
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
    fn setup_verify_classifies_response() {
        assert!(matches!(
            classify_setup_response(Some(r#"{"setupComplete":{}}"#)),
            SetupVerify::Ok
        ));
        assert!(matches!(
            classify_setup_response(Some("<CLOSE code=Close(1008) reason=bad>")),
            SetupVerify::Unexpected
        ));
        assert!(matches!(
            classify_setup_response(Some("<read error: timed out>")),
            SetupVerify::Unexpected
        ));
        assert!(matches!(classify_setup_response(None), SetupVerify::Timeout));
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
}
