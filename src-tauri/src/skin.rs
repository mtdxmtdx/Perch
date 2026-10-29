//! 皮肤发现与读取。皮肤目录兼容 Codex 宠物格式：pet.json + spritesheet。
//! 可选的 skin.json 用来覆盖默认的动画映射。
//! 默认皮肤「栖栖」编进程序里（由 tools/bird-skin 生成），不依赖安装目录。

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

/// 内置默认皮肤的 id（不是路径）
pub const BUILTIN_ID: &str = "builtin:perch-bird";
const BUILTIN_PET: &str = include_str!("../resources/skins/perch-bird/pet.json");
const BUILTIN_OVERRIDES: &str = include_str!("../resources/skins/perch-bird/skin.json");
const BUILTIN_SHEET: &[u8] = include_bytes!("../resources/skins/perch-bird/spritesheet.png");

fn builtin_entry() -> SkinEntry {
    let name = serde_json::from_str::<Value>(BUILTIN_PET)
        .ok()
        .and_then(|v| v.get("displayName").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "栖栖".into());
    SkinEntry { id: BUILTIN_ID.into(), name }
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

/// 所有皮肤：内置的排第一，后面是皮肤目录里找到的
pub fn list() -> Vec<SkinEntry> {
    let mut out = vec![builtin_entry()];
    out.extend(user_skins());
    out
}

/// 皮肤目录里找到的（不含内置）
fn user_skins() -> Vec<SkinEntry> {
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

/// 选出要用的皮肤：配置里指定且仍存在的 → 内置的栖栖
pub fn resolve(configured: Option<&str>) -> Option<SkinEntry> {
    if let Some(id) = configured {
        if let Some(e) = list().into_iter().find(|e| e.id == id) {
            return Some(e);
        }
    }
    Some(builtin_entry())
}

/// 内置栖栖之前的默认选法（uika → 皮肤目录里第一个），给老用户迁移用：
/// 没手动选过皮肤的老用户，升级后保持原来看到的形象
pub fn legacy_default() -> Option<SkinEntry> {
    let all = user_skins();
    all.iter()
        .find(|e| Path::new(&e.id).file_name().is_some_and(|n| n == "uika"))
        .or_else(|| all.first())
        .cloned()
}

pub fn load_info(entry: &SkinEntry) -> Result<SkinInfo, String> {
    if entry.id == BUILTIN_ID {
        return Ok(SkinInfo {
            id: entry.id.clone(),
            name: entry.name.clone(),
            pet: serde_json::from_str(BUILTIN_PET).map_err(|e| e.to_string())?,
            overrides: serde_json::from_str(BUILTIN_OVERRIDES).ok(),
        });
    }
    let dir = Path::new(&entry.id);
    let pet = read_json(&dir.join("pet.json")).ok_or("pet.json 读取失败")?;
    Ok(SkinInfo {
        id: entry.id.clone(),
        name: entry.name.clone(),
        pet,
        overrides: read_json(&dir.join("skin.json")),
    })
}

/// 读皮肤目录里的一个文件（图集、骨骼、atlas 等，由渲染器决定读什么）。
/// rel 是相对皮肤目录的路径，不允许跳出皮肤目录。
pub fn file_bytes(entry: &SkinEntry, rel: &str) -> Result<Vec<u8>, String> {
    if entry.id == BUILTIN_ID {
        // 内置皮肤只有一张图集
        return match rel {
            "spritesheet.png" => Ok(BUILTIN_SHEET.to_vec()),
            _ => Err(format!("内置皮肤里没有 {rel}")),
        };
    }
    if rel.is_empty() || Path::new(rel).is_absolute() {
        return Err("皮肤文件路径必须是相对路径".into());
    }
    let dir = Path::new(&entry.id);
    let canon_dir = dir.canonicalize().map_err(|e| e.to_string())?;
    let canon = dir.join(rel).canonicalize().map_err(|e| format!("皮肤文件 {rel} 不存在: {e}"))?;
    if !canon.starts_with(&canon_dir) {
        return Err(format!("{rel} 指向了皮肤目录之外"));
    }
    std::fs::read(canon).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_bird_is_the_default_and_loads() {
        // 没选过、或选的皮肤不在了：用内置栖栖
        assert_eq!(resolve(None).unwrap().id, BUILTIN_ID);
        assert_eq!(resolve(Some(r"C:\不存在的皮肤")).unwrap().id, BUILTIN_ID);
        // 内置的排在列表第一个
        assert_eq!(list()[0].id, BUILTIN_ID);
        let e = builtin_entry();
        assert_eq!(e.name, "栖栖");
        let info = load_info(&e).unwrap();
        assert_eq!(info.pet["spriteVersionNumber"], 2);
        let overrides = info.overrides.unwrap();
        assert_eq!(overrides["animations"]["sleep"]["row"], 9);
        // 第 12 行是"坐在窗沿上"，用 Perch 的 rows 扩展声明
        assert_eq!(overrides["rows"], 12);
        assert_eq!(overrides["animations"]["sit"]["row"], 11);
        let png = file_bytes(&e, "spritesheet.png").unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // 图集是 8×12 格、每格 192×208
        let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert_eq!((w, h), (192 * 8, 208 * 12));
        assert!(file_bytes(&e, "skin.json").is_err());
        // 老用户迁移时不会选到内置的
        assert!(legacy_default().is_none_or(|l| l.id != BUILTIN_ID));
    }

    #[test]
    fn skin_files_stay_inside_the_skin_dir() {
        let root = std::env::temp_dir().join(format!("perch-skin-test-{}", std::process::id()));
        let dir = root.join("bird");
        std::fs::create_dir_all(dir.join("anim")).unwrap();
        std::fs::write(dir.join("pet.json"), r#"{"displayName":"测试"}"#).unwrap();
        std::fs::write(dir.join("anim").join("a.skel"), b"skel").unwrap();
        std::fs::write(root.join("secret.txt"), b"secret").unwrap();
        let e = entry_for(&dir).unwrap();
        assert_eq!(file_bytes(&e, "anim/a.skel").unwrap(), b"skel");
        assert!(file_bytes(&e, "../secret.txt").is_err());
        assert!(file_bytes(&e, &root.join("secret.txt").to_string_lossy()).is_err());
        assert!(file_bytes(&e, "").is_err());
        assert!(file_bytes(&e, "missing.png").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
