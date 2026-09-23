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

pub const PILL_W: u32 = 252;
pub const PILL_H: u32 = 48;

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

/// Mic-dot hit region (logical px): circle at (22,24) r=14.
pub fn hit_mic(x: f32, y: f32) -> bool {
    let dx = x - 22.0;
    let dy = y - 24.0;
    dx * dx + dy * dy <= 14.0 * 14.0
}

/// Close-box hit region (logical px): a 20x20 target at the right edge.
pub fn hit_close(x: f32, y: f32, w: f32) -> bool {
    (w - 28.0..=w - 8.0).contains(&x) && (14.0..=34.0).contains(&y)
}

pub fn hit_settings(x: f32, y: f32, w: f32) -> bool {
    (w - 64.0..=w - 36.0).contains(&x) && (10.0..=38.0).contains(&y)
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
        settings,
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
    // Keep the compact pill near the bottom edge, clear of the taskbar.
    if let Some(monitor) = window.current_monitor() {
        let ms = monitor.size();
        let mp = monitor.position();
        let win_size = window.outer_size();
        let x = mp.x + (ms.width as i32 - win_size.width as i32) / 2;
        let y = mp.y + ms.height as i32 - win_size.height as i32 - 48;
        window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
    }
    set_rounded_window(&window);

    let context = Context::new(window.clone()).map_err(|e| e.to_string())?;
    let mut surface = Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;

    // Initial paint before making the window visible avoids any white flash or unpainted glitch.
    let init_size = window.inner_size();
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
        false,
        settings.is_some(),
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
    let mut settings_hot = false;
    let mut close_hot = false;
    // Hidden-to-tray state. Hide NEVER exits the loop (no elwt.exit(), no
    // process::exit) — the tray icon keeps the app alive and re-shows the
    // pill (tray click, or the next Listening update from a hotkey press).
    // NOTE: the window handle lives on this thread (created inside run_pill),
    // so the session thread cannot hold an Arc<Window> for re-show; instead
    // the Listening PillUpdate the session already sends on hotkey press
    // re-shows the pill here — same observable behavior.
    let mut hidden = false;
    // Live-verbatim flag from the session thread (see PillUpdate).
    let mut verbatim_live = false;
    let start = Instant::now();
    let mut last_frame = Instant::now();
    // Smoothed meter (attack fast, release slow) — 1 float, no history buffer.
    let mut smooth: f32 = 0.0;

    // Non-blocking drain helper.
    let mut pending_title: Option<String> = None;
    let mut hotkey = String::from("Ctrl+Space");

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
                    // Hotkey press while hidden: re-show the pill. The session
                    // always sends Listening on press, so this covers the
                    // "hotkey Pressed while hidden" path without sharing the
                    // window handle across threads.
                    if hidden {
                        window.set_visible(true);
                        hidden = false;
                    }
                }
                mode = u.mode;
                dirty = true;
                tray_dirty = true;
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
                    window.set_visible(true);
                    hidden = false;
                    dirty = true;
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
                    if let Err(e) = draw(
                        &mut surface,
                        size.width,
                        size.height,
                        mode,
                        smooth,
                        start.elapsed().as_secs_f64(),
                        verbatim_live,
                        hover_inside,
                        settings_hot,
                        close_hot,
                        settings.is_some(),
                    ) {
                        eprintln!("[utterly] pill draw: {e}");
                    }
                    last_frame = Instant::now();
                    dirty = false;
                }
                // Native input wakes immediately. Background channels poll
                // at 10 Hz when idle and at the animation rate when active.
                let wait = if mode == Mode::Listening || mode == Mode::Transcribing {
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
                if cursor.take().is_some() || hover_inside || settings_hot || close_hot {
                    hover_inside = false;
                    settings_hot = false;
                    close_hot = false;
                    dirty = true;
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // OS close (X button / Alt+F4) hides to tray — NEVER exits.
                window.set_visible(false);
                hidden = true;
                let _ = ui_cmd_tx.send(UiCmd::Hide);
            }
            Event::WindowEvent {
                event: WindowEvent::CursorMoved { position, .. },
                ..
            } => {
                let logical: winit::dpi::LogicalPosition<f32> =
                    position.to_logical(window.scale_factor());
                cursor = Some((logical.x, logical.y));
                let inside = logical.x >= 0.0
                    && logical.y >= 0.0
                    && logical.x < PILL_W as f32
                    && logical.y < PILL_H as f32;
                let close = inside && hit_close(logical.x, logical.y, PILL_W as f32);
                let settings_hover = inside && hit_settings(logical.x, logical.y, PILL_W as f32);
                if hover_inside != inside || close_hot != close || settings_hot != settings_hover {
                    hover_inside = inside;
                    close_hot = close;
                    settings_hot = settings_hover;
                    dirty = true;
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
                if let Some((x, y)) = cursor {
                    let w = PILL_W as f32;
                    if hit_close(x, y, w) {
                        window.set_visible(false);
                        hidden = true;
                        let _ = ui_cmd_tx.send(UiCmd::Hide);
                    } else if hit_mic(x, y) {
                        let _ = ui_cmd_tx.send(UiCmd::MicToggle);
                    } else if hit_settings(x, y, w) {
                        if let Some(settings) = &settings {
                            settings.show();
                        }
                    } else if let Err(error) = window.drag_window() {
                        eprintln!("[utterly] pill drag: {error}");
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
    settings_hot: bool,
    close_hot: bool,
    settings_available: bool,
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
    // shows unpainted/transparent regions as white).
    let bg: u32 = 0x1E_1E_20; // near-black
    let border: u32 = 0x3A_3A_3C; // separator grey
    let track: u32 = 0x2C_2C_2E; // meter track
    let bar: u32 = match mode {
        Mode::Idle => 0x48_48_4A,
        Mode::Listening => 0xFF_45_3A, // system red
        // Live verbatim types instantly: listening-style red meter, no
        // spinner (see `spinning` below).
        Mode::Transcribing if verbatim_live => 0xFF_45_3A,
        Mode::Transcribing => 0x30_D1_58, // system green
    };

    let (dr, dg, db) = dot_color(mode, t);
    let dot: u32 = ((dr as u32) << 16) | ((dg as u32) << 8) | db as u32;
    let dot_cx = 22.0;
    let dot_cy = 24.0;
    let dot_r = 8.0;
    // Mode-aware Transcribing: smart-mode finals arrive after the turn end,
    // so spin; live verbatim types instantly over the websocket, so there is
    // nothing to wait on — show the listening-style meter instead. Chose
    // meter-over-spinner (not skip-Transcribing) so the green dot + title
    // still mark the state.
    let spinning = mode == Mode::Transcribing && !verbatim_live;

    // Rounded-rect mask radii.
    let radius = 24.0f32;
    let pw = PILL_W as f32;
    let ph = PILL_H as f32;
    let x_color: u32 = if close_hot { 0xFF_FF_FF } else { 0x8E_8E_93 };
    let norm = (level / 4000.0).clamp(0.0, 1.0).sqrt();

    let scale_x = width as f32 / pw;
    let scale_y = height as f32 / ph;

    for y in 0..h {
        let ly = (y as f32 + 0.5) / scale_y;
        for x in 0..w {
            let lx = (x as f32 + 0.5) / scale_x;

            // Rounded corners: reject outside quarter-circles.
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
                buf[y * w + x] = bg; // opaque: no transparency artifact on Windows
                continue;
            }
            // Border: 1px outline.
            let is_border = x == 0 || y == 0 || x == w - 1 || y == h - 1;
            let mut px = if is_border { border } else { bg };

            // Mic dot: circle at (32 + shake, 32), r=10*scale.
            {
                let ddx = lx - dot_cx;
                let ddy = ly - dot_cy;
                if ddx * ddx + ddy * ddy <= dot_r * dot_r {
                    px = dot;
                }
            }
            // Transcribing spinner (smart mode only): rotating arc on an
            // r=14 ring around the mic dot, driven by wall-clock t at 30fps.
            // Integer ring-band test first; atan2 only on band pixels.
            if spinning {
                let ddx = lx - dot_cx;
                let ddy = ly - dot_cy;
                let r2 = ddx * ddx + ddy * ddy;
                if (12.0 * 12.0..=16.0 * 16.0).contains(&r2) {
                    let ang = (ddy as f64).atan2(ddx as f64);
                    let d = (ang - spinner_angle(t) + std::f64::consts::PI)
                        .rem_euclid(2.0 * std::f64::consts::PI)
                        - std::f64::consts::PI;
                    if d.abs() < 0.5 {
                        px = 0xFF_FF_FF;
                    }
                }
            }
            // Sixteen short bars stay legible in the compact capsule.
            if (14.0..34.0).contains(&ly) && lx >= 48.0 {
                let bx = ((lx - 48.0) / 8.0).floor() as i32;
                if (0..16).contains(&bx) {
                    let frac = (bx as f32 + 1.0) / 16.0;
                    let on = frac <= norm.max(0.04);
                    let bar_h = 4.0 + 12.0 * frac;
                    let bar_rem = (lx - 48.0) - (bx as f32 * 8.0);
                    if (ly - dot_cy).abs() * 2.0 <= bar_h && bar_rem < 5.0 {
                        px = if on { bar } else { track };
                    }
                }
            }
            // Close-X overlay (drawn last = on top).
            if hover_inside {
                let x0 = pw - 23.0;
                let y0 = 18.0;
                let i = lx - x0;
                let j = ly - y0;
                if (0.0..7.0).contains(&i)
                    && (0.0..7.0).contains(&j)
                    && ((i - j).abs() <= 1.0 || (i + j - 6.0).abs() <= 1.0)
                {
                    px = x_color;
                }
            }
            if settings_available {
                let settings_color = if settings_hot { 0xFF_FF_FF } else { 0x8E_8E_93 };
                let knob_x = [198.0, 205.0, 195.0];
                for (row, knob) in [(18.0, knob_x[0]), (24.0, knob_x[1]), (30.0, knob_x[2])] {
                    if ((191.0..207.0).contains(&lx) && (ly - row).abs() <= 0.7)
                        || ((lx - knob).abs() <= 1.5 && (ly - row).abs() <= 1.5)
                    {
                        px = settings_color;
                    }
                }
            }
            buf[y * w + x] = px;
        }
    }
    buf.present().map_err(|e| e.to_string())
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
        let region = CreateRoundRectRgn(
            0,
            0,
            size.width as i32,
            size.height as i32,
            diameter,
            diameter,
        );
        if !region.is_null() && SetWindowRgn(handle.hwnd.get() as *mut c_void, region, 1) == 0 {
            DeleteObject(region);
        }
    }
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
    fn DeleteObject(object: *mut std::ffi::c_void) -> i32;
}

#[cfg(target_os = "windows")]
#[link(name = "user32")]
unsafe extern "system" {
    fn SetWindowRgn(
        window: *mut std::ffi::c_void,
        region: *mut std::ffi::c_void,
        redraw: i32,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_center_hits() {
        assert!(hit_mic(22.0, 24.0));
        assert!(hit_mic(22.0 + 14.0, 24.0), "r=14 edge counts");
    }

    #[test]
    fn mic_misses_away_from_dot() {
        assert!(!hit_mic(200.0, 32.0));
        assert!(!hit_mic(22.0, 24.0 + 14.1));
        assert!(!hit_mic(0.0, 0.0));
    }

    #[test]
    fn close_corner_hits() {
        let w = PILL_W as f32;
        assert!(hit_close(w - 18.0, 24.0, w), "box center");
        assert!(hit_close(w - 28.0, 14.0, w), "box top-left edge");
    }

    #[test]
    fn close_misses_outside_box() {
        let w = PILL_W as f32;
        assert!(!hit_close(200.0, 24.0, w));
        assert!(!hit_close(22.0, 24.0, w), "mic dot is not close");
        assert!(!hit_close(w - 2.0, 24.0, w), "right of box");
        assert!(!hit_close(w - 18.0, 40.0, w), "below box");
    }

    #[test]
    fn settings_button_has_a_separate_hit_area() {
        let w = PILL_W as f32;
        assert!(hit_settings(w - 48.0, 24.0, w));
        assert!(hit_settings(w - 64.0, 10.0, w));
        assert!(!hit_settings(22.0, 24.0, w));
        assert!(!hit_settings(w - 18.0, 24.0, w));
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
}
