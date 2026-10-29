pub mod controller;
pub mod fullscreen;
pub mod hittest;
pub mod surfaces;
pub mod win;

use hittest::Masks;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 前端加载皮肤后上报的尺寸信息
#[derive(Clone, Debug)]
pub struct Metrics {
    pub cell_w: f64,
    pub cell_h: f64,
    /// 脚底在格子里的相对高度（0 顶 ~ 1 底）
    pub foot_ratio: f64,
    /// 身体中心在格子里的相对横坐标
    pub center_ratio: f64,
    /// 头顶在格子里的相对高度，气泡贴着它放
    pub top_ratio: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self { cell_w: 192.0, cell_h: 208.0, foot_ratio: 1.0, center_ratio: 0.5, top_ratio: 0.0 }
    }
}

/// 命令、托盘和控制线程之间共享的桌宠状态
#[derive(Default)]
pub struct Shared {
    /// 皮肤和掩码都就绪后才显示窗口
    pub ready: bool,
    /// 当前皮肤有"坐"的动画（skin.json 里定义了 sit）：有才会坐在窗沿上
    pub can_sit: bool,
    pub masks: Masks,
    pub frame: usize,
    pub metrics: Metrics,
    /// 前端收到 mousedown，等控制线程判定是单击还是拖拽
    pub pointer_pressed: bool,
    pub work_mode: bool,
    pub hide_on_fullscreen: bool,
    pub user_hidden: bool,
    /// 托盘里点了"放回任务栏"
    pub reset_requested: bool,
    /// 对话时覆盖动画："think"（等模型）/"talk"（出字中）
    pub speech: Option<&'static str>,
    /// speech 到这个时间后自动清除；None 表示一直保持
    pub speech_until: Option<Instant>,
    pub bubble_visible: bool,
    /// 气泡窗口的物理像素尺寸
    pub bubble_size: (i32, i32),
    /// 气泡刚打开或尺寸变了，需要重新定位
    pub bubble_moved: bool,
    /// 所有 agent 会话合并出的动画（working / think / attention / error / done）
    pub agent_anim: Option<&'static str>,
    /// 桌宠当前被隐藏（用户手动隐藏或全屏），汇报时只发系统通知
    pub pet_hidden: bool,
    /// 桌宠所在显示器上有全屏程序（游戏、视频、放映），不管设置里要不要因此隐藏
    pub fullscreen: bool,
    /// agent 状态标签窗口的物理像素尺寸；(0, 0) 表示没有要显示的标签
    pub status_size: (i32, i32),
}

pub type SharedState = Arc<Mutex<Shared>>;
