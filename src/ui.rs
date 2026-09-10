//! macOS-like dictation pill: winit + softbuffer, no GPU, no font engine.
//! - 380×64 pill, OS window decorations disabled (borderless, always-on-top,
//!   translucent look via dark fill + rounded corners).
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

#[derive(Debug, Clone)]
pub struct PillUpdate {
    pub mode: Mode,
    pub level: f32, // RMS for meter
    pub title: String,
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

/// Run the pill window on the MAIN thread. `rx` receives PillUpdate from the
/// session thread; `tray`+`menu` are owned here because tray-icon/muda handles
/// are !Send on some platforms. `sync_rx` carries (mic, hotkey, mode)
/// triples for radio-checkmark updates. Returns when the window is closed.
pub fn run_pill(
    rx: Receiver<PillUpdate>,
    initial_title: String,
    mut tray: Option<tray_icon::TrayIcon>,
    menu: crate::tray::TrayMenu,
    sync_rx: Receiver<(String, String, String)>,
    gtk_pump: bool,
) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| e.to_string())?;
    let attrs = Window::default_attributes()
        .with_title(initial_title)
        .with_inner_size(LogicalSize::new(PILL_W, PILL_H))
        .with_decorations(false)
        .with_transparent(true)
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
                mode = u.mode;
                dirty = true;
                tray_dirty = true;
            }
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
                // 30 fps cap for the pulse animation; sleep otherwise.
                if last_frame.elapsed() >= Duration::from_millis(33) && mode == Mode::Listening {
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
                    if let Err(e) = draw(&mut surface, mode, smooth, start.elapsed().as_secs_f64())
                    {
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
                elwt.exit();
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

fn draw<D, W>(surface: &mut Surface<D, W>, mode: Mode, level: f32, t: f64) -> Result<(), String>
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

    // Palette: macOS graphite pill.
    let bg: u32 = 0x1E_1E_20; // near-black
    let border: u32 = 0x3A_3A_3C; // separator grey
    let track: u32 = 0x2C_2C_2E; // meter track
    let bar: u32 = match mode {
        Mode::Idle => 0x48_48_4A,
        Mode::Listening => 0xFF_45_3A,    // system red
        Mode::Transcribing => 0x30_D1_58, // system green
    };
    let (dr, dg, db) = dot_color(mode, t);
    let dot: u32 = ((dr as u32) << 16) | ((dg as u32) << 8) | db as u32;

    // Rounded-rect mask radii.
    let radius: i32 = 16;
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
                buf[y * w + x] = 0x00_00_00; // transparent shows desktop
                continue;
            }
            // Border: 1px outline.
            let is_border = xi == 0 || yi == 0 || xi == w as i32 - 1 || yi == h as i32 - 1;
            let mut px = if is_border { border } else { bg };

            // Mic dot: circle at (32, 32) r=10.
            {
                let ddx = xi - 32;
                let ddy = yi - 32;
                if ddx * ddx + ddy * ddy <= 100 {
                    px = dot;
                }
            }
            // Meter bars: x from 56..364, 24 bars of 10px + 3px gap.
            if xi >= 56 && (20..44).contains(&yi) {
                let bx = (xi - 56) / 13;
                if bx < 24 {
                    let frac = (bx as f32 + 1.0) / 24.0;
                    let on = frac <= norm.max(0.04);
                    let bar_h = 6 + (18.0 * frac) as i32; // taller to the right
                    let cy = 32;
                    if (yi - cy).abs() * 2 <= bar_h && (xi - 56) % 13 < 10 {
                        px = if on { bar } else { track };
                    }
                }
            }
            buf[y * w + x] = px;
        }
    }
    buf.present().map_err(|e| e.to_string())
}
