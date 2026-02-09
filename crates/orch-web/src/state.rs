use chrono::Utc;
use orch_core::types::Task;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::model::{SandboxRunView, WebEvent, WebEventKind};

#[derive(Debug, Default)]
struct WebStateInner {
    tasks: BTreeMap<String, Task>,
    sandboxes: BTreeMap<String, SandboxRunView>,
}

#[derive(Debug, Clone)]
pub struct WebState {
    inner: Arc<RwLock<WebStateInner>>,
    events_tx: broadcast::Sender<WebEvent>,
    sandbox_counter: Arc<AtomicU64>,
}

impl Default for WebState {
    fn default() -> Self {
        let (events_tx, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(RwLock::new(WebStateInner::default())),
            events_tx,
            sandbox_counter: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl WebState {
    pub async fn replace_tasks(&self, tasks: Vec<Task>) {
        let mut guard = self.inner.write().await;
        guard.tasks.clear();
        for task in tasks {
            guard.tasks.insert(task.id.0.clone(), task);
        }
        drop(guard);
        self.emit(WebEventKind::TasksReplaced {
            count: self.list_tasks().await.len(),
        });
    }

    pub async fn upsert_task(&self, task: Task) {
        let task_id = task.id.0.clone();
        let state = task.state;
        let mut guard = self.inner.write().await;
        guard.tasks.insert(task_id.clone(), task);
        drop(guard);
        self.emit(WebEventKind::TaskUpserted { task_id, state });
    }

    pub async fn list_tasks(&self) -> Vec<Task> {
        let guard = self.inner.read().await;
        guard.tasks.values().cloned().collect()
    }

    pub async fn task(&self, task_id: &str) -> Option<Task> {
        let guard = self.inner.read().await;
        guard.tasks.get(task_id).cloned()
    }

    pub async fn upsert_sandbox(&self, sandbox: SandboxRunView) {
        let sandbox_id = sandbox.sandbox_id.clone();
        let status = sandbox.status.clone();
        let mut guard = self.inner.write().await;
        guard.sandboxes.insert(sandbox_id.clone(), sandbox);
        drop(guard);
        self.emit(WebEventKind::SandboxUpdated { sandbox_id, status });
    }

    pub async fn update_sandbox<F>(&self, sandbox_id: &str, updater: F) -> Option<SandboxRunView>
    where
        F: FnOnce(&mut SandboxRunView),
    {
        let mut guard = self.inner.write().await;
        let sandbox = guard.sandboxes.get_mut(sandbox_id)?;
        updater(sandbox);
        sandbox.updated_at = Utc::now();
        let snapshot = sandbox.clone();
        drop(guard);
        self.emit(WebEventKind::SandboxUpdated {
            sandbox_id: snapshot.sandbox_id.clone(),
            status: snapshot.status.clone(),
        });
        Some(snapshot)
    }

    pub async fn sandbox(&self, sandbox_id: &str) -> Option<SandboxRunView> {
        let guard = self.inner.read().await;
        guard.sandboxes.get(sandbox_id).cloned()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WebEvent> {
        self.events_tx.subscribe()
    }

    pub fn next_sandbox_id(&self) -> String {
        let id = self.sandbox_counter.fetch_add(1, Ordering::Relaxed);
        format!("SBX-{id}")
    }

    fn emit(&self, kind: WebEventKind) {
        let _ = self.events_tx.send(WebEvent {
            at: Utc::now(),
            kind,
        });
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::state::{ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus};
    use orch_core::types::{RepoId, SubmitMode, Task, TaskId, TaskRole, TaskType};
    use std::path::PathBuf;
    use tokio::sync::broadcast::Receiver;
    use tokio::time::{timeout, Duration};

    use crate::model::{SandboxRunView, SandboxStatus, SandboxTarget, WebEvent};

    use super::WebState;

    fn mk_task(id: &str, state: TaskState) -> Task {
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
            patch_ready: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn mk_sandbox(id: &str, status: SandboxStatus) -> SandboxRunView {
        SandboxRunView {
            sandbox_id: id.to_string(),
            target: SandboxTarget::Task {
                task_id: "T1".to_string(),
            },
            status,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            sandbox_path: Some(PathBuf::from(format!("/tmp/{id}"))),
            checkout_ref: Some("HEAD".to_string()),
            cleanup_worktree: true,
            worktree_cleaned: false,
            worktree_cleanup_error: None,
            logs: Vec::new(),
            last_error: None,
        }
    }

    async fn recv_event(rx: &mut Receiver<WebEvent>) -> WebEvent {
        timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("event timeout")
            .expect("event received")
    }

    #[tokio::test]
    async fn replace_tasks_emits_count_event_and_persists_tasks() {
        let state = WebState::default();
        let mut rx = state.subscribe();

        state
            .replace_tasks(vec![
                mk_task("T1", TaskState::AwaitingMerge),
                mk_task("T2", TaskState::Running),
            ])
            .await;

        let event = recv_event(&mut rx).await;
        assert!(matches!(
            event.kind,
            crate::model::WebEventKind::TasksReplaced { count } if count == 2
        ));
        assert_eq!(state.list_tasks().await.len(), 2);
    }

    #[tokio::test]
    async fn upsert_task_emits_event_and_allows_lookup() {
        let state = WebState::default();
        let mut rx = state.subscribe();

        state.upsert_task(mk_task("T9", TaskState::Reviewing)).await;

        let event = recv_event(&mut rx).await;
        assert!(matches!(
            event.kind,
            crate::model::WebEventKind::TaskUpserted { task_id, state: task_state }
                if task_id == "T9" && task_state == TaskState::Reviewing
        ));
        let loaded = state.task("T9").await.expect("task exists");
        assert_eq!(loaded.state, TaskState::Reviewing);
    }

    #[tokio::test]
    async fn upsert_and_update_sandbox_emit_events_and_mutate_state() {
        let state = WebState::default();
        let mut rx = state.subscribe();

        state
            .upsert_sandbox(mk_sandbox("SBX-1", SandboxStatus::Queued))
            .await;
        let queued_event = recv_event(&mut rx).await;
        assert!(matches!(
            queued_event.kind,
            crate::model::WebEventKind::SandboxUpdated { sandbox_id, status }
                if sandbox_id == "SBX-1" && status == SandboxStatus::Queued
        ));

        let before = state
            .sandbox("SBX-1")
            .await
            .expect("sandbox exists")
            .updated_at;
        let updated = state
            .update_sandbox("SBX-1", |run| {
                run.status = SandboxStatus::Running;
            })
            .await
            .expect("sandbox updated");
        assert_eq!(updated.status, SandboxStatus::Running);
        assert!(updated.updated_at >= before);

        let running_event = recv_event(&mut rx).await;
        assert!(matches!(
            running_event.kind,
            crate::model::WebEventKind::SandboxUpdated { sandbox_id, status }
                if sandbox_id == "SBX-1" && status == SandboxStatus::Running
        ));
    }

    #[tokio::test]
    async fn update_sandbox_returns_none_for_unknown_id_and_emits_no_event() {
        let state = WebState::default();
        let mut rx = state.subscribe();

        let updated = state
            .update_sandbox("SBX-404", |run| {
                run.status = SandboxStatus::Failed;
            })
            .await;
        assert!(updated.is_none());

        let next = timeout(Duration::from_millis(100), rx.recv()).await;
        assert!(
            next.is_err(),
            "no event should be emitted for missing sandbox"
        );
    }

    #[tokio::test]
    async fn replace_tasks_replaces_previous_task_set() {
        let state = WebState::default();
        state
            .replace_tasks(vec![
                mk_task("T1", TaskState::Running),
                mk_task("T2", TaskState::Reviewing),
            ])
            .await;
        assert_eq!(state.list_tasks().await.len(), 2);

        state
            .replace_tasks(vec![mk_task("T3", TaskState::AwaitingMerge)])
            .await;

        let tasks = state.list_tasks().await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id.0, "T3");
        assert!(state.task("T1").await.is_none());
        assert!(state.task("T2").await.is_none());
    }

    #[test]
    fn next_sandbox_id_increments_monotonically() {
        let state = WebState::default();
        assert_eq!(state.next_sandbox_id(), "SBX-1");
        assert_eq!(state.next_sandbox_id(), "SBX-2");
        assert_eq!(state.next_sandbox_id(), "SBX-3");
    }

    #[test]
    fn next_sandbox_id_counter_is_shared_across_clones() {
        let state = WebState::default();
        let cloned = state.clone();

        assert_eq!(state.next_sandbox_id(), "SBX-1");
        assert_eq!(cloned.next_sandbox_id(), "SBX-2");
        assert_eq!(state.next_sandbox_id(), "SBX-3");
    }
}
