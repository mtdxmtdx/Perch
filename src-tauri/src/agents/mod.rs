//! 接入 Claude Code / Codex / DeepSeek Harness：收事件 → 更新桌宠动画 → 汇报（气泡、通知、人格口吻总结）。

pub mod dsh;
pub mod hook;
pub mod install;
pub mod model;
mod server;

use crate::config::AgentsParams;
use crate::llm::{self, ChatMessage, Endpoint};
use crate::{windows, AppState};
use install::HookState;
use model::{Agent, Hub, Report, Status};
use serde::Serialize;
use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Default)]
pub struct AgentRuntime {
    hub: Mutex<Hub>,
    /// 实际监听的端口（配置的端口被占用时会往后挪）
    port: Mutex<Option<u16>>,
    token: Mutex<String>,
    /// 首次启动的接入询问，用户回答前一直留着（气泡重新打开时还能看到）
    offer: Mutex<Option<Vec<OfferItem>>>,
}

/// 校验令牌：首次运行时随机生成，存在数据目录里
fn load_token() -> String {
    let path = crate::config::data_dir().join("agent-token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if t.len() >= 16 {
            return t;
        }
    }
    let t = uuid::Uuid::new_v4().simple().to_string();
    let _ = std::fs::create_dir_all(crate::config::data_dir());
    let _ = std::fs::write(&path, &t);
    t
}

fn params(app: &AppHandle) -> AgentsParams {
    app.state::<AppState>().config.lock().unwrap().agents.clone()
}

pub fn port_and_token(app: &AppHandle) -> (u16, String) {
    let state = app.state::<AppState>();
    let port = state.agents.port.lock().unwrap().unwrap_or_else(|| params(app).port);
    let token = state.agents.token.lock().unwrap().clone();
    (port, token)
}

/// 启动事件服务和定时器（完成/出错动画到时间回到待机、清理僵尸会话）
pub fn start(app: &AppHandle) {
    let token = load_token();
    *app.state::<AppState>().agents.token.lock().unwrap() = token.clone();
    let port = params(app).port;
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        match server::start(app2.clone(), port, token).await {
            Ok(p) => {
                *app2.state::<AppState>().agents.port.lock().unwrap() = Some(p);
                let _ = std::fs::write(crate::config::data_dir().join("agent-port"), p.to_string());
                if p != port {
                    eprintln!("[agents] 端口 {port} 被占用，改用 {p}");
                }
                startup_check(&app2);
            }
            Err(e) => eprintln!("[agents] {e}"),
        }
    });
    let app3 = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let changed = app3.state::<AppState>().agents.hub.lock().unwrap().tick(Instant::now());
            if changed {
                publish(&app3);
            }
        }
    });
}

fn enabled(p: &AgentsParams, agent: Agent) -> bool {
    match agent {
        Agent::ClaudeCode => p.claude_code,
        Agent::Codex => p.codex,
        Agent::Dsh => p.dsh,
    }
}

/// 服务收到一个事件
pub fn handle(app: &AppHandle, agent_slug: &str, payload: Value) {
    let Some(agent) = Agent::from_slug(agent_slug) else { return };
    let p = params(app);
    // 设置里关掉了但 hooks 还残留时，忽略
    if !enabled(&p, agent) {
        return;
    }
    let Some(ev) = model::normalize(agent, &payload) else { return };
    let cwd = ev.cwd.clone();
    let report = app.state::<AppState>().agents.hub.lock().unwrap().apply(ev, Instant::now());
    publish(app);
    if let Some(r) = report {
        let app = app.clone();
        tauri::async_runtime::spawn(async move { report_out(&app, agent, &cwd, r, &p).await });
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub agent: Agent,
    pub agent_name: &'static str,
    pub id: String,
    pub project: String,
    pub cwd: String,
    pub status: Status,
    pub tool: Option<String>,
    /// 这一轮已经跑了多少秒；没在跑时是距上次动静的秒数
    pub seconds: u64,
    pub running: bool,
}

pub fn sessions(app: &AppHandle) -> Vec<SessionView> {
    let state = app.state::<AppState>();
    let hub = state.agents.hub.lock().unwrap();
    let now = Instant::now();
    let mut v: Vec<SessionView> = hub
        .sessions
        .values()
        .map(|s| SessionView {
            agent: s.agent,
            agent_name: s.agent.display(),
            id: s.id.clone(),
            project: model::project_name(&s.cwd),
            cwd: s.cwd.clone(),
            status: s.status,
            tool: s.tool.clone(),
            seconds: s.turn_started.map_or_else(|| now.duration_since(s.updated).as_secs(), |t| now.duration_since(t).as_secs()),
            running: s.turn_started.is_some(),
        })
        .collect();
    v.sort_by(|a, b| b.running.cmp(&a.running).then(a.seconds.cmp(&b.seconds)));
    v
}

/// 合并状态 → 桌宠动画；会话列表推给界面
fn publish(app: &AppHandle) {
    let anim = app.state::<AppState>().agents.hub.lock().unwrap().pet_anim();
    app.state::<AppState>().shared.lock().unwrap().agent_anim = anim;
    let _ = app.emit("agents-changed", sessions(app));
}

fn fmt_duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} 秒"),
        60..=3599 => format!("{} 分 {} 秒", secs / 60, secs % 60),
        _ => format!("{} 小时 {} 分", secs / 3600, secs % 3600 / 60),
    }
}

