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

    /// 完成 / 出错状态过一会儿回到待机；清理太久没动静的会话
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
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
        }
        changed
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
        assert!(hub.tick(t0 + Duration::from_secs(60)));
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
