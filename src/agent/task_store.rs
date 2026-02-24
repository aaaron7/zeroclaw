use crate::agent::task_types::{TaskArtifactRecord, TaskEventRecord, TaskRunRecord, TaskStatus};
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct TaskStore {
    db_path: PathBuf,
}

impl TaskStore {
    pub fn new(workspace_dir: &Path) -> Result<Self> {
        let db_path = workspace_dir.join("state").join("task-runs.db");
        let store = Self { db_path };
        store.with_connection(|_| Ok(()))?;
        Ok(store)
    }

    fn with_connection<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create task-store directory: {}",
                    parent.display()
                )
            })?;
        }

        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("Failed to open task-store DB: {}", self.db_path.display()))?;

        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS task_runs (
               id                   TEXT PRIMARY KEY,
               channel              TEXT NOT NULL,
               sender_key           TEXT NOT NULL,
               reply_target         TEXT NOT NULL,
               status               TEXT NOT NULL,
               original_request     TEXT NOT NULL,
               last_response        TEXT,
               attempt_count        INTEGER NOT NULL DEFAULT 0,
               provider_retry_count INTEGER NOT NULL DEFAULT 0,
               created_at           TEXT NOT NULL,
               updated_at           TEXT NOT NULL,
               completed_at         TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_task_runs_status
               ON task_runs(status);
             CREATE INDEX IF NOT EXISTS idx_task_runs_sender_status
               ON task_runs(channel, sender_key, status);

             CREATE TABLE IF NOT EXISTS task_events (
               id         INTEGER PRIMARY KEY AUTOINCREMENT,
               task_id    TEXT NOT NULL,
               event_type TEXT NOT NULL,
               payload    TEXT,
               created_at TEXT NOT NULL,
               FOREIGN KEY(task_id) REFERENCES task_runs(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_task_events_task_created
               ON task_events(task_id, created_at);

             CREATE TABLE IF NOT EXISTS task_artifacts (
               id          INTEGER PRIMARY KEY AUTOINCREMENT,
               task_id     TEXT NOT NULL,
               path        TEXT NOT NULL,
               verified    INTEGER NOT NULL DEFAULT 0,
               checksum    TEXT,
               verified_at TEXT,
               FOREIGN KEY(task_id) REFERENCES task_runs(id) ON DELETE CASCADE
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_task_artifacts_task_path
               ON task_artifacts(task_id, path);",
        )
        .context("Failed to initialize task-store schema")?;

        f(&conn)
    }

    pub fn insert_task_run(
        &self,
        id: &str,
        channel: &str,
        sender_key: &str,
        reply_target: &str,
        original_request: &str,
    ) -> Result<()> {
        let now = now_rfc3339();
        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO task_runs (
                   id, channel, sender_key, reply_target, status, original_request,
                   last_response, attempt_count, provider_retry_count,
                   created_at, updated_at, completed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 0, 0, ?7, ?8, NULL)",
                params![
                    id,
                    channel,
                    sender_key,
                    reply_target,
                    TaskStatus::Queued.as_str(),
                    original_request,
                    now,
                    now
                ],
            )
            .with_context(|| format!("Failed to insert task run '{id}'"))?;
            Ok(())
        })
    }

    pub fn update_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        let now = now_rfc3339();
        let completed_at = if status.is_terminal() {
            Some(now.clone())
        } else {
            None
        };
        self.with_connection(|conn| {
            let changed = conn.execute(
                "UPDATE task_runs
                    SET status = ?2, updated_at = ?3, completed_at = ?4
                  WHERE id = ?1",
                params![id, status.as_str(), now, completed_at],
            )?;
            if changed == 0 {
                anyhow::bail!("Task run '{id}' not found");
            }
            Ok(())
        })
    }

    pub fn increment_attempt_count(&self, id: &str) -> Result<()> {
        self.bump_counter(id, "attempt_count")
    }

    pub fn increment_provider_retry_count(&self, id: &str) -> Result<()> {
        self.bump_counter(id, "provider_retry_count")
    }

    fn bump_counter(&self, id: &str, column: &str) -> Result<()> {
        let now = now_rfc3339();
        self.with_connection(|conn| {
            let changed = conn.execute(
                &format!(
                    "UPDATE task_runs
                       SET {column} = {column} + 1, updated_at = ?2
                     WHERE id = ?1"
                ),
                params![id, now],
            )?;
            if changed == 0 {
                anyhow::bail!("Task run '{id}' not found");
            }
            Ok(())
        })
    }

    pub fn set_last_response(&self, id: &str, last_response: &str) -> Result<()> {
        let now = now_rfc3339();
        self.with_connection(|conn| {
            let changed = conn.execute(
                "UPDATE task_runs
                    SET last_response = ?2, updated_at = ?3
                  WHERE id = ?1",
                params![id, last_response, now],
            )?;
            if changed == 0 {
                anyhow::bail!("Task run '{id}' not found");
            }
            Ok(())
        })
    }

    pub fn get_task_run(&self, id: &str) -> Result<Option<TaskRunRecord>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, channel, sender_key, reply_target, status, original_request,
                        last_response, attempt_count, provider_retry_count,
                        created_at, updated_at, completed_at
                   FROM task_runs
                  WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;
            if let Some(row) = rows.next()? {
                Ok(Some(map_task_run_row(row)?))
            } else {
                Ok(None)
            }
        })
    }

    pub fn list_recoverable_tasks(&self) -> Result<Vec<TaskRunRecord>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, channel, sender_key, reply_target, status, original_request,
                        last_response, attempt_count, provider_retry_count,
                        created_at, updated_at, completed_at
                   FROM task_runs
                  WHERE status IN ('queued', 'running', 'blocked')
               ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], map_task_run_row)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn append_event(
        &self,
        task_id: &str,
        event_type: &str,
        payload: Option<&Value>,
    ) -> Result<()> {
        let now = now_rfc3339();
        let payload_json = payload.map(Value::to_string);
        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO task_events (task_id, event_type, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![task_id, event_type, payload_json, now],
            )
            .with_context(|| format!("Failed to append task event for '{task_id}'"))?;
            Ok(())
        })
    }

    pub fn list_events(&self, task_id: &str) -> Result<Vec<TaskEventRecord>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, task_id, event_type, payload, created_at
                   FROM task_events
                  WHERE task_id = ?1
               ORDER BY id ASC",
            )?;
            let rows = stmt.query_map(params![task_id], |row| {
                Ok(TaskEventRecord {
                    id: row.get::<_, i64>(0)?,
                    task_id: row.get(1)?,
                    event_type: row.get(2)?,
                    payload_json: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn upsert_artifact_verification(
        &self,
        task_id: &str,
        path: &str,
        checksum: Option<&str>,
        verified: bool,
    ) -> Result<()> {
        let verified_at = if verified { Some(now_rfc3339()) } else { None };
        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO task_artifacts (task_id, path, verified, checksum, verified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(task_id, path) DO UPDATE SET
                   verified = excluded.verified,
                   checksum = excluded.checksum,
                   verified_at = excluded.verified_at",
                params![
                    task_id,
                    path,
                    if verified { 1 } else { 0 },
                    checksum,
                    verified_at
                ],
            )
            .with_context(|| format!("Failed to upsert task artifact for '{task_id}'"))?;
            Ok(())
        })
    }

    pub fn list_artifacts(&self, task_id: &str) -> Result<Vec<TaskArtifactRecord>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, task_id, path, verified, checksum, verified_at
                   FROM task_artifacts
                  WHERE task_id = ?1
               ORDER BY id ASC",
            )?;
            let rows = stmt.query_map(params![task_id], |row| {
                let verified_raw: i64 = row.get(3)?;
                Ok(TaskArtifactRecord {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    path: row.get(2)?,
                    verified: verified_raw == 1,
                    checksum: row.get(4)?,
                    verified_at: row.get(5)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn map_task_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRunRecord> {
    let raw_status: String = row.get(4)?;
    let status = TaskStatus::parse(&raw_status).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("Unknown task status: {raw_status}").into(),
        )
    })?;

    Ok(TaskRunRecord {
        id: row.get(0)?,
        channel: row.get(1)?,
        sender_key: row.get(2)?,
        reply_target: row.get(3)?,
        status,
        original_request: row.get(5)?,
        last_response: row.get(6)?,
        attempt_count: row.get(7)?,
        provider_retry_count: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        completed_at: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::TaskStore;
    use crate::agent::task_types::TaskStatus;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn task_store_initializes_schema_and_roundtrips_task_run() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");

        let store = TaskStore::new(&workspace).expect("task store init");
        let task_id = "task-1";
        store
            .insert_task_run(task_id, "imessage", "sender-a", "sender-a", "draft report")
            .expect("insert task run");

        store
            .update_status(task_id, TaskStatus::Running)
            .expect("mark running");
        store
            .increment_attempt_count(task_id)
            .expect("increment attempts");
        store
            .increment_provider_retry_count(task_id)
            .expect("increment provider retries");
        store
            .set_last_response(task_id, "processing")
            .expect("set last response");
        store
            .append_event(task_id, "started", Some(&json!({"phase":"start"})))
            .expect("append event");
        store
            .upsert_artifact_verification(task_id, "report.md", Some("abc123"), true)
            .expect("upsert artifact");

        let row = store
            .get_task_run(task_id)
            .expect("get run")
            .expect("existing row");
        assert_eq!(row.status, TaskStatus::Running);
        assert_eq!(row.attempt_count, 1);
        assert_eq!(row.provider_retry_count, 1);
        assert_eq!(row.last_response.as_deref(), Some("processing"));

        let events = store.list_events(task_id).expect("list events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "started");

        let artifacts = store.list_artifacts(task_id).expect("list artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].path, "report.md");
        assert!(artifacts[0].verified);
    }

    #[test]
    fn task_store_lists_recoverable_statuses_only() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let store = TaskStore::new(&workspace).expect("task store init");

        store
            .insert_task_run("queued", "imessage", "sender-1", "sender-1", "req")
            .expect("insert queued");
        store
            .insert_task_run("completed", "imessage", "sender-1", "sender-1", "req")
            .expect("insert completed");
        store
            .update_status("completed", TaskStatus::Completed)
            .expect("complete task");

        let recoverable = store.list_recoverable_tasks().expect("recoverable list");
        let ids: Vec<String> = recoverable.into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["queued".to_string()]);
    }
}
