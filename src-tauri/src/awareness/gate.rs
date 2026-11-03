//! 截图门禁：锁死名单 > 白名单模式 > 用户拦截名单。
//!
//! 标题只在本地比对，不上传。拒绝原因里写哪一层、哪个程序，不写标题。

use crate::config::AwarenessParams;

/// 门禁看到的窗口信息（标题仅本地用）
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: isize,
    /// 不带 .exe，小写
    pub process: String,
    /// 本地比对用；不会出现在拒绝原因里
    pub title: String,
}

/// 拒绝截图的原因（对外文案不含标题）
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureDeny {
    LockedProcess,
    LockedTitle,
    NotInWhitelist,
    UserBlockedProcess,
    UserBlockedTitle,
    Minimized,
    NoTarget,
    Cloaked,
}

impl CaptureDeny {
    /// 给工具 / 用户看的原因（不含窗口标题）
    pub fn message(&self, process: &str) -> String {
        let p = if process.is_empty() { "未知程序" } else { process };
        match self {
            Self::LockedProcess => format!("目标窗口是 {p}，在锁死名单里（支付/银行/密码管理器），没有截图"),
            Self::LockedTitle => format!("目标窗口标题命中锁死关键字（支付/银行/密码管理器），没有截图"),
            Self::NotInWhitelist => format!("目标窗口是 {p}，不在截图白名单里，没有截图"),
            Self::UserBlockedProcess => format!("目标窗口是 {p}，在拦截名单里，没有截图"),
            Self::UserBlockedTitle => format!("目标窗口标题命中拦截关键字（如无痕模式），没有截图"),
            Self::Minimized => "目标窗口已最小化或不可见，没有截图".into(),
            Self::NoTarget => "找不到最近在用的窗口（不属于 Perch 的前台窗口），没有截图".into(),
            Self::Cloaked => "目标窗口当前不可见（被系统隐藏），没有截图".into(),
        }
    }
}

/// 锁死：进程名（不带 .exe，小写）。不可配置、不可清空。
pub const LOCKED_PROCESSES: &[&str] = &[
    "alipay",
    "alipaywindesktop",
    "alipaysecurity",
    "cloudpay",
    "unionpay",
    "uppay",
    "abc",
    "abchina",
    "icbc",
    "icbccmb",
    "cmbchina",
    "cmbmobilebank",
    "ccb",
    "ccbclient",
    "boc",
    "bocpc",
    "psbc",
    "bankcomm",
    "cmb",
    "spdb",
    "cib",
    "ceb",
    "hxb",
    "cmbc",
    "pinganbank",
    "1password",
    "1password8",
    "keepass",
    "keepassxc",
    "bitwarden",
    "bitwarden-app",
    "lastpass",
    "dashlane",
    "enpass",
    "robosoft",
];

/// 锁死：标题关键字（本地匹配，大小写不敏感）
pub const LOCKED_TITLE_KEYWORDS: &[&str] = &[
    "支付宝",
    "收银台",
    "网上银行",
    "网银",
    "手机银行",
    "paypal",
    "1password",
    "bitwarden",
    "keepass",
    "云闪付",
    "支付密码",
    "银行网银",
];

/// 用户拦截名单预置：进程名
pub fn default_blocklist_processes() -> Vec<String> {
    [
        "weixin",
        "wechat",
        "qq",
        "qqnt",
        "tim",
        "dingtalk",
        "feishu",
        "lark",
        "telegram",
        "telegram.exe",
    ]
    .into_iter()
    .map(|s| s.trim_end_matches(".exe").to_string())
    .collect()
}

