//! Compact dictation pill: winit + softbuffer, no GPU or font engine.
//! Native OS dragging is kept outside the render path; pointer movement only
//! redraws when a visible control changes state.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use softbuffer::{Context, Surface};
use std::sync::Arc;
use winit::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::EventLoop,
    window::{Window, WindowLevel},
};

pub const PILL_W: u32 = 380;
pub const PILL_H: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Listening,
    Transcribing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCmd {
    MicToggle,
    Hide,
}

/// Mic-dot hit region (logical px): circle at (28,32) r=16.
pub fn hit_mic(x: f32, y: f32) -> bool {
    let dx = x - 28.0;
    let dy = y - 32.0;
    dx * dx + dy * dy <= 16.0 * 16.0
}

/// Close-box hit region (logical px): tiny X in top right corner.
/// Matches the circular button disc drawn at (w - 22.0, 18.0) r=8.5.
pub fn hit_close(x: f32, y: f32, w: f32) -> bool {
    let dx = x - (w - 22.0);
    let dy = y - 18.0;
    dx * dx + dy * dy <= 12.0 * 12.0
}

/// Extract clean user-facing status or transcript from the window title string.
pub fn display_text(title: &str) -> &str {
    if let Some(rest) = title.strip_prefix("Utterly ● ") {
        rest
    } else if title == "Utterly ●" {
        ""
    } else if let Some(rest) = title.strip_prefix("Utterly … transcribing ") {
        rest
    } else if let Some(rest) = title.strip_prefix("Utterly … transcribing") {
        rest
    } else if let Some(rest) = title.strip_prefix("Utterly — ") {
        rest
    } else {
        title
    }
}

/// Truncate long live streaming text from the left at word boundaries so that
/// the latest spoken words are visible during live speech transcription.
pub fn format_live_text(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let chars: Vec<char> = trimmed.chars().collect();
        let start_idx = chars.len().saturating_sub(max_chars);
        let mut split = start_idx;
        while split < chars.len() && chars[split] != ' ' {
            split += 1;
        }
        if split < chars.len() - 5 {
            format!("… {}", chars[split + 1..].iter().collect::<String>())
        } else {
            format!("… {}", chars[start_idx..].iter().collect::<String>())
        }
    }
}

/// True when an Idle title is a normal state that should auto-hide to tray
/// after a grace period. Persistent onboarding ("no API key") and hardware
/// setup errors ("mic error: not found" / "no input device") stay visible until resolved.
/// Temporary notices ("heard nothing", "saved", "recovered", "paste failed") auto-hide.
pub fn should_auto_hide(title: &str) -> bool {
    let t = title.to_ascii_lowercase();
    if t.contains("no api key")
        || (t.contains("api key") && (t.contains("copy") || t.contains("missing") || t.contains("enter") || t.contains("set")))
        || t.contains("no input device")
        || (t.contains("mic") && (t.contains("not found") || t.contains("no input device")))
    {
        return false;
    }
    true
}

#[derive(Debug, Clone)]
pub struct PillUpdate {
    pub mode: Mode,
    pub level: f32, // RMS for meter
    pub title: String,
    /// True when the session runs live-verbatim (finals stream in over the
    /// websocket, so Transcribing shows a meter instead of the spinner).
    pub verbatim_live: bool,
}

/// Platform objects and channels owned by the pill thread.
pub struct UiServices {
    pub hotkey: Option<crate::hotkey::Hotkey>,
    pub hotkey_events: std::sync::mpsc::Sender<crate::hotkey::KeyEvent>,
    pub hotkey_requests: Receiver<String>,
    pub hotkey_results: std::sync::mpsc::Sender<(String, Result<(), String>)>,
    pub tray: Option<tray_icon::TrayIcon>,
    pub menu: crate::tray::TrayMenu,
    pub sync_rx: Receiver<(String, String, String)>,
    pub gtk_pump: bool,
    pub ui_cmd_tx: std::sync::mpsc::Sender<UiCmd>,
    pub settings: Option<crate::settings::SettingsWindow>,
}

pub fn dot_color(mode: Mode, t: f64) -> (u8, u8, u8) {
    match mode {
        Mode::Idle => (0x8E, 0x8E, 0x93), // macOS grey
        Mode::Listening => {
            // Pulsing red: 2 Hz sine, integer math friendly.
            let pulse = (t * 4.0 * std::f64::consts::PI).sin() * 0.5 + 0.5;
            let v = (160.0 + 95.0 * pulse) as u8;
            (v, 0x30, 0x30)
        }
        Mode::Transcribing => (0x34, 0xC7, 0x59), // macOS green
    }
}

/// Transcribing spinner angle in radians at wall-clock `t` seconds
/// (pure, tested): 2 rev/s so the motion reads at the 30 fps pill cap.
pub fn spinner_angle(t: f64) -> f64 {
    (t * 4.0 * std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI)
}

