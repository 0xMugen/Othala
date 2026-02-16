//! MVP persistence layer using SQLite.

use chrono::{DateTime, Utc};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{ModelKind, Session, SessionStatus, Task, TaskId, TaskPriority};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::state_machine::task_state_tag;
use crate::types::{ArtifactRecord, TaskRunRecord};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedTaskRecord {
    pub task_id: TaskId,
    pub repo_id: String,
    pub state_tag: String,
    pub payload_json: String,
    pub created_at: DateTime<Utc>,
    pub archived_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskCloneOverrides {
    pub title: Option<String>,
    pub preferred_model: Option<ModelKind>,
    pub priority: Option<TaskPriority>,
}

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
    #[error("task not found: {task_id}")]
    TaskNotFound { task_id: String },
    #[error("session not found: {session_id}")]
    SessionNotFound { session_id: String },
}

/// SQLite-based store for tasks and events.
#[derive(Debug)]
pub struct SqliteStore {
    conn: Connection,
}

pub trait SessionStore {
    fn create_session(&self, session: &Session) -> Result<(), PersistenceError>;
    fn get_session(&self, id: &str) -> Result<Option<Session>, PersistenceError>;
    fn list_sessions(&self) -> Result<Vec<Session>, PersistenceError>;
    fn update_session(&self, session: &Session) -> Result<(), PersistenceError>;
    fn fork_session(&self, id: &str) -> Result<Session, PersistenceError>;
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
    labels_json TEXT NOT NULL DEFAULT '[]',
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

CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    task_ids_json TEXT NOT NULL DEFAULT '[]',
    parent_session_id TEXT,
    status TEXT NOT NULL DEFAULT 'active'
);

CREATE INDEX IF NOT EXISTS idx_sessions_updated_at ON sessions(updated_at);
CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_id);

