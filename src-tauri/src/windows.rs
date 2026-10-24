//! 气泡、聊天、设置三个窗口的显示与定位。气泡跟随桌宠，位置由控制线程每帧更新。

use crate::AppState;
use serde::Serialize;
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewWindow};

pub const BUBBLE: &str = "bubble";
pub const CHAT: &str = "chat";
pub const SETTINGS: &str = "settings";
/// agent 状态标签：跟着桌宠、点击穿透、不抢焦点
pub const STATUS: &str = "status";

/// 告诉气泡窗口小尾巴该画在哪
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BubbleAnchor {
    /// "above"：气泡在头顶，尾巴朝下；"side"：放不下时放在侧面，不画尾巴
    pub placement: &'static str,
    /// 尾巴在气泡里的横向位置（0 左 ~ 1 右）
    pub tail_ratio: f64,
}

/// 计算气泡左上角（物理像素）。pet：桌宠窗口矩形；head_y：头顶位置；work：所在显示器工作区。
pub fn place_bubble(
    pet: (i32, i32, i32, i32),
    cx: f64,
    head_y: f64,
    size: (i32, i32),
    work: (i32, i32, i32, i32),
) -> ((i32, i32), &'static str) {
    let (pl, _pt, pw, _ph) = pet;
    let (bw, bh) = size;
    let (wl, wt, wr, wb) = work;
    let clamp_x = |x: i32| x.clamp(wl + 4, (wr - 4 - bw).max(wl + 4));
    let above_y = head_y as i32 - bh + 4;
    if above_y >= wt + 4 {
        return ((clamp_x(cx as i32 - bw / 2), above_y), "above");
    }
    // 头顶放不下：放到右侧，右侧也不够就放左侧
    let y = (head_y as i32).clamp(wt + 4, (wb - bh - 4).max(wt + 4));
    let right = pl + (pw as f64 * 0.8) as i32;
    let x = if right + bw <= wr - 4 { right } else { pl + (pw as f64 * 0.2) as i32 - bw };
    ((clamp_x(x), y), "side")
}

/// 计算状态标签左上角（物理像素）。放在桌宠上半身的侧面，和头顶的对话气泡错开。
/// cx：身体中心；head_y / foot_y：头顶和脚底；half_body：身体半宽；right：上次放在右侧（尽量不左右跳）。
/// 返回位置和这次是否放在右侧。
pub fn place_status(
    cx: f64,
    head_y: f64,
    foot_y: f64,
    half_body: f64,
    size: (i32, i32),
    work: (i32, i32, i32, i32),
    right: bool,
) -> ((i32, i32), bool) {
    let (sw, sh) = size;
    let (wl, wt, wr, wb) = work;
    let right_x = (cx + half_body) as i32;
    let left_x = (cx - half_body) as i32 - sw;
    let fits_right = right_x + sw <= wr - 4;
    let fits_left = left_x >= wl + 4;
    let right = if right { fits_right || !fits_left } else { !fits_left && fits_right };
    let x = if right { right_x } else { left_x };
    // 第一个标签和脸差不多高，多了往下排；不超出工作区
    let y = (head_y + (foot_y - head_y) * 0.22) as i32;
    let y = y.clamp(wt + 4, (wb - sh - 4).max(wt + 4));
    ((x.clamp(wl + 4, (wr - 4 - sw).max(wl + 4)), y), right)
}

/// 前端上报标签区域的逻辑尺寸；宽或高为 0 表示没有要显示的标签
pub fn resize_status(app: &AppHandle, width: f64, height: f64) {
    let state = app.state::<AppState>();
    if width <= 0.0 || height <= 0.0 {
        state.shared.lock().unwrap().status_size = (0, 0);
        return;
    }
    let Some(w) = app.get_webview_window(STATUS) else { return };
    let scale = w.scale_factor().unwrap_or(1.0);
    let _ = w.set_size(LogicalSize::new(width, height));
    state.shared.lock().unwrap().status_size = ((width * scale).round() as i32, (height * scale).round() as i32);
}

fn show_and_focus(w: &WebviewWindow) {
    let _ = w.show();
    let _ = w.unminimize();
    let _ = w.set_focus();
}

