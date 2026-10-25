//! 对话流程：处理附件 → 拼上下文 → 流式回复 → 后台维护（滚动总结、长期记忆）。
//! 通过 "chat-event" 事件把过程广播给气泡窗口和聊天窗口。

use crate::config::{Config, Persona, Provider};
use crate::files::{self, AttachmentIn};
use crate::llm::{self, ChatMessage, Endpoint, ImageUrl, Part};
use crate::memory::{self, FullInjectRetriever, MemoryRetriever};
use crate::store::{AttachmentMeta, Message, StoredReasoning};
use crate::{imagegen, tools, windows, AppState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// 最近几条消息里的图片会原样发给支持视觉的模型，更早的只留占位
const IMAGE_WINDOW: usize = 6;
/// 回复结束后说话动画再保持多久
const TALK_LINGER: Duration = Duration::from_millis(1500);
/// 修改确认框多久没人理就当拒绝
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Default)]
pub struct ChatRuntime {
    busy: AtomicBool,
    cancel: Arc<AtomicBool>,
    maintenance: AtomicBool,
    approvals: Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Decision>>>,
    next_approval: AtomicU64,
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatEvent {
    User { persona: String, message: Message },
    Start { persona: String },
    Delta { persona: String, text: String },
    Done { persona: String, message: Message },
    /// 对话之外加进来的消息（桌宠主动说的话）
    Added { persona: String, message: Message },
    Error { persona: String, error: String },
    /// 工具调用进度。status: running / ok / error / denied
    Tool { persona: String, id: String, title: String, status: String, detail: Option<String> },
}

fn emit(app: &AppHandle, ev: ChatEvent) {
    let _ = app.emit("chat-event", ev);
}

pub(crate) fn set_speech(app: &AppHandle, anim: Option<&'static str>, until: Option<Instant>) {
    let state = app.state::<AppState>();
    let mut s = state.shared.lock().unwrap();
    s.speech = anim;
    s.speech_until = until;
}

pub(crate) fn now_label() -> String {
    let now = chrono::Local::now();
    let week = ["一", "二", "三", "四", "五", "六", "日"][chrono::Datelike::weekday(&now).num_days_from_monday() as usize];
    format!("{}（周{}）", now.format("%Y-%m-%d %H:%M"), week)
}

fn today_label() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 用户消息发给模型的文字：原话 + 文件内容 + 图片转述
fn compose_llm_text(text: &str, blocks: &[String], metas: &[AttachmentMeta]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !text.trim().is_empty() {
        parts.push(text.to_string());
    }
    parts.extend(blocks.iter().cloned());
    for m in metas.iter().filter(|m| m.kind == "image") {
        if let Some(d) = &m.desc {
            parts.push(format!("【图片：{}】（由视觉模型转述）{}", m.name, d));
        }
    }
    parts.join("\n\n")
}

/// 拼装上下文（稳定内容在前，方便命中前缀缓存）：
/// 人格 → 长期记忆 → 滚动总结 → 预设对话 → 未总结的历史消息
pub fn build_context(
    persona: &Persona,
    memory_block: Option<String>,
    summary: Option<String>,
    history: &[Message],
    vision: bool,
    now: &str,
    provider_id: &str,
) -> Vec<ChatMessage> {
    let mut system = persona.system_prompt.trim().to_string();
    if let Some(m) = memory_block {
        system.push_str("\n\n");
        system.push_str(&m);
    }
    if let Some(s) = summary.filter(|s| !s.trim().is_empty()) {
        system.push_str("\n\n## 更早对话的摘要\n");
        system.push_str(&s);
    }
    let mut out = vec![ChatMessage::text("system", system)];
    for d in &persona.begin_dialogs {
        if !d.user.trim().is_empty() && !d.assistant.trim().is_empty() {
            out.push(ChatMessage::text("user", &d.user));
            out.push(ChatMessage::text("assistant", &d.assistant));
        }
    }

    let n = history.len();
    for (i, m) in history.iter().enumerate() {
        let is_last = i + 1 == n;
        let mut text = m.llm_text.clone();
        if is_last && m.role == "user" {
            // 当前时间只放在最后一条，不破坏前面的缓存前缀
            text.push_str(&format!("\n\n（当前时间：{now}）"));
        }
        let images: Vec<&AttachmentMeta> =
            m.attachments.iter().filter(|a| a.kind == "image" && a.desc.is_none()).collect();
        if m.role == "user" && !images.is_empty() {
            if vision && i + IMAGE_WINDOW >= n {
                let mut parts = vec![Part::Text { text }];
                for a in images {
                    match a.file.as_deref().map(files::image_data_url) {
                        Some(Ok(url)) => parts.push(Part::ImageUrl { image_url: ImageUrl { url } }),
                        _ => parts.push(Part::Text { text: format!("【图片：{}（文件已丢失）】", a.name) }),
                    }
                }
                out.push(ChatMessage::parts("user", parts));
                continue;
            }
            for a in images {
                text.push_str(&format!("\n【图片：{}（较早发送，已省略）】", a.name));
            }
        }
        if m.role == "assistant" {
            // 思考过程只传回给产生它的同一个 provider（DeepSeek 带工具时要求完整传回）
            let reasoning = m.reasoning.as_ref().filter(|r| !provider_id.is_empty() && r.provider == provider_id);
            push_tool_history(&mut out, m, reasoning.map_or("", |r| r.tools.as_str()));
            let reply = strip_fake_ops(&text);
            let msg = ChatMessage::text("assistant", if reply.trim().is_empty() { "（已完成）" } else { reply });
            out.push(msg.with_reasoning(reasoning.map_or("", |r| r.final_answer.as_str())));
            continue;
        }
        out.push(ChatMessage::text(&m.role, text));
    }
    out
}

/// 早期版本把"（本轮操作：……）"拼在回复后面，模型学会了模仿这句话而不调用工具，发给模型前去掉
fn strip_fake_ops(text: &str) -> &str {
    match text.rfind("（本轮操作：") {
        Some(i) => text[..i].trim_end(),
        None => text,
    }
}

/// 把这条回复之前执行过的工具调用还原成 assistant(tool_calls) + tool 消息，让模型看到"做事要调用工具"
fn push_tool_history(out: &mut Vec<ChatMessage>, m: &Message, reasoning: &str) {
    let calls: Vec<(&AttachmentMeta, llm::ToolCall)> = m
        .attachments
        .iter()
        .filter(|a| a.kind == "tool")
        .filter_map(|a| {
            let v: Value = serde_json::from_str(a.call.as_deref()?).ok()?;
            Some((a, v))
        })
        .enumerate()
        .map(|(i, (a, v))| {
            let call = llm::ToolCall {
                id: format!("h{}_{i}", m.id),
                kind: "function".into(),
                function: llm::FunctionCall {
                    name: v.get("name").and_then(Value::as_str).unwrap_or_default().into(),
                    arguments: v.get("arguments").and_then(Value::as_str).unwrap_or("{}").into(),
                },
            };
            (a, call)
        })
        .collect();
    if calls.is_empty() {
        return;
    }
    out.push(ChatMessage::assistant_tool_calls("", calls.iter().map(|(_, c)| c.clone()).collect()).with_reasoning(reasoning));
    for (a, c) in &calls {
        let detail = a.desc.clone().unwrap_or_default();
        let result = match a.note.as_deref() {
            Some("ok") if c.function.name == "read_file" => "（读取成功，内容较早，已省略）".to_string(),
            Some("ok") => format!("完成 {detail}"),
            Some("denied") => "用户拒绝了这次修改，文件没有改动。".to_string(),
            _ => format!("失败：{detail}"),
        };
        out.push(ChatMessage::tool_result(&c.id, result));
    }
}

async fn describe_image(ep: &Endpoint, file: &str, user_text: &str) -> Result<String, String> {
    let url = files::image_data_url(file)?;
    let prompt = if user_text.trim().is_empty() {
        "请描述这张图片。".to_string()
    } else {
        format!("用户发这张图时说：{user_text}\n请描述这张图片。")
    };
    llm::chat_once(
        ep,
        &[
            ChatMessage::text(
                "system",
                "你是图片转述助手。用中文客观、具体地描述图片：画面内容、人物和物体、场景、情绪氛围；图中的文字原样转写。不要评论，不要寒暄。",
            ),
            ChatMessage::parts("user", vec![Part::Text { text: prompt }, Part::ImageUrl { image_url: ImageUrl { url } }]),
        ],
        Some(0.2),
    )
    .await
}

struct BusyGuard<'a>(&'a AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// 用户对一次文件修改的决定
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    Allow,
    AllowTurn,
    Deny,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ApprovalRequest {
    id: u64,
    tool: String,
    path: String,
    diff: String,
    added: usize,
    removed: usize,
    created: bool,
}

/// 弹出确认框（在聊天窗口里），等用户决定；超时、点停止都算拒绝
async fn ask_approval(app: &AppHandle, tool: &str, change: &tools::Change) -> Decision {
    let state = app.state::<AppState>();
    let id = state.chat.next_approval.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.chat.approvals.lock().unwrap().insert(id, tx);
    let (added, removed) = change.stats();
    let req = ApprovalRequest {
        id,
        tool: tool.into(),
        path: tools::display(&change.path),
        diff: change.diff(),
        added,
        removed,
        created: change.old.is_none(),
    };
    let _ = app.emit("tool-approval", req);
    let chat_focused = app
        .get_webview_window(windows::CHAT)
        .map(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
        .unwrap_or(false);
    if !chat_focused {
        windows::open_bubble(app, "input");
    }
    set_speech(app, Some("attention"), None);
    let decision = tokio::select! {
        r = rx => r.unwrap_or(Decision::Deny),
        _ = tokio::time::sleep(APPROVAL_TIMEOUT) => Decision::Deny,
        _ = llm::wait_cancel(&state.chat.cancel) => Decision::Deny,
    };
    state.chat.approvals.lock().unwrap().remove(&id);
    let _ = app.emit("tool-approval-done", id);
    set_speech(app, Some("think"), None);
    decision
}

pub fn approve(app: &AppHandle, id: u64, decision: Decision) {
    if let Some(tx) = app.state::<AppState>().chat.approvals.lock().unwrap().remove(&id) {
        let _ = tx.send(decision);
    }
}

/// 一轮对话里的工具执行状态
struct Turn<'a> {
    app: &'a AppHandle,
    cfg: &'a Config,
    persona: String,
    allowed: Vec<PathBuf>,
    allow_all_writes: bool,
    /// 记到助手消息里的操作记录（kind = "tool"）和生成的图片
    log: Vec<AttachmentMeta>,
    images: Vec<AttachmentMeta>,
    seq: u32,
    /// 正在执行的这次调用 {"name","arguments"}，记进日志用来重建历史
    current_call: Option<String>,
}

impl Turn<'_> {
    fn report(&mut self, call_id: &str, title: &str, status: &str, detail: Option<String>) {
        emit(
            self.app,
            ChatEvent::Tool {
                persona: self.persona.clone(),
                id: call_id.into(),
                title: title.into(),
                status: status.into(),
                detail: detail.clone(),
            },
        );
        if status != "running" {
            self.log.push(AttachmentMeta { kind: "tool".into(), name: title.into(), file: None, desc: detail, note: Some(status.into()), call: self.current_call.clone() });
        }
    }

