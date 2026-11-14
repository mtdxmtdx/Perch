//! 统一的 agent 事件、会话状态，以及多会话合并成一个桌宠状态。纯逻辑，不碰 IO。

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Codex,
    Dsh,
}

impl Agent {
    pub fn from_slug(s: &str) -> Option<Self> {
        match s {
            "claude-code" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "dsh" => Some(Self::Dsh),
            _ => None,
        }
    }
    pub fn display(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Dsh => "DeepSeek Harness",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    SessionStart,
    Prompt,
    ToolStart { tool: String },
    ToolEnd,
    NeedPermission { tool: Option<String> },
    Stop { final_message: String, canceled: bool },
    Error { message: String },
    SessionEnd,
}

#[derive(Clone, Debug)]
pub struct AgentEvent {
    pub agent: Agent,
    pub session_id: String,
    pub cwd: String,
    pub kind: Kind,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Claude Code 与 Codex 的 hook 输入格式相同（hook_event_name + 各事件字段）
fn from_hook_payload(agent: Agent, v: &Value) -> Option<AgentEvent> {
    let kind = match s(v, "hook_event_name").as_str() {
        "SessionStart" => Kind::SessionStart,
        "UserPromptSubmit" => Kind::Prompt,
        "PreToolUse" => Kind::ToolStart { tool: s(v, "tool_name") },
        "PostToolUse" | "PostToolUseFailure" => Kind::ToolEnd,
        // Codex 开了「自动审批」时，审批请求交给审核代理处理，不会停下来问人：当作还在干活
        "PermissionRequest" if agent == Agent::Codex && codex_auto_review(v) => Kind::ToolStart { tool: s(v, "tool_name") },
        "PermissionRequest" => Kind::NeedPermission { tool: Some(s(v, "tool_name")).filter(|t| !t.is_empty()) },
        "Notification" => {
            // 只关心"等你授权"；空闲提醒等其他通知忽略
            let ty = s(v, "notification_type");
            let msg = s(v, "message").to_lowercase();
            if ty == "permission_prompt" || (ty.is_empty() && msg.contains("permission")) {
                Kind::NeedPermission { tool: None }
            } else {
                return None;
            }
        }
        "Stop" => Kind::Stop { final_message: s(v, "last_assistant_message"), canceled: false },
        "Interrupt" => Kind::Stop { final_message: String::new(), canceled: true },
        "StopFailure" => Kind::Error { message: s(v, "error").chars().take(200).collect() },
        "SessionEnd" => Kind::SessionEnd,
        _ => return None,
    };
    Some(AgentEvent { agent, session_id: s(v, "session_id"), cwd: s(v, "cwd"), kind })
}

/// 往回最多读多少字节找这一轮的 turn_context（会话记录可能很大）
const ROLLOUT_TAIL_MAX: u64 = 16 * 1024 * 1024;

/// 这次 Codex 审批请求会不会由审核代理自动处理。
/// 钩子数据里没有这个信息，要看会话记录（transcript_path）里这一轮的 turn_context：
/// `approvals_reviewer` 为 `auto_review` 时，请求交给审核代理，不问人。读不到就当作要问人（照旧提醒）
fn codex_auto_review(v: &Value) -> bool {
    let path = s(v, "transcript_path");
    if path.is_empty() {
        return false;
    }
    let turn = s(v, "turn_id");
    read_rollout_tail(std::path::Path::new(&path))
        .and_then(|tail| rollout_reviewer(&tail, Some(turn.as_str()).filter(|t| !t.is_empty())))
        .is_some_and(|r| r == "auto_review")
}

/// 从文件末尾往前读，直到读到一个 turn_context 或者读满上限
fn read_rollout_tail(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut size = 256 * 1024u64;
    loop {
        let start = len.saturating_sub(size);
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::new();
        f.by_ref().take(len - start).read_to_end(&mut buf).ok()?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        if text.contains("\"turn_context\"") || start == 0 || size >= ROLLOUT_TAIL_MAX {
            return Some(text);
        }
        size = (size * 4).min(ROLLOUT_TAIL_MAX);
    }
}

/// 在会话记录的末尾里找这一轮（turn_id 对得上的；对不上就用最后一个）的审批方式
pub fn rollout_reviewer(tail: &str, turn_id: Option<&str>) -> Option<String> {
    let mut last: Option<String> = None;
    for line in tail.lines().rev() {
        if !line.contains("\"turn_context\"") {
            continue;
        }
        let Ok(o) = serde_json::from_str::<Value>(line) else { continue }; // 开头那行可能被截断
        if o.get("type").and_then(Value::as_str) != Some("turn_context") {
            continue;
        }
        let p = o.get("payload").unwrap_or(&Value::Null);
        let reviewer = p.get("approvals_reviewer").and_then(Value::as_str).unwrap_or("user").to_string();
        match turn_id {
            Some(t) if p.get("turn_id").and_then(Value::as_str) == Some(t) => return Some(reviewer),
            Some(_) => {
                last.get_or_insert(reviewer);
            }
            None => return Some(reviewer),
        }
    }
    last
}

/// DSH 插件发来的是已经简化过的事件
fn from_dsh(v: &Value) -> Option<AgentEvent> {
    let kind = match s(v, "event").as_str() {
        "session_start" => Kind::SessionStart,
        "prompt" => Kind::Prompt,
        "tool_start" => Kind::ToolStart { tool: s(v, "tool") },
        "tool_end" | "resume" => Kind::ToolEnd,
        "need_permission" => Kind::NeedPermission { tool: Some(s(v, "tool")).filter(|t| !t.is_empty()) },
        "stop" => Kind::Stop {
            final_message: s(v, "final_message"),
            canceled: v.get("canceled").and_then(Value::as_bool).unwrap_or(false),
        },
        "error" => Kind::Error { message: s(v, "message") },
        "session_end" => Kind::SessionEnd,
        _ => return None,
    };
    Some(AgentEvent { agent: Agent::Dsh, session_id: s(v, "session_id"), cwd: s(v, "cwd"), kind })
}

pub fn normalize(agent: Agent, v: &Value) -> Option<AgentEvent> {
    let ev = match agent {
        Agent::Dsh => from_dsh(v),
        _ => from_hook_payload(agent, v),
    }?;
    (!ev.session_id.is_empty()).then_some(ev)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Idle,
    Thinking,
    Working,
    Attention,
    Done,
    Error,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub agent: Agent,
    pub id: String,
    pub cwd: String,
    pub status: Status,
    pub tool: Option<String>,
    pub started: Instant,
    pub updated: Instant,
    /// 这一轮开始的时间（算耗时）
    pub turn_started: Option<Instant>,
}

/// 状态变化后需要对外汇报的事
#[derive(Clone, Debug, PartialEq)]
pub enum Report {
    NeedPermission { tool: Option<String> },
    Done { final_message: String, seconds: u64 },
    Failed { message: String },
}

/// 完成 / 出错的动画只播一会儿，之后回到待机
const DONE_SHOW: Duration = Duration::from_secs(5);
const ERROR_SHOW: Duration = Duration::from_secs(10);
/// 太久没有动静的会话（比如终端被直接关掉、没发 SessionEnd）清理掉
const STALE: Duration = Duration::from_secs(2 * 3600);
/// Codex 的 Computer Use 调用跑了这么久还没结束，多半是在等你允许它操作某个程序。
/// 它的授权框是 Codex 自己弹的，不经过 PermissionRequest 钩子；平时一次调用 90% 在 2 秒内结束
const COMPUTER_USE_WAIT: Duration = Duration::from_secs(6);

/// Codex 的 Computer Use 工具（MCP 服务 cua_repl）
pub fn is_computer_use(tool: &str) -> bool {
    let t = tool.to_lowercase();
    t.contains("cua_repl") || t.contains("computer_use") || t.contains("computer-use")
}

#[derive(Default)]
pub struct Hub {
    pub sessions: HashMap<(Agent, String), Session>,
}

impl Hub {
    /// 处理一个事件，返回需要汇报的内容
    pub fn apply(&mut self, ev: AgentEvent, now: Instant) -> Option<Report> {
        let key = (ev.agent, ev.session_id.clone());
        if ev.kind == Kind::SessionEnd {
            self.sessions.remove(&key);
            return None;
        }
        let s = self.sessions.entry(key).or_insert_with(|| Session {
            agent: ev.agent,
            id: ev.session_id.clone(),
            cwd: ev.cwd.clone(),
            status: Status::Idle,
            tool: None,
            started: now,
            updated: now,
            turn_started: None,
        });
        if !ev.cwd.is_empty() {
            s.cwd = ev.cwd.clone();
        }
        s.updated = now;
        match ev.kind {
            Kind::SessionStart => None,
            Kind::Prompt => {
                s.status = Status::Thinking;
                s.tool = None;
                s.turn_started = Some(now);
                None
            }
            Kind::ToolStart { tool } => {
                s.status = Status::Working;
                s.tool = Some(tool).filter(|t| !t.is_empty());
                s.turn_started.get_or_insert(now);
                None
            }
            Kind::ToolEnd => {
                // 工具跑完（包括授权通过后跑完），回到思考
                s.status = Status::Thinking;
                s.tool = None;
                None
            }
            Kind::NeedPermission { tool } => {
                s.status = Status::Attention;
                Some(Report::NeedPermission { tool: tool.or_else(|| s.tool.clone()) })
            }
            Kind::Stop { final_message, canceled } => {
                let seconds = s.turn_started.take().map_or(0, |t| now.duration_since(t).as_secs());
                s.tool = None;
                if canceled {
                    s.status = Status::Idle;
                    return None;
                }
                s.status = Status::Done;
                Some(Report::Done { final_message, seconds })
            }
            Kind::Error { message } => {
                s.status = Status::Error;
                s.turn_started = None;
                Some(Report::Failed { message })
            }
            Kind::SessionEnd => unreachable!(),
        }
    }

