//! 长期记忆：后台由辅助模型从对话中提取，超过上限时合并去重。
//! 放进提示词时：核心记忆全部带上；事件记忆在配了向量模型时只挑和当前对话相关的几条加最近的几条，
//! 没配（或者向量服务出错）时全部带上。

use crate::config::Config;
use crate::llm::{self, ChatMessage, Endpoint};
use crate::store::{Memory, Message, Store};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// 除了相关的，总会带上最近更新的几条事件记忆（"上次说的那个"这类指代靠它）
const RECENT_EVENTS: usize = 3;
/// 一次最多给多少条记忆算向量
const EMBED_BATCH: usize = 32;

/// 向量检索的配置：用哪个向量模型、每次挑几条
pub struct Recall {
    ep: Endpoint,
    /// 区分向量是哪个模型算的，换了地址或模型名就要重算
    key: String,
    pub top_k: usize,
}

impl Recall {
    /// 配了向量模型才有
    pub fn from_config(cfg: &Config) -> Option<Self> {
        let p = cfg.provider(cfg.roles.embedding.as_deref())?;
        if p.model.trim().is_empty() {
            return None;
        }
        let ep = Endpoint::from_provider(p);
        Some(Self { key: format!("{}|{}", ep.base_url, ep.model), ep, top_k: cfg.chat.memory_top_k.max(1) })
    }
}

/// 记忆条数上限：配了向量模型时只挑相关的放进提示词，可以记得更多
pub fn memory_limit(cfg: &Config) -> usize {
    if Recall::from_config(cfg).is_some() {
        cfg.chat.memory_limit_retrieval
    } else {
        cfg.chat.memory_limit
    }
}

/// 挑出要放进提示词的记忆。query 是当前对话的要点（最近几句）
pub async fn recall(store: &Store, recall: Option<&Recall>, query: &str) -> Vec<Memory> {
    let all = store.memories().unwrap_or_default();
    let Some(r) = recall else { return all };
    match recall_with(store, r, &all, query, r.top_k).await {
        Ok(v) => v,
        Err(e) => {
            // 向量服务出错时退回全部注入，聊天不受影响
            eprintln!("[memory] 向量检索失败，改为全部注入：{e}");
            all
        }
    }
}

async fn recall_with(store: &Store, r: &Recall, all: &[Memory], query: &str, top_k: usize) -> Result<Vec<Memory>, String> {
    let events = all.iter().filter(|m| m.kind != "core").count();
    // 事件记忆不多时用不着检索，省一次请求
    if events <= top_k + RECENT_EVENTS || query.trim().is_empty() {
        return Ok(all.to_vec());
    }
    let mut vectors: HashMap<i64, Vec<f32>> = store.memory_vectors(&r.key).map_err(|e| e.to_string())?.into_iter().collect();
    // 还没算向量的事件记忆和问题一起算（一般只有最近新增的几条）
    let missing: Vec<&Memory> = all
        .iter()
        .filter(|m| m.kind != "core" && !vectors.contains_key(&m.id))
        .take(EMBED_BATCH)
        .collect();
    let mut inputs = vec![query.to_string()];
    inputs.extend(missing.iter().map(|m| m.content.clone()));
    let mut out = llm::embed(&r.ep, &inputs).await?.into_iter();
    let q = normalize(out.next().ok_or("没有拿到问题的向量")?);
    for (m, v) in missing.iter().zip(out) {
        let v = normalize(v);
        let _ = store.set_memory_vector(m.id, &r.key, &v);
        vectors.insert(m.id, v);
    }
    let scores: HashMap<i64, f32> = vectors.iter().map(|(id, v)| (*id, dot(&q, v))).collect();
    Ok(select(all, &scores, top_k, RECENT_EVENTS))
}

/// 核心记忆全要；事件记忆取最近的 recent 条，再按相关度取 top_k 条。
/// 没有向量的事件记忆（没算出来的）也带上，宁多勿漏。结果保持原来的顺序。
fn select(all: &[Memory], scores: &HashMap<i64, f32>, top_k: usize, recent: usize) -> Vec<Memory> {
    let mut events: Vec<&Memory> = all.iter().filter(|m| m.kind != "core").collect();
    let mut keep: HashSet<i64> = HashSet::new();
    events.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.id.cmp(&a.id)));
    keep.extend(events.iter().take(recent).map(|m| m.id));
    keep.extend(events.iter().filter(|m| !scores.contains_key(&m.id)).map(|m| m.id));
    let mut ranked: Vec<(&Memory, f32)> = events.iter().filter_map(|m| scores.get(&m.id).map(|s| (*m, *s))).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    keep.extend(ranked.iter().filter(|(m, _)| !keep.contains(&m.id)).take(top_k).map(|(m, _)| m.id).collect::<Vec<_>>());
    all.iter().filter(|m| m.kind == "core" || keep.contains(&m.id)).cloned().collect()
}

