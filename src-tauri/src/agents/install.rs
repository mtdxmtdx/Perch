//! 往 Claude Code / Codex 的配置里写入（或移除）桌宠的 hooks。
//! 原则：写之前备份；只认、只删桌宠自己加的条目；别的 hooks 原样保留（键顺序也保留）。

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

pub const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "Interrupt",
    "SessionEnd",
];

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HookState {
    /// 没装
    Missing,
    /// 装好了，和当前端口、令牌、程序路径一致
    Ok,
    /// 装过，但端口 / 令牌 / 程序路径变了，需要修复
    Outdated,
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn claude_settings_path() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home().join(".claude")).join("settings.json")
}

pub fn codex_hooks_path() -> PathBuf {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".codex")).join("hooks.json")
}

pub fn claude_handler(port: u16, token: &str) -> Value {
    json!({
        "type": "http",
        "url": format!("http://127.0.0.1:{port}/v1/agent-event/claude-code"),
        "timeout": 3,
        "headers": { "X-Perch-Token": token },
    })
}

fn is_claude_ours(h: &Value) -> bool {
    h.get("url").and_then(Value::as_str).is_some_and(|u| u.contains("/v1/agent-event/claude-code"))
}

/// Codex 只支持命令型 hook。实测 Codex 0.153 在 Windows 上用 `pwsh -NoProfile -Command "<命令>"` 执行
/// （不是 cmd），所以命令直接写成一行 PowerShell：把标准输入的事件 JSON 原样（按字节，避免中文被控制台编码弄乱）
/// POST 给桌宠。不依赖桌宠程序的路径和构建方式；桌宠没运行时静默失败；不往标准输出写东西，退出码总是 0。
/// 只用单引号，Codex 包进 -Command "…" 时不需要转义。
pub fn codex_command(port: u16, token: &str) -> String {
    format!(
        "$ProgressPreference='SilentlyContinue';try{{$i=[Console]::OpenStandardInput();$m=New-Object IO.MemoryStream;$i.CopyTo($m);\
if($m.Length -gt 0){{Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:{port}/v1/agent-event/codex' -Method Post \
-Headers @{{'X-Perch-Token'='{token}'}} -ContentType 'application/json' -Body $m.ToArray() -TimeoutSec 2|Out-Null}}}}catch{{}};exit 0"
    )
}

pub fn codex_handler(port: u16, token: &str) -> Value {
    let cmd = codex_command(port, token);
    json!({ "type": "command", "command": cmd, "commandWindows": cmd, "timeout": 5 })
}

/// 也认旧版写入的 `table-pet.exe --hook codex …`（改名前），修复时会被替换掉
fn is_codex_ours(h: &Value) -> bool {
    ["command", "commandWindows"].iter().any(|k| {
        h.get(k).and_then(Value::as_str).is_some_and(|c| c.contains("/v1/agent-event/codex") || c.contains("--hook codex"))
    })
}

fn read_json(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => Ok(json!({})),
        Ok(t) => serde_json::from_str(t.trim_start_matches('\u{feff}')).map_err(|e| format!("{} 不是有效的 JSON，没有改动：{e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("读取 {} 失败：{e}", path.display())),
    }
}

/// 先写临时文件再改名，避免写到一半损坏原配置
fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.perch-tmp");
    let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&tmp, text).map_err(|e| format!("写入 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

/// 第一次改动前备份原文件（之后不再覆盖这份最原始的备份）
fn backup_once(path: &Path) -> Result<(), String> {
    let bak = path.with_extension("json.perch-backup");
    // 改名前留下的备份才是最原始的那份，有它就不再备份
    let legacy = path.with_extension("json.table-pet-backup");
    if path.is_file() && !bak.exists() && !legacy.exists() {
        std::fs::copy(path, &bak).map_err(|e| format!("备份 {} 失败：{e}", path.display()))?;
    }
    Ok(())
}

/// 从 hooks 对象里删掉桌宠的条目；删空的分组和事件一并去掉
fn strip_ours(hooks: &mut Map<String, Value>, is_ours: fn(&Value) -> bool) {
    for groups in hooks.values_mut() {
        if let Some(arr) = groups.as_array_mut() {
            for g in arr.iter_mut() {
                if let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                    hs.retain(|h| !is_ours(h));
                }
            }
            arr.retain(|g| g.get("hooks").and_then(Value::as_array).is_none_or(|hs| !hs.is_empty()));
        }
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|a| !a.is_empty()));
}

fn hooks_obj(root: &mut Value) -> Result<&mut Map<String, Value>, String> {
    let obj = root.as_object_mut().ok_or("配置文件的顶层不是 JSON 对象")?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    hooks.as_object_mut().ok_or_else(|| "配置里的 hooks 不是对象".to_string())
}

fn install(path: &Path, events: &[&str], handler: &Value, is_ours: fn(&Value) -> bool) -> Result<(), String> {
    let mut root = read_json(path)?;
    backup_once(path)?;
    let hooks = hooks_obj(&mut root)?;
    strip_ours(hooks, is_ours);
    for ev in events {
        let groups = hooks.entry(ev.to_string()).or_insert_with(|| json!([]));
        if let Some(arr) = groups.as_array_mut() {
            arr.push(json!({ "hooks": [handler.clone()] }));
        }
    }
    write_json(path, &root)
}

