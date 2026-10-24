pub mod agents;
mod chat;
mod commands;
mod config;
mod files;
mod imagegen;
mod llm;
mod memory;
mod pet;
mod skin;
mod store;
mod tools;
mod tray;
mod voice;
mod windows;

use pet::hittest::Masks;
use pet::{Metrics, Shared, SharedState};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tauri::ipc::Response;
use tauri::{AppHandle, Manager, State, WebviewWindow, WindowEvent};

pub struct AppState {
    pub shared: SharedState,
    pub config: Mutex<config::Config>,
    pub store: store::Store,
    pub chat: chat::ChatRuntime,
    pub voice: voice::VoiceRuntime,
    pub agents: agents::AgentRuntime,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkinMetrics {
    cell_w: f64,
    cell_h: f64,
    mask_w: usize,
    mask_h: usize,
    masks: Vec<Vec<u8>>,
    foot_ratio: f64,
    center_ratio: f64,
    top_ratio: f64,
}

fn current_skin(state: &AppState) -> Option<skin::SkinEntry> {
    let cfg = state.config.lock().unwrap();
    skin::resolve(cfg.skin.as_deref())
}

/// 当前皮肤的 pet.json 等信息；没有任何皮肤时返回 null，前端改用占位形象
#[tauri::command]
fn skin_info(state: State<AppState>) -> Result<Option<skin::SkinInfo>, String> {
    current_skin(&state).map(|e| skin::load_info(&e)).transpose()
}

#[tauri::command]
fn skin_image(state: State<AppState>) -> Result<Response, String> {
    let entry = current_skin(&state).ok_or("没有可用的皮肤")?;
    skin::image_bytes(&entry).map(Response::new)
}

#[tauri::command]
fn set_skin_metrics(app: AppHandle, state: State<AppState>, metrics: SkinMetrics) {
    {
        let mut s = state.shared.lock().unwrap();
        s.metrics = Metrics {
            cell_w: metrics.cell_w,
            cell_h: metrics.cell_h,
            foot_ratio: metrics.foot_ratio.clamp(0.1, 1.0),
            center_ratio: metrics.center_ratio.clamp(0.0, 1.0),
            top_ratio: metrics.top_ratio.clamp(0.0, 0.9),
        };
        s.masks = Masks { w: metrics.mask_w, h: metrics.mask_h, frames: metrics.masks };
        s.frame = 0;
        s.ready = true;
        s.bubble_moved = true;
    }
    tray::apply_size(&app);
}

#[tauri::command]
fn set_frame(state: State<AppState>, index: usize) {
    state.shared.lock().unwrap().frame = index;
}

#[tauri::command]
fn pointer_down(state: State<AppState>) {
    state.shared.lock().unwrap().pointer_pressed = true;
}

#[tauri::command]
fn show_menu(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    let menu = tray::build_menu(&app).map_err(|e| e.to_string())?;
    window.popup_menu(&menu).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 改名前（table pet）的数据目录和 API Key 搬到新名字下
    config::migrate_legacy_dir();
    let cfg = config::load();
    config::migrate_legacy_keys(&cfg.providers.iter().map(|p| p.id.clone()).collect::<Vec<_>>());
    let shared: SharedState = Arc::new(Mutex::new(Shared {
        work_mode: cfg.work_mode,
        hide_on_fullscreen: cfg.hide_on_fullscreen,
        ..Default::default()
    }));
    let store = store::Store::open(&config::data_dir().join("data.db")).expect("打开数据库失败");
    let shared_for_setup = shared.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 再次启动时把已有的桌宠叫出来
            app.state::<AppState>().shared.lock().unwrap().user_hidden = false;
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    // 按住说话：按下开始录音，松开识别
                    match event.state() {
                        tauri_plugin_global_shortcut::ShortcutState::Pressed => {
                            if let Err(e) = voice::start(app) {
                                voice::report_error(app, e);
                            }
                        }
                        tauri_plugin_global_shortcut::ShortcutState::Released => {
                            let app = app.clone();
                            tauri::async_runtime::spawn(async move {
                                let _ = voice::stop(&app, None).await;
                            });
                        }
                    }
                })
                .build(),
        )
        .manage(AppState {
            shared,
            config: Mutex::new(cfg),
            store,
            chat: chat::ChatRuntime::default(),
            voice: voice::VoiceRuntime::default(),
            agents: agents::AgentRuntime::default(),
        })
        .invoke_handler(tauri::generate_handler![
            skin_info,
            skin_image,
            set_skin_metrics,
            set_frame,
            pointer_down,
            show_menu,
            commands::settings_get,
            commands::settings_save,
            commands::provider_set_key,
            commands::provider_test,
            commands::persona_activate,
            commands::chat_history,
            commands::chat_send,
            commands::chat_cancel,
            commands::tool_approve,
            commands::chat_clear,
            commands::attachment_image,
            commands::memory_list,
            commands::memory_save,
            commands::memory_delete,
            commands::bubble_open,
            commands::bubble_close,
            commands::bubble_resize,
            commands::status_resize,
            commands::open_chat,
            commands::open_settings,
            commands::pet_drop_files,
            commands::voice_status,
            commands::voice_download,
            commands::voice_download_cancel,
            commands::voice_start,
            commands::voice_stop,
            commands::voice_cancel,
            commands::agents_overview,
            commands::agents_set,
            commands::agents_offer,
            commands::agents_onboard,
            commands::agents_repair,
        ])
        .on_menu_event(|app, event| tray::handle(app, event.id().as_ref()))
        .on_window_event(|window, event| {
            // 聊天和设置窗口点关闭只是隐藏，下次打开更快，也保留输入状态
            if let WindowEvent::CloseRequested { api, .. } = event {
                if matches!(window.label(), windows::CHAT | windows::SETTINGS) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(move |app| {
            tray::create(app.handle())?;
            if let Err(e) = voice::register_hotkey(app.handle()) {
                eprintln!("[voice] {e}");
            }
            pet::controller::spawn(app.handle().clone(), shared_for_setup);
            agents::start(app.handle());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("启动 Perch 失败");
}