    /// 执行一次工具调用，返回给模型看的结果文字
    async fn run(&mut self, call: &llm::ToolCall) -> String {
        // 界面上区分每次调用用的 id（有的服务不给 call id，每轮都从 call_0 开始）
        self.seq += 1;
        let rid = format!("t{}", self.seq);
        self.current_call = Some(serde_json::json!({ "name": call.function.name, "arguments": call.function.arguments }).to_string());
        let args: Value = match serde_json::from_str(if call.function.arguments.trim().is_empty() { "{}" } else { &call.function.arguments }) {
            Ok(v) => v,
            Err(e) => return format!("参数不是有效的 JSON：{e}"),
        };
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let path = s("path");
        match call.function.name.as_str() {
            "read_file" => {
                let title = format!("读取 {path}");
                self.report(&rid, &title, "running", None);
                let r = tools::resolve(&path, &self.allowed, true).and_then(|p| {
                    tools::read(&p, args.get("offset").and_then(Value::as_u64).map(|v| v as usize), args.get("limit").and_then(Value::as_u64).map(|v| v as usize))
                });
                match r {
                    Ok(text) => {
                        self.report(&rid, &title, "ok", None);
                        text
                    }
                    Err(e) => {
                        self.report(&rid, &title, "error", Some(e.clone()));
                        format!("读取失败：{e}")
                    }
                }
            }
            name @ ("write_file" | "edit_file") => {
                let verb = if name == "write_file" { "写入" } else { "修改" };
                let title = format!("{verb} {path}");
                self.report(&rid, &title, "running", None);
                let planned = tools::resolve(&path, &self.allowed, name == "edit_file").and_then(|p| {
                    if name == "write_file" {
                        tools::plan_write(&p, &s("content"))
                    } else {
                        let all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
                        tools::plan_edit(&p, &s("old_string"), &s("new_string"), all)
                    }
                });
                let change = match planned {
                    Ok(c) => c,
                    Err(e) => {
                        self.report(&rid, &title, "error", Some(e.clone()));
                        return format!("{verb}失败：{e}");
                    }
                };
                if !self.allow_all_writes {
                    match ask_approval(self.app, name, &change).await {
                        Decision::Deny => {
                            self.report(&rid, &title, "denied", None);
                            return "用户拒绝了这次修改，文件没有改动。".into();
                        }
                        Decision::AllowTurn => self.allow_all_writes = true,
                        Decision::Allow => {}
                    }
                }
                let (added, removed) = change.stats();
                match change.apply() {
                    Ok(()) => {
                        let detail = format!("+{added} -{removed}");
                        self.report(&rid, &title, "ok", Some(detail.clone()));
                        format!("已{verb} {}（{detail} 行）", tools::display(&change.path))
                    }
                    Err(e) => {
                        self.report(&rid, &title, "error", Some(e.clone()));
                        format!("{verb}失败：{e}")
                    }
                }
            }
            "generate_image" => {
                let prompt = s("prompt");
                let title = format!("画图：{}", prompt.chars().take(40).collect::<String>());
                self.report(&rid, &title, "running", None);
                let store = &self.app.state::<AppState>().store;
                match imagegen::generate(self.cfg, store, &prompt).await {
                    Ok(g) => {
                        let saved = tools::display(&g.saved);
                        self.report(&rid, &title, "ok", Some(saved.clone()));
                        self.images.push(AttachmentMeta { kind: "image".into(), name: prompt.clone(), file: Some(g.file), desc: Some(prompt), note: Some(saved.clone()), call: None });
                        format!("图片已生成并展示给用户，另存在 {saved}。不要在回复里再描述图片链接。")
                    }
                    Err(e) => {
                        self.report(&rid, &title, "error", Some(e.clone()));
                        format!("生图失败：{e}")
                    }
                }
            }
            other => format!("没有名为 {other} 的工具"),
        }
    }
}

