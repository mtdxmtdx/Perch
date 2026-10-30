//! 关系数值：好感度、信任度、熟悉度、心情（0～100），每个人格各一份。
//! - 熟悉度随聊天慢慢涨（越熟涨得越慢）；
//! - 好感度、信任度、心情由辅助模型每隔几轮按最近的对话打分调整，每次变化有上限；
//! - 心情过几个小时会慢慢回到平常的水平；
//! - 用户可以用聊天命令（@状态、@好感度 80 等）查看和调整。
//!
//! 数值以文字描述的形式附在最后一条用户消息后面（和当前时间放在一起），不改 system prompt，不破坏缓存前缀。

use crate::llm::{self, ChatMessage, Endpoint};
use crate::store::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 心情平常的水平，和回落的半衰期（小时）
const MOOD_BASE: f64 = 60.0;
const MOOD_HALF_LIFE_HOURS: f64 = 6.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    pub affection: f64,
    pub trust: f64,
    pub familiarity: f64,
    /// 存的是 updated_at 那一刻的心情，用的时候按时间回落（见 mood_at）
    pub mood: f64,
    pub updated_at: i64,
}

impl Default for Relation {
    fn default() -> Self {
        Self { affection: 50.0, trust: 50.0, familiarity: 10.0, mood: MOOD_BASE, updated_at: 0 }
    }
}

fn clamp(v: f64) -> f64 {
    v.clamp(0.0, 100.0)
}

impl Relation {
    /// now 时刻的心情：离上次变化越久越接近平常
    pub fn mood_at(&self, now: i64) -> f64 {
        if self.updated_at <= 0 {
            return self.mood;
        }
        let hours = (now - self.updated_at).max(0) as f64 / 3600.0;
        MOOD_BASE + (self.mood - MOOD_BASE) * 0.5f64.powf(hours / MOOD_HALF_LIFE_HOURS)
    }

    /// 改数值前先把心情的回落结算到 now
    pub fn settle(&mut self, now: i64) {
        self.mood = self.mood_at(now);
        self.updated_at = now;
    }

    /// 用户说了一句话：熟悉度涨一点，越熟涨得越慢（从 10 到 50 大约要聊两百句）
    pub fn on_user_message(&mut self, now: i64) {
        self.settle(now);
        self.familiarity = clamp(self.familiarity + 0.4 * (1.0 - self.familiarity / 100.0));
    }

    pub fn apply(&mut self, d: &Deltas, now: i64) {
        self.settle(now);
        self.affection = clamp(self.affection + d.affection);
        self.trust = clamp(self.trust + d.trust);
        self.mood = clamp(self.mood + d.mood);
    }

    /// 附在用户消息后面的说明
    pub fn note(&self, now: i64) -> String {
        format!(
            "你们现在的关系：好感{}，信任{}，{}；你此刻的心情：{}",
            level(self.affection, &["很低", "偏低", "一般", "不错", "很高"]),
            level(self.trust, &["很低", "有些保留", "一般", "比较信任", "非常信任"]),
            level(self.familiarity, &["还很陌生", "刚认识不久", "渐渐熟悉", "已经很熟", "像老朋友一样"]),
            level(self.mood_at(now), &["很低落", "有点闷闷不乐", "平静", "愉快", "特别开心"]),
        )
    }
}

/// 0～100 分成五档
fn level<'a>(v: f64, names: &[&'a str; 5]) -> &'a str {
    names[((v / 20.0).floor() as usize).min(4)]
}

/// 写进 system prompt 的说明（固定不变）
pub const GUIDE: &str = "## 你和用户的关系\n用户的消息末尾的括号里会附上你们当前的关系和你此刻的心情。据此自然地调整语气和亲近程度：\
关系越好越亲昵随意，越陌生越客气；心情好时更活泼，低落时话少一点。不要提到这些说明，也不要说出任何数值。";

/// 一次评估得出的变化量
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Deltas {
    pub affection: f64,
    pub trust: f64,
    pub mood: f64,
}

