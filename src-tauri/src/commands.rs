//! M2 的前端命令：设置、对话、记忆、窗口。

use crate::config::{self, AgentsParams, ChatParams, ImageParams, Persona, ProactiveParams, Provider, Roles, ToolParams, VoiceParams};
use crate::files::{self, AttachmentIn};
use crate::llm::{self, Endpoint};
use crate::store::{Memory, Message};
use crate::{chat, skin, tray, windows, AppState};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, State};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    #[serde(flatten)]
    pub provider: Provider,
    pub has_key: bool,
    /// 设置了推理强度，但服务不接受推理参数，已自动忽略
    pub reasoning_ignored: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub providers: Vec<ProviderView>,
    pub roles: Roles,
    pub personas: Vec<Persona>,
    pub active_persona: String,
    pub chat: ChatParams,
    pub tools: ToolParams,
    pub image: ImageParams,
    pub voice: VoiceParams,
    pub agents: AgentsParams,
    pub proactive: ProactiveParams,
    pub skins: Vec<skin::SkinEntry>,
    /// 生成的图片另存到哪
    pub pictures_dir: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsIn {
    pub providers: Vec<Provider>,
    pub roles: Roles,
    pub personas: Vec<Persona>,
    pub active_persona: String,
    pub chat: ChatParams,
    pub tools: ToolParams,
    pub image: ImageParams,
    pub voice: VoiceParams,
    pub agents: AgentsParams,
    pub proactive: ProactiveParams,
}

#[tauri::command]
pub fn settings_get(state: State<AppState>) -> SettingsView {
    let cfg = state.config.lock().unwrap().clone();
    SettingsView {
        providers: cfg
            .providers
            .into_iter()
            .map(|p| ProviderView {
                has_key: config::get_api_key(&p.id).is_some(),
                reasoning_ignored: !p.reasoning_effort.is_empty() && state.store.get_state(&p.reasoning_flag_key()).is_some(),
                provider: p,
            })
            .collect(),
        roles: cfg.roles,
        personas: cfg.personas,
        active_persona: cfg.active_persona,
        chat: cfg.chat,
        tools: cfg.tools,
        image: cfg.image,
        voice: cfg.voice,
        agents: cfg.agents,
        proactive: cfg.proactive,
        skins: skin::list(),
        pictures_dir: crate::tools::display(&crate::imagegen::pictures_dir()),
    }
}

fn validate(s: &SettingsIn) -> Result<(), String> {
    let mut ids = HashSet::new();
    for p in &s.providers {
        if p.id.trim().is_empty() || !ids.insert(p.id.as_str()) {
            return Err(format!("模型 \"{}\" 的 id 为空或重复", p.name));
        }
        if !p.base_url.starts_with("http://") && !p.base_url.starts_with("https://") {
            return Err(format!("模型 \"{}\" 的接口地址要以 http:// 或 https:// 开头", p.name));
        }
        if p.model.trim().is_empty() {
            return Err(format!("模型 \"{}\" 没填模型名", p.name));
        }
    }
    let mut pids = HashSet::new();
    for p in &s.personas {
        if p.id.trim().is_empty() || !pids.insert(p.id.as_str()) {
            return Err(format!("人格 \"{}\" 的 id 为空或重复", p.name));
        }
    }
    if s.personas.is_empty() {
        return Err("至少要保留一个人格".into());
    }
    Ok(())
}

#[tauri::command]
pub fn settings_save(app: AppHandle, state: State<AppState>, settings: SettingsIn) -> Result<(), String> {
    validate(&settings)?;
    crate::voice::parse_hotkey(&settings.voice.hotkey)?;
    let hotkey_changed = {
        let old = &state.config.lock().unwrap().voice;
        old.hotkey != settings.voice.hotkey || old.enabled != settings.voice.enabled
    };
    let (skin_changed, removed): (bool, Vec<String>) = {
        let mut cfg = state.config.lock().unwrap();
        let removed = cfg
            .providers
            .iter()
            .filter(|old| !settings.providers.iter().any(|p| p.id == old.id))
            .map(|p| p.id.clone())
            .collect();
        let old_skin = cfg.skin.clone();
        // 推理强度改了就重新尝试（之前判定"不支持"的记录作废）
        for p in &settings.providers {
            let changed = cfg.providers.iter().find(|o| o.id == p.id).is_none_or(|o| o.reasoning_effort != p.reasoning_effort);
            if changed {
                state.store.delete_state(&p.reasoning_flag_key());
            }
        }
        cfg.providers = settings.providers;
        cfg.roles = settings.roles;
        cfg.personas = settings.personas;
        cfg.active_persona = settings.active_persona;
        cfg.chat = settings.chat;
        cfg.tools = settings.tools;
        cfg.image = settings.image;
        cfg.voice = settings.voice;
        cfg.proactive = settings.proactive;
        // 各接入的开关由"Agent 接入"页单独控制（要改外部配置），这里只保存其余参数
        let (cc, cx, ds, onboarded) = (cfg.agents.claude_code, cfg.agents.codex, cfg.agents.dsh, cfg.agents.onboarded);
        cfg.agents = AgentsParams { claude_code: cc, codex: cx, dsh: ds, onboarded, ..settings.agents };
        cfg.normalize();
        if let Some(skin) = cfg.active_persona().skin {
            cfg.skin = Some(skin);
        }
        config::save(&cfg);
        (cfg.skin != old_skin, removed)
    };
    // 删掉的 provider 顺手清理它的 Key
    for id in removed {
        let _ = config::set_api_key(&id, "");
    }
    if skin_changed {
        let _ = app.emit_to("pet", "pet-skin-changed", ());
    }
    let _ = app.emit("settings-changed", ());
    tray::refresh_menu(&app);
    if hotkey_changed {
        // 设置已保存；快捷键被占用时把原因告诉用户
        crate::voice::register_hotkey(&app).map_err(|e| format!("设置已保存，但{e}"))?;
    }
    Ok(())
}

