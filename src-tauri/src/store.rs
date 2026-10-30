//! SQLite 存储：对话消息、滚动总结、长期记忆。文件在 %APPDATA%\Perch\data.db。

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    /// "image" | "file"
    pub kind: String,
    pub name: String,
    /// 图片保存在 attachments 目录里的文件名
    pub file: Option<String>,
    /// 视觉模型转述出的图片描述
    pub desc: Option<String>,
    /// 给用户看的处理说明，比如"已截断"
    pub note: Option<String>,
    /// kind=tool 时：原始调用 {"name","arguments"}，用来在历史里重建真正的工具调用
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
}

/// 思考模型在一轮里产生的思考过程。DeepSeek 要求带工具的请求里把它完整传回，否则报 400
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct StoredReasoning {
    /// 产生它的 provider；只传回给同一个 provider，别的服务可能不认这个字段
    pub provider: String,
    /// 调用工具那几步的思考（历史里还原工具调用时带上）
    pub tools: String,
    /// 最后给出回答那一步的思考
    #[serde(rename = "final")]
    pub final_answer: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub persona: String,
    /// "user" | "assistant"
    pub role: String,
    /// 显示给用户的文字
    pub display: String,
    /// 发给模型的文字（用户消息会拼上文件内容、图片描述）
    #[serde(skip)]
    pub llm_text: String,
    pub attachments: Vec<AttachmentMeta>,
    pub created_at: i64,
    #[serde(skip)]
    pub reasoning: Option<StoredReasoning>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: i64,
    /// "core" | "event"
    pub kind: String,
    pub content: String,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct Store {
    conn: Mutex<Connection>,
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_to_message(r: &rusqlite::Row) -> rusqlite::Result<Message> {
    let atts: String = r.get(5)?;
    Ok(Message {
        id: r.get(0)?,
        persona: r.get(1)?,
        role: r.get(2)?,
        display: r.get(3)?,
        llm_text: r.get(4)?,
        attachments: serde_json::from_str(&atts).unwrap_or_default(),
        created_at: r.get(6)?,
        reasoning: r.get::<_, Option<String>>(7)?.and_then(|j| serde_json::from_str(&j).ok()),
    })
}

const MSG_COLS: &str = "id, persona, role, display, llm_text, attachments, created_at, reasoning";

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 persona TEXT NOT NULL,
                 role TEXT NOT NULL,
                 display TEXT NOT NULL,
                 llm_text TEXT NOT NULL,
                 attachments TEXT NOT NULL DEFAULT '[]',
                 created_at INTEGER NOT NULL,
                 summarized INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_messages_persona ON messages(persona, id);
             CREATE TABLE IF NOT EXISTS summaries (
                 persona TEXT PRIMARY KEY,
                 text TEXT NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS memories (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind TEXT NOT NULL,
                 content TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 embedding BLOB
             );
             CREATE TABLE IF NOT EXISTS state (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS relations (
                 persona TEXT PRIMARY KEY,
                 affection REAL NOT NULL,
                 trust REAL NOT NULL,
                 familiarity REAL NOT NULL,
                 mood REAL NOT NULL,
                 updated_at INTEGER NOT NULL
             );",
        )?;
        // 旧数据库补列
        let has_reasoning = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'reasoning'")?
            .exists([])?;
        if !has_reasoning {
            conn.execute_batch("ALTER TABLE messages ADD COLUMN reasoning TEXT")?;
        }
        // 记忆向量是哪个向量模型算的（换了模型要重算）
        let has_embed_key = conn
            .prepare("SELECT 1 FROM pragma_table_info('memories') WHERE name = 'embed_key'")?
            .exists([])?;
        if !has_embed_key {
            conn.execute_batch("ALTER TABLE memories ADD COLUMN embed_key TEXT")?;
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn insert_message(
        &self,
        persona: &str,
        role: &str,
        display: &str,
        llm_text: &str,
        attachments: &[AttachmentMeta],
    ) -> rusqlite::Result<Message> {
        let conn = self.conn.lock().unwrap();
        let at = now_secs();
        let atts = serde_json::to_string(attachments).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "INSERT INTO messages (persona, role, display, llm_text, attachments, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![persona, role, display, llm_text, atts, at],
        )?;
        Ok(Message {
            id: conn.last_insert_rowid(),
            persona: persona.into(),
            role: role.into(),
            display: display.into(),
            llm_text: llm_text.into(),
            attachments: attachments.to_vec(),
            created_at: at,
            reasoning: None,
        })
    }

    pub fn set_reasoning(&self, id: i64, reasoning: &StoredReasoning) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let j = serde_json::to_string(reasoning).unwrap_or_default();
        conn.execute("UPDATE messages SET reasoning = ?2 WHERE id = ?1", params![id, j])?;
        Ok(())
    }

    /// 还没并入滚动总结的消息，按时间正序
    pub fn unsummarized(&self, persona: &str) -> rusqlite::Result<Vec<Message>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MSG_COLS} FROM messages WHERE persona = ?1 AND summarized = 0 ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![persona], row_to_message)?;
        rows.collect()
    }

    /// 聊天窗口分页：id 小于 before 的最近 limit 条，按时间正序返回
    pub fn history(&self, persona: &str, before: Option<i64>, limit: usize) -> rusqlite::Result<Vec<Message>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MSG_COLS} FROM messages WHERE persona = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![persona, before.unwrap_or(i64::MAX), limit as i64], row_to_message)?;
        let mut out: Vec<Message> = rows.collect::<rusqlite::Result<_>>()?;
        out.reverse();
        Ok(out)
    }

    /// 最近 n 条消息（不管是否已总结），给记忆提取用
    pub fn recent(&self, persona: &str, n: usize) -> rusqlite::Result<Vec<Message>> {
        self.history(persona, None, n)
    }

    pub fn mark_summarized(&self, persona: &str, upto_id: i64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE messages SET summarized = 1 WHERE persona = ?1 AND id <= ?2",
            params![persona, upto_id],
        )?;
        Ok(())
    }

    pub fn summary(&self, persona: &str) -> rusqlite::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT text FROM summaries WHERE persona = ?1", params![persona], |r| r.get(0))
            .optional()
    }

    pub fn set_summary(&self, persona: &str, text: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO summaries (persona, text, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(persona) DO UPDATE SET text = excluded.text, updated_at = excluded.updated_at",
            params![persona, text, now_secs()],
        )?;
        Ok(())
    }

    /// 清空某个人格的对话和总结（长期记忆是共享的，不动）
    pub fn clear_persona(&self, persona: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM messages WHERE persona = ?1", params![persona])?;
        conn.execute("DELETE FROM summaries WHERE persona = ?1", params![persona])?;
        Ok(())
    }

    pub fn memories(&self) -> rusqlite::Result<Vec<Memory>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, kind, content, created_at, updated_at FROM memories ORDER BY kind, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Memory {
                id: r.get(0)?,
                kind: r.get(1)?,
                content: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })?;
        rows.collect()
    }

    pub fn add_memory(&self, kind: &str, content: &str) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap();
        let at = now_secs();
        conn.execute(
            "INSERT INTO memories (kind, content, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![kind, content, at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_memory(&self, id: i64, kind: Option<&str>, content: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            // 内容变了，旧向量作废
            "UPDATE memories SET content = ?2, kind = COALESCE(?3, kind), updated_at = ?4,
                 embedding = CASE WHEN content = ?2 THEN embedding END,
                 embed_key = CASE WHEN content = ?2 THEN embed_key END
             WHERE id = ?1",
            params![id, content, kind, now_secs()],
        )?;
        Ok(())
    }

    pub fn delete_memory(&self, id: i64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// 用 key 这个向量模型算好的记忆向量
    pub fn memory_vectors(&self, key: &str) -> rusqlite::Result<Vec<(i64, Vec<f32>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, embedding FROM memories WHERE embed_key = ?1 AND embedding IS NOT NULL")?;
        let rows = stmt.query_map(params![key], |r| Ok((r.get::<_, i64>(0)?, decode_vector(&r.get::<_, Vec<u8>>(1)?))))?;
        rows.collect()
    }

    pub fn set_memory_vector(&self, id: i64, key: &str, v: &[f32]) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE memories SET embedding = ?2, embed_key = ?3 WHERE id = ?1",
            params![id, encode_vector(v), key],
        )?;
        Ok(())
    }

    /// 合并去重后整体替换
    pub fn replace_memories(&self, items: &[(String, String)]) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM memories", [])?;
        let at = now_secs();
        for (kind, content) in items {
            tx.execute(
                "INSERT INTO memories (kind, content, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                params![kind, content, at],
            )?;
        }
        tx.commit()
    }

    /// 和某个人格的关系；还没有记录时是默认值
    pub fn relation(&self, persona: &str) -> crate::relation::Relation {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT affection, trust, familiarity, mood, updated_at FROM relations WHERE persona = ?1",
            params![persona],
            |r| {
                Ok(crate::relation::Relation {
                    affection: r.get(0)?,
                    trust: r.get(1)?,
                    familiarity: r.get(2)?,
                    mood: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            },
        )
        .unwrap_or_default()
    }

    pub fn set_relation(&self, persona: &str, r: &crate::relation::Relation) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO relations (persona, affection, trust, familiarity, mood, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(persona) DO UPDATE SET affection = ?2, trust = ?3, familiarity = ?4, mood = ?5, updated_at = ?6",
            params![persona, r.affection, r.trust, r.familiarity, r.mood, r.updated_at],
        )?;
        Ok(())
    }

    pub fn delete_relation(&self, persona: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM relations WHERE persona = ?1", params![persona])?;
        Ok(())
    }

    pub fn get_state(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT value FROM state WHERE key = ?1", params![key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    /// 用户最近一次发消息的时间（任意人格），秒级时间戳
    pub fn last_user_message_at(&self) -> Option<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT MAX(created_at) FROM messages WHERE role = 'user'", [], |r| r.get::<_, Option<i64>>(0))
            .ok()
            .flatten()
    }

    pub fn delete_state(&self, key: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM state WHERE key = ?1", params![key]);
    }

    pub fn set_state(&self, key: &str, value: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO state (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        );
    }
}

