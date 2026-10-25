//! 【留接口】桌宠主动找你聊天、发提醒。
//!
//! 结构：
//! 1. 若干个触发器（[`ProactiveTrigger`]）每 30 秒被轮询一次，看当前情况（[`Context`]）决定要不要开口，给出一个"念头"（[`Intent`]）；
//! 2. 念头交给 [`compose`] 变成要说的话（以后：用当前人格的口吻让模型写一两句）；
//! 3. 说的话统一交给 `speak::say`（气泡 / 系统通知 / 以后的 TTS）。
//!
//! 现在：
//! - 已有：接口、上下文、三种基础触发器（很久没聊天、每天定时、agent 事件后续提醒）和轮询循环；
//! - 默认关闭（`proactive.enabled`），没有设置界面；
//! - `compose` 还没实现：念头只记日志，桌宠不会真的开口。
//!
//! 打扰规则：桌宠隐藏、工作模式、对话进行中、正在录音、气泡开着时都不开口；两次主动开口至少隔 `min_interval_minutes`。

use crate::config::ProactiveParams;
use crate::AppState;
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, NaiveTime};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// 触发器判断时能看到的情况
#[derive(Clone, Debug)]
pub struct Context {
    pub now: DateTime<Local>,
    /// 多久没有键盘、鼠标操作了（秒）
    pub user_idle_secs: u64,
    /// 距上次和桌宠说话多久了（秒）；从没聊过时为 None
    pub since_last_chat_secs: Option<u64>,
    /// 现在适不适合开口（见模块说明的打扰规则）
    pub can_disturb: bool,
}

/// 一个"想说点什么"的念头
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    /// 哪个触发器给的
    pub trigger: &'static str,
    /// 为什么想说（给 compose 当素材，比如"用户两小时没说话了"、"到了 18:00 下班提醒"）
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentSignalKind {
    Done,
    NeedPermission,
    Failed,
}

/// agent 汇报时顺带发给触发器的消息
#[derive(Clone, Debug)]
pub struct AgentSignal {
    pub agent: &'static str,
    pub kind: AgentSignalKind,
    pub project: String,
}

/// 一种开口的理由
pub trait ProactiveTrigger: Send {
    fn id(&self) -> &'static str;
    /// 每 30 秒轮询一次（只在可以打扰时调用）；想开口就返回念头
    fn poll(&mut self, ctx: &Context) -> Option<Intent>;
    /// 收到 agent 汇报（不管当时能不能打扰都会调用）
    fn on_agent_signal(&mut self, _signal: &AgentSignal) {}
}

// ---------- 基础触发器 ----------

/// 用户在电脑前（最近有键鼠操作），但很久没和桌宠说话：找你聊两句。聊过之后才会再次触发
pub struct LongNoChat {
    pub after_secs: u64,
    /// 多少秒内有过操作才算"在电脑前"
    pub active_within_secs: u64,
    armed: bool,
}

impl LongNoChat {
    pub fn new(after_secs: u64) -> Self {
        Self { after_secs, active_within_secs: 120, armed: true }
    }
}

impl ProactiveTrigger for LongNoChat {
    fn id(&self) -> &'static str {
        "long-no-chat"
    }
    fn poll(&mut self, ctx: &Context) -> Option<Intent> {
        let since = ctx.since_last_chat_secs?;
        if since < self.after_secs {
            self.armed = true;
            return None;
        }
        if self.armed && ctx.user_idle_secs <= self.active_within_secs {
            self.armed = false;
            return Some(Intent { trigger: self.id(), reason: format!("用户 {} 分钟没和你说话了，但一直在用电脑", since / 60) });
        }
        None
    }
}

/// 每天到点提醒一次（到点后 30 分钟内，等到可以打扰时再说）
pub struct DailyAt {
    pub at: NaiveTime,
    pub label: String,
    last: Option<NaiveDate>,
}

impl DailyAt {
    pub fn new(at: NaiveTime, label: impl Into<String>) -> Self {
        Self { at, label: label.into(), last: None }
    }
}

impl ProactiveTrigger for DailyAt {
    fn id(&self) -> &'static str {
        "daily-at"
    }
    fn poll(&mut self, ctx: &Context) -> Option<Intent> {
        let today = ctx.now.date_naive();
        let t = ctx.now.time();
        let in_window = t >= self.at && t < self.at + ChronoDuration::minutes(30);
        if !in_window || self.last == Some(today) {
            return None;
        }
        self.last = Some(today);
        Some(Intent { trigger: self.id(), reason: format!("到了 {} 的提醒：{}", self.at.format("%H:%M"), self.label) })
    }
}

/// agent 有了结果（完成、等授权、出错）之后，等可以打扰时再提一次
pub struct AgentFollowUp {
    pub kinds: Vec<AgentSignalKind>,
    pending: VecDeque<AgentSignal>,
}