/// 用当前人格的口吻把完成情况说成一两句话
async fn persona_summary(app: &AppHandle, agent: Agent, project: &str, final_message: &str) -> Result<String, String> {
    let cfg = app.state::<AppState>().config.lock().unwrap().clone();
    let persona = cfg.active_persona();
    let provider = cfg.aux_provider(&persona).ok_or("没有可用的模型")?;
    let system = format!(
        "{}\n\n你现在要向主人汇报：一个编程助手刚完成了一项任务。用你自己的口吻、一两句话（不超过 60 字）告诉主人它做完了什么。只根据下面给出的内容说，不要编造细节。",
        persona.system_prompt.trim()
    );
    let excerpt: String = final_message.chars().take(1500).collect();
    let user = format!("{} 在项目「{project}」完成了任务。它最后的回复：\n{excerpt}", agent.display());
    let text = llm::chat_once(&Endpoint::from_provider(provider), &[ChatMessage::text("system", system), ChatMessage::text("user", user)], Some(0.7)).await?;
    Ok(text.trim().to_string())
}

async fn report_out(app: &AppHandle, agent: Agent, cwd: &str, r: Report, p: &AgentsParams) {
    let project = model::project_name(cwd);
    // 告诉主动聊天的触发器（关着时什么都不做）
    let kind = match &r {
        Report::NeedPermission { .. } => crate::proactive::AgentSignalKind::NeedPermission,
        Report::Failed { .. } => crate::proactive::AgentSignalKind::Failed,
        Report::Done { .. } => crate::proactive::AgentSignalKind::Done,
    };
    crate::proactive::agent_signal(
        app,
        crate::proactive::AgentSignal { agent: agent.display(), kind, project: project.clone(), at: chrono::Local::now() },
    );
    let where_ = if project.is_empty() { String::new() } else { format!("「{project}」的 ") };
    let (title, text) = match r {
        Report::NeedPermission { tool } => {
            let what = tool.map(|t| format!("（{t}）")).unwrap_or_default();
            (format!("{} 等你授权", agent.display()), format!("{where_}{} 在等你授权{what}，去看看吧～", agent.display()))
        }
        Report::Failed { message } => {
            let m: String = message.chars().take(120).collect();
            (format!("{} 出错了", agent.display()), format!("{where_}{} 出错了{}", agent.display(), if m.is_empty() { String::new() } else { format!("：{m}") }))
        }
        Report::Done { final_message, seconds } => {
            // 对话式使用时每句回复都会结束一轮，太短的只播动画不打扰
            if seconds < p.min_report_seconds {
                return;
            }
            let template = format!("{where_}{} 干完活啦（用时 {}）", agent.display(), fmt_duration(seconds));
            let text = if p.summarize && !final_message.trim().is_empty() {
                persona_summary(app, agent, &project, &final_message).await.unwrap_or(template)
            } else {
                template
            };
            (format!("{} 完成了", agent.display()), text)
        }
    };
    crate::speak::say(app, &title, &text, crate::speak::Source::Agent, p.notify);
}

/// 程序自己的提示（需要修复、改名迁移等）
pub fn say(app: &AppHandle, title: &str, text: &str, notify: bool) {
    crate::speak::say(app, title, text, crate::speak::Source::System, notify);
}

// ---------- 设置页 ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub agent: Agent,
    pub name: &'static str,
    /// 本机是否装了这个工具（找到了它的配置目录）
    pub detected: bool,
    pub enabled: bool,
    pub state: HookState,
    /// 给用户看的提示
    pub note: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsOverview {
    pub port: Option<u16>,
    pub agents: Vec<AgentStatus>,
    pub sessions: Vec<SessionView>,
}