/// 回复里声称完成了文件操作的说法
const CLAIM_WORDS: &[&str] = &[
    "建好", "创建好", "创建了", "新建了", "已创建", "写好", "写入了", "已写入", "写进去", "改好", "修改好", "改完",
    "已修改", "删掉了", "删好", "删除了", "已删除", "保存好", "已保存",
];

/// 模型有时嘴上说"建好啦"却没调用工具（尤其历史里有过这种例子时）
fn claims_file_done(text: &str) -> bool {
    CLAIM_WORDS.iter().any(|w| text.contains(w))
}

const NUDGE: &str = "【系统提示，不是用户说的】你刚才说已经完成了文件操作，但这一轮并没有调用任何工具，文件实际上没有任何变化。\
请现在调用 write_file 或 edit_file 真正完成；如果做不到，就如实告诉用户没有完成。如果你刚才说的根本不是文件操作，就照常回答。不要提到这条提示。";

fn tool_note(allowed: &[PathBuf], with_image: bool) -> String {
    let mut s = String::from("\n\n## 工具\n");
    if !allowed.is_empty() {
        s.push_str("你可以用 read_file / write_file / edit_file 读写用户电脑上的文件，只允许访问这些目录：\n");
        for a in allowed {
            s.push_str(&format!("- {}\n", tools::display(a)));
        }
        s.push_str("只在用户的请求需要时才使用；写入和修改前用户会看到确认框，被拒绝就不要重试同样的修改。\n");
        s.push_str("创建、写入、修改文件必须真的调用 write_file 或 edit_file。没有调用工具时，绝对不要说文件已经创建或改好了。\n");
    }
    if with_image {
        s.push_str("用户明确要求画图时，可以用 generate_image 生成图片。\n");
    }
    s
}

