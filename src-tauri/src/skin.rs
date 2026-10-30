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

// ---------- 导入 ----------

/// 导入时的上限：文件数、单个文件大小、总大小
const IMPORT_MAX_FILES: usize = 2000;
const IMPORT_MAX_FILE: u64 = 100 * 1024 * 1024;
const IMPORT_MAX_TOTAL: u64 = 300 * 1024 * 1024;

/// 从 zip 或文件夹导入皮肤到 `%APPDATA%\Perch\skins\`，返回新皮肤
pub fn import(src: &Path) -> Result<SkinEntry, String> {
    import_into(src, &crate::config::data_dir().join("skins"))
}

/// 导入到 root 下。pet.json 可以在压缩包（文件夹）根目录，也可以在里面某一层；取最浅的那个所在的文件夹。
/// 先解到临时文件夹、检查通过后再改名，失败不留下半个皮肤。
pub fn import_into(src: &Path, root: &Path) -> Result<SkinEntry, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let tmp = root.join(format!(".importing-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let fallback_name = if src.is_dir() {
            copy_dir_skin(src, &tmp)?
        } else {
            extract_zip_skin(src, &tmp)?
        };
        let pet = check_skin(&tmp)?;
        let base = pet
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !sanitize(s).is_empty())
            .unwrap_or(fallback_name);
        let dest = unique_dir(root, &sanitize(&base));
        std::fs::rename(&tmp, &dest).map_err(|e| format!("保存皮肤失败: {e}"))?;
        entry_for(&dest).ok_or_else(|| "导入后读不到 pet.json".to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    result
}

/// 文件夹名里不能用的字符换掉
fn sanitize(s: &str) -> String {
    let s: String = s
        .trim()
        .chars()
        .map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .take(60)
        .collect();
    s.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string()
}

/// 重名时加 -2、-3……
fn unique_dir(root: &Path, base: &str) -> PathBuf {
    let base = if base.is_empty() { "skin" } else { base };
    let mut dest = root.join(base);
    let mut n = 2;
    while dest.exists() {
        dest = root.join(format!("{base}-{n}"));
        n += 1;
    }
    dest
}

/// 解出来的皮肤要能用：pet.json 能读；精灵图集皮肤的图集要在
fn check_skin(dir: &Path) -> Result<Value, String> {
    let pet = read_json(&dir.join("pet.json")).ok_or("pet.json 不是有效的 JSON")?;
    let renderer = read_json(&dir.join("skin.json"))
        .and_then(|v| v.get("renderer").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "sprite".into());
    if renderer == "sprite" {
        let rel = pet.get("spritesheetPath").and_then(Value::as_str).unwrap_or("spritesheet.webp");
        let sheet = dir.join(rel);
        let inside = sheet.canonicalize().ok().zip(dir.canonicalize().ok()).is_some_and(|(s, d)| s.starts_with(d));
        if !inside || !sheet.is_file() {
            return Err(format!("找不到图集 {rel}（pet.json 的 spritesheetPath）"));
        }
    }
    Ok(pet)
}

/// 解压 zip 里 pet.json 所在的那一层。返回备用的皮肤名（那一层的文件夹名或压缩包名）
fn extract_zip_skin(src: &Path, dest: &Path) -> Result<String, String> {
    let file = std::fs::File::open(src).map_err(|e| format!("打不开文件: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("不是有效的 zip: {e}"))?;
    if zip.len() > IMPORT_MAX_FILES {
        return Err(format!("压缩包里文件太多（超过 {IMPORT_MAX_FILES} 个）"));
    }
    // 先找 pet.json：取最浅的；忽略 macOS 打包时带的 __MACOSX
    let mut names = Vec::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index(i).map_err(|e| e.to_string())?;
        // 路径里有 .. 或绝对路径的条目直接跳过（防止解到别处）
        let Some(path) = f.enclosed_name() else { continue };
        if path.starts_with("__MACOSX") || f.is_dir() || f.is_symlink() {
            continue;
        }
        if f.size() > IMPORT_MAX_FILE {
            return Err(format!("{} 太大了", path.display()));
        }
        total += f.size();
        names.push((i, path));
    }
    if total > IMPORT_MAX_TOTAL {
        return Err("压缩包解开后太大了（超过 300MB）".into());
    }
    let pets: Vec<PathBuf> = names
        .iter()
        .filter(|(_, p)| p.file_name().is_some_and(|n| n == "pet.json"))
        .map(|(_, p)| p.parent().unwrap_or(Path::new("")).to_path_buf())
        .collect();
    let depth = pets.iter().map(|p| p.components().count()).min().ok_or("压缩包里没有 pet.json，不像是皮肤")?;
    let top: Vec<&PathBuf> = pets.iter().filter(|p| p.components().count() == depth).collect();
    if top.len() > 1 {
        return Err("压缩包里有好几个皮肤，请一个一个导入".into());
    }
    let prefix = top[0].clone();
    let fallback = prefix
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .or_else(|| src.file_stem().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "skin".into());
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for (i, path) in names {
        let Ok(rel) = path.strip_prefix(&prefix) else { continue };
        let out = dest.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let f = zip.by_index(i).map_err(|e| e.to_string())?;
        let mut w = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        // 声明的大小可能是假的，实际写入也要限制
        let n = std::io::copy(&mut std::io::Read::take(f, IMPORT_MAX_FILE + 1), &mut w).map_err(|e| format!("解压失败: {e}"))?;
        if n > IMPORT_MAX_FILE {
            return Err(format!("{} 太大了", path.display()));
        }
    }
    Ok(fallback)
}

/// 从文件夹复制：pet.json 在这一层，或者下面一层只有一个子文件夹里有
fn copy_dir_skin(src: &Path, dest: &Path) -> Result<String, String> {
    let base = if src.join("pet.json").is_file() {
        src.to_path_buf()
    } else {
        let subs: Vec<PathBuf> = std::fs::read_dir(src)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.join("pet.json").is_file())
            .collect();
        match subs.as_slice() {
            [one] => one.clone(),
            [] => return Err("文件夹里没有 pet.json，不像是皮肤".into()),
            _ => return Err("文件夹里有好几个皮肤，请选其中一个".into()),
        }
    };
    let mut count = 0usize;
    let mut total = 0u64;
    copy_tree(&base, dest, &mut count, &mut total)?;
    Ok(base.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "skin".into()))
}