fn dsh_state(port: u16, token: &str, summarize: bool) -> HookState {
    if !dsh::installed_in_profile() {
        HookState::Missing
    } else if dsh::enabled_in_profile() && dsh::plugin_config_current(port, token, summarize) {
        HookState::Ok
    } else {
        HookState::Outdated
    }
}

pub fn overview(app: &AppHandle) -> AgentsOverview {
    let p = params(app);
    let (port, token) = port_and_token(app);
    let claude_dir = install::claude_settings_path().parent().map(|d| d.is_dir()).unwrap_or(false);
    let codex_dir = install::codex_hooks_path().parent().map(|d| d.is_dir()).unwrap_or(false);
    let dsh_found = dsh::find_dsh().is_some();
    AgentsOverview {
        port: *app.state::<AppState>().agents.port.lock().unwrap(),
        agents: vec![
            AgentStatus {
                agent: Agent::ClaudeCode,
                name: Agent::ClaudeCode.display(),
                detected: claude_dir,
                enabled: p.claude_code,
                state: install::claude_state(port, &token),
                note: None,
            },
            AgentStatus {
                agent: Agent::Codex,
                name: Agent::Codex.display(),
                detected: codex_dir,
                enabled: p.codex,
                state: install::codex_state(port, &token),
                note: p.codex.then(|| "首次开启或修复后，要在 Codex 里输入 /hooks，信任桌宠的 hooks（未信任的 hook 会被跳过）".into()),
            },
            AgentStatus {
                agent: Agent::Dsh,
                name: Agent::Dsh.display(),
                detected: dsh::desktop_profile_exists(),
                enabled: p.dsh,
                state: dsh_state(port, &token, p.summarize),
                note: if !dsh_found {
                    Some(dsh::NO_DSH_HINT.into())
                } else {
                    Some("装好或修复后请重启 DeepSeek Harness，插件才会加载".into())
                },
            },
        ],
        sessions: sessions(app),
    }
}

/// 开关某个接入：开 = 写入 hooks / 安装插件；关 = 移除
pub fn set_enabled(app: &AppHandle, agent: Agent, on: bool) -> Result<(), String> {
    let (port, token) = port_and_token(app);
    let summarize = params(app).summarize;
    match (agent, on) {
        (Agent::ClaudeCode, true) => install::install_claude(port, &token)?,
        (Agent::ClaudeCode, false) => install::uninstall_claude()?,
        (Agent::Codex, true) => install::install_codex(port, &token)?,
        (Agent::Codex, false) => install::uninstall_codex()?,
        (Agent::Dsh, true) => {
            dsh::install(port, &token, summarize)?;
        }
        (Agent::Dsh, false) => dsh::uninstall()?,
    }
    let state = app.state::<AppState>();
    let mut cfg = state.config.lock().unwrap();
    match agent {
        Agent::ClaudeCode => cfg.agents.claude_code = on,
        Agent::Codex => cfg.agents.codex = on,
        Agent::Dsh => cfg.agents.dsh = on,
    }
    crate::config::save(&cfg);
    drop(cfg);
    if !on {
        // 关掉后清掉这个工具的会话
        state.agents.hub.lock().unwrap().sessions.retain(|(a, _), _| *a != agent);
        publish(app);
    }
    Ok(())
}

/// 已开启的接入如果配置过期了（端口、令牌、程序路径变了），重新写入
pub fn repair(app: &AppHandle) -> Result<(), String> {
    let p = params(app);
    let mut errs = Vec::new();
    for (agent, on) in [(Agent::ClaudeCode, p.claude_code), (Agent::Codex, p.codex), (Agent::Dsh, p.dsh)] {
        if on {
            if let Err(e) = set_enabled(app, agent, true) {
                errs.push(format!("{}：{e}", agent.display()));
            }
        }
    }
    if errs.is_empty() { Ok(()) } else { Err(errs.join("\n")) }
}

/// 启动时检查：已开启的接入缺了或过期了，在气泡里提示去设置里修复
fn startup_check(app: &AppHandle) {
    if crate::config::JUST_MIGRATED.load(std::sync::atomic::Ordering::SeqCst) {
        after_rename(app);
        return;
    }
    let broken: Vec<&str> = overview(app)
        .agents
        .iter()
        .filter(|a| a.enabled && a.state != HookState::Ok)
        .map(|a| a.name)
        .collect();
    if !broken.is_empty() {
        say(app, "Agent 接入需要修复", &format!("{} 的接入需要修复，去 设置 → Agent 接入 点一下「修复」吧", broken.join("、")), false);
    }
    offer_onboarding(app);
}