fn encode_vector(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn decode_vector(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_summary_flow() {
        let s = Store::in_memory();
        let a = s.insert_message("p", "user", "你好", "你好", &[]).unwrap();
        let b = s.insert_message("p", "assistant", "嗨", "嗨", &[]).unwrap();
        s.insert_message("q", "user", "别的人格", "别的人格", &[]).unwrap();
        assert_eq!(s.unsummarized("p").unwrap().len(), 2);
        s.mark_summarized("p", a.id).unwrap();
        let rest = s.unsummarized("p").unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, b.id);
        // 历史分页不受总结影响
        assert_eq!(s.history("p", None, 10).unwrap().len(), 2);
        assert_eq!(s.history("p", Some(b.id), 10).unwrap()[0].id, a.id);
        s.set_summary("p", "一").unwrap();
        s.set_summary("p", "二").unwrap();
        assert_eq!(s.summary("p").unwrap().as_deref(), Some("二"));
        s.clear_persona("p").unwrap();
        assert!(s.history("p", None, 10).unwrap().is_empty());
        assert_eq!(s.history("q", None, 10).unwrap().len(), 1);
    }

    #[test]
    fn reasoning_round_trip_and_migration() {
        // 模拟旧版本的表（没有 reasoning 列）
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, persona TEXT NOT NULL, role TEXT NOT NULL,
             display TEXT NOT NULL, llm_text TEXT NOT NULL, attachments TEXT NOT NULL DEFAULT '[]',
             created_at INTEGER NOT NULL, summarized INTEGER NOT NULL DEFAULT 0);
             INSERT INTO messages (persona, role, display, llm_text, created_at) VALUES ('p', 'user', '旧消息', '旧消息', 1);",
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        assert!(s.history("p", None, 10).unwrap()[0].reasoning.is_none());
        let m = s.insert_message("p", "assistant", "答", "答", &[]).unwrap();
        let r = StoredReasoning { provider: "ds".into(), tools: "想调工具".into(), final_answer: "想答案".into() };
        s.set_reasoning(m.id, &r).unwrap();
        assert_eq!(s.history("p", None, 10).unwrap()[1].reasoning.as_ref(), Some(&r));
    }

    #[test]
    fn memory_crud() {
        let s = Store::in_memory();
        let id = s.add_memory("core", "喜欢猫").unwrap();
        s.add_memory("event", "周三面试").unwrap();
        s.update_memory(id, None, "喜欢三花猫").unwrap();
        let all = s.memories().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|m| m.content == "喜欢三花猫" && m.kind == "core"));
        // 向量：按模型区分，改了内容就作废，内容没变保留
        s.set_memory_vector(id, "m1", &[0.5, -1.0]).unwrap();
        assert_eq!(s.memory_vectors("m1").unwrap(), vec![(id, vec![0.5, -1.0])]);
        assert!(s.memory_vectors("m2").unwrap().is_empty());
        s.update_memory(id, Some("core"), "喜欢三花猫").unwrap();
        assert_eq!(s.memory_vectors("m1").unwrap().len(), 1);
        s.update_memory(id, None, "喜欢橘猫").unwrap();
        assert!(s.memory_vectors("m1").unwrap().is_empty());
        s.replace_memories(&[("core".into(), "合并后".into())]).unwrap();
        // 关系：没有记录时是默认值，存了能读回来，各人格分开
        assert_eq!(s.relation("p"), crate::relation::Relation::default());
        let r = crate::relation::Relation { affection: 70.0, trust: 40.0, familiarity: 20.0, mood: 90.0, updated_at: 5 };
        s.set_relation("p", &r).unwrap();
        s.set_relation("p", &r).unwrap();
        assert_eq!(s.relation("p"), r);
        assert_eq!(s.relation("q"), crate::relation::Relation::default());
        s.delete_relation("p").unwrap();
        assert_eq!(s.relation("p"), crate::relation::Relation::default());
        assert_eq!(s.memories().unwrap().len(), 1);
    }
}
