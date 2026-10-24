//! 生图：trait ImageProvider，第一版实现 OpenAI 兼容的 /images/generations；本地生图留接口。

use crate::config::{Config, Provider};
use crate::files;
use crate::llm::Endpoint;
use crate::store::Store;
use base64::Engine;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

pub trait ImageProvider {
    /// 返回图片的原始字节
    async fn generate(&self, prompt: &str, size: &str) -> Result<Vec<u8>, String>;
}

/// OpenAI 兼容接口。兼容几种常见返回：data[].b64_json、data[].url、images[].url（硅基流动）
pub struct OpenAiCompatImage {
    pub ep: Endpoint,
}

impl ImageProvider for OpenAiCompatImage {
    async fn generate(&self, prompt: &str, size: &str) -> Result<Vec<u8>, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| e.to_string())?;
        let mut req = http
            .post(format!("{}/images/generations", self.ep.base_url))
            .json(&json!({ "model": self.ep.model, "prompt": prompt, "size": size, "n": 1 }));
        if let Some(k) = &self.ep.api_key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|e| format!("生图请求失败: {e}"))?;
        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| format!("生图响应解析失败: {e}"))?;
        if !status.is_success() {
            let msg = body.pointer("/error/message").or_else(|| body.get("message")).and_then(Value::as_str).unwrap_or("未知错误");
            return Err(format!("生图失败 HTTP {}: {msg}", status.as_u16()));
        }
        let first = body.pointer("/data/0").or_else(|| body.pointer("/images/0")).ok_or("生图响应里没有图片")?;
        if let Some(b64) = first.get("b64_json").and_then(Value::as_str) {
            return base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| format!("图片数据无效: {e}"));
        }
        let url = first.get("url").and_then(Value::as_str).ok_or("生图响应里既没有 b64_json 也没有 url")?;
        let bytes = http.get(url).send().await.map_err(|e| format!("下载图片失败: {e}"))?.bytes().await.map_err(|e| e.to_string())?;
        Ok(bytes.to_vec())
    }
}

/// 【留接口】本地生图（以后接 ComfyUI / SD WebUI）
pub struct LocalStub;

impl ImageProvider for LocalStub {
    async fn generate(&self, _prompt: &str, _size: &str) -> Result<Vec<u8>, String> {
        Err("本地生图还没有实现".into())
    }
}

pub enum Backend {
    OpenAiCompat(OpenAiCompatImage),
    #[allow(dead_code)]
    Local(LocalStub),
}

impl Backend {
    pub fn from_config(cfg: &Config) -> Result<Self, String> {
        let p: &Provider = cfg.provider(cfg.roles.image.as_deref()).ok_or("还没有配置生图模型，请在设置 → 模型 → 模型用途里选择")?;
        Ok(Backend::OpenAiCompat(OpenAiCompatImage { ep: Endpoint::from_provider(p) }))
    }

    async fn generate(&self, prompt: &str, size: &str) -> Result<Vec<u8>, String> {
        match self {
            Backend::OpenAiCompat(b) => b.generate(prompt, size).await,
            Backend::Local(b) => b.generate(prompt, size).await,
        }
    }
}

fn sniff_ext(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        "jpg"
    } else if bytes.len() > 12 && &bytes[8..12] == b"WEBP" {
        "webp"
    } else {
        "png"
    }
}

pub fn pictures_dir() -> PathBuf {
    // 测试时（指定了数据目录）不往真实的"图片"文件夹里写
    if crate::config::data_dir_overridden() {
        return crate::config::data_dir().join("pictures");
    }
    // 改名前的图存在"图片\table-pet"，留在原处（聊天记录里写着那个位置），新图存到"图片\Perch"
    dirs::picture_dir().unwrap_or_else(|| crate::config::data_dir()).join("Perch")
}

/// 当天已生成张数，key 形如 "2026-10-24:3"
fn today_count(store: &Store, today: &str) -> u32 {
    store
        .get_state("image_count")
        .and_then(|v| v.split_once(':').filter(|(d, _)| *d == today).and_then(|(_, n)| n.parse().ok()))
        .unwrap_or(0)
}

/// 检查生图模型是否配置、今天是否还有额度；返回今天已用张数
pub fn check_quota(cfg: &Config, store: &Store) -> Result<u32, String> {
    Backend::from_config(cfg)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let used = today_count(store, &today);
    if used >= cfg.image.daily_limit {
        return Err(format!("今天已经画了 {used} 张，达到每日上限（{}），可以在设置里调整", cfg.image.daily_limit));
    }
    Ok(used)
}

pub struct Generated {
    /// attachments 目录里的文件名（聊天里显示用）
    pub file: String,
    /// 另存到"图片"目录的完整路径
    pub saved: PathBuf,
}

/// 检查每日上限 → 生成 → 存到 attachments 和"图片\Perch"
pub async fn generate(cfg: &Config, store: &Store, prompt: &str) -> Result<Generated, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("画什么呢？/draw 后面写上描述".into());
    }
    let used = check_quota(cfg, store)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let backend = Backend::from_config(cfg)?;
    let bytes = backend.generate(prompt, &cfg.image.size).await?;
    store.set_state("image_count", &format!("{today}:{}", used + 1));

    let ext = sniff_ext(&bytes);
    let file = files::save_image(&bytes, ext)?;
    let dir = pictures_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // 同一秒内画多张时不能互相覆盖
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f").to_string();
    let mut saved = dir.join(format!("{stamp}.{ext}"));
    let mut n = 1;
    while saved.exists() {
        n += 1;
        saved = dir.join(format!("{stamp}-{n}.{ext}"));
    }
    std::fs::write(&saved, &bytes).map_err(|e| format!("保存到图片目录失败: {e}"))?;
    Ok(Generated { file, saved })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_count_resets_on_new_day() {
        let s = Store::in_memory();
        assert_eq!(today_count(&s, "2026-10-24"), 0);
        s.set_state("image_count", "2026-10-24:3");
        assert_eq!(today_count(&s, "2026-10-24"), 3);
        assert_eq!(today_count(&s, "2026-10-25"), 0);
    }

    #[test]
    fn sniffs_image_type() {
        assert_eq!(sniff_ext(&[0x89, b'P', b'N', b'G', 0, 0]), "png");
        assert_eq!(sniff_ext(&[0xFF, 0xD8, 0xFF]), "jpg");
        assert_eq!(sniff_ext(b"RIFF\0\0\0\0WEBPVP8 "), "webp");
    }
}
