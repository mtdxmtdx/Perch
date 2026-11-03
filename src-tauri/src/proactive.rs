//! 桌宠主动找你聊天、发提醒。
//!
//! 流程：
//! 1. 若干个触发器（[`ProactiveTrigger`]）每 30 秒被轮询一次，看当前情况（[`Context`]）决定要不要开口，给出一个"念头"（[`Intent`]）；
//! 2. [`compose`] 把念头写成一两句话：默认让辅助模型用当前人格的口吻写，没有模型或失败时用触发器给的模板；
//! 3. 这句话记进当前人格的对话（用户回复时模型知道自己刚说了什么），再交给 `speak::say` 说出来。
//!
//! 打扰规则（[`allowed`]）：
//! - 对话进行中、正在录音、气泡开着（`busy`）或用户手动隐藏时什么都不说；
//! - 静默时按原因放行：开会/通话一律不说；全屏 / 在看只放行对应白名单里的重要提醒
//!   （全屏时桌宠已隐藏，走系统通知）；白名单里的闲聊 id 也不放行；
//! - 关掉桌面状态后，全屏自动隐藏仍什么都不说（`pet_hidden`）；
//! - 专注时闲聊不说，只说重要的和鼓励；工作模式、免打扰时段、冷却期内只说重要的。

use crate::config::ProactiveParams;
use crate::AppState;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveTime, Timelike};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// 触发器判断时能看到的情况
#[derive(Clone, Debug)]
pub struct Context {
    pub now: DateTime<Local>,
    /// 多久没有键盘、鼠标操作了（秒）
    pub user_idle_secs: u64,
    /// 距上次和桌宠说话多久了（秒）；从没聊过时为 None
    pub since_last_chat_secs: Option<u64>,
    /// 气泡开着、对话中、录音中（原 can_disturb 去掉「桌宠隐藏」）
    pub busy: bool,
    /// 用户手动隐藏桌宠
    pub user_hidden: bool,
    /// 手动隐藏或全屏自动隐藏
    pub pet_hidden: bool,
    /// 安静：工作模式或免打扰时段，只说重要的
    pub quiet: bool,
    /// 还在等你授权的 agent
    pub agents_waiting: Vec<&'static str>,
    /// 桌面上在干什么（对外仍是三态）
    pub mode: DesktopMode,
    /// 为什么静默（仅 mode == Silent 时有）
    pub silent: Option<SilentReason>,
    /// 多久不在电脑前：在看时为 0，否则等于 user_idle_secs
    pub away_secs: u64,
    /// 前台进程名（不带 .exe）；主动看屏/标题素材用
    pub fg_process: Option<String>,
    /// 前台窗口标题（本地用；上传受 include_title + 门禁约束）
    pub fg_title: Option<String>,
}

/// 桌面上在干什么（按前台程序与廉价信号判断）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopMode {
    Normal,
    /// 专注：前台是编辑器、终端，或者 agent 在干活。闲聊不说，只说重要的和鼓励
    Focus,
    /// 静默：开会/通话、全屏、在看视频。按 [`SilentReason`] 决定白名单
    Silent,
}

/// 为什么静默（mode == Silent 时才有）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SilentReason {
    Meeting,
    Fullscreen,
    Watching,
}

/// 判断桌面状态。优先级：开会 > 全屏 > 在看 > 专注 > 普通。
/// fg：前台程序名（不带 .exe）；agents_busy：有 agent 在思考或干活；
/// in_call / watching：廉价信号（调用方在 desktop_modes 关闭时传 false）。
pub fn desktop_mode(
    p: &ProactiveParams,
    fullscreen: bool,
    fg: Option<&str>,
    agents_busy: bool,
    in_call: bool,
    watching: bool,
) -> (DesktopMode, Option<SilentReason>) {
    if !p.desktop_modes {
        return (DesktopMode::Normal, None);
    }
    let listed = |list: &[String]| fg.is_some_and(|f| list.iter().any(|a| a.trim().trim_end_matches(".exe").eq_ignore_ascii_case(f)));
    if in_call || listed(&p.silent_apps) {
        (DesktopMode::Silent, Some(SilentReason::Meeting))
    } else if fullscreen {
        (DesktopMode::Silent, Some(SilentReason::Fullscreen))
    } else if watching {
        (DesktopMode::Silent, Some(SilentReason::Watching))
    } else if agents_busy || listed(&p.focus_apps) {
        (DesktopMode::Focus, None)
    } else {
        (DesktopMode::Normal, None)
    }
}

/// 一个"想说点什么"的念头
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    /// 哪个触发器给的
    pub trigger: &'static str,
    /// 重要提醒（影响 NO_SPEAK 回退模板）
    pub important: bool,
    /// 为什么想说（给模型当素材）
    pub reason: String,
    /// 不用模型、或模型失败时直接说的话
    pub fallback: String,
    /// screen-glance：规则命中直接看；变化检测需再打分
    pub glance_kind: Option<GlanceKind>,
}

/// 主动看屏意图子类
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlanceKind {
    /// 进程/标题规则命中
    Rule,
    /// 窗口切走后做一次变化比对
    ChangeCheck,
}


fn intent(trigger: &'static str, important: bool, reason: impl Into<String>, fallback: impl Into<String>) -> Intent {
    Intent {
        trigger,
        important,
        reason: reason.into(),
        fallback: fallback.into(),
        glance_kind: None,
    }
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
    pub at: DateTime<Local>,
}

/// 一种开口的理由
pub trait ProactiveTrigger: Send {
    fn id(&self) -> &'static str;
    /// 重要的提醒在安静时（工作模式、免打扰时段）和冷却期内也会说；闲聊不会
    fn important(&self) -> bool {
        false
    }
    /// 专注状态（写代码）时也可以说（比如鼓励）；重要的本来就可以
    fn focus_ok(&self) -> bool {
        false
    }
    /// 每 30 秒都会调用（不管能不能开口），给需要持续观察的触发器记录情况
    fn observe(&mut self, _ctx: &Context) {}
    /// 每 30 秒轮询一次（只在允许它开口时调用）；想开口就返回念头
    fn poll(&mut self, ctx: &Context) -> Option<Intent>;
    /// 收到 agent 汇报（不管当时能不能开口都会调用）
    fn on_agent_signal(&mut self, _signal: &AgentSignal) {}
    /// 要跨重启保存的状态（比如"今天问候过了"），没有就是 None
    fn saved(&self) -> Option<String> {
        None
    }
    fn restore(&mut self, _saved: &str) {}
}

/// 这个触发器现在能不能开口
pub fn allowed(t: &dyn ProactiveTrigger, ctx: &Context, p: &ProactiveParams, cooling: bool) -> bool {
    if ctx.busy || ctx.user_hidden {
        return false;
    }
    if let Some(why) = ctx.silent {
        let list = match why {
            SilentReason::Meeting => return false,
            SilentReason::Fullscreen => &p.fullscreen_allowed_triggers,
            SilentReason::Watching => &p.media_allowed_triggers,
        };
        // 白名单里写了闲聊 id 也不放行
        return t.important() && list.iter().any(|id| id == t.id());
    }
    if ctx.pet_hidden {
        // 关掉桌面状态时，全屏隐藏仍什么都不说（保持现状）
        return false;
    }
    if t.important() {
        return true;
    }
    !ctx.quiet && !cooling && (ctx.mode == DesktopMode::Normal || t.focus_ok())
}

fn minutes(secs: u64) -> u64 {
    secs / 60
}

// ---------- 触发器 ----------

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
        if self.armed && ctx.away_secs <= self.active_within_secs {
            self.armed = false;
            return Some(Intent {
                trigger: self.id(),
                important: self.important(),
                glance_kind: None,
                reason: format!("用户已经 {} 分钟没和你说话了，但一直在用电脑。找个轻松的话题打个招呼", minutes(since)),
                fallback: "在忙什么呀？要不要歇一会儿，陪我聊两句～".into(),
            });
        }
        None
    }
}

/// 用户自己设的每天定时提醒（到点后 30 分钟内，等到可以开口时再说）
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
    fn important(&self) -> bool {
        true
    }
    fn poll(&mut self, ctx: &Context) -> Option<Intent> {
        let today = ctx.now.date_naive();
        let t = ctx.now.time();
        let in_window = t >= self.at && t < self.at + ChronoDuration::minutes(30);
        if !in_window || self.last == Some(today) {
            return None;
        }
        self.last = Some(today);
        let at = self.at.format("%H:%M");
        let label = self.label.trim();
        Some(Intent {
            trigger: self.id(),
                important: self.important(),
                glance_kind: None,
            reason: format!("到了用户设的 {at} 的提醒：{}。提醒用户这件事", if label.is_empty() { "（没写内容）" } else { label }),
            fallback: if label.is_empty() { format!("到 {at} 啦～") } else { format!("提醒你：{label}") },
        })
    }
}

/// 连续用电脑太久没休息：提醒起来活动。中间离开电脑超过 break_secs 就重新计时
pub struct Sedentary {
    pub after_secs: u64,
    pub break_secs: u64,
    active_since: Option<DateTime<Local>>,
}

impl Sedentary {
    pub fn new(after_secs: u64) -> Self {
        Self { after_secs, break_secs: 300, active_since: None }
    }
}

