//! 多显示器的几何判断（不调 Win32，方便测试）：点在哪块屏、离哪块屏最近、怎么把桌宠收回一块屏里。
//! 坐标都是物理像素。几块屏可以左右排、上下叠，大小和缩放也可以不同，中间会有不属于任何屏的空隙。

use super::win::Monitor;

/// 点在这块屏里（含底边，脚底正好踩在屏幕最下沿时也算）
pub fn contains(m: &Monitor, x: f64, y: f64) -> bool {
    x >= m.full.l as f64 && x < m.full.r as f64 && y >= m.full.t as f64 && y <= m.full.b as f64
}

/// 点到这块屏的距离（在屏里是 0）
pub fn distance(m: &Monitor, x: f64, y: f64) -> f64 {
    let dx = (m.full.l as f64 - x).max(0.0).max(x - m.full.r as f64);
    let dy = (m.full.t as f64 - y).max(0.0).max(y - m.full.b as f64);
    dx.hypot(dy)
}

/// 包含这个点的屏；没有就取最近的
pub fn at_or_nearest<'a>(mons: impl IntoIterator<Item = &'a Monitor>, x: f64, y: f64) -> Option<&'a Monitor> {
    mons.into_iter().min_by(|a, b| distance(a, x, y).total_cmp(&distance(b, x, y)))
}

/// 桌宠（身体中心 cx、脚底 fy、半宽 half、身高 h）收进这块屏：返回新位置，以及水平、竖直方向有没有被挡住
pub fn clamp_into(m: &Monitor, cx: f64, fy: f64, half: f64, h: f64) -> (f64, f64, bool, bool) {
    let (l, r) = (m.full.l as f64 + half, m.full.r as f64 - half);
    let (t, b) = (m.full.t as f64 + h, m.full.b as f64);
    let x = if l <= r { cx.clamp(l, r) } else { (m.full.l + m.full.r) as f64 / 2.0 };
    let y = if t <= b { fy.clamp(t, b) } else { b };
    (x, y, (x - cx).abs() > 0.5, (y - fy).abs() > 0.5)
}

/// 屏幕编号：设备名 \\.\DISPLAY2 → 2
pub fn number(name: &str) -> Option<u32> {
    let digits: String = name.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse().ok()
}

/// 菜单里显示的名字：显示器 2（1080×1920，主）
pub fn label(m: &Monitor, index: usize) -> String {
    let n = number(&m.name).unwrap_or(index as u32 + 1);
    let size = format!("{}×{}", m.full.r - m.full.l, m.full.b - m.full.t);
    if m.primary {
        format!("显示器 {n}（{size}，主）")
    } else {
        format!("显示器 {n}（{size}）")
    }
}

/// 在工作区里的横向位置（0 左边 ~ 1 右边），用来记住位置、换屏时放到差不多的地方
pub fn x_ratio(m: &Monitor, cx: f64) -> f64 {
    let w = (m.work.r - m.work.l).max(1) as f64;
    ((cx - m.work.l as f64) / w).clamp(0.0, 1.0)
}

pub fn x_at(m: &Monitor, ratio: f64, half: f64) -> f64 {
    let (l, r) = (m.work.l as f64 + half, m.work.r as f64 - half);
    let x = m.work.l as f64 + ratio.clamp(0.0, 1.0) * (m.work.r - m.work.l) as f64;
    if l <= r { x.clamp(l, r) } else { (m.work.l + m.work.r) as f64 / 2.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pet::win::Rect;

    fn mon(name: &str, l: i32, t: i32, r: i32, b: i32, work_b: i32, primary: bool) -> Monitor {
        Monitor { handle: 0, name: name.into(), primary, full: Rect { l, t, r, b }, work: Rect { l, t, r, b: work_b } }
    }

    /// 你现在的摆法：左边一块竖屏（没有任务栏），右边主屏 2560×1440，顶边对齐
    fn layout() -> Vec<Monitor> {
        vec![mon(r"\\.\DISPLAY1", 0, 0, 2560, 1440, 1392, true), mon(r"\\.\DISPLAY2", -1080, 0, 0, 1920, 1920, false)]
    }

    #[test]
    fn finds_the_screen_by_both_x_and_y() {
        let m = layout();
        assert_eq!(at_or_nearest(&m, 100.0, 1000.0).unwrap().name, r"\\.\DISPLAY1");
        assert_eq!(at_or_nearest(&m, -100.0, 1800.0).unwrap().name, r"\\.\DISPLAY2");
        // 主屏下面那块不存在的区域：离主屏底边最近
        assert_eq!(at_or_nearest(&m, 1200.0, 1700.0).unwrap().name, r"\\.\DISPLAY1");
        // 上下叠放：只看横坐标会认错
        let stacked = vec![mon("A", 0, 0, 1920, 1080, 1040, true), mon("B", 0, 1080, 1920, 2160, 2120, false)];
        assert_eq!(at_or_nearest(&stacked, 500.0, 1500.0).unwrap().name, "B");
        assert_eq!(at_or_nearest(&stacked, 500.0, 500.0).unwrap().name, "A");
    }

    #[test]
    fn clamps_the_pet_back_into_a_screen() {
        let m = layout();
        // 被扔到主屏下面的空隙里：收回主屏底边，竖直方向被挡
        let (x, y, hx, hy) = clamp_into(&m[0], 1200.0, 1700.0, 70.0, 156.0);
        assert_eq!((x, y, hx, hy), (1200.0, 1440.0, false, true));
        // 撞到左边：身体不出屏
        let (x, _, hx, _) = clamp_into(&m[0], 10.0, 800.0, 70.0, 156.0);
        assert_eq!((x, hx), (70.0, true));
        // 头不顶出屏幕
        let (_, y, _, hy) = clamp_into(&m[1], -500.0, 50.0, 70.0, 156.0);
        assert_eq!((y, hy), (156.0, true));
    }

    #[test]
    fn names_and_positions() {
        let m = layout();
        assert_eq!(number(r"\\.\DISPLAY12"), Some(12));
        assert_eq!(label(&m[0], 0), "显示器 1（2560×1440，主）");
        assert_eq!(label(&m[1], 1), "显示器 2（1080×1920）");
        assert_eq!(x_ratio(&m[0], 1280.0), 0.5);
        assert_eq!(x_at(&m[1], 0.5, 70.0), -540.0);
        assert_eq!(x_at(&m[1], 0.0, 70.0), -1010.0);
        assert!(distance(&m[0], 100.0, 100.0) == 0.0 && distance(&m[0], -100.0, 100.0) == 100.0);
    }
}