    /// 完成 / 出错状态过一会儿回到待机；清理太久没动静的会话；
    /// Codex 的 Computer Use 调用卡住太久时当作在等你授权。返回 (状态有没有变, 要汇报的事)
    pub fn tick(&mut self, now: Instant) -> (bool, Vec<(Agent, String, Report)>) {
        let mut changed = false;
        let mut reports = Vec::new();
        self.sessions.retain(|_, s| {
            let keep = now.duration_since(s.updated) < STALE;
            changed |= !keep;
            keep
        });
        for s in self.sessions.values_mut() {
            let expire = match s.status {
                Status::Done => Some(DONE_SHOW),
                Status::Error => Some(ERROR_SHOW),
                _ => None,
            };
            if expire.is_some_and(|d| now.duration_since(s.updated) >= d) {
                s.status = Status::Idle;
                changed = true;
            }
            let waiting_on_computer_use = s.agent == Agent::Codex
                && s.status == Status::Working
                && s.tool.as_deref().is_some_and(is_computer_use)
                && now.duration_since(s.updated) >= COMPUTER_USE_WAIT;
            if waiting_on_computer_use {
                // 调用结束（PostToolUse）时会回到「思考」
                s.status = Status::Attention;
                changed = true;
                reports.push((s.agent, s.cwd.clone(), Report::NeedPermission { tool: Some("Computer Use".into()) }));
            }
        }
        (changed, reports)
    }