CREATE TABLE IF NOT EXISTS archived_tasks (
    task_id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL,
    state_tag TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    archived_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_archived_tasks_repo ON archived_tasks(repo_id);
CREATE INDEX IF NOT EXISTS idx_archived_tasks_archived_at ON archived_tasks(archived_at);
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
            "ALTER TABLE tasks ADD COLUMN labels_json TEXT NOT NULL DEFAULT '[]'",
            [],
        ) {
            if !matches!(
                &err,
                rusqlite::Error::SqliteFailure(_, Some(message))
                    if message.contains("duplicate column name: labels_json")
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
INSERT INTO tasks (task_id, repo_id, state_tag, priority, labels_json, payload_json, created_at, updated_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
ON CONFLICT(task_id) DO UPDATE SET
  repo_id = excluded.repo_id,
  state_tag = excluded.state_tag,
  priority = excluded.priority,
  labels_json = excluded.labels_json,
  payload_json = excluded.payload_json,
  updated_at = excluded.updated_at
"#,
            params![
                task.id.0,
                task.repo_id.0,
                task_state_tag(task.state),
                task.priority.as_str(),
                serde_json::to_string(&task.labels)?,
                payload,
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn create_session(&self, session: &Session) -> Result<(), PersistenceError> {
        self.conn.execute(
            r#"
INSERT INTO sessions (session_id, title, created_at, updated_at, task_ids_json, parent_session_id, status)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
"#,
            params![
                session.id.as_str(),
                session.title.as_str(),
                session.created_at.to_rfc3339(),
                session.updated_at.to_rfc3339(),
                serde_json::to_string(&session.task_ids)?,
                session.parent_session_id.as_deref(),
                session.status.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>, PersistenceError> {
        type SessionRow = (String, String, String, String, String, Option<String>, String);
        let row: Option<SessionRow> = self
            .conn
            .query_row(
                "SELECT session_id, title, created_at, updated_at, task_ids_json, parent_session_id, status FROM sessions WHERE session_id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;

        row.map(
            |(session_id, title, created_at_raw, updated_at_raw, task_ids_json, parent_id, status)| {
                let created_at = DateTime::parse_from_rfc3339(&created_at_raw)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|source| PersistenceError::TimestampParse {
                        value: created_at_raw,
                        source,
                    })?;
                let updated_at = DateTime::parse_from_rfc3339(&updated_at_raw)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|source| PersistenceError::TimestampParse {
                        value: updated_at_raw,
                        source,
                    })?;
                let task_ids = serde_json::from_str::<Vec<TaskId>>(&task_ids_json)?;

                Ok(Session {
                    id: session_id,
                    title,
                    created_at,
                    updated_at,
                    task_ids,
                    parent_session_id: parent_id,
                    status: status.parse::<SessionStatus>().unwrap_or_default(),
                })
            },
        )
        .transpose()
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>, PersistenceError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, title, created_at, updated_at, task_ids_json, parent_session_id, status FROM sessions ORDER BY updated_at DESC, session_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            let (session_id, title, created_at_raw, updated_at_raw, task_ids_json, parent_id, status) =
                row?;
            let created_at = DateTime::parse_from_rfc3339(&created_at_raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|source| PersistenceError::TimestampParse {
                    value: created_at_raw,
                    source,
                })?;
            let updated_at = DateTime::parse_from_rfc3339(&updated_at_raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|source| PersistenceError::TimestampParse {
                    value: updated_at_raw,
                    source,
                })?;
            let task_ids = serde_json::from_str::<Vec<TaskId>>(&task_ids_json)?;
            sessions.push(Session {
                id: session_id,
                title,
                created_at,
                updated_at,
                task_ids,
                parent_session_id: parent_id,
                status: status.parse::<SessionStatus>().unwrap_or_default(),
            });
        }

        Ok(sessions)
    }

    pub fn update_session(&self, session: &Session) -> Result<(), PersistenceError> {
        let updated = self.conn.execute(
            r#"
UPDATE sessions
SET title = ?2,
    created_at = ?3,
    updated_at = ?4,
    task_ids_json = ?5,
    parent_session_id = ?6,
    status = ?7
WHERE session_id = ?1
"#,
            params![
                session.id.as_str(),
                session.title.as_str(),
                session.created_at.to_rfc3339(),
                session.updated_at.to_rfc3339(),
                serde_json::to_string(&session.task_ids)?,
                session.parent_session_id.as_deref(),
                session.status.as_str(),
            ],
        )?;

        if updated == 0 {
            return Err(PersistenceError::SessionNotFound {
                session_id: session.id.clone(),
            });
        }

        Ok(())
    }

    pub fn fork_session(&self, id: &str) -> Result<Session, PersistenceError> {
        let parent = self
            .get_session(id)?
            .ok_or_else(|| PersistenceError::SessionNotFound {
                session_id: id.to_string(),
            })?;
        let now = Utc::now();

        let child = Session {
            id: format!(
                "S-{}",
                now.timestamp_nanos_opt().unwrap_or(now.timestamp_millis() * 1_000_000)
            ),
            title: format!("{} (fork)", parent.title),
            created_at: now,
            updated_at: now,
            task_ids: parent.task_ids,
            parent_session_id: Some(parent.id),
            status: SessionStatus::Active,
        };

        self.create_session(&child)?;
        Ok(child)
    }

    pub fn load_task(&self, task_id: &TaskId) -> Result<Option<Task>, PersistenceError> {
        let row: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT payload_json, priority, labels_json FROM tasks WHERE task_id = ?1",
                params![task_id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        row.map(|(payload, priority, labels_json)| {
            let mut task = serde_json::from_str::<Task>(&payload)?;
            task.priority = priority.parse::<TaskPriority>().unwrap_or_default();
            task.labels = serde_json::from_str::<Vec<String>>(&labels_json)?;
            Ok::<Task, serde_json::Error>(task)
        })
        .transpose()
        .map_err(PersistenceError::from)
    }

    pub fn clone_task(
        &self,
        source_id: &str,
        new_id: &str,
        overrides: TaskCloneOverrides,
    ) -> Result<(), PersistenceError> {
        let source_task = self
            .load_task(&TaskId::new(source_id))?
            .ok_or_else(|| PersistenceError::TaskNotFound {
                task_id: source_id.to_string(),
            })?;

        let mut cloned = source_task;
        let now = Utc::now();
        cloned.id = TaskId::new(new_id);
        if let Some(title) = overrides.title {
            cloned.title = title;
        }
        if let Some(model) = overrides.preferred_model {
            cloned.preferred_model = Some(model);
        }
        if let Some(priority) = overrides.priority {
            cloned.priority = priority;
        }
        cloned.state = TaskState::Chatting;
        cloned.retry_count = 0;
        cloned.failed_models.clear();
        cloned.created_at = now;
        cloned.updated_at = now;

        self.upsert_task(&cloned)
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, PersistenceError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT payload_json, priority, labels_json FROM tasks ORDER BY updated_at DESC, task_id ASC",
            )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            let (payload, priority, labels_json) = row?;
            let mut task = serde_json::from_str::<Task>(&payload)?;
            task.priority = priority.parse::<TaskPriority>().unwrap_or_default();
            task.labels = serde_json::from_str::<Vec<String>>(&labels_json)?;
            tasks.push(task);
        }
        Ok(tasks)
    }

    pub fn search_tasks(
        &self,
        query: &str,
        label: Option<&str>,
        state: Option<&str>,
    ) -> Result<Vec<Task>, PersistenceError> {
        let query_lc = query.to_lowercase();
        let label_lc = label.map(|value| value.to_lowercase());
        let state_lc = state.map(|value| value.trim().to_lowercase().replace('-', "_"));

        let mut tasks = self.list_tasks()?;
        tasks.retain(|task| {
            let title_match = task.title.to_lowercase().contains(&query_lc);
            let id_match = task.id.0.to_lowercase().contains(&query_lc);
            let label_match = task
                .labels
                .iter()
                .any(|existing| existing.to_lowercase().contains(&query_lc));
            if !(title_match || id_match || label_match) {
                return false;
            }

            if let Some(ref expected_label) = label_lc {
                let has_label = task
                    .labels
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(expected_label));
                if !has_label {
                    return false;
                }
            }

            if let Some(ref expected_state) = state_lc {
                let task_state = task_state_tag(task.state).to_lowercase();
                if task_state != *expected_state {
                    return false;
                }
            }

            true
        });

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
            "SELECT payload_json, priority, labels_json FROM tasks WHERE state_tag = ?1 ORDER BY updated_at DESC, task_id ASC",
        )?;
        let rows = stmt.query_map(params![task_state_tag(state)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            let (payload, priority, labels_json) = row?;
            let mut task = serde_json::from_str::<Task>(&payload)?;
            task.priority = priority.parse::<TaskPriority>().unwrap_or_default();
            task.labels = serde_json::from_str::<Vec<String>>(&labels_json)?;
            tasks.push(task);
        }
        Ok(tasks)
    }

    pub fn archive_task(
        &self,
        task: &Task,
        archived_at: DateTime<Utc>,
    ) -> Result<(), PersistenceError> {
        let payload = serde_json::to_string(task)?;
        self.conn.execute(
            r#"
INSERT INTO archived_tasks (task_id, repo_id, state_tag, payload_json, created_at, archived_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT(task_id) DO UPDATE SET
  repo_id = excluded.repo_id,
  state_tag = excluded.state_tag,
  payload_json = excluded.payload_json,
  created_at = excluded.created_at,
  archived_at = excluded.archived_at
"#,
            params![
                task.id.0,
                task.repo_id.0,
                task_state_tag(task.state),
                payload,
                task.created_at.to_rfc3339(),
                archived_at.to_rfc3339(),
            ],
        )?;
        self.conn
            .execute("DELETE FROM tasks WHERE task_id = ?1", params![task.id.0.as_str()])?;
        Ok(())
    }

    pub fn list_archived(&self) -> Result<Vec<ArchivedTaskRecord>, PersistenceError> {
        let mut stmt = self.conn.prepare(
            "SELECT task_id, repo_id, state_tag, payload_json, created_at, archived_at FROM archived_tasks ORDER BY archived_at DESC, task_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;

        let mut archived = Vec::new();
        for row in rows {
            let (task_id, repo_id, state_tag, payload_json, created_at_raw, archived_at_raw) = row?;
            let created_at = DateTime::parse_from_rfc3339(&created_at_raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|source| PersistenceError::TimestampParse {
                    value: created_at_raw,
                    source,
                })?;
            let archived_at = DateTime::parse_from_rfc3339(&archived_at_raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|source| PersistenceError::TimestampParse {
                    value: archived_at_raw,
                    source,
                })?;

            archived.push(ArchivedTaskRecord {
                task_id: TaskId::new(task_id),
                repo_id,
                state_tag,
                payload_json,
                created_at,
                archived_at,
            });
        }

        Ok(archived)
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

    pub fn list_events_for_task(&self, task_id: &str) -> Result<Vec<Event>, PersistenceError> {
        let mut stmt = self.conn.prepare(
            "SELECT payload_json FROM events WHERE task_id = ?1 ORDER BY at ASC, event_id ASC",
        )?;
        let rows = stmt.query_map(params![task_id], |row| row.get::<_, String>(0))?;
        let mut events = Vec::new();
        for row in rows {
            let payload = row?;
            events.push(serde_json::from_str::<Event>(&payload)?);
        }
        Ok(events)
    }

    pub fn list_all_events(
        &self,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<Event>, PersistenceError> {
        let mut events = Vec::new();

        match (since, until) {
            (Some(since), Some(until)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT payload_json FROM events WHERE at >= ?1 AND at <= ?2 ORDER BY at ASC, event_id ASC",
                )?;
                let rows = stmt.query_map(params![since, until], |row| row.get::<_, String>(0))?;
                for row in rows {
                    let payload = row?;
                    events.push(serde_json::from_str::<Event>(&payload)?);
                }
            }
            (Some(since), None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT payload_json FROM events WHERE at >= ?1 ORDER BY at ASC, event_id ASC",
                )?;
                let rows = stmt.query_map(params![since], |row| row.get::<_, String>(0))?;
                for row in rows {
                    let payload = row?;
                    events.push(serde_json::from_str::<Event>(&payload)?);
                }
            }
            (None, Some(until)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT payload_json FROM events WHERE at <= ?1 ORDER BY at ASC, event_id ASC",
                )?;
                let rows = stmt.query_map(params![until], |row| row.get::<_, String>(0))?;
                for row in rows {
                    let payload = row?;
                    events.push(serde_json::from_str::<Event>(&payload)?);
                }
            }
            (None, None) => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT payload_json FROM events ORDER BY at ASC, event_id ASC")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                for row in rows {
                    let payload = row?;
                    events.push(serde_json::from_str::<Event>(&payload)?);
                }
            }
        }

        Ok(events)
    }

    pub fn list_events_global(&self) -> Result<Vec<Event>, PersistenceError> {
        self.list_all_events(None, None)
    }

    pub fn task_count_by_state(&self) -> Result<Vec<(String, i64)>, PersistenceError> {
        let mut stmt = self
            .conn
            .prepare("SELECT state_tag, COUNT(*) FROM tasks GROUP BY state_tag")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = Vec::new();
        for row in rows {
            counts.push(row?);
        }
        Ok(counts)
    }

    pub fn total_event_count(&self) -> Result<i64, PersistenceError> {
        let count = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(count)
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

impl SessionStore for SqliteStore {
    fn create_session(&self, session: &Session) -> Result<(), PersistenceError> {
        SqliteStore::create_session(self, session)
    }

    fn get_session(&self, id: &str) -> Result<Option<Session>, PersistenceError> {
        SqliteStore::get_session(self, id)
    }

    fn list_sessions(&self) -> Result<Vec<Session>, PersistenceError> {
        SqliteStore::list_sessions(self)
    }

    fn update_session(&self, session: &Session) -> Result<(), PersistenceError> {
        SqliteStore::update_session(self, session)
    }

    fn fork_session(&self, id: &str) -> Result<Session, PersistenceError> {
        SqliteStore::fork_session(self, id)
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
        EventKind::CancellationRequested { .. } => "cancellation_requested",
        EventKind::ModelFallback { .. } => "model_fallback",
        EventKind::ContextRegenStarted => "context_regen_started",
        EventKind::ContextRegenCompleted { .. } => "context_regen_completed",
        EventKind::ConfigReloaded { .. } => "config_reloaded",
        EventKind::TaskFailed { .. } => "task_failed",
        EventKind::TestSpecValidated { .. } => "test_spec_validated",
        EventKind::OrchestratorDecomposed { .. } => "orchestrator_decomposed",
        EventKind::QAStarted { .. } => "qa_started",
        EventKind::QACompleted { .. } => "qa_completed",
        EventKind::QAFailed { .. } => "qa_failed",
        EventKind::BudgetExceeded => "budget_exceeded",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use orch_core::types::{EventId, ModelKind, RepoId, Session, SessionStatus, TaskPriority};
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

    fn mk_session(id: &str) -> Session {
        let now = Utc::now();
        Session {
            id: id.to_string(),
            title: format!("Session {id}"),
            created_at: now,
            updated_at: now,
            task_ids: vec![TaskId::new("T1")],
            parent_session_id: None,
            status: SessionStatus::Active,
        }
    }

    #[test]
    fn sessions_table_created() {
        let store = mk_store();
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sessions'",
                [],
                |row| row.get(0),
            )
            .expect("query sqlite_master");
        assert_eq!(count, 1);
    }

    #[test]
    fn create_and_get_session_round_trip() {
        let store = mk_store();
        let session = mk_session("S-1");

        store.create_session(&session).expect("create session");
        let loaded = store
            .get_session(&session.id)
            .expect("get session")
            .expect("session exists");

        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.title, session.title);
        assert_eq!(loaded.task_ids, session.task_ids);
        assert_eq!(loaded.status, SessionStatus::Active);
    }

    #[test]
    fn list_sessions_orders_by_updated_at_desc() {
        let store = mk_store();
        let mut s1 = mk_session("S-2");
        let mut s2 = mk_session("S-3");
        s1.updated_at = Utc::now() - chrono::Duration::seconds(60);
        s2.updated_at = Utc::now();

        store.create_session(&s1).expect("create s1");
        store.create_session(&s2).expect("create s2");

        let sessions = store.list_sessions().expect("list sessions");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "S-3");
        assert_eq!(sessions[1].id, "S-2");
    }

    #[test]
    fn update_session_persists_changes() {
        let store = mk_store();
        let mut session = mk_session("S-4");
        store.create_session(&session).expect("create session");

        session.title = "Renamed Session".to_string();
        session.status = SessionStatus::Completed;
        session.task_ids.push(TaskId::new("T2"));
        session.updated_at = Utc::now();
        store.update_session(&session).expect("update session");

        let loaded = store
            .get_session(&session.id)
            .expect("get session")
            .expect("session exists");
        assert_eq!(loaded.title, "Renamed Session");
        assert_eq!(loaded.status, SessionStatus::Completed);
        assert_eq!(loaded.task_ids, vec![TaskId::new("T1"), TaskId::new("T2")]);
    }

    #[test]
    fn fork_session_creates_child_with_parent_link() {
        let store = mk_store();
        let mut parent = mk_session("S-5");
        parent.task_ids = vec![TaskId::new("T-A"), TaskId::new("T-B")];
        parent.status = SessionStatus::Completed;
        store.create_session(&parent).expect("create parent");

        let child = store.fork_session(&parent.id).expect("fork session");
        assert!(child.id.starts_with("S-"));
        assert_eq!(child.parent_session_id.as_deref(), Some(parent.id.as_str()));
        assert_eq!(child.task_ids, parent.task_ids);
        assert_eq!(child.status, SessionStatus::Active);

        let loaded = store
            .get_session(&child.id)
            .expect("get child")
            .expect("child exists");
        assert_eq!(loaded.parent_session_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn fork_session_fails_for_missing_parent() {
        let store = mk_store();
        let err = store
            .fork_session("S-DOES-NOT-EXIST")
            .expect_err("missing parent should fail");
        assert!(matches!(err, PersistenceError::SessionNotFound { .. }));
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
            .list_events_for_task(task.id.0.as_str())
            .expect("events")
            .is_empty());
    }

    #[test]
    fn list_events_for_task_returns_correct_events() {
        let store = mk_store();
        let task_a = mk_task("T-EVENT-A", TaskState::Chatting);
        let task_b = mk_task("T-EVENT-B", TaskState::Chatting);
        store.upsert_task(&task_a).expect("upsert task a");
        store.upsert_task(&task_b).expect("upsert task b");

        let first_at = Utc::now();
        let second_at = first_at + chrono::Duration::seconds(1);
        let third_at = first_at + chrono::Duration::seconds(2);

        store
            .append_event(&Event {
                id: EventId("E-EVENT-A-1".to_string()),
                task_id: Some(task_a.id.clone()),
                repo_id: Some(task_a.repo_id.clone()),
                at: second_at,
                kind: EventKind::TaskCreated,
            })
            .expect("append task a event 1");
        store
            .append_event(&Event {
                id: EventId("E-EVENT-B-1".to_string()),
                task_id: Some(task_b.id.clone()),
                repo_id: Some(task_b.repo_id.clone()),
                at: first_at,
                kind: EventKind::TaskCreated,
            })
            .expect("append task b event");
        store
            .append_event(&Event {
                id: EventId("E-EVENT-A-2".to_string()),
                task_id: Some(task_a.id.clone()),
                repo_id: Some(task_a.repo_id.clone()),
                at: third_at,
                kind: EventKind::VerifyStarted,
            })
            .expect("append task a event 2");

        let events = store
            .list_events_for_task(task_a.id.0.as_str())
            .expect("list task events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id.0, "E-EVENT-A-1");
        assert_eq!(events[1].id.0, "E-EVENT-A-2");
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

    #[test]
    fn archive_table_created() {
        let store = mk_store();
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='archived_tasks'",
                [],
                |row| row.get(0),
            )
            .expect("query sqlite_master");
        assert_eq!(count, 1);
    }

    #[test]
    fn archive_task_moves_row_to_archive_table() {
        let store = mk_store();
        let task = mk_task("T-ARCHIVE-1", TaskState::Merged);
        store.upsert_task(&task).expect("upsert");

        store
            .archive_task(&task, Utc::now())
            .expect("archive task should succeed");

        assert!(store.load_task(&task.id).expect("load").is_none());
        let archived = store.list_archived().expect("list archived");
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].task_id, task.id);
        assert_eq!(archived[0].state_tag, "MERGED");
    }

    #[test]
    fn total_event_count_returns_correct_count() {
        let store = mk_store();
        let task = mk_task("T-EVENT-COUNT", TaskState::Chatting);
        store.upsert_task(&task).expect("upsert");

        for idx in 0..3 {
            let event = Event {
                id: EventId(format!("E-EVENT-COUNT-{idx}")),
                task_id: Some(task.id.clone()),
                repo_id: Some(task.repo_id.clone()),
                at: Utc::now(),
                kind: EventKind::TaskCreated,
            };
            store.append_event(&event).expect("append event");
        }

        let count = store.total_event_count().expect("count events");
        assert_eq!(count, 3);
    }

    #[test]
    fn clone_creates_new_task_from_source() {
        let store = mk_store();
        let mut source = mk_task("T-SRC-1", TaskState::Ready);
        source.branch_name = Some("task/T-SRC-1".to_string());
        source.preferred_model = Some(ModelKind::Codex);
        source.depends_on = vec![TaskId::new("T-DEP-1")];
        source.priority = TaskPriority::High;
        store.upsert_task(&source).expect("upsert source");

        let new_id = "T-SRC-1-clone-1";
        store
            .clone_task(&source.id.0, new_id, TaskCloneOverrides::default())
            .expect("clone task");

        let cloned = store
            .load_task(&TaskId::new(new_id))
            .expect("load cloned")
            .expect("cloned exists");

        assert_eq!(cloned.id.0, new_id);
        assert_eq!(cloned.repo_id, source.repo_id);
        assert_eq!(cloned.title, source.title);
        assert_eq!(cloned.branch_name, source.branch_name);
        assert_eq!(cloned.preferred_model, source.preferred_model);
        assert_eq!(cloned.depends_on, source.depends_on);
        assert_eq!(cloned.priority, source.priority);
    }

    #[test]
    fn clone_resets_state_and_retries() {
        let store = mk_store();
        let mut source = mk_task("T-SRC-2", TaskState::Stopped);
        source.retry_count = 3;
        source.failed_models = vec![ModelKind::Claude, ModelKind::Gemini];
        store.upsert_task(&source).expect("upsert source");

        store
            .clone_task(&source.id.0, "T-SRC-2-clone-1", TaskCloneOverrides::default())
            .expect("clone task");

        let cloned = store
            .load_task(&TaskId::new("T-SRC-2-clone-1"))
            .expect("load cloned")
            .expect("cloned exists");

        assert_eq!(cloned.state, TaskState::Chatting);
        assert_eq!(cloned.retry_count, 0);
        assert!(cloned.failed_models.is_empty());
    }

    #[test]
    fn clone_applies_title_override() {
        let store = mk_store();
        let source = mk_task("T-SRC-3", TaskState::Ready);
        store.upsert_task(&source).expect("upsert source");

        store
            .clone_task(
                &source.id.0,
                "T-SRC-3-clone-1",
                TaskCloneOverrides {
                    title: Some("Override title".to_string()),
                    preferred_model: None,
                    priority: None,
                },
            )
            .expect("clone task");

        let cloned = store
            .load_task(&TaskId::new("T-SRC-3-clone-1"))
            .expect("load cloned")
            .expect("cloned exists");
        assert_eq!(cloned.title, "Override title");
    }

    #[test]
    fn clone_fails_for_missing_source() {
        let store = mk_store();
        let err = store
            .clone_task(
                "T-DOES-NOT-EXIST",
                "T-DOES-NOT-EXIST-clone-1",
                TaskCloneOverrides::default(),
            )
            .expect_err("missing source should fail");
        assert!(matches!(err, PersistenceError::TaskNotFound { .. }));
    }
}
