//! 语音识别运行库：sherpa-onnx 和 onnxruntime 的 DLL。
//! 程序对 sherpa-onnx-c-api.dll 是延迟加载（见 build.rs）：不开语音输入时根本用不到这些 DLL，程序照常启动。
//! 开启后第一次用之前，按完整路径把它们加载进来；之后 sherpa 的调用就落到已加载的这一份上。
//! 先按完整路径加载 onnxruntime.dll，避免系统目录里别的程序留下的旧版被抢先加载。

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 和 Cargo.toml 里钉死的 sherpa-onnx 版本一致（C 接口要对得上）
const ARCHIVE_NAME: &str = "sherpa-onnx-v2.0.0-win-x64-shared-MT-Release-lib.tar.bz2";
pub const ARCHIVE_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.0/sherpa-onnx-v2.0.0-win-x64-shared-MT-Release-lib.tar.bz2";
pub const ARCHIVE_SIZE: u64 = 7_878_613;

/// 必须有的 DLL，按加载顺序排（sherpa 依赖 onnxruntime）
const REQUIRED: [&str; 2] = ["onnxruntime.dll", "sherpa-onnx-c-api.dll"];
/// 压缩包里有就一起取出来
const OPTIONAL: [&str; 1] = ["onnxruntime_providers_shared.dll"];

/// 下载的运行库放在数据目录下
pub fn default_dir() -> PathBuf {
    crate::config::data_dir().join("voice-runtime")
}

pub fn archive_path() -> PathBuf {
    default_dir().join(ARCHIVE_NAME)
}

fn complete(dir: &Path) -> bool {
    REQUIRED.iter().all(|f| dir.join(f).is_file())
}

/// 找运行库：设置里指定的文件夹 → 下载的 → 程序旁边（从源码运行时构建脚本会把 DLL 复制过去）
pub fn locate(custom: Option<&str>) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = custom.map(str::trim).filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(d));
    }
    dirs.push(default_dir());
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir);
    }
    dirs.into_iter().find(|d| complete(d))
}

/// 已经从哪个文件夹加载了运行库（一个进程只加载一次，换文件夹要重启才生效）
static LOADED: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn loaded_from() -> Option<PathBuf> {
    LOADED.lock().unwrap().clone()
}

/// 调用任何 sherpa 接口之前先调用它；找不到运行库时返回给用户看的提示
pub fn ensure_loaded(custom: Option<&str>) -> Result<(), String> {
    let mut loaded = LOADED.lock().unwrap();
    if loaded.is_some() {
        return Ok(());
    }
    let dir = locate(custom).ok_or("还没有语音识别运行库，请到 设置 → 语音 里下载语音组件")?;
    for f in REQUIRED {
        load(&dir.join(f))?;
    }
    *loaded = Some(dir);
    Ok(())
}

#[cfg(windows)]
fn load(path: &Path) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};
    // 带完整路径 + LOAD_WITH_ALTERED_SEARCH_PATH：它依赖的 DLL 先在同一个文件夹里找。句柄不释放，整个进程都要用
    unsafe { LoadLibraryExW(&HSTRING::from(path.to_string_lossy().as_ref()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
        .map(|_| ())
        .map_err(|e| format!("加载 {} 失败：{e}", path.display()))
}

#[cfg(not(windows))]
fn load(_path: &Path) -> Result<(), String> {
    Err("语音识别目前只支持 Windows".into())
}

/// 从下载的 tar.bz2 里取出需要的 DLL 放进 dir（先写临时文件再改名）
pub fn extract(archive: &Path, dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("打开 {} 失败：{e}", archive.display()))?;
    let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(BufReader::new(file)));
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut got = Vec::new();
    for entry in tar.entries().map_err(|e| format!("解压失败：{e}"))? {
        let mut entry = entry.map_err(|e| format!("解压失败：{e}"))?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        if !(REQUIRED.contains(&name.as_str()) || OPTIONAL.contains(&name.as_str())) {
            continue;
        }
        let tmp = dir.join(format!("{name}.part"));
        entry.unpack(&tmp).map_err(|e| format!("解压 {name} 失败：{e}"))?;
        std::fs::rename(&tmp, dir.join(&name)).map_err(|e| format!("保存 {name} 失败：{e}"))?;
        got.push(name);
    }
    let missing: Vec<&str> = REQUIRED.iter().copied().filter(|f| !got.iter().any(|g| g == f)).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("压缩包里没有 {}", missing.join("、")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用构建时下载的同一个压缩包验证解压：取出的 DLL 和压缩包里的一致
    #[test]
    fn extracts_required_dlls_from_prebuilt_archive() {
        let archive = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("sherpa-onnx-prebuilt").join(ARCHIVE_NAME);
        if !archive.is_file() {
            eprintln!("跳过：没有 {}", archive.display());
            return;
        }
        assert_eq!(std::fs::metadata(&archive).unwrap().len(), ARCHIVE_SIZE);
        let dir = std::env::temp_dir().join(format!("perch-rt-{}", uuid::Uuid::new_v4()));
        extract(&archive, &dir).unwrap();
        assert!(complete(&dir));
        assert!(dir.join("onnxruntime_providers_shared.dll").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn locate_prefers_custom_dir_and_skips_incomplete_ones() {
        let dir = std::env::temp_dir().join(format!("perch-rt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // 只有一个 DLL：不算
        std::fs::write(dir.join("sherpa-onnx-c-api.dll"), b"x").unwrap();
        assert_ne!(locate(Some(dir.to_str().unwrap())), Some(dir.clone()));
        std::fs::write(dir.join("onnxruntime.dll"), b"x").unwrap();
        assert_eq!(locate(Some(dir.to_str().unwrap())), Some(dir.clone()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
