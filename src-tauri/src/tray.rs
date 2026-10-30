//! 托盘图标和右键菜单（两处用同一套菜单）。

use crate::{config, skin, AppState};
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, Wry};

const SCALES: &[(f64, &str)] = &[(0.5, "50%"), (0.75, "75%"), (1.0, "100%"), (1.25, "125%"), (1.5, "150%")];

pub fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let user_hidden = state.shared.lock().unwrap().user_hidden;

    let current = skin::resolve(cfg.skin.as_deref()).map(|e| e.id);
    let skin_items = skin::list()
        .into_iter()
        .map(|e| {
            let checked = current.as_deref() == Some(e.id.as_str());
            CheckMenuItem::with_id(app, format!("skin:{}", e.id), &e.name, true, checked, None::<&str>)
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let skin_refs: Vec<&dyn IsMenuItem<Wry>> = skin_items.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect();
    let sep = PredefinedMenuItem::separator(app)?;
    let import = MenuItem::with_id(app, "skin_import", "导入皮肤（zip）…", true, None::<&str>)?;
    let none = MenuItem::with_id(app, "noop", "（未找到皮肤，使用占位形象）", false, None::<&str>)?;
    let mut skin_refs = skin_refs;
    if skin_refs.is_empty() {
        skin_refs.push(&none);
    }
    skin_refs.push(&sep);
    skin_refs.push(&import);
    let skins = Submenu::with_items(app, "皮肤", true, &skin_refs)?;

    let scale_items = SCALES
        .iter()
        .map(|(v, label)| {
            let checked = (cfg.scale - v).abs() < 1e-6;
            CheckMenuItem::with_id(app, format!("scale:{v}"), *label, true, checked, None::<&str>)
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let scale_refs: Vec<&dyn IsMenuItem<Wry>> = scale_items.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect();
    let scales = Submenu::with_items(app, "大小", true, &scale_refs)?;

    let persona_items = cfg
        .personas
        .iter()
        .map(|p| CheckMenuItem::with_id(app, format!("persona:{}", p.id), &p.name, true, p.id == cfg.active_persona, None::<&str>))
        .collect::<tauri::Result<Vec<_>>>()?;
    let persona_refs: Vec<&dyn IsMenuItem<Wry>> = persona_items.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect();
    let personas = Submenu::with_items(app, "人格", true, &persona_refs)?;

    Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "open_chat", "聊天窗口", true, None::<&str>)?,
            &personas,
            &PredefinedMenuItem::separator(app)?,
            &CheckMenuItem::with_id(app, "work_mode", "工作模式（不乱走乱跳）", true, cfg.work_mode, None::<&str>)?,
            &CheckMenuItem::with_id(app, "hide_fullscreen", "全屏时自动隐藏", true, cfg.hide_on_fullscreen, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &skins,
            &scales,
            &MenuItem::with_id(app, "reset_pos", "放回任务栏", true, None::<&str>)?,
            &MenuItem::with_id(app, "toggle_visible", if user_hidden { "显示桌宠" } else { "隐藏桌宠" }, true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "open_settings", "设置…", true, None::<&str>)?,
            &MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?,
        ],
    )
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Perch")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            // 左键单击托盘：把藏起来的桌宠叫回来
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                let app = tray.app_handle();
                app.state::<AppState>().shared.lock().unwrap().user_hidden = false;
                refresh_menu(app);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// 托盘「导入皮肤」：选一个 zip，导入后直接换上，结果用气泡说一声
fn import_from_tray(app: &AppHandle) {
    use tauri_plugin_dialog::DialogExt;
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(path) = app
            .dialog()
            .file()
            .set_title("导入皮肤")
            .add_filter("皮肤压缩包", &["zip"])
            .blocking_pick_file()
            .and_then(|p| p.into_path().ok())
        else {
            return;
        };
        let say = |text: &str| crate::speak::say(&app, "Perch", text, crate::speak::Source::System, false);
        match skin::import(&path) {
            Ok(entry) => {
                after_skin_import(&app, &entry, true);
                say(&format!("换上新皮肤「{}」啦～", entry.name));
            }
            Err(e) => say(&format!("皮肤没导入成功：{e}")),
        }
    });
}

/// 导入皮肤之后：刷新菜单里的皮肤列表；apply 时换上它
pub fn after_skin_import(app: &AppHandle, entry: &skin::SkinEntry, apply: bool) {
    if apply {
        let state = app.state::<AppState>();
        let mut cfg = state.config.lock().unwrap();
        cfg.skin = Some(entry.id.clone());
        config::save(&cfg);
        drop(cfg);
        let _ = app.emit_to("pet", "pet-skin-changed", ());
    }
    refresh_menu(app);
}

pub fn refresh_menu(app: &AppHandle) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id("main"), build_menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// 按皮肤格子大小和缩放比例设置桌宠窗口大小
pub fn apply_size(app: &AppHandle) {
    let state = app.state::<AppState>();
    let scale = state.config.lock().unwrap().scale;
    let m = state.shared.lock().unwrap().metrics.clone();
    if let Some(w) = app.get_webview_window("pet") {
        let _ = w.set_size(LogicalSize::new(m.cell_w * scale, m.cell_h * scale));
    }
}

pub fn handle(app: &AppHandle, id: &str) {
    let state = app.state::<AppState>();
    match id {
        "work_mode" | "hide_fullscreen" => {
            let mut cfg = state.config.lock().unwrap();
            let mut s = state.shared.lock().unwrap();
            if id == "work_mode" {
                cfg.work_mode = !cfg.work_mode;
                s.work_mode = cfg.work_mode;
            } else {
                cfg.hide_on_fullscreen = !cfg.hide_on_fullscreen;
                s.hide_on_fullscreen = cfg.hide_on_fullscreen;
            }
            config::save(&cfg);
        }
        "toggle_visible" => {
            let mut s = state.shared.lock().unwrap();
            s.user_hidden = !s.user_hidden;
        }
        "reset_pos" => {
            let mut s = state.shared.lock().unwrap();
            s.user_hidden = false;
            s.reset_requested = true;
        }
        "quit" => app.exit(0),
        "open_chat" => crate::windows::open_chat(app),
        "open_settings" => crate::windows::open_settings(app),
        "skin_import" => import_from_tray(app),
        _ => {
            if let Some(pid) = id.strip_prefix("persona:") {
                crate::commands::activate_persona(app, pid);
                return;
            } else if let Some(path) = id.strip_prefix("skin:") {
                let mut cfg = state.config.lock().unwrap();
                cfg.skin = Some(path.to_string());
                config::save(&cfg);
                drop(cfg);
                let _ = app.emit_to("pet", "pet-skin-changed", ());
            } else if let Some(v) = id.strip_prefix("scale:").and_then(|v| v.parse::<f64>().ok()) {
                let mut cfg = state.config.lock().unwrap();
                cfg.scale = v;
                config::save(&cfg);
                drop(cfg);
                apply_size(app);
            } else {
                return;
            }
        }
    }
    refresh_menu(app);
}