fn uninstall(path: &Path, is_ours: fn(&Value) -> bool) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let mut root = read_json(path)?;
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else { return Ok(()) };
    strip_ours(hooks, is_ours);
    if hooks.is_empty() {
        if let Some(o) = root.as_object_mut() {
            o.remove("hooks");
        }
    }
    write_json(path, &root)
}

fn state(path: &Path, events: &[&str], handler: &Value, is_ours: fn(&Value) -> bool) -> HookState {
    let Ok(root) = read_json(path) else { return HookState::Missing };
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else { return HookState::Missing };
    let ours = |ev: &str| -> Vec<Value> {
        hooks
            .get(ev)
            .and_then(Value::as_array)
            .map(|gs| {
                gs.iter()
                    .filter_map(|g| g.get("hooks").and_then(Value::as_array))
                    .flatten()
                    .filter(|h| is_ours(h))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    let found: Vec<Vec<Value>> = events.iter().map(|e| ours(e)).collect();
    if found.iter().all(Vec::is_empty) {
        HookState::Missing
    } else if found.iter().all(|f| f.len() == 1 && &f[0] == handler) {
        HookState::Ok
    } else {
        HookState::Outdated
    }
}

pub fn install_claude(port: u16, token: &str) -> Result<(), String> {
    install(&claude_settings_path(), CLAUDE_EVENTS, &claude_handler(port, token), is_claude_ours)
}
pub fn uninstall_claude() -> Result<(), String> {
    uninstall(&claude_settings_path(), is_claude_ours)
}
pub fn claude_state(port: u16, token: &str) -> HookState {
    state(&claude_settings_path(), CLAUDE_EVENTS, &claude_handler(port, token), is_claude_ours)
}

pub fn install_codex(port: u16, token: &str) -> Result<(), String> {
    install(&codex_hooks_path(), CODEX_EVENTS, &codex_handler(port, token), is_codex_ours)
}
pub fn uninstall_codex() -> Result<(), String> {
    uninstall(&codex_hooks_path(), is_codex_ours)
}
pub fn codex_state(port: u16, token: &str) -> HookState {
    state(&codex_hooks_path(), CODEX_EVENTS, &codex_handler(port, token), is_codex_ours)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("tp-install-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn install_keeps_foreign_hooks_and_order_then_uninstall_restores() {
        let dir = tmp();
        let path = dir.join("settings.json");
        let original = r#"{
  "model": "opus",
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash", "hooks": [ { "type": "command", "command": "my-guard.sh" } ] }
    ]
  },
  "permissions": { "allow": ["Read"] }
}"#;
        std::fs::write(&path, original).unwrap();
        let h = claude_handler(47821, "tok");
        install(&path, CLAUDE_EVENTS, &h, is_claude_ours).unwrap();
        assert_eq!(state(&path, CLAUDE_EVENTS, &h, is_claude_ours), HookState::Ok);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // 原有 hook 还在，键顺序没变
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "my-guard.sh");
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["model", "hooks", "permissions"]);
        assert!(dir.join("settings.json.perch-backup").is_file());
        // 重复安装不会叠加
        install(&path, CLAUDE_EVENTS, &h, is_claude_ours).unwrap();
        assert_eq!(state(&path, CLAUDE_EVENTS, &h, is_claude_ours), HookState::Ok);
        // 端口变了：判定为需要修复
        assert_eq!(state(&path, CLAUDE_EVENTS, &claude_handler(1, "tok"), is_claude_ours), HookState::Outdated);
        // 卸载后与原文件等价
        uninstall(&path, is_claude_ours).unwrap();
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after, serde_json::from_str::<Value>(original).unwrap());
        assert_eq!(state(&path, CLAUDE_EVENTS, &h, is_claude_ours), HookState::Missing);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_file_and_bad_json() {
        let dir = tmp();
        let path = dir.join("hooks.json");
        let h = codex_handler(47821, "t");
        install(&path, CODEX_EVENTS, &h, is_codex_ours).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["commandWindows"], codex_command(47821, "t"));
        uninstall(&path, is_codex_ours).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(), json!({}));
        // 旧版写入的 exe 转发命令：认得出来，修复时换成新命令，不会留下两份
        let old = json!({ "type": "command", "command": "\"D:\\table pet\\table-pet.exe\" --hook codex --port 47821 --token t", "timeout": 5 });
        install(&path, CODEX_EVENTS, &old, is_codex_ours).unwrap();
        assert_eq!(state(&path, CODEX_EVENTS, &h, is_codex_ours), HookState::Outdated);
        install(&path, CODEX_EVENTS, &h, is_codex_ours).unwrap();
        assert_eq!(state(&path, CODEX_EVENTS, &h, is_codex_ours), HookState::Ok);
        uninstall(&path, is_codex_ours).unwrap();
        // 用户的配置写坏了：报错，不覆盖
        std::fs::write(&path, "{ not json").unwrap();
        assert!(install(&path, CODEX_EVENTS, &h, is_codex_ours).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        let _ = std::fs::remove_dir_all(dir);
    }
}
