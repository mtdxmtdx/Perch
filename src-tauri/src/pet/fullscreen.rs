//! 检测桌宠所在显示器上是否有全屏程序（游戏、全屏视频、PPT 放映）。

use super::win;
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsZoomed, WS_CAPTION};

/// (x, y)：桌宠的逻辑位置（隐藏时窗口被挪到屏幕外，不能用窗口所在显示器）
pub fn is_fullscreen_at(x: i32, y: i32, own_pid: u32) -> bool {
    if let Ok(state) = unsafe { SHQueryUserNotificationState() } {
        if state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE {
            return true;
        }
    }

    let fg = unsafe { GetForegroundWindow() };
    let fg_raw = fg.0 as isize;
    if fg_raw == 0 || win::window_pid(fg_raw) == own_pid {
        return false;
    }
    if matches!(win::class_name(fg_raw).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
        return false;
    }
    // 普通最大化窗口在任务栏自动隐藏时也会盖满屏幕，不算全屏。
    // 但浏览器在最大化状态下进入网页全屏时仍是 zoomed，区别是标题栏被去掉了。
    let has_caption = win::style(fg_raw) & WS_CAPTION.0 as isize == WS_CAPTION.0 as isize;
    if unsafe { IsZoomed(fg) }.as_bool() && has_caption {
        return false;
    }
    let mon = win::monitor_of_window(fg_raw);
    if mon != win::monitor_at_point(x, y) {
        return false;
    }
    let Some(r) = win::window_rect(fg_raw) else { return false };
    win::monitors()
        .into_iter()
        .find(|m| m.handle == mon)
        .is_some_and(|m| r.l <= m.full.l && r.t <= m.full.t && r.r >= m.full.r && r.b >= m.full.b)
}

/// 调试用：描述当前前台窗口
pub fn describe_foreground() -> String {
    let fg = unsafe { GetForegroundWindow() };
    let raw = fg.0 as isize;
    format!(
        "fg={:x} class={} zoomed={} style={:08x} rect={:?} mon={:x} quns={:?}",
        raw,
        win::class_name(raw),
        unsafe { IsZoomed(fg) }.as_bool(),
        win::style(raw),
        win::window_rect(raw),
        win::monitor_of_window(raw),
        unsafe { SHQueryUserNotificationState() }.map(|s| s.0),
    )
}
