//! 语音识别运行库：sherpa-onnx 和 onnxruntime 的 DLL。
//! 程序对 sherpa-onnx-c-api.dll 是延迟加载（见 build.rs）：不开语音输入时根本用不到这些 DLL，程序照常启动。
//! 开启后第一次用之前，按完整路径把它们加载进来；之后 sherpa 的调用就落到已加载的这一份上。
//! 先按完整路径加载 onnxruntime.dll，避免系统目录里别的程序留下的旧版被抢先加载。

use sha2::{Digest, Sha256};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 构建脚本下载的官方压缩包名（和 Cargo.toml 里钉死的 sherpa-onnx 版本一致，C 接口要对得上）
#[cfg(test)]
const ARCHIVE_NAME: &str = "sherpa-onnx-v2.0.0-win-x64-shared-MT-Release-lib.tar.bz2";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    TarBz2,
    Tgz,
    Zip,
}

/// 一个下载来源：同一版本（v2.0.0）的 sherpa-onnx Windows 运行库，换了打包方式。
/// 各来源的 DLL 是同一份代码分别构建的（只有 PE 头里的时间戳不同），逐个钉死 SHA256，解压时校验。
pub struct Source {
    pub label: &'static str,
    pub url: &'static str,
    /// 下载后存成的文件名
    pub file: &'static str,
    pub size: u64,
    pub format: Format,
    /// 要取出的 DLL 和它的 SHA256
    pub dlls: &'static [(&'static str, &'static str)],
}

const ORT: (&str, &str) = ("onnxruntime.dll", "7f66f939a881baf4f46a2216496798edf4a1429878b646d12674aa62f27d8a25");
const ORT_SHARED: (&str, &str) =
    ("onnxruntime_providers_shared.dll", "551d0e1fe4c227d8542314ba718d52f4379e0c7bfe729a37c59833a884e27b4d");

/// 依次尝试：npm 国内镜像（快）→ GitHub 官方发布 → NuGet
pub const SOURCES: [Source; 3] = [
    Source {
        label: "npmmirror（国内镜像）",
        url: "https://registry.npmmirror.com/sherpa-onnx-win-x64/-/sherpa-onnx-win-x64-2.0.0.tgz",
        file: "sherpa-onnx-win-x64-2.0.0.tgz",
        size: 8_755_181,
        format: Format::Tgz,
        dlls: &[ORT, ORT_SHARED, ("sherpa-onnx-c-api.dll", "a47b373906ca4c240f50b74144a2df8e71ed451c915baecc02771cb21d3cc3af")],
    },
    Source {
        label: "GitHub（官方发布）",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.0/sherpa-onnx-v2.0.0-win-x64-shared-MT-Release-lib.tar.bz2",
        file: "sherpa-onnx-v2.0.0-win-x64-shared-MT-Release-lib.tar.bz2",
        size: 7_878_613,
        format: Format::TarBz2,
        dlls: &[ORT, ORT_SHARED, ("sherpa-onnx-c-api.dll", "9b6dc14bfab0378fc4569d6b9785cd06036d36efd3a90f0842e42b9e12f0b1ce")],
    },
    Source {
        label: "NuGet",
        url: "https://api.nuget.org/v3-flatcontainer/org.k2fsa.sherpa.onnx.runtime.win-x64/2.0.0/org.k2fsa.sherpa.onnx.runtime.win-x64.2.0.0.nupkg",
        file: "org.k2fsa.sherpa.onnx.runtime.win-x64.2.0.0.nupkg",
        size: 8_390_394,
        format: Format::Zip,
        dlls: &[ORT, ("sherpa-onnx-c-api.dll", "5534797438348994e7e21630044e94924a33c85ce638f954550b6878d56f8c6e")],
    },
];

/// 必须有的 DLL，按加载顺序排（sherpa 依赖 onnxruntime）
const REQUIRED: [&str; 2] = ["onnxruntime.dll", "sherpa-onnx-c-api.dll"];

/// 下载的运行库放在数据目录下
pub fn default_dir() -> PathBuf {
    crate::config::data_dir().join("voice-runtime")
}