impl ProactiveTrigger for Sedentary {
    fn id(&self) -> &'static str {
        "sedentary"
    }
    fn important(&self) -> bool {
        true
    }
    fn poll(&mut self, ctx: &Context) -> Option<Intent> {
        // 用 away_secs：看视频时不算离开，久坐继续计时
        if ctx.away_secs >= self.break_secs {
            self.active_since = None;
            return None;
        }
        let start = *self.active_since.get_or_insert(ctx.now - ChronoDuration::seconds(ctx.away_secs as i64));
        let used = (ctx.now - start).num_seconds().max(0) as u64;
        if used < self.after_secs {
            return None;
        }
        // 提醒过了：再连续这么久才会再提醒
        self.active_since = Some(ctx.now);
        let m = minutes(used);
        Some(Intent {
            trigger: self.id(),
                important: self.important(),
                glance_kind: None,
            reason: format!("用户已经连续用了 {m} 分钟电脑，中间没怎么休息。提醒用户起来活动、喝水、看看远处"),
            fallback: format!("已经连续坐了 {m} 分钟啦，起来活动一下、喝口水吧～"),
        })
    }
}

/// 到了睡觉时间用户还在用电脑：催睡。从就寝时间到早上 5 点之间，最多提醒 3 次，每次隔 30 分钟
pub struct Bedtime {
    pub at: NaiveTime,
    pub max_times: u32,
    pub every: ChronoDuration,
    night: Option<NaiveDate>,
    count: u32,
    last: Option<DateTime<Local>>,
}

impl Bedtime {
    pub fn new(at: NaiveTime) -> Self {
        Self { at, max_times: 3, every: ChronoDuration::minutes(30), night: None, count: 0, last: None }
    }

    /// 距就寝时间过去了多少分钟（跨午夜也算对），以及这次提醒窗口有多长（到早上 5 点，至少 1 小时）
    fn window(&self, t: NaiveTime) -> (i64, i64) {
        let day = 24 * 60;
        let mins = |x: NaiveTime| (x.hour() * 60 + x.minute()) as i64;
        let since = (mins(t) - mins(self.at)).rem_euclid(day);
        let len = (5 * 60 - mins(self.at)).rem_euclid(day).max(60);
        (since, len)
    }
}

impl ProactiveTrigger for Bedtime {
    fn id(&self) -> &'static str {
        "bedtime"
    }
    fn important(&self) -> bool {
        true
    }
    fn poll(&mut self, ctx: &Context) -> Option<Intent> {
        let (since, len) = self.window(ctx.now.time());
        if since >= len || ctx.away_secs > 120 {
            return None;
        }
        // 这一晚从哪天开始（过了午夜也算前一天晚上）
        let night = (ctx.now - ChronoDuration::minutes(since)).date_naive();
        if self.night != Some(night) {
            self.night = Some(night);
            self.count = 0;
            self.last = None;
        }
        if self.count >= self.max_times || self.last.is_some_and(|l| ctx.now - l < self.every) {
            return None;
        }
        self.count += 1;
        self.last = Some(ctx.now);
        let now = ctx.now.format("%H:%M");
        Some(Intent {
            trigger: self.id(),
                important: self.important(),
                glance_kind: None,
            reason: format!("已经 {now} 了，过了用户设的睡觉时间（{}），用户还在用电脑。温柔地催用户早点休息（这是今晚第 {} 次提醒）", self.at.format("%H:%M"), self.count),
            fallback: format!("已经 {now} 啦，早点休息吧～"),
        })
    }
}

/// agent 等你授权，报告过之后过了一会儿还在等：再提一次
pub struct AgentFollowUp {
    pub after: ChronoDuration,
    pending: Vec<AgentSignal>,
}

impl AgentFollowUp {
    pub fn new(after: ChronoDuration) -> Self {
        Self { after, pending: Vec::new() }
    }
}

impl ProactiveTrigger for AgentFollowUp {
    fn id(&self) -> &'static str {
        "agent-follow-up"
    }
    fn important(&self) -> bool {
        true
    }
    fn on_agent_signal(&mut self, s: &AgentSignal) {
        // 同一个 agent 只留最新的一条；不是等授权的消息（完成、出错）说明已经不用等了
        self.pending.retain(|p| p.agent != s.agent);
        if s.kind == AgentSignalKind::NeedPermission {
            self.pending.push(s.clone());
        }
    }
    fn poll(&mut self, ctx: &Context) -> Option<Intent> {
        // 已经不在等了（用户处理过了）的丢掉
        self.pending.retain(|p| ctx.agents_waiting.contains(&p.agent));
        let i = self.pending.iter().position(|p| ctx.now - p.at >= self.after)?;
        let s = self.pending.remove(i);
        let waited = (ctx.now - s.at).num_minutes();
        let place = if s.project.is_empty() { String::new() } else { format!("（项目「{}」）", s.project) };
        Some(Intent {
            trigger: self.id(),
                important: self.important(),
                glance_kind: None,
            reason: format!("{}{place}在等用户授权，已经等了 {waited} 分钟，用户还没去处理。提醒用户去看看", s.agent),
            fallback: format!("{} 还在等你授权哦，记得去看看～", s.agent),
        })
    }
}

/// 专注写代码（或者 agent 在干活）很久了：说句鼓励的话。离开专注状态超过 10 分钟就重新计时
pub struct Encourage {
    pub after_secs: u64,
    start: Option<DateTime<Local>>,
    last_focus: Option<DateTime<Local>>,
}

impl Encourage {
    pub fn new(after_secs: u64) -> Self {
        Self { after_secs, start: None, last_focus: None }
    }
}

impl ProactiveTrigger for Encourage {
    fn id(&self) -> &'static str {
        "encourage"
    }
    fn focus_ok(&self) -> bool {
        true
    }
    fn observe(&mut self, ctx: &Context) {
        if ctx.mode == DesktopMode::Focus && ctx.user_idle_secs < 300 {
            self.start.get_or_insert(ctx.now);
            self.last_focus = Some(ctx.now);
        } else if self.last_focus.is_some_and(|l| ctx.now - l > ChronoDuration::minutes(10)) {
            self.start = None;
            self.last_focus = None;
        }
    }
    fn poll(&mut self, ctx: &Context) -> Option<Intent> {
        // 只在还专注着的时候说
        if ctx.mode != DesktopMode::Focus {
            return None;
        }
        let used = (ctx.now - self.start?).num_seconds().max(0) as u64;
        if used < self.after_secs {
            return None;
        }
        self.start = Some(ctx.now);
        let m = minutes(used);
        Some(Intent {
            trigger: self.id(),
                important: self.important(),
                glance_kind: None,
            reason: format!(
                "用户已经专注写代码（或者和 AI 编程助手一起干活）{m} 分钟了。说一句简短真诚的鼓励或打气；\
不要劝休息（休息另有提醒），不要提问打断思路"
            ),
            fallback: format!("已经专注 {m} 分钟啦，进展不错吧，加油～"),
        })
    }
}

/// 离开电脑一阵子回来时打个招呼；每天早上第一次见面问早安。电脑睡眠（轮询停了）也算离开
pub struct WelcomeBack {
    pub away_secs: u64,
    /// 正在离开：已经离开了多少秒
    away: Option<u64>,
    /// 刚回来：离开了多少秒、什么时候回来的
    back: Option<(u64, DateTime<Local>)>,
    last_tick: Option<DateTime<Local>>,
    /// 哪天问过早安了
    greeted: Option<NaiveDate>,
}

impl WelcomeBack {
    pub fn new(away_secs: u64) -> Self {
        Self { away_secs, away: None, back: None, last_tick: None, greeted: None }
    }
}

fn duration_text(secs: u64) -> String {
    if secs >= 3600 {
        format!("{} 小时", (secs as f64 / 3600.0).round().max(1.0))
    } else {
        format!("{} 分钟", minutes(secs))
    }
}

impl ProactiveTrigger for WelcomeBack {
    fn id(&self) -> &'static str {
        "welcome-back"
    }
    fn observe(&mut self, ctx: &Context) {
        // 两次轮询隔了很久：电脑睡眠或休眠过；away_secs 在看时为 0，看完碰鼠标不会欢迎回来
        let gap = self.last_tick.map_or(0, |t| (ctx.now - t).num_seconds().max(0) as u64);
        self.last_tick = Some(ctx.now);
        let away_now = ctx.away_secs.max(gap);
        if away_now >= self.away_secs {
            self.away = Some(self.away.unwrap_or(0).max(away_now));
        }
        if ctx.away_secs < 60 {
            if let Some(secs) = self.away.take() {
                self.back = Some((secs, ctx.now));
            }
        }
    }
    fn poll(&mut self, ctx: &Context) -> Option<Intent> {
        // 回来超过 10 分钟还没轮到说，就不说了
        let back = self.back.take().filter(|(_, at)| ctx.now - *at <= ChronoDuration::minutes(10));
        let today = ctx.now.date_naive();
        if (5..11).contains(&ctx.now.hour()) && self.greeted != Some(today) && ctx.away_secs < 60 {
            self.greeted = Some(today);
            return Some(Intent {
                trigger: self.id(),
                important: self.important(),
                glance_kind: None,
                reason: "早上，今天第一次见到用户。道声早安，可以顺口关心一句今天的安排或者睡得好不好".into(),
                fallback: "早上好呀～今天也一起加油吧！".into(),
            });
        }
        let (secs, _) = back?;
        let gone = duration_text(secs);
        Some(Intent {
            trigger: self.id(),
                important: self.important(),
                glance_kind: None,
            reason: format!("用户离开电脑大约 {gone}，刚回来。欢迎用户回来，简短自然"),
            fallback: "欢迎回来～".into(),
        })
    }
    fn saved(&self) -> Option<String> {
        self.greeted.map(|d| d.to_string())
    }
    fn restore(&mut self, saved: &str) {
        self.greeted = saved.parse().ok();
    }
}

