//! 长期记忆：后台由辅助模型从对话中提取，超过上限时合并去重。
//! 第一版把全部记忆注入 prompt（FullInjectRetriever）；MemoryRetriever 为以后的混合检索留位置。

use crate::llm::{self, ChatMessage, Endpoint};
use crate::store::{Memory, Message, Store};
use serde_json::Value;

/// 【留接口】按当前对话挑选要注入的记忆。以后加向量检索时实现一个新的 retriever。
pub trait MemoryRetriever {
    fn retrieve(&self, store: &Store, query: &str) -> Vec<Memory>;
}

pub struct FullInjectRetriever;

impl MemoryRetriever for FullInjectRetriever {
    fn retrieve(&self, store: &Store, _query: &str) -> Vec<Memory> {
        store.memories().unwrap_or_default()
    }
}

/// 拼成 system prompt 里的记忆块；没有记忆时返回 None
pub fn prompt_block(memories: &[Memory]) -> Option<String> {
    if memories.is_empty() {
        return None;
    }
    let mut s = String::from("## 关于用户的长期记忆（自然地运用，不要逐条复述）\n");
    for m in memories {
        let tag = if m.kind == "core" { "核心" } else { "事件" };
        s.push_str(&format!("- [{tag}] {}\n", m.content));
    }
    Some(s)
}

