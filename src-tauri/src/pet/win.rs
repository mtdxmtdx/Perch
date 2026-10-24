//! 桌宠用到的 Win32 小工具。坐标一律是物理像素（Tauri 进程是 Per-Monitor V2 DPI 感知）。

use std::ffi::c_void;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetCursorPos, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, GWL_STYLE, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_SWAPBUTTON, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    pub fn w(&self) -> i32 {
        self.r - self.l
    }
    pub fn h(&self) -> i32 {
        self.b - self.t
    }
    pub fn contains_x(&self, x: f64) -> bool {
        x >= self.l as f64 && x < self.r as f64
    }
}

impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Rect { l: r.left, t: r.top, r: r.right, b: r.bottom }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Monitor {
    pub handle: isize,
    pub full: Rect,
    pub work: Rect,
}

pub fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

/// 主按钮是否按下。GetAsyncKeyState 看的是物理按键，左右键互换时要换成右键。
pub fn primary_button_down() -> bool {
    unsafe {
        let key = if GetSystemMetrics(SM_SWAPBUTTON) != 0 { VK_RBUTTON } else { VK_LBUTTON };
        (GetAsyncKeyState(key.0 as i32) as u16 & 0x8000) != 0
    }
}

pub fn window_rect(raw: isize) -> Option<Rect> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd(raw), &mut r).ok()? };
    Some(r.into())
}

/// 窗口的可见边框（不含 Win10/11 的透明缩放边）
pub fn frame_bounds(raw: isize) -> Option<Rect> {
    let mut r = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd(raw),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .ok()?;
    }
    Some(r.into())
}

pub fn is_cloaked(raw: isize) -> bool {
    let mut cloaked: u32 = 0;
    unsafe {
        DwmGetWindowAttribute(
            hwnd(raw),
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
    }
}

pub fn is_visible(raw: isize) -> bool {
    unsafe { IsWindowVisible(hwnd(raw)).as_bool() && !IsIconic(hwnd(raw)).as_bool() }
}

pub fn ex_style(raw: isize) -> isize {
    unsafe { GetWindowLongPtrW(hwnd(raw), GWL_EXSTYLE) }
}

pub fn style(raw: isize) -> isize {
    unsafe { GetWindowLongPtrW(hwnd(raw), GWL_STYLE) }
}

pub fn class_name(raw: isize) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd(raw), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn window_pid(raw: isize) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd(raw), Some(&mut pid)) };
    pid
}

pub fn virtual_screen() -> Rect {
    unsafe {
        let l = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let t = GetSystemMetrics(SM_YVIRTUALSCREEN);
        Rect {
            l,
            t,
            r: l + GetSystemMetrics(SM_CXVIRTUALSCREEN),
            b: t + GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

unsafe extern "system" fn monitor_cb(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let out = &mut *(data.0 as *mut Vec<Monitor>);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(m, &mut info).as_bool() {
        out.push(Monitor {
            handle: m.0 as isize,
            full: info.rcMonitor.into(),
            work: info.rcWork.into(),
        });
    }
    BOOL(1)
}

pub fn monitors() -> Vec<Monitor> {
    let mut out: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

pub fn monitor_of_window(raw: isize) -> isize {
    unsafe { MonitorFromWindow(hwnd(raw), MONITOR_DEFAULTTONEAREST).0 as isize }
}

pub fn monitor_at_point(x: i32, y: i32) -> isize {
    unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST).0 as isize }
}
