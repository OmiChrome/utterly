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

pub const PILL_W: u32 = 100;
pub const PILL_H: u32 = 36;
const FRAME_TIME: Duration = Duration::from_millis(34);
const EXPAND_TIME: Duration = Duration::from_millis(300);

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
    Position(i32, i32),
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
    if max_chars == 0 {
        return String::new();
    }
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
        if split < chars.len().saturating_sub(5) {
            format!("… {}", chars[split + 1..].iter().collect::<String>())
        } else {
            format!("… {}", chars[start_idx..].iter().collect::<String>())
        }
    }
}

/// True when an Idle title may collapse back into the idle handle. Persistent onboarding ("no API key") and hardware
/// setup errors ("mic error: not found" / "no input device") stay visible until resolved.
/// Temporary notices ("heard nothing", "saved", "recovered", "paste failed") auto-hide.
pub fn should_auto_hide(title: &str) -> bool {
    let t = title.to_ascii_lowercase();
    if t.contains("no api key")
        || (t.contains("api key")
            && (t.contains("copy")
                || t.contains("missing")
                || t.contains("enter")
                || t.contains("set")))
        || t.contains("hotkey error")
        || t.contains("hotkey unavailable")
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
    /// True when the session runs live-verbatim; retained in the protocol.
    /// Both modes collapse to the processing indicator on release.
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
    pub preferences_rx: Receiver<crate::config::Preferences>,
}

/// Native region is the union of transcript and capsule: the gap is click-through.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    width: u32,
    height: u32,
    transcript: bool,
    notice: bool,
}

fn layout(mode: Mode, title: &str) -> Layout {
    let notice = mode == Mode::Idle && !should_auto_hide(title);
    let transcript = mode == Mode::Listening && !display_text(title).trim().is_empty();
    let (width, height) = if notice {
        (440, 52)
    } else if transcript {
        (440, 80)
    } else {
        match mode {
            Mode::Idle => (32, 6),
            Mode::Listening => (PILL_W, PILL_H),
            Mode::Transcribing => (48, 20),
        }
    };
    Layout {
        width,
        height,
        transcript,
        notice,
    }
}

fn notification_layout(mode: Mode, title: &str, muted: bool, notice_active: bool) -> Layout {
    let mut geometry = layout(mode, title);
    if mode == Mode::Idle && !muted && notice_active {
        geometry = Layout {
            width: 440,
            height: 52,
            transcript: false,
            notice: true,
        };
    }
    geometry
}

fn temporary_notice(title: &str) -> bool {
    let text = title.to_ascii_lowercase();
    [
        "failed",
        "error",
        "heard nothing",
        "saved",
        "recovered",
        "clipboard",
    ]
    .iter()
    .any(|word| text.contains(word))
}

fn displayed_mode(released: bool, incoming: Mode) -> Mode {
    if released && incoming == Mode::Listening {
        Mode::Transcribing
    } else {
        incoming
    }
}

/// The visible handle grows into the capsule without animating transcript text.
fn expanding_layout(elapsed: Duration) -> Layout {
    let t = (elapsed.as_secs_f32() / EXPAND_TIME.as_secs_f32()).min(1.0);
    // Willow's cubic-bezier(.2, 0, 0, 1); a few bisections suffice at 30 fps.
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..10 {
        let u = (low + high) / 2.0;
        let x = 0.6 * u * (1.0 - u).powi(2) + u.powi(3);
        if x < t {
            low = u;
        } else {
            high = u;
        }
    }
    let u = (low + high) / 2.0;
    let eased = u * u * (3.0 - 2.0 * u);
    Layout {
        width: (32.0 + 68.0 * eased).round() as u32,
        height: (6.0 + 30.0 * eased).round() as u32,
        transcript: false,
        notice: false,
    }
}

fn bottom_position(work: (i32, i32, i32, i32), size: (u32, u32), scale: f64) -> (i32, i32) {
    let (left, top, right, bottom) = work;
    (
        left + (right - left - size.0 as i32).max(0) / 2,
        (bottom - size.1 as i32 - (8.0 * scale).round() as i32).max(top),
    )
}

pub fn spinner_angle(t: f64) -> f64 {
    (t * std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI)
}