/// Run the pill window on the MAIN thread. `rx` receives PillUpdate from the
/// session thread; `tray`+`menu` are owned here because tray-icon/muda handles
/// are !Send on some platforms. `sync_rx` carries (mic, hotkey, mode)
/// triples for radio-checkmark updates. `ui_cmd_tx` carries MicToggle/Hide
/// clicks back to the session thread. Returns when the window is closed.
pub fn run_pill(
    rx: Receiver<PillUpdate>,
    initial_title: String,
    services: UiServices,
) -> Result<(), String> {
    let UiServices {
        hotkey: mut hotkey_manager,
        hotkey_events,
        hotkey_requests,
        hotkey_results,
        mut tray,
        menu,
        sync_rx,
        gtk_pump,
        ui_cmd_tx,
        settings: _settings,
    } = services;
    let event_loop = EventLoop::new().map_err(|e| e.to_string())?;
    let attrs = Window::default_attributes()
        .with_title(initial_title.clone())
        .with_inner_size(LogicalSize::new(PILL_W, PILL_H))
        .with_decorations(false)
        .with_transparent(false)
        .with_window_level(WindowLevel::AlwaysOnTop)
        .with_resizable(false)
        .with_visible(false);
    let window: Arc<Window> = Arc::new(
        #[allow(deprecated)] // pre-run creation is exactly our case (single pill window)
        event_loop.create_window(attrs).map_err(|e| e.to_string())?,
    );
    // No taskbar button, no focus steal: toolwindow + no-activate on Windows.
    apply_window_chrome(&window);
    // Restore last drag position when it lands on a connected monitor;
    // default center-bottom, clear of taskbar. Physical px; winit
    // LogicalSize already keeps the pill DPI-agnostic.
    let restored = crate::config::load();
    let mut placed = false;
    let win_size = window.outer_size();
    if let (Some(x), Some(y)) = (restored.pill_x, restored.pill_y) {
        for monitor in window.available_monitors() {
            let ms = monitor.size();
            let mp = monitor.position();
            let min_w = (win_size.width as i32).min(100);
            let min_h = (win_size.height as i32).min(30);
            if x + min_w > mp.x
                && x < mp.x + ms.width as i32 - min_w
                && y >= mp.y
                && y < mp.y + ms.height as i32 - min_h
            {
                let clamped_x = x.clamp(mp.x, mp.x + ms.width as i32 - win_size.width as i32);
                let clamped_y = y.clamp(mp.y, mp.y + ms.height as i32 - win_size.height as i32);
                window.set_outer_position(winit::dpi::PhysicalPosition::new(clamped_x, clamped_y));
                placed = true;
                break;
            }
        }
    }
    if !placed {
        let m = window
            .current_monitor()
            .or_else(|| window.primary_monitor())
            .or_else(|| window.available_monitors().next());
        if let Some(monitor) = m {
            let ms = monitor.size();
            let mp = monitor.position();
            let x = mp.x + (ms.width as i32 - win_size.width as i32) / 2;
            let y = mp.y + ms.height as i32 - win_size.height as i32 - 48;
            window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
        }
    }
    set_rounded_window(&window);

    let context = Context::new(window.clone()).map_err(|e| e.to_string())?;
    let mut surface = Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;

    // Initial paint before making the window visible avoids any white flash or unpainted glitch.
    let init_size = window.inner_size();
    let init_scale = window.scale_factor() as f32;
    let _ = draw(
        &mut surface,
        init_size.width,
        init_size.height,
        Mode::Idle,
        0.0,
        0.0,
        false,
        false,
        false,
        &initial_title,
        init_scale,
    );
    window.set_visible(true);
    // `tray` arrives built (with its options menu) from main(); the pill loop
    // only refreshes its icon/tooltip on state changes (see tray_dirty below).

    let mut mode = Mode::Idle;
    let mut level: f32 = 0.0;
    let mut title = initial_title;
    let mut dirty = true;
    // Track pointer position for hit tests, but redraw only when a control
    // changes hover state. Dragging across the body never repaints the pill.
    let mut cursor: Option<(f32, f32)> = None;
    let mut hover_inside = false;
    let mut close_hot = false;
    // Hidden-to-tray state. Hide NEVER exits the loop (no elwt.exit(), no
    // process::exit) — the tray icon keeps the app alive and re-shows the
    // pill (tray click, or the next Listening update from a hotkey press).
    let mut hidden = false;
    // Live-verbatim flag from the session thread (see PillUpdate).
    let mut verbatim_live = false;
    let start = Instant::now();
    let mut last_frame = Instant::now();
    // Smoothed meter (attack fast, release slow) — 1 float, no history buffer.
    let mut smooth: f32 = 0.0;

    // Non-blocking drain helper.
    let mut pending_title: Option<String> = None;
    let mut hotkey = String::from("Alt+Space");

    // In-memory drag position tracking: eliminates jitter, hitching, and DPAPI
    // encryption disk writes on the UI thread during active mouse movement.
    let mut saved_pos = (restored.pill_x, restored.pill_y);
    let mut pending_pos: Option<(i32, i32)> = None;
    let mut last_move_event = Instant::now();

    // Auto-hide countdown timer: grace period after speech so user can read transcript.
    // On launch, auto-hide after 2.5s if ready so the app sits quietly in the tray.
    let initial_auto_hide = should_auto_hide(&title);
    let mut auto_hide_at: Option<Instant> = if initial_auto_hide {
        Some(Instant::now() + Duration::from_millis(2500))
    } else {
        None
    };

    let (pos_tx, pos_rx) = std::sync::mpsc::channel::<(i32, i32)>();
    let _ = std::thread::Builder::new()
        .name("utterly-save-pos".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            while let Ok(pos) = pos_rx.recv() {
                // Coalesce rapid position updates so only the latest is saved
                let mut latest = pos;
                while let Ok(next) = pos_rx.try_recv() {
                    latest = next;
                }
                let mut cfg = crate::config::load();
                cfg.pill_x = Some(latest.0);
                cfg.pill_y = Some(latest.1);
                let _ = crate::config::save(&cfg);
            }
        });
    let save_pos_bg = move |px: i32, py: i32| {
        let _ = pos_tx.send((px, py));
    };

    // winit 0.30: run() takes ownership; use run_ondemand-friendly closure.
    #[allow(deprecated)]
    let r = event_loop.run(move |event, elwt| {
        // Drain pending updates every event + every ~33ms via AboutToWait.
        let mut tray_dirty = false;
        if let Some(manager) = hotkey_manager.as_ref() {
            while let Some(key_event) = manager.try_event() {
                let _ = hotkey_events.send(key_event);
            }
        }
        while let Ok(preset) = hotkey_requests.try_recv() {
            let result = if let Some(manager) = hotkey_manager.as_mut() {
                manager.set(&preset)
            } else {
                match crate::hotkey::Hotkey::register(&preset) {
                    Ok(manager) => {
                        hotkey_manager = Some(manager);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            };
            let _ = hotkey_results.send((preset, result));
        }
        while let Ok(u) = rx.try_recv() {
            if u.mode != mode {
                if u.mode == Mode::Listening {
                    if hidden {
                        if let Ok(pos) = window.outer_position() {
                            let win_size = window.outer_size();
                            let on_screen = window.available_monitors().any(|m| {
                                let ms = m.size();
                                let mp = m.position();
                                pos.x + (win_size.width as i32) / 2 >= mp.x
                                    && pos.x + (win_size.width as i32) / 2 < mp.x + ms.width as i32
                                    && pos.y >= mp.y
                                    && pos.y < mp.y + ms.height as i32
                            });
                            if !on_screen {
                                let m = window
                                    .current_monitor()
                                    .or_else(|| window.primary_monitor())
                                    .or_else(|| window.available_monitors().next());
                                if let Some(m) = m {
                                    let ms = m.size();
                                    let mp = m.position();
                                    let x = mp.x + (ms.width as i32 - win_size.width as i32) / 2;
                                    let y = mp.y + ms.height as i32 - win_size.height as i32 - 48;
                                    window.set_outer_position(winit::dpi::PhysicalPosition::new(
                                        x, y,
                                    ));
                                }
                            }
                        }
                        window.set_window_level(WindowLevel::AlwaysOnTop);
                        window.set_visible(true);
                        hidden = false;
                    }
                    auto_hide_at = None;
                } else if u.mode == Mode::Transcribing {
                    auto_hide_at = None;
                }
                let entering_idle =
                    u.mode == Mode::Idle && (mode == Mode::Listening || mode == Mode::Transcribing);
                let title_changed = u.title != title;
                mode = u.mode;
                dirty = true;
                tray_dirty = true;
                if u.mode == Mode::Idle && (entering_idle || title_changed) {
                    if should_auto_hide(&u.title) {
                        // ~3s grace period so user can read temporary notices before pill auto-hides
                        auto_hide_at = Some(Instant::now() + Duration::from_millis(3000));
                    } else {
                        auto_hide_at = None;
                    }
                }
            }
            verbatim_live = u.verbatim_live;
            level = u.level;
            if u.title != title {
                title = u.title.clone();
                pending_title = Some(title.clone());
                tray_dirty = true;
            }
            dirty = true;
        }
        // Radio checkmarks are applied here (main thread owns the muda items).
        while let Ok((mic, hk, mode)) = sync_rx.try_recv() {
            menu.sync(&mic, &hk, &mode);
            hotkey = hk;
        }
        if tray_dirty {
            let preview: String = title.chars().take(60).collect();
            crate::tray::set_mode(&mut tray, mode, &preview, &hotkey);
        }
        // Tray click re-shows a hidden pill (hide-to-tray counterpart).
        // Non-blocking; no-op when the tray is absent (headless).
        while let Ok(ev) = tray_icon::tray_event_receiver().try_recv() {
            match ev.event {
                tray_icon::ClickEvent::Left | tray_icon::ClickEvent::Double if hidden => {
                    if let Ok(pos) = window.outer_position() {
                        let win_size = window.outer_size();
                        let on_screen = window.available_monitors().any(|m| {
                            let ms = m.size();
                            let mp = m.position();
                            pos.x + (win_size.width as i32) / 2 >= mp.x
                                && pos.x + (win_size.width as i32) / 2 < mp.x + ms.width as i32
                                && pos.y >= mp.y
                                && pos.y < mp.y + ms.height as i32
                        });
                        if !on_screen {
                            let m = window
                                .current_monitor()
                                .or_else(|| window.primary_monitor())
                                .or_else(|| window.available_monitors().next());
                            if let Some(m) = m {
                                let ms = m.size();
                                let mp = m.position();
                                let x = mp.x + (ms.width as i32 - win_size.width as i32) / 2;
                                let y = mp.y + ms.height as i32 - win_size.height as i32 - 48;
                                window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
                            }
                        }
                    }
                    window.set_window_level(WindowLevel::AlwaysOnTop);
                    window.set_visible(true);
                    hidden = false;
                    dirty = true;
                    auto_hide_at = Some(Instant::now() + Duration::from_millis(3000));
                }
                _ => {}
            }
        }
        // Linux: pump the GTK main loop so tray-menu clicks dispatch while the
        // winit loop owns the thread. No-op when idle (events_pending=false).
        // Gated by gtk_pump so headless runs never touch GTK.
        #[cfg(target_os = "linux")]
        if gtk_pump {
            while gtk::events_pending() {
                gtk::main_iteration();
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = gtk_pump; // no GTK outside Linux; tray works without pumping

        match event {
            Event::NewEvents(_) => {
                // Active feedback is capped at 30 fps; idle only redraws on
                // input or state changes.
                if last_frame.elapsed() >= Duration::from_millis(33)
                    && (mode == Mode::Listening || mode == Mode::Transcribing)
                {
                    dirty = true;
                }
            }
            Event::AboutToWait => {
                if let Some(t) = pending_title.take() {
                    window.set_title(&t);
                }
                // Save pending drag position when stationary for 500ms
                if let Some((px, py)) = pending_pos {
                    if last_move_event.elapsed() >= Duration::from_millis(500) {
                        if saved_pos != (Some(px), Some(py)) {
                            saved_pos = (Some(px), Some(py));
                            save_pos_bg(px, py);
                        }
                        pending_pos = None;
                    }
                }
                // Auto-hide when countdown expires
                if let Some(hide_time) = auto_hide_at {
                    if Instant::now() >= hide_time && mode == Mode::Idle && !hidden {
                        window.set_visible(false);
                        hidden = true;
                        auto_hide_at = None;
                        let _ = ui_cmd_tx.send(UiCmd::Hide);
                    }
                }
                if dirty
                    && (last_frame.elapsed() >= Duration::from_millis(33) || mode == Mode::Idle)
                {
                    // attack/release smoothing without a history buffer
                    if level > smooth {
                        smooth = level;
                    } else {
                        smooth += (level - smooth) * 0.25;
                    }
                    let size = window.inner_size();
                    let scale = window.scale_factor() as f32;
                    if let Err(e) = draw(
                        &mut surface,
                        size.width,
                        size.height,
                        mode,
                        smooth,
                        start.elapsed().as_secs_f64(),
                        verbatim_live,
                        hover_inside,
                        close_hot,
                        &title,
                        scale,
                    ) {
                        eprintln!("[utterly] pill draw: {e}");
                    }
                    last_frame = Instant::now();
                    dirty = false;
                }
                // Native input wakes immediately. Background channels poll
                // at 10 Hz when idle and at the animation rate when active.
                let wait = if mode == Mode::Listening
                    || mode == Mode::Transcribing
                    || auto_hide_at.is_some()
                {
                    Duration::from_millis(33)
                } else {
                    Duration::from_millis(100)
                };
                elwt.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                    Instant::now() + wait,
                ));
            }
            Event::WindowEvent {
                event: WindowEvent::RedrawRequested,
                ..
            } => {
                dirty = true;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => {
                set_rounded_window(&window);
                dirty = true;
            }
            Event::WindowEvent {
                event: WindowEvent::ScaleFactorChanged { .. },
                ..
            } => {
                set_rounded_window(&window);
                dirty = true;
            }
            Event::WindowEvent {
                event: WindowEvent::CursorLeft { .. },
                ..
            } => {
                if cursor.take().is_some() || hover_inside || close_hot {
                    hover_inside = false;
                    close_hot = false;
                    dirty = true;
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // OS close (X button / Alt+F4) hides to tray — NEVER exits.
                if let Some((px, py)) = pending_pos.take() {
                    if saved_pos != (Some(px), Some(py)) {
                        saved_pos = (Some(px), Some(py));
                        save_pos_bg(px, py);
                    }
                }
                window.set_visible(false);
                hidden = true;
                auto_hide_at = None;
                let _ = ui_cmd_tx.send(UiCmd::Hide);
            }
            Event::WindowEvent {
                event: WindowEvent::CursorMoved { position, .. },
                ..
            } => {
                let logical: winit::dpi::LogicalPosition<f32> =
                    position.to_logical(window.scale_factor());
                cursor = Some((logical.x, logical.y));
                // DPI-agnostic hit test: use live logical size, not constants.
                let scale = window.scale_factor() as f32;
                let inner = window.inner_size();
                let lw = inner.width as f32 / scale;
                let lh = inner.height as f32 / scale;
                let inside =
                    logical.x >= 0.0 && logical.y >= 0.0 && logical.x < lw && logical.y < lh;
                let close = inside && hit_close(logical.x, logical.y, lw);
                if hover_inside != inside || close_hot != close {
                    hover_inside = inside;
                    close_hot = close;
                    dirty = true;
                }
            }
            Event::WindowEvent {
                event: WindowEvent::Moved(pos),
                ..
            } => {
                // In-memory position tracking during mouse move.
                // Eliminates jitter, hitching, and DPAPI encryption churn during drag.
                pending_pos = Some((pos.x, pos.y));
                last_move_event = Instant::now();
            }
            Event::WindowEvent {
                event:
                    WindowEvent::MouseInput {
                        state: winit::event::ElementState::Released,
                        button: winit::event::MouseButton::Left,
                        ..
                    },
                ..
            } => {
                if let Some((px, py)) = pending_pos.take() {
                    if saved_pos != (Some(px), Some(py)) {
                        saved_pos = (Some(px), Some(py));
                        save_pos_bg(px, py);
                    }
                }
            }
            Event::WindowEvent {
                event:
                    WindowEvent::MouseInput {
                        state: winit::event::ElementState::Pressed,
                        button: winit::event::MouseButton::Left,
                        ..
                    },
                ..
            } => {
                // DPI-agnostic width for hit tests.
                let scale = window.scale_factor() as f32;
                let lw = window.inner_size().width as f32 / scale;
                if let Some((x, y)) = cursor {
                    if hit_close(x, y, lw) {
                        if let Some((px, py)) = pending_pos.take() {
                            if saved_pos != (Some(px), Some(py)) {
                                saved_pos = (Some(px), Some(py));
                                save_pos_bg(px, py);
                            }
                        }
                        window.set_visible(false);
                        hidden = true;
                        auto_hide_at = None;
                        let _ = ui_cmd_tx.send(UiCmd::Hide);
                    } else if hit_mic(x, y) {
                        let _ = ui_cmd_tx.send(UiCmd::MicToggle);
                    } else if let Err(error) = window.drag_window() {
                        eprintln!("[utterly] pill drag: {error}");
                    } else if let Ok(pos) = window.outer_position() {
                        pending_pos = None;
                        if saved_pos != (Some(pos.x), Some(pos.y)) {
                            saved_pos = (Some(pos.x), Some(pos.y));
                            save_pos_bg(pos.x, pos.y);
                        }
                    }
                } else if let Err(error) = window.drag_window() {
                    // Touch/pen press with no prior CursorMoved: still drag,
                    // never drop the gesture.
                    eprintln!("[utterly] pill drag: {error}");
                } else if let Ok(pos) = window.outer_position() {
                    pending_pos = None;
                    if saved_pos != (Some(pos.x), Some(pos.y)) {
                        saved_pos = (Some(pos.x), Some(pos.y));
                        save_pos_bg(pos.x, pos.y);
                    }
                }
            }
            Event::WindowEvent {
                event: WindowEvent::Focused(false),
                ..
            } => {
                // Focus loss is normal (dictating into other apps while the
                // always-on-top pill stays visible) — ignore. Re-show paths:
                // tray click, or the next Listening update (hotkey press).
            }
            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { .. },
                ..
            } => {
                // All hotkeys are global; nothing to handle locally in v1.
            }
            _ => {}
        }
    });
    r.map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn draw<D, W>(
    surface: &mut Surface<D, W>,
    width: u32,
    height: u32,
    mode: Mode,
    level: f32,
    t: f64,
    verbatim_live: bool,
    hover_inside: bool,
    close_hot: bool,
    title: &str,
    scale: f32,
) -> Result<(), String>
where
    D: raw_window_handle::HasDisplayHandle,
    W: raw_window_handle::HasWindowHandle,
{
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return Ok(());
    }
    surface
        .resize(
            std::num::NonZeroU32::new(width).unwrap(),
            std::num::NonZeroU32::new(height).unwrap(),
        )
        .map_err(|e| e.to_string())?;
    let mut buf = surface.buffer_mut().map_err(|e| e.to_string())?;

    // Palette: macOS graphite pill (opaque; no per-pixel alpha — Windows
    // shows unpainted/transparent regions as white). Dark theme only.
    let bg: u32 = 0x1E_1E_20; // obsidian dark
    let border: u32 = 0x3A_3A_3C; // subtle separator outline

    let (dr, dg, db) = dot_color(mode, t);
    let dot: u32 = ((dr as u32) << 16) | ((dg as u32) << 8) | db as u32;
    let dot_cx = 28.0f32;
    let dot_cy = 32.0f32;
    let dot_r = 8.5f32;

    let pw = PILL_W as f32;
    let ph = PILL_H as f32;
    let radius = ph / 2.0; // 32.0

    let spinning = mode == Mode::Transcribing && !verbatim_live;
    let breathing = mode == Mode::Listening;
    let pulse = ((t * 4.0).sin() * 0.5 + 0.5) as f32;
    let glow_r = 11.5 + 3.5 * pulse;

    let clean = display_text(title).trim();
    let show_waveform = mode == Mode::Listening && clean.is_empty();
    let norm = (level / 4000.0).clamp(0.0, 1.0).sqrt();

    let close_cx = pw - 22.0f32;
    let close_cy = 18.0f32;

    let scale_x = width as f32 / pw;
    let scale_y = height as f32 / ph;

    for y in 0..h {
        let ly = (y as f32 + 0.5) / scale_y;
        for x in 0..w {
            let lx = (x as f32 + 0.5) / scale_x;

            // Rounded corners: reject outside semicircle caps.
            let in_corner = |cx: f32, cy: f32| {
                let ddx = lx - cx;
                let ddy = ly - cy;
                ddx * ddx + ddy * ddy > radius * radius
            };
            let corner_cut = (lx < radius && ly < radius && in_corner(radius, radius))
                || (lx >= pw - radius && ly < radius && in_corner(pw - radius, radius))
                || (lx < radius && ly >= ph - radius && in_corner(radius, ph - radius))
                || (lx >= pw - radius && ly >= ph - radius && in_corner(pw - radius, ph - radius));
            if corner_cut {
                buf[y * w + x] = bg;
                continue;
            }

            // Border: 1px outline around both straight edges and circular endcaps
            let in_border = if lx < radius {
                let ddx = lx - radius;
                let ddy = ly - radius;
                let r2 = ddx * ddx + ddy * ddy;
                r2 >= (radius - 1.0) * (radius - 1.0)
            } else if lx >= pw - radius {
                let ddx = lx - (pw - radius);
                let ddy = ly - radius;
                let r2 = ddx * ddx + ddy * ddy;
                r2 >= (radius - 1.0) * (radius - 1.0)
            } else {
                ly < 1.0 || ly >= ph - 1.0
            };
            let mut px = if in_border { border } else { bg };

            // Mic dot and breathing glow ring
            let ddx = lx - dot_cx;
            let ddy = ly - dot_cy;
            let r2 = ddx * ddx + ddy * ddy;
            if r2 <= dot_r * dot_r {
                px = dot;
            } else if breathing && r2 <= glow_r * glow_r {
                let dist = r2.sqrt() - dot_r;
                let max_dist = glow_r - dot_r;
                let alpha = ((1.0 - dist / max_dist) * (0.25 + 0.35 * pulse)).clamp(0.0, 1.0);
                let pr = ((px >> 16) & 0xFF) as f32;
                let pg = ((px >> 8) & 0xFF) as f32;
                let pb = (px & 0xFF) as f32;
                let out_r = (pr + (255.0 - pr) * alpha) as u32;
                let out_g = (pg + (69.0 - pg) * alpha) as u32;
                let out_b = (pb + (58.0 - pb) * alpha) as u32;
                px = (out_r << 16) | (out_g << 8) | out_b;
            }

            // Transcribing spinner
            if spinning && (12.0 * 12.0..=16.0 * 16.0).contains(&r2) {
                let ang = (ddy as f64).atan2(ddx as f64);
                let d = (ang - spinner_angle(t) + std::f64::consts::PI)
                    .rem_euclid(2.0 * std::f64::consts::PI)
                    - std::f64::consts::PI;
                if d.abs() < 0.65 {
                    px = 0x30_D1_58;
                }
            }

            // Fluid waveform bars (when show_waveform)
            if show_waveform && (18.0..=46.0).contains(&ly) {
                let bar_centers = [46.0f32, 52.0, 58.0, 64.0, 70.0];
                for (i, &bc) in bar_centers.iter().enumerate() {
                    if (lx - bc).abs() <= 1.5 {
                        let phase = t * 7.0 + (i as f64) * 0.85;
                        let wave = phase.sin().abs() as f32;
                        let bar_h = 4.0 + (norm * 20.0 * (0.35 + 0.65 * wave)).clamp(0.0, 22.0);
                        if (ly - dot_cy).abs() * 2.0 <= bar_h {
                            px = 0xFF_45_3A;
                        }
                    }
                }
            }

            // Close button overlay (tiny X at top right, hover-only)
            if hover_inside {
                let cdx = lx - close_cx;
                let cdy = ly - close_cy;
                let cr2 = cdx * cdx + cdy * cdy;
                if close_hot && cr2 <= 8.5 * 8.5 {
                    px = 0x34_34_38; // circular hover disc
                }
                let i = lx - (close_cx - 3.5);
                let j = ly - (close_cy - 3.5);
                if (0.0..7.0).contains(&i)
                    && (0.0..7.0).contains(&j)
                    && ((i - j).abs() <= 1.0 || (i + j - 6.0).abs() <= 1.0)
                {
                    px = if close_hot { 0xFF_FF_FF } else { 0x8E_8E_93 };
                }
            }

            buf[y * w + x] = px;
        }
    }

    // Large, crisp Apple typography text overlay
    let label = match mode {
        Mode::Listening => {
            if clean.is_empty() {
                "Listening..."
            } else {
                clean
            }
        }
        Mode::Transcribing => {
            if clean.is_empty() {
                "Transcribing..."
            } else {
                clean
            }
        }
        Mode::Idle => {
            if clean.is_empty() {
                "Hold Alt+Space to dictate"
            } else {
                clean
            }
        }
    };

    let text_color = match mode {
        Mode::Listening if !clean.is_empty() => 0x00FF_FFFF,
        Mode::Transcribing if !clean.is_empty() => 0x00FF_FFFF,
        Mode::Idle => {
            let lower = label.to_ascii_lowercase();
            if lower.contains("error") || lower.contains("fail") || lower.contains("no api key") {
                0x00FF_9F0A // warning amber
            } else {
                0x00ED_EDED
            }
        }
        _ => 0x00A1_A1A6, // placeholder grey
    };

    let formatted = if (mode == Mode::Listening || mode == Mode::Transcribing) && !clean.is_empty()
    {
        format_live_text(label, 32)
    } else {
        label.to_string()
    };

    let text_lx = if show_waveform { 80.0 } else { 50.0 };
    let dst_x = (text_lx * scale_x).round() as usize;
    let dst_y = (14.0 * scale_y).round() as usize;
    let text_rx = pw - 34.0;
    let dst_w = ((text_rx - text_lx) * scale_x).round() as usize;
    let dst_h = ((ph - 28.0) * scale_y).round() as usize;

    render_text(
        &mut buf, w, dst_x, dst_y, dst_w, dst_h, &formatted, scale, text_color, bg,
    );

    buf.present().map_err(|e| e.to_string())
}

#[cfg(target_os = "windows")]
#[allow(clippy::upper_case_acronyms)]
#[repr(C)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(target_os = "windows")]
#[allow(clippy::upper_case_acronyms)]
#[repr(C)]
struct BITMAPINFOHEADER {
    bi_size: u32,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: u32,
    bi_size_image: u32,
    bi_x_pels_per_meter: i32,
    bi_y_pels_per_meter: i32,
    bi_clr_used: u32,
    bi_clr_important: u32,
}

#[cfg(target_os = "windows")]
#[allow(clippy::upper_case_acronyms)]
#[repr(C)]
struct BITMAPINFO {
    bmi_header: BITMAPINFOHEADER,
    bmi_colors: [u32; 1],
}

#[cfg(target_os = "windows")]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "windows")]
struct GdiTextCache {
    hdc: *mut std::ffi::c_void,
    hbmp: *mut std::ffi::c_void,
    old_bmp: *mut std::ffi::c_void,
    p_bits: *mut u32,
    bmp_w: usize,
    bmp_h: usize,
    hfont: *mut std::ffi::c_void,
    old_font: *mut std::ffi::c_void,
    font_scale: f32,
    cached_text: String,
    cached_color: u32,
    cached_bg_color: u32,
    cached_scale: f32,
    cached_w: usize,
    cached_h: usize,
    cached_pixels: Vec<u32>,
}

#[cfg(target_os = "windows")]
impl GdiTextCache {
    fn new() -> Self {
        Self {
            hdc: std::ptr::null_mut(),
            hbmp: std::ptr::null_mut(),
            old_bmp: std::ptr::null_mut(),
            p_bits: std::ptr::null_mut(),
            bmp_w: 0,
            bmp_h: 0,
            hfont: std::ptr::null_mut(),
            old_font: std::ptr::null_mut(),
            font_scale: 0.0,
            cached_text: String::new(),
            cached_color: 0,
            cached_bg_color: 0,
            cached_scale: 0.0,
            cached_w: 0,
            cached_h: 0,
            cached_pixels: Vec::new(),
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for GdiTextCache {
    fn drop(&mut self) {
        unsafe {
            if !self.hdc.is_null() {
                if !self.old_font.is_null() {
                    SelectObject(self.hdc, self.old_font);
                }
                if !self.hfont.is_null() {
                    DeleteObject(self.hfont);
                }
                if !self.old_bmp.is_null() {
                    SelectObject(self.hdc, self.old_bmp);
                }
                if !self.hbmp.is_null() {
                    DeleteObject(self.hbmp);
                }
                DeleteDC(self.hdc);
            }
        }
    }
}

#[cfg(target_os = "windows")]
thread_local! {
    static GDI_CACHE: std::cell::RefCell<GdiTextCache> = std::cell::RefCell::new(GdiTextCache::new());
}

#[cfg(target_os = "windows")]
#[allow(clippy::too_many_arguments)]
fn render_text(
    buf: &mut [u32],
    stride: usize,
    dst_x: usize,
    dst_y: usize,
    dst_w: usize,
    dst_h: usize,
    text: &str,
    scale: f32,
    color: u32,
    bg_color: u32,
) {
    if text.trim().is_empty() || dst_w == 0 || dst_h == 0 || stride == 0 {
        return;
    }
    let total_rows = buf.len() / stride;
    if dst_x + dst_w > stride || dst_y + dst_h > total_rows {
        return;
    }

    GDI_CACHE.with(|cache_cell| {
        let mut cache = cache_cell.borrow_mut();

        // If text content, dimensions, scale, and colors are unchanged, copy from cache (0 GDI calls!)
        let is_cached = cache.cached_text == text
            && cache.cached_w == dst_w
            && cache.cached_h == dst_h
            && (cache.cached_scale - scale).abs() < 1e-4
            && cache.cached_color == color
            && cache.cached_bg_color == bg_color
            && cache.cached_pixels.len() == dst_w * dst_h;

        if is_cached {
            for y in 0..dst_h {
                let src_start = y * dst_w;
                let dst_start = (dst_y + y) * stride + dst_x;
                buf[dst_start..dst_start + dst_w]
                    .copy_from_slice(&cache.cached_pixels[src_start..src_start + dst_w]);
            }
            return;
        }

        unsafe {
            if cache.hdc.is_null() {
                cache.hdc = CreateCompatibleDC(std::ptr::null_mut());
                if cache.hdc.is_null() {
                    return;
                }
            }

            // Reuse existing DIB section if dimensions match, else recreate
            if cache.hbmp.is_null() || cache.bmp_w != dst_w || cache.bmp_h != dst_h {
                if !cache.hbmp.is_null() {
                    if !cache.old_bmp.is_null() {
                        SelectObject(cache.hdc, cache.old_bmp);
                        cache.old_bmp = std::ptr::null_mut();
                    }
                    DeleteObject(cache.hbmp);
                    cache.hbmp = std::ptr::null_mut();
                    cache.p_bits = std::ptr::null_mut();
                }

                let mut bmi: BITMAPINFO = std::mem::zeroed();
                bmi.bmi_header.bi_size = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bmi.bmi_header.bi_width = dst_w as i32;
                bmi.bmi_header.bi_height = -(dst_h as i32); // top-down DIB
                bmi.bmi_header.bi_planes = 1;
                bmi.bmi_header.bi_bit_count = 32;
                bmi.bmi_header.bi_compression = 0; // BI_RGB

                let mut p_bits: *mut std::ffi::c_void = std::ptr::null_mut();
                let hbmp = CreateDIBSection(
                    cache.hdc,
                    &bmi,
                    0,
                    &mut p_bits,
                    std::ptr::null_mut(),
                    0,
                );
                if hbmp.is_null() || p_bits.is_null() {
                    return;
                }
                cache.hbmp = hbmp;
                cache.p_bits = p_bits as *mut u32;
                cache.bmp_w = dst_w;
                cache.bmp_h = dst_h;
                let old = SelectObject(cache.hdc, hbmp);
                if cache.old_bmp.is_null() {
                    cache.old_bmp = old;
                }
            }

            // Reuse existing Font handle if scale matches, else recreate
            if cache.hfont.is_null() || (cache.font_scale - scale).abs() > 1e-4 {
                if !cache.hfont.is_null() {
                    if !cache.old_font.is_null() {
                        SelectObject(cache.hdc, cache.old_font);
                        cache.old_font = std::ptr::null_mut();
                    }
                    DeleteObject(cache.hfont);
                    cache.hfont = std::ptr::null_mut();
                }

                let font_face = wide("Segoe UI");
                let font_h = -(18.0 * scale).round() as i32;
                let hfont = CreateFontW(
                    font_h,
                    0,
                    0,
                    0,
                    600, // FW_SEMIBOLD
                    0,
                    0,
                    0,
                    1, // DEFAULT_CHARSET
                    0,
                    0,
                    5, // CLEARTYPE_QUALITY
                    0,
                    font_face.as_ptr(),
                );
                if !hfont.is_null() {
                    let old = SelectObject(cache.hdc, hfont);
                    if cache.old_font.is_null() {
                        cache.old_font = old;
                    }
                    cache.hfont = hfont;
                    cache.font_scale = scale;
                }
            }

            let dib_slice = std::slice::from_raw_parts_mut(cache.p_bits, dst_w * dst_h);
            dib_slice.fill(bg_color);

            let to_colorref = |c: u32| -> u32 {
                let r = (c >> 16) & 0xFF;
                let g = (c >> 8) & 0xFF;
                let b = c & 0xFF;
                r | (g << 8) | (b << 16)
            };

            SetBkColor(cache.hdc, to_colorref(bg_color));
            SetTextColor(cache.hdc, to_colorref(color));
            SetBkMode(cache.hdc, 1); // TRANSPARENT

            let mut rect = RECT {
                left: 0,
                top: 0,
                right: dst_w as i32,
                bottom: dst_h as i32,
            };
            let wide_str = wide(text);
            DrawTextW(
                cache.hdc,
                wide_str.as_ptr(),
                wide_str.len() as i32 - 1,
                &mut rect,
                0x0000_8824, // DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS
            );

            if cache.cached_pixels.len() != dst_w * dst_h {
                cache.cached_pixels.resize(dst_w * dst_h, 0);
            }
            cache.cached_pixels.copy_from_slice(dib_slice);
            cache.cached_text.clear();
            cache.cached_text.push_str(text);
            cache.cached_color = color;
            cache.cached_bg_color = bg_color;
            cache.cached_scale = scale;
            cache.cached_w = dst_w;
            cache.cached_h = dst_h;

            for y in 0..dst_h {
                let src_start = y * dst_w;
                let dst_start = (dst_y + y) * stride + dst_x;
                buf[dst_start..dst_start + dst_w]
                    .copy_from_slice(&dib_slice[src_start..src_start + dst_w]);
            }
        }
    });
}

#[cfg(not(target_os = "windows"))]
#[allow(clippy::too_many_arguments)]
fn render_text(
    _buf: &mut [u32],
    _stride: usize,
    _dst_x: usize,
    _dst_y: usize,
    _dst_w: usize,
    _dst_h: usize,
    _text: &str,
    _scale: f32,
    _color: u32,
    _bg_color: u32,
) {
}

#[cfg(target_os = "windows")]
fn set_rounded_window(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ffi::c_void;

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let size = window.outer_size();
    let diameter = (PILL_H as f64 * window.scale_factor()).round() as i32;
    unsafe {
        // Win32 excludes right/bottom edges: +1 avoids a 1px clip.
        let region = CreateRoundRectRgn(
            0,
            0,
            size.width as i32 + 1,
            size.height as i32 + 1,
            diameter,
            diameter,
        );
        if !region.is_null() && SetWindowRgn(handle.hwnd.get() as *mut c_void, region, 1) == 0 {
            DeleteObject(region);
        }
    }
}

#[cfg(target_os = "windows")]
fn apply_window_chrome(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ffi::c_void;

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    unsafe {
        // TOOLWINDOW = no taskbar button + no Alt+Tab; NOACTIVATE = click
        // never steals focus from the target editor (paste lands correctly).
        const GWL_EXSTYLE: i32 = -20;
        const WS_EX_TOOLWINDOW: isize = 0x0000_0080;
        const WS_EX_NOACTIVATE: isize = 0x0800_0000;
        const SWP_NOMOVE: u32 = 0x0002;
        const SWP_NOSIZE: u32 = 0x0001;
        const SWP_NOZORDER: u32 = 0x0004;
        const SWP_FRAMECHANGED: u32 = 0x0020;
        let hwnd = handle.hwnd.get() as *mut c_void;
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE);
        let _ = SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_window_chrome(_window: &Window) {}

#[cfg(target_os = "windows")]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetWindowLongPtrW(window: *mut std::ffi::c_void, index: i32) -> isize;
    fn SetWindowLongPtrW(window: *mut std::ffi::c_void, index: i32, style: isize) -> isize;
    fn SetWindowPos(
        window: *mut std::ffi::c_void,
        after: *mut std::ffi::c_void,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    fn SetWindowRgn(
        window: *mut std::ffi::c_void,
        region: *mut std::ffi::c_void,
        redraw: i32,
    ) -> i32;
    fn DrawTextW(
        hdc: *mut std::ffi::c_void,
        text: *const u16,
        len: i32,
        rect: *mut RECT,
        format: u32,
    ) -> i32;
}

#[cfg(not(target_os = "windows"))]
fn set_rounded_window(_window: &Window) {}

#[cfg(target_os = "windows")]
#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateRoundRectRgn(
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        width: i32,
        height: i32,
    ) -> *mut std::ffi::c_void;
    fn CreateCompatibleDC(hdc: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn DeleteDC(hdc: *mut std::ffi::c_void) -> i32;
    fn CreateDIBSection(
        hdc: *mut std::ffi::c_void,
        pbmi: *const BITMAPINFO,
        usage: u32,
        ppv_bits: *mut *mut std::ffi::c_void,
        h_section: *mut std::ffi::c_void,
        offset: u32,
    ) -> *mut std::ffi::c_void;
    fn SelectObject(
        hdc: *mut std::ffi::c_void,
        obj: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn DeleteObject(object: *mut std::ffi::c_void) -> i32;
    fn SetBkMode(hdc: *mut std::ffi::c_void, mode: i32) -> i32;
    fn SetBkColor(hdc: *mut std::ffi::c_void, color: u32) -> u32;
    fn SetTextColor(hdc: *mut std::ffi::c_void, color: u32) -> u32;
    fn CreateFontW(
        c_height: i32,
        c_width: i32,
        c_escapement: i32,
        c_orientation: i32,
        c_weight: i32,
        b_italic: u32,
        b_underline: u32,
        b_strike_out: u32,
        i_char_set: u32,
        i_out_precision: u32,
        i_clip_precision: u32,
        i_quality: u32,
        i_pitch_and_family: u32,
        psz_face_name: *const u16,
    ) -> *mut std::ffi::c_void;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_center_hits() {
        assert!(hit_mic(28.0, 32.0));
        assert!(hit_mic(28.0 + 16.0, 32.0), "r=16 edge counts");
    }

    #[test]
    fn mic_misses_away_from_dot() {
        assert!(!hit_mic(200.0, 32.0));
        assert!(!hit_mic(28.0, 32.0 + 16.1));
        assert!(!hit_mic(0.0, 0.0));
    }

    #[test]
    fn close_corner_hits() {
        let w = PILL_W as f32;
        assert!(hit_close(w - 22.0, 18.0, w), "box center");
        assert!(hit_close(w - 22.0 + 7.0, 18.0, w), "box right edge");
        assert!(hit_close(w - 22.0, 18.0 - 7.0, w), "box top edge");
    }

    #[test]
    fn close_misses_outside_box() {
        let w = PILL_W as f32;
        assert!(!hit_close(200.0, 32.0, w));
        assert!(!hit_close(28.0, 32.0, w), "mic dot is not close");
        assert!(
            !hit_close(w - 22.0, 34.0, w),
            "middle of pill allows dragging"
        );
        assert!(!hit_close(w - 22.0, 50.0, w), "below box");
    }

    #[test]
    fn auto_hide_keeps_errors_visible() {
        assert!(should_auto_hide("Utterly — hello world"));
        assert!(should_auto_hide("Utterly — hold Alt+Space to dictate"));
        assert!(should_auto_hide("Utterly — connect failed: timeout"));
        assert!(should_auto_hide("Utterly — heard nothing, try again"));
        assert!(should_auto_hide("Utterly — saved"));
        assert!(should_auto_hide("Utterly — mic stream recovered, hold Alt+Space to dictate"));
        assert!(should_auto_hide("Utterly — paste failed; transcript is on clipboard: hello"));
        assert!(!should_auto_hide("Utterly — no API key: copy one"));
        assert!(!should_auto_hide("Utterly — mic error: not found"));
        assert!(!should_auto_hide("Utterly — mic error: no input device"));
    }

    #[test]
    fn spinner_advances_and_wraps() {
        let pi = std::f64::consts::PI;
        assert!(spinner_angle(0.0).abs() < 1e-9);
        assert!((spinner_angle(0.125) - pi / 2.0).abs() < 1e-9);
        assert!((spinner_angle(0.25) - pi).abs() < 1e-9);
        assert!(spinner_angle(0.5).abs() < 1e-9, "full turn wraps to 0");
        assert!(spinner_angle(1.0).abs() < 1e-9);
    }

    #[test]
    fn format_live_text_short_unchanged() {
        assert_eq!(format_live_text("hello world", 36), "hello world");
    }

    #[test]
    fn format_live_text_long_shows_tail() {
        let long = "the quick brown fox jumps over the lazy dog and runs away";
        let formatted = format_live_text(long, 25);
        assert!(formatted.starts_with("… "));
        assert!(formatted.ends_with("runs away"));
    }

    #[test]
    fn display_text_strips_various_prefixes() {
        assert_eq!(display_text("Utterly ● hello"), "hello");
        assert_eq!(display_text("Utterly … transcribing hello"), "hello");
        assert_eq!(display_text("Utterly … transcribing"), "");
        assert_eq!(display_text("Utterly — ready"), "ready");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_render_text_gdi() {
        let mut buf = vec![0x001E_1E20_u32; 380 * 64];
        render_text(
            &mut buf,
            380,
            50,
            14,
            200,
            36,
            "Hello World",
            1.0,
            0x00FF_FFFF,
            0x001E_1E20,
        );
        let modified = buf.iter().filter(|&&p| p != 0x001E_1E20).count();
        assert!(
            modified > 0,
            "GDI text rendering must draw text pixels into buffer"
        );

        // Repeated call with same parameters exercises the fast cache path
        let mut buf2 = vec![0x001E_1E20_u32; 380 * 64];
        render_text(
            &mut buf2,
            380,
            50,
            14,
            200,
            36,
            "Hello World",
            1.0,
            0x00FF_FFFF,
            0x001E_1E20,
        );
        assert_eq!(buf, buf2, "Cached render must match initial render bit-for-bit");
    }
}