#[tauri::command]
pub fn provider_set_key(id: String, key: String) -> Result<(), String> {
    config::set_api_key(&id, key.trim())
}

/// 用设置页里还没保存的值测试；key 为空时用已保存的。image_only：这个模型只用来生图
#[tauri::command]
pub async fn provider_test(
    provider: Provider,
    key: Option<String>,
    image_only: Option<bool>,
    embedding_only: Option<bool>,
) -> Result<String, String> {
    let mut ep = Endpoint::from_provider(&provider);
    if let Some(k) = key.filter(|k| !k.trim().is_empty()) {
        ep.api_key = Some(k.trim().to_string());
    }
    if embedding_only.unwrap_or(false) {
        let v = llm::embed(&ep, &["测试".to_string()]).await?;
        return Ok(format!("连接成功，向量维度 {}", v.first().map_or(0, Vec::len)));
    }
    llm::test_connection(&ep, !image_only.unwrap_or(false)).await
}

pub fn activate_persona(app: &AppHandle, id: &str) {
    let state = tauri::Manager::state::<AppState>(app);
    let skin_changed = {
        let mut cfg = state.config.lock().unwrap();
        if !cfg.personas.iter().any(|p| p.id == id) {
            return;
        }
        cfg.active_persona = id.to_string();
        let old = cfg.skin.clone();
        if let Some(skin) = cfg.active_persona().skin {
            cfg.skin = Some(skin);
        }
        config::save(&cfg);
        cfg.skin != old
    };
    if skin_changed {
        let _ = app.emit_to("pet", "pet-skin-changed", ());
    }
    let _ = app.emit("settings-changed", ());
    tray::refresh_menu(app);
}

