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

    pub fn finish_open_runs_for_task(
        &self,
        task_id: &TaskId,
        finished_at: DateTime<Utc>,
        stop_reason: &str,
        exit_code: Option<i32>,
    ) -> Result<usize, PersistenceError> {
        let updated = self.conn.execute(
            r#"
UPDATE runs
SET finished_at = ?1, stop_reason = ?2, exit_code = ?3
WHERE task_id = ?4 AND finished_at IS NULL
"#,
            params![finished_at.to_rfc3339(), stop_reason, exit_code, task_id.0],
        )?;
        Ok(updated)
    }

    pub fn list_open_runs(&self) -> Result<Vec<TaskRunRecord>, PersistenceError> {
        let mut stmt = self.conn.prepare(
            "SELECT payload_json FROM runs WHERE finished_at IS NULL ORDER BY started_at ASC, run_id ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut runs = Vec::new();
        for row in rows {
            let payload = row?;
            runs.push(serde_json::from_str::<TaskRunRecord>(&payload)?);
        }
        Ok(runs)
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

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use orch_core::events::{Event, EventKind, ReviewVerdict};
    use orch_core::state::{ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus};
    use orch_core::types::{
        EventId, ModelKind, RepoId, SubmitMode, Task, TaskApproval, TaskId, TaskRole, TaskType,
    };
    use rusqlite::params;
    use std::path::PathBuf;

    use crate::types::{ArtifactRecord, TaskRunRecord};

    use super::SqliteStore;

    fn mk_store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().expect("in-memory store");
        store.migrate().expect("migrate");
        store
    }

    fn mk_task(id: &str, state: TaskState, updated_at: chrono::DateTime<Utc>) -> Task {
        Task {
            id: TaskId(id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Task {id}"),
            state,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            branch_name: Some(format!("task/{id}")),
            worktree_path: PathBuf::from(format!(".orch/wt/{id}")),
            pr: None,
            verify_status: VerifyStatus::NotRun,
            review_status: ReviewStatus {
                required_models: Vec::new(),
                approvals_received: 0,
                approvals_required: 0,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            created_at: updated_at,
            updated_at,
        }
    }

    fn mk_event(id: &str, task_id: &str, at: chrono::DateTime<Utc>) -> Event {
        Event {
            id: EventId(id.to_string()),
            task_id: Some(TaskId(task_id.to_string())),
            repo_id: Some(RepoId("example".to_string())),
            at,
            kind: EventKind::TaskCreated,
        }
    }

    #[test]
    fn upsert_and_load_task_roundtrip() {
        let store = mk_store();
        let task = mk_task("T1", TaskState::Running, Utc::now());
        store.upsert_task(&task).expect("upsert task");

        let loaded = store
            .load_task(&TaskId("T1".to_string()))
            .expect("load task")
            .expect("task exists");
        assert_eq!(loaded, task);
    }

    #[test]
    fn list_tasks_by_state_filters_and_sorts_by_updated_desc() {
        let store = mk_store();
        let base = Utc::now();
        let t1 = mk_task("T1", TaskState::Running, base + Duration::seconds(1));
        let t2 = mk_task("T2", TaskState::Reviewing, base + Duration::seconds(2));
        let t3 = mk_task("T3", TaskState::Running, base + Duration::seconds(3));
        store.upsert_task(&t1).expect("upsert t1");
        store.upsert_task(&t2).expect("upsert t2");
        store.upsert_task(&t3).expect("upsert t3");

        let running = store
            .list_tasks_by_state(TaskState::Running)
            .expect("list by state");
        let ids = running
            .into_iter()
            .map(|task| task.id.0)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["T3".to_string(), "T1".to_string()]);
    }

    #[test]
    fn append_event_and_latest_event_at_for_task() {
        let store = mk_store();
        let base = Utc::now();
        let e1 = mk_event("E1", "T1", base);
        let e2 = mk_event("E2", "T1", base + Duration::seconds(10));
        store.append_event(&e1).expect("append e1");
        store.append_event(&e2).expect("append e2");

        let events = store
            .list_events_for_task(&TaskId("T1".to_string()))
            .expect("list events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id.0, "E1");
        assert_eq!(events[1].id.0, "E2");

        let latest = store
            .latest_event_at_for_task(&TaskId("T1".to_string()))
            .expect("latest event")
            .expect("latest present");
        assert_eq!(latest, e2.at);
    }

    #[test]
    fn list_events_global_orders_by_timestamp_then_event_id() {
        let store = mk_store();
        let base = Utc::now();
        let e2 = mk_event("E2", "T1", base);
        let e1 = mk_event("E1", "T2", base);
        let e3 = mk_event("E3", "T1", base + Duration::seconds(1));

        store.append_event(&e2).expect("append e2");
        store.append_event(&e1).expect("append e1");
        store.append_event(&e3).expect("append e3");

        let events = store.list_events_global().expect("list global events");
        let ids = events
            .into_iter()
            .map(|event| event.id.0)
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["E1".to_string(), "E2".to_string(), "E3".to_string()]
        );
    }

    #[test]
    fn latest_event_at_for_task_returns_none_when_task_has_no_events() {
        let store = mk_store();
        let latest = store
            .latest_event_at_for_task(&TaskId("NO-EVENTS".to_string()))
            .expect("latest event query");
        assert_eq!(latest, None);
    }

    #[test]
    fn upsert_approval_replaces_existing_reviewer_verdict() {
        let store = mk_store();
        let task_id = TaskId("T1".to_string());
        let first = TaskApproval {
            task_id: task_id.clone(),
            reviewer: ModelKind::Codex,
            verdict: ReviewVerdict::Approve,
            issued_at: Utc::now(),
        };
        let second = TaskApproval {
            task_id: task_id.clone(),
            reviewer: ModelKind::Codex,
            verdict: ReviewVerdict::RequestChanges,
            issued_at: Utc::now() + Duration::seconds(5),
        };
        store.upsert_approval(&first).expect("upsert first");
        store.upsert_approval(&second).expect("upsert second");

        let approvals = store
            .list_approvals_for_task(&task_id)
            .expect("list approvals");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].reviewer, ModelKind::Codex);
        assert_eq!(approvals[0].verdict, ReviewVerdict::RequestChanges);
    }

    #[test]
    fn insert_run_and_artifact_persist_rows() {
        let store = mk_store();
        let now = Utc::now();
        let run = TaskRunRecord {
            run_id: "R1".to_string(),
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Claude,
            started_at: now,
            finished_at: Some(now + Duration::seconds(1)),
            stop_reason: Some("completed".to_string()),
            exit_code: Some(0),
        };
        let artifact = ArtifactRecord {
            artifact_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            kind: "patch".to_string(),
            path: "/tmp/patch.diff".to_string(),
            created_at: now,
            metadata_json: Some("{\"size\":1}".to_string()),
        };
        store.insert_run(&run).expect("insert run");
        store.insert_artifact(&artifact).expect("insert artifact");

        let run_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id = ?1",
                params!["R1"],
                |row| row.get(0),
            )
            .expect("count run");
        let artifact_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM artifacts WHERE artifact_id = ?1",
                params!["A1"],
                |row| row.get(0),
            )
            .expect("count artifact");
        assert_eq!(run_count, 1);
        assert_eq!(artifact_count, 1);
    }

    #[test]
    fn list_open_runs_returns_only_unfinished_runs_ordered_by_started_at() {
        let store = mk_store();
        let base = Utc::now();
        let open_earlier = TaskRunRecord {
            run_id: "R-OPEN-EARLY".to_string(),
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Codex,
            started_at: base,
            finished_at: None,
            stop_reason: None,
            exit_code: None,
        };
        let closed = TaskRunRecord {
            run_id: "R-CLOSED".to_string(),
            task_id: TaskId("T2".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Claude,
            started_at: base + Duration::seconds(1),
            finished_at: Some(base + Duration::seconds(2)),
            stop_reason: Some("completed".to_string()),
            exit_code: Some(0),
        };
        let open_later = TaskRunRecord {
            run_id: "R-OPEN-LATE".to_string(),
            task_id: TaskId("T3".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Gemini,
            started_at: base + Duration::seconds(3),
            finished_at: None,
            stop_reason: None,
            exit_code: None,
        };

        store.insert_run(&open_later).expect("insert open later");
        store.insert_run(&closed).expect("insert closed");
        store
            .insert_run(&open_earlier)
            .expect("insert open earlier");

        let open_runs = store.list_open_runs().expect("list open runs");
        let run_ids = open_runs
            .iter()
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            run_ids,
            vec!["R-OPEN-EARLY".to_string(), "R-OPEN-LATE".to_string()]
        );
    }

    #[test]
    fn finish_open_runs_for_task_marks_only_unfinished_rows_for_task() {
        let store = mk_store();
        let base = Utc::now();
        let open_target = TaskRunRecord {
            run_id: "R-TARGET-OPEN".to_string(),
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Codex,
            started_at: base,
            finished_at: None,
            stop_reason: None,
            exit_code: None,
        };
        let closed_target = TaskRunRecord {
            run_id: "R-TARGET-CLOSED".to_string(),
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Codex,
            started_at: base + Duration::seconds(1),
            finished_at: Some(base + Duration::seconds(2)),
            stop_reason: Some("done".to_string()),
            exit_code: Some(0),
        };
        let other_open = TaskRunRecord {
            run_id: "R-OTHER-OPEN".to_string(),
            task_id: TaskId("T2".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Claude,
            started_at: base + Duration::seconds(3),
            finished_at: None,
            stop_reason: None,
            exit_code: None,
        };

        store.insert_run(&open_target).expect("insert target open");
        store
            .insert_run(&closed_target)
            .expect("insert target closed");
        store.insert_run(&other_open).expect("insert other open");

        let count = store
            .finish_open_runs_for_task(
                &TaskId("T1".to_string()),
                base + Duration::seconds(9),
                "initialized",
                Some(0),
            )
            .expect("finish open runs for task");
        assert_eq!(count, 1);

        let open_runs = store.list_open_runs().expect("list open runs");
        let open_ids = open_runs
            .iter()
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(open_ids, vec!["R-OTHER-OPEN".to_string()]);
    }
}
