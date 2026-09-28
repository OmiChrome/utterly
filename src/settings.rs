//! Dark, native Windows settings. Owner-drawn buttons retain native focus and keyboard behavior.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub mode: String,
    pub hotkey: String,
    pub vocabulary: Vec<String>,
    pub preferences: crate::config::Preferences,
    pub mic: String,
    pub has_api_key: bool,
}

#[cfg(any(target_os = "windows", test))]
fn preference_value(p: &crate::config::Preferences, index: usize) -> bool {
    match index {
        0 => p.interaction_sounds,
        1 => p.duck_audio,
        2 => p.mute_notifications,
        3 => p.context_awareness,
        4 => p.auto_dictionary,
        5 => p.smart_insertion,
        6 => p.automatic_positioning,
        7 => p.show_idle_bar,
        8 => p.hide_app_icon,
        _ => false,
    }
}
#[cfg(any(target_os = "windows", test))]
fn toggle_preference(p: &mut crate::config::Preferences, index: usize) {
    let value = match index {
        0 => &mut p.interaction_sounds,
        1 => &mut p.duck_audio,
        2 => &mut p.mute_notifications,
        3 => &mut p.context_awareness,
        4 => &mut p.auto_dictionary,
        5 => &mut p.smart_insertion,
        6 => &mut p.automatic_positioning,
        7 => &mut p.show_idle_bar,
        8 => &mut p.hide_app_icon,
        _ => return,
    };
    *value = !*value;
}

