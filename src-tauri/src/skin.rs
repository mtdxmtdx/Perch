//! 皮肤发现与读取。皮肤目录兼容 Codex 宠物格式：pet.json + spritesheet。
//! 可选的 skin.json 用来覆盖默认的动画映射。

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct SkinEntry {
    /// 皮肤目录的绝对路径，同时作为 id
    pub id: String,
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkinInfo {
    pub id: String,
    pub name: String,
    pub pet: Value,
    pub overrides: Option<Value>,
}

/// 依次扫描的皮肤根目录：Codex 宠物目录、桌宠自己的皮肤目录
fn skin_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".codex").join("pets"));
    }
    roots.push(crate::config::data_dir().join("skins"));
    roots
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    // 有的编辑器会写入 BOM
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

fn entry_for(dir: &Path) -> Option<SkinEntry> {
    let pet = read_json(&dir.join("pet.json"))?;
    let fallback = dir.file_name()?.to_string_lossy().to_string();
    let name = pet
        .get("displayName")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or(fallback);
    Some(SkinEntry {
        id: dir.to_string_lossy().to_string(),
        name,
    })
}

pub fn list() -> Vec<SkinEntry> {
    let mut out = Vec::new();
    for root in skin_roots() {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        let mut dirs: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        out.extend(dirs.iter().filter_map(|d| entry_for(d)));
    }
    out
}

/// 选出要用的皮肤：配置里指定且仍存在的 → uika → 第一个
pub fn resolve(configured: Option<&str>) -> Option<SkinEntry> {
    let all = list();
    if let Some(id) = configured {
        if let Some(e) = all.iter().find(|e| e.id == id) {
            return Some(e.clone());
        }
    }
    all.iter()
        .find(|e| Path::new(&e.id).file_name().is_some_and(|n| n == "uika"))
        .or_else(|| all.first())
        .cloned()
}

pub fn load_info(entry: &SkinEntry) -> Result<SkinInfo, String> {
    let dir = Path::new(&entry.id);
    let pet = read_json(&dir.join("pet.json")).ok_or("pet.json 读取失败")?;
    Ok(SkinInfo {
        id: entry.id.clone(),
        name: entry.name.clone(),
        pet,
        overrides: read_json(&dir.join("skin.json")),
    })
}

pub fn image_bytes(entry: &SkinEntry) -> Result<Vec<u8>, String> {
    let dir = Path::new(&entry.id);
    let pet = read_json(&dir.join("pet.json")).ok_or("pet.json 读取失败")?;
    let rel = pet
        .get("spritesheetPath")
        .and_then(Value::as_str)
        .unwrap_or("spritesheet.webp");
    let path = dir.join(rel);
    // 不允许 spritesheetPath 跳出皮肤目录
    let canon_dir = dir.canonicalize().map_err(|e| e.to_string())?;
    let canon = path.canonicalize().map_err(|e| format!("图集不存在: {e}"))?;
    if !canon.starts_with(&canon_dir) {
        return Err("spritesheetPath 指向了皮肤目录之外".into());
    }
    std::fs::read(canon).map_err(|e| e.to_string())
}
