//! MVP Orchestrator Service.
//!
//! Simplified service for managing chats (AI coding sessions) that
//! auto-submit to Graphite with clean stacking.

use chrono::{DateTime, Utc};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{EventId, SubmitMode, Task, TaskId};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::dependency_graph::{build_dependency_graph, restack_descendants_for_parent};
use crate::event_log::{EventLogError, JsonlEventLog};
use crate::persistence::{PersistenceError, SqliteStore};
use crate::scheduler::{
    BlockedTask, ModelAvailability, QueuedTask, RunningTask, SchedulePlan, ScheduledAssignment,
    Scheduler, SchedulingInput,
};
use crate::state_machine::{task_state_tag, transition_task, StateMachineError};
use crate::types::TaskRunRecord;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    EventLog(#[from] EventLogError),
    #[error(transparent)]
    StateMachine(#[from] StateMachineError),
    #[error("task not found: {task_id}")]
    TaskNotFound { task_id: String },
}

/// Event IDs for state transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateChangeEventIds {
    pub state_changed: EventId,
}

/// Outcome of scheduling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulingTickOutcome {
    pub scheduled: Vec<ScheduledAssignment>,
    pub blocked: Vec<BlockedTask>,
}

/// The main service.
pub struct OrchdService {
    pub store: SqliteStore,
    pub event_log: JsonlEventLog,
    pub scheduler: Scheduler,
}

impl OrchdService {
    pub fn new(store: SqliteStore, event_log: JsonlEventLog, scheduler: Scheduler) -> Self {
        Self {
            store,
            event_log,
            scheduler,
        }
    }

    pub fn open(
        sqlite_path: impl Into<PathBuf>,
        event_log_root: impl Into<PathBuf>,
        scheduler: Scheduler,
    ) -> Result<Self, ServiceError> {
        let store = SqliteStore::open(sqlite_path.into())?;
        let event_log = JsonlEventLog::new(event_log_root.into());
        let svc = Self::new(store, event_log, scheduler);
        svc.bootstrap()?;
        Ok(svc)
    }

    pub fn bootstrap(&self) -> Result<(), ServiceError> {
        self.store.migrate()?;
        self.event_log.ensure_layout()?;
        Ok(())
    }

    // --- Task CRUD ---

    pub fn create_task(&self, task: &Task, created_event: &Event) -> Result<(), ServiceError> {
        self.store.upsert_task(task)?;
        self.record_event(created_event)?;
        Ok(())
    }

    pub fn upsert_task(&self, task: &Task) -> Result<(), ServiceError> {
        self.store.upsert_task(task)?;
        Ok(())
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, ServiceError> {
        Ok(self.store.list_tasks()?)
    }

    pub fn list_tasks_by_state(&self, state: TaskState) -> Result<Vec<Task>, ServiceError> {
        Ok(self.store.list_tasks_by_state(state)?)
    }

    pub fn task(&self, task_id: &TaskId) -> Result<Option<Task>, ServiceError> {
        Ok(self.store.load_task(task_id)?)
    }

    /// List only top-level tasks (those without a parent_task_id).
    /// Sub-tasks created by orchestrator decomposition are filtered out.
    pub fn list_top_level_tasks(&self) -> Result<Vec<Task>, ServiceError> {
        let all = self.store.list_tasks()?;
        Ok(all
            .into_iter()
            .filter(|t| t.parent_task_id.is_none())
            .collect())
    }

    pub fn delete_task(&self, task_id: &TaskId) -> Result<bool, ServiceError> {
        Ok(self.store.delete_task(task_id)?)
    }

    // --- Events ---

    pub fn record_event(&self, event: &Event) -> Result<(), ServiceError> {
        self.store.append_event(event)?;
        self.event_log.append_both(event)?;
        Ok(())
    }

    pub fn task_events(&self, task_id: &TaskId) -> Result<Vec<Event>, ServiceError> {
        Ok(self.store.list_events_for_task(&task_id.0)?)
    }

    pub fn global_events(&self) -> Result<Vec<Event>, ServiceError> {
        Ok(self.store.list_events_global()?)
    }

    pub fn task_runs(
        &self,
        task_id: &TaskId,
    ) -> Result<Vec<crate::types::TaskRunRecord>, ServiceError> {
        Ok(self.store.list_runs_for_task(task_id)?)
    }

    pub fn runs_by_model(&self) -> Result<Vec<(String, i64)>, ServiceError> {
        Ok(self.store.count_runs_by_model()?)
    }

    // --- State Transitions ---

    pub fn transition_task_state(
        &self,
        task_id: &TaskId,
        to: TaskState,
        event_id: EventId,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;
        let transition = transition_task(&mut task, to, at)?;
        self.store.upsert_task(&task)?;

        let event = Event {
            id: event_id,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::TaskStateChanged {
                from: task_state_tag(transition.from).to_string(),
                to: task_state_tag(transition.to).to_string(),
            },
        };
        self.record_event(&event)?;
        Ok(task)
    }

    /// Increment the retry count for a task and store the failure reason.
    pub fn increment_retry(&self, task_id: &TaskId, reason: &str) -> Result<(), ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;
        task.retry_count += 1;
        task.last_failure_reason = Some(reason.to_string());
        self.store.upsert_task(&task)?;
        Ok(())
    }