/// 节日、生日、纪念日：当天第一次见到用户时（8 点以后）问候一次
pub struct SpecialDay {
    pub holidays: bool,
    pub birthday: Option<(u32, u32)>,
    pub anniversaries: Vec<((u32, u32), String)>,
    last: Option<NaiveDate>,
}

/// "MM-DD" → (月, 日)
pub fn parse_month_day(s: &str) -> Option<(u32, u32)> {
    let (m, d) = s.trim().split_once('-')?;
    let (m, d) = (m.trim().parse().ok()?, d.trim().parse().ok()?);
    // 用闰年检查日期是否存在（2 月 29 日也算）
    NaiveDate::from_ymd_opt(2024, m, d).map(|_| (m, d))
}

/// 这一天是什么节日（公历节日和农历节日）
pub fn holidays_on(date: NaiveDate) -> Vec<&'static str> {
    use chinese_lunisolar_calendar::LunisolarDate;
    let mut out = Vec::new();
    let solar = match (date.month(), date.day()) {
        (1, 1) => Some("元旦"),
        (2, 14) => Some("情人节"),
        (5, 1) => Some("劳动节"),
        (6, 1) => Some("儿童节"),
        (10, 1) => Some("国庆节"),
        (12, 24) => Some("平安夜"),
        (12, 25) => Some("圣诞节"),
        (12, 31) => Some("跨年夜"),
        _ => None,
    };
    out.extend(solar);
    // 农历：闰月不算；除夕是正月初一的前一天
    let lunar = |d: NaiveDate| {
        LunisolarDate::from_date(d)
            .ok()
            .filter(|l| !l.to_lunar_month().is_leap_month())
            .map(|l| (l.to_lunar_month().to_u8(), l.to_lunar_day().to_u8()))
    };
    if date.succ_opt().and_then(lunar) == Some((1, 1)) {
        out.push("除夕");
    }
    let lunar_name = match lunar(date) {
        Some((1, 1)) => Some("春节"),
        Some((1, 15)) => Some("元宵节"),
        Some((5, 5)) => Some("端午节"),
        Some((7, 7)) => Some("七夕"),
        Some((8, 15)) => Some("中秋节"),
        Some((9, 9)) => Some("重阳节"),
        _ => None,
    };
    out.extend(lunar_name);
    out
}

impl SpecialDay {
    pub fn new(holidays: bool, birthday: Option<(u32, u32)>, anniversaries: Vec<((u32, u32), String)>) -> Self {
        Self { holidays, birthday, anniversaries, last: None }
    }

    /// 这一天要问候什么：(给模型的说明, 模板话术)
    fn events(&self, date: NaiveDate) -> Vec<(String, String)> {
        let md = (date.month(), date.day());
        let mut out = Vec::new();
        if self.birthday == Some(md) {
            out.push(("今天是用户的生日".to_string(), "生日快乐！今天要开开心心的～".to_string()));
        }
        for (d, label) in &self.anniversaries {
            if *d == md {
                out.push((format!("今天是用户记下的纪念日「{label}」"), format!("今天是「{label}」呢～")));
            }
        }
        if self.holidays {
            for h in holidays_on(date) {
                out.push((format!("今天是{h}"), format!("{h}快乐～")));
            }
        }
        out
    }
}

impl ProactiveTrigger for SpecialDay {
    fn id(&self) -> &'static str {
        "special-day"
    }
    fn important(&self) -> bool {
        true
    }
    fn poll(&mut self, ctx: &Context) -> Option<Intent> {
        let today = ctx.now.date_naive();
        if self.last == Some(today) || ctx.now.hour() < 8 || ctx.user_idle_secs > 120 {
            return None;
        }
        let events = self.events(today);
        let (_, fallback) = events.first()?.clone();
        self.last = Some(today);
        let what: Vec<String> = events.into_iter().map(|(r, _)| r).collect();
        Some(Intent {
            trigger: self.id(),
                important: self.important(),
                glance_kind: None,
            reason: format!("{}。向用户送上一两句应景的祝福或问候", what.join("；")),
            fallback,
        })
    }
    fn saved(&self) -> Option<String> {
        self.last.map(|d| d.to_string())
    }
    fn restore(&mut self, saved: &str) {
        self.last = saved.parse().ok();
    }
}


// ---------- 主动看屏 screen-glance（闲聊） ----------

/// 变化不显著时，同一窗口切换场景至多每 5 分钟再试一次
const GLANCE_CHANGE_RETRY_SECS: u64 = 5 * 60;

/// 主动看屏：规则命中或画面显著变化时，在写话阶段截一帧当素材
pub struct ScreenGlance {
    pub min_interval_secs: u64,
    pub daily_cap: u32,
    pub rules: Vec<crate::config::VisionProcessRule>,
    pub on_change: bool,
}

impl ScreenGlance {
    pub fn new(a: &crate::config::AwarenessParams) -> Self {
        Self {
            min_interval_secs: a.vision_min_interval_minutes.max(1) * 60,
            daily_cap: a.vision_daily_cap.max(1),
            rules: a.vision_process_rules.clone(),
            on_change: a.vision_on_significant_change,
        }
    }
}

impl ProactiveTrigger for ScreenGlance {
    fn id(&self) -> &'static str {
        "screen-glance"
    }
    fn poll(&mut self, ctx: &Context) -> Option<Intent> {
        let today = ctx.now.format("%Y-%m-%d").to_string();
        {
            let mut st = GLANCE_STATE.lock().unwrap();
            if st.day != today {
                st.day = today.clone();
                st.glances_today = 0;
            }
        }
        let st = GLANCE_STATE.lock().unwrap();
        if !glance_caps_allow(
            st.glances_today,
            self.daily_cap,
            st.last_success.map(|t| t.elapsed().as_secs()),
            self.min_interval_secs,
        ) {
            return None;
        }
        let proc = ctx.fg_process.clone().unwrap_or_default();
        let title = ctx.fg_title.clone().unwrap_or_default();
        if proc.is_empty() {
            return None;
        }
        if glance_rule_hit(&self.rules, &proc, &title) {
            return Some(Intent {
                trigger: self.id(),
                important: false,
                glance_kind: Some(GlanceKind::Rule),
                reason: format!(
                    "看了一眼用户正在用的「{proc}」窗口，根据画面内容打招呼或接一句；不要复述隐私内容"
                ),
                fallback: "你在忙什么呀～".into(),
            });
        }
        if self.on_change {
            let changed = proc != st.last_proc || title != st.last_title;
            if changed {
                let retry_ok = st
                    .last_change_try
                    .map(|t| t.elapsed() >= Duration::from_secs(GLANCE_CHANGE_RETRY_SECS))
                    .unwrap_or(true);
                if retry_ok || st.last_hsv.is_none() {
                    return Some(Intent {
                        trigger: self.id(),
                        important: false,
                        glance_kind: Some(GlanceKind::ChangeCheck),
                        reason: format!(
                            "用户刚切到「{proc}」，看一眼画面有没有值得接一句的变化；不要复述隐私内容"
                        ),
                        fallback: "换了个窗口呀～".into(),
                    });
                }
            }
        }
        None
    }
    fn saved(&self) -> Option<String> {
        let st = GLANCE_STATE.lock().unwrap();
        Some(format!("{}:{}", st.day, st.glances_today))
    }
    fn restore(&mut self, saved: &str) {
        if let Some((d, n)) = saved.split_once(':') {
            let mut st = GLANCE_STATE.lock().unwrap();
            st.day = d.to_string();
            st.glances_today = n.parse().unwrap_or(0);
        }
    }
}

/// 纯逻辑：主动看屏间隔与每日上限（供单测）
pub fn glance_caps_allow(
    glances_today: u32,
    daily_cap: u32,
    secs_since_last: Option<u64>,
    min_interval_secs: u64,
) -> bool {
    if glances_today >= daily_cap {
        return false;
    }
    match secs_since_last {
        Some(s) if s < min_interval_secs => false,
        _ => true,
    }
}

/// 规则是否命中（进程名小写比对；标题关键字本地包含）
pub fn glance_rule_hit(rules: &[crate::config::VisionProcessRule], proc: &str, title: &str) -> bool {
    let proc_n = proc.trim().trim_end_matches(".exe").to_lowercase();
    for r in rules {
        let rp = r.process.trim().trim_end_matches(".exe").to_lowercase();
        if rp.is_empty() || rp != proc_n {
            continue;
        }
        match r.title_keyword.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => return true,
            Some(k) if title.to_lowercase().contains(&k.to_lowercase()) => return true,
            Some(_) => {}
        }
    }
    false
}

