//! Core types for the MVP orchestrator.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::state::{TaskState, VerifyStatus};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for TaskId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoId(pub String);

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for RepoId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub String);

impl std::fmt::Display for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for EventId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Claude,
    Codex,
    Gemini,
}

impl ModelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelKind::Claude => "claude",
            ModelKind::Codex => "codex",
            ModelKind::Gemini => "gemini",
        }
    }
}

impl std::fmt::Display for ModelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitMode {
    Single,
    Stack,
}

/// The type of work a task performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    /// Standard code implementation.
    #[default]
    Implement,
    /// Write a test specification before implementation.
    TestSpecWrite,
    /// Validate code against a test specification.
    TestValidate,
    /// High-level orchestration / task decomposition.
    Orchestrate,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub number: u64,
    pub url: String,
    pub draft: bool,
}

/// Task specification for creating new tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub repo_id: RepoId,
    pub task_id: TaskId,
    pub title: String,
    pub preferred_model: Option<ModelKind>,
    #[serde(default)]
    pub depends_on: Vec<TaskId>,
    pub submit_mode: Option<SubmitMode>,
}

/// A task (AI coding session) - simplified for MVP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub repo_id: RepoId,
    pub title: String,
    pub state: TaskState,
    pub preferred_model: Option<ModelKind>,
    pub depends_on: Vec<TaskId>,
    pub submit_mode: SubmitMode,
    pub branch_name: Option<String>,
    pub worktree_path: PathBuf,
    pub pr: Option<PullRequestRef>,
    #[serde(default)]
    pub verify_status: VerifyStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // --- Orchestrator extensions (all serde(default) for backward compat) ---

    /// Current retry attempt (0 = first try).
    #[serde(default)]
    pub retry_count: u32,
    /// Maximum retries before giving up.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Models that have already failed on this task.
    #[serde(default)]
    pub failed_models: Vec<ModelKind>,
    /// Why the last attempt failed.
    #[serde(default)]
    pub last_failure_reason: Option<String>,
    /// The kind of work this task performs.
    #[serde(default)]
    pub task_type: TaskType,
    /// Path to the test spec file for this task.
    #[serde(default)]
    pub test_spec_path: Option<PathBuf>,
    /// Parent task ID (for decomposed sub-tasks).
    #[serde(default)]
    pub parent_task_id: Option<TaskId>,
}

fn default_max_retries() -> u32 {
    3
}

impl Task {
    /// Create a new task in Chatting state.
    pub fn new(id: TaskId, repo_id: RepoId, title: String, worktree_path: PathBuf) -> Self {
        let now = Utc::now();
        Self {
            id,
            repo_id,
            title,
            state: TaskState::Chatting,
            preferred_model: None,
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            branch_name: None,
            worktree_path,
            pr: None,
            verify_status: VerifyStatus::NotRun,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: default_max_retries(),
            failed_models: Vec::new(),
            last_failure_reason: None,
            task_type: TaskType::default(),
            test_spec_path: None,
            parent_task_id: None,
        }
    }

    /// Add explicit dependency.
    pub fn with_dependency(mut self, dep: TaskId) -> Self {
        self.depends_on.push(dep);
        self
    }

    /// Set preferred model.
    pub fn with_model(mut self, model: ModelKind) -> Self {
        self.preferred_model = Some(model);
        self
    }

    /// Check if all explicit dependencies are resolved (merged).
    pub fn dependencies_resolved(&self, tasks: &[Task]) -> bool {
        self.depends_on.iter().all(|dep_id| {
            tasks
                .iter()
                .find(|t| &t.id == dep_id)
                .map(|t| t.state == TaskState::Merged)
                .unwrap_or(false)
        })
    }

    /// Transition to Ready state.
    pub fn mark_ready(&mut self) {
        self.state = TaskState::Ready;
        self.updated_at = Utc::now();
    }

    /// Transition to Submitting state.
    pub fn mark_submitting(&mut self) {
        self.state = TaskState::Submitting;
        self.updated_at = Utc::now();
    }

    /// Transition to Restacking state.
    pub fn mark_restacking(&mut self) {
        self.state = TaskState::Restacking;
        self.updated_at = Utc::now();
    }

    /// Transition to AwaitingMerge state with PR URL.
    pub fn mark_submitted(&mut self, pr_url: String, pr_number: u64) {
        self.state = TaskState::AwaitingMerge;
        self.pr = Some(PullRequestRef {
            number: pr_number,
            url: pr_url,
            draft: false,
        });
        self.updated_at = Utc::now();
    }

    /// Transition to Merged state.
    pub fn mark_merged(&mut self) {
        self.state = TaskState::Merged;
        self.updated_at = Utc::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task(id: &str, state: TaskState) -> Task {
        let mut task = Task::new(
            TaskId::new(id),
            RepoId("test-repo".to_string()),
            format!("Test {}", id),
            PathBuf::from(format!(".orch/wt/{}", id)),
        );
        task.state = state;
        task
    }

    #[test]
    fn new_task_starts_in_chatting_state() {
        let task = Task::new(
            TaskId::new("T1"),
            RepoId("repo".to_string()),
            "Test".to_string(),
            PathBuf::from(".orch/wt/T1"),
        );
        assert_eq!(task.state, TaskState::Chatting);
    }

    #[test]
    fn dependencies_resolved_when_all_merged() {
        let t1 = make_task("T1", TaskState::Merged);
        let t2 = make_task("T2", TaskState::Merged);
        let t3 = make_task("T3", TaskState::Chatting)
            .with_dependency(TaskId::new("T1"))
            .with_dependency(TaskId::new("T2"));

        let tasks = vec![t1, t2, t3.clone()];
        assert!(t3.dependencies_resolved(&tasks));
    }

    #[test]
    fn dependencies_not_resolved_when_some_not_merged() {
        let t1 = make_task("T1", TaskState::Merged);
        let t2 = make_task("T2", TaskState::Ready); // Not merged!
        let t3 = make_task("T3", TaskState::Chatting)
            .with_dependency(TaskId::new("T1"))
            .with_dependency(TaskId::new("T2"));

        let tasks = vec![t1, t2, t3.clone()];
        assert!(!t3.dependencies_resolved(&tasks));
    }

    #[test]
    fn task_spec_deserializes_with_defaults() {
        let spec: TaskSpec = toml::from_str(
            r#"
repo_id = "example"
task_id = "T123"
title = "Add endpoint"
preferred_model = "codex"
"#,
        )
        .expect("deserialize task spec");

        assert!(spec.depends_on.is_empty());
        assert_eq!(spec.preferred_model, Some(ModelKind::Codex));
    }

    #[test]
    fn model_kind_serializes_as_snake_case() {
        let json = serde_json::to_string(&ModelKind::Claude).unwrap();
        assert_eq!(json, "\"claude\"");
    }
}