/// Eleven bars driven by microphone energy. Silence never fabricates speech.
fn waveform_height(level: f32, index: usize) -> f32 {
    let level = if level.is_finite() {
        level.max(0.0)
    } else {
        0.0
    };
    let amplitude = (level / 4000.0).clamp(0.0, 1.0).sqrt();
    let edge = ((index as f32 - 5.0) / 5.0).abs().min(1.0);
    2.52 + 15.48 * amplitude * (1.0 - 0.55 * edge)
}

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
        preferences_rx,
    } = services;
    let restored = crate::config::load();
    let mut preferences = restored.preferences;
    let mut mode = Mode::Idle;
    let mut title = initial_title;
    let mut geometry = layout(mode, &title);
    let event_loop = EventLoop::new().map_err(|e| e.to_string())?;
    let mut attrs = Window::default_attributes()
        .with_title(title.clone())
        .with_inner_size(LogicalSize::new(geometry.width, geometry.height))
        .with_decorations(false)
        .with_transparent(false)
        .with_window_level(WindowLevel::AlwaysOnTop)
        .with_resizable(false)
        .with_visible(false);
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::WindowAttributesExtWindows;
        attrs = attrs.with_skip_taskbar(true);
    }
    let window: Arc<Window> = Arc::new({
        #[allow(deprecated)]
        event_loop.create_window(attrs).map_err(|e| e.to_string())?
    });
    apply_window_chrome(&window);
    if !preferences.automatic_positioning {
        if let (Some(x), Some(y)) = (restored.pill_x, restored.pill_y) {
            window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
        }
    }
    place_window(
        &window,
        geometry,
        preferences.automatic_positioning || restored.pill_x.is_none() || restored.pill_y.is_none(),
    );
    let context = Context::new(window.clone()).map_err(|e| e.to_string())?;
    let mut surface = Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;
    let mut hidden = !preferences.show_idle_bar && !geometry.notice;
    let mut dirty = true;
    let mut level = 0.0_f32;
    let mut smooth = 0.0_f32;
    let mut app_icon: Option<Vec<u8>> = None;
    let mut cursor = None;
    let mut hover = false;
    let mut hotkey = restored.hotkey;
    let start = Instant::now();
    let mut last_frame = Instant::now() - FRAME_TIME;
    let mut reduced_motion = !animations_enabled();
    let mut released = false;
    let mut expand_started: Option<Instant> = None;
    let mut pending_pos: Option<(i32, i32)> = None;
    let mut last_move = Instant::now();
    let mut notice_until: Option<Instant> = None;
    let mut checked_work = active_work_area(&window);
    let mut last_monitor_check = Instant::now();
    #[allow(deprecated)]
    event_loop
        .run(move |event, elwt| {
            let mut tray_dirty = false;
            if let Some(manager) = hotkey_manager.as_ref() {
                while let Some(key_event) = manager.try_event() {
                    match key_event {
                        crate::hotkey::KeyEvent::Pressed => {
                            released = false;
                            notice_until = None;
                            app_icon = crate::output::focused_app_icon();
                            smooth = 0.0;
                            mode = Mode::Listening;
                            title = "Utterly ●".into();
                            reduced_motion = !animations_enabled();
                            hidden = false;
                            expand_started = (!reduced_motion
                                && geometry == layout(Mode::Idle, "Utterly — ready"))
                            .then(Instant::now);
                        }
                        crate::hotkey::KeyEvent::Released => {
                            released = true;
                            mode = Mode::Transcribing;
                            expand_started = None;
                        }
                    }
                    if expand_started.is_none() {
                        geometry = layout(mode, &title);
                    }
                    place_window(&window, geometry, preferences.automatic_positioning);
                    last_frame = Instant::now() - FRAME_TIME;
                    dirty = true;
                    tray_dirty = true;
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
            while let Ok(next) = preferences_rx.try_recv() {
                preferences = next;
                geometry = notification_layout(
                    mode,
                    &title,
                    preferences.mute_notifications,
                    notice_until.is_some(),
                );
                hidden = mode == Mode::Idle && !preferences.show_idle_bar && !geometry.notice;
                if hidden {
                    hide_window(&window);
                }
                place_window(&window, geometry, preferences.automatic_positioning);
                dirty = true;
            }
            while let Ok(update) = rx.try_recv() {
                // The session can still be connecting when the user releases. Never
                // reopen its recording UI in response to a stale Listening update.
                let next_mode = displayed_mode(released, update.mode);
                let changed = mode != next_mode;
                if changed && next_mode == Mode::Listening {
                    notice_until = None;
                    app_icon = crate::output::focused_app_icon();
                    smooth = 0.0;
                    reduced_motion = !animations_enabled();
                    expand_started = (!reduced_motion
                        && geometry == layout(Mode::Idle, "Utterly — ready"))
                    .then(Instant::now);
                } else if changed {
                    expand_started = None;
                }
                mode = next_mode;
                level = update.level;
                if title != update.title || changed {
                    title = update.title;
                    let active_mode = if update.verbatim_live {
                        "Live"
                    } else {
                        "Smart"
                    };
                    window.set_title(&format!("{title} — {active_mode}"));
                    notice_until = if mode == Mode::Idle && temporary_notice(&title) {
                        Some(Instant::now() + Duration::from_secs(3))
                    } else {
                        None
                    };
                    tray_dirty = true;
                }
                let next = notification_layout(
                    mode,
                    &title,
                    preferences.mute_notifications,
                    notice_until.is_some(),
                );
                if expand_started.is_none()
                    && (geometry != next || (changed && mode == Mode::Listening))
                {
                    geometry = next;
                    place_window(&window, geometry, preferences.automatic_positioning);
                }
                if changed {
                    last_frame = Instant::now() - FRAME_TIME;
                }
                hidden = mode == Mode::Idle && !preferences.show_idle_bar && !geometry.notice;
                if hidden {
                    hide_window(&window);
                }
                dirty = true;
            }
            while let Ok((mic, hk, current_mode)) = sync_rx.try_recv() {
                menu.sync(&mic, &hk, &current_mode);
                hotkey = hk;
            }
            if tray_dirty {
                crate::tray::set_mode(
                    &mut tray,
                    mode,
                    &title.chars().take(60).collect::<String>(),
                    &hotkey,
                );
            }
            while let Ok(ev) = tray_icon::tray_event_receiver().try_recv() {
                if matches!(
                    ev.event,
                    tray_icon::ClickEvent::Left | tray_icon::ClickEvent::Double
                ) {
                    hidden = false;
                    dirty = true;
                }
            }
            #[cfg(target_os = "linux")]
            if gtk_pump {
                while gtk::events_pending() {
                    gtk::main_iteration();
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = gtk_pump;
            match event {
                Event::AboutToWait => {
                    if let Some(since) = expand_started {
                        let elapsed = since.elapsed();
                        let next = if elapsed >= EXPAND_TIME {
                            expand_started = None;
                            notification_layout(
                                mode,
                                &title,
                                preferences.mute_notifications,
                                notice_until.is_some(),
                            )
                        } else {
                            expanding_layout(elapsed)
                        };
                        if geometry != next {
                            geometry = next;
                            place_window(&window, geometry, preferences.automatic_positioning);
                            dirty = true;
                        }
                    }
                    if notice_until.is_some_and(|until| Instant::now() >= until) {
                        notice_until = None;
                        geometry = layout(mode, &title);
                        hidden =
                            mode == Mode::Idle && !preferences.show_idle_bar && !geometry.notice;
                        if hidden {
                            hide_window(&window);
                        }
                        place_window(&window, geometry, preferences.automatic_positioning);
                        last_frame = Instant::now() - FRAME_TIME;
                        dirty = true;
                    }
                    if preferences.automatic_positioning
                        && last_monitor_check.elapsed() >= Duration::from_secs(1)
                    {
                        let work = active_work_area(&window);
                        if work != checked_work {
                            checked_work = work;
                            place_window(&window, geometry, true);
                            dirty = true;
                        }
                        last_monitor_check = Instant::now();
                    }
                    if !hidden
                        && (mode == Mode::Listening
                            || (mode == Mode::Transcribing && !reduced_motion))
                        && last_frame.elapsed() >= FRAME_TIME
                    {
                        dirty = true;
                    }
                    if let Some(pos) = pending_pos {
                        if last_move.elapsed() >= Duration::from_millis(500) {
                            let _ = ui_cmd_tx.send(UiCmd::Position(pos.0, pos.1));
                            pending_pos = None;
                        }
                    }
                    if dirty && last_frame.elapsed() >= FRAME_TIME {
                        smooth = if level > smooth {
                            level
                        } else {
                            smooth + (level - smooth) * 0.25
                        };
                        let size = window.inner_size();
                        if !hidden {
                            let result = draw(
                                &mut surface,
                                size.width,
                                size.height,
                                if expand_started.is_some() {
                                    Mode::Idle
                                } else {
                                    mode
                                },
                                smooth,
                                if reduced_motion {
                                    0.0
                                } else {
                                    start.elapsed().as_secs_f64()
                                },
                                hover,
                                &title,
                                window.scale_factor() as f32,
                                geometry,
                                if preferences.hide_app_icon {
                                    None
                                } else {
                                    app_icon.as_deref()
                                },
                            );
                            if let Err(error) = result {
                                eprintln!("[utterly] pill draw: {error}");
                            }
                            if window.is_visible() != Some(true) {
                                show_window_no_activate(&window);
                            }
                        }
                        dirty = false;
                        last_frame = Instant::now();
                    }
                    let active = expand_started.is_some()
                        || mode == Mode::Listening
                        || (mode == Mode::Transcribing && !reduced_motion);
                    elwt.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                        Instant::now()
                            + if active {
                                FRAME_TIME
                            } else {
                                Duration::from_millis(100)
                            },
                    ));
                }
                Event::WindowEvent { window_id, event } if window_id == window.id() => {
                    match event {
                        WindowEvent::RedrawRequested => dirty = true,
                        WindowEvent::Resized(_) => {
                            set_window_region(&window, geometry);
                            dirty = true;
                        }
                        WindowEvent::ScaleFactorChanged { .. } => {
                            place_window(&window, geometry, preferences.automatic_positioning);
                            dirty = true;
                        }
                        WindowEvent::CloseRequested => {
                            hidden = true;
                            hide_window(&window);
                            let _ = ui_cmd_tx.send(UiCmd::Hide);
                        }
                        WindowEvent::CursorMoved { position, .. } => {
                            cursor = Some(position.to_logical::<f32>(window.scale_factor()));
                            if !hover {
                                hover = true;
                                dirty = true;
                            }
                        }
                        WindowEvent::CursorLeft { .. } => {
                            cursor = None;
                            hover = false;
                            dirty = true;
                        }
                        WindowEvent::Moved(pos) if !preferences.automatic_positioning => {
                            pending_pos = Some((pos.x, pos.y));
                            last_move = Instant::now();
                        }
                        WindowEvent::MouseInput {
                            state: winit::event::ElementState::Pressed,
                            button,
                            ..
                        } => {
                            if button == winit::event::MouseButton::Right {
                                hidden = true;
                                hide_window(&window);
                                let _ = ui_cmd_tx.send(UiCmd::Hide);
                            } else if button == winit::event::MouseButton::Left {
                                let drag = mode == Mode::Idle
                                    || cursor.is_some_and(|p| geometry.transcript && p.y < 36.0);
                                if drag && !preferences.automatic_positioning {
                                    let _ = window.drag_window();
                                } else {
                                    if mode == Mode::Listening {
                                        released = true;
                                        expand_started = None;
                                        mode = Mode::Transcribing;
                                        geometry = layout(mode, &title);
                                        place_window(
                                            &window,
                                            geometry,
                                            preferences.automatic_positioning,
                                        );
                                        last_frame = Instant::now() - FRAME_TIME;
                                        dirty = true;
                                    } else {
                                        released = false;
                                    }
                                    let _ = ui_cmd_tx.send(UiCmd::MicToggle);
                                }
                            }
                        }
                        WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                            use winit::keyboard::{Key, NamedKey};
                            match event.logical_key {
                                Key::Named(NamedKey::Escape) => {
                                    hidden = true;
                                    hide_window(&window);
                                    let _ = ui_cmd_tx.send(UiCmd::Hide);
                                }
                                Key::Named(NamedKey::Enter | NamedKey::Space) => {
                                    if mode == Mode::Listening {
                                        released = true;
                                        expand_started = None;
                                        mode = Mode::Transcribing;
                                        geometry = layout(mode, &title);
                                        place_window(
                                            &window,
                                            geometry,
                                            preferences.automatic_positioning,
                                        );
                                        last_frame = Instant::now() - FRAME_TIME;
                                        dirty = true;
                                    } else {
                                        released = false;
                                    }
                                    let _ = ui_cmd_tx.send(UiCmd::MicToggle);
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        })
        .map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn draw<D, W>(
    surface: &mut Surface<D, W>,
    width: u32,
    height: u32,
    mode: Mode,
    level: f32,
    t: f64,
    hover: bool,
    title: &str,
    scale: f32,
    geometry: Layout,
    app_icon: Option<&[u8]>,
) -> Result<(), String>
where
    D: raw_window_handle::HasDisplayHandle,
    W: raw_window_handle::HasWindowHandle,
{
    if width == 0 || height == 0 {
        return Ok(());
    }
    surface
        .resize(
            std::num::NonZeroU32::new(width).unwrap(),
            std::num::NonZeroU32::new(height).unwrap(),
        )
        .map_err(|e| e.to_string())?;
    let mut buf = surface.buffer_mut().map_err(|e| e.to_string())?;
    paint(
        &mut buf,
        width as usize,
        height as usize,
        mode,
        level,
        t,
        hover,
        title,
        scale,
        geometry,
        app_icon,
    );
    buf.present().map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn paint(
    buf: &mut [u32],
    w: usize,
    h: usize,
    mode: Mode,
    level: f32,
    t: f64,
    hover: bool,
    title: &str,
    scale: f32,
    geometry: Layout,
    app_icon: Option<&[u8]>,
) {
    let bg = if hover { 0x252527 } else { 0x1e1e20 };
    buf.fill(bg);
    let sx = w as f32 / geometry.width as f32;
    let sy = h as f32 / geometry.height as f32;
    let pill_x = if geometry.transcript {
        (geometry.width - PILL_W) as f32 / 2.0
    } else {
        0.0
    };
    let pill_y = if geometry.transcript { 44.0 } else { 0.0 };
    let icon = app_icon.filter(|icon| icon.len() == 32 * 32 * 4);
    for y in 0..h {
        let ly = (y as f32 + 0.5) / sy;
        for x in 0..w {
            let lx = (x as f32 + 0.5) / sx;
            let px = &mut buf[y * w + x];
            if mode == Mode::Idle && !geometry.notice && geometry.width == 32 {
                *px = if hover { 0x65656c } else { 0x36363b };
                continue;
            }
            if geometry.notice || (geometry.transcript && ly < 36.0) {
                *px = if ly < 1.0 || lx < 1.0 || lx > geometry.width as f32 - 1.0 {
                    0x48484c
                } else {
                    bg
                };
                continue;
            }
            let local_x = lx - pill_x;
            let local_y = ly - pill_y;
            let (pw, ph) = match mode {
                Mode::Idle => (geometry.width as f32, geometry.height as f32),
                Mode::Listening => (PILL_W as f32, PILL_H as f32),
                Mode::Transcribing => (48.0, 20.0),
            };
            let radius = ph / 2.0;
            let center_x = local_x.clamp(radius, pw - radius);
            let distance = ((local_x - center_x).powi(2) + (local_y - radius).powi(2)).sqrt();
            if distance >= radius - 1.0 {
                *px = 0x48484c;
            }
            if mode == Mode::Transcribing {
                let dx = local_x - 24.0;
                let dy = local_y - 10.0;
                let r = (dx * dx + dy * dy).sqrt();
                if (5.5..7.0).contains(&r) {
                    let phase =
                        (dy.atan2(dx) as f64 + spinner_angle(t)).rem_euclid(std::f64::consts::TAU);
                    let grey = (75.0 + 170.0 * phase / std::f64::consts::TAU) as u32;
                    *px = (grey << 16) | (grey << 8) | grey;
                }
            } else if mode == Mode::Listening {
                let wave_left = if icon.is_some() { 40.0 } else { 30.0 };
                let index = ((local_x - wave_left) / 4.0).round() as i32;
                if (0..11).contains(&index) {
                    let cx = wave_left + index as f32 * 4.0;
                    let height = waveform_height(level, index as usize);
                    let dy = (local_y - 18.0).abs();
                    let dx = (local_x - cx).abs();
                    if dx * dx + (dy - (height / 2.0 - 1.0)).max(0.0).powi(2) <= 1.0 {
                        *px = 0xf4f4f5;
                    }
                }
                if let Some(icon) = icon {
                    if (12.0..30.0).contains(&local_x) && (9.0..27.0).contains(&local_y) {
                        let ix = ((local_x - 12.0) * 32.0 / 18.0) as usize;
                        let iy = ((local_y - 9.0) * 32.0 / 18.0) as usize;
                        let offset = (iy * 32 + ix) * 4;
                        let alpha = icon[offset + 3] as u32;
                        let channel = |shift: u32, channel: usize| {
                            (((bg >> shift) & 255_u32) * (255 - alpha)
                                + icon[offset + channel] as u32 * alpha)
                                / 255
                        };
                        *px = (channel(16, 0) << 16) | (channel(8, 1) << 8) | channel(0, 2);
                    }
                }
            }
        }
    }
    if geometry.transcript || geometry.notice {
        let text = format_live_text(display_text(title), 66);
        let dx = (16.0 * sx) as usize;
        let dy = (if geometry.notice { 8.0 } else { 2.0 } * sy) as usize;
        render_text(
            buf,
            w,
            dx,
            dy,
            w.saturating_sub(dx * 2),
            (32.0 * sy) as usize,
            &text,
            scale,
            if geometry.notice { 0xffcc8c } else { 0xf2f2f4 },
            bg,
        );
    }
}

fn place_window(window: &Window, geometry: Layout, automatic: bool) {
    let scale = window.scale_factor();
    let old_size = window.outer_size();
    let size = (
        (geometry.width as f64 * scale).round() as u32,
        (geometry.height as f64 * scale).round() as u32,
    );
    let work = active_work_area(window);
    let position = if automatic {
        bottom_position(work, size, scale)
    } else if let Ok(pos) = window.outer_position() {
        let x = pos.x + (old_size.width as i32 - size.0 as i32) / 2;
        let y = pos.y + old_size.height as i32 - size.1 as i32;
        (
            x.clamp(work.0, (work.2 - size.0 as i32).max(work.0)),
            y.clamp(work.1, (work.3 - size.1 as i32).max(work.1)),
        )
    } else {
        bottom_position(work, size, scale)
    };
    let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(size.0, size.1));
    window.set_outer_position(winit::dpi::PhysicalPosition::new(position.0, position.1));
    set_window_region(window, geometry);
}

fn active_work_area(window: &Window) -> (i32, i32, i32, i32) {
    #[cfg(target_os = "windows")]
    unsafe {
        #[repr(C)]
        struct MonitorInfo {
            size: u32,
            monitor: RECT,
            work: RECT,
            flags: u32,
        }
        let monitor = MonitorFromWindow(GetForegroundWindow(), 2);
        let mut info: MonitorInfo = std::mem::zeroed();
        info.size = std::mem::size_of::<MonitorInfo>() as u32;
        if GetMonitorInfoW(monitor, &mut info as *mut _ as *mut std::ffi::c_void) != 0 {
            return (
                info.work.left,
                info.work.top,
                info.work.right,
                info.work.bottom,
            );
        }
    }
    if let Some(m) = window
        .current_monitor()
        .or_else(|| window.primary_monitor())
    {
        let p = m.position();
        let s = m.size();
        (p.x, p.y, p.x + s.width as i32, p.y + s.height as i32)
    } else {
        (0, 0, 1920, 1040)
    }
}

fn animations_enabled() -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        let mut enabled = 1_i32;
        let _ = SystemParametersInfoW(
            0x1042,
            0,
            &mut enabled as *mut _ as *mut std::ffi::c_void,
            0,
        );
        enabled != 0
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
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
                let hbmp =
                    CreateDIBSection(cache.hdc, &bmi, 0, &mut p_bits, std::ptr::null_mut(), 0);
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
                let font_h = -(15.0 * scale).round() as i32;
                let hfont = CreateFontW(
                    font_h,
                    0,
                    0,
                    0,
                    400, // FW_NORMAL
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
fn set_window_region(window: &Window, geometry: Layout) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let size = window.inner_size();
    let scale = window.scale_factor();
    let px = |value: u32| (value as f64 * scale).round() as i32;
    unsafe {
        let diameter = if geometry.notice {
            px(20)
        } else if geometry.transcript {
            px(16)
        } else {
            size.height as i32
        };
        let region = CreateRoundRectRgn(
            0,
            0,
            size.width as i32 + 1,
            if geometry.transcript {
                px(36) + 1
            } else {
                size.height as i32 + 1
            },
            diameter,
            diameter,
        );
        if region.is_null() {
            return;
        }
        if geometry.transcript {
            let left = px((geometry.width - PILL_W) / 2);
            let capsule = CreateRoundRectRgn(
                left,
                px(44),
                left + px(PILL_W) + 1,
                size.height as i32 + 1,
                px(PILL_H),
                px(PILL_H),
            );
            if !capsule.is_null() {
                CombineRgn(region, region, capsule, 2);
                DeleteObject(capsule);
            }
        }
        if SetWindowRgn(handle.hwnd.get() as *mut std::ffi::c_void, region, 1) == 0 {
            DeleteObject(region);
        }
    }
}

#[cfg(target_os = "windows")]
pub fn apply_window_chrome(window: &Window) {
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
        const WS_EX_APPWINDOW: isize = 0x0004_0000;
        const WS_EX_NOACTIVATE: isize = 0x0800_0000;
        const SWP_NOMOVE: u32 = 0x0002;
        const SWP_NOSIZE: u32 = 0x0001;
        const SWP_NOZORDER: u32 = 0x0004;
        const SWP_FRAMECHANGED: u32 = 0x0020;
        const SWP_NOACTIVATE: u32 = 0x0010;
        let hwnd = handle.hwnd.get() as *mut c_void;
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let target = (ex & !WS_EX_APPWINDOW) | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, target);
        let _ = SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE,
        );
    }
}

pub fn show_window_no_activate(window: &Window) {
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use std::ffi::c_void;
        if let Ok(handle) = window.window_handle() {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                const SW_SHOWNOACTIVATE: i32 = 4;
                unsafe {
                    ShowWindow(handle.hwnd.get() as *mut c_void, SW_SHOWNOACTIVATE);
                }
            }
        }
        apply_window_chrome(window);
    }
    #[cfg(not(target_os = "windows"))]
    {
        window.set_visible(true);
    }
}

pub fn hide_window(window: &Window) {
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use std::ffi::c_void;
        if let Ok(handle) = window.window_handle() {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                const SW_HIDE: i32 = 0;
                unsafe {
                    ShowWindow(handle.hwnd.get() as *mut c_void, SW_HIDE);
                }
            }
        }
    }
    window.set_visible(false);
}