/// 按配置建触发器（顺序就是优先级：重要的在前）
pub fn build_triggers(p: &ProactiveParams, a: &crate::config::AwarenessParams) -> Vec<Box<dyn ProactiveTrigger>> {
    let time = |s: &str| NaiveTime::parse_from_str(s.trim(), "%H:%M").ok();
    let mut v: Vec<Box<dyn ProactiveTrigger>> = Vec::new();
    for d in &p.daily {
        match time(&d.at) {
            Some(at) => v.push(Box::new(DailyAt::new(at, d.label.clone()))),
            None => eprintln!("[proactive] 忽略写错的提醒时间：{}", d.at),
        }
    }
    let birthday = p.birthday.as_deref().and_then(parse_month_day);
    let anniversaries: Vec<_> = p
        .anniversaries
        .iter()
        .filter_map(|a| parse_month_day(&a.date).map(|d| (d, a.label.trim().to_string())))
        .collect();
    if p.holidays || birthday.is_some() || !anniversaries.is_empty() {
        v.push(Box::new(SpecialDay::new(p.holidays, birthday, anniversaries)));
    }
    if let Some(at) = p.bedtime.as_deref().and_then(time) {
        v.push(Box::new(Bedtime::new(at)));
    }
    if p.agent_follow_up {
        v.push(Box::new(AgentFollowUp::new(ChronoDuration::minutes(5))));
    }
    if let Some(m) = p.sedentary_minutes.filter(|m| *m > 0) {
        v.push(Box::new(Sedentary::new(m * 60)));
    }
    if let Some(m) = p.welcome_back_minutes.filter(|m| *m > 0) {
        v.push(Box::new(WelcomeBack::new(m * 60)));
    }
    if let Some(m) = p.encourage_minutes.filter(|m| *m > 0) {
        v.push(Box::new(Encourage::new(m * 60)));
    }
    if let Some(m) = p.no_chat_minutes.filter(|m| *m > 0) {
        v.push(Box::new(LongNoChat::new(m * 60)));
    }
    // 主动看屏：需开关 + 已授权；有能看图的模型在 speak 阶段再校验
    if a.proactive_vision && a.vision_authorized {
        v.push(Box::new(ScreenGlance::new(a)));
    }
    v
}

/// 现在是不是免打扰时段（可以跨午夜，比如 23:30～07:30；开始等于结束视为没设）
pub fn in_quiet_hours(p: &ProactiveParams, now: NaiveTime) -> bool {
    let parse = |s: &Option<String>| s.as_deref().and_then(|s| NaiveTime::parse_from_str(s.trim(), "%H:%M").ok());
    match (parse(&p.quiet_start), parse(&p.quiet_end)) {
        (Some(a), Some(b)) if a < b => now >= a && now < b,
        (Some(a), Some(b)) if a > b => now >= a || now < b,
        _ => false,
    }
}

// ---------- 运行 ----------

struct Runtime {
    /// 建触发器时用的配置（变了就重建）
    params: Option<String>,
    triggers: Vec<Box<dyn ProactiveTrigger>>,
    last_spoken: Option<Instant>,
    /// 上次的桌面状态（变了打一行日志，方便排查）
    last_mode: Option<DesktopMode>,
}

static RUNTIME: Mutex<Runtime> = Mutex::new(Runtime { params: None, triggers: Vec::new(), last_spoken: None, last_mode: None });

const POLL_EVERY: Duration = Duration::from_secs(30);

/// 启动轮询循环（关着时只是空转）
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(POLL_EVERY).await;
            if let Some(intent) = tick(&app) {
                speak_intent(&app, &intent).await;
            }
        }
    });
}

/// 轮询一次：关着就清空触发器；按打扰规则问允许开口的触发器
fn tick(app: &AppHandle) -> Option<Intent> {
    let (p, a) = {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap();
        (cfg.proactive.clone(), cfg.awareness.clone())
    };
    let mut rt = RUNTIME.lock().unwrap();
    if !p.enabled {
        rt.triggers.clear();
        rt.params = None;
        return None;
    }
    let store = &app.state::<AppState>().store;
    let key = format!(
        "{}|{}",
        serde_json::to_string(&p).unwrap_or_default(),
        serde_json::to_string(&a).unwrap_or_default()
    );
    if rt.params.as_deref() != Some(key.as_str()) {
        rt.triggers = build_triggers(&p, &a);
        // 接上重启前的状态（比如今天已经问候过了）
        for t in rt.triggers.iter_mut() {
            if let Some(saved) = store.get_state(&format!("proactive:{}", t.id())) {
                t.restore(&saved);
            }
        }
        rt.params = Some(key);
    }
    let ctx = context(app, &p);
    if rt.last_mode != Some(ctx.mode) {
        eprintln!(
            "[proactive] 桌面状态：{:?}{:?}（前台 {}）",
            ctx.mode,
            ctx.silent,
            crate::pet::win::foreground_process().unwrap_or_default()
        );
        rt.last_mode = Some(ctx.mode);
    }
    for t in rt.triggers.iter_mut() {
        t.observe(&ctx);
    }
    let cooling = rt.last_spoken.is_some_and(|t| t.elapsed() < Duration::from_secs(p.min_interval_minutes * 60));
    // 冷却改在成功开口 / 闲聊 NO_SPEAK 之后写入，避免 screen-glance 被拦或变化不显著时误占冷却
    rt.triggers
        .iter_mut()
        .filter(|t| allowed(t.as_ref(), &ctx, &p, cooling))
        .find_map(|t| {
            let i = t.poll(&ctx)?;
            if let Some(saved) = t.saved() {
                store.set_state(&format!("proactive:{}", t.id()), &saved);
            }
            Some(i)
        })
}

/// 收集当前情况
pub fn context(app: &AppHandle, p: &ProactiveParams) -> Context {
    let state = app.state::<AppState>();
    let (pet_hidden, user_hidden, work_mode, bubble, fullscreen) = {
        let s = state.shared.lock().unwrap();
        (s.pet_hidden, s.user_hidden, s.work_mode, s.bubble_visible, s.fullscreen)
    };
    let busy = bubble || crate::chat::is_busy(app) || crate::voice::is_recording(app);
    let now = Local::now();
    let since_last_chat_secs = state.store.last_user_message_at().map(|t| (now.timestamp() - t).max(0) as u64);
    use crate::agents::model::Status;
    let sessions = crate::agents::sessions(app);
    let agents_waiting = sessions.iter().filter(|s| s.status == Status::Attention).map(|s| s.agent_name).collect();
    let agents_busy = sessions.iter().any(|s| matches!(s.status, Status::Thinking | Status::Working));
    let fg = crate::pet::win::foreground_process();
    let idle = user_idle_secs();
    let (watching, in_call) = if p.desktop_modes {
        let s = crate::awareness::signals(fg.as_deref(), idle, p.watching_idle_secs, p.watching_hold_secs);
        (s.watching, s.in_call)
    } else {
        (false, false)
    };
    let (mode, silent) = desktop_mode(p, fullscreen, fg.as_deref(), agents_busy, in_call, watching);
    // 在看（含全屏看视频）都算在电脑前：看完一部电影碰鼠标不算「离开归来」
    let away_secs = if watching { 0 } else { idle };
    let (fg_process, fg_title) = fg_window_info();
    Context {
        now,
        user_idle_secs: idle,
        since_last_chat_secs,
        busy,
        user_hidden,
        pet_hidden,
        quiet: work_mode || in_quiet_hours(p, now.time()),
        agents_waiting,
        mode,
        silent,
        away_secs,
        fg_process,
        fg_title,
    }
}

fn fg_window_info() -> (Option<String>, Option<String>) {
    #[cfg(windows)]
    {
        match crate::awareness::capture::resolve_target() {
            Ok(w) => (Some(w.process), Some(w.title)),
            Err(_) => (crate::pet::win::foreground_process(), None),
        }
    }
    #[cfg(not(windows))]
    {
        let t = crate::awareness::target_record();
        if t.hwnd != 0 {
            (Some(t.process), None)
        } else {
            (None, None)
        }
    }
}