/// 桌宠主动说的话记进对话：用户回复时，模型知道自己刚说了什么
pub(crate) fn add_assistant_message(app: &AppHandle, persona: &str, text: &str) -> Result<(), String> {
    let msg = app
        .state::<AppState>()
        .store
        .insert_message(persona, "assistant", text, text, &[])
        .map_err(|e| e.to_string())?;
    emit(app, ChatEvent::Added { persona: persona.to_string(), message: msg });
    Ok(())
}

/// 正在回复中（主动聊天据此判断能不能打扰）
pub fn is_busy(app: &AppHandle) -> bool {
    app.state::<AppState>().chat.busy.load(Ordering::SeqCst)
}

pub async fn send(app: AppHandle, text: String, attachments: Vec<AttachmentIn>) -> Result<(), String> {
    // 用户说新的话了：打断正在读的
    crate::tts::stop();
    let state = app.state::<AppState>();
    if state.chat.busy.swap(true, Ordering::SeqCst) {
        return Err("上一条还没回完，稍等一下".into());
    }
    let _guard = BusyGuard(&state.chat.busy);
    state.chat.cancel.store(false, Ordering::SeqCst);

    let cfg: Config = state.config.lock().unwrap().clone();
    let persona = cfg.active_persona();
    if text.trim().is_empty() && attachments.is_empty() {
        return Err("说点什么吧".into());
    }
    if let Some(prompt) = text.trim().strip_prefix("/draw") {
        return draw(&app, &cfg, &persona, &text, prompt.trim()).await;
    }
    let provider: Provider = cfg
        .chat_provider(&persona)
        .cloned()
        .ok_or("还没有配置对话模型，请先在设置里添加模型并选为\"主对话\"")?;

    let mut metas = Vec::new();
    let mut blocks = Vec::new();
    for a in attachments {
        let p = files::process(a, cfg.chat.file_chars)?;
        metas.push(p.meta);
        blocks.extend(p.text_block);
    }

    let has_images = metas.iter().any(|m| m.kind == "image");
    if has_images && !provider.supports_vision {
        let vision = cfg
            .provider(cfg.roles.vision.as_deref())
            .ok_or("当前对话模型不支持图片，请在设置里配置一个视觉模型")?;
        set_speech(&app, Some("think"), None);
        let ep = Endpoint::from_provider(vision);
        for m in metas.iter_mut().filter(|m| m.kind == "image") {
            let file = m.file.clone().unwrap_or_default();
            match describe_image(&ep, &file, &text).await {
                Ok(d) => m.desc = Some(d),
                Err(e) => {
                    set_speech(&app, None, None);
                    return Err(format!("视觉模型转述图片失败：{e}"));
                }
            }
        }
    }

    let llm_text = compose_llm_text(&text, &blocks, &metas);
    let user_msg = state
        .store
        .insert_message(&persona.id, "user", &text, &llm_text, &metas)
        .map_err(|e| e.to_string())?;
    emit(&app, ChatEvent::User { persona: persona.id.clone(), message: user_msg });

    let history = state.store.unsummarized(&persona.id).map_err(|e| e.to_string())?;
    let memory_block = memory::prompt_block(&FullInjectRetriever.retrieve(&state.store, &text));
    let summary = state.store.summary(&persona.id).map_err(|e| e.to_string())?;
    let mut messages = build_context(&persona, memory_block, summary, &history, provider.supports_vision, &now_label(), &provider.id);

    // 工具：模型支持、用户开启，且有可用的目录或生图模型
    let allowed = tools::canonical_allowed(&cfg.tools.allowed_dirs);
    let with_image = cfg.provider(cfg.roles.image.as_deref()).is_some();
    let tools_on = cfg.tools.enabled && provider.supports_tools && (!allowed.is_empty() || with_image);
    let mut tool_defs = tools_on.then(|| tools::definitions(with_image));
    if let Some(defs) = tool_defs.as_mut() {
        if allowed.is_empty() {
            // 没配目录时只保留生图
            if let Some(arr) = defs.as_array_mut() {
                arr.retain(|t| t.pointer("/function/name").and_then(Value::as_str) == Some("generate_image"));
            }
        }
        if let Some(llm::Content::Text(sys)) = messages[0].content.as_mut() {
            sys.push_str(&tool_note(&allowed, with_image));
        }
    }

    set_speech(&app, Some("think"), None);
    emit(&app, ChatEvent::Start { persona: persona.id.clone() });
    let reasoning_key = provider.reasoning_flag_key();
    let mut ep = Endpoint::for_chat(&provider, state.store.get_state(&reasoning_key).is_some());
    let cancel = state.chat.cancel.clone();
    let file_tools_on = tool_defs.is_some() && !allowed.is_empty();
    // 每轮最多纠正一次"说做了但没调用工具"
    let mut nudged = false;
    let mut turn = Turn { app: &app, cfg: &cfg, persona: persona.id.clone(), allowed, allow_all_writes: false, log: Vec::new(), images: Vec::new(), seq: 0, current_call: None };
    let mut final_text = String::new();
    // 调用工具那几步的思考 / 最后回答那一步的思考
    let mut tools_reasoning = String::new();
    let mut final_reasoning = String::new();
    let mut calls_used = 0usize;
    let max_calls = cfg.tools.max_calls.max(1);

    let outcome: Result<(), String> = loop {
        let pid = persona.id.clone();
        let app2 = app.clone();
        let mut started = false;
        let need_sep = !final_text.is_empty() || nudged;
        let r = llm::chat_stream(&ep, &messages, persona.temperature, tool_defs.as_ref(), &cancel, |delta| {
            if !started {
                started = true;
                set_speech(&app2, Some("talk"), None);
                if need_sep {
                    emit(&app2, ChatEvent::Delta { persona: pid.clone(), text: "\n\n".into() });
                }
            }
            emit(&app2, ChatEvent::Delta { persona: pid.clone(), text: delta.to_string() });
        })
        .await;
        let out = match r {
            Ok(o) => o,
            Err(e) => break Err(e),
        };
        if out.reasoning_dropped {
            // 记住这个模型不接受推理参数，以后直接不发
            state.store.set_state(&reasoning_key, "1");
            ep.reasoning = None;
        }
        if !out.content.trim().is_empty() {
            if !final_text.is_empty() {
                final_text.push_str("\n\n");
            }
            final_text.push_str(&out.content);
        }
        if out.tool_calls.is_empty() {
            final_reasoning = out.reasoning_content.clone();
        } else {
            tools_reasoning.push_str(&out.reasoning_content);
        }
        if out.tool_calls.is_empty() || cancel.load(Ordering::SeqCst) {
            let faked = file_tools_on && tool_defs.is_some() && turn.log.is_empty() && claims_file_done(&out.content);
            if faked && !nudged && !cancel.load(Ordering::SeqCst) {
                nudged = true;
                messages.push(ChatMessage::text("assistant", out.content.clone()).with_reasoning(&out.reasoning_content));
                messages.push(ChatMessage::text("user", NUDGE));
                // 假话不存进历史，免得再被模仿
                final_text.clear();
                continue;
            }
            break Ok(());
        }

        set_speech(&app, Some("think"), None);
        messages.push(ChatMessage::assistant_tool_calls(&out.content, out.tool_calls.clone()).with_reasoning(&out.reasoning_content));
        for call in &out.tool_calls {
            let result = if cancel.load(Ordering::SeqCst) {
                "用户停止了本轮对话".to_string()
            } else if calls_used >= max_calls {
                format!("本轮工具调用次数已达上限（{max_calls} 次），请根据已有信息直接回答用户")
            } else {
                calls_used += 1;
                turn.run(call).await
            };
            messages.push(ChatMessage::tool_result(&call.id, result));
        }
        if calls_used >= max_calls {
            // 下一次不再提供工具，逼模型给出回答
            tool_defs = None;
        }
    };

    if let Err(e) = outcome {
        set_speech(&app, None, None);
        emit(&app, ChatEvent::Error { persona: persona.id.clone(), error: e.clone() });
        return Err(e);
    }
    if final_text.trim().is_empty() && turn.images.is_empty() && turn.log.is_empty() {
        set_speech(&app, None, None);
        let error = if cancel.load(Ordering::SeqCst) { "已停止".to_string() } else { "模型没有返回内容".to_string() };
        emit(&app, ChatEvent::Error { persona: persona.id.clone(), error: error.clone() });
        return Err(error);
    }

    let mut atts = turn.log.clone();
    atts.extend(turn.images.clone());
    let msg = state
        .store
        .insert_message(&persona.id, "assistant", &final_text, &final_text, &atts)
        .map_err(|e| e.to_string())?;
    if !tools_reasoning.is_empty() || !final_reasoning.is_empty() {
        let r = StoredReasoning { provider: provider.id.clone(), tools: tools_reasoning, final_answer: final_reasoning };
        if let Err(e) = state.store.set_reasoning(msg.id, &r) {
            eprintln!("[chat] 保存思考过程失败: {e}");
        }
    }
    crate::speak::reply_finished(&app, &final_text);
    emit(&app, ChatEvent::Done { persona: persona.id.clone(), message: msg });
    set_speech(&app, Some("talk"), Some(Instant::now() + TALK_LINGER));
    spawn_maintenance(app.clone(), persona);
    Ok(())
}