    /// Mark a chat as ready (coding complete, verified).
    pub fn mark_ready(
        &self,
        task_id: &TaskId,
        event_id: EventId,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        self.transition_task_state(task_id, TaskState::Ready, event_id, at)
    }

    /// Start submitting a chat to Graphite.
    pub fn start_submit(
        &self,
        task_id: &TaskId,
        mode: SubmitMode,
        event_id: EventId,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let task =
            self.transition_task_state(task_id, TaskState::Submitting, event_id.clone(), at)?;

        self.record_event(&Event {
            id: EventId(format!("{}-submit-started", event_id.0)),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::SubmitStarted { mode },
        })?;

        Ok(task)
    }

    /// Complete submit - move to AwaitingMerge.
    pub fn complete_submit(
        &self,
        task_id: &TaskId,
        pr_url: String,
        pr_number: u64,
        event_id: EventId,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        task.mark_submitted(pr_url, pr_number);
        self.store.upsert_task(&task)?;

        self.record_event(&Event {
            id: event_id.clone(),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::TaskStateChanged {
                from: "SUBMITTING".to_string(),
                to: "AWAITING_MERGE".to_string(),
            },
        })?;

        self.record_event(&Event {
            id: EventId(format!("{}-submit-completed", event_id.0)),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::SubmitCompleted,
        })?;

        Ok(task)
    }

    /// Mark a chat as merged.
    pub fn mark_merged(
        &self,
        task_id: &TaskId,
        event_id: EventId,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        self.transition_task_state(task_id, TaskState::Merged, event_id, at)
    }

    /// Start restacking (rebasing onto parent).
    pub fn start_restack(
        &self,
        task_id: &TaskId,
        event_id: EventId,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let task =
            self.transition_task_state(task_id, TaskState::Restacking, event_id.clone(), at)?;

        self.record_event(&Event {
            id: EventId(format!("{}-restack-started", event_id.0)),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::RestackStarted,
        })?;

        Ok(task)
    }

    /// Complete restack - move back to Ready.
    pub fn complete_restack(
        &self,
        task_id: &TaskId,
        event_id: EventId,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let task = self.transition_task_state(task_id, TaskState::Ready, event_id.clone(), at)?;

        self.record_event(&Event {
            id: EventId(format!("{}-restack-completed", event_id.0)),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::RestackCompleted,
        })?;

        Ok(task)
    }

    // --- Dependency Graph ---

    /// Get tasks that need restacking when a parent task is updated.
    pub fn restack_targets_for_parent(
        &self,
        parent_task_id: &TaskId,
    ) -> Result<Vec<TaskId>, ServiceError> {
        let tasks = self.store.list_tasks()?;
        let graph = build_dependency_graph(&tasks);
        Ok(restack_descendants_for_parent(&graph, parent_task_id))
    }

    // --- Scheduling ---

    pub fn schedule(&self, input: SchedulingInput) -> SchedulePlan {
        self.scheduler.plan(input)
    }