/// 用户拦截名单预置：标题关键字（无痕等）
pub fn default_blocklist_title_keywords() -> Vec<String> {
    ["InPrivate", "无痕", "隐身", "Private Browsing", "Incognito"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn norm_proc(s: &str) -> String {
    s.trim().trim_end_matches(".exe").trim_end_matches(".EXE").to_lowercase()
}

fn title_has_keyword(title: &str, keywords: &[impl AsRef<str>]) -> bool {
    let t = title.to_lowercase();
    keywords.iter().any(|k| {
        let k = k.as_ref().trim();
        !k.is_empty() && t.contains(&k.to_lowercase())
    })
}

/// 三层门禁。锁死名单是常量，配置无法清空。
pub fn gate(w: &WindowInfo, p: &AwarenessParams) -> Result<(), CaptureDeny> {
    let proc = norm_proc(&w.process);
    if LOCKED_PROCESSES.iter().any(|x| *x == proc) {
        return Err(CaptureDeny::LockedProcess);
    }
    if title_has_keyword(&w.title, LOCKED_TITLE_KEYWORDS) {
        return Err(CaptureDeny::LockedTitle);
    }
    if p.capture_whitelist_mode {
        let ok = p
            .capture_whitelist
            .iter()
            .any(|x| norm_proc(x) == proc);
        if !ok {
            return Err(CaptureDeny::NotInWhitelist);
        }
    }
    // 用户名单：进程与标题关键字混在同一 Vec 里（设置页可编辑）
    // 约定：不含空格且像进程名的按进程比；否则按标题关键字比。预置里进程与关键字都在这一列表。
    for entry in &p.capture_blocklist_user {
        let e = entry.trim();
        if e.is_empty() {
            continue;
        }
        // 标题关键字：含中文、空格或常见无痕词；或显式以 title: 前缀
        let as_title = e.starts_with("title:")
            || e.contains(' ')
            || e.chars().any(|c| !c.is_ascii())
            || matches!(
                e.to_lowercase().as_str(),
                "inprivate" | "incognito" | "private browsing"
            );
        if as_title {
            let key = e.strip_prefix("title:").unwrap_or(e);
            if title_has_keyword(&w.title, &[key]) {
                return Err(CaptureDeny::UserBlockedTitle);
            }
        } else if norm_proc(e) == proc {
            return Err(CaptureDeny::UserBlockedProcess);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(process: &str, title: &str) -> WindowInfo {
        WindowInfo { hwnd: 1, process: process.into(), title: title.into() }
    }

    fn params() -> AwarenessParams {
        AwarenessParams::default()
    }

    #[test]
    fn locked_list_cannot_be_cleared_by_config() {
        let mut p = params();
        p.capture_blocklist_user.clear();
        p.capture_whitelist.clear();
        p.capture_whitelist_mode = false;
        assert_eq!(gate(&win("alipay", "首页"), &p), Err(CaptureDeny::LockedProcess));
        assert_eq!(gate(&win("chrome", "网上银行 - 登录"), &p), Err(CaptureDeny::LockedTitle));
        // 常量仍在
        assert!(LOCKED_PROCESSES.contains(&"keepass"));
        assert!(LOCKED_TITLE_KEYWORDS.iter().any(|k| *k == "网上银行"));
    }

    #[test]
    fn whitelist_mode_still_respects_locked() {
        let mut p = params();
        p.capture_whitelist_mode = true;
        p.capture_whitelist = vec!["alipay".into(), "code".into()];
        assert_eq!(gate(&win("alipay", "x"), &p), Err(CaptureDeny::LockedProcess));
        assert!(gate(&win("code", "main.rs"), &p).is_ok());
        assert_eq!(gate(&win("chrome", "doc"), &p), Err(CaptureDeny::NotInWhitelist));
    }

    #[test]
    fn user_can_remove_wechat_preset() {
        let mut p = params();
        assert_eq!(gate(&win("weixin", "聊天"), &p), Err(CaptureDeny::UserBlockedProcess));
        p.capture_blocklist_user.retain(|x| norm_proc(x) != "weixin" && norm_proc(x) != "wechat");
        assert!(gate(&win("weixin", "聊天"), &p).is_ok());
    }

    #[test]
    fn private_browsing_title_blocked() {
        let p = params();
        assert_eq!(
            gate(&win("msedge", "InPrivate - Bing"), &p),
            Err(CaptureDeny::UserBlockedTitle)
        );
        assert_eq!(
            gate(&win("chrome", "无痕模式"), &p),
            Err(CaptureDeny::UserBlockedTitle)
        );
    }

    #[test]
    fn bank_title_in_browser_locked_without_title_upload() {
        // 未开启标题上传也不影响本地门禁
        let p = params();
        assert_eq!(
            gate(&win("chrome", "工商银行网上银行"), &p),
            Err(CaptureDeny::LockedTitle)
        );
    }

    #[test]
    fn deny_messages_omit_window_titles() {
        let m = CaptureDeny::UserBlockedTitle.message("chrome");
        assert!(!m.contains("InPrivate - Bing"));
        assert!(!m.contains("秘密聊天"));
        let m = CaptureDeny::LockedTitle.message("chrome");
        assert!(!m.contains("工商银行网上银行"));
    }
}
