//! macOS-like dictation pill: winit + softbuffer, no GPU, no font engine.
//! - 380×64 pill, OS window decorations disabled (borderless, always-on-top,
//!   opaque dark fill + rounded corners).
//! - Text rendering is delegated to the OS window TITLE (zero font deps,
//!   ~0 KiB): pill surface shows only state dot + 24-bar level meter.
//! - Dirty redraw: present() only on state/level/text change, else sleep.
//!   Idle CPU ~0%, framebuffer 380*64*4 = 95 KiB.

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

/// Mic-dot hit region (logical px): circle at (32,32) r=14 — slightly larger
/// than the drawn r=10 dot for touch.
pub fn hit_mic(x: f32, y: f32) -> bool {
    let dx = x - 32.0;
    let dy = y - 32.0;
    dx * dx + dy * dy <= 14.0 * 14.0
}

/// Close-box hit region (logical px): 16x16 box at top-right (w-20, 4).
pub fn hit_close(x: f32, y: f32, w: f32) -> bool {
    (w - 20.0..=w - 4.0).contains(&x) && (4.0..=20.0).contains(&y)
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

/// Press-shake kinematics (pure, tested): x-offset in px and uniform scale
/// for `elapsed_ms` since entering Listening. The shake decays to rest and
/// the 200 ms window matches the `press_at` gate in `run_pill`.
pub fn shake_offset(elapsed_ms: u64) -> (i32, f32) {
    if elapsed_ms >= 200 {
        return (0, 1.0);
    }
    let e = elapsed_ms as f64;
    let decay = 1.0 - e / 200.0;
    let dx = (e * 0.06).sin() * 2.0 * decay;
    let scale = 1.0 + 0.04 * (std::f64::consts::PI * e / 200.0).sin();
    (dx.round() as i32, scale as f32)
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
    mut tray: Option<tray_icon::TrayIcon>,
    menu: crate::tray::TrayMenu,
    sync_rx: Receiver<(String, String, String)>,
    gtk_pump: bool,
    ui_cmd_tx: std::sync::mpsc::Sender<UiCmd>,
) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| e.to_string())?;
    let attrs = Window::default_attributes()
        .with_title(initial_title)
        .with_inner_size(LogicalSize::new(PILL_W, PILL_H))
        .with_decorations(false)
        .with_transparent(false)
        .with_window_level(WindowLevel::AlwaysOnTop)
        .with_resizable(false);
    let window: Arc<Window> = Arc::new(
        #[allow(deprecated)] // pre-run creation is exactly our case (single pill window)
        event_loop.create_window(attrs).map_err(|e| e.to_string())?,
    );
    // Center the pill horizontally in the lower third of its monitor, like
    // macOS dictation. Falls back to the WM default without monitor info.
    if let Some(monitor) = window.current_monitor() {
        let ms = monitor.size();
        let mp = monitor.position();
        let x = mp.x + (ms.width as i32 - PILL_W as i32) / 2;
        let y = mp.y + (ms.height as i32 * 3) / 4 - PILL_H as i32 / 2;
        window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
    }

    let context = Context::new(window.clone()).map_err(|e| e.to_string())?;
    let mut surface = Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;
    // `tray` arrives built (with its options menu) from main(); the pill loop
    // only refreshes its icon/tooltip on state changes (see tray_dirty below).

    let mut mode = Mode::Idle;
    let mut level: f32 = 0.0;
    let mut title = String::from("Utterly — hold Ctrl+Space to dictate");
    let mut dirty = true;
    // Press animation: timestamp of the last transition into Listening.
    // Render-only state (no click handling — Task 5 owns hit-test regions).
    let mut press_at: Option<Instant> = None;
    // Hover position in logical px (CursorMoved); drives the close-X overlay.
    let mut hover: Option<(f32, f32)> = None;
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

    // winit 0.30: run() takes ownership; use run_ondemand-friendly closure.
    #[allow(deprecated)]
    let r = event_loop.run(move |event, elwt| {
        // Drain pending updates every event + every ~33ms via AboutToWait.
        let mut tray_dirty = false;
        while let Ok(u) = rx.try_recv() {
            if u.mode != mode {
                if u.mode == Mode::Listening {
                    press_at = Some(Instant::now());
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
        if tray_dirty {
            let preview: String = title.chars().take(60).collect();
            crate::tray::set_mode(&mut tray, mode, &preview);
        }
        // Radio checkmarks are applied here (main thread owns the muda items).
        while let Ok((mic, hk, mode)) = sync_rx.try_recv() {
            menu.sync(&mic, &hk, &mode);
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
                // 30 fps cap for animations (pulse + transcribing spinner);
                // sleep otherwise so idle CPU stays ~0%.
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
                if dirty && last_frame.elapsed() >= Duration::from_millis(33) {
                    // attack/release smoothing without a history buffer
                    if level > smooth {
                        smooth = level;
                    } else {
                        smooth += (level - smooth) * 0.25;
                    }
                    // Press-shake window: 200 ms after entering Listening.
                    let press_ms: Option<u64> = match press_at {
                        Some(p) if mode == Mode::Listening => {
                            let ms = p.elapsed().as_millis() as u64;
                            if ms < 200 {
                                Some(ms)
                            } else {
                                press_at = None;
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Err(e) = draw(
                        &mut surface,
                        mode,
                        smooth,
                        start.elapsed().as_secs_f64(),
                        press_ms,
                        verbatim_live,
                        hover,
                    ) {
                        eprintln!("[utterly] pill draw: {e}");
                    }
                    last_frame = Instant::now();
                    dirty = false;
                }
                // Keep the loop mostly asleep: ~30 wakeups/sec max.
                elwt.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_millis(33),
                ));
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
                let next = (logical.x, logical.y);
                if hover != Some(next) {
                    hover = Some(next);
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
                if let Some((x, y)) = hover {
                    let w = PILL_W as f32;
                    if hit_close(x, y, w) {
                        window.set_visible(false);
                        hidden = true;
                        let _ = ui_cmd_tx.send(UiCmd::Hide);
                    } else if hit_mic(x, y) {
                        let _ = ui_cmd_tx.send(UiCmd::MicToggle);
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

fn draw<D, W>(
    surface: &mut Surface<D, W>,
    mode: Mode,
    level: f32,
    t: f64,
    press_ms: Option<u64>,
    verbatim_live: bool,
    hover: Option<(f32, f32)>,
) -> Result<(), String>
where
    D: raw_window_handle::HasDisplayHandle,
    W: raw_window_handle::HasWindowHandle,
{
    let (w, h) = (PILL_W as usize, PILL_H as usize);
    surface
        .resize(
            std::num::NonZeroU32::new(PILL_W).unwrap(),
            std::num::NonZeroU32::new(PILL_H).unwrap(),
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
    // Opaque full-surface paint: covers HiDPI scaled buffers too so no
    // unpainted strip ever shows (e.g. white on Windows). The x/y loop below
    // rewrites the 380x64 region; any excess pixels stay bg.
    for px in buf.iter_mut() {
        *px = bg;
    }
    let (dr, dg, db) = dot_color(mode, t);
    let dot: u32 = ((dr as u32) << 16) | ((dg as u32) << 8) | db as u32;
    // Press shake (Listening entry, 200 ms window): pixel offset + in-place
    // grow. The window never resizes (PILL_W/H fixed); "grow" is inner
    // bar/dot scaling, i.e. padding shrink.
    let (shake_dx, shake_scale) = press_ms.map(shake_offset).unwrap_or((0, 1.0));
    let dot_cx = 32 + shake_dx;
    let dot_r2 = (10.0 * shake_scale).round() as i32;
    // Mode-aware Transcribing: smart-mode finals arrive after the turn end,
    // so spin; live verbatim types instantly over the websocket, so there is
    // nothing to wait on — show the listening-style meter instead. Chose
    // meter-over-spinner (not skip-Transcribing) so the green dot + title
    // still mark the state.
    let spinning = mode == Mode::Transcribing && !verbatim_live;

    // Rounded-rect mask radii.
    let radius: i32 = 16;
    // Hover close-X: shown whenever the cursor is inside the pill, drawn
    // last so it overlays everything. 12x12 X inside the 16x16 hit box at
    // top-right (w-20, 4); grey, brightens to white over the box itself.
    // Meter bars are untouched (they live at y 20..44, below the X).
    let pw = PILL_W as f32;
    let show_x =
        hover.is_some_and(|(hx, hy)| hx >= 0.0 && hy >= 0.0 && hx < pw && hy < PILL_H as f32);
    let x_hot = hover.is_some_and(|(hx, hy)| hit_close(hx, hy, pw));
    let x_color: u32 = if x_hot { 0xFF_FF_FF } else { 0x8E_8E_93 };
    let x0 = w as i32 - 18; // 12px X: x in [w-18, w-7)
    let y0 = 6; //            y in [6, 18)
                // Meter: 24 bars, height ∝ smoothed RMS (log-ish via sqrt), peak-hold omitted
                // to keep state at 1 float.
    let norm = (level / 4000.0).clamp(0.0, 1.0).sqrt();

    for y in 0..h {
        for x in 0..w {
            let xi = x as i32;
            let yi = y as i32;
            let dx = (xi.min(w as i32 - 1 - xi)).min(radius);
            let _ = dx;
            // Rounded corners: reject outside quarter-circles.
            let in_corner = |cx: i32, cy: i32| {
                let ddx = xi - cx;
                let ddy = yi - cy;
                ddx * ddx + ddy * ddy > radius * radius
            };
            let corner_cut = (xi < radius && yi < radius && in_corner(radius, radius))
                || (xi >= w as i32 - radius
                    && yi < radius
                    && in_corner(w as i32 - 1 - radius, radius))
                || (xi < radius
                    && yi >= h as i32 - radius
                    && in_corner(radius, h as i32 - 1 - radius))
                || (xi >= w as i32 - radius
                    && yi >= h as i32 - radius
                    && in_corner(w as i32 - 1 - radius, h as i32 - 1 - radius));
            if corner_cut {
                buf[y * w + x] = bg; // opaque: no transparency artifact on Windows
                continue;
            }
            // Border: 1px outline.
            let is_border = xi == 0 || yi == 0 || xi == w as i32 - 1 || yi == h as i32 - 1;
            let mut px = if is_border { border } else { bg };

            // Mic dot: circle at (32 + shake, 32), r=10*scale.
            {
                let ddx = xi - dot_cx;
                let ddy = yi - 32;
                if ddx * ddx + ddy * ddy <= dot_r2 * dot_r2 {
                    px = dot;
                }
            }
            // Transcribing spinner (smart mode only): rotating arc on an
            // r=14 ring around the mic dot, driven by wall-clock t at 30fps.
            // Integer ring-band test first; atan2 only on band pixels.
            if spinning {
                let ddx = xi - dot_cx;
                let ddy = yi - 32;
                let r2 = ddx * ddx + ddy * ddy;
                if (12 * 12..=16 * 16).contains(&r2) {
                    let ang = (ddy as f64).atan2(ddx as f64);
                    let d = (ang - spinner_angle(t) + std::f64::consts::PI)
                        .rem_euclid(2.0 * std::f64::consts::PI)
                        - std::f64::consts::PI;
                    if d.abs() < 0.5 {
                        px = 0xFF_FF_FF;
                    }
                }
            }
            // Meter bars: x from 56..364, 24 bars of 10px + 3px gap,
            // shifted by the press shake (visual offset only; Task 5
            // hit-test regions stay separate).
            if (20..44).contains(&yi) {
                let mxi = xi - shake_dx;
                if mxi >= 56 {
                    let bx = (mxi - 56) / 13;
                    if bx < 24 {
                        let frac = (bx as f32 + 1.0) / 24.0;
                        let on = frac <= norm.max(0.04);
                        let bar_h = ((6.0 + 18.0 * frac) * shake_scale) as i32; // taller to the right
                        let cy = 32;
                        if (yi - cy).abs() * 2 <= bar_h && (mxi - 56) % 13 < 10 {
                            px = if on { bar } else { track };
                        }
                    }
                }
            }
            // Close-X overlay (2px diagonals, drawn last = on top).
            if show_x {
                let i = xi - x0;
                let j = yi - y0;
                if (0..12).contains(&i) && (0..12).contains(&j) {
                    let d1 = i - j;
                    let d2 = i + j - 11;
                    if d1 == 0 || d1 == 1 || d2 == 0 || d2 == 1 {
                        px = x_color;
                    }
                }
            }
            buf[y * w + x] = px;
        }
    }
    buf.present().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shake_rests_at_entry_and_after_window() {
        assert_eq!(shake_offset(0), (0, 1.0));
        assert_eq!(shake_offset(200), (0, 1.0));
        assert_eq!(shake_offset(10_000), (0, 1.0));
    }

    #[test]
    fn shake_peaks_mid_press_and_stays_bounded() {
        let (_, s100) = shake_offset(100);
        assert!((s100 as f64 - 1.04).abs() < 1e-6, "scale at 100ms = {s100}");
        for ms in 0..200 {
            let (dx, s) = shake_offset(ms);
            assert!(dx.abs() <= 2, "dx at {ms}ms = {dx}");
            assert!((1.0f32..=1.041).contains(&s), "scale at {ms}ms = {s}");
        }
        assert_ne!(shake_offset(26).0, 0, "shake must displace early");
    }

    #[test]
    fn mic_center_hits() {
        assert!(hit_mic(32.0, 32.0));
        assert!(hit_mic(32.0 + 14.0, 32.0), "r=14 edge counts");
    }

    #[test]
    fn mic_misses_away_from_dot() {
        assert!(!hit_mic(200.0, 32.0));
        assert!(!hit_mic(32.0, 32.0 + 14.1));
        assert!(!hit_mic(0.0, 0.0));
    }

    #[test]
    fn close_corner_hits() {
        let w = PILL_W as f32;
        assert!(hit_close(w - 12.0, 12.0, w), "box center");
        assert!(hit_close(w - 20.0, 4.0, w), "box top-left edge");
    }

    #[test]
    fn close_misses_outside_box() {
        let w = PILL_W as f32;
        assert!(!hit_close(200.0, 32.0, w));
        assert!(!hit_close(32.0, 32.0, w), "mic dot is not close");
        assert!(!hit_close(w - 2.0, 12.0, w), "right of box");
        assert!(!hit_close(w - 12.0, 24.0, w), "below box");
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