fn copy_tree(from: &Path, to: &Path, count: &mut usize, total: &mut u64) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from).map_err(|e| e.to_string())?.filter_map(Result::ok) {
        // 不跟随符号链接，免得复制到皮肤文件夹以外的东西
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let path = e.path();
        if ft.is_dir() {
            copy_tree(&path, &to.join(e.file_name()), count, total)?;
        } else if ft.is_file() {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            *count += 1;
            *total += size;
            if *count > IMPORT_MAX_FILES || size > IMPORT_MAX_FILE || *total > IMPORT_MAX_TOTAL {
                return Err("皮肤文件夹太大了".into());
            }
            std::fs::copy(&path, to.join(e.file_name())).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
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

    fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut z, data).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn imports_skin_from_zip_and_folder() {
        let base = std::env::temp_dir().join(format!("perch-import-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("skins");
        std::fs::create_dir_all(&base).unwrap();
        let pet = r#"{"id":"bird","displayName":"小鸟","spritesheetPath":"sheet.png"}"#.as_bytes();

        // 包在一层文件夹里；带 __MACOSX 和想跳出去的条目
        let z1 = base.join("bird-v1.zip");
        write_zip(&z1, &[
            ("bird-v1/pet.json", pet),
            ("bird-v1/sheet.png", b"png"),
            ("bird-v1/extra/notes.txt", b"hi"),
            ("__MACOSX/bird-v1/._pet.json", b"x"),
            ("../evil.txt", b"evil"),
        ]);
        let e = import_into(&z1, &root).unwrap();
        assert_eq!(e.name, "小鸟");
        let dir = PathBuf::from(&e.id);
        assert_eq!(dir.file_name().unwrap(), "bird"); // 用 pet.json 的 id 当文件夹名
        assert!(dir.join("sheet.png").is_file() && dir.join("extra/notes.txt").is_file());
        assert!(!base.join("evil.txt").exists() && !root.join("evil.txt").exists());
        assert!(!dir.join("__MACOSX").exists());

        // 重名：加 -2；pet.json 直接在根目录也行
        let z2 = base.join("flat.zip");
        write_zip(&z2, &[("pet.json", pet), ("sheet.png", b"png")]);
        let e2 = import_into(&z2, &root).unwrap();
        assert_eq!(PathBuf::from(&e2.id).file_name().unwrap(), "bird-2");

        // 缺图集、没有 pet.json、不是 zip、一个包里好几个皮肤：报错，不留临时文件夹
        let z3 = base.join("nosheet.zip");
        write_zip(&z3, &[("pet.json", pet)]);
        assert!(import_into(&z3, &root).unwrap_err().contains("sheet.png"));
        let z4 = base.join("empty.zip");
        write_zip(&z4, &[("readme.txt", b"hi")]);
        assert!(import_into(&z4, &root).unwrap_err().contains("pet.json"));
        std::fs::write(base.join("bad.zip"), b"not a zip").unwrap();
        assert!(import_into(&base.join("bad.zip"), &root).is_err());
        let z5 = base.join("two.zip");
        write_zip(&z5, &[("a/pet.json", pet), ("a/sheet.png", b"p"), ("b/pet.json", pet), ("b/sheet.png", b"p")]);
        assert!(import_into(&z5, &root).unwrap_err().contains("好几个"));
        // 渲染器插件的皮肤不要求图集
        let z6 = base.join("blob.zip");
        write_zip(&z6, &[("pet.json", r#"{"displayName":"团子"}"#.as_bytes()), ("skin.json", br#"{"renderer":"blob"}"#)]);
        assert_eq!(PathBuf::from(import_into(&z6, &root).unwrap().id).file_name().unwrap(), "blob");
        let leftovers = std::fs::read_dir(&root).unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with(".importing")).count();
        assert_eq!(leftovers, 0);

        // 文件夹：pet.json 在下一层的唯一子文件夹里
        let src = base.join("src-folder");
        std::fs::create_dir_all(src.join("My Bird")).unwrap();
        std::fs::write(src.join("My Bird/pet.json"), r#"{"displayName":"文件夹鸟","spritesheetPath":"sheet.png"}"#.as_bytes()).unwrap();
        std::fs::write(src.join("My Bird/sheet.png"), b"png").unwrap();
        let e7 = import_into(&src, &root).unwrap();
        assert_eq!(PathBuf::from(&e7.id).file_name().unwrap(), "My Bird");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn sanitizes_folder_names() {
        assert_eq!(sanitize(" a/b:c*? "), "a_b_c__");
        assert_eq!(sanitize(".."), "");
        assert_eq!(sanitize(&"长".repeat(100)).chars().count(), 60);
    }
}