#[tauri::command]
pub fn persona_activate(app: AppHandle, id: String) {
    activate_persona(&app, &id);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryView {
    pub persona: String,
    pub persona_name: String,
    pub messages: Vec<Message>,
}

#[tauri::command]
pub fn chat_history(state: State<AppState>, before: Option<i64>, limit: Option<usize>) -> Result<HistoryView, String> {
    let persona = state.config.lock().unwrap().active_persona();
    let messages = state
        .store
        .history(&persona.id, before, limit.unwrap_or(50).min(200))
        .map_err(|e| e.to_string())?;
    Ok(HistoryView { persona: persona.id, persona_name: persona.name, messages })
}

#[tauri::command]
pub async fn chat_send(app: AppHandle, text: String, attachments: Vec<AttachmentIn>) -> Result<(), String> {
    chat::send(app, text, attachments).await
}

#[tauri::command]
pub fn chat_cancel(app: AppHandle) {
    chat::cancel(&app);
}

#[tauri::command]
pub fn tool_approve(app: AppHandle, id: u64, decision: chat::Decision) {
    chat::approve(&app, id, decision);
}

#[tauri::command]
pub fn chat_clear(app: AppHandle, state: State<AppState>, persona: String) -> Result<(), String> {
    state.store.clear_persona(&persona).map_err(|e| e.to_string())?;
    let _ = app.emit("chat-cleared", persona);
    Ok(())
}

#[tauri::command]
pub fn attachment_image(file: String) -> Result<Response, String> {
    files::read_saved(&file).map(Response::new)
}

#[tauri::command]
pub fn memory_list(state: State<AppState>) -> Result<Vec<Memory>, String> {
    state.store.memories().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_save(state: State<AppState>, id: Option<i64>, kind: String, content: String) -> Result<(), String> {
    let kind = if kind == "core" { "core" } else { "event" };
    let content = content.trim();
    if content.is_empty() {
        return Err("记忆内容不能为空".into());
    }
    match id {
        Some(id) => state.store.update_memory(id, Some(kind), content),
        None => state.store.add_memory(kind, content).map(|_| ()),
    }
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_delete(state: State<AppState>, id: i64) -> Result<(), String> {
    state.store.delete_memory(id).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationView {
    pub persona: String,
    pub persona_name: String,
    pub affection: f64,
    pub trust: f64,
    pub familiarity: f64,
    /// 此刻的心情（已经按时间回落）
    pub mood: f64,
    /// 给模型看的文字描述
    pub note: String,
}

/// 当前人格的关系数值
#[tauri::command]
pub fn relation_get(state: State<AppState>) -> RelationView {
    let persona = state.config.lock().unwrap().active_persona();
    let r = state.store.relation(&persona.id);
    let now = crate::store::now_secs();
    RelationView {
        persona: persona.id,
        persona_name: persona.name,
        affection: r.affection,
        trust: r.trust,
        familiarity: r.familiarity,
        mood: r.mood_at(now),
        note: r.note(now),
    }
}

/// 手动设置当前人格的关系数值；reset 为 true 时恢复默认
#[tauri::command]
pub fn relation_set(
    app: AppHandle,
    state: State<AppState>,
    affection: f64,
    trust: f64,
    familiarity: f64,
    mood: f64,
    reset: Option<bool>,
) -> Result<(), String> {
    let persona = state.config.lock().unwrap().active_persona();
    if reset.unwrap_or(false) {
        state.store.delete_relation(&persona.id).map_err(|e| e.to_string())?;
    } else {
        let c = |v: f64| if v.is_finite() { v.clamp(0.0, 100.0) } else { 50.0 };
        let r = crate::relation::Relation {
            affection: c(affection),
            trust: c(trust),
            familiarity: c(familiarity),
            mood: c(mood),
            updated_at: crate::store::now_secs(),
        };
        state.store.set_relation(&persona.id, &r).map_err(|e| e.to_string())?;
    }
    let _ = app.emit("relation-changed", ());
    Ok(())
}

#[tauri::command]
pub fn bubble_open(app: AppHandle, mode: String) {
    windows::open_bubble(&app, &mode);
}

#[tauri::command]
pub fn bubble_close(app: AppHandle) {
    windows::close_bubble(&app);
}

#[tauri::command]
pub fn bubble_resize(app: AppHandle, width: f64, height: f64) {
    windows::resize_bubble(&app, width, height);
}

#[tauri::command]
pub fn status_resize(app: AppHandle, width: f64, height: f64) {
    windows::resize_status(&app, width, height);
}

/// 开机自启动开没开（以注册表为准）
#[tauri::command]
pub fn autostart_get() -> bool {
    crate::autostart::is_enabled()
}

#[tauri::command]
pub fn autostart_set(enabled: bool) -> Result<(), String> {
    crate::autostart::set(enabled)
}

#[tauri::command]
pub fn open_chat(app: AppHandle) {
    windows::open_chat(&app);
}

#[tauri::command]
pub fn open_settings(app: AppHandle) {
    windows::open_settings(&app);
}

/// 把文件拖到桌宠身上：打开输入气泡并把文件交给它
#[tauri::command]
pub fn pet_drop_files(app: AppHandle, paths: Vec<String>) {
    windows::open_bubble(&app, "input");
    let _ = app.emit_to(windows::BUBBLE, "bubble-attach", paths);
}

#[tauri::command]
pub fn voice_status(app: AppHandle) -> crate::voice::Status {
    crate::voice::status(&app)
}

#[tauri::command]
pub async fn voice_download(app: AppHandle) -> Result<(), String> {
    crate::voice::download(&app).await
}

#[tauri::command]
pub fn voice_download_cancel(app: AppHandle) {
    crate::voice::cancel_download(&app);
}

#[tauri::command]
pub fn voice_start(app: AppHandle) -> Result<(), String> {
    crate::voice::start(&app)
}

/// send：true 直接发送，false 只返回文字，不传则按设置
#[tauri::command]
pub async fn voice_stop(app: AppHandle, send: Option<bool>) -> Result<String, String> {
    crate::voice::stop(&app, send).await
}

#[tauri::command]
pub fn voice_cancel(app: AppHandle) {
    crate::voice::cancel(&app);
}

#[tauri::command]
pub fn agents_overview(app: AppHandle) -> crate::agents::AgentsOverview {
    crate::agents::overview(&app)
}

#[tauri::command]
pub fn agents_set(app: AppHandle, agent: String, enabled: bool) -> Result<(), String> {
    let a = crate::agents::model::Agent::from_slug(&agent).ok_or("未知的 agent")?;
    crate::agents::set_enabled(&app, a, enabled)
}

/// 首次启动的接入询问：还没回答的那一份（气泡打开时取）
#[tauri::command]
pub fn agents_offer(app: AppHandle) -> Option<Vec<crate::agents::OfferItem>> {
    crate::agents::offer_pending(&app)
}

/// 回答接入询问：接入选中的 agent（空列表 = 以后再说）。装 DSH 插件要跑 dsh 命令，放到后台线程
#[tauri::command]
pub async fn agents_onboard(app: AppHandle, agents: Vec<String>) -> Result<Vec<crate::agents::OnboardResult>, String> {
    let list: Vec<_> = agents.iter().filter_map(|a| crate::agents::model::Agent::from_slug(a)).collect();
    tauri::async_runtime::spawn_blocking(move || crate::agents::onboard(&app, &list)).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn agents_repair(app: AppHandle) -> Result<(), String> {
    crate::agents::repair(&app)
}