impl AgentFollowUp {
    pub fn new(kinds: Vec<AgentSignalKind>) -> Self {
        Self { kinds, pending: VecDeque::new() }
    }
}

impl ProactiveTrigger for AgentFollowUp {
    fn id(&self) -> &'static str {
        "agent-follow-up"
    }
    fn on_agent_signal(&mut self, s: &AgentSignal) {
        if self.kinds.contains(&s.kind) {
            // 同一个 agent 只留最新的一条
            self.pending.retain(|p| p.agent != s.agent);
            self.pending.push_back(s.clone());
        }
    }
    fn poll(&mut self, _ctx: &Context) -> Option<Intent> {
        let s = self.pending.pop_front()?;
        let what = match s.kind {
            AgentSignalKind::Done => "完成了任务",
            AgentSignalKind::NeedPermission => "在等授权",
            AgentSignalKind::Failed => "出错了",
        };
        let place = if s.project.is_empty() { String::new() } else { format!("（项目「{}」）", s.project) };
        Some(Intent { trigger: self.id(), reason: format!("{}{place}{what}，提醒用户去看看", s.agent) })
    }
}

/// 按配置建触发器
pub fn build_triggers(p: &ProactiveParams) -> Vec<Box<dyn ProactiveTrigger>> {
    let mut v: Vec<Box<dyn ProactiveTrigger>> = Vec::new();
    if let Some(m) = p.no_chat_minutes.filter(|m| *m > 0) {
        v.push(Box::new(LongNoChat::new(m * 60)));
    }
    for d in &p.daily {
        match NaiveTime::parse_from_str(d.at.trim(), "%H:%M") {
            Ok(at) => v.push(Box::new(DailyAt::new(at, d.label.clone()))),
            Err(_) => eprintln!("[proactive] 忽略写错的提醒时间：{}", d.at),
        }
    }
    if p.agent_follow_up {
        v.push(Box::new(AgentFollowUp::new(vec![AgentSignalKind::NeedPermission, AgentSignalKind::Failed])));
    }
    v
}

// ---------- 运行 ----------

struct Runtime {
    /// 建触发器时用的配置（变了就重建）
    params: Option<String>,
    triggers: Vec<Box<dyn ProactiveTrigger>>,
    last_spoken: Option<Instant>,
}

static RUNTIME: Mutex<Runtime> = Mutex::new(Runtime { params: None, triggers: Vec::new(), last_spoken: None });

const POLL_EVERY: Duration = Duration::from_secs(30);

/// 启动轮询循环（关着时只是空转）
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(POLL_EVERY).await;
            if let Some(intent) = tick(&app) {
                if let Some(text) = compose(&app, &intent).await {
                    RUNTIME.lock().unwrap().last_spoken = Some(Instant::now());
                    let title = app.state::<AppState>().config.lock().unwrap().active_persona().name;
                    crate::speak::say(&app, &title, &text, crate::speak::Source::Proactive, false);
                }
            }
        }
    });
}

/// 轮询一次：关着就清空触发器；能打扰、没在冷却时，问一圈触发器
fn tick(app: &AppHandle) -> Option<Intent> {
    let p = app.state::<AppState>().config.lock().unwrap().proactive.clone();
    let mut rt = RUNTIME.lock().unwrap();
    if !p.enabled {
        rt.triggers.clear();
        rt.params = None;
        return None;
    }
    let key = serde_json::to_string(&p).unwrap_or_default();
    if rt.params.as_deref() != Some(key.as_str()) {
        rt.triggers = build_triggers(&p);
        rt.params = Some(key);
    }
    if rt.last_spoken.is_some_and(|t| t.elapsed() < Duration::from_secs(p.min_interval_minutes * 60)) {
        return None;
    }
    let ctx = context(app);
    if !ctx.can_disturb {
        return None;
    }
    rt.triggers.iter_mut().find_map(|t| t.poll(&ctx))
}

/// 收集当前情况
pub fn context(app: &AppHandle) -> Context {
    let state = app.state::<AppState>();
    let (hidden, work_mode, bubble) = {
        let s = state.shared.lock().unwrap();
        (s.pet_hidden, s.work_mode, s.bubble_visible)
    };
    let can_disturb = !hidden && !work_mode && !bubble && !crate::chat::is_busy(app) && !crate::voice::is_recording(app);
    let now = Local::now();
    let since_last_chat_secs = state.store.last_user_message_at().map(|t| (now.timestamp() - t).max(0) as u64);
    Context { now, user_idle_secs: user_idle_secs(), since_last_chat_secs, can_disturb }
}