/// 现在出声合不合适（给朗读用）：(静默状态：全屏、开会、在看, 安静：工作模式或免打扰时段)。
/// 不管主动聊天开没开都按同样的规则判断；窗口化看视频时朗读也会停。
pub fn sound_state(app: &AppHandle) -> (bool, bool) {
    let state = app.state::<AppState>();
    let p = state.config.lock().unwrap().proactive.clone();
    let (work_mode, fullscreen) = {
        let s = state.shared.lock().unwrap();
        (s.work_mode, s.fullscreen)
    };
    use crate::agents::model::Status;
    let agents_busy = crate::agents::sessions(app).iter().any(|s| matches!(s.status, Status::Thinking | Status::Working));
    let fg = crate::pet::win::foreground_process();
    let idle = user_idle_secs();
    let (watching, in_call) = if p.desktop_modes {
        let s = crate::awareness::signals(fg.as_deref(), idle, p.watching_idle_secs, p.watching_hold_secs);
        (s.watching, s.in_call)
    } else {
        (false, false)
    };
    let (mode, _) = desktop_mode(&p, fullscreen, fg.as_deref(), agents_busy, in_call, watching);
    (mode == DesktopMode::Silent, work_mode || in_quiet_hours(&p, Local::now().time()))
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

/// 把念头写成话，记进对话，说出来。screen-glance 失败不占冷却；闲聊 NO_SPEAK 占冷却但不说。
async fn speak_intent(app: &AppHandle, intent: &Intent) {
    let (use_model, awareness) = {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap();
        (cfg.proactive.use_model, cfg.awareness.clone())
    };
    let mut intent = intent.clone();
    let mut atts: Vec<crate::store::AttachmentMeta> = Vec::new();

    if intent.trigger == "screen-glance" {
        match prepare_screen_glance(app, &mut intent, &awareness).await {
            GlanceOutcome::Abort => {
                eprintln!("[proactive] screen-glance 放弃（门禁/不显著/不可用），不占冷却");
                return;
            }
            GlanceOutcome::Ready { thumb_file, process, record } => {
                atts.push(crate::store::AttachmentMeta {
                    kind: "image".into(),
                    name: format!("看屏 · {process}"),
                    file: Some(thumb_file),
                    desc: Some(record.clone()),
                    note: Some(record),
                    call: None,
                });
            }
        }
    }

    let title_note = title_material_for_prompt(&awareness);

    match compose(app, &intent, use_model, title_note.as_deref()).await {
        None => {
            mark_spoken();
            eprintln!("[proactive] NO_SPEAK（{}），进入冷却", intent.trigger);
        }
        Some(text) => {
            mark_spoken();
            let persona = app.state::<AppState>().config.lock().unwrap().active_persona();
            if let Err(e) = crate::chat::add_assistant_message_with_atts(app, &persona.id, &text, &atts) {
                eprintln!("[proactive] 记进对话失败：{e}");
            }
            crate::speak::say(app, &persona.name, &text, crate::speak::Source::Proactive, false);
        }
    }
}

fn mark_spoken() {
    RUNTIME.lock().unwrap().last_spoken = Some(Instant::now());
}

enum GlanceOutcome {
    Abort,
    Ready {
        thumb_file: String,
        process: String,
        record: String,
    },
}

/// 写话阶段截屏：门禁 + 可选变化打分；成功则带缩略图
async fn prepare_screen_glance(
    app: &AppHandle,
    intent: &mut Intent,
    awareness: &crate::config::AwarenessParams,
) -> GlanceOutcome {
    if !(awareness.proactive_vision && awareness.vision_authorized) {
        return GlanceOutcome::Abort;
    }
    let cfg = awareness.clone();
    let kind = intent.glance_kind.unwrap_or(GlanceKind::Rule);
    crate::awareness::set_seeing(true);
    let _ = app.emit("seeing-screen", true);
    crate::tray::refresh_tooltip(app, true);
    let captured = tokio::task::spawn_blocking(move || {
        crate::awareness::capture(crate::awareness::Scope::Window, &cfg)
    })
    .await;
    crate::awareness::set_seeing(false);
    let _ = app.emit("seeing-screen", false);
    crate::tray::refresh_tooltip(app, false);

    let shot = match captured {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            eprintln!("[proactive] screen-glance 截屏失败：{e}");
            if kind == GlanceKind::ChangeCheck {
                GLANCE_STATE.lock().unwrap().last_change_try = Some(Instant::now());
            }
            return GlanceOutcome::Abort;
        }
        Err(e) => {
            eprintln!("[proactive] screen-glance 任务失败：{e}");
            return GlanceOutcome::Abort;
        }
    };

    {
        let mut st = GLANCE_STATE.lock().unwrap();
        if kind == GlanceKind::ChangeCheck {
            st.last_change_try = Some(Instant::now());
            if let Some(prev) = st.last_hsv.as_ref() {
                let score = crate::awareness::score_pair(prev, &shot.hsv_grid);
                if !crate::awareness::is_significant(score) {
                    eprintln!("[proactive] screen-glance 变化不显著（score={score:.2}），丢弃");
                    return GlanceOutcome::Abort;
                }
            }
        }
        st.last_hsv = Some(shot.hsv_grid.clone());
    }

    if let Ok(w) = crate::awareness::capture::resolve_target() {
        let mut st = GLANCE_STATE.lock().unwrap();
        st.last_proc = w.process.clone();
        st.last_title = w.title.clone();
    } else {
        let mut st = GLANCE_STATE.lock().unwrap();
        st.last_proc = shot.process.clone();
    }

    let thumb_file = match crate::files::save_image(&shot.thumb_png, "png") {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[proactive] 缩略图保存失败：{e}");
            return GlanceOutcome::Abort;
        }
    };
    let record = format!("看了「{}」的窗口（{}×{}）", shot.process, shot.width, shot.height);

    let state = app.state::<AppState>();
    let full_cfg = state.config.lock().unwrap().clone();
    let persona = full_cfg.active_persona();
    if let Some(aux) = full_cfg.aux_provider(&persona).filter(|p| p.supports_vision) {
        let _ = aux;
        *GLANCE_PNG.lock().unwrap() = Some(shot.png.clone());
        intent.reason = format!("{}。{}", record, intent.reason);
    } else if let Some(vision) = full_cfg
        .provider(full_cfg.roles.vision.as_deref())
        .filter(|p| p.supports_vision)
    {
        let tmp = match crate::files::save_image(&shot.png, "png") {
            Ok(f) => f,
            Err(_) => return GlanceOutcome::Abort,
        };
        let ep = crate::llm::Endpoint::from_provider(vision);
        let described = crate::chat::describe_image_for_proactive(
            &ep,
            &tmp,
            &format!("桌面「{}」窗口", shot.process),
        )
        .await;
        let _ = std::fs::remove_file(crate::files::attachments_dir().join(&tmp));
        match described {
            Ok(text) => intent.reason = format!("{}。画面转述：{text}", record),
            Err(e) => {
                eprintln!("[proactive] 视觉转述失败：{e}");
                return GlanceOutcome::Abort;
            }
        }
    } else {
        eprintln!("[proactive] 没有能看图的模型，放弃 screen-glance");
        return GlanceOutcome::Abort;
    }

    {
        let mut st = GLANCE_STATE.lock().unwrap();
        let today = Local::now().format("%Y-%m-%d").to_string();
        if st.day != today {
            st.day = today;
            st.glances_today = 0;
        }
        st.glances_today += 1;
        st.last_success = Some(Instant::now());
        let store = &app.state::<AppState>().store;
        let _ = store.set_state(
            "proactive:screen-glance",
            &format!("{}:{}", st.day, st.glances_today),
        );
    }

    GlanceOutcome::Ready {
        thumb_file,
        process: shot.process,
        record,
    }
}

struct GlanceState {
    day: String,
    glances_today: u32,
    last_success: Option<Instant>,
    last_proc: String,
    last_title: String,
    last_hsv: Option<Vec<f32>>,
    last_change_try: Option<Instant>,
}

static GLANCE_STATE: Mutex<GlanceState> = Mutex::new(GlanceState {
    day: String::new(),
    glances_today: 0,
    last_success: None,
    last_proc: String::new(),
    last_title: String::new(),
    last_hsv: None,
    last_change_try: None,
});
static GLANCE_PNG: Mutex<Option<Vec<u8>>> = Mutex::new(None);

fn title_material_for_prompt(a: &crate::config::AwarenessParams) -> Option<String> {
    if !a.include_title {
        return None;
    }
    let w = crate::awareness::capture::resolve_target().ok()?;
    crate::awareness::gate(&w, a).ok()?;
    let title: String = w.title.chars().take(140).collect();
    Some(format!("前台程序 {}，窗口标题：{}", w.process, title))
}

/// 念头 → 一两句话。闲聊若模型只回 NO_SPEAK 则返回 None；重要触发器回 NO_SPEAK 时改用模板。
pub async fn compose(
    app: &AppHandle,
    intent: &Intent,
    use_model: bool,
    title_note: Option<&str>,
) -> Option<String> {
    if use_model {
        match compose_with_model(app, intent, title_note).await {
            Ok(t) if is_no_speak(&t) => {
                if intent.important {
                    return Some(intent.fallback.clone());
                }
                return None;
            }
            Ok(t) if !t.is_empty() => return Some(t),
            Ok(_) => {}
            Err(e) => eprintln!("[proactive] 用模型写失败，改用模板：{e}"),
        }
    }
    Some(intent.fallback.clone())
}

async fn compose_with_model(
    app: &AppHandle,
    intent: &Intent,
    title_note: Option<&str>,
) -> Result<String, String> {
    use crate::llm::{self, ChatMessage, Endpoint, ImageUrl, Part};
    use crate::memory;
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let persona = cfg.active_persona();
    let provider = cfg.aux_provider(&persona).ok_or("没有可用的模型")?;
    let recall = memory::Recall::from_config(&cfg);
    let memory = memory::prompt_block(
        &memory::recall(&state.store, recall.as_ref(), &intent.reason).await,
    )
    .unwrap_or_default();
    let recent: Vec<String> = state
        .store
        .history(&persona.id, None, 6)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|m| !m.llm_text.is_empty())
        .map(|m| {
            let who = if m.role == "user" { "用户" } else { "你" };
            format!("{who}：{}", m.display.chars().take(80).collect::<String>())
        })
        .collect();
    let nospeak_hint = if intent.important {
        String::new()
    } else {
        "如果现在不适合打扰，只回 NO_SPEAK。".to_string()
    };
    let system = format!(
        "{}\n\n{}\n\n现在由你主动开口跟用户说一句话（不是回复）。要求：一两句、不超过 40 个字，自然口语，符合你的人设；\
不要提到「系统」「触发」「提醒功能」这类词，不要加引号、旁白或动作描写。{nospeak_hint}",
        persona.system_prompt.trim(),
        memory
    );
    let relation = if cfg.chat.relationship {
        format!(
            "\n{}。语气要符合这种关系和心情。",
            state.store.relation(&persona.id).note(crate::store::now_secs())
        )
    } else {
        String::new()
    };
    let title_line = title_note.map(|s| format!("\n{s}")).unwrap_or_default();
    let user_text = format!(
        "现在是 {}。开口的原因：{}{title_line}{relation}\n\n最近的对话：\n{}",
        crate::chat::now_label(),
        intent.reason,
        if recent.is_empty() {
            "（还没聊过）".to_string()
        } else {
            recent.join("\n")
        }
    );
    let png = GLANCE_PNG.lock().unwrap().take();
    let messages = if let Some(png) = png.filter(|_| provider.supports_vision) {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let url = format!("data:image/png;base64,{b64}");
        vec![
            ChatMessage::text("system", system),
            ChatMessage::parts(
                "user",
                vec![
                    Part::Text { text: user_text },
                    Part::ImageUrl {
                        image_url: ImageUrl { url },
                    },
                ],
            ),
        ]
    } else {
        vec![
            ChatMessage::text("system", system),
            ChatMessage::text("user", user_text),
        ]
    };
    let text =
        llm::chat_once(&Endpoint::from_provider(provider), &messages, Some(0.8)).await?;
    Ok(clean_line(&text))
}

