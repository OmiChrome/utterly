//! Focus and app icons via Windows APIs; no process text or user context is logged.
use super::FocusTarget;
use std::ffi::c_void;
use windows::core::Interface;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HDC,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    SAFEARRAY,
};
use windows::Win32::UI::Accessibility::*;

#[link(name = "oleaut32")]
unsafe extern "system" {
    fn SafeArrayGetLBound(array: *mut SAFEARRAY, dimension: u32, bound: *mut i32) -> i32;
    fn SafeArrayGetUBound(array: *mut SAFEARRAY, dimension: u32, bound: *mut i32) -> i32;
    fn SafeArrayGetElement(array: *mut SAFEARRAY, index: *const i32, value: *mut c_void) -> i32;
    fn SafeArrayDestroy(array: *mut SAFEARRAY) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetForegroundWindow() -> *mut c_void;
    fn GetWindowThreadProcessId(window: *mut c_void, process: *mut u32) -> u32;
    fn SendMessageTimeoutW(
        window: *mut c_void,
        message: u32,
        wparam: usize,
        lparam: isize,
        flags: u32,
        timeout: u32,
        result: *mut usize,
    ) -> isize;
    fn GetClassLongPtrW(window: *mut c_void, index: i32) -> usize;
    fn DrawIconEx(
        dc: *mut c_void,
        x: i32,
        y: i32,
        icon: *mut c_void,
        width: i32,
        height: i32,
        step: u32,
        brush: *mut c_void,
        flags: u32,
    ) -> i32;
    fn DestroyIcon(icon: *mut c_void) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, process: u32) -> *mut c_void;
    fn QueryFullProcessImageNameW(
        process: *mut c_void,
        flags: u32,
        path: *mut u16,
        size: *mut u32,
    ) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn ExtractIconExW(
        file: *const u16,
        index: i32,
        large: *mut *mut c_void,
        small: *mut *mut c_void,
        count: u32,
    ) -> u32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(dc: *mut c_void) -> *mut c_void;
    fn SelectObject(dc: *mut c_void, object: *mut c_void) -> *mut c_void;
    fn DeleteObject(object: *mut c_void) -> i32;
    fn DeleteDC(dc: *mut c_void) -> i32;
    fn GdiFlush() -> i32;
}

struct ComGuard(bool);
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

unsafe fn runtime_id(element: &IUIAutomationElement) -> Option<Vec<i32>> {
    let array = element.GetRuntimeId().ok()?;
    if array.is_null() {
        return None;
    }
    let mut lower = 0;
    let mut upper = -1;
    let valid = SafeArrayGetLBound(array, 1, &mut lower) >= 0
        && SafeArrayGetUBound(array, 1, &mut upper) >= 0
        && upper >= lower
        && upper.saturating_sub(lower) < 64;
    let mut result = Vec::new();
    if valid {
        for index in lower..=upper {
            let mut value = 0i32;
            if SafeArrayGetElement(array, &index, (&mut value as *mut i32).cast()) < 0 {
                result.clear();
                break;
            }
            result.push(value);
        }
    }
    SafeArrayDestroy(array);
    (!result.is_empty()).then_some(result)
}

unsafe fn caret_context(element: &IUIAutomationElement) -> Option<(String, String)> {
    let pattern: IUIAutomationTextPattern = element.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
    let selection = pattern.GetSelection().ok()?;
    if selection.Length().ok()? != 1 {
        return None;
    }
    let range = selection.GetElement(0).ok()?;
    let before = range.Clone().ok()?;
    before
        .MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &range,
            TextPatternRangeEndpoint_Start,
        )
        .ok()?;
    before
        .MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -160)
        .ok()?;
    let after = range.Clone().ok()?;
    after
        .MoveEndpointByRange(
            TextPatternRangeEndpoint_Start,
            &range,
            TextPatternRangeEndpoint_End,
        )
        .ok()?;
    after
        .MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, 160)
        .ok()?;
    Some((
        before.GetText(160).ok()?.to_string(),
        after.GetText(160).ok()?.to_string(),
    ))
}