/// 从模型输出里取变化量，并限制每次的幅度（好感、信任 ±5，心情 ±15）
pub fn parse_deltas(v: &Value) -> Deltas {
    let get = |k: &str, max: f64| v.get(k).and_then(Value::as_f64).unwrap_or(0.0).clamp(-max, max);
    Deltas { affection: get("affection", 5.0), trust: get("trust", 5.0), mood: get("mood", 15.0) }
}

const EVAL_PROMPT: &str = r#"你在扮演桌宠「NAME」。根据最近的对话，评估这段时间用户的言行让你对用户的感受发生了什么变化。
- affection（好感）：用户友善、关心你、聊得开心就加；冷淡、粗鲁就减。范围 -5～5。
- trust（信任）：用户坦诚、分享心事、托付事情就加；欺骗、反复无常就减。范围 -5～5。
- mood（你的心情）：这段对话让你开心就加，难过、被冒犯就减。范围 -15～15。
普通的闲聊和问答变化都应该很小（0～1）；只有明显的情绪事件才给大的数。
只输出 JSON：{"affection":0,"trust":0,"mood":0}"#;

/// 让辅助模型按最近的对话打分
pub async fn evaluate(ep: &Endpoint, persona_name: &str, recent: &[Message], current: &Relation, now: i64) -> Result<Deltas, String> {
    let transcript: Vec<String> = recent
        .iter()
        .filter(|m| !m.llm_text.is_empty())
        .map(|m| {
            let who = if m.role == "user" { "用户" } else { persona_name };
            format!("{who}：{}", m.display.chars().take(500).collect::<String>())
        })
        .collect();
    if transcript.is_empty() {
        return Ok(Deltas::default());
    }
    let user = format!("目前：{}。\n\n最近的对话：\n{}", current.note(now), transcript.join("\n"));
    let reply = llm::chat_once(
        ep,
        &[ChatMessage::text("system", EVAL_PROMPT.replace("NAME", persona_name)), ChatMessage::text("user", user)],
        Some(0.2),
    )
    .await?;
    let v = crate::memory::extract_json(&reply).ok_or("关系评估：模型没有返回 JSON")?;
    Ok(parse_deltas(&v))
}

// ---------- 聊天命令 ----------