/// agent 汇报时调用：转给各触发器（关着时什么都不做）
pub fn agent_signal(app: &AppHandle, signal: AgentSignal) {
    if !app.state::<AppState>().config.lock().unwrap().proactive.enabled {
        return;
    }
    for t in RUNTIME.lock().unwrap().triggers.iter_mut() {
        t.on_agent_signal(&signal);
    }
}

/// 【待实现】把念头变成一两句话。
/// 打算：像 agents::persona_summary 那样，把人格的 system prompt + 念头的 reason 交给辅助模型，
/// 让它用人格的口吻说一两句（不超过 40 字）；用户关掉"允许发给云端模型"时只用模板话术。
/// 现在只记日志，返回 None，所以桌宠不会真的开口。
async fn compose(_app: &AppHandle, intent: &Intent) -> Option<String> {
    eprintln!("[proactive] 念头（{}）：{}——compose 还没实现，先不说", intent.trigger, intent.reason);
    None
}

/// 多久没有键盘、鼠标操作了（秒）
#[cfg(windows)]
fn user_idle_secs() -> u64 {
    use windows::Win32::System::SystemInformation::GetTickCount64;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    if unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        // dwTime 是 32 位的开机毫秒数，约 49.7 天回绕一次，按低 32 位相减
        let now = unsafe { GetTickCount64() } as u32;
        u64::from(now.wrapping_sub(info.dwTime)) / 1000
    } else {
        0
    }
}

#[cfg(not(windows))]
fn user_idle_secs() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ctx(h: u32, m: u32, idle: u64, since_chat: Option<u64>) -> Context {
        Context { now: Local.with_ymd_and_hms(2026, 10, 25, h, m, 0).unwrap(), user_idle_secs: idle, since_last_chat_secs: since_chat, can_disturb: true }
    }

    #[test]
    fn long_no_chat_fires_once_while_user_is_active() {
        let mut t = LongNoChat::new(3600);
        assert!(t.poll(&ctx(10, 0, 5, Some(100))).is_none()); // 刚聊过
        assert!(t.poll(&ctx(11, 0, 600, Some(3700))).is_none()); // 够久了但人不在电脑前
        assert!(t.poll(&ctx(11, 1, 10, Some(3760))).is_some()); // 人回来了：开口
        assert!(t.poll(&ctx(11, 2, 10, Some(3820))).is_none()); // 不会一直说
        assert!(t.poll(&ctx(12, 0, 10, Some(30))).is_none()); // 聊过之后重新计时
        assert!(t.poll(&ctx(13, 1, 10, Some(3700))).is_some());
        assert!(t.poll(&ctx(13, 2, 10, None)).is_none()); // 从没聊过：不打扰
    }

    #[test]
    fn daily_at_fires_once_per_day_within_window() {
        let mut t = DailyAt::new(NaiveTime::from_hms_opt(18, 0, 0).unwrap(), "下班啦");
        assert!(t.poll(&ctx(17, 59, 0, None)).is_none());
        let i = t.poll(&ctx(18, 5, 0, None)).unwrap();
        assert!(i.reason.contains("18:00") && i.reason.contains("下班啦"));
        assert!(t.poll(&ctx(18, 10, 0, None)).is_none()); // 当天只说一次
        assert!(t.poll(&ctx(18, 40, 0, None)).is_none()); // 过了窗口
    }

    #[test]
    fn agent_follow_up_keeps_latest_per_agent_and_filters_kinds() {
        let mut t = AgentFollowUp::new(vec![AgentSignalKind::NeedPermission]);
        let sig = |agent, kind| AgentSignal { agent, kind, project: "perch".into() };
        t.on_agent_signal(&sig("Codex", AgentSignalKind::Done)); // 不关心的类型
        t.on_agent_signal(&sig("Codex", AgentSignalKind::NeedPermission));
        t.on_agent_signal(&sig("Codex", AgentSignalKind::NeedPermission));
        let c = ctx(10, 0, 0, None);
        let i = t.poll(&c).unwrap();
        assert!(i.reason.contains("Codex") && i.reason.contains("等授权"));
        assert!(t.poll(&c).is_none());
    }

    #[test]
    fn builds_triggers_from_config_and_skips_bad_times() {
        let mut p = ProactiveParams::default();
        p.no_chat_minutes = None;
        assert!(build_triggers(&p).is_empty());
        p.no_chat_minutes = Some(90);
        p.daily = vec![
            crate::config::DailyReminder { at: "18:00".into(), label: "下班".into() },
            crate::config::DailyReminder { at: "25:99".into(), label: "写错的".into() },
        ];
        p.agent_follow_up = true;
        let ids: Vec<_> = build_triggers(&p).iter().map(|t| t.id()).collect();
        assert_eq!(ids, vec!["long-no-chat", "daily-at", "agent-follow-up"]);
    }
}
