//! 计算桌宠能站的面：各显示器工作区底边（任务栏上沿），以及没被遮挡的窗口顶边。

use super::win::{self, Monitor, Rect};
use std::time::Instant;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextLengthW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST};

/// 一段可以站立的水平线段
#[derive(Clone, Copy, Debug)]
pub struct Interval {
    /// 所属窗口；0 表示工作区底边
    pub hwnd: isize,
    pub y: f64,
    pub x1: f64,
    pub x2: f64,
    /// 快照时宿主窗口的左边界，宿主移动后用来平移线段
    pub host_left: i32,
}

pub struct Snapshot {
    pub at: Instant,
    pub intervals: Vec<Interval>,
    pub monitors: Vec<Monitor>,
}

const SHELL_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
];

/// 能挡住或承载桌宠的普通顶层窗口
fn is_candidate(raw: isize, own_pid: u32) -> bool {
    if !win::is_visible(raw) || win::is_cloaked(raw) {
        return false;
    }
    let ex = win::ex_style(raw);
    // 自己的桌宠和气泡窗口（都是置顶的）不算；聊天、设置窗口和别的窗口一样可以站、可以遮挡
    if win::window_pid(raw) == own_pid && ex & WS_EX_TOPMOST.0 as isize != 0 {
        return false;
    }
    if ex & WS_EX_TOOLWINDOW.0 as isize != 0 {
        return false;
    }
    if unsafe { GetWindowTextLengthW(HWND(raw as *mut _)) } == 0 {
        return false;
    }
    !SHELL_CLASSES.contains(&win::class_name(raw).as_str())
}

unsafe extern "system" fn enum_cb(h: HWND, data: LPARAM) -> BOOL {
    let out = &mut *(data.0 as *mut Vec<isize>);
    out.push(h.0 as isize);
    BOOL(1)
}

/// 按 Z 序（从上到下）列出候选窗口及其可见边框
fn windows_top_down(own_pid: u32) -> Vec<(isize, Rect)> {
    let mut all: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut all as *mut _ as isize));
    }
    all.into_iter()
        .filter(|&h| is_candidate(h, own_pid))
        .filter_map(|h| win::frame_bounds(h).map(|r| (h, r)))
        .filter(|(_, r)| r.w() >= 120 && r.h() >= 60)
        .collect()
}

/// 从 [x1, x2] 中扣掉 [a, b]
fn subtract(segs: Vec<(f64, f64)>, a: f64, b: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(segs.len() + 1);
    for (x1, x2) in segs {
        if b <= x1 || a >= x2 {
            out.push((x1, x2));
            continue;
        }
        if a > x1 {
            out.push((x1, a));
        }
        if b < x2 {
            out.push((b, x2));
        }
    }
    out
}

fn monitor_at(monitors: &[Monitor], x: f64, y: f64) -> Option<&Monitor> {
    monitors.iter().find(|m| {
        x >= m.full.l as f64 && x < m.full.r as f64 && y >= m.full.t as f64 && y <= m.full.b as f64
    })
}

/// pet_h：桌宠高度。顶边离屏幕顶部太近（站上去身子会出屏）的窗口不算。
pub fn snapshot(own_pid: u32, pet_h: i32) -> Snapshot {
    let monitors = win::monitors();
    let mut intervals: Vec<Interval> = monitors
        .iter()
        .map(|m| Interval {
            hwnd: 0,
            y: m.work.b as f64,
            x1: m.work.l as f64,
            x2: m.work.r as f64,
            host_left: 0,
        })
        .collect();

    let wins = windows_top_down(own_pid);
    for (i, &(h, r)) in wins.iter().enumerate() {
        let y = r.t as f64;
        let mid = (r.l + r.r) as f64 / 2.0;
        let Some(mon) = monitor_at(&monitors, mid, y) else { continue };
        if r.t - pet_h < mon.work.t || r.t >= mon.work.b {
            continue;
        }
        let mut segs = vec![(r.l as f64, r.r as f64)];
        // 只有更上层、且盖住了这条顶边的窗口才算遮挡
        for &(_, o) in &wins[..i] {
            if o.t <= r.t && o.b > r.t {
                segs = subtract(segs, o.l as f64, o.r as f64);
                if segs.is_empty() {
                    break;
                }
            }
        }
        intervals.extend(
            segs.into_iter()
                .filter(|(a, b)| b - a >= 40.0)
                .map(|(x1, x2)| Interval { hwnd: h, y, x1, x2, host_left: r.l }),
        );
    }

    Snapshot { at: Instant::now(), intervals, monitors }
}

/// 宿主窗口当前仍可站立时返回它的边框
pub fn host_rect(raw: isize) -> Option<Rect> {
    if !win::is_visible(raw) || win::is_cloaked(raw) {
        return None;
    }
    win::frame_bounds(raw)
}

#[cfg(test)]
mod tests {
    use super::subtract;

    #[test]
    fn subtract_splits_and_trims() {
        assert_eq!(subtract(vec![(0.0, 100.0)], 30.0, 60.0), vec![(0.0, 30.0), (60.0, 100.0)]);
        assert_eq!(subtract(vec![(0.0, 100.0)], -10.0, 40.0), vec![(40.0, 100.0)]);
        assert_eq!(subtract(vec![(0.0, 100.0)], 100.0, 140.0), vec![(0.0, 100.0)]);
        assert!(subtract(vec![(0.0, 100.0)], -1.0, 101.0).is_empty());
    }
}
