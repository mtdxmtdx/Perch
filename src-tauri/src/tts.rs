//! 【留接口】TTS 语音输出。
//!
//! 现在没有任何引擎：默认的 [`NoTts`] 什么都不做；配置 `tts.enabled` 默认关闭，也没有设置界面。
//! 以后接引擎（Windows 自带语音、云端 TTS、本地模型）时：
//! 1. 实现 [`TtsProvider`]；
//! 2. 在 [`make_provider`] 里按 `tts.provider` 返回它；
//! 3. 设置页加开关和音色选择。人格可以以后再加自己的音色，覆盖 `tts.voice`。
//!
//! 调用方只有两个：`speak::say`（桌宠主动说的话）和 `speak::reply_finished`（对话回复说完）。
//! 开始录音、发新消息时调用 [`stop`] 打断正在读的。

use crate::config::TtsParams;
use crate::AppState;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

/// 一个 TTS 引擎
pub trait TtsProvider: Send + Sync {
    /// 和配置里 `tts.provider` 对应的名字
    fn id(&self) -> &'static str;
    /// 合成并开始朗读。在阻塞线程里调用，可以读完再返回，也可以开始播放就返回
    fn speak(&self, text: &str, voice: Option<&str>) -> Result<(), String>;
    /// 打断正在读的
    fn stop(&self) {}
}

/// 没有配置引擎：什么都不读
pub struct NoTts;

impl TtsProvider for NoTts {
    fn id(&self) -> &'static str {
        "none"
    }
    fn speak(&self, _text: &str, _voice: Option<&str>) -> Result<(), String> {
        Ok(())
    }
}

/// 读的是什么（设置里可以分开开关）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    Reply,
    Agent,
    System,
    Proactive,
}

impl From<crate::speak::Source> for What {
    fn from(s: crate::speak::Source) -> Self {
        match s {
            crate::speak::Source::Agent => What::Agent,
            crate::speak::Source::System => What::System,
            crate::speak::Source::Proactive => What::Proactive,
        }
    }
}

/// 按配置的名字造引擎。以后在这里加分支
fn make_provider(id: &str) -> Arc<dyn TtsProvider> {
    match id {
        _ => Arc::new(NoTts),
    }
}

/// 当前引擎，换了 `tts.provider` 才重新造（引擎可能要保持播放状态，好打断）
static CURRENT: Mutex<Option<Arc<dyn TtsProvider>>> = Mutex::new(None);

fn provider(id: &str) -> Arc<dyn TtsProvider> {
    let mut cur = CURRENT.lock().unwrap();
    match cur.as_ref() {
        Some(p) if p.id() == id => p.clone(),
        _ => {
            let p = make_provider(id);
            *cur = Some(p.clone());
            p
        }
    }
}

/// 这类内容要不要读
pub fn wants(p: &TtsParams, what: What) -> bool {
    p.enabled
        && match what {
            What::Reply => p.read_replies,
            What::Agent | What::Proactive => p.read_reports,
            What::System => false,
        }
}

/// 按设置读一段文字（不读就什么都不做）。在后台线程里读，不阻塞调用方
pub fn speak(app: &AppHandle, text: &str, what: What) {
    let p = app.state::<AppState>().config.lock().unwrap().tts.clone();
    if !wants(&p, what) {
        return;
    }
    let text = plain_text(text);
    if text.is_empty() {
        return;
    }
    let engine = provider(&p.provider);
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = engine.speak(&text, p.voice.as_deref()) {
            eprintln!("[tts] 朗读失败：{e}");
        }
    });
}

/// 打断正在读的（开始录音、发新消息时）
pub fn stop() {
    if let Some(p) = CURRENT.lock().unwrap().as_ref() {
        p.stop();
    }
}

/// 读之前去掉 Markdown 记号：代码块换成"（代码略）"，链接只留文字，去掉强调、标题、引用符号
pub fn plain_text(s: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in s.lines() {
        if line.trim_start().starts_with("```") {
            if !in_code {
                out.push_str("（代码略）\n");
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let line = line.trim_start().trim_start_matches(['#', '>']).trim_start();
        let line = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).unwrap_or(line);
        // [文字](链接) → 文字
        let mut rest = line;
        let mut buf = String::new();
        while let Some(i) = rest.find('[') {
            match (rest[i..].find("]("), rest[i..].find(')')) {
                (Some(j), Some(k)) if j < k => {
                    buf.push_str(&rest[..i]);
                    buf.push_str(&rest[i + 1..i + j]);
                    rest = &rest[i + k + 1..];
                }
                _ => break,
            }
        }
        buf.push_str(rest);
        out.push_str(&buf.replace(['*', '`', '_'], ""));
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markdown_for_reading() {
        let md = "# 标题\n**好的**，看 [文档](https://x.y/z)：\n```rust\nfn main() {}\n```\n- 第一条\n> 引用";
        assert_eq!(plain_text(md), "标题\n好的，看 文档：\n（代码略）\n第一条\n引用");
        assert_eq!(plain_text("   "), "");
    }

    #[test]
    fn reading_is_off_by_default_and_follows_switches() {
        let mut p = TtsParams::default();
        assert!(!wants(&p, What::Reply));
        p.enabled = true;
        assert!(wants(&p, What::Reply) && wants(&p, What::Agent));
        assert!(!wants(&p, What::System));
        p.read_replies = false;
        assert!(!wants(&p, What::Reply));
        assert_eq!(provider("none").id(), "none");
        // 不认识的引擎名退回 NoTts，不会出错
        assert_eq!(provider("某个以后的引擎").id(), "none");
    }
}