/// 模型写的话收拾一下：去掉首尾引号、换行合并，太长截断
pub fn clean_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s.trim_matches(|c| matches!(c, '"' | '“' | '”' | '「' | '」' | '\'' | ' '));
    s.chars().take(120).collect()
}

/// clean_line 之后整句等于 NO_SPEAK（忽略大小写和空白/常见标点；夹杂其它文字则否）
pub fn is_no_speak(s: &str) -> bool {
    let cleaned = clean_line(s);
    let mut buf = String::new();
    for c in cleaned.chars() {
        if c.is_ascii_alphanumeric() {
            buf.push(c.to_ascii_lowercase());
        } else if c.is_whitespace()
            || matches!(
                c,
                '_' | '-' | '.' | '!' | '?' | ',' | ';' | ':' | '"' | '\'' | '“' | '”' | '「' | '」'
                    | '。' | '！' | '？' | '，' | '；' | '：'
            )
        {
            continue;
        } else {
            return false;
        }
    }
    buf == "nospeak"
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

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 25, h, m, 0).unwrap()
    }

    fn ctx_at(now: DateTime<Local>, idle: u64, since_chat: Option<u64>) -> Context {
        Context {
            now,
            user_idle_secs: idle,
            since_last_chat_secs: since_chat,
            busy: false,
            user_hidden: false,
            pet_hidden: false,
            quiet: false,
            agents_waiting: vec![],
            mode: DesktopMode::Normal,
            silent: None,
            away_secs: idle,
            fg_process: None,
            fg_title: None,
        }
    }

    /// 测试用触发器桩
    struct StubTrigger {
        id: &'static str,
        important: bool,
        focus_ok: bool,
    }
    impl ProactiveTrigger for StubTrigger {
        fn id(&self) -> &'static str {
            self.id
        }
        fn important(&self) -> bool {
            self.important
        }
        fn focus_ok(&self) -> bool {
            self.focus_ok
        }
        fn poll(&mut self, _ctx: &Context) -> Option<Intent> {
            None
        }
    }
    fn stub(id: &'static str, important: bool, focus_ok: bool) -> StubTrigger {
        StubTrigger { id, important, focus_ok }
    }

    fn ctx(h: u32, m: u32, idle: u64, since_chat: Option<u64>) -> Context {
        ctx_at(at(h, m), idle, since_chat)
    }

    #[test]
    fn long_no_chat_fires_once_while_user_is_active() {
        let mut t = LongNoChat::new(3600);
        assert!(t.poll(&ctx(10, 0, 5, Some(100))).is_none());
        assert!(t.poll(&ctx(11, 0, 600, Some(3700))).is_none()); // 人不在电脑前
        assert!(t.poll(&ctx(11, 1, 10, Some(3760))).is_some());
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
        assert_eq!(i.fallback, "提醒你：下班啦");
        assert!(t.poll(&ctx(18, 10, 0, None)).is_none());
        assert!(t.poll(&ctx(18, 40, 0, None)).is_none());
    }

    #[test]
    fn sedentary_resets_on_break_and_repeats_after_another_stretch() {
        let mut t = Sedentary::new(3600);
        assert!(t.poll(&ctx(9, 0, 0, None)).is_none()); // 开始计时
        assert!(t.poll(&ctx(9, 50, 10, None)).is_none());
        assert!(t.poll(&ctx(9, 55, 400, None)).is_none()); // 离开了 6 分多钟：重新计时
        assert!(t.poll(&ctx(10, 0, 0, None)).is_none());
        let i = t.poll(&ctx(11, 0, 20, None)).unwrap(); // 连续 60 分钟
        assert!(i.fallback.contains("60 分钟"));
        assert!(t.poll(&ctx(11, 30, 0, None)).is_none()); // 刚提醒过
        assert!(t.poll(&ctx(12, 0, 0, None)).is_some()); // 又连续了 60 分钟
    }

    #[test]
    fn bedtime_window_crosses_midnight_and_limits_reminders() {
        let mut t = Bedtime::new(NaiveTime::from_hms_opt(23, 30, 0).unwrap());
        assert!(t.poll(&ctx(23, 0, 0, None)).is_none()); // 还没到
        assert!(t.poll(&ctx(23, 31, 600, None)).is_none()); // 人不在电脑前
        assert!(t.poll(&ctx(23, 32, 0, None)).is_some()); // 第 1 次
        assert!(t.poll(&ctx(23, 50, 0, None)).is_none()); // 不到 30 分钟
        let next = Local.with_ymd_and_hms(2026, 10, 26, 0, 5, 0).unwrap();
        assert!(t.poll(&ctx_at(next, 0, None)).is_some()); // 过了午夜还是同一晚，第 2 次
        let n3 = Local.with_ymd_and_hms(2026, 10, 26, 0, 40, 0).unwrap();
        assert!(t.poll(&ctx_at(n3, 0, None)).is_some()); // 第 3 次
        let n4 = Local.with_ymd_and_hms(2026, 10, 26, 1, 20, 0).unwrap();
        assert!(t.poll(&ctx_at(n4, 0, None)).is_none()); // 一晚最多 3 次
        let morning = Local.with_ymd_and_hms(2026, 10, 26, 6, 0, 0).unwrap();
        assert!(t.poll(&ctx_at(morning, 0, None)).is_none()); // 早上不催
        let tomorrow = Local.with_ymd_and_hms(2026, 10, 26, 23, 40, 0).unwrap();
        assert!(t.poll(&ctx_at(tomorrow, 0, None)).is_some()); // 第二天晚上重新计数
    }

    #[test]
    fn agent_follow_up_waits_and_drops_handled_ones() {
        let mut t = AgentFollowUp::new(ChronoDuration::minutes(5));
        let sig = |agent, kind, h, m| AgentSignal { agent, kind, project: "perch".into(), at: at(h, m) };
        t.on_agent_signal(&sig("Codex", AgentSignalKind::NeedPermission, 10, 0));
        t.on_agent_signal(&sig("Claude Code", AgentSignalKind::NeedPermission, 10, 0));
        let mut c = ctx(10, 3, 0, None);
        c.agents_waiting = vec!["Codex", "Claude Code"];
        assert!(t.poll(&c).is_none()); // 还没等够 5 分钟
        let mut c = ctx(10, 6, 0, None);
        c.agents_waiting = vec!["Codex"]; // Claude Code 已经处理了
        let i = t.poll(&c).unwrap();
        assert!(i.reason.contains("Codex") && i.reason.contains("6 分钟"));
        assert!(t.poll(&c).is_none());
        // 完成了就不用再提醒
        t.on_agent_signal(&sig("Codex", AgentSignalKind::NeedPermission, 11, 0));
        t.on_agent_signal(&sig("Codex", AgentSignalKind::Done, 11, 1));
        let mut c = ctx(11, 10, 0, None);
        c.agents_waiting = vec!["Codex"];
        assert!(t.poll(&c).is_none());
    }

    #[test]
    fn quiet_hours_and_disturb_rules() {
        let mut p = ProactiveParams::default();
        p.quiet_start = Some("23:30".into());
        p.quiet_end = Some("07:30".into());
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert!(in_quiet_hours(&p, t(23, 45)) && in_quiet_hours(&p, t(3, 0)));
        assert!(!in_quiet_hours(&p, t(7, 30)) && !in_quiet_hours(&p, t(12, 0)));
        p.quiet_start = Some("13:00".into());
        p.quiet_end = Some("14:00".into());
        assert!(in_quiet_hours(&p, t(13, 30)) && !in_quiet_hours(&p, t(14, 0)));
        p.quiet_start = None;
        assert!(!in_quiet_hours(&p, t(13, 30)));

        let chat = stub("long-no-chat", false, false);
        let important = stub("daily-at", true, false);
        let encourage = stub("encourage", false, true);
        let mut c = ctx(12, 0, 0, None);
        assert!(allowed(&chat, &c, &p, false));
        assert!(!allowed(&chat, &c, &p, true)); // 冷却中：闲聊不说
        assert!(allowed(&important, &c, &p, true)); // 重要的照说
        c.mode = DesktopMode::Focus;
        assert!(!allowed(&chat, &c, &p, false)); // 专注时闲聊不说
        assert!(allowed(&encourage, &c, &p, false) && allowed(&important, &c, &p, false)); // 鼓励和重要的可以
        assert!(!allowed(&encourage, &c, &p, true)); // 鼓励也受冷却限制
        c.mode = DesktopMode::Silent;
        c.silent = Some(SilentReason::Meeting);
        assert!(!allowed(&important, &c, &p, false)); // 开会一律不说
        c.mode = DesktopMode::Normal;
        c.silent = None;
        c.quiet = true;
        assert!(!allowed(&chat, &c, &p, false) && allowed(&important, &c, &p, false));
        assert!(!allowed(&encourage, &c, &p, false)); // 工作模式、免打扰时段不鼓励
        c.busy = true;
        assert!(!allowed(&important, &c, &p, false)); // 忙着时谁都不说
        c.busy = false;
        c.user_hidden = true;
        assert!(!allowed(&important, &c, &p, false)); // 手动隐藏谁都不说
    }

    #[test]
    fn builds_triggers_in_priority_order_and_skips_bad_times() {
        let mut p = ProactiveParams::default();
        p.no_chat_minutes = None;
        p.sedentary_minutes = None;
        p.bedtime = None;
        p.agent_follow_up = false;
        p.holidays = false;
        p.encourage_minutes = None;
        p.welcome_back_minutes = None;
        assert!(build_triggers(&p, &crate::config::AwarenessParams::default()).is_empty());
        p.birthday = Some("13-40".into()); // 写错的生日不算
        assert!(build_triggers(&p, &crate::config::AwarenessParams::default()).is_empty());
        p.holidays = true;
        p.encourage_minutes = Some(90);
        p.welcome_back_minutes = Some(60);
        p.no_chat_minutes = Some(90);
        p.sedentary_minutes = Some(60);
        p.bedtime = Some("23:30".into());
        p.agent_follow_up = true;
        p.daily = vec![
            crate::config::DailyReminder { at: "18:00".into(), label: "下班".into() },
            crate::config::DailyReminder { at: "25:99".into(), label: "写错的".into() },
        ];
        let ids: Vec<_> = build_triggers(&p, &crate::config::AwarenessParams::default()).iter().map(|t| t.id()).collect();
        assert_eq!(
            ids,
            vec!["daily-at", "special-day", "bedtime", "agent-follow-up", "sedentary", "welcome-back", "encourage", "long-no-chat"]
        );
    }

    #[test]
    fn desktop_mode_follows_foreground_app() {
        let mut p = ProactiveParams::default();
        let mode = |p: &ProactiveParams, fs, fg, busy, call, watch| desktop_mode(p, fs, fg, busy, call, watch);
        assert_eq!(mode(&p, false, Some("code"), false, false, false), (DesktopMode::Focus, None));
        assert_eq!(mode(&p, false, Some("windowsterminal"), false, false, false), (DesktopMode::Focus, None));
        assert_eq!(
            mode(&p, false, Some("wemeetapp"), false, false, false),
            (DesktopMode::Silent, Some(SilentReason::Meeting))
        );
        assert_eq!(
            mode(&p, true, Some("code"), true, false, false),
            (DesktopMode::Silent, Some(SilentReason::Fullscreen))
        ); // 全屏优先于专注
        assert_eq!(mode(&p, false, Some("chrome"), true, false, false), (DesktopMode::Focus, None)); // agent 在干活
        assert_eq!(mode(&p, false, Some("chrome"), false, false, false), (DesktopMode::Normal, None));
        assert_eq!(mode(&p, false, None, false, false, false), (DesktopMode::Normal, None));
        p.focus_apps = vec!["Obsidian.exe".into()]; // 带 .exe、大小写都行
        assert_eq!(mode(&p, false, Some("obsidian"), false, false, false), (DesktopMode::Focus, None));
        p.desktop_modes = false;
        assert_eq!(mode(&p, true, Some("zoom"), false, true, true), (DesktopMode::Normal, None));
    }

    #[test]
    fn desktop_mode_priority_meeting_over_fullscreen_watching_focus() {
        let p = ProactiveParams::default();
        // 开会 > 全屏 > 在看 > 专注 > 普通
        assert_eq!(
            desktop_mode(&p, true, Some("code"), true, true, true),
            (DesktopMode::Silent, Some(SilentReason::Meeting))
        );
        assert_eq!(
            desktop_mode(&p, true, Some("code"), true, false, true),
            (DesktopMode::Silent, Some(SilentReason::Fullscreen))
        );
        assert_eq!(
            desktop_mode(&p, false, Some("chrome"), true, false, true),
            (DesktopMode::Silent, Some(SilentReason::Watching))
        );
        assert_eq!(
            desktop_mode(&p, false, Some("code"), false, false, false),
            (DesktopMode::Focus, None)
        );
        assert_eq!(
            desktop_mode(&p, false, Some("chrome"), false, false, false),
            (DesktopMode::Normal, None)
        );
    }

    #[test]
    fn encourage_after_long_focus_and_resets_after_leaving() {
        let mut t = Encourage::new(90 * 60);
        let focus = |h, m, idle| {
            let mut c = ctx(h, m, idle, None);
            c.mode = DesktopMode::Focus;
            c
        };
        let mut step = |c: &Context| {
            t.observe(c);
            t.poll(c)
        };
        assert!(step(&focus(9, 0, 0)).is_none());
        assert!(step(&focus(10, 0, 0)).is_none());
        assert!(step(&ctx(10, 5, 0, None)).is_none()); // 切出去 5 分钟：不算中断，但这时不说
        let i = step(&focus(10, 31, 0)).unwrap();
        assert!(i.reason.contains("91 分钟"));
        assert!(step(&focus(11, 0, 0)).is_none()); // 刚鼓励过
        assert!(step(&ctx(11, 20, 0, None)).is_none()); // 离开专注 20 分钟：重新计时
        assert!(step(&ctx(11, 30, 0, None)).is_none());
        assert!(step(&focus(11, 31, 0)).is_none());
        assert!(step(&focus(12, 30, 0)).is_none());
        assert!(step(&focus(13, 1, 0)).is_some());
    }

    #[test]
    fn welcome_back_and_morning_greeting() {
        // 早上第一次见面问早安，一天一次
        let mut m = WelcomeBack::new(3600);
        for (h, min, want) in [(8, 0, true), (8, 1, false)] {
            let c = ctx(h, min, 0, None);
            m.observe(&c);
            assert_eq!(m.poll(&c).is_some_and(|i| i.reason.contains("早安")), want);
        }
        let mut t = WelcomeBack::new(3600);
        let mut step = |c: &Context| {
            t.observe(c);
            t.poll(c)
        };
        // 离开一个多小时回来（轮询一直在跑，只是没有键鼠操作）
        assert!(step(&ctx(11, 58, 3880, None)).is_none());
        assert!(step(&ctx(12, 0, 4000, None)).is_none());
        let i = step(&ctx(12, 1, 5, None)).unwrap();
        assert!(i.reason.contains("1 小时"));
        assert!(step(&ctx(12, 2, 5, None)).is_none());
        // 电脑睡眠：轮询停了两个小时（醒来时 idle 很小）
        assert!(step(&ctx(14, 3, 5, None)).unwrap().reason.contains("2 小时"));
        // 离开得不够久不打招呼
        assert!(step(&ctx(15, 0, 600, None)).is_none());
        assert!(step(&ctx(15, 1, 5, None)).is_none());
        // 回来超过 10 分钟还没轮到说，就不说了
        let mut t = WelcomeBack::new(3600);
        t.observe(&ctx(15, 0, 4000, None));
        t.observe(&ctx(15, 1, 5, None));
        assert!(t.poll(&ctx(15, 20, 5, None)).is_none());
        // 早安状态跨重启保存
        let mut t = WelcomeBack::new(3600);
        t.restore("2026-10-25");
        assert!(t.poll(&ctx(9, 0, 0, None)).is_none());
    }

    #[test]
    fn holidays_include_lunar_festivals() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(holidays_on(d(2026, 10, 1)), vec!["国庆节"]);
        // 2025 年：除夕 1 月 28 日，春节 1 月 29 日，端午 5 月 31 日，中秋 10 月 6 日
        assert_eq!(holidays_on(d(2025, 1, 28)), vec!["除夕"]);
        assert_eq!(holidays_on(d(2025, 1, 29)), vec!["春节"]);
        assert_eq!(holidays_on(d(2025, 5, 31)), vec!["端午节"]);
        assert_eq!(holidays_on(d(2025, 10, 6)), vec!["中秋节"]);
        // 2020 年国庆节和中秋在同一天
        assert_eq!(holidays_on(d(2020, 10, 1)), vec!["国庆节", "中秋节"]);
        assert!(holidays_on(d(2026, 7, 15)).is_empty());
        assert_eq!(parse_month_day("02-29"), Some((2, 29)));
        assert_eq!(parse_month_day(" 3-8 "), Some((3, 8)));
        assert!(parse_month_day("02-30").is_none() && parse_month_day("abc").is_none());
    }

    #[test]
    fn special_day_greets_once_and_combines_events() {
        let mut t = SpecialDay::new(true, Some((10, 25)), vec![((10, 25), "认识栖栖一周年".into())]);
        assert!(t.poll(&ctx(7, 30, 0, None)).is_none()); // 8 点以前不说
        assert!(t.poll(&ctx(9, 0, 600, None)).is_none()); // 人不在
        let i = t.poll(&ctx(9, 1, 0, None)).unwrap();
        assert!(i.reason.contains("生日") && i.reason.contains("认识栖栖一周年"));
        assert!(i.fallback.contains("生日快乐"));
        assert!(t.poll(&ctx(10, 0, 0, None)).is_none()); // 一天一次
        assert_eq!(t.saved().as_deref(), Some("2026-10-25"));
        let mut t = SpecialDay::new(true, None, vec![]);
        assert!(t.poll(&ctx(9, 0, 0, None)).is_none()); // 普通日子
    }

    #[test]
    fn fullscreen_whitelist_allows_daily_at_only_by_default() {
        let p = ProactiveParams::default();
        let mut c = ctx(12, 0, 0, None);
        c.mode = DesktopMode::Silent;
        c.silent = Some(SilentReason::Fullscreen);
        c.pet_hidden = true; // 全屏时桌宠通常已隐藏，白名单应绕过
        assert!(allowed(&stub("daily-at", true, false), &c, &p, false));
        assert!(!allowed(&stub("sedentary", true, false), &c, &p, false));
        assert!(!allowed(&stub("long-no-chat", false, false), &c, &p, false));
    }

    #[test]
    fn watching_whitelist_allows_sedentary_and_daily_by_default() {
        let p = ProactiveParams::default();
        let mut c = ctx(12, 0, 0, None);
        c.mode = DesktopMode::Silent;
        c.silent = Some(SilentReason::Watching);
        c.away_secs = 0;
        for id in ["sedentary", "daily-at"] {
            assert!(allowed(&stub(id, true, false), &c, &p, false), "{id}");
        }
        assert!(!allowed(&stub("bedtime", true, false), &c, &p, false)); // 就寝默认不勾，可在设置里勾
        for id in ["encourage", "welcome-back", "long-no-chat"] {
            assert!(!allowed(&stub(id, false, id == "encourage"), &c, &p, false), "{id}");
        }
    }

    #[test]
    fn watching_bedtime_allowed_when_checked() {
        let mut p = ProactiveParams::default();
        p.media_allowed_triggers.push("bedtime".into());
        let mut c = ctx(23, 40, 60, None);
        c.mode = DesktopMode::Silent;
        c.silent = Some(SilentReason::Watching);
        c.away_secs = 0; // 在看算在电脑前
        assert!(allowed(&stub("bedtime", true, false), &c, &p, false));
        let mut t = Bedtime::new(NaiveTime::from_hms_opt(23, 30, 0).unwrap());
        assert!(t.poll(&c).is_some());
    }

    #[test]
    fn meeting_blocks_even_whitelisted() {
        let mut p = ProactiveParams::default();
        p.media_allowed_triggers = vec!["daily-at".into(), "sedentary".into()];
        p.fullscreen_allowed_triggers = vec!["daily-at".into()];
        let mut c = ctx(12, 0, 0, None);
        c.mode = DesktopMode::Silent;
        c.silent = Some(SilentReason::Meeting);
        assert!(!allowed(&stub("daily-at", true, false), &c, &p, false));
        assert!(!allowed(&stub("sedentary", true, false), &c, &p, false));
    }

    #[test]
    fn whitelist_chit_chat_ids_still_blocked() {
        let mut p = ProactiveParams::default();
        p.media_allowed_triggers = vec!["long-no-chat".into(), "encourage".into(), "sedentary".into()];
        p.fullscreen_allowed_triggers = vec!["welcome-back".into(), "daily-at".into()];
        let mut c = ctx(12, 0, 0, None);
        c.mode = DesktopMode::Silent;
        c.silent = Some(SilentReason::Watching);
        assert!(!allowed(&stub("long-no-chat", false, false), &c, &p, false));
        assert!(!allowed(&stub("encourage", false, true), &c, &p, false));
        assert!(allowed(&stub("sedentary", true, false), &c, &p, false));
        c.silent = Some(SilentReason::Fullscreen);
        assert!(!allowed(&stub("welcome-back", false, false), &c, &p, false));
        assert!(allowed(&stub("daily-at", true, false), &c, &p, false));
    }

    #[test]
    fn user_hidden_and_pet_hidden_without_desktop_modes() {
        let p = ProactiveParams::default();
        let mut c = ctx(12, 0, 0, None);
        c.user_hidden = true;
        c.pet_hidden = true;
        assert!(!allowed(&stub("daily-at", true, false), &c, &p, false));
        // 关掉桌面状态、全屏隐藏：silent 为空，pet_hidden 仍拦住
        c.user_hidden = false;
        c.silent = None;
        c.mode = DesktopMode::Normal;
        c.pet_hidden = true;
        assert!(!allowed(&stub("daily-at", true, false), &c, &p, false));
    }

    #[test]
    fn away_secs_zero_while_watching_keeps_sedentary_and_skips_welcome_back() {
        // 在看时 away_secs 为 0：久坐继续计时；看完碰鼠标不触发离开归来
        let mut sed = Sedentary::new(3600);
        let mut c = ctx(9, 0, 0, None);
        c.away_secs = 0;
        assert!(sed.poll(&c).is_none());
        let mut c = ctx(10, 0, 4000, None); // user_idle 很长，但 away 仍为 0
        c.away_secs = 0;
        assert!(sed.poll(&c).is_some());

        let mut wb = WelcomeBack::new(3600);
        let mut away = ctx(11, 0, 4000, None);
        away.away_secs = 0; // 在看
        wb.observe(&away);
        let mut back = ctx(11, 1, 5, None);
        back.away_secs = 5;
        wb.observe(&back);
        assert!(wb.poll(&back).is_none()); // 看视频不算离开
    }

    #[test]
    fn cleans_model_output() {
        assert_eq!(clean_line("“早点睡呀，\n明天还要早起呢～”"), "早点睡呀， 明天还要早起呢～");
        assert_eq!(clean_line("「休息一下吧」"), "休息一下吧");
        assert_eq!(clean_line(&"啊".repeat(200)).chars().count(), 120);
    }

    #[test]
    fn no_speak_detection_ignores_case_and_punct() {
        assert!(is_no_speak("NO_SPEAK"));
        assert!(is_no_speak("no-speak"));
        assert!(is_no_speak("「No_Speak」"));
        assert!(is_no_speak(" NO SPEAK "));
        assert!(!is_no_speak("不要说话"));
        assert!(!is_no_speak("NO_SPEAK 吧"));
    }

    #[test]
    fn glance_caps_interval_and_daily() {
        assert!(glance_caps_allow(0, 30, None, 600));
        assert!(!glance_caps_allow(0, 30, Some(599), 600));
        assert!(glance_caps_allow(0, 30, Some(600), 600));
        assert!(!glance_caps_allow(30, 30, Some(9999), 600));
        assert!(glance_caps_allow(29, 30, Some(9999), 600));
    }

    #[test]
    fn glance_rules_match_process_and_title() {
        use crate::config::VisionProcessRule;
        let rules = vec![
            VisionProcessRule { process: "Code".into(), title_keyword: None },
            VisionProcessRule { process: "chrome".into(), title_keyword: Some("YouTube".into()) },
        ];
        assert!(glance_rule_hit(&rules, "code.exe", "任意"));
        assert!(!glance_rule_hit(&rules, "chrome", "首页"));
        assert!(glance_rule_hit(&rules, "Chrome.exe", "看 YouTube 视频"));
        assert!(!glance_rule_hit(&rules, "firefox", "YouTube"));
    }

    #[test]
    fn screen_glance_is_chit_chat_and_respects_caps() {
        use crate::config::{AwarenessParams, VisionProcessRule};
        let mut a = AwarenessParams::default();
        a.proactive_vision = true;
        a.vision_authorized = true;
        a.vision_min_interval_minutes = 10;
        a.vision_daily_cap = 30;
        a.vision_process_rules = vec![VisionProcessRule { process: "code".into(), title_keyword: None }];
        a.vision_on_significant_change = false;
        let mut g = ScreenGlance::new(&a);
        assert!(!g.important());
        {
            let mut st = GLANCE_STATE.lock().unwrap();
            st.day = "2026-10-25".into();
            st.glances_today = 0;
            st.last_success = None;
        }
        let mut c = ctx(10, 0, 5, Some(100));
        c.fg_process = Some("code".into());
        c.fg_title = Some("main.rs".into());
        let i = g.poll(&c).expect("rule hit");
        assert_eq!(i.trigger, "screen-glance");
        assert!(!i.important);
        assert_eq!(i.glance_kind, Some(GlanceKind::Rule));

        {
            let mut st = GLANCE_STATE.lock().unwrap();
            st.last_success = Some(std::time::Instant::now());
            st.glances_today = 1;
        }
        assert!(g.poll(&c).is_none(), "min interval");

        {
            let mut st = GLANCE_STATE.lock().unwrap();
            st.last_success = Some(std::time::Instant::now() - std::time::Duration::from_secs(601));
            st.glances_today = 30;
        }
        assert!(g.poll(&c).is_none(), "daily cap");
    }

    #[test]
    fn build_triggers_adds_screen_glance_when_enabled() {
        let p = ProactiveParams { enabled: true, ..ProactiveParams::default() };
        let mut a = crate::config::AwarenessParams::default();
        assert!(!build_triggers(&p, &a).iter().any(|t| t.id() == "screen-glance"));
        a.proactive_vision = true;
        a.vision_authorized = true;
        assert!(build_triggers(&p, &a).iter().any(|t| t.id() == "screen-glance"));
    }
}