/// /draw 命令：不经过对话模型，直接生图
async fn draw(app: &AppHandle, cfg: &Config, persona: &Persona, text: &str, prompt: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    if prompt.is_empty() {
        return Err("画什么呢？在 /draw 后面写上描述，比如 /draw 一只在窗台晒太阳的橘猫".into());
    }
    // 额度和配置先检查，失败时不留下一条没有回复的消息
    imagegen::check_quota(cfg, &state.store)?;
    let user_msg = state.store.insert_message(&persona.id, "user", text, text, &[]).map_err(|e| e.to_string())?;
    emit(app, ChatEvent::User { persona: persona.id.clone(), message: user_msg });
    emit(app, ChatEvent::Start { persona: persona.id.clone() });
    set_speech(app, Some("think"), None);
    let title = format!("画图：{}", prompt.chars().take(40).collect::<String>());
    emit(app, ChatEvent::Tool { persona: persona.id.clone(), id: "draw".into(), title: title.clone(), status: "running".into(), detail: None });
    match imagegen::generate(cfg, &state.store, prompt).await {
        Ok(g) => {
            let saved = tools::display(&g.saved);
            let display = "画好啦～".to_string();
            let llm = format!("（应用户的 /draw 命令画了一张图：{prompt}）");
            let img = AttachmentMeta { kind: "image".into(), name: prompt.into(), file: Some(g.file), desc: Some(prompt.into()), note: Some(saved), call: None };
            let msg = state.store.insert_message(&persona.id, "assistant", &display, &llm, &[img]).map_err(|e| e.to_string())?;
            emit(app, ChatEvent::Done { persona: persona.id.clone(), message: msg });
            set_speech(app, Some("talk"), Some(Instant::now() + TALK_LINGER));
            Ok(())
        }
        Err(e) => {
            set_speech(app, None, None);
            emit(app, ChatEvent::Error { persona: persona.id.clone(), error: e.clone() });
            Err(e)
        }
    }
}