/// mode："input" 弹出输入气泡；"speak" 桌宠主动说话（不抢焦点）
pub fn open_bubble(app: &AppHandle, mode: &str) {
    let Some(w) = app.get_webview_window(BUBBLE) else { return };
    {
        let state = app.state::<AppState>();
        let mut s = state.shared.lock().unwrap();
        s.bubble_visible = true;
        s.bubble_moved = true;
    }
    let _ = app.emit_to(BUBBLE, "bubble-mode", mode);
    let _ = w.show();
    if mode == "input" {
        let _ = w.set_focus();
    }
}

pub fn close_bubble(app: &AppHandle) {
    app.state::<AppState>().shared.lock().unwrap().bubble_visible = false;
    if let Some(w) = app.get_webview_window(BUBBLE) {
        let _ = w.hide();
    }
}

pub fn toggle_bubble(app: &AppHandle) {
    let visible = app.state::<AppState>().shared.lock().unwrap().bubble_visible;
    if visible {
        close_bubble(app);
    } else {
        open_bubble(app, "input");
    }
}

/// 气泡内容变化后由前端上报逻辑尺寸
pub fn resize_bubble(app: &AppHandle, width: f64, height: f64) {
    let Some(w) = app.get_webview_window(BUBBLE) else { return };
    let scale = w.scale_factor().unwrap_or(1.0);
    let _ = w.set_size(LogicalSize::new(width, height));
    let state = app.state::<AppState>();
    let mut s = state.shared.lock().unwrap();
    s.bubble_size = ((width * scale).round() as i32, (height * scale).round() as i32);
    s.bubble_moved = true;
}

pub fn move_bubble(app: &AppHandle, pos: (i32, i32)) {
    if let Some(w) = app.get_webview_window(BUBBLE) {
        let _ = w.set_position(PhysicalPosition::new(pos.0, pos.1));
    }
}

/// 桌宠主动说一句话（agent 汇报等）：气泡关着就以"说话"模式打开，开着就在里面加一行
pub fn say(app: &AppHandle, text: &str) {
    let visible = app.state::<AppState>().shared.lock().unwrap().bubble_visible;
    if !visible {
        open_bubble(app, "speak");
    }
    let _ = app.emit_to(BUBBLE, "bubble-say", text);
}

pub fn open_chat(app: &AppHandle) {
    close_bubble(app);
    if let Some(w) = app.get_webview_window(CHAT) {
        show_and_focus(&w);
    }
}

pub fn open_settings(app: &AppHandle) {
    close_bubble(app);
    if let Some(w) = app.get_webview_window(SETTINGS) {
        show_and_focus(&w);
    }
}

#[cfg(test)]
mod tests {
    use super::{place_bubble, place_status};

    #[test]
    fn status_sits_beside_the_body_and_flips_at_the_edge() {
        let work = (0, 0, 1920, 1040);
        // 默认在右侧，和脸差不多高
        let ((x, y), right) = place_status(900.0, 900.0, 1040.0, 40.0, (200, 30), work, true);
        assert!(right);
        assert_eq!(x, 940);
        assert!(y > 900 && y < 960);
        // 右边放不下：换到左侧
        let ((x, _), right) = place_status(1850.0, 900.0, 1040.0, 40.0, (200, 30), work, true);
        assert!(!right);
        assert_eq!(x, 1850 - 40 - 200);
        // 上次在左侧、两边都放得下：留在左侧，不来回跳
        let (_, right) = place_status(900.0, 900.0, 1040.0, 40.0, (200, 30), work, false);
        assert!(!right);
        // 标签很高：不超出工作区底部
        let ((_, y), _) = place_status(900.0, 900.0, 1040.0, 40.0, (200, 200), work, true);
        assert_eq!(y, 1040 - 200 - 4);
    }

    #[test]
    fn bubble_goes_above_and_clamps_to_screen() {
        let work = (0, 0, 1920, 1040);
        // 头顶有空间：居中放在上方
        let ((x, y), p) = place_bubble((900, 800, 144, 156), 972.0, 820.0, (300, 120), work);
        assert_eq!(p, "above");
        assert_eq!((x, y), (822, 704));
        // 靠近左边缘：被夹在屏幕内
        let ((x, _), _) = place_bubble((0, 800, 144, 156), 40.0, 820.0, (300, 120), work);
        assert_eq!(x, 4);
        // 头顶放不下：放到侧面
        let ((x, y), p) = place_bubble((900, 0, 144, 156), 972.0, 30.0, (300, 120), work);
        assert_eq!(p, "side");
        assert!(x >= 1000 && y >= 4);
    }
}