pub(super) fn capture(with_context: bool) -> Option<FocusTarget> {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _com = ComGuard(initialized.is_ok());
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).ok()?;
        if let Ok(timeout) = automation.cast::<IUIAutomation2>() {
            let _ = timeout.SetConnectionTimeout(250);
            let _ = timeout.SetTransactionTimeout(250);
        }
        let mut element = automation.GetFocusedElement();
        for _ in 0..2 {
            if !element
                .as_ref()
                .is_err_and(|error| error.code().0 == 0x8013_1505_u32 as i32)
            {
                break;
            }
            // A cold UIA provider can miss its first transaction deadline.
            element = automation.GetFocusedElement();
        }
        let element = element.ok()?;
        if element.CurrentIsPassword().ok()?.as_bool()
            || !element.CurrentIsEnabled().ok()?.as_bool()
            || !element.CurrentHasKeyboardFocus().ok()?.as_bool()
        {
            return None;
        }
        let mut process = 0;
        GetWindowThreadProcessId(window, &mut process);
        if process == 0 || element.CurrentProcessId().ok()? as u32 != process {
            return None;
        }
        let value: Option<IUIAutomationValuePattern> =
            element.GetCurrentPatternAs(UIA_ValuePatternId).ok();
        let editable = if let Some(value) = value {
            !value.CurrentIsReadOnly().ok()?.as_bool()
        } else {
            // ponytail: unknown rich-document providers fall back to clipboard;
            // broaden only with a provider's reliable editable/read-only signal.
            element.CurrentControlType().ok()? == UIA_EditControlTypeId
        };
        if !editable {
            return None;
        }
        let runtime_id = runtime_id(&element)?;
        let surrounding = if with_context {
            caret_context(&element)
        } else {
            None
        };
        let context_known = surrounding.is_some();
        let (before, after) = surrounding.unwrap_or_default();
        if window != GetForegroundWindow() {
            return None;
        }
        let context = format!("{before}{after}");
        Some(FocusTarget {
            window: window as isize,
            runtime_id,
            before,
            after,
            context,
            context_known,
        })
    }
}

unsafe fn render_icon(icon: *mut c_void, background: u8) -> Option<Vec<u8>> {
    let dc = CreateCompatibleDC(std::ptr::null_mut());
    if dc.is_null() {
        return None;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: 40,
            biWidth: 32,
            biHeight: -32,
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let bitmap = CreateDIBSection(
        HDC(dc as isize),
        &info,
        DIB_RGB_COLORS,
        &mut bits,
        HANDLE::default(),
        0,
    )
    .ok()
    .map(|b| b.0 as *mut c_void)
    .unwrap_or_default();
    if bitmap.is_null() || bits.is_null() {
        if !bitmap.is_null() {
            DeleteObject(bitmap);
        }
        DeleteDC(dc);
        return None;
    }
    let previous = SelectObject(dc, bitmap);
    let pixels = std::slice::from_raw_parts_mut(bits.cast::<u8>(), 32 * 32 * 4);
    for px in pixels.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&[background, background, background, 255]);
    }
    let drawn = DrawIconEx(dc, 0, 0, icon, 32, 32, 0, std::ptr::null_mut(), 3) != 0;
    GdiFlush();
    let copy = drawn.then(|| pixels.to_vec());
    SelectObject(dc, previous);
    DeleteObject(bitmap);
    DeleteDC(dc);
    copy
}

pub(super) fn focused_app_icon() -> Option<Vec<u8>> {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let mut icon = 0usize;
        for kind in [2, 0, 1] {
            SendMessageTimeoutW(window, 0x007f, kind, 0, 2, 80, &mut icon);
            if icon != 0 {
                break;
            }
        }
        if icon == 0 {
            icon = GetClassLongPtrW(window, -34);
        }
        if icon == 0 {
            icon = GetClassLongPtrW(window, -14);
        }
        let mut owned = false;
        if icon == 0 {
            let mut process_id = 0;
            GetWindowThreadProcessId(window, &mut process_id);
            let process = OpenProcess(0x1000, 0, process_id);
            if !process.is_null() {
                let mut path = [0u16; 32768];
                let mut length = path.len() as u32;
                if QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) != 0
                    && (length as usize) < path.len()
                {
                    path[length as usize] = 0;
                    let mut extracted = std::ptr::null_mut();
                    if ExtractIconExW(path.as_ptr(), 0, std::ptr::null_mut(), &mut extracted, 1) > 0
                    {
                        icon = extracted as usize;
                        owned = true;
                    }
                }
                CloseHandle(process);
            }
        }
        if icon == 0 {
            return None;
        }
        let black = render_icon(icon as *mut c_void, 0);
        let white = render_icon(icon as *mut c_void, 255);
        if owned {
            DestroyIcon(icon as *mut c_void);
        }
        let (black, white) = (black?, white?);
        let mut rgba = Vec::with_capacity(4096);
        for (b, w) in black
            .as_chunks::<4>()
            .0
            .iter()
            .zip(white.as_chunks::<4>().0)
        {
            // Recover alpha for both modern alpha icons and legacy AND-mask icons.
            let alpha = 255 - w[0].saturating_sub(b[0]);
            for channel in [2, 1, 0] {
                rgba.push(if alpha == 0 {
                    0
                } else {
                    ((b[channel] as u32 * 255) / alpha as u32).min(255) as u8
                });
            }
            rgba.push(alpha);
        }
        Some(rgba)
    }
}