/// 后台补算还没有向量的记忆（提取、合并之后调用），返回补了几条
pub async fn backfill(store: &Store, r: &Recall) -> Result<usize, String> {
    let have: HashSet<i64> = store.memory_vectors(&r.key).map_err(|e| e.to_string())?.into_iter().map(|(id, _)| id).collect();
    let missing: Vec<Memory> = store
        .memories()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|m| m.kind != "core" && !have.contains(&m.id))
        .collect();
    for chunk in missing.chunks(EMBED_BATCH) {
        let inputs: Vec<String> = chunk.iter().map(|m| m.content.clone()).collect();
        let vecs = llm::embed(&r.ep, &inputs).await?;
        for (m, v) in chunk.iter().zip(vecs) {
            store.set_memory_vector(m.id, &r.key, &normalize(v)).map_err(|e| e.to_string())?;
        }
    }
    Ok(missing.len())
}

/// 给记忆提取用：配了向量模型时只给辅助模型看核心记忆和与最近对话相关的事件记忆，记忆多了也不会把提示词撑爆
pub async fn recall_for_extract(store: &Store, recall: Option<&Recall>, query: &str) -> Vec<Memory> {
    let all = store.memories().unwrap_or_default();
    let Some(r) = recall else { return all };
    recall_with(store, r, &all, query, 30).await.unwrap_or(all)
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// 检索用的问题：用户刚说的话。很短时（"那后来呢"这类）带上前一条消息补充话题，
/// 否则只看这一句，免得上一个话题把新话题的记忆挤掉
pub fn query_text(recent: &[Message]) -> String {
    let take = |m: &Message, n: usize| m.display.chars().take(n).collect::<String>();
    // 聊天命令（@状态 等）不给模型看，也不参与检索
    let recent: Vec<&Message> = recent.iter().filter(|m| !m.llm_text.is_empty()).collect();
    let Some((last, before)) = recent.split_last() else { return String::new() };
    let now = take(last, 500);
    match before.last() {
        Some(prev) if now.chars().count() < 8 => format!("{}\n{now}", take(prev, 200)),
        _ => now,
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

/// existing：给辅助模型看的已有记忆（它只能改、删这些）
pub async fn extract(
    store: &Store,
    ep: &Endpoint,
    existing: &[Memory],
    recent: &[Message],
    persona_name: &str,
    today: &str,
) -> Result<usize, String> {
    let all = store.memories().map_err(|e| e.to_string())?;
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
                if all.iter().any(|m| m.content == *content) {
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

    fn mem(id: i64, kind: &str, updated_at: i64) -> Memory {
        Memory { id, kind: kind.into(), content: format!("m{id}"), created_at: 0, updated_at }
    }

    #[test]
    fn select_keeps_core_recent_relevant_and_unembedded() {
        // 1 是核心；2~9 是事件，updated_at 越大越新；9 没有向量
        let mut all = vec![mem(1, "core", 0)];
        all.extend((2..=9).map(|i| mem(i, "event", i)));
        let scores: HashMap<i64, f32> = [(2, 0.9), (3, 0.1), (4, 0.8), (5, 0.2), (6, 0.3), (7, 0.05), (8, 0.0)].into_iter().collect();
        let ids: Vec<i64> = select(&all, &scores, 2, 2).iter().map(|m| m.id).collect();
        // 核心 1；最近的 9、8；相关度最高的 2、4（不和最近的重复）；9 没向量本来就会带上
        assert_eq!(ids, vec![1, 2, 4, 8, 9]);
    }

    #[test]
    fn query_uses_previous_message_only_when_short() {
        let m = |id: i64, role: &str, t: &str| Message {
            id,
            persona: "p".into(),
            role: role.into(),
            display: t.into(),
            llm_text: t.into(),
            attachments: vec![],
            created_at: 0,
            reasoning: None,
        };
        let long = [m(1, "assistant", "团团还好吗"), m(2, "user", "下周想再去杭州玩，顺便跑个步，你觉得呢")];
        assert_eq!(query_text(&long), "下周想再去杭州玩，顺便跑个步，你觉得呢");
        let short = [m(1, "assistant", "团团还好吗"), m(2, "user", "后来呢")];
        assert_eq!(query_text(&short), "团团还好吗\n后来呢");
        assert_eq!(query_text(&[]), "");
    }

    #[test]
    fn normalize_and_dot_give_cosine() {
        let a = normalize(vec![3.0, 4.0]);
        assert!((a[0] - 0.6).abs() < 1e-6 && (a[1] - 0.8).abs() < 1e-6);
        assert!((dot(&a, &normalize(vec![6.0, 8.0])) - 1.0).abs() < 1e-6);
        assert_eq!(normalize(vec![0.0, 0.0]), vec![0.0, 0.0]);
    }
}
