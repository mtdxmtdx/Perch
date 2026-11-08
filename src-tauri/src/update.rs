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
use std::sync::Mutex;
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

/// 下载和安装进行到哪一步。放在后台，设置页切到别的标签页、关掉再打开都能接着看
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// idle / downloading / installing / ready（下载好了，测试时不安装）/ error
    pub phase: &'static str,
    pub version: String,
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
}

static STATUS: Mutex<Status> = Mutex::new(Status { phase: "idle", version: String::new(), done: 0, total: 0, error: None });

pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

/// 改状态并通知设置页（update-status 事件）
fn set_status(app: &AppHandle, f: impl FnOnce(&mut Status)) {
    let s = {
        let mut g = STATUS.lock().unwrap();
        f(&mut g);
        g.clone()
    };
    let _ = app.emit("update-status", s);
}

/// 开始下载并安装（在后台进行）。已经在下载或安装时不重复开始
pub fn start(app: &AppHandle) {
    {
        let mut g = STATUS.lock().unwrap();
        if matches!(g.phase, "downloading" | "installing") {
            return;
        }
        *g = Status { phase: "downloading", version: String::new(), done: 0, total: 0, error: None };
    }
    set_status(app, |_| {});
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match download(&app).await {
            // 测试用：设了这个环境变量就只下载、不安装（安装包会关掉并重开装好的那个 Perch）
            Ok(_) if std::env::var_os("PERCH_UPDATE_NO_INSTALL").is_some() => set_status(&app, |s| s.phase = "ready"),
            Ok(path) => {
                set_status(&app, |s| s.phase = "installing");
                if let Err(e) = install(&app, &path) {
                    set_status(&app, |s| {
                        s.phase = "error";
                        s.error = Some(e);
                    });
                }
            }
            Err(e) => set_status(&app, |s| {
                s.phase = "error";
                s.error = Some(e);
            }),
        }
    });
}

/// 网络错误的完整原因。reqwest 的说明只有一句 "error sending request for url"，真正的原因（超时、连接被重置、证书……）在 source 里
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        let text = c.to_string();
        if !out.contains(&text) {
            out.push_str("：");
            out.push_str(&text);
        }
        cur = c.source();
    }
    out
}

/// 一次下载尝试的失败：retry 为 true 时可以接着重试（网络断了、服务器临时出错）
struct Failure {
    message: String,
    retry: bool,
}

/// 最多试几次。每次从已经下载的地方接着下（HTTP Range）
const ATTEMPTS: u32 = 4;

/// 下载最新的安装包并校验，返回文件路径。断了会自动重试，从断开的地方接着下
async fn download(app: &AppHandle) -> Result<String, String> {
    let release = latest(app).await?;
    set_status(app, |s| {
        s.version = release.version.clone();
        s.total = release.size;
    });
    let dir = updates_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let part = dir.join(format!("{}.part", release.asset_name));
    // 别的版本留下的文件不要了；同一个版本没下完的留着接着下
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            if e.path() != part {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        match save(app, &release, &part).await {
            Ok(()) => {
                last.clear();
                break;
            }
            Err(f) if f.retry && attempt < ATTEMPTS => {
                eprintln!("[update] 第 {attempt} 次下载没成功，稍后接着下：{}", f.message);
                last = f.message;
                tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
            }
            Err(f) => {
                last = f.message;
                break;
            }
        }
    }
    if !last.is_empty() {
        return Err(format!("{last}（试了几次都没成功；再点一次会从断开的地方接着下）"));
    }
    // 整个文件下完再校验，对不上就删掉重下
    let actual = file_sha256(&part)?;
    if let Err(e) = verify(&actual, &release.sha256) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let path = dir.join(&release.asset_name);
    std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

fn verify(actual: &str, expected: &str) -> Result<(), String> {
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err("安装包校验失败，已丢弃，没有安装".into())
    }
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher).map_err(|e| e.to_string())?;
    Ok(hex(&hasher.finalize()))
}

/// 下载一次：part 里已经有内容就用 Range 接着下
async fn save(app: &AppHandle, release: &Release, part: &Path) -> Result<(), Failure> {
    let net = |what: &str, e: &reqwest::Error| Failure { message: format!("{what}：{}", error_chain(e)), retry: true };
    let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if release.size > 0 && have >= release.size {
        return Ok(()); // 上次已经下完了
    }
    let client = client(app).map_err(|e| Failure { message: e, retry: false })?;
    let mut req = client.get(&release.asset_url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req.send().await.map_err(|e| net("下载失败", &e))?;
    let code = resp.status();
    if !code.is_success() {
        // 5xx 和 429 是服务器临时的问题，可以重试；其他（404、403）重试也没用
        let retry = code.is_server_error() || code.as_u16() == 429;
        return Err(Failure { message: format!("下载失败：HTTP {}", code.as_u16()), retry });
    }
    // 服务器不支持接着下时会返回完整的文件（200），从头写
    let resume = have > 0 && code.as_u16() == 206;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resume)
        .truncate(!resume)
        .open(part)
        .map_err(|e| Failure { message: e.to_string(), retry: false })?;
    let mut done = if resume { have } else { 0 };
    let total = release.size.max(done + resp.content_length().unwrap_or(0));
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| net("下载中断", &e))?;
        file.write_all(&chunk).map_err(|e| Failure { message: e.to_string(), retry: false })?;
        done += chunk.len() as u64;
        set_status(app, |s| {
            s.done = done;
            s.total = total;
        });
    }
    if release.size > 0 && done < release.size {
        return Err(Failure { message: format!("下载中断：只收到 {done} / {} 字节", release.size), retry: true });
    }
    Ok(())
}

/// 运行下载好的安装包并退出 Perch。安装包不弹向导，装到原来的位置，装完重新打开 Perch
fn install(app: &AppHandle, path: &str) -> Result<(), String> {
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
    fn network_errors_keep_the_real_cause() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "error sending request for url")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let e = Outer(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "连接被重置"));
        assert_eq!(error_chain(&e), "error sending request for url：连接被重置");
    }

    #[test]
    fn hashes_a_file_on_disk() {
        let dir = std::env::temp_dir().join(format!("perch-upd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.bin");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(file_sha256(&f).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_mismatch_is_rejected() {
        assert!(verify("ab", "AB").is_ok());
        assert!(verify("ab", "cd").unwrap_err().contains("校验失败"));
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
    }
}