/// 从模型输出里取出第一个 JSON 对象（容忍 ```json 代码块和前后废话）
pub fn extract_json(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

#[derive(Debug, PartialEq)]
pub enum Op {
    Add { kind: String, content: String },
    Update { id: i64, content: String },
    Delete { id: i64 },
}

fn norm_kind(k: Option<&str>) -> String {
    if k == Some("core") { "core".into() } else { "event".into() }
}

pub fn parse_ops(v: &Value) -> Vec<Op> {
    let Some(ops) = v.get("ops").and_then(Value::as_array) else { return Vec::new() };
    ops.iter()
        .filter_map(|o| {
            let content = o.get("content").and_then(Value::as_str).map(str::trim).unwrap_or("");
            let id = o.get("id").and_then(Value::as_i64);
            match o.get("op").and_then(Value::as_str)? {
                "add" if !content.is_empty() => Some(Op::Add {
                    kind: norm_kind(o.get("kind").and_then(Value::as_str)),
                    content: content.to_string(),
                }),
                "update" if !content.is_empty() => Some(Op::Update { id: id?, content: content.to_string() }),
                "delete" => Some(Op::Delete { id: id? }),
                _ => None,
            }
        })
        .collect()
}

fn transcript(messages: &[Message], persona_name: &str) -> String {
    messages
        .iter()
        .map(|m| {
            let who = if m.role == "user" { "用户" } else { persona_name };
            let text: String = m.display.chars().take(800).collect();
            format!("{who}：{text}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const EXTRACT_PROMPT: &str = r#"你负责维护一个桌宠对"用户"的长期记忆。根据最近的对话，决定要新增、修改或删除哪些记忆。
规则：
- 只记录关于用户本人、以后聊天用得上的信息：身份、喜好、习惯、人际关系、长期计划（kind=core），以及有时间意义的具体事件（kind=event，写上大概日期）。
- 不要记录桌宠自己说的话、闲聊客套、一次性的琐碎问题。
- 已有记忆的事实变了就 update，过时或错误就 delete；和已有记忆重复的不要 add。
- 每条记忆一句话，简洁具体。
只输出 JSON，格式：{"ops":[{"op":"add","kind":"core","content":"..."},{"op":"update","id":3,"content":"..."},{"op":"delete","id":5}]}。没有要改的就输出 {"ops":[]}。"#;

const CONSOLIDATE_PROMPT: &str = r#"下面是桌宠关于用户的全部长期记忆，条数太多了。请合并重复、删除过时和琐碎的条目，把总数压缩到 LIMIT 条以内，保留最重要的信息。
只输出 JSON：{"memories":[{"kind":"core","content":"..."},{"kind":"event","content":"..."}]}"#;

pub async fn extract(
    store: &Store,
    ep: &Endpoint,
    recent: &[Message],
    persona_name: &str,
    today: &str,
) -> Result<usize, String> {
    let existing = store.memories().map_err(|e| e.to_string())?;
    let existing_json: Vec<Value> = existing
        .iter()
        .map(|m| serde_json::json!({ "id": m.id, "kind": m.kind, "content": m.content }))
        .collect();
    let user = format!(
        "今天是 {today}。\n\n已有记忆：\n{}\n\n最近的对话：\n{}",
        serde_json::to_string(&existing_json).unwrap_or_default(),
        transcript(recent, persona_name)
    );
    let reply = llm::chat_once(ep, &[ChatMessage::text("system", EXTRACT_PROMPT), ChatMessage::text("user", user)], Some(0.2)).await?;
    let v = extract_json(&reply).ok_or("记忆提取：模型没有返回 JSON")?;
    let ops = parse_ops(&v);
    let known: std::collections::HashSet<i64> = existing.iter().map(|m| m.id).collect();
    let mut applied = 0;
    for op in ops {
        let r = match &op {
            Op::Add { kind, content } => {
                if existing.iter().any(|m| m.content == *content) {
                    continue;
                }
                store.add_memory(kind, content).map(|_| ())
            }
            Op::Update { id, content } if known.contains(id) => store.update_memory(*id, None, content),
            Op::Delete { id } if known.contains(id) => store.delete_memory(*id),
            _ => continue,
        };
        r.map_err(|e| e.to_string())?;
        applied += 1;
    }
    Ok(applied)
}

pub async fn consolidate(store: &Store, ep: &Endpoint, limit: usize) -> Result<(), String> {
    let all = store.memories().map_err(|e| e.to_string())?;
    if all.len() <= limit {
        return Ok(());
    }
    let list: Vec<Value> = all.iter().map(|m| serde_json::json!({ "kind": m.kind, "content": m.content })).collect();
    let prompt = CONSOLIDATE_PROMPT.replace("LIMIT", &limit.to_string());
    let reply = llm::chat_once(
        ep,
        &[ChatMessage::text("system", prompt), ChatMessage::text("user", serde_json::to_string(&list).unwrap_or_default())],
        Some(0.2),
    )
    .await?;
    let v = extract_json(&reply).ok_or("记忆合并：模型没有返回 JSON")?;
    let items: Vec<(String, String)> = v
        .get("memories")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let c = m.get("content")?.as_str()?.trim();
                    (!c.is_empty()).then(|| (norm_kind(m.get("kind").and_then(Value::as_str)), c.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    // 模型返回异常（空的或没压下来）时不覆盖，宁可下次再试
    if items.is_empty() || items.len() > limit {
        return Err("记忆合并结果无效，已保留原记忆".into());
    }
    store.replace_memories(&items).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_from_fenced_reply() {
        let v = extract_json("好的：\n```json\n{\"ops\":[{\"op\":\"add\",\"kind\":\"core\",\"content\":\"喜欢猫\"}]}\n```").unwrap();
        assert_eq!(parse_ops(&v), vec![Op::Add { kind: "core".into(), content: "喜欢猫".into() }]);
    }

    #[test]
    fn drops_invalid_ops() {
        let v: Value = serde_json::json!({ "ops": [
            { "op": "add", "content": "  " },
            { "op": "update", "content": "缺 id" },
            { "op": "delete", "id": 3 },
            { "op": "add", "kind": "weird", "content": "周三面试" },
            { "op": "rename", "id": 1 }
        ]});
        assert_eq!(
            parse_ops(&v),
            vec![Op::Delete { id: 3 }, Op::Add { kind: "event".into(), content: "周三面试".into() }]
        );
        assert!(extract_json("没有 JSON").is_none());
    }

    #[test]
    fn prompt_block_marks_kinds() {
        let m = |kind: &str, c: &str| Memory { id: 1, kind: kind.into(), content: c.into(), created_at: 0, updated_at: 0 };
        let b = prompt_block(&[m("core", "叫小潘"), m("event", "10/20 面试")]).unwrap();
        assert!(b.contains("[核心] 叫小潘") && b.contains("[事件] 10/20 面试"));
        assert!(prompt_block(&[]).is_none());
    }
}