/// 改名后的第一次启动：已开启的接入按新名字重写（令牌头、DSH 插件名都变了），再告诉用户
fn after_rename(app: &AppHandle) {
    let p = params(app);
    let mut notes = vec!["我改名叫 Perch 啦，之前的设置、聊天记录和记忆都搬过来了。".to_string()];
    if p.claude_code || p.codex || p.dsh {
        match repair(app) {
            Ok(()) => notes.push("各 agent 的接入也换成了新名字。".into()),
            Err(e) => notes.push(format!("有的接入没更新成功，去 设置 → Agent 接入 点「修复」：{e}")),
        }
        if p.codex {
            notes.push("Codex 需要在 /hooks 里重新信任一次。".into());
        }
        if p.dsh {
            notes.push("DeepSeek Harness 重启后生效。".into());
        }
    }
    offer_onboarding(app);
    say(app, "Perch", &notes.join(""), false);
}

// ---------- 首次启动的接入询问 ----------

/// 询问卡片里的一项
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OfferItem {
    pub agent: Agent,
    pub name: &'static str,
    /// 现在就能接入（DSH 还没装命令行时不能）
    pub ready: bool,
    pub hint: Option<String>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OnboardResult {
    pub agent: Agent,
    pub name: &'static str,
    pub error: Option<String>,
}

/// 本机装了、但还没接入的 agent
fn offer_candidates(app: &AppHandle) -> Vec<OfferItem> {
    overview(app)
        .agents
        .into_iter()
        .filter(|a| a.detected && !a.enabled)
        .map(|a| {
            let (ready, hint) = match a.agent {
                Agent::ClaudeCode => (true, None),
                Agent::Codex => (true, Some("接入后要在 Codex 里输入 /hooks 信任一次".to_string())),
                Agent::Dsh if dsh::find_dsh().is_none() => {
                    (false, Some("要先在 DeepSeek Harness 菜单「管理 dsh 命令…」里安装命令行".to_string()))
                }
                Agent::Dsh => (true, Some("接入后重启 DeepSeek Harness 生效".to_string())),
            };
            OfferItem { agent: a.agent, name: a.name, ready, hint }
        })
        .collect()
}

fn mark_onboarded(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut cfg = state.config.lock().unwrap();
    if !cfg.agents.onboarded {
        cfg.agents.onboarded = true;
        crate::config::save(&cfg);
    }
}

/// 第一次启动时问一次要不要接入；什么都没检测到就直接记为问过
fn offer_onboarding(app: &AppHandle) {
    if params(app).onboarded {
        return;
    }
    let items = offer_candidates(app);
    if items.is_empty() {
        mark_onboarded(app);
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // 等桌宠出现（全屏时先不问）；最多等 30 分钟，这次没问成就下次启动再问
        tokio::time::sleep(Duration::from_secs(3)).await;
        let visible = |app: &AppHandle| {
            let s = app.state::<AppState>();
            let s = s.shared.lock().unwrap();
            s.ready && !s.pet_hidden
        };
        let mut waited = 0;
        while !visible(&app) {
            if waited >= 30 * 60 {
                return;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
            waited += 5;
        }
        *app.state::<AppState>().agents.offer.lock().unwrap() = Some(items.clone());
        let bubble_open = app.state::<AppState>().shared.lock().unwrap().bubble_visible;
        if !bubble_open {
            windows::open_bubble(&app, "speak");
        }
        let _ = app.emit_to(windows::BUBBLE, "agent-offer", items);
    });
}

pub fn offer_pending(app: &AppHandle) -> Option<Vec<OfferItem>> {
    app.state::<AppState>().agents.offer.lock().unwrap().clone()
}

/// 用户回答了询问：记为问过，接入选中的（逐个尝试，失败的带上原因）
pub fn onboard(app: &AppHandle, agents: &[Agent]) -> Vec<OnboardResult> {
    *app.state::<AppState>().agents.offer.lock().unwrap() = None;
    mark_onboarded(app);
    agents
        .iter()
        .map(|&agent| OnboardResult { agent, name: agent.display(), error: set_enabled(app, agent, true).err() })
        .collect()
}

