//! 桌宠说话的统一出口。
//! 桌宠主动说的话（agent 汇报、启动提示、以后的主动聊天）都走 [`say`]：
//! 桌宠看得见时在气泡里说；看不见（全屏、手动隐藏）或要求发通知时发系统通知；开了 TTS 的再读出来。
//! 对话回复是流式显示在气泡里的，不经过 `say`；回复说完时调用 [`reply_finished`]，交给 TTS。

use crate::{tts, windows, AppState};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// 这句话是谁让桌宠说的（TTS 按来源决定读不读）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// agent 汇报：完成、等授权、出错
    Agent,
    /// 程序自己的提示：需要修复、改名迁移等
    System,
    /// 主动聊天、提醒（见 proactive.rs）
    Proactive,
}

fn pet_hidden(app: &AppHandle) -> bool {
    app.state::<AppState>().shared.lock().unwrap().pet_hidden
}

/// 桌宠说一句话。title 只用在系统通知上
pub fn say(app: &AppHandle, title: &str, text: &str, source: Source, notify: bool) {
    let hidden = pet_hidden(app);
    if notify || hidden {
        let _ = app.notification().builder().title(title).body(text).show();
    }
    if !hidden {
        windows::say(app, text);
        tts::speak(app, text, source.into());
    }
}

/// 一条对话回复说完了（气泡里已经流式显示过）
pub fn reply_finished(app: &AppHandle, text: &str) {
    if !pet_hidden(app) {
        tts::speak(app, text, tts::What::Reply);
    }
}