pub fn cancel(app: &AppHandle) {
    app.state::<AppState>().chat.cancel.store(true, Ordering::SeqCst);
}

/// 选出要并入总结的最早一段：保留大约一半预算的最近消息，且切在助手回复之后
pub fn pick_summary_cut(history: &[Message], budget: usize) -> Option<usize> {
    let total: usize = history.iter().map(|m| llm::estimate_tokens(&m.llm_text)).sum();
    if total <= budget || history.len() < 4 {
        return None;
    }
    let target_remove = total - budget / 2;
    let mut removed = 0;
    let mut cut = None;
    // 至少留下最后两条，不把正在进行的一轮也总结掉
    for (i, m) in history.iter().enumerate().take(history.len() - 2) {
        removed += llm::estimate_tokens(&m.llm_text);
        if m.role == "assistant" {
            cut = Some(i);
            if removed >= target_remove {
                break;
            }
        }
    }
    cut
}

const SUMMARY_PROMPT: &str = "你负责维护桌宠与用户之间的对话摘要。把新增的对话并入已有摘要，输出更新后的完整摘要：\
按时间顺序记录聊过的话题、用户提到的重要事情和情绪、双方的约定。不超过 800 字，只输出摘要正文。";

async fn summarize_if_needed(state: &AppState, cfg: &Config, persona: &Persona) -> Result<(), String> {
    let history = state.store.unsummarized(&persona.id).map_err(|e| e.to_string())?;
    let Some(cut) = pick_summary_cut(&history, cfg.chat.context_tokens) else { return Ok(()) };
    let ep = Endpoint::from_provider(cfg.aux_provider(persona).ok_or("没有可用的辅助模型")?);
    let old = state.store.summary(&persona.id).map_err(|e| e.to_string())?.unwrap_or_default();
    let dialog: String = history[..=cut]
        .iter()
        .map(|m| format!("{}：{}", if m.role == "user" { "用户" } else { persona.name.as_str() }, m.llm_text))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!("已有摘要：\n{}\n\n新增对话：\n{}", if old.is_empty() { "（无）" } else { &old }, dialog);
    let summary = llm::chat_once(&ep, &[ChatMessage::text("system", SUMMARY_PROMPT), ChatMessage::text("user", user)], Some(0.3)).await?;
    state.store.set_summary(&persona.id, summary.trim()).map_err(|e| e.to_string())?;
    state.store.mark_summarized(&persona.id, history[cut].id).map_err(|e| e.to_string())
}