#[cfg(target_os = "windows")]
mod windows {
    use super::{preference_value, toggle_preference, Snapshot};
    use crate::{config, hotkey, tray::MenuCmd};
    use std::{
        ffi::c_void,
        sync::{
            atomic::{AtomicIsize, Ordering},
            mpsc::{self, Sender},
            Arc, Mutex,
        },
        thread,
        time::Duration,
    };
    type Hwnd = *mut c_void;
    type WndProc = unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;
    const BG: u32 = 0x00191817;
    const SIDEBAR: u32 = 0x00211F1D;
    const CARD: u32 = 0x002B2825;
    const BORDER: u32 = 0x00443F3A;
    const INK: u32 = 0x00F7F5F3;
    const MUTED: u32 = 0x00ADA7A1;
    const VIOLET: u32 = 0x00D97C86;
    const ID_MODE: i32 = 101;
    const ID_HOTKEY: i32 = 103;
    const ID_WORD: i32 = 104;
    const ID_ADD: i32 = 105;
    const ID_WORDS: i32 = 106;
    const ID_REMOVE: i32 = 107;
    const ID_MIC: i32 = 110;
    const ID_KEY: i32 = 111;
    const ID_PREF: i32 = 200;
    const ID_NAV: i32 = 300;
    const WM_SHOW: u32 = 0x8001;
    const WM_REFRESH: u32 = 0x8002;
    const WM_QUIT: u32 = 0x8003;
    const CB_ADDSTRING: u32 = 0x143;
    const CB_SETCURSEL: u32 = 0x14E;
    const CB_GETCURSEL: u32 = 0x147;
    const LB_ADDSTRING: u32 = 0x180;
    const LB_GETCURSEL: u32 = 0x188;
    const LB_RESETCONTENT: u32 = 0x184;
    const PAGES: [(&str, &str); 4] = [
        ("General", "Make Utterly feel at home."),
        ("Dictionary", "The names and phrases that matter to you."),
        ("Intelligence", "A little context. Better words."),
        ("System", "Small details for a quieter workflow."),
    ];
    const PREFS: [(&str, &str); 9] = [
        (
            "Interaction sounds",
            "Play a soft sound when recording starts and stops.",
        ),
        (
            "Auto duck audio",
            "Lower other audio while recording, then restore it.",
        ),
        ("Mute notifications", "Keep Utterly notifications quiet."),
        (
            "Context awareness",
            "Use limited text from the focused field to help spell names.",
        ),
        (
            "Auto dictionary",
            "Learn candidate names from focused text when context is enabled.",
        ),
        (
            "Smart text insertion",
            "Adjust spacing and capitalization to the surrounding text.",
        ),
        (
            "Automatic overlay positioning",
            "Keep the pill above the taskbar on your active display.",
        ),
        (
            "Show idle bar",
            "Keep the small handle visible when you are not recording.",
        ),
        (
            "Hide focused app icon",
            "Show only the waveform in the recording pill.",
        ),
    ];
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    struct Message {
        hwnd: Hwnd,
        message: u32,
        w_param: usize,
        l_param: isize,
        time: u32,
        point: Point,
        private: u32,
    }
    #[repr(C)]
    struct WindowClass {
        style: u32,
        wnd_proc: Option<WndProc>,
        cls_extra: i32,
        wnd_extra: i32,
        instance: Hwnd,
        icon: Hwnd,
        cursor: Hwnd,
        background: Hwnd,
        menu_name: *const u16,
        class_name: *const u16,
    }
    #[repr(C)]
    struct CreateStruct {
        params: *mut c_void,
        instance: Hwnd,
        menu: Hwnd,
        parent: Hwnd,
        height: i32,
        width: i32,
        y: i32,
        x: i32,
        style: i32,
        name: *const u16,
        class: *const u16,
        ex_style: u32,
    }
    #[repr(C)]
    struct Paint {
        dc: Hwnd,
        erase: i32,
        rect: Rect,
        restore: i32,
        update: i32,
        reserved: [u8; 32],
    }
    #[repr(C)]
    struct DrawItem {
        control_type: u32,
        id: u32,
        item_id: u32,
        action: u32,
        state: u32,
        hwnd: Hwnd,
        dc: Hwnd,
        rect: Rect,
        data: usize,
    }
    #[repr(C)]
    struct ScrollInfo {
        size: u32,
        mask: u32,
        min: i32,
        max: i32,
        page: u32,
        pos: i32,
        track: i32,
    }
    #[repr(C)]
    struct MinMax {
        reserved: Point,
        max_size: Point,
        max_position: Point,
        min_track: Point,
        max_track: Point,
    }
    struct Control {
        hwnd: Hwnd,
        id: i32,
        page: usize,
        y: i32,
        height: i32,
    }
    struct WindowState {
        snapshot: Arc<Mutex<Snapshot>>,
        actions: Sender<MenuCmd>,
        controls: Vec<Control>,
        font: Hwnd,
        small_font: Hwnd,
        title_font: Hwnd,
        icon: Hwnd,
        bg_brush: Hwnd,
        card_brush: Hwnd,
        instance: Hwnd,
        scale: f32,
        page: usize,
        scroll: i32,
        mics: Vec<String>,
        status: String,
    }
    #[derive(Clone)]
    pub struct SettingsWindow {
        hwnd: Arc<AtomicIsize>,
        snapshot: Arc<Mutex<Snapshot>>,
    }
    impl SettingsWindow {
        pub fn spawn(snapshot: Snapshot, actions: Sender<MenuCmd>) -> Result<Self, String> {
            let snapshot = Arc::new(Mutex::new(snapshot));
            let hwnd = Arc::new(AtomicIsize::new(0));
            let (s, h) = (snapshot.clone(), hwnd.clone());
            let (tx, rx) = mpsc::sync_channel(1);
            thread::Builder::new()
                .name("utterly-settings".into())
                .stack_size(512 * 1024)
                .spawn(move || window_thread(s, h, actions, tx))
                .map_err(|e| e.to_string())?;
            rx.recv_timeout(Duration::from_secs(5))
                .map_err(|e| e.to_string())??;
            Ok(Self { hwnd, snapshot })
        }
        pub fn show(&self) {
            self.post(WM_SHOW);
        }
        pub fn update(&self, snapshot: Snapshot) {
            if let Ok(mut s) = self.snapshot.lock() {
                *s = snapshot;
            }
            self.post(WM_REFRESH);
        }
        pub fn close(&self) {
            self.post(WM_QUIT);
        }
        fn post(&self, message: u32) {
            let h = self.hwnd.load(Ordering::Acquire);
            if h != 0 {
                unsafe {
                    PostMessageW(h as Hwnd, message, 0, 0);
                }
            }
        }
    }
    // The thread holds a slot reference; the last public handle owns shutdown.
    impl Drop for SettingsWindow {
        fn drop(&mut self) {
            if Arc::strong_count(&self.hwnd) == 2 {
                self.close();
            }
        }
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    fn scaled(v: i32, s: f32) -> i32 {
        (v as f32 * s).round() as i32
    }
    unsafe fn font(size: i32, weight: i32, s: f32) -> Hwnd {
        CreateFontW(
            -scaled(size, s),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            wide("Segoe UI").as_ptr(),
        )
    }
    unsafe fn app_icon() -> Hwnd {
        let bytes = include_bytes!("../assets/utterly.ico");
        let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        for entry in bytes[6..].as_chunks::<16>().0.iter().take(count) {
            if entry[0] == 32 && entry[1] == 32 {
                let len = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
                let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
                if let Some(data) = bytes.get(offset..offset + len) {
                    return CreateIconFromResourceEx(
                        data.as_ptr(),
                        len as u32,
                        1,
                        0x30000,
                        32,
                        32,
                        0,
                    );
                }
            }
        }
        std::ptr::null_mut()
    }
    fn window_thread(
        snapshot: Arc<Mutex<Snapshot>>,
        slot: Arc<AtomicIsize>,
        actions: Sender<MenuCmd>,
        ready: mpsc::SyncSender<Result<(), String>>,
    ) {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("UtterlyNativeSettings");
            let bg = CreateSolidBrush(BG);
            let card = CreateSolidBrush(CARD);
            let wc = WindowClass {
                style: 3,
                wnd_proc: Some(window_proc),
                cls_extra: 0,
                wnd_extra: 0,
                instance,
                icon: std::ptr::null_mut(),
                cursor: LoadCursorW(std::ptr::null_mut(), 32512usize as *const u16),
                background: std::ptr::null_mut(),
                menu_name: std::ptr::null(),
                class_name: class.as_ptr(),
            };
            // A second settings instance (including tests) may reuse the registered class.
            RegisterClassW(&wc);
            let s = GetDpiForSystem().max(96) as f32 / 96.0;
            let mut mics = vec![String::new()];
            mics.extend(crate::audio::list_mics());
            let mut state = Box::new(WindowState {
                snapshot,
                actions,
                controls: vec![],
                font: font(16, 400, s),
                small_font: font(13, 400, s),
                title_font: font(24, 600, s),
                icon: app_icon(),
                bg_brush: bg,
                card_brush: card,
                instance,
                scale: s,
                page: 0,
                scroll: 0,
                mics,
                status: String::new(),
            });
            let hwnd = CreateWindowExW(
                0x00010000,
                class.as_ptr(),
                wide("Utterly · Settings").as_ptr(),
                0x00CF0000 | 0x02000000 | 0x00200000,
                i32::MIN,
                i32::MIN,
                scaled(1000, s),
                scaled(750, s),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                (&mut *state as *mut WindowState).cast(),
            );
            if hwnd.is_null() {
                let _ = ready.send(Err(format!(
                    "create settings: {}",
                    std::io::Error::last_os_error()
                )));
                return;
            }
            let dark = 1i32;
            SendMessageW(hwnd, 0x80, 0, state.icon as isize);
            SendMessageW(hwnd, 0x80, 1, state.icon as isize);
            DwmSetWindowAttribute(hwnd, 20, (&dark as *const i32).cast(), 4);
            slot.store(hwnd as isize, Ordering::Release);
            let _ = ready.send(Ok(()));
            let mut msg: Message = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if IsDialogMessageW(hwnd, &mut msg) == 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            slot.store(0, Ordering::Release);
            for obj in [state.font, state.small_font, state.title_font, bg, card] {
                DeleteObject(obj);
            }
            if !state.icon.is_null() {
                DestroyIcon(state.icon);
            }
        }
    }
    unsafe extern "system" fn window_proc(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize {
        if msg == 0x81 {
            let c = &*(l as *const CreateStruct);
            SetWindowLongPtrW(hwnd, -21, c.params as isize);
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let ptr = GetWindowLongPtrW(hwnd, -21) as *mut WindowState;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let s = &mut *ptr;
        match msg {
            1 => {
                create_controls(hwnd, s);
                refresh(hwnd, s);
                0
            }
            5 => {
                layout(hwnd, s);
                0
            }
            WM_SHOW => {
                refresh(hwnd, s);
                ShowWindow(hwnd, 9);
                SetForegroundWindow(hwnd);
                0
            }
            WM_REFRESH => {
                refresh(hwnd, s);
                0
            }
            0x10 => {
                ShowWindow(hwnd, 0);
                0
            }
            WM_QUIT => {
                DestroyWindow(hwnd);
                0
            }
            2 => {
                PostQuitMessage(0);
                0
            }
            0x111 => {
                command(hwnd, s, w);
                0
            }
            0x2B => {
                draw_control(s, &*(l as *const DrawItem));
                1
            }
            0xF => {
                let mut p: Paint = std::mem::zeroed();
                let dc = BeginPaint(hwnd, &mut p);
                paint(hwnd, s, dc);
                EndPaint(hwnd, &p);
                0
            }
            0x14 => 1,
            0x133 | 0x134 | 0x138 => {
                SetTextColor(w as Hwnd, INK);
                SetBkColor(w as Hwnd, CARD);
                s.card_brush as isize
            }
            0x115 => {
                let mut info = ScrollInfo {
                    size: std::mem::size_of::<ScrollInfo>() as u32,
                    mask: 0x10,
                    min: 0,
                    max: 0,
                    page: 0,
                    pos: 0,
                    track: 0,
                };
                GetScrollInfo(hwnd, 1, &mut info);
                match w & 0xffff {
                    0 => s.scroll -= 36,
                    1 => s.scroll += 36,
                    2 => s.scroll -= 240,
                    3 => s.scroll += 240,
                    4 | 5 => s.scroll = info.track,
                    6 => s.scroll = 0,
                    7 => s.scroll = 10000,
                    _ => {}
                }
                layout(hwnd, s);
                0
            }
            0x20A => {
                s.scroll -= ((w >> 16) as u16 as i16 as i32) / 120 * 60;
                layout(hwnd, s);
                0
            }
            0x2E0 => {
                s.scale = ((w & 0xffff) as f32 / 96.0).max(1.0);
                for f in [s.font, s.small_font, s.title_font] {
                    DeleteObject(f);
                }
                s.font = font(16, 400, s.scale);
                s.small_font = font(13, 400, s.scale);
                s.title_font = font(24, 600, s.scale);
                for c in &s.controls {
                    SendMessageW(c.hwnd, 0x30, s.font as usize, 1);
                }
                let r = &*(l as *const Rect);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    0x14,
                );
                layout(hwnd, s);
                0
            }
            0x24 => {
                let m = &mut *(l as *mut MinMax);
                m.min_track = Point {
                    x: scaled(720, s.scale),
                    y: scaled(480, s.scale),
                };
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
    #[allow(clippy::too_many_arguments)]
    unsafe fn child(
        parent: Hwnd,
        s: &mut WindowState,
        class: &str,
        text: &str,
        id: i32,
        page: usize,
        y: i32,
        height: i32,
        style: u32,
    ) {
        let hwnd = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            0x40000000 | 0x00010000 | style,
            0,
            0,
            1,
            1,
            parent,
            id as usize as Hwnd,
            s.instance,
            std::ptr::null_mut(),
        );
        SendMessageW(hwnd, 0x30, s.font as usize, 0);
        let theme = if class == "COMBOBOX" {
            "DarkMode_CFD"
        } else {
            "DarkMode_Explorer"
        };
        SetWindowTheme(hwnd, wide(theme).as_ptr(), std::ptr::null());
        s.controls.push(Control {
            hwnd,
            id,
            page,
            y,
            height,
        });
    }
    unsafe fn button(
        parent: Hwnd,
        s: &mut WindowState,
        text: &str,
        id: i32,
        page: usize,
        y: i32,
        h: i32,
    ) {
        child(parent, s, "BUTTON", text, id, page, y, h, 0xB | 0x4000);
    }
    unsafe fn create_controls(hwnd: Hwnd, s: &mut WindowState) {
        for (i, (title, _)) in PAGES.iter().enumerate() {
            button(
                hwnd,
                s,
                title,
                ID_NAV + i as i32,
                usize::MAX,
                108 + i as i32 * 52,
                44,
            );
        }
        for (id, y, choices) in [
            (
                ID_MODE,
                150,
                vec![
                    "Smart · clean up and format".into(),
                    "Verbatim · keep every word".into(),
                ],
            ),
            (
                ID_HOTKEY,
                238,
                hotkey::PRESETS.iter().map(|x| x.to_string()).collect(),
            ),
            (
                ID_MIC,
                326,
                s.mics
                    .iter()
                    .map(|m| {
                        if m.is_empty() {
                            "System default microphone".into()
                        } else {
                            m.clone()
                        }
                    })
                    .collect(),
            ),
        ] {
            child(hwnd, s, "COMBOBOX", "", id, 0, y, 30, 0x0003 | 0x00200000);
            for choice in choices {
                SendMessageW(
                    handle(s, id),
                    CB_ADDSTRING,
                    0,
                    wide(&choice).as_ptr() as isize,
                );
            }
        }
        button(hwnd, s, "Paste API key from clipboard", ID_KEY, 0, 416, 44);
        for (i, pref) in PREFS.iter().enumerate().skip(6) {
            button(
                hwnd,
                s,
                pref.0,
                ID_PREF + i as i32,
                0,
                518 + (i as i32 - 6) * 94,
                86,
            );
        }
        child(hwnd, s, "EDIT", "", ID_WORD, 1, 164, 38, 0x00800000 | 0x80);
        SendMessageW(handle(s, ID_WORD), 0xC5, 120, 0);
        button(hwnd, s, "+ Add phrase", ID_ADD, 1, 164, 38);
        child(
            hwnd,
            s,
            "LISTBOX",
            "",
            ID_WORDS,
            1,
            222,
            286,
            0x00800000 | 0x00200000 | 1,
        );
        button(hwnd, s, "Remove selected phrase", ID_REMOVE, 1, 526, 40);
        for (i, pref) in PREFS.iter().enumerate().take(6).skip(3) {
            button(
                hwnd,
                s,
                pref.0,
                ID_PREF + i as i32,
                2,
                140 + (i as i32 - 3) * 112,
                102,
            );
        }
        for (i, pref) in PREFS.iter().enumerate().take(3) {
            button(
                hwnd,
                s,
                pref.0,
                ID_PREF + i as i32,
                3,
                140 + i as i32 * 112,
                102,
            );
        }
    }
    fn handle(s: &WindowState, id: i32) -> Hwnd {
        s.controls
            .iter()
            .find(|c| c.id == id)
            .map_or(std::ptr::null_mut(), |c| c.hwnd)
    }
    unsafe fn client(hwnd: Hwnd, s: &WindowState) -> (i32, i32) {
        let mut r = Rect::default();
        GetClientRect(hwnd, &mut r);
        (
            (r.right as f32 / s.scale) as i32,
            (r.bottom as f32 / s.scale) as i32,
        )
    }
    fn sidebar(width: i32) -> i32 {
        if width < 850 {
            184
        } else {
            220
        }
    }
    fn content_height(page: usize) -> i32 {
        match page {
            0 => 826,
            1 => 638,
            _ => 516,
        }
    }
    unsafe fn layout(hwnd: Hwnd, s: &mut WindowState) {
        let (width, height) = client(hwnd, s);
        let side = sidebar(width);
        let x = side + 32;
        let cw = (width - x - 32).max(200);
        s.scroll = s.scroll.clamp(0, (content_height(s.page) - height).max(0));
        let si = ScrollInfo {
            size: std::mem::size_of::<ScrollInfo>() as u32,
            mask: 7,
            min: 0,
            max: content_height(s.page) - 1,
            page: height.max(1) as u32,
            pos: s.scroll,
            track: 0,
        };
        SetScrollInfo(hwnd, 1, &si, 1);
        for c in &s.controls {
            let nav = c.page == usize::MAX;
            let visible = nav || c.page == s.page;
            let (cx, cy, ww) = if nav {
                (16, c.y, side - 32)
            } else {
                let ww = match c.id {
                    ID_WORD => cw - 150,
                    ID_ADD => 138,
                    _ => cw,
                };
                let xx = if c.id == ID_ADD { x + cw - 138 } else { x };
                (xx, c.y - s.scroll, ww)
            };
            let h = if [ID_MODE, ID_HOTKEY, ID_MIC].contains(&c.id) {
                240
            } else {
                c.height
            };
            MoveWindow(
                c.hwnd,
                scaled(cx, s.scale),
                scaled(cy, s.scale),
                scaled(ww, s.scale),
                scaled(h, s.scale),
                0,
            );
            ShowWindow(c.hwnd, if visible { 5 } else { 0 });
        }
        InvalidateRect(hwnd, std::ptr::null(), 1);
    }
    unsafe fn refresh(hwnd: Hwnd, s: &mut WindowState) {
        let snapshot = match s.snapshot.lock() {
            Ok(v) => v.clone(),
            Err(_) => return,
        };
        SendMessageW(
            handle(s, ID_MODE),
            CB_SETCURSEL,
            usize::from(snapshot.mode == "verbatim"),
            0,
        );
        let h = hotkey::PRESETS
            .iter()
            .position(|p| *p == hotkey::normalize(&snapshot.hotkey))
            .unwrap_or(0);
        SendMessageW(handle(s, ID_HOTKEY), CB_SETCURSEL, h, 0);
        let m = s.mics.iter().position(|m| m == &snapshot.mic).unwrap_or(0);
        SendMessageW(handle(s, ID_MIC), CB_SETCURSEL, m, 0);
        let list = handle(s, ID_WORDS);
        let selected = SendMessageW(list, LB_GETCURSEL, 0, 0);
        SendMessageW(list, LB_RESETCONTENT, 0, 0);
        for phrase in &snapshot.vocabulary {
            SendMessageW(list, LB_ADDSTRING, 0, wide(phrase).as_ptr() as isize);
        }
        if selected >= 0 {
            SendMessageW(list, 0x186, selected as usize, 0);
        }
        EnableWindow(
            handle(s, ID_REMOVE),
            i32::from(!snapshot.vocabulary.is_empty()),
        );
        EnableWindow(
            handle(s, ID_ADD),
            i32::from(snapshot.vocabulary.len() < config::MAX_CUSTOM_VOCABULARY),
        );
        layout(hwnd, s);
    }
    unsafe fn command(hwnd: Hwnd, s: &mut WindowState, w: usize) {
        let id = (w & 0xffff) as i32;
        let code = (w >> 16) & 0xffff;
        // Native Tab navigation should also reveal controls below the viewport.
        let focus = if [ID_MODE, ID_HOTKEY, ID_MIC].contains(&id) {
            code == 3
        } else if id == ID_WORD {
            code == 0x100
        } else if id == ID_WORDS {
            code == 4
        } else {
            code == 6
        };
        if focus {
            if let Some(c) = s.controls.iter().find(|c| c.id == id && c.page == s.page) {
                let (_, height) = client(hwnd, s);
                if c.y < s.scroll + 12 {
                    s.scroll = c.y - 12;
                } else if c.y + c.height > s.scroll + height - 12 {
                    s.scroll = c.y + c.height - height + 12;
                }
                layout(hwnd, s);
            }
            return;
        }
        if id == 2 && code == 0 {
            ShowWindow(hwnd, 0);
            return;
        }
        if (ID_NAV..ID_NAV + 4).contains(&id) && code == 0 {
            s.page = (id - ID_NAV) as usize;
            s.scroll = 0;
            layout(hwnd, s);
            for i in 0..PAGES.len() {
                InvalidateRect(handle(s, ID_NAV + i as i32), std::ptr::null(), 1);
            }
            return;
        }
        if (ID_PREF..ID_PREF + 9).contains(&id) && code == 0 {
            let prefs = if let Ok(mut snap) = s.snapshot.lock() {
                toggle_preference(&mut snap.preferences, (id - ID_PREF) as usize);
                Some(snap.preferences.clone())
            } else {
                None
            };
            if let Some(p) = prefs {
                let _ = s.actions.send(MenuCmd::Preferences(p));
            }
            InvalidateRect(handle(s, id), std::ptr::null(), 0);
            return;
        }
        let cmd = match (id, code) {
            (ID_MODE, 1) => Some(MenuCmd::Mode(
                if SendMessageW(handle(s, id), CB_GETCURSEL, 0, 0) == 1 {
                    "verbatim"
                } else {
                    "smart"
                }
                .into(),
            )),
            (ID_HOTKEY, 1) => hotkey::PRESETS
                .get(SendMessageW(handle(s, id), CB_GETCURSEL, 0, 0) as usize)
                .map(|x| MenuCmd::Hotkey((*x).into())),
            (ID_MIC, 1) => s
                .mics
                .get(SendMessageW(handle(s, id), CB_GETCURSEL, 0, 0) as usize)
                .map(|x| MenuCmd::Mic(x.clone())),
            (ID_KEY, 0) => Some(MenuCmd::PasteKey),
            (ID_ADD, 0) => {
                let edit = handle(s, ID_WORD);
                let n = GetWindowTextLengthW(edit).max(0) as usize;
                let mut b = vec![0u16; n + 1];
                GetWindowTextW(edit, b.as_mut_ptr(), b.len() as i32);
                match config::normalize_custom_term(&String::from_utf16_lossy(&b[..n])) {
                    Some(p) => {
                        let snap = s.snapshot.lock().unwrap();
                        if snap.vocabulary.iter().any(|x| x.eq_ignore_ascii_case(&p)) {
                            s.status = "That phrase is already in your dictionary.".into();
                            None
                        } else if snap.vocabulary.len() >= config::MAX_CUSTOM_VOCABULARY {
                            s.status = "Your dictionary is full (1,000 phrases).".into();
                            None
                        } else {
                            SetWindowTextW(edit, wide("").as_ptr());
                            s.status = "Phrase added for your next dictation.".into();
                            Some(MenuCmd::DictionaryAdd(p))
                        }
                    }
                    None => {
                        s.status = "Enter a phrase of up to 120 characters.".into();
                        None
                    }
                }
            }
            (ID_REMOVE, 0) => {
                let index = SendMessageW(handle(s, ID_WORDS), LB_GETCURSEL, 0, 0);
                if index >= 0 {
                    s.snapshot
                        .lock()
                        .ok()
                        .and_then(|v| v.vocabulary.get(index as usize).cloned())
                        .map(MenuCmd::DictionaryRemove)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(cmd) = cmd {
            if s.actions.send(cmd).is_err() {
                s.status = "Utterly is not responding.".into();
            }
        }
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
    unsafe fn rounded(dc: Hwnd, r: Rect, radius: i32, fill: u32, stroke: u32) {
        let brush = CreateSolidBrush(fill);
        let pen = CreatePen(0, 1, stroke);
        let old_b = SelectObject(dc, brush);
        let old_p = SelectObject(dc, pen);
        RoundRect(dc, r.left, r.top, r.right, r.bottom, radius, radius);
        SelectObject(dc, old_b);
        SelectObject(dc, old_p);
        DeleteObject(brush);
        DeleteObject(pen);
    }
    unsafe fn text(dc: Hwnd, font: Hwnd, color: u32, value: &str, mut r: Rect, flags: u32) {
        let old = SelectObject(dc, font);
        SetBkMode(dc, 1);
        SetTextColor(dc, color);
        DrawTextW(dc, wide(value).as_ptr(), -1, &mut r, flags | 0x800);
        SelectObject(dc, old);
    }
    fn rect(x: i32, y: i32, w: i32, h: i32, s: f32) -> Rect {
        Rect {
            left: scaled(x, s),
            top: scaled(y, s),
            right: scaled(x + w, s),
            bottom: scaled(y + h, s),
        }
    }
    unsafe fn paint(hwnd: Hwnd, s: &WindowState, dc: Hwnd) {
        let (w, h) = client(hwnd, s);
        FillRect(dc, &rect(0, 0, w, h, s.scale), s.bg_brush);
        let side = sidebar(w);
        let brush = CreateSolidBrush(SIDEBAR);
        FillRect(dc, &rect(0, 0, side, h, s.scale), brush);
        DeleteObject(brush);
        if !s.icon.is_null() {
            DrawIconEx(
                dc,
                scaled(26, s.scale),
                scaled(36, s.scale),
                s.icon,
                scaled(30, s.scale),
                scaled(30, s.scale),
                0,
                std::ptr::null_mut(),
                3,
            );
        }
        text(
            dc,
            s.title_font,
            INK,
            "Utterly",
            rect(66, 35, side - 78, 36, s.scale),
            0,
        );
        text(
            dc,
            s.small_font,
            MUTED,
            "YOUR VOICE, EVERYWHERE",
            rect(28, 76, side - 40, 20, s.scale),
            0,
        );
        text(
            dc,
            s.small_font,
            MUTED,
            "Ready when you are",
            rect(28, h - 46, side - 40, 24, s.scale),
            0,
        );
        let x = side + 32;
        let cw = w - x - 32;
        let dy = s.scroll;
        text(
            dc,
            s.title_font,
            INK,
            PAGES[s.page].0,
            rect(x, 38 - dy, cw, 36, s.scale),
            0,
        );
        text(
            dc,
            s.font,
            MUTED,
            PAGES[s.page].1,
            rect(x, 80 - dy, cw, 30, s.scale),
            0,
        );
        let label = |value: &str, y: i32| {
            text(
                dc,
                s.small_font,
                MUTED,
                value,
                rect(x, y - dy, cw, 24, s.scale),
                0,
            )
        };
        match s.page {
            0 => {
                label("TRANSCRIPTION STYLE", 122);
                label("HOLD TO DICTATE · RELEASE TO INSERT", 210);
                label("MICROPHONE", 298);
                let key = s.snapshot.lock().map(|v| v.has_api_key).unwrap_or(false);
                label(
                    if key {
                        "GEMINI API KEY · SAVED"
                    } else {
                        "GEMINI API KEY · NOT SET"
                    },
                    386,
                );
                label("APPEARANCE", 482);
            }
            1 => {
                let count = s.snapshot.lock().map(|v| v.vocabulary.len()).unwrap_or(0);
                label(&format!("PERSONAL PHRASES · {count} / 1,000"), 126);
                label("Best results typically use 100 phrases or fewer.", 584);
                if !s.status.is_empty() {
                    text(
                        dc,
                        s.small_font,
                        VIOLET,
                        &s.status,
                        rect(x, 610 - dy, cw, 24, s.scale),
                        0,
                    );
                }
            }
            2 => {
                text(
                    dc,
                    s.small_font,
                    MUTED,
                    "Context features are optional. Password fields are excluded.",
                    rect(x, 492 - dy, cw, 40, s.scale),
                    0x10,
                );
            }
            _ => {}
        }
    }
    unsafe fn draw_control(s: &WindowState, d: &DrawItem) {
        let id = d.id as i32;
        let scale = s.scale;
        let w = ((d.rect.right - d.rect.left) as f32 / scale) as i32;
        let h = ((d.rect.bottom - d.rect.top) as f32 / scale) as i32;
        let focus = d.state & 0x10 != 0;
        let pressed = d.state & 1 != 0;
        let disabled = d.state & 4 != 0;
        let nav = (ID_NAV..ID_NAV + 4).contains(&id);
        let background = CreateSolidBrush(if nav { SIDEBAR } else { BG });
        FillRect(d.dc, &d.rect, background);
        DeleteObject(background);
        if nav {
            let selected = s.page == (id - ID_NAV) as usize;
            rounded(
                d.dc,
                d.rect,
                scaled(18, scale),
                if selected { 0x004A3838 } else { SIDEBAR },
                if focus { VIOLET } else { SIDEBAR },
            );
            text(
                d.dc,
                s.font,
                if selected { INK } else { MUTED },
                PAGES[(id - ID_NAV) as usize].0,
                rect(18, 0, w - 32, h, scale),
                0x24,
            );
            return;
        }
        let is_pref = (ID_PREF..ID_PREF + 9).contains(&id);
        let fill = if pressed { 0x003B3430 } else { CARD };
        rounded(
            d.dc,
            d.rect,
            scaled(22, scale),
            fill,
            if focus { VIOLET } else { BORDER },
        );
        if is_pref {
            let i = (id - ID_PREF) as usize;
            let enabled = s
                .snapshot
                .lock()
                .map(|v| preference_value(&v.preferences, i))
                .unwrap_or(false);
            text(
                d.dc,
                s.font,
                INK,
                PREFS[i].0,
                rect(20, 17, w - 108, 25, scale),
                0x20,
            );
            text(
                d.dc,
                s.small_font,
                MUTED,
                PREFS[i].1,
                rect(20, 46, w - 108, h - 49, scale),
                0x10,
            );
            let x = w - 66;
            let y = (h - 26) / 2;
            rounded(
                d.dc,
                rect(x, y, 46, 26, scale),
                scaled(26, scale),
                if enabled { VIOLET } else { 0x00544D47 },
                if enabled { VIOLET } else { 0x00544D47 },
            );
            rounded(
                d.dc,
                rect(x + if enabled { 23 } else { 3 }, y + 3, 20, 20, scale),
                scaled(20, scale),
                INK,
                INK,
            );
        } else {
            let title = match id {
                ID_ADD => "+ Add phrase",
                ID_REMOVE => "Remove selected phrase",
                ID_KEY => "Paste API key from clipboard",
                _ => "",
            };
            text(
                d.dc,
                s.font,
                if disabled { MUTED } else { INK },
                title,
                rect(12, 0, w - 24, h, scale),
                0x25,
            );
        }
    }
    #[test]
    fn native_settings_dispatch_preferences_mode_and_dictionary() {
        let (tx, rx) = mpsc::channel();
        let win = SettingsWindow::spawn(Snapshot::default(), tx).unwrap();
        let hwnd = win.hwnd.load(Ordering::Acquire) as Hwnd;
        unsafe {
            SendMessageW(hwnd, 0x111, (ID_PREF + 1) as usize, 0);
        }
        match rx.recv_timeout(Duration::from_secs(2)).unwrap() {
            MenuCmd::Preferences(p) => assert!(p.duck_audio),
            other => panic!("unexpected settings command: {other:?}"),
        }
        unsafe {
            let mode = GetDlgItem(hwnd, ID_MODE);
            SendMessageW(mode, CB_SETCURSEL, 1, 0);
            SendMessageW(hwnd, 0x111, ID_MODE as usize | (1 << 16), mode as isize);
        }
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), MenuCmd::Mode(m) if m == "verbatim")
        );
        unsafe {
            SendMessageW(hwnd, 0x111, (ID_NAV + 1) as usize, 0);
            SetWindowTextW(GetDlgItem(hwnd, ID_WORD), wide("Rust language").as_ptr());
            SendMessageW(hwnd, 0x111, ID_ADD as usize, 0);
        }
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), MenuCmd::DictionaryAdd(p) if p == "Rust language")
        );
        win.close();
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassW(c: *const WindowClass) -> u16;
        fn CreateIconFromResourceEx(
            data: *const u8,
            size: u32,
            icon: i32,
            version: u32,
            w: i32,
            h: i32,
            flags: u32,
        ) -> Hwnd;
        fn DestroyIcon(icon: Hwnd) -> i32;
        fn DrawIconEx(
            dc: Hwnd,
            x: i32,
            y: i32,
            icon: Hwnd,
            w: i32,
            h: i32,
            step: u32,
            brush: Hwnd,
            flags: u32,
        ) -> i32;
        #[cfg(test)]
        fn GetDlgItem(h: Hwnd, id: i32) -> Hwnd;
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: Hwnd,
            menu: Hwnd,
            instance: Hwnd,
            param: *mut c_void,
        ) -> Hwnd;
        fn DefWindowProcW(h: Hwnd, m: u32, w: usize, l: isize) -> isize;
        fn SetWindowLongPtrW(h: Hwnd, i: i32, v: isize) -> isize;
        fn GetWindowLongPtrW(h: Hwnd, i: i32) -> isize;
        fn GetMessageW(m: *mut Message, h: Hwnd, a: u32, b: u32) -> i32;
        fn TranslateMessage(m: *const Message) -> i32;
        fn DispatchMessageW(m: *const Message) -> isize;
        fn IsDialogMessageW(h: Hwnd, m: *mut Message) -> i32;
        fn PostMessageW(h: Hwnd, m: u32, w: usize, l: isize) -> i32;
        fn ShowWindow(h: Hwnd, c: i32) -> i32;
        fn SetForegroundWindow(h: Hwnd) -> i32;
        fn SendMessageW(h: Hwnd, m: u32, w: usize, l: isize) -> isize;
        fn GetWindowTextLengthW(h: Hwnd) -> i32;
        fn GetWindowTextW(h: Hwnd, t: *mut u16, n: i32) -> i32;
        fn SetWindowTextW(h: Hwnd, t: *const u16) -> i32;
        fn EnableWindow(h: Hwnd, e: i32) -> i32;
        fn LoadCursorW(h: Hwnd, c: *const u16) -> Hwnd;
        fn GetDpiForSystem() -> u32;
        fn GetClientRect(h: Hwnd, r: *mut Rect) -> i32;
        fn FillRect(d: Hwnd, r: *const Rect, b: Hwnd) -> i32;
        fn DestroyWindow(h: Hwnd) -> i32;
        fn PostQuitMessage(c: i32);
        fn MoveWindow(h: Hwnd, x: i32, y: i32, w: i32, height: i32, repaint: i32) -> i32;
        fn InvalidateRect(h: Hwnd, r: *const Rect, e: i32) -> i32;
        fn BeginPaint(h: Hwnd, p: *mut Paint) -> Hwnd;
        fn EndPaint(h: Hwnd, p: *const Paint) -> i32;
        fn DrawTextW(d: Hwnd, t: *const u16, n: i32, r: *mut Rect, f: u32) -> i32;
        fn SetScrollInfo(h: Hwnd, bar: i32, s: *const ScrollInfo, redraw: i32) -> i32;
        fn GetScrollInfo(h: Hwnd, bar: i32, s: *mut ScrollInfo) -> i32;
        fn SetWindowPos(
            h: Hwnd,
            after: Hwnd,
            x: i32,
            y: i32,
            w: i32,
            height: i32,
            flags: u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(n: *const u16) -> Hwnd;
    }
    #[link(name = "uxtheme")]
    unsafe extern "system" {
        fn SetWindowTheme(h: Hwnd, app: *const u16, id: *const u16) -> i32;
    }
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(h: Hwnd, a: u32, v: *const c_void, n: u32) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateSolidBrush(c: u32) -> Hwnd;
        fn CreatePen(style: i32, width: i32, color: u32) -> Hwnd;
        fn CreateFontW(
            h: i32,
            w: i32,
            e: i32,
            o: i32,
            weight: i32,
            italic: u32,
            underline: u32,
            strike: u32,
            charset: u32,
            out: u32,
            clip: u32,
            quality: u32,
            pitch: u32,
            face: *const u16,
        ) -> Hwnd;
        fn SetTextColor(d: Hwnd, c: u32) -> u32;
        fn SetBkColor(d: Hwnd, c: u32) -> u32;
        fn SetBkMode(d: Hwnd, m: i32) -> i32;
        fn DeleteObject(o: Hwnd) -> i32;
        fn SelectObject(d: Hwnd, o: Hwnd) -> Hwnd;
        fn RoundRect(d: Hwnd, l: i32, t: i32, r: i32, b: i32, ew: i32, eh: i32) -> i32;
    }
}
#[cfg(target_os = "windows")]
pub use windows::SettingsWindow;
#[cfg(not(target_os = "windows"))]
#[derive(Clone)]
pub struct SettingsWindow;
#[cfg(not(target_os = "windows"))]
impl SettingsWindow {
    pub fn spawn(
        _: Snapshot,
        _: std::sync::mpsc::Sender<crate::tray::MenuCmd>,
    ) -> Result<Self, String> {
        Ok(Self)
    }
    pub fn show(&self) {}
    pub fn update(&self, _: Snapshot) {}
    pub fn close(&self) {}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_toggle_preserves_other_settings() {
        let mut p = crate::config::Preferences::default();
        let original = p.clone();
        toggle_preference(&mut p, 0);
        assert_eq!(p.interaction_sounds, !original.interaction_sounds);
        assert_eq!(p.duck_audio, original.duck_audio);
        toggle_preference(&mut p, 0);
        assert_eq!(p, original);
    }
    #[test]
    fn all_preferences_toggle_independently() {
        let mut p = crate::config::Preferences::default();
        for i in 0..9 {
            let before = p.clone();
            let value = preference_value(&p, i);
            toggle_preference(&mut p, i);
            assert_eq!(preference_value(&p, i), !value);
            for j in 0..9 {
                if j != i {
                    assert_eq!(preference_value(&p, j), preference_value(&before, j));
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn test_settings_window_lifecycle() {
        let (tx, _) = std::sync::mpsc::channel();
        let snapshot = Snapshot {
            mode: "smart".into(),
            hotkey: "Ctrl+Win".into(),
            vocabulary: vec!["Kubernetes".into()],
            ..Snapshot::default()
        };
        let win = SettingsWindow::spawn(snapshot.clone(), tx).expect("spawn settings");
        win.update(snapshot);
        win.close();
    }
}
