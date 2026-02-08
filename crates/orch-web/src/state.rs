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