    /// 所有会话合并成一个桌宠动画：等待授权 > 出错 > 工作中 > 思考 > 完成
    pub fn pet_anim(&self) -> Option<&'static str> {
        let has = |st: Status| self.sessions.values().any(|s| s.status == st);
        if has(Status::Attention) {
            Some("attention")
        } else if has(Status::Error) {
            Some("error")
        } else if has(Status::Working) {
            Some("working")
        } else if has(Status::Thinking) {
            Some("think")
        } else if has(Status::Done) {
            Some("done")
        } else {
            None
        }
    }
}

/// 项目名：取工作目录的最后一段
pub fn project_name(cwd: &str) -> String {
    let t = cwd.trim_end_matches(['\\', '/']);
    t.rsplit(['\\', '/']).next().filter(|s| !s.is_empty()).unwrap_or(t).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(v: Value) -> AgentEvent {
        normalize(Agent::ClaudeCode, &v).unwrap()
    }

    #[test]
    fn codex_computer_use_stuck_long_means_waiting_for_permission() {
        let t0 = Instant::now();
        let mut hub = Hub::default();
        let ev = |kind: Kind| AgentEvent { agent: Agent::Codex, session_id: "s".into(), cwd: "D:\\proj".into(), kind };
        hub.apply(ev(Kind::Prompt), t0);
        hub.apply(ev(Kind::ToolStart { tool: "mcp__cua_repl__js".into() }), t0);
        // 平时很快就结束：不提醒
        assert!(hub.tick(t0 + Duration::from_secs(3)).1.is_empty());
        // 卡住 6 秒：当作在等授权，只提醒一次
        let (changed, reports) = hub.tick(t0 + Duration::from_secs(7));
        assert!(changed);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].2, Report::NeedPermission { tool: Some("Computer Use".into()) });
        assert_eq!(hub.pet_anim(), Some("attention"));
        assert!(hub.tick(t0 + Duration::from_secs(9)).1.is_empty());
        // 允许以后调用结束：回到思考
        hub.apply(ev(Kind::ToolEnd), t0 + Duration::from_secs(10));
        assert_eq!(hub.sessions.values().next().unwrap().status, Status::Thinking);
        // 别的工具跑得久不算
        hub.apply(ev(Kind::ToolStart { tool: "Bash".into() }), t0 + Duration::from_secs(11));
        assert!(hub.tick(t0 + Duration::from_secs(60)).1.is_empty());
        assert!(is_computer_use("mcp__cua_repl__js") && !is_computer_use("exec_command"));
    }

    #[test]
    fn codex_auto_review_is_read_from_the_turn_context() {
        let ctx = |turn: &str, reviewer: &str| {
            json!({ "timestamp": "t", "type": "turn_context", "payload": { "turn_id": turn, "approval_policy": "on-request", "approvals_reviewer": reviewer } })
                .to_string()
        };
        let other = json!({ "type": "response_item", "payload": { "text": "\"turn_context\" 出现在正文里" } }).to_string();
        let tail = format!("{{\"type\":\"turn_co\n{}\n{}\n{}\n{}", ctx("t1", "user"), other, ctx("t2", "auto_review"), other);
        assert_eq!(rollout_reviewer(&tail, Some("t2")).as_deref(), Some("auto_review"));
        assert_eq!(rollout_reviewer(&tail, Some("t1")).as_deref(), Some("user"));
        // turn_id 对不上（比如记录还没写到这一轮）：用最后一个
        assert_eq!(rollout_reviewer(&tail, Some("t9")).as_deref(), Some("auto_review"));
        assert_eq!(rollout_reviewer(&tail, None).as_deref(), Some("auto_review"));
        assert_eq!(rollout_reviewer(&other, None), None);

        // 整条链路：自动审批时当作在干活，否则提醒
        let dir = std::env::temp_dir().join(format!("perch-rollout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rollout.jsonl");
        let ev = |path: &std::path::Path| {
            json!({ "hook_event_name": "PermissionRequest", "session_id": "s", "cwd": "D:\\p", "tool_name": "Bash",
                    "turn_id": "t2", "transcript_path": path.to_string_lossy() })
        };
        std::fs::write(&file, format!("{}\n{}\n", ctx("t2", "auto_review"), other)).unwrap();
        assert_eq!(normalize(Agent::Codex, &ev(&file)).unwrap().kind, Kind::ToolStart { tool: "Bash".into() });
        std::fs::write(&file, format!("{}\n", ctx("t2", "user"))).unwrap();
        assert_eq!(normalize(Agent::Codex, &ev(&file)).unwrap().kind, Kind::NeedPermission { tool: Some("Bash".into()) });
        // 读不到记录：照旧提醒
        assert_eq!(normalize(Agent::Codex, &ev(&dir.join("missing.jsonl"))).unwrap().kind, Kind::NeedPermission { tool: Some("Bash".into()) });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_payloads_normalize() {
        let base = |name: &str| json!({ "hook_event_name": name, "session_id": "s1", "cwd": "D:\\proj\\demo" });
        assert_eq!(ev(base("UserPromptSubmit")).kind, Kind::Prompt);
        let mut pre = base("PreToolUse");
        pre["tool_name"] = json!("Bash");
        assert_eq!(ev(pre).kind, Kind::ToolStart { tool: "Bash".into() });
        let mut n = base("Notification");
        n["notification_type"] = json!("permission_prompt");
        assert_eq!(ev(n).kind, Kind::NeedPermission { tool: None });
        let mut idle = base("Notification");
        idle["notification_type"] = json!("idle_prompt");
        assert!(normalize(Agent::ClaudeCode, &idle).is_none());
        let mut stop = base("Stop");
        stop["last_assistant_message"] = json!("改好了");
        assert_eq!(ev(stop).kind, Kind::Stop { final_message: "改好了".into(), canceled: false });
        assert!(normalize(Agent::ClaudeCode, &json!({ "hook_event_name": "Stop" })).is_none(), "没有 session_id 的丢掉");
        assert!(normalize(Agent::Codex, &base("SomethingNew")).is_none());
        assert_eq!(normalize(Agent::Codex, &base("Interrupt")).unwrap().kind, Kind::Stop { final_message: String::new(), canceled: true });
    }

    #[test]
    fn session_lifecycle_and_reports() {
        let mut hub = Hub::default();
        let t0 = Instant::now();
        let mk = |kind| AgentEvent { agent: Agent::ClaudeCode, session_id: "s".into(), cwd: "D:\\p".into(), kind };
        assert_eq!(hub.apply(mk(Kind::Prompt), t0), None);
        assert_eq!(hub.pet_anim(), Some("think"));
        hub.apply(mk(Kind::ToolStart { tool: "Bash".into() }), t0);
        assert_eq!(hub.pet_anim(), Some("working"));
        assert_eq!(hub.apply(mk(Kind::NeedPermission { tool: None }), t0), Some(Report::NeedPermission { tool: Some("Bash".into()) }));
        assert_eq!(hub.pet_anim(), Some("attention"));
        hub.apply(mk(Kind::ToolEnd), t0);
        let r = hub.apply(mk(Kind::Stop { final_message: "好了".into(), canceled: false }), t0 + Duration::from_secs(42));
        assert_eq!(r, Some(Report::Done { final_message: "好了".into(), seconds: 42 }));
        assert_eq!(hub.pet_anim(), Some("done"));
        // 完成动画过一会儿消失
        assert!(hub.tick(t0 + Duration::from_secs(60)).0);
        assert_eq!(hub.pet_anim(), None);
        hub.apply(mk(Kind::SessionEnd), t0);
        assert!(hub.sessions.is_empty());
    }

    #[test]
    fn priority_across_sessions() {
        let mut hub = Hub::default();
        let t = Instant::now();
        let mk = |id: &str, kind| AgentEvent { agent: Agent::Codex, session_id: id.into(), cwd: String::new(), kind };
        hub.apply(mk("a", Kind::ToolStart { tool: "x".into() }), t);
        hub.apply(mk("b", Kind::Prompt), t);
        assert_eq!(hub.pet_anim(), Some("working"));
        hub.apply(mk("b", Kind::NeedPermission { tool: None }), t);
        assert_eq!(hub.pet_anim(), Some("attention"));
        // 取消不汇报
        assert_eq!(hub.apply(mk("a", Kind::Stop { final_message: String::new(), canceled: true }), t), None);
    }

    #[test]
    fn project_names() {
        assert_eq!(project_name("D:\\table pet\\"), "table pet");
        assert_eq!(project_name("/home/u/demo"), "demo");
        assert_eq!(project_name(""), "");
    }
}
