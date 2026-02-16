//! MVP persistence layer using SQLite.

use chrono::{DateTime, Utc};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{Task, TaskId, TaskPriority};
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

/// SQLite-based store for tasks and events.
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
    priority TEXT NOT NULL DEFAULT 'normal',
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

        if let Err(err) = self.conn.execute(
            "ALTER TABLE tasks ADD COLUMN priority TEXT NOT NULL DEFAULT 'normal'",
            [],
        ) {
            if !matches!(
                &err,
                rusqlite::Error::SqliteFailure(_, Some(message))
                    if message.contains("duplicate column name: priority")
            ) {
                return Err(err.into());
            }
        }

        if let Err(err) = self.conn.execute(
            "ALTER TABLE runs ADD COLUMN estimated_tokens INTEGER DEFAULT NULL",
            [],
        ) {
            if !matches!(
                &err,
                rusqlite::Error::SqliteFailure(_, Some(message))
                    if message.contains("duplicate column name: estimated_tokens")
            ) {
                return Err(err.into());
            }
        }

        if let Err(err) = self.conn.execute(
            "ALTER TABLE runs ADD COLUMN duration_secs REAL DEFAULT NULL",
            [],
        ) {
            if !matches!(
                &err,
                rusqlite::Error::SqliteFailure(_, Some(message))
                    if message.contains("duplicate column name: duration_secs")
            ) {
                return Err(err.into());
            }
        }
        Ok(())
    }

    // --- Task CRUD ---

    pub fn upsert_task(&self, task: &Task) -> Result<(), PersistenceError> {
        let payload = serde_json::to_string(task)?;
        self.conn.execute(
            r#"
INSERT INTO tasks (task_id, repo_id, state_tag, priority, payload_json, created_at, updated_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
ON CONFLICT(task_id) DO UPDATE SET
  repo_id = excluded.repo_id,
  state_tag = excluded.state_tag,
  priority = excluded.priority,
  payload_json = excluded.payload_json,
  updated_at = excluded.updated_at
"#,
            params![
                task.id.0,
                task.repo_id.0,
                task_state_tag(task.state),
                task.priority.as_str(),
                payload,
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, PersistenceError> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT payload_json, priority FROM tasks WHERE task_id = ?1",
                params![task_id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(payload, priority)| {
            let mut task = serde_json::from_str::<Task>(&payload)?;
            task.priority = priority.parse::<TaskPriority>().unwrap_or_default();
            Ok::<Task, serde_json::Error>(task)
        })
        .transpose()
        .map_err(PersistenceError::from)
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, PersistenceError> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload_json, priority FROM tasks ORDER BY updated_at DESC, task_id ASC")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            let (payload, priority) = row?;
            let mut task = serde_json::from_str::<Task>(&payload)?;
            task.priority = priority.parse::<TaskPriority>().unwrap_or_default();
            tasks.push(task);
        }
        Ok(tasks)
    }

    pub fn delete_task(&self, task_id: &TaskId) -> Result<bool, PersistenceError> {
        self.conn.execute(
            "DELETE FROM runs WHERE task_id = ?1",
            params![task_id.0.as_str()],
        )?;
        self.conn.execute(
            "DELETE FROM artifacts WHERE task_id = ?1",
            params![task_id.0.as_str()],
        )?;
        self.conn.execute(
            "DELETE FROM events WHERE task_id = ?1",
            params![task_id.0.as_str()],
        )?;
        let deleted = self.conn.execute(
            "DELETE FROM tasks WHERE task_id = ?1",
            params![task_id.0.as_str()],
        )?;
        Ok(deleted > 0)
    }

    pub fn list_tasks_by_state(&self, state: TaskState) -> Result<Vec<Task>, PersistenceError> {
        let mut stmt = self.conn.prepare(
            "SELECT payload_json, priority FROM tasks WHERE state_tag = ?1 ORDER BY updated_at DESC, task_id ASC",
        )?;
        let rows = stmt.query_map(params![task_state_tag(state)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            let (payload, priority) = row?;
            let mut task = serde_json::from_str::<Task>(&payload)?;
            task.priority = priority.parse::<TaskPriority>().unwrap_or_default();
            tasks.push(task);
        }
        Ok(tasks)
    }

    // --- Events ---

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

    // --- Runs ---

    pub fn insert_run(&self, run: &TaskRunRecord) -> Result<(), PersistenceError> {
        let payload = serde_json::to_string(run)?;
        self.conn.execute(
            r#"
INSERT INTO runs (run_id, task_id, model, started_at, finished_at, stop_reason, exit_code, estimated_tokens, duration_secs, payload_json)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
"#,
            params![
                run.run_id,
                run.task_id.0,
                run.model.as_str(),
                run.started_at.to_rfc3339(),
                run.finished_at.map(|value| value.to_rfc3339()),
                run.stop_reason,
                run.exit_code,
                run.estimated_tokens,
                run.duration_secs,
                payload
            ],
        )?;
        Ok(())
    }

    pub fn set_open_run_estimated_tokens(
        &self,
        task_id: &TaskId,
        estimated_tokens: u64,
    ) -> Result<usize, PersistenceError> {
        let updated = self.conn.execute(
            r#"
UPDATE runs
SET estimated_tokens = ?1
WHERE task_id = ?2 AND finished_at IS NULL
"#,
            params![estimated_tokens, task_id.0],
        )?;
        Ok(updated)
    }

    pub fn finish_open_runs_for_task(
        &self,
        task_id: &TaskId,
        finished_at: DateTime<Utc>,
        stop_reason: &str,
        exit_code: Option<i32>,
        duration_secs: Option<f64>,
    ) -> Result<usize, PersistenceError> {
        let updated = self.conn.execute(
            r#"
UPDATE runs
SET finished_at = ?1, stop_reason = ?2, exit_code = ?3, duration_secs = ?4
WHERE task_id = ?5 AND finished_at IS NULL
"#,
            params![
                finished_at.to_rfc3339(),
                stop_reason,
                exit_code,
                duration_secs,
                task_id.0
            ],
        )?;
        Ok(updated)
    }

    pub fn list_open_runs(&self) -> Result<Vec<TaskRunRecord>, PersistenceError> {
        let mut stmt = self.conn.prepare(
            "SELECT payload_json, finished_at, stop_reason, exit_code, estimated_tokens, duration_secs FROM runs WHERE finished_at IS NULL ORDER BY started_at ASC, run_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i32>>(3)?,
                row.get::<_, Option<u64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
            ))
        })?;
        let mut runs = Vec::new();
        for row in rows {
            let (payload, finished_at, stop_reason, exit_code, estimated_tokens, duration_secs) =
                row?;
            let mut run = serde_json::from_str::<TaskRunRecord>(&payload)?;
            run.finished_at = parse_optional_rfc3339(finished_at)?;
            run.stop_reason = stop_reason;
            run.exit_code = exit_code;
            run.estimated_tokens = estimated_tokens.or(run.estimated_tokens);
            run.duration_secs = duration_secs.or(run.duration_secs);
            runs.push(run);
        }
        Ok(runs)
    }

    pub fn list_runs_for_task(
        &self,
        task_id: &TaskId,
    ) -> Result<Vec<TaskRunRecord>, PersistenceError> {
        let mut stmt = self.conn.prepare(
            "SELECT payload_json, finished_at, stop_reason, exit_code, estimated_tokens, duration_secs FROM runs WHERE task_id = ?1 ORDER BY started_at ASC, run_id ASC",
        )?;
        let rows = stmt.query_map(params![task_id.0], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i32>>(3)?,
                row.get::<_, Option<u64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
            ))
        })?;
        let mut runs = Vec::new();
        for row in rows {
            let (payload, finished_at, stop_reason, exit_code, estimated_tokens, duration_secs) =
                row?;
            let mut run = serde_json::from_str::<TaskRunRecord>(&payload)?;
            run.finished_at = parse_optional_rfc3339(finished_at)?;
            run.stop_reason = stop_reason;
            run.exit_code = exit_code;
            run.estimated_tokens = estimated_tokens.or(run.estimated_tokens);
            run.duration_secs = duration_secs.or(run.duration_secs);
            runs.push(run);
        }
        Ok(runs)
    }

    pub fn count_runs_by_model(&self) -> Result<Vec<(String, i64)>, PersistenceError> {
        let mut stmt = self
            .conn
            .prepare("SELECT model, COUNT(*) FROM runs GROUP BY model ORDER BY COUNT(*) DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = Vec::new();
        for row in rows {
            counts.push(row?);
        }
        Ok(counts)
    }

    // --- Artifacts ---

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

fn parse_optional_rfc3339(
    value: Option<String>,
) -> Result<Option<DateTime<Utc>>, PersistenceError> {
    value
        .map(|raw| {
            DateTime::parse_from_rfc3339(&raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|source| PersistenceError::TimestampParse { value: raw, source })
        })
        .transpose()
}

fn event_kind_tag(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::TaskCreated => "task_created",
        EventKind::TaskStateChanged { .. } => "task_state_changed",
        EventKind::ParentHeadUpdated { .. } => "parent_head_updated",
        EventKind::RestackStarted => "restack_started",
        EventKind::RestackCompleted => "restack_completed",
        EventKind::RestackConflict => "restack_conflict",
        EventKind::VerifyStarted => "verify_started",
        EventKind::VerifyCompleted { .. } => "verify_completed",
        EventKind::ReadyReached => "ready_reached",
        EventKind::SubmitStarted { .. } => "submit_started",
        EventKind::SubmitCompleted => "submit_completed",
        EventKind::NeedsHuman { .. } => "needs_human",
        EventKind::Error { .. } => "error",
        EventKind::RetryScheduled { .. } => "retry_scheduled",
        EventKind::AgentSpawned { .. } => "agent_spawned",
        EventKind::AgentCompleted { .. } => "agent_completed",
        EventKind::ModelFallback { .. } => "model_fallback",
        EventKind::ContextRegenStarted => "context_regen_started",
        EventKind::ContextRegenCompleted { .. } => "context_regen_completed",
        EventKind::TaskFailed { .. } => "task_failed",
        EventKind::TestSpecValidated { .. } => "test_spec_validated",
        EventKind::OrchestratorDecomposed { .. } => "orchestrator_decomposed",
        EventKind::QAStarted { .. } => "qa_started",
        EventKind::QACompleted { .. } => "qa_completed",
        EventKind::QAFailed { .. } => "qa_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use orch_core::types::{EventId, ModelKind, RepoId, TaskPriority};
    use std::path::PathBuf;

    fn mk_store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().expect("in-memory store");
        store.migrate().expect("migrate");
        store
    }

    fn mk_task(id: &str, state: TaskState) -> Task {
        let mut task = Task::new(
            TaskId(id.to_string()),
            RepoId("example".to_string()),
            format!("Task {id}"),
            PathBuf::from(format!(".orch/wt/{id}")),
        );
        task.state = state;
        task
    }

    #[test]
    fn upsert_and_load_task() {
        let store = mk_store();
        let task = mk_task("T1", TaskState::Chatting);

        store.upsert_task(&task).expect("upsert");
        let loaded = store.load_task(&task.id).expect("load").expect("exists");

        assert_eq!(loaded.id, task.id);
        assert_eq!(loaded.state, task.state);
    }

    #[test]
    fn list_tasks_ordered_by_updated_at() {
        let store = mk_store();
        let mut t1 = mk_task("T1", TaskState::Chatting);
        let mut t2 = mk_task("T2", TaskState::Ready);

        t1.updated_at = Utc::now() - chrono::Duration::seconds(100);
        t2.updated_at = Utc::now();

        store.upsert_task(&t1).expect("upsert t1");
        store.upsert_task(&t2).expect("upsert t2");

        let tasks = store.list_tasks().expect("list");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id.0, "T2"); // Most recent first
        assert_eq!(tasks[1].id.0, "T1");
    }

    #[test]
    fn delete_task_removes_all_related_records() {
        let store = mk_store();
        let task = mk_task("T1", TaskState::Chatting);
        store.upsert_task(&task).expect("upsert");

        let event = Event {
            id: EventId("E1".to_string()),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at: Utc::now(),
            kind: EventKind::TaskCreated,
        };
        store.append_event(&event).expect("append event");

        assert!(store.delete_task(&task.id).expect("delete"));
        assert!(store.load_task(&task.id).expect("load").is_none());
        assert!(store
            .list_events_for_task(&task.id)
            .expect("events")
            .is_empty());
    }

    #[test]
    fn list_tasks_by_state() {
        let store = mk_store();
        store
            .upsert_task(&mk_task("T1", TaskState::Chatting))
            .expect("upsert");
        store
            .upsert_task(&mk_task("T2", TaskState::Ready))
            .expect("upsert");
        store
            .upsert_task(&mk_task("T3", TaskState::Chatting))
            .expect("upsert");

        let chatting = store
            .list_tasks_by_state(TaskState::Chatting)
            .expect("list");
        assert_eq!(chatting.len(), 2);

        let ready = store.list_tasks_by_state(TaskState::Ready).expect("list");
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn upsert_and_load_task_priority_round_trips() {
        let store = mk_store();
        let mut task = mk_task("TP1", TaskState::Chatting);
        task.priority = TaskPriority::Critical;

        store.upsert_task(&task).expect("upsert");
        let loaded = store.load_task(&task.id).expect("load").expect("exists");

        assert_eq!(loaded.priority, TaskPriority::Critical);
    }

    #[test]
    fn insert_and_list_open_runs() {
        let store = mk_store();
        let run = TaskRunRecord {
            run_id: "R1".to_string(),
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            finished_at: None,
            stop_reason: None,
            exit_code: None,
            estimated_tokens: Some(42),
            duration_secs: None,
        };

        store.insert_run(&run).expect("insert");
        let runs = store.list_open_runs().expect("list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "R1");
    }

    #[test]
    fn finish_open_runs() {
        let store = mk_store();
        let run = TaskRunRecord {
            run_id: "R1".to_string(),
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            finished_at: None,
            stop_reason: None,
            exit_code: None,
            estimated_tokens: Some(88),
            duration_secs: None,
        };

        store.insert_run(&run).expect("insert");
        let updated = store
            .finish_open_runs_for_task(
                &TaskId("T1".to_string()),
                Utc::now(),
                "completed",
                Some(0),
                Some(4.5),
            )
            .expect("finish");
        assert_eq!(updated, 1);

        let runs = store.list_open_runs().expect("list");
        assert!(runs.is_empty());
    }

    #[test]
    fn set_open_run_estimated_tokens_updates_open_rows() {
        let store = mk_store();
        let run = TaskRunRecord {
            run_id: "R2".to_string(),
            task_id: TaskId("T2".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Codex,
            started_at: Utc::now(),
            finished_at: None,
            stop_reason: None,
            exit_code: None,
            estimated_tokens: None,
            duration_secs: None,
        };

        store.insert_run(&run).expect("insert");
        let updated = store
            .set_open_run_estimated_tokens(&TaskId("T2".to_string()), 777)
            .expect("update");
        assert_eq!(updated, 1);

        let runs = store
            .list_runs_for_task(&TaskId("T2".to_string()))
            .expect("list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].estimated_tokens, Some(777));
    }
}
