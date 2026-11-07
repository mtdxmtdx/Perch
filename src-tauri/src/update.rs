//! 从 GitHub Releases 检查和安装新版本。只有用户在设置里点了才联网，不在后台检查。
//!
//! 安装包没有代码签名，所以不用 Tauri 官方的更新插件（它要求签名）；
//! 下载后用 GitHub 给的 SHA256 校验，对不上就不安装。

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const LATEST_URL: &str = "https://api.github.com/repos/mtdxmtdx/Perch/releases/latest";
const ASSET_SUFFIX: &str = "_x64-setup.exe";

/// 最新版本的情况
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub page: String,
    #[serde(skip)]
    pub asset_name: String,
    #[serde(skip)]
    pub asset_url: String,
    #[serde(skip)]
    pub sha256: String,
    pub size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub current: String,
    pub newer: bool,
    pub release: Release,
}

#[derive(Serialize, Clone)]
struct Progress {
    done: u64,
    total: u64,
}

fn client(app: &AppHandle) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(format!("Perch/{}", app.package_info().version))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

/// 版本号按数字逐段比较（0.10.0 比 0.9.0 新）
fn is_newer(latest: &str, current: &str) -> bool {
    let parts = |v: &str| v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(latest) > parts(current)
}

/// 解析 /releases/latest。安装包和 SHA256 都要有，缺了不能安全更新
fn parse_release(v: &serde_json::Value) -> Result<Release, String> {
    let text = |key: &str| v.get(key).and_then(|s| s.as_str()).unwrap_or_default().to_string();
    let tag = text("tag_name");
    if tag.is_empty() {
        return Err("GitHub 返回的内容里没有版本号".into());
    }
    let asset = v
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find(|x| x.get("name").and_then(|n| n.as_str()).is_some_and(|n| n.ends_with(ASSET_SUFFIX))))
        .ok_or("这个版本没有安装包")?;
    let field = |key: &str| asset.get(key).and_then(|s| s.as_str()).unwrap_or_default().to_string();
    let sha256 = field("digest").strip_prefix("sha256:").unwrap_or_default().to_lowercase();
    if sha256.len() != 64 {
        return Err("这个版本的安装包没有校验值，请到发布页手动下载".into());
    }
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        notes: text("body"),
        page: text("html_url"),
        asset_name: field("name"),
        asset_url: field("browser_download_url"),
        sha256,
        size: asset.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
    })
}

async fn latest(app: &AppHandle) -> Result<Release, String> {
    let resp = client(app)?
        .get(LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("连不上 GitHub：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub 返回 HTTP {}", resp.status().as_u16()));
    }
    parse_release(&resp.json().await.map_err(|e| format!("读不懂 GitHub 返回的内容：{e}"))?)
}

pub async fn check(app: &AppHandle) -> Result<UpdateCheck, String> {
    let release = latest(app).await?;
    let current = app.package_info().version.to_string();
    Ok(UpdateCheck { newer: is_newer(&release.version, &current), current, release })
}

fn updates_dir() -> PathBuf {
    crate::config::data_dir().join("updates")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 下载最新的安装包并校验，返回文件路径。下载进度通过 update-progress 事件发给设置页
pub async fn download(app: &AppHandle) -> Result<String, String> {
    let release = latest(app).await?;
    let dir = updates_dir();
    // 旧版本留下的安装包不要了
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let part = dir.join(format!("{}.part", release.asset_name));
    let result = save(app, &release, &part).await;
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let path = dir.join(&release.asset_name);
    std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

async fn save(app: &AppHandle, release: &Release, path: &Path) -> Result<(), String> {
    let resp = client(app)?.get(&release.asset_url).send().await.map_err(|e| format!("下载失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载失败：HTTP {}", resp.status().as_u16()));
    }
    let total = resp.content_length().unwrap_or(release.size);
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("下载中断：{e}"))?;
        hasher.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        done += chunk.len() as u64;
        let _ = app.emit("update-progress", Progress { done, total });
    }
    verify(&hex(&hasher.finalize()), &release.sha256)
}

fn verify(actual: &str, expected: &str) -> Result<(), String> {
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err("安装包校验失败，已丢弃，没有安装".into())
    }
}

/// 运行下载好的安装包并退出 Perch。安装包不弹向导，装到原来的位置，装完重新打开 Perch
pub fn install(app: &AppHandle, path: &str) -> Result<(), String> {
    let path = Path::new(path);
    let inside = path.canonicalize().ok().zip(updates_dir().canonicalize().ok()).is_some_and(|(p, d)| p.starts_with(d));
    let is_installer = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(ASSET_SUFFIX));
    if !inside || !is_installer {
        return Err("只能运行下载好的安装包".into());
    }
    Command::new(path).args(["/P", "/R", "/UPDATE"]).spawn().map_err(|e| format!("启动安装包失败：{e}"))?;
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_by_number() {
        assert!(is_newer("0.6.0", "0.5.0"));
        assert!(is_newer("v0.10.0", "0.9.0"));
        assert!(is_newer("1.0.0", "0.99.9"));
        assert!(is_newer("0.5.1", "0.5"));
        assert!(!is_newer("0.5.0", "0.5.0"));
        assert!(!is_newer("0.4.9", "0.5.0"));
    }

    fn release_json() -> serde_json::Value {
        let sha = "A".repeat(64);
        serde_json::json!({
            "tag_name": "v0.6.0",
            "body": "说明",
            "html_url": "https://github.com/mtdxmtdx/Perch/releases/tag/v0.6.0",
            "assets": [
                { "name": "notes.txt", "digest": "sha256:00", "size": 1, "browser_download_url": "https://example.com/n" },
                { "name": "Perch_0.6.0_x64-setup.exe", "digest": format!("sha256:{sha}"), "size": 5, "browser_download_url": "https://example.com/p" },
            ],
        })
    }

    #[test]
    fn parses_release_and_picks_the_installer() {
        let r = parse_release(&release_json()).unwrap();
        assert_eq!((r.version.as_str(), r.asset_name.as_str(), r.size), ("0.6.0", "Perch_0.6.0_x64-setup.exe", 5));
        assert_eq!(r.sha256, "a".repeat(64));
    }

    #[test]
    fn refuses_a_release_it_cannot_verify() {
        let mut v = release_json();
        v["assets"][1]["digest"] = serde_json::Value::Null;
        assert!(parse_release(&v).unwrap_err().contains("校验"));
        let mut v = release_json();
        v["assets"].as_array_mut().unwrap().remove(1);
        assert!(parse_release(&v).unwrap_err().contains("安装包"));
        assert!(parse_release(&serde_json::json!({})).is_err());
    }

    #[test]
    fn hash_mismatch_is_rejected() {
        assert!(verify("ab", "AB").is_ok());
        assert!(verify("ab", "cd").unwrap_err().contains("校验失败"));
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
    }
}