async fn update_memories(app: &AppHandle, state: &AppState, cfg: &Config, persona: &Persona) -> Result<(), String> {
    let turns: usize = state.store.get_state("memory_turns").and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
    if turns < cfg.chat.memory_extract_every.max(1) {
        state.store.set_state("memory_turns", &turns.to_string());
        return Ok(());
    }
    state.store.set_state("memory_turns", "0");
    let ep = Endpoint::from_provider(cfg.aux_provider(persona).ok_or("没有可用的辅助模型")?);
    let recent = state
        .store
        .recent(&persona.id, cfg.chat.memory_extract_every * 2 + 2)
        .map_err(|e| e.to_string())?;
    memory::extract(&state.store, &ep, &recent, &persona.name, &today_label()).await?;
    memory::consolidate(&state.store, &ep, cfg.chat.memory_limit).await?;
    let _ = app.emit("memories-changed", ());
    Ok(())
}

fn spawn_maintenance(app: AppHandle, persona: Persona) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        if state.chat.maintenance.swap(true, Ordering::SeqCst) {
            return;
        }
        let cfg = state.config.lock().unwrap().clone();
        if let Err(e) = summarize_if_needed(&state, &cfg, &persona).await {
            log::warn!("滚动总结失败: {e}");
            eprintln!("[chat] 滚动总结失败: {e}");
        }
        if let Err(e) = update_memories(&app, &state, &cfg, &persona).await {
            log::warn!("记忆维护失败: {e}");
            eprintln!("[chat] 记忆维护失败: {e}");
        }
        state.chat.maintenance.store(false, Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: i64, role: &str, text: &str, atts: Vec<AttachmentMeta>) -> Message {
        Message {
            id,
            persona: "p".into(),
            role: role.into(),
            display: text.into(),
            llm_text: text.into(),
            attachments: atts,
            created_at: 0,
            reasoning: None,
        }
    }

    fn image(desc: Option<&str>) -> AttachmentMeta {
        AttachmentMeta { kind: "image".into(), name: "a.png".into(), file: Some("missing.png".into()), desc: desc.map(Into::into), note: None, call: None }
    }

    #[test]
    fn context_order_and_time_on_last_message() {
        let mut p = Persona::default();
        p.begin_dialogs = vec![crate::config::DialogPair { user: "你是谁".into(), assistant: "我是小桌宠".into() }];
        let hist = vec![msg(1, "user", "早", vec![]), msg(2, "assistant", "早呀", vec![]), msg(3, "user", "在吗", vec![])];
        let ctx = build_context(&p, Some("## 记忆\n- x".into()), Some("聊过天气".into()), &hist, false, "T", "");
        let v = serde_json::to_value(&ctx).unwrap();
        let sys = v[0]["content"].as_str().unwrap();
        assert!(sys.starts_with("你是栖栖"));
        assert!(sys.find("## 记忆").unwrap() < sys.find("聊过天气").unwrap());
        assert_eq!(v[1]["content"], "你是谁");
        assert_eq!(v[2]["content"], "我是小桌宠");
        assert_eq!(v[3]["content"], "早");
        assert!(v[5]["content"].as_str().unwrap().ends_with("（当前时间：T）"));
        assert_eq!(ctx.len(), 6);
    }

    #[test]
    fn images_only_sent_when_vision_and_recent() {
        let p = Persona::default();
        let hist = vec![msg(1, "user", "看", vec![image(None)])];
        let v = serde_json::to_value(build_context(&p, None, None, &hist, true, "T", "")).unwrap();
        // 文件不存在时退化成文字占位，但仍是 parts 形式
        assert!(v[1]["content"].is_array());
        let v = serde_json::to_value(build_context(&p, None, None, &hist, false, "T", "")).unwrap();
        assert!(v[1]["content"].as_str().unwrap().contains("已省略"));
        // 已经转述过的图片不再重复发送
        let hist = vec![msg(1, "user", "看", vec![image(Some("一只猫"))])];
        let v = serde_json::to_value(build_context(&p, None, None, &hist, true, "T", "")).unwrap();
        assert!(v[1]["content"].is_string());
    }

    #[test]
    fn summary_cut_keeps_recent_and_ends_on_assistant() {
        let long = "字".repeat(1000);
        let hist: Vec<Message> = (0..10)
            .map(|i| msg(i, if i % 2 == 0 { "user" } else { "assistant" }, &long, vec![]))
            .collect();
        let cut = pick_summary_cut(&hist, 4000).unwrap();
        assert_eq!(hist[cut].role, "assistant");
        assert!(cut + 2 < hist.len());
        assert!(pick_summary_cut(&hist, 1_000_000).is_none());
    }

    #[test]
    fn past_tool_calls_rebuilt_as_real_messages() {
        let tool = |name: &str, args: &str, note: &str| AttachmentMeta {
            kind: "tool".into(),
            name: format!("写入 {args}"),
            file: None,
            desc: Some("+1 -0".into()),
            note: Some(note.into()),
            call: Some(serde_json::json!({ "name": name, "arguments": args }).to_string()),
        };
        let mut reply = msg(2, "assistant", "写好啦", vec![tool("write_file", "{\"path\":\"D:\\\\1.txt\"}", "ok"), tool("edit_file", "{}", "denied")]);
        reply.llm_text = "写好啦".into();
        let hist = vec![msg(1, "user", "写个文件", vec![]), reply, msg(3, "user", "再写一个", vec![])];
        let v = serde_json::to_value(build_context(&Persona::default(), None, None, &hist, false, "T", "")).unwrap();
        // system, user, assistant(tool_calls), tool, tool, assistant(文字), user
        assert_eq!(v.as_array().unwrap().len(), 7);
        assert_eq!(v[2]["tool_calls"][0]["function"]["name"], "write_file");
        assert_eq!(v[3]["role"], "tool");
        assert_eq!(v[3]["tool_call_id"], v[2]["tool_calls"][0]["id"]);
        assert!(v[4]["content"].as_str().unwrap().contains("拒绝"));
        assert_eq!(v[5]["content"], "写好啦");
    }

    #[test]
    fn reasoning_replayed_only_to_same_provider() {
        let mut a = msg(2, "assistant", "答案", vec![]);
        a.reasoning = Some(StoredReasoning { provider: "ds".into(), tools: String::new(), final_answer: "我想了想".into() });
        let hist = vec![msg(1, "user", "问", vec![]), a, msg(3, "user", "再问", vec![])];
        let same = serde_json::to_value(build_context(&Persona::default(), None, None, &hist, false, "T", "ds")).unwrap();
        assert_eq!(same[2]["reasoning_content"], "我想了想");
        let other = serde_json::to_value(build_context(&Persona::default(), None, None, &hist, false, "T", "openai")).unwrap();
        assert!(other[2].get("reasoning_content").is_none());
    }

    #[test]
    fn fake_ops_text_is_stripped() {
        let fake = msg(2, "assistant", "建好啦～  （本轮操作：创建 D:\\2.txt（完成））", vec![]);
        let v = serde_json::to_value(build_context(&Persona::default(), None, None, &[msg(1, "user", "建", vec![]), fake], false, "T", "")).unwrap();
        assert_eq!(v[2]["content"], "建好啦～");
        assert_eq!(strip_fake_ops("没有说明"), "没有说明");
    }

    #[test]
    fn detects_claimed_file_operations() {
        assert!(claims_file_done("4.txt 也建好了，在 D:\\desktop 下"));
        assert!(claims_file_done("写好啦，里面现在是你好"));
        assert!(claims_file_done("已删除第一行"));
        assert!(!claims_file_done("今天天气不错，要出去走走吗"));
        assert!(!claims_file_done("要我帮你建一个文件吗？"));
    }

    #[test]
    fn compose_includes_files_and_relayed_images() {
        let t = compose_llm_text("看看", &["【附件：a.txt】\nhi".into()], &[image(Some("一只猫"))]);
        assert!(t.starts_with("看看") && t.contains("【附件：a.txt】") && t.contains("一只猫"));
    }
}
