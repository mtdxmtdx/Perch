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

/// 把置顶窗口提到所有置顶窗口的最上面（也会补回丢掉的置顶），不抢焦点、不动位置
pub fn raise_topmost(raw: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE};
    if raw == 0 {
        return;
    }
    unsafe {
        let _ = SetWindowPos(hwnd(raw), Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
    }
}

/// 挡住了也不该抢回来的窗口：菜单、提示、输入法候选、任务栏和它的缩略图、系统弹出界面、
/// 点得穿的透明浮层（显卡、录屏软件的叠加层）
pub fn ignorable_cover(class: &str, ex_style: isize) -> bool {
    const WS_EX_TRANSPARENT: isize = 0x20;
    if ex_style & WS_EX_TRANSPARENT != 0 {
        return true;
    }
    matches!(
        class,
        "#32768"
            | "tooltips_class32"
            | "SysShadow"
            | "ComboLBox"
            | "DropDown"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "TaskListThumbnailWnd"
            | "NotifyIconOverflowWindow"
            | "TopLevelWindowForOverflowXamlIsland"
            | "Windows.UI.Core.CoreWindow"
            | "Xaml_WindowedPopupClass"
            | "XamlExplorerHostIslandWindow"
            | "ForegroundStaging"
            | "MultitaskingViewFrame"
    ) || class.contains("IME")
        || class.contains("Candidate")
}

/// 挡在 raw 上面、和它有重叠的别的程序的窗口。raw 是置顶窗口时只有别的置顶窗口能盖在上面；
/// raw 丢了置顶时普通窗口也会盖住它，这时它本身就要补回置顶，返回 Some(0)
pub fn covering_window(raw: isize, own_pid: u32) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindow, GW_HWNDPREV, WS_EX_TOPMOST};
    let me = window_rect(raw)?;
    if ex_style(raw) & WS_EX_TOPMOST.0 as isize == 0 {
        return Some(0);
    }
    let mut cur = hwnd(raw);
    for _ in 0..1000 {
        cur = match unsafe { GetWindow(cur, GW_HWNDPREV) } {
            Ok(h) if !h.is_invalid() => h,
            _ => return None,
        };
        let w = cur.0 as isize;
        if !is_visible(w) || is_cloaked(w) || window_pid(w) == own_pid || ignorable_cover(&class_name(w), ex_style(w)) {
            continue;
        }
        let Some(r) = frame_bounds(w).or_else(|| window_rect(w)) else { continue };
        if r.l < me.r && me.l < r.r && r.t < me.b && me.t < r.b {
            return Some(w);
        }
    }
    None
}

pub fn window_pid(raw: isize) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd(raw), Some(&mut pid)) };
    pid
}

/// 进程的可执行文件名（不带 .exe，小写），拿不到时返回 None
pub fn process_name(pid: u32) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return None;
    }
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.is_ok();
    let _ = unsafe { CloseHandle(h) };
    if !ok {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let file = path.rsplit(['\\', '/']).next()?.to_lowercase();
    Some(file.strip_suffix(".exe").unwrap_or(&file).to_string())
}

/// 前台窗口所属程序的名字（不带 .exe，小写）
pub fn foreground_process() -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    if fg == 0 {
        return None;
    }
    process_name(window_pid(fg))
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

#[cfg(test)]
mod tests {
    use super::ignorable_cover;

    #[test]
    fn menus_ime_taskbar_and_click_through_overlays_are_not_fought() {
        for class in ["#32768", "tooltips_class32", "Shell_TrayWnd", "TaskListThumbnailWnd", "MSCTFIME UI", "Microsoft.IME.UIManager.CandidateWindow.Host"] {
            assert!(ignorable_cover(class, 0), "{class}");
        }
        // 点得穿的浮层
        assert!(ignorable_cover("SomeOverlay", 0x20 | 0x80000));
        // 普通的置顶窗口要抢回来
        assert!(!ignorable_cover("WindowsForms10.Window.8.app.0.141b42a_r6_ad1", 0x8));
        assert!(!ignorable_cover("Chrome_WidgetWin_1", 0x8));
    }
}
