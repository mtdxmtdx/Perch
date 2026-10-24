//! 语音模型的位置、下载（多个来源依次尝试、断点续传、大小校验）。

use futures_util::StreamExt;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct ModelFile {
    /// 相对模型目录的路径
    pub path: &'static str,
    /// 认可的文件大小（不同来源的同名文件可能版本不同）
    pub sizes: &'static [u64],
    /// 依次尝试的下载地址，国内镜像优先
    pub urls: &'static [&'static str],
}

pub const SENSE_VOICE_DIR: &str = "sense-voice-int8-2025-09-09";

pub const FILES: &[ModelFile] = &[
    ModelFile {
        path: "silero_vad.onnx",
        sizes: &[643_854, 1_807_522],
        urls: &[
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
            "https://hf-mirror.com/csukuangfj/vad/resolve/main/silero_vad.onnx",
            "https://huggingface.co/csukuangfj/vad/resolve/main/silero_vad.onnx",
        ],
    },
    ModelFile {
        path: "sense-voice-int8-2025-09-09/tokens.txt",
        sizes: &[315_894],
        urls: &[
            "https://hf-mirror.com/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09/resolve/main/tokens.txt",
            "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09/resolve/main/tokens.txt",
        ],
    },
    ModelFile {
        path: "sense-voice-int8-2025-09-09/model.int8.onnx",
        sizes: &[237_115_547],
        urls: &[
            "https://hf-mirror.com/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09/resolve/main/model.int8.onnx",
            "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09/resolve/main/model.int8.onnx",
        ],
    },
];

/// 实际要加载的三个文件
#[derive(Clone, Debug)]
pub struct ModelPaths {
    pub sense_voice: PathBuf,
    pub tokens: PathBuf,
    pub vad: PathBuf,
}

/// 默认下载目录：数据目录下的 models
pub fn default_dir() -> PathBuf {
    crate::config::data_dir().join("models")
}

/// 找模型：用户手动指定的 SenseVoice 文件夹优先（里面要有 model.int8.onnx 或 model.onnx，以及 tokens.txt），
/// VAD 总是用下载目录里的。缺文件时返回缺了哪些。
pub fn locate(custom_sense_voice_dir: Option<&str>) -> Result<ModelPaths, Vec<String>> {
    let base = default_dir();
    let sv_dir = custom_sense_voice_dir
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| base.join(SENSE_VOICE_DIR));
    let model = ["model.int8.onnx", "model.onnx"].iter().map(|f| sv_dir.join(f)).find(|p| p.is_file());
    let tokens = sv_dir.join("tokens.txt");
    let vad = base.join("silero_vad.onnx");
    let mut missing = Vec::new();
    if model.is_none() {
        missing.push(format!("{}\\model.int8.onnx", sv_dir.display()));
    }
    if !tokens.is_file() {
        missing.push(tokens.display().to_string());
    }
    if !vad.is_file() {
        missing.push(vad.display().to_string());
    }
    match model {
        Some(sense_voice) if missing.is_empty() => Ok(ModelPaths { sense_voice, tokens, vad }),
        _ => Err(missing),
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub file: String,
    pub downloaded: u64,
    pub total: u64,
    pub source: String,
}

fn host_of(url: &str) -> String {
    url.split('/').nth(2).unwrap_or(url).to_string()
}

/// 下载一个文件：.part 断点续传，下完校验大小再改名
pub(super) async fn fetch(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
    sizes: &[u64],
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(Progress),
) -> Result<(), String> {
    let part = dest.with_extension(format!("{}.part", dest.extension().and_then(|e| e.to_str()).unwrap_or("")));
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut req = http.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req.send().await.map_err(|e| format!("连接 {} 失败：{e}", host_of(url)))?;
    let status = resp.status().as_u16();
    if status == 416 {
        // 已经下完但还没改名
        have = 0;
    }
    if !(200..300).contains(&status) && status != 416 {
        return Err(format!("{} 返回 HTTP {status}", host_of(url)));
    }
    let resumed = status == 206;
    if !resumed {
        have = 0;
    }
    let total = have + resp.content_length().unwrap_or(0);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&part)
        .map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut got = have;
    let mut last = Instant::now() - Duration::from_secs(1);
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        let chunk = chunk.map_err(|e| format!("下载中断：{e}"))?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        got += chunk.len() as u64;
        if last.elapsed() >= Duration::from_millis(300) {
            last = Instant::now();
            on_progress(Progress { file: dest.file_name().unwrap().to_string_lossy().into(), downloaded: got, total, source: host_of(url) });
        }
    }
    drop(file);
    let size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if !sizes.contains(&size) {
        // 内容不对（比如镜像返回了错误页），删掉重来
        let _ = std::fs::remove_file(&part);
        return Err(format!("{} 下载的文件大小不对（{size} 字节）", host_of(url)));
    }
    std::fs::rename(&part, dest).map_err(|e| e.to_string())
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .user_agent("perch")
        .build()
        .map_err(|e| e.to_string())
}

/// 下载语音运行库（sherpa-onnx 官方发布的 Windows 动态库，约 7.5MB）并解压出需要的 DLL
pub async fn download_runtime(cancel: &AtomicBool, mut on_progress: impl FnMut(Progress)) -> Result<(), String> {
    use super::runtime;
    let http = client()?;
    let archive = runtime::archive_path();
    std::fs::create_dir_all(runtime::default_dir()).map_err(|e| e.to_string())?;
    fetch(&http, runtime::ARCHIVE_URL, &archive, &[runtime::ARCHIVE_SIZE], cancel, &mut on_progress)
        .await
        .map_err(|e| format!("语音运行库下载失败：{e}"))?;
    let a = archive.clone();
    tokio::task::spawn_blocking(move || runtime::extract(&a, &runtime::default_dir()))
        .await
        .map_err(|e| e.to_string())??;
    let _ = std::fs::remove_file(&archive);
    Ok(())
}

/// 下载所有缺少的文件；每个文件依次尝试各个来源
pub async fn download_all(cancel: &AtomicBool, mut on_progress: impl FnMut(Progress)) -> Result<(), String> {
    let http = client()?;
    let base = default_dir();
    for f in FILES {
        let dest = base.join(f.path);
        if dest.is_file() && std::fs::metadata(&dest).is_ok_and(|m| f.sizes.contains(&m.len())) {
            continue;
        }
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut errors = Vec::new();
        let mut ok = false;
        for url in f.urls {
            if cancel.load(Ordering::Relaxed) {
                return Err("已取消".into());
            }
            match fetch(&http, url, &dest, f.sizes, cancel, &mut on_progress).await {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) if e == "已取消" => return Err(e),
                Err(e) => errors.push(e),
            }
        }
        if !ok {
            return Err(format!("{} 下载失败：{}", f.path, errors.join("；")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_reports_missing_and_accepts_custom_dir() {
        let dir = std::env::temp_dir().join(format!("tp-voice-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = locate(Some(dir.to_str().unwrap())).unwrap_err();
        assert!(missing.iter().any(|m| m.contains("model.int8.onnx")));
        assert!(missing.iter().any(|m| m.contains("tokens.txt")));
        // 手动指定的目录里放 model.onnx（非量化版）也认
        std::fs::write(dir.join("model.onnx"), b"x").unwrap();
        std::fs::write(dir.join("tokens.txt"), b"x").unwrap();
        let still = locate(Some(dir.to_str().unwrap()));
        // VAD 不在测试环境的下载目录里时仍然报缺
        if let Err(m) = still {
            assert!(m.iter().all(|x| x.contains("silero_vad")));
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
