//! SQLite 持久化：会话、消息、用量账本、工具审计。
//!
//! 选 SQLite 而不是"一堆 JSON 文件"的原因：消息是追加写、用量要按天聚合、
//! 工具审计要能追溯，关系型存储天然合适，且单文件便于备份。

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::model::{Message, Usage};
use crate::{Error, Result};

const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    model_ref TEXT NOT NULL,
    workspace TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);

CREATE TABLE IF NOT EXISTS usage_ledger (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    model_ref TEXT NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_usage_session ON usage_ledger(session_id);
CREATE INDEX IF NOT EXISTS idx_usage_model ON usage_ledger(model_ref);

CREATE TABLE IF NOT EXISTS tool_audit (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    tool TEXT NOT NULL,
    args TEXT NOT NULL,
    decision TEXT NOT NULL,
    ok INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_session ON tool_audit(session_id, created_at);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub model_ref: String,
    pub workspace: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: i64,
    pub total_cost_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model_ref: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageSummary {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
    pub by_model: Vec<ModelUsage>,
}

pub struct Store {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn: Arc::new(Mutex::new(conn)), path: path.to_path_buf() })
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn: Arc::new(Mutex::new(conn)), path: PathBuf::from(":memory:") })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self.conn.lock().map_err(|_| Error::Store("数据库锁中毒".into()))?;
        f(&guard)
    }

    pub fn create_session(&self, title: &str, model_ref: &str, workspace: &str) -> Result<SessionRow> {
        let now = chrono::Utc::now().timestamp_millis();
        let id = uuid::Uuid::new_v4().to_string();
        self.with(|c| {
            c.execute(
                "INSERT INTO sessions (id,title,model_ref,workspace,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?5)",
                params![id, title, model_ref, workspace, now],
            )?;
            Ok(())
        })?;
        Ok(SessionRow {
            id,
            title: title.to_string(),
            model_ref: model_ref.to_string(),
            workspace: workspace.to_string(),
            created_at: now,
            updated_at: now,
            message_count: 0,
            total_cost_usd: 0.0,
        })
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionRow>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT s.id, s.title, s.model_ref, s.workspace, s.created_at, s.updated_at,
                        (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) AS cnt,
                        (SELECT COALESCE(SUM(u.cost_usd),0) FROM usage_ledger u WHERE u.session_id = s.id) AS cost
                 FROM sessions s WHERE s.archived = 0 ORDER BY s.updated_at DESC",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(SessionRow {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        model_ref: r.get(2)?,
                        workspace: r.get(3)?,
                        created_at: r.get(4)?,
                        updated_at: r.get(5)?,
                        message_count: r.get(6)?,
                        total_cost_usd: r.get(7)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![title, chrono::Utc::now().timestamp_millis(), session_id],
            )?;
            Ok(())
        })
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        self.with(|c| {
            c.execute("DELETE FROM messages WHERE session_id = ?1", params![session_id])?;
            c.execute("DELETE FROM usage_ledger WHERE session_id = ?1", params![session_id])?;
            c.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
            Ok(())
        })
    }

    pub fn append_message(&self, session_id: &str, message: &Message) -> Result<i64> {
        let now = chrono::Utc::now().timestamp_millis();
        let payload = serde_json::to_string(message)?;
        let role = format!("{:?}", message.role).to_lowercase();
        self.with(|c| {
            let next: i64 = c
                .query_row("SELECT COALESCE(MAX(seq),0) + 1 FROM messages WHERE session_id = ?1", params![session_id], |r| r.get(0))
                .unwrap_or(1);
            c.execute(
                "INSERT INTO messages (id, session_id, seq, role, payload, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![uuid::Uuid::new_v4().to_string(), session_id, next, role, payload, now],
            )?;
            c.execute("UPDATE sessions SET updated_at = ?1 WHERE id = ?2", params![now, session_id])?;
            Ok(next)
        })
    }

    pub fn load_messages(&self, session_id: &str) -> Result<Vec<Message>> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT payload FROM messages WHERE session_id = ?1 ORDER BY seq ASC")?;
            let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                let raw = row?;
                match serde_json::from_str::<Message>(&raw) {
                    Ok(m) => out.push(m),
                    Err(e) => tracing::warn!("跳过损坏的消息记录: {e}"),
                }
            }
            Ok(out)
        })
    }

    pub fn record_usage(&self, session_id: &str, model_ref: &str, usage: &Usage, cost_usd: f64) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO usage_ledger (id,session_id,model_ref,input_tokens,output_tokens,cache_read_tokens,cost_usd,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    session_id,
                    model_ref,
                    usage.input_tokens as i64,
                    usage.output_tokens as i64,
                    usage.cache_read_tokens as i64,
                    cost_usd,
                    chrono::Utc::now().timestamp_millis()
                ],
            )?;
            Ok(())
        })
    }

    pub fn audit_tool(&self, session_id: &str, tool: &str, args: &serde_json::Value, decision: &str, ok: bool, duration_ms: u64) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO tool_audit (id,session_id,tool,args,decision,ok,duration_ms,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    session_id,
                    tool,
                    args.to_string(),
                    decision,
                    if ok { 1 } else { 0 },
                    duration_ms as i64,
                    chrono::Utc::now().timestamp_millis()
                ],
            )?;
            Ok(())
        })
    }

    pub fn usage_summary(&self, session_id: Option<&str>) -> Result<UsageSummary> {
        self.with(|c| {
            let (filter, param): (&str, Option<String>) = match session_id {
                Some(s) => ("WHERE session_id = ?1", Some(s.to_string())),
                None => ("", None),
            };
            let sql_tot = format!(
                "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), COALESCE(SUM(cost_usd),0) FROM usage_ledger {filter}"
            );
            let tot: (i64, i64, i64, f64) = match param.clone() {
                Some(p) => c.query_row(&sql_tot, params![p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?,
                None => c.query_row(&sql_tot, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?,
            };

            let sql_by = format!(
                "SELECT model_ref, COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), COALESCE(SUM(cost_usd),0)
                 FROM usage_ledger {filter} GROUP BY model_ref ORDER BY 5 DESC"
            );
            let mut by_model = Vec::new();
            let map_row = |r: &rusqlite::Row| -> rusqlite::Result<ModelUsage> {
                Ok(ModelUsage {
                    model_ref: r.get(0)?,
                    requests: r.get(1)?,
                    input_tokens: r.get(2)?,
                    output_tokens: r.get(3)?,
                    cost_usd: r.get(4)?,
                })
            };
            match param {
                Some(p) => {
                    let mut stmt = c.prepare(&sql_by)?;
                    by_model.extend(stmt.query_map(params![p], map_row)?.collect::<std::result::Result<Vec<_>, _>>()?);
                }
                None => {
                    let mut stmt = c.prepare(&sql_by)?;
                    by_model.extend(stmt.query_map([], map_row)?.collect::<std::result::Result<Vec<_>, _>>()?);
                }
            }

            Ok(UsageSummary {
                requests: tot.0,
                input_tokens: tot.1,
                output_tokens: tot.2,
                cost_usd: tot.3,
                by_model,
            })
        })
    }

    pub fn last_message_id(&self, session_id: &str) -> Result<Option<String>> {
        self.with(|c| {
            let id = c
                .query_row(
                    "SELECT id FROM messages WHERE session_id = ?1 ORDER BY seq DESC LIMIT 1",
                    params![session_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            Ok(id)
        })
    }
}

impl Clone for Store {
    fn clone(&self) -> Self {
        Store { conn: self.conn.clone(), path: self.path.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ContentPart, Role};

    #[test]
    fn session_message_roundtrip() {
        let s = Store::in_memory().unwrap();
        let sess = s.create_session("测试", "openai/gpt-4o", "/tmp/work").unwrap();
        let m1 = Message::user("你好");
        let m2 = Message::new(Role::Assistant, vec![ContentPart::text("在"), ContentPart::tool_use("c1", "fs_read", serde_json::json!({"path":"a.txt"}) )]);
        assert_eq!(s.append_message(&sess.id, &m1).unwrap(), 1);
        assert_eq!(s.append_message(&sess.id, &m2).unwrap(), 2);
        let loaded = s.load_messages(&sess.id).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].parts.len(), 2);
        let list = s.list_sessions().unwrap();
        assert_eq!(list[0].message_count, 2);
    }

    #[test]
    fn usage_aggregates_by_model() {
        let s = Store::in_memory().unwrap();
        let sess = s.create_session("t", "openai/gpt-4o", "").unwrap();
        let u = Usage { input_tokens: 100, output_tokens: 50, ..Default::default() };
        s.record_usage(&sess.id, "openai/gpt-4o", &u, 0.001).unwrap();
        s.record_usage(&sess.id, "claude/claude-sonnet-4-5", &u, 0.003).unwrap();
        let sum = s.usage_summary(Some(&sess.id)).unwrap();
        assert_eq!(sum.requests, 2);
        assert_eq!(sum.input_tokens, 200);
        assert!((sum.cost_usd - 0.004).abs() < 1e-9);
        assert_eq!(sum.by_model.len(), 2);
    }

    #[test]
    fn delete_cascades() {
        let s = Store::in_memory().unwrap();
        let sess = s.create_session("t", "m", "").unwrap();
        s.append_message(&sess.id, &Message::user("x")).unwrap();
        s.delete_session(&sess.id).unwrap();
        assert!(s.load_messages(&sess.id).unwrap().is_empty());
        assert!(s.list_sessions().unwrap().is_empty());
    }
}