#[derive(Debug, PartialEq)]
pub enum Command {
    Help,
    Status,
    Set { field: &'static str, value: f64 },
    Remember(String),
    Forget(String),
    /// 以 @ 开头但不认识
    Unknown,
}

/// 字段名：命令里的中文名 → 内部名
const FIELDS: [(&str, &str); 8] = [
    ("好感度", "affection"),
    ("好感", "affection"),
    ("信任度", "trust"),
    ("信任", "trust"),
    ("熟悉度", "familiarity"),
    ("熟悉", "familiarity"),
    ("心情", "mood"),
    ("心情值", "mood"),
];

/// 解析聊天命令；不是 @ 开头的返回 None（照常发给模型）
pub fn parse_command(text: &str) -> Option<Command> {
    let t = text.trim();
    let rest = t.strip_prefix('@').or_else(|| t.strip_prefix('＠'))?.trim();
    // 名字后面紧跟参数（"好感度80"）或者用空格隔开都行
    let split = rest.find(|c: char| c.is_whitespace() || c.is_ascii_digit()).unwrap_or(rest.len());
    let (name, arg) = (rest[..split].trim(), rest[split..].trim());
    Some(match name {
        "帮助" | "help" | "命令" => Command::Help,
        "状态" | "关系" => Command::Status,
        "记住" if !arg.is_empty() => Command::Remember(arg.to_string()),
        "忘记" if !arg.is_empty() => Command::Forget(arg.to_string()),
        _ => match FIELDS.iter().find(|(n, _)| *n == name) {
            Some((_, field)) => match arg.parse::<f64>() {
                Ok(v) if v.is_finite() => Command::Set { field, value: clamp(v) },
                _ => Command::Unknown,
            },
            None => Command::Unknown,
        },
    })
}

pub const HELP: &str = "可以用的命令：\n\
@状态 —— 看看我们现在的关系和我的心情\n\
@好感度 80 / @信任度 70 / @熟悉度 50 / @心情 90 —— 直接调整（0～100）\n\
@记住 内容 —— 记下一条核心记忆\n\
@忘记 关键词 —— 删掉包含这个关键词的记忆\n\
@帮助 —— 显示这些说明";

pub fn status_text(r: &Relation, now: i64, persona_name: &str) -> String {
    format!(
        "我和你的关系（{persona_name}）：\n好感度 {:.0}　信任度 {:.0}　熟悉度 {:.0}　心情 {:.0}\n{}",
        r.affection,
        r.trust,
        r.familiarity,
        r.mood_at(now),
        r.note(now)
    )
}

pub fn set_field(r: &mut Relation, field: &str, value: f64, now: i64) {
    r.settle(now);
    match field {
        "affection" => r.affection = value,
        "trust" => r.trust = value,
        "familiarity" => r.familiarity = value,
        "mood" => r.mood = value,
        _ => {}
    }
}

pub fn field_label(field: &str) -> &'static str {
    match field {
        "affection" => "好感度",
        "trust" => "信任度",
        "familiarity" => "熟悉度",
        _ => "心情",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mood_drifts_back_to_base() {
        let r = Relation { mood: 100.0, updated_at: 1000, ..Default::default() };
        assert_eq!(r.mood_at(1000), 100.0);
        assert!((r.mood_at(1000 + 6 * 3600) - 80.0).abs() < 1e-9); // 一个半衰期：差距减半
        assert!((r.mood_at(1000 + 60 * 3600) - MOOD_BASE).abs() < 0.1);
        let mut r = r;
        r.settle(1000 + 6 * 3600);
        assert!((r.mood - 80.0).abs() < 1e-9 && r.updated_at == 1000 + 6 * 3600);
    }

    #[test]
    fn familiarity_grows_slower_when_familiar() {
        let mut r = Relation::default();
        r.on_user_message(1);
        let early = r.familiarity - 10.0;
        let mut r2 = Relation { familiarity: 90.0, ..Default::default() };
        r2.on_user_message(1);
        assert!(r2.familiarity - 90.0 < early / 5.0);
        let mut r3 = Relation { familiarity: 100.0, ..Default::default() };
        r3.on_user_message(1);
        assert_eq!(r3.familiarity, 100.0);
    }

    #[test]
    fn deltas_are_limited_and_values_clamped() {
        let d = parse_deltas(&serde_json::json!({ "affection": 20, "trust": -1.5, "mood": -40, "x": 1 }));
        assert_eq!(d, Deltas { affection: 5.0, trust: -1.5, mood: -15.0 });
        assert_eq!(parse_deltas(&serde_json::json!({})), Deltas::default());
        let mut r = Relation { affection: 98.0, mood: 5.0, ..Default::default() };
        r.apply(&d, 0);
        assert_eq!((r.affection, r.trust, r.mood), (100.0, 48.5, 0.0));
    }

    #[test]
    fn note_uses_words_not_numbers() {
        let r = Relation { affection: 85.0, trust: 10.0, familiarity: 45.0, mood: 60.0, updated_at: 0 };
        let n = r.note(0);
        assert!(n.contains("好感很高") && n.contains("信任很低") && n.contains("渐渐熟悉") && n.contains("愉快"));
        assert!(!n.chars().any(|c| c.is_ascii_digit()));
        assert_eq!(level(100.0, &["a", "b", "c", "d", "e"]), "e");
    }

    #[test]
    fn parses_commands() {
        assert_eq!(parse_command("@状态"), Some(Command::Status));
        assert_eq!(parse_command(" ＠帮助 "), Some(Command::Help));
        assert_eq!(parse_command("@好感度 80"), Some(Command::Set { field: "affection", value: 80.0 }));
        assert_eq!(parse_command("@心情120"), Some(Command::Set { field: "mood", value: 100.0 }));
        assert_eq!(parse_command("@信任 abc"), Some(Command::Unknown));
        assert_eq!(parse_command("@记住 我对花粉过敏"), Some(Command::Remember("我对花粉过敏".into())));
        assert_eq!(parse_command("@忘记 花粉"), Some(Command::Forget("花粉".into())));
        assert_eq!(parse_command("@记住"), Some(Command::Unknown));
        assert_eq!(parse_command("@随便什么"), Some(Command::Unknown));
        assert_eq!(parse_command("你好 @状态"), None);
        assert_eq!(parse_command("邮箱是 a@b.com"), None);
    }
}