#[cfg(not(target_os = "windows"))]
pub fn apply_window_chrome(_window: &Window) {}

#[cfg(target_os = "windows")]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetForegroundWindow() -> *mut std::ffi::c_void;
    fn MonitorFromWindow(window: *mut std::ffi::c_void, flags: u32) -> *mut std::ffi::c_void;
    fn GetMonitorInfoW(monitor: *mut std::ffi::c_void, info: *mut std::ffi::c_void) -> i32;
    fn SystemParametersInfoW(
        action: u32,
        param: u32,
        value: *mut std::ffi::c_void,
        flags: u32,
    ) -> i32;
    fn ShowWindow(window: *mut std::ffi::c_void, cmd: i32) -> i32;
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
fn set_window_region(_window: &Window, _geometry: Layout) {}

#[cfg(target_os = "windows")]
#[link(name = "gdi32")]
unsafe extern "system" {
    fn CombineRgn(
        dest: *mut std::ffi::c_void,
        a: *mut std::ffi::c_void,
        b: *mut std::ffi::c_void,
        mode: i32,
    ) -> i32;
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
    fn release_collapses_even_if_network_sends_stale_listening() {
        assert_eq!(displayed_mode(true, Mode::Listening), Mode::Transcribing);
        assert_eq!(displayed_mode(false, Mode::Listening), Mode::Listening);
        assert_eq!(displayed_mode(true, Mode::Idle), Mode::Idle);
        let live = layout(Mode::Listening, "Utterly ● words appear while speaking");
        let processing = layout(Mode::Transcribing, "Utterly ● words appear while speaking");
        assert!(live.transcript);
        assert!(!processing.transcript);
        assert!(processing.height < PILL_H && processing.width < PILL_W);
    }

    #[test]
    fn expansion_reaches_capsule_in_300ms() {
        assert_eq!(
            expanding_layout(Duration::ZERO),
            layout(Mode::Idle, "Utterly — ready")
        );
        let halfway = expanding_layout(Duration::from_millis(150));
        assert!(halfway.width > 80 && halfway.width < PILL_W);
        assert_eq!(
            expanding_layout(EXPAND_TIME),
            layout(Mode::Listening, "Utterly ●")
        );
    }

    #[test]
    fn position_respects_negative_monitor_and_taskbar_work_area() {
        let work = (-1920, -200, 0, 840);
        assert_eq!(bottom_position(work, (100, 36), 1.0), (-1010, 796));
        assert_eq!(bottom_position(work, (150, 54), 1.5), (-1035, 774));
        assert_eq!(bottom_position((0, 0, 80, 30), (100, 36), 1.0), (0, 0));
    }

    #[test]
    fn muted_notices_keep_actionable_setup_visible() {
        assert!(!notification_layout(Mode::Idle, "Utterly — saved", true, true).notice);
        assert!(notification_layout(Mode::Idle, "Utterly — saved", false, true).notice);
        assert!(!notification_layout(Mode::Idle, "Utterly — saved", false, false).notice);
        for title in [
            "Utterly — no API key: copy one",
            "Utterly — mic error: not found",
            "Utterly — hotkey error: occupied",
        ] {
            assert!(notification_layout(Mode::Idle, title, true, false).notice);
        }
    }

    #[test]
    fn wave_is_bounded_and_silence_is_stable() {
        for index in 0..11 {
            assert_eq!(waveform_height(0.0, index), 2.52);
            assert_eq!(waveform_height(f32::NAN, index), 2.52);
            assert_eq!(waveform_height(-100.0, index), 2.52);
            assert!((2.52..=18.01).contains(&waveform_height(32768.0, index)));
        }
        assert!(waveform_height(1000.0, 5) > waveform_height(1000.0, 0));
        assert!(waveform_height(2000.0, 5) > waveform_height(1000.0, 5));
    }

    #[test]
    fn paints_audio_changes_and_accepts_malformed_icon_safely() {
        let geometry = layout(Mode::Listening, "Utterly ●");
        let mut quiet = vec![0; 100 * 36];
        let mut speech = quiet.clone();
        paint(
            &mut quiet,
            100,
            36,
            Mode::Listening,
            0.0,
            0.0,
            false,
            "Utterly ●",
            1.0,
            geometry,
            Some(&[255]),
        );
        paint(
            &mut speech,
            100,
            36,
            Mode::Listening,
            2400.0,
            0.0,
            false,
            "Utterly ●",
            1.0,
            geometry,
            None,
        );
        assert_ne!(quiet, speech);
        assert!(
            speech.iter().filter(|&&px| px == 0xf4f4f5).count()
                > quiet.iter().filter(|&&px| px == 0xf4f4f5).count()
        );
    }

    #[test]
    fn unicode_transcript_and_small_limits_never_panic() {
        assert_eq!(format_live_text("a", 0), "");
        assert_eq!(format_live_text("🙂", 1), "🙂");
        assert!(format_live_text("नमस्ते दुनिया hello", 8).ends_with("hello"));
    }

    #[test]
    #[ignore = "Writes an opt-in render preview to target/pill-preview.ppm"]
    fn render_preview() {
        use std::io::Write;
        let (width, height) = (920, 460);
        let mut canvas = vec![0x101013_u32; width * height];
        let states = [
            (Mode::Idle, "Utterly — ready", 0.0),
            (Mode::Listening, "Utterly ●", 1600.0),
            (
                Mode::Listening,
                "Utterly ● Live words appear here while you speak.",
                2800.0,
            ),
            (Mode::Transcribing, "Utterly … transcribing", 0.0),
        ];
        let icon = include_bytes!("../assets/utterly-tray-32.rgba");
        let mut top = 20;
        for (mode, title, level) in states {
            let geometry = layout(mode, title);
            let w = geometry.width as usize * 2;
            let h = geometry.height as usize * 2;
            let mut buf = vec![0; w * h];
            paint(
                &mut buf,
                w,
                h,
                mode,
                level,
                0.8,
                false,
                title,
                2.0,
                geometry,
                Some(icon),
            );
            let left = (width - w) / 2;
            for y in 0..h {
                for x in 0..w {
                    let lx = x as f32 / 2.0;
                    let ly = y as f32 / 2.0;
                    let (rx, ry, rw, rh, radius) = if geometry.transcript && ly >= 36.0 {
                        (
                            (geometry.width - PILL_W) as f32 / 2.0,
                            44.0,
                            PILL_W as f32,
                            PILL_H as f32,
                            18.0,
                        )
                    } else {
                        (
                            0.0,
                            0.0,
                            geometry.width as f32,
                            if geometry.transcript {
                                36.0
                            } else {
                                geometry.height as f32
                            },
                            if geometry.transcript {
                                8.0
                            } else {
                                geometry.height as f32 / 2.0
                            },
                        )
                    };
                    let dx = lx - lx.clamp(rx + radius, rx + rw - radius);
                    let dy = ly - ly.clamp(ry + radius, ry + rh - radius);
                    if lx >= rx
                        && lx < rx + rw
                        && ly >= ry
                        && ly < ry + rh
                        && dx * dx + dy * dy <= radius * radius
                    {
                        canvas[(top + y) * width + left + x] = buf[y * w + x];
                    }
                }
            }
            top += h + 26;
        }
        let mut file = std::fs::File::create("target/pill-preview.ppm").unwrap();
        write!(file, "P6\n{width} {height}\n255\n").unwrap();
        for pixel in canvas {
            file.write_all(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8])
                .unwrap();
        }
    }

    #[test]
    fn auto_hide_keeps_errors_visible() {
        assert!(should_auto_hide("Utterly — hello world"));
        assert!(should_auto_hide("Utterly — hold Alt+Space to dictate"));
        assert!(should_auto_hide("Utterly — connect failed: timeout"));
        assert!(should_auto_hide("Utterly — heard nothing, try again"));
        assert!(should_auto_hide("Utterly — saved"));
        assert!(should_auto_hide(
            "Utterly — mic stream recovered, hold Alt+Space to dictate"
        ));
        assert!(should_auto_hide(
            "Utterly — paste failed; transcript is on clipboard: hello"
        ));
        assert!(!should_auto_hide("Utterly — no API key: copy one"));
        assert!(!should_auto_hide("Utterly — mic error: not found"));
        assert!(!should_auto_hide("Utterly — mic error: no input device"));
    }

    #[test]
    fn spinner_advances_and_wraps() {
        let pi = std::f64::consts::PI;
        assert!(spinner_angle(0.0).abs() < 1e-9);
        assert!((spinner_angle(0.5) - pi / 2.0).abs() < 1e-9);
        assert!((spinner_angle(1.0) - pi).abs() < 1e-9);
        assert!(spinner_angle(2.0).abs() < 1e-9, "full turn wraps to 0");
        assert!(spinner_angle(4.0).abs() < 1e-9);
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
        assert_eq!(
            buf, buf2,
            "Cached render must match initial render bit-for-bit"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_window_chrome_flags() {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use winit::event_loop::EventLoop;
        use winit::platform::windows::EventLoopBuilderExtWindows;
        use winit::window::Window;

        let mut builder = EventLoop::builder();
        builder.with_any_thread(true);
        let el = builder.build().unwrap();
        let attrs = Window::default_attributes().with_visible(false);
        #[allow(deprecated)]
        let window = el.create_window(attrs).unwrap();
        apply_window_chrome(&window);

        let handle = window.window_handle().unwrap();
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            panic!("not win32");
        };
        unsafe {
            let ex = GetWindowLongPtrW(handle.hwnd.get() as *mut std::ffi::c_void, -20);
            const WS_EX_TOOLWINDOW: isize = 0x0000_0080;
            const WS_EX_APPWINDOW: isize = 0x0004_0000;
            const WS_EX_NOACTIVATE: isize = 0x0800_0000;

            assert_ne!(ex & WS_EX_TOOLWINDOW, 0, "WS_EX_TOOLWINDOW must be set");
            assert_ne!(ex & WS_EX_NOACTIVATE, 0, "WS_EX_NOACTIVATE must be set");
            assert_eq!(ex & WS_EX_APPWINDOW, 0, "WS_EX_APPWINDOW must be cleared");
        }
    }
}