    /// Schedule queued tasks for execution.
    pub fn schedule_queued_tasks(
        &self,
        enabled_models: &[orch_core::types::ModelKind],
        availability: &[ModelAvailability],
        at: DateTime<Utc>,
    ) -> Result<SchedulingTickOutcome, ServiceError> {
        let chatting_tasks = self.store.list_tasks_by_state(TaskState::Chatting)?;
        if chatting_tasks.is_empty() {
            return Ok(SchedulingTickOutcome {
                scheduled: Vec::new(),
                blocked: Vec::new(),
            });
        }

        let all_task_states = self
            .store
            .list_tasks()?
            .into_iter()
            .map(|task| (task.id, task.state))
            .collect::<HashMap<_, _>>();

        let running = self
            .store
            .list_open_runs()?
            .into_iter()
            .map(|run| RunningTask {
                task_id: run.task_id,
                repo_id: run.repo_id,
                model: run.model,
            })
            .collect::<Vec<_>>();

        let queued = chatting_tasks
            .iter()
            .map(|task| QueuedTask {
                task_id: task.id.clone(),
                repo_id: task.repo_id.clone(),
                depends_on: task.depends_on.clone(),
                preferred_model: task.preferred_model,
                priority: task.priority,
                enqueued_at: task.created_at,
            })
            .collect::<Vec<_>>();

        let plan = self.scheduler.plan(SchedulingInput {
            queued,
            running,
            all_task_states,
            enabled_models: enabled_models.to_vec(),
            availability: availability.to_vec(),
        });

        // Record runs for scheduled tasks
        let tick_nonce = at.timestamp_nanos_opt().unwrap_or_default();
        for assignment in &plan.assignments {
            let run = TaskRunRecord {
                run_id: format!(
                    "RUN-{}-{}-{tick_nonce}",
                    assignment.task_id.0,
                    assignment.model.as_str()
                ),
                task_id: assignment.task_id.clone(),
                repo_id: assignment.repo_id.clone(),
                model: assignment.model,
                started_at: at,
                finished_at: None,
                stop_reason: None,
                exit_code: None,
                estimated_tokens: None,
                duration_secs: None,
            };
            self.store.insert_run(&run)?;
        }

        Ok(SchedulingTickOutcome {
            scheduled: plan.assignments,
            blocked: plan.blocked,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::SchedulerConfig;
    use chrono::Utc;
    use orch_core::types::{ModelKind, RepoId};
    use std::fs;
    use std::path::PathBuf;

    fn mk_service() -> OrchdService {
        let store = SqliteStore::open_in_memory().expect("in-memory db");
        let dir = std::env::temp_dir().join(format!(
            "othala-orchd-test-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");

        let svc = OrchdService::new(
            store,
            JsonlEventLog::new(dir),
            Scheduler::new(SchedulerConfig {
                per_repo_limit: 10,
                per_model_limit: vec![
                    (ModelKind::Claude, 10),
                    (ModelKind::Codex, 10),
                    (ModelKind::Gemini, 10),
                ]
                .into_iter()
                .collect(),
            }),
        );
        svc.bootstrap().expect("bootstrap");
        svc
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

    fn mk_created_event(task: &Task) -> Event {
        Event {
            id: EventId(format!("E-CREATE-{}", task.id.0)),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at: Utc::now(),
            kind: EventKind::TaskCreated,
        }
    }

    #[test]
    fn create_and_list_tasks() {
        let svc = mk_service();
        let task = mk_task("T1", TaskState::Chatting);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let tasks = svc.list_tasks().expect("list tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, task.id);
    }

    #[test]
    fn transition_chatting_to_ready() {
        let svc = mk_service();
        let task = mk_task("T1", TaskState::Chatting);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .mark_ready(&task.id, EventId("E-READY".to_string()), Utc::now())
            .expect("mark ready");
        assert_eq!(updated.state, TaskState::Ready);
    }

    #[test]
    fn full_submit_flow() {
        let svc = mk_service();
        let task = mk_task("T1", TaskState::Chatting);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        // Chatting -> Ready
        svc.mark_ready(&task.id, EventId("E1".to_string()), Utc::now())
            .expect("mark ready");

        // Ready -> Submitting
        svc.start_submit(
            &task.id,
            SubmitMode::Single,
            EventId("E2".to_string()),
            Utc::now(),
        )
        .expect("start submit");

        // Submitting -> AwaitingMerge
        let task = svc
            .complete_submit(
                &task.id,
                "https://github.com/test/pr/1".to_string(),
                1,
                EventId("E3".to_string()),
                Utc::now(),
            )
            .expect("complete submit");
        assert_eq!(task.state, TaskState::AwaitingMerge);

        // AwaitingMerge -> Merged
        let task = svc
            .mark_merged(&task.id, EventId("E4".to_string()), Utc::now())
            .expect("mark merged");
        assert_eq!(task.state, TaskState::Merged);
    }

    #[test]
    fn restack_flow() {
        let svc = mk_service();
        let task = mk_task("T1", TaskState::Ready);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        // Ready -> Restacking
        svc.start_restack(&task.id, EventId("E1".to_string()), Utc::now())
            .expect("start restack");

        let loaded = svc.task(&task.id).expect("load").unwrap();
        assert_eq!(loaded.state, TaskState::Restacking);

        // Restacking -> Ready
        let task = svc
            .complete_restack(&task.id, EventId("E2".to_string()), Utc::now())
            .expect("complete restack");
        assert_eq!(task.state, TaskState::Ready);
    }

    #[test]
    fn delete_task_removes_it() {
        let svc = mk_service();
        let task = mk_task("T1", TaskState::Chatting);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        assert!(svc.delete_task(&task.id).expect("delete"));
        assert!(svc.task(&task.id).expect("load").is_none());
    }
}