pub fn archive_path(source: &Source) -> PathBuf {
    default_dir().join(source.file)
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

/// 从下载的压缩包里取出需要的 DLL，逐个校验 SHA256；全部通过才放进 dir，有一个不对就什么都不动
pub fn extract(archive: &Path, source: &Source, dir: &Path) -> Result<(), String> {
    let staging = dir.join(".staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let result = extract_to(archive, source, &staging).and_then(|got| {
        for name in &got {
            std::fs::rename(staging.join(name), dir.join(name)).map_err(|e| format!("保存 {name} 失败：{e}"))?;
        }
        Ok(())
    });
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// 取出 DLL 到 staging 并校验，返回取出的文件名
fn extract_to(archive: &Path, source: &Source, staging: &Path) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("打开 {} 失败：{e}", archive.display()))?;
    let reader = BufReader::new(file);
    let mut got: Vec<String> = Vec::new();
    // 压缩包里的一个文件：只认文件名在 source.dlls 里的，写出来的同时算哈希
    let mut take = |name: &str, r: &mut dyn Read| -> Result<(), String> {
        let Some((_, want)) = source.dlls.iter().find(|(n, _)| *n == name) else { return Ok(()) };
        let mut out = std::fs::File::create(staging.join(name)).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = r.read(&mut buf).map_err(|e| format!("解压 {name} 失败：{e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        }
        let hash = hex(&hasher.finalize());
        if hash != *want {
            return Err(format!("{name} 的校验值不对，可能下载的文件被改过"));
        }
        got.push(name.to_string());
        Ok(())
    };
    let base_name = |p: &Path| p.file_name().and_then(|n| n.to_str()).map(str::to_string);
    match source.format {
        Format::TarBz2 | Format::Tgz => {
            let decoder: Box<dyn Read> = if source.format == Format::TarBz2 {
                Box::new(bzip2::read::BzDecoder::new(reader))
            } else {
                Box::new(flate2::read::GzDecoder::new(reader))
            };
            let mut tar = tar::Archive::new(decoder);
            for entry in tar.entries().map_err(|e| format!("解压失败：{e}"))? {
                let mut entry = entry.map_err(|e| format!("解压失败：{e}"))?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let Some(name) = base_name(&entry.path().map_err(|e| e.to_string())?) else { continue };
                take(&name, &mut entry)?;
            }
        }
        Format::Zip => {
            let mut zip = zip::ZipArchive::new(reader).map_err(|e| format!("解压失败：{e}"))?;
            for i in 0..zip.len() {
                let mut f = zip.by_index(i).map_err(|e| format!("解压失败：{e}"))?;
                if !f.is_file() {
                    continue;
                }
                let Some(name) = f.enclosed_name().as_deref().and_then(base_name) else { continue };
                take(&name, &mut f)?;
            }
        }
    }
    let missing: Vec<&str> = REQUIRED.iter().copied().filter(|f| !got.iter().any(|g| g == f)).collect();
    if missing.is_empty() {
        Ok(got)
    } else {
        Err(format!("压缩包里没有 {}", missing.join("、")))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
        let github = &SOURCES[1];
        assert_eq!(std::fs::metadata(&archive).unwrap().len(), github.size);
        let dir = std::env::temp_dir().join(format!("perch-rt-{}", uuid::Uuid::new_v4()));
        extract(&archive, github, &dir).unwrap();
        assert!(complete(&dir));
        assert!(dir.join("onnxruntime_providers_shared.dll").is_file());
        assert!(!dir.join(".staging").exists());
        // 拿别的来源的哈希去校验：c-api 对不上，什么都不放进去
        let dir2 = std::env::temp_dir().join(format!("perch-rt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir2).unwrap();
        let wrong = Source { format: Format::TarBz2, ..SOURCES[0] };
        assert!(extract(&archive, &wrong, &dir2).unwrap_err().contains("sherpa-onnx-c-api.dll"));
        assert!(!complete(&dir2) && !dir2.join("onnxruntime.dll").exists());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    /// 联网检查三个来源都还能下、钉死的哈希都对得上（换 sherpa-onnx 版本时跑一下）：
    /// cargo test all_sources_download_and_verify -- --ignored
    #[test]
    #[ignore]
    fn all_sources_download_and_verify() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        for source in &SOURCES {
            let dir = std::env::temp_dir().join(format!("perch-rt-net-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let archive = dir.join(source.file);
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let http = reqwest::Client::builder().user_agent("perch").build().unwrap();
            rt.block_on(super::super::models::fetch(&http, source.url, &archive, &[source.size], &cancel, &mut |_| {}))
                .unwrap_or_else(|e| panic!("{}: {e}", source.label));
            extract(&archive, source, &dir).unwrap_or_else(|e| panic!("{}: {e}", source.label));
            assert!(complete(&dir), "{}", source.label);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// 三种打包格式都能取出 DLL：用小的假压缩包，哈希按假内容算
    #[test]
    fn extracts_and_verifies_each_format() {
        fn sha(b: &[u8]) -> &'static str {
            Box::leak(hex(&Sha256::digest(b)).into_boxed_str())
        }
        let base = std::env::temp_dir().join(format!("perch-rt-fmt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let (ort, capi) = (b"ort".as_slice(), b"capi".as_slice());
        let dlls: &'static [(&str, &str)] =
            Box::leak(vec![("onnxruntime.dll", sha(ort)), ("sherpa-onnx-c-api.dll", sha(capi))].into_boxed_slice());

        // tgz：npm 包的样子
        let tgz = base.join("a.tgz");
        {
            let gz = flate2::write::GzEncoder::new(std::fs::File::create(&tgz).unwrap(), flate2::Compression::fast());
            let mut t = tar::Builder::new(gz);
            for (name, data) in [("package/onnxruntime.dll", ort), ("package/sherpa-onnx-c-api.dll", capi), ("package/README.md", b"x".as_slice())] {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                t.append_data(&mut h, name, data).unwrap();
            }
            t.into_inner().unwrap().finish().unwrap();
        }
        // zip：NuGet 包的样子
        let nupkg = base.join("b.nupkg");
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&nupkg).unwrap());
            for (name, data) in [("runtimes/win-x64/native/onnxruntime.dll", ort), ("runtimes/win-x64/native/sherpa-onnx-c-api.dll", capi)] {
                z.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
                z.write_all(data).unwrap();
            }
            z.finish().unwrap();
        }
        for (archive, format) in [(&tgz, Format::Tgz), (&nupkg, Format::Zip)] {
            let src = Source { label: "t", url: "", file: "", size: 0, format, dlls };
            let dir = base.join(format!("{format:?}"));
            std::fs::create_dir_all(&dir).unwrap();
            extract(archive, &src, &dir).unwrap();
            assert_eq!(std::fs::read(dir.join("sherpa-onnx-c-api.dll")).unwrap(), capi);
            assert!(complete(&dir) && !dir.join("README.md").exists());
        }
        let _ = std::fs::remove_dir_all(base);
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
