//! 新用户引导：还没配对话模型时，在气泡里请用户选一家服务商、粘贴 API Key，测试通过就保存。
//! 第一次启动问一次（选了"以后再说"就不再主动问）；之后聊天时发现没有模型，会再弹出来。

use crate::config::{self, Config, Provider};
use crate::llm::{self, Endpoint};
use crate::{windows, AppState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// 气泡里正挂着引导卡片
static PENDING: AtomicBool = AtomicBool::new(false);

/// 当前人格没有可用的对话模型
pub fn needed(cfg: &Config) -> bool {
    cfg.chat_provider(&cfg.active_persona()).is_none()
}

/// 启动时：还没问过、也还没配模型，就等桌宠出现后在气泡里问
pub fn start(app: &AppHandle) {
    {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap();
        if cfg.model_onboarded || !needed(&cfg) {
            return;
        }
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        // 等桌宠出现（全屏时先不问）；最多等 30 分钟，这次没问成就下次启动再问
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
        request(&app, "speak");
    });
}

/// 弹出引导卡片（聊天时发现没有模型也会调用）
pub fn request(app: &AppHandle, mode: &str) {
    PENDING.store(true, Ordering::SeqCst);
    let bubble_open = app.state::<AppState>().shared.lock().unwrap().bubble_visible;
    if !bubble_open {
        windows::open_bubble(app, mode);
    }
    let _ = app.emit_to(windows::BUBBLE, "model-setup", ());
}

/// 气泡打开时问一下要不要显示卡片
pub fn pending(app: &AppHandle) -> bool {
    PENDING.load(Ordering::SeqCst) && needed(&app.state::<AppState>().config.lock().unwrap())
}

fn mark_done(app: &AppHandle) {
    PENDING.store(false, Ordering::SeqCst);
    let state = app.state::<AppState>();
    let mut cfg = state.config.lock().unwrap();
    if !cfg.model_onboarded {
        cfg.model_onboarded = true;
        config::save(&cfg);
    }
}

/// "以后再说"
pub fn dismiss(app: &AppHandle) {
    mark_done(app);
}

/// 测试连接，通过后加进模型列表、设为主对话、Key 存进凭据管理器。返回测试结果的说明
pub async fn save(app: &AppHandle, mut provider: Provider, key: String) -> Result<String, String> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("先把 API Key 粘贴进来".into());
    }
    if provider.base_url.trim().is_empty() || provider.model.trim().is_empty() {
        return Err("服务商的地址或模型名是空的".into());
    }
    let mut ep = Endpoint::from_provider(&provider);
    ep.api_key = Some(key.clone());
    let msg = llm::test_connection(&ep, true).await?;

    provider.id = new_id();
    config::set_api_key(&provider.id, &key)?;
    {
        let state = app.state::<AppState>();
        let mut cfg = state.config.lock().unwrap();
        cfg.roles.chat = Some(provider.id.clone());
        cfg.providers.push(provider);
        cfg.model_onboarded = true;
        config::save(&cfg);
    }
    PENDING.store(false, Ordering::SeqCst);
    let _ = app.emit("settings-changed", ());
    Ok(msg)
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// 只允许打开这些服务商的控制台页面（拿 Key 用）
const KEY_PAGE_HOSTS: [&str; 5] =
    ["platform.deepseek.com", "bailian.console.aliyun.com", "cloud.siliconflow.cn", "openrouter.ai", "platform.openai.com"];

fn allowed_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    KEY_PAGE_HOSTS.contains(&host)
}

/// 用默认浏览器打开服务商拿 Key 的页面
pub fn open_key_page(url: &str) -> Result<(), String> {
    if !allowed_url(url) {
        return Err("不支持打开这个地址".into());
    }
    #[cfg(windows)]
    {
        use ::windows::core::HSTRING;
        use ::windows::Win32::UI::Shell::ShellExecuteW;
        use ::windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let r = unsafe { ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL) };
        // 返回值大于 32 表示成功
        if r.0 as isize <= 32 {
            return Err("打不开浏览器".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_key_pages_can_be_opened() {
        assert!(allowed_url("https://platform.deepseek.com/api_keys"));
        assert!(allowed_url("https://openrouter.ai/settings/keys"));
        assert!(!allowed_url("http://platform.deepseek.com/api_keys"));
        assert!(!allowed_url("https://platform.deepseek.com.evil.com/"));
        assert!(!allowed_url("file:///C:/Windows/System32/calc.exe"));
        assert!(!allowed_url("https://example.com/?u=platform.openai.com"));
    }

    #[test]
    fn needed_follows_chat_provider() {
        let mut cfg = Config::default();
        assert!(needed(&cfg));
        cfg.providers.push(Provider { id: "a".into(), base_url: "https://x".into(), model: "m".into(), ..Default::default() });
        assert!(needed(&cfg)); // 有模型但没选为主对话
        cfg.roles.chat = Some("a".into());
        assert!(!needed(&cfg));
    }
}
