use chrono::{DateTime, Utc};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{Task, TaskApproval, TaskId};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::state_machine::task_state_tag;
use crate::types::{ArtifactRecord, TaskRunRecord};

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("sqlite error: {source}")]
    Sql {
        #[from]
        source: rusqlite::Error,
    },
    #[error("json serialization error: {source}")]
    Json {
        #[from]
        source: serde_json::Error,
    },
    #[error("timestamp parse error for value '{value}': {source}")]
    TimestampParse {
        value: String,
        #[source]
        source: chrono::ParseError,
    },
}

#[derive(Debug)]
pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        let conn = Connection::open(path)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, PersistenceError> {
        let conn = Connection::open_in_memory()?;
        Ok(Self { conn })
    }

    pub fn migrate(&self) -> Result<(), PersistenceError> {
        self.conn.execute_batch(
            r#"
CREATE TABLE IF NOT EXISTS tasks (
    task_id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL,
    state_tag TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tasks_repo ON tasks(repo_id);
CREATE INDEX IF NOT EXISTS idx_tasks_state ON tasks(state_tag);

CREATE TABLE IF NOT EXISTS events (
    event_id TEXT PRIMARY KEY,
    task_id TEXT,
    repo_id TEXT,
    at TEXT NOT NULL,
    kind_tag TEXT NOT NULL,
    payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_task_at ON events(task_id, at);
CREATE INDEX IF NOT EXISTS idx_events_repo_at ON events(repo_id, at);

CREATE TABLE IF NOT EXISTS approvals (
    task_id TEXT NOT NULL,
    reviewer TEXT NOT NULL,
    verdict TEXT NOT NULL,
    issued_at TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (task_id, reviewer)
);

CREATE INDEX IF NOT EXISTS idx_approvals_task ON approvals(task_id);

CREATE TABLE IF NOT EXISTS runs (
    run_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    model TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    stop_reason TEXT,
    exit_code INTEGER,
    payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_runs_task ON runs(task_id, started_at);

CREATE TABLE IF NOT EXISTS artifacts (
    artifact_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    payload_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_artifacts_task ON artifacts(task_id, created_at);
"#,
        )?;
        Ok(())
    }

    pub fn upsert_task(&self, task: &Task) -> Result<(), PersistenceError> {
        let payload = serde_json::to_string(task)?;
        self.conn.execute(
            r#"
INSERT INTO tasks (task_id, repo_id, state_tag, payload_json, created_at, updated_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT(task_id) DO UPDATE SET
  repo_id = excluded.repo_id,
  state_tag = excluded.state_tag,
  payload_json = excluded.payload_json,
  updated_at = excluded.updated_at
"#,
            params![
                task.id.0,
                task.repo_id.0,
                task_state_tag(task.state),
                payload,
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, PersistenceError> {
        let payload: Option<String> = self
            .conn
            .query_row(
                "SELECT payload_json FROM tasks WHERE task_id = ?1",
                params![task_id.0],
                |row| row.get(0),
            )
            .optional()?;
        payload
            .map(|value| serde_json::from_str::<Task>(&value))
            .transpose()
            .map_err(PersistenceError::from)
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, PersistenceError> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload_json FROM tasks ORDER BY updated_at DESC, task_id ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut tasks = Vec::new();
        for row in rows {
            let payload = row?;
            tasks.push(serde_json::from_str::<Task>(&payload)?);
        }
        Ok(tasks)
    }

    pub fn list_tasks_by_state(&self, state: TaskState) -> Result<Vec<Task>, PersistenceError> {
        let mut stmt = self.conn.prepare(
            "SELECT payload_json FROM tasks WHERE state_tag = ?1 ORDER BY updated_at DESC, task_id ASC",
        )?;
        let rows = stmt.query_map(params![task_state_tag(state)], |row| {
            row.get::<_, String>(0)
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            let payload = row?;
            tasks.push(serde_json::from_str::<Task>(&payload)?);
        }
        Ok(tasks)
    }

    pub fn append_event(&self, event: &Event) -> Result<(), PersistenceError> {
        let payload = serde_json::to_string(event)?;
        self.conn.execute(
            r#"
INSERT INTO events (event_id, task_id, repo_id, at, kind_tag, payload_json)
VALUES (?1, ?2, ?3, ?4, ?5, ?6)
"#,
            params![
                event.id.0,
                event.task_id.as_ref().map(|id| id.0.clone()),
                event.repo_id.as_ref().map(|id| id.0.clone()),
                event.at.to_rfc3339(),
                event_kind_tag(&event.kind),
                payload,
            ],
        )?;
        Ok(())
    }

    pub fn list_events_for_task(&self, task_id: &TaskId) -> Result<Vec<Event>, PersistenceError> {
        let mut stmt = self.conn.prepare(
            "SELECT payload_json FROM events WHERE task_id = ?1 ORDER BY at ASC, event_id ASC",
        )?;
        let rows = stmt.query_map(params![task_id.0], |row| row.get::<_, String>(0))?;
        let mut events = Vec::new();
        for row in rows {
            let payload = row?;
            events.push(serde_json::from_str::<Event>(&payload)?);
        }
        Ok(events)
    }

    pub fn list_events_global(&self) -> Result<Vec<Event>, PersistenceError> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload_json FROM events ORDER BY at ASC, event_id ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut events = Vec::new();
        for row in rows {
            let payload = row?;
            events.push(serde_json::from_str::<Event>(&payload)?);
        }
        Ok(events)
    }

    pub fn upsert_approval(&self, approval: &TaskApproval) -> Result<(), PersistenceError> {
        let payload = serde_json::to_string(approval)?;
        self.conn.execute(
            r#"
INSERT INTO approvals (task_id, reviewer, verdict, issued_at, payload_json)
VALUES (?1, ?2, ?3, ?4, ?5)
ON CONFLICT(task_id, reviewer) DO UPDATE SET
  verdict = excluded.verdict,
  issued_at = excluded.issued_at,
  payload_json = excluded.payload_json
"#,
            params![
                approval.task_id.0,
                model_tag(approval.reviewer),
                review_verdict_tag(approval.verdict),
                approval.issued_at.to_rfc3339(),
                payload
            ],
        )?;
        Ok(())
    }

    pub fn list_approvals_for_task(
        &self,
        task_id: &TaskId,
    ) -> Result<Vec<TaskApproval>, PersistenceError> {
        let mut stmt = self.conn.prepare(
            "SELECT payload_json FROM approvals WHERE task_id = ?1 ORDER BY issued_at ASC, reviewer ASC",
        )?;
        let rows = stmt.query_map(params![task_id.0], |row| row.get::<_, String>(0))?;
        let mut approvals = Vec::new();
        for row in rows {
            let payload = row?;
            approvals.push(serde_json::from_str::<TaskApproval>(&payload)?);
        }
        Ok(approvals)
    }

    pub fn insert_run(&self, run: &TaskRunRecord) -> Result<(), PersistenceError> {
        let payload = serde_json::to_string(run)?;
        self.conn.execute(
            r#"
INSERT INTO runs (run_id, task_id, model, started_at, finished_at, stop_reason, exit_code, payload_json)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
"#,
            params![
                run.run_id,
                run.task_id.0,
                model_tag(run.model),
                run.started_at.to_rfc3339(),
                run.finished_at.map(|value| value.to_rfc3339()),
                run.stop_reason,
                run.exit_code,
                payload
            ],
        )?;
        Ok(())
    }

    pub fn insert_artifact(&self, artifact: &ArtifactRecord) -> Result<(), PersistenceError> {
        let payload = serde_json::to_string(artifact)?;
        self.conn.execute(
            r#"
INSERT INTO artifacts (artifact_id, task_id, kind, path, created_at, payload_json)
VALUES (?1, ?2, ?3, ?4, ?5, ?6)
"#,
            params![
                artifact.artifact_id,
                artifact.task_id.0,
                artifact.kind,
                artifact.path,
                artifact.created_at.to_rfc3339(),
                payload
            ],
        )?;
        Ok(())
    }

    pub fn latest_event_at_for_task(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<DateTime<Utc>>, PersistenceError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT at FROM events WHERE task_id = ?1 ORDER BY at DESC LIMIT 1",
                params![task_id.0],
                |row| row.get(0),
            )
            .optional()?;

        raw.map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|source| PersistenceError::TimestampParse { value, source })
        })
        .transpose()
    }
}

fn event_kind_tag(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::TaskCreated => "task_created",
        EventKind::TaskStateChanged { .. } => "task_state_changed",
        EventKind::DraftPrCreated { .. } => "draft_pr_created",
        EventKind::ParentHeadUpdated { .. } => "parent_head_updated",
        EventKind::RestackStarted => "restack_started",
        EventKind::RestackCompleted => "restack_completed",
        EventKind::RestackConflict => "restack_conflict",
        EventKind::RestackResolved => "restack_resolved",
        EventKind::VerifyRequested { .. } => "verify_requested",
        EventKind::VerifyCompleted { .. } => "verify_completed",
        EventKind::ReviewRequested { .. } => "review_requested",
        EventKind::ReviewCompleted { .. } => "review_completed",
        EventKind::ReadyReached => "ready_reached",
        EventKind::SubmitStarted { .. } => "submit_started",
        EventKind::SubmitCompleted => "submit_completed",
        EventKind::NeedsHuman { .. } => "needs_human",
        EventKind::Error { .. } => "error",
    }
}

fn model_tag(model: orch_core::types::ModelKind) -> &'static str {
    match model {
        orch_core::types::ModelKind::Claude => "claude",
        orch_core::types::ModelKind::Codex => "codex",
        orch_core::types::ModelKind::Gemini => "gemini",
    }
}

fn review_verdict_tag(verdict: orch_core::events::ReviewVerdict) -> &'static str {
    match verdict {
        orch_core::events::ReviewVerdict::Approve => "approve",
        orch_core::events::ReviewVerdict::RequestChanges => "request_changes",
        orch_core::events::ReviewVerdict::Block => "block",
    }
}
