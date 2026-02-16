//! Core types for the MVP orchestrator.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::state::{TaskState, VerifyStatus};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

impl TaskPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskPriority::Low => "low",
            TaskPriority::Normal => "normal",
            TaskPriority::High => "high",
            TaskPriority::Critical => "critical",
        }
    }
}

impl std::str::FromStr for TaskPriority {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_lowercase().as_str() {
            "low" => Ok(TaskPriority::Low),
            "normal" => Ok(TaskPriority::Normal),
            "high" => Ok(TaskPriority::High),
            "critical" => Ok(TaskPriority::Critical),
            other => Err(format!(
                "invalid task priority '{other}'. valid values: low, normal, high, critical"
            )),
        }
    }
}

impl std::fmt::Display for TaskPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Active,
    Completed,
    Archived,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionStatus::Active => "active",
            SessionStatus::Completed => "completed",
            SessionStatus::Archived => "archived",
        }
    }
}

impl std::str::FromStr for SessionStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_lowercase().as_str() {
            "active" => Ok(SessionStatus::Active),
            "completed" => Ok(SessionStatus::Completed),
            "archived" => Ok(SessionStatus::Archived),
            other => Err(format!(
                "invalid session status '{other}'. valid values: active, completed, archived"
            )),
        }
    }
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YamlTaskSpec {
    pub title: String,
    pub model: Option<String>,
    pub priority: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub labels: Option<Vec<String>>,
    pub verify_command: Option<String>,
    pub context_files: Option<Vec<String>>,
}

fn parse_yaml_scalar(value: &str) -> String {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn parse_yaml_task_spec(content: &str) -> Result<YamlTaskSpec, String> {
    let mut title: Option<String> = None;
    let mut model: Option<String> = None;
    let mut priority: Option<String> = None;
    let mut depends_on: Vec<String> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let mut verify_command: Option<String> = None;
    let mut context_files: Vec<String> = Vec::new();

    let mut current_list_key: Option<&str> = None;
    let mut depends_seen = false;
    let mut labels_seen = false;
    let mut context_files_seen = false;

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(value) = line.strip_prefix("- ") {
            let key = current_list_key
                .ok_or_else(|| format!("line {line_no}: list item found without list key"))?;
            let value = parse_yaml_scalar(value);
            if value.is_empty() {
                return Err(format!("line {line_no}: list item cannot be empty"));
            }

            match key {
                "depends_on" => depends_on.push(value),
                "labels" => labels.push(value),
                "context_files" => context_files.push(value),
                _ => return Err(format!("line {line_no}: unsupported list key '{key}'")),
            }
            continue;
        }

        let Some((raw_key, raw_value)) = line.split_once(':') else {
            return Err(format!("line {line_no}: expected 'key: value'"));
        };

        let key = raw_key.trim();
        let value = raw_value.trim();
        current_list_key = None;

        match key {
            "title" => {
                if title.is_some() {
                    return Err(format!("line {line_no}: duplicate key 'title'"));
                }
                let parsed = parse_yaml_scalar(value);
                if parsed.is_empty() {
                    return Err(format!("line {line_no}: title cannot be empty"));
                }
                title = Some(parsed);
            }
            "model" => {
                if model.is_some() {
                    return Err(format!("line {line_no}: duplicate key 'model'"));
                }
                let parsed = parse_yaml_scalar(value);
                if !parsed.is_empty() {
                    model = Some(parsed);
                }
            }
            "priority" => {
                if priority.is_some() {
                    return Err(format!("line {line_no}: duplicate key 'priority'"));
                }
                let parsed = parse_yaml_scalar(value);
                if !parsed.is_empty() {
                    priority = Some(parsed);
                }
            }
            "verify_command" => {
                if verify_command.is_some() {
                    return Err(format!("line {line_no}: duplicate key 'verify_command'"));
                }
                let parsed = parse_yaml_scalar(value);
                if !parsed.is_empty() {
                    verify_command = Some(parsed);
                }
            }
            "depends_on" => {
                if depends_seen {
                    return Err(format!("line {line_no}: duplicate key 'depends_on'"));
                }
                depends_seen = true;
                if value.is_empty() {
                    current_list_key = Some("depends_on");
                } else {
                    depends_on.push(parse_yaml_scalar(value));
                }
            }
            "labels" => {
                if labels_seen {
                    return Err(format!("line {line_no}: duplicate key 'labels'"));
                }
                labels_seen = true;
                if value.is_empty() {
                    current_list_key = Some("labels");
                } else {
                    labels.push(parse_yaml_scalar(value));
                }
            }
            "context_files" => {
                if context_files_seen {
                    return Err(format!("line {line_no}: duplicate key 'context_files'"));
                }
                context_files_seen = true;
                if value.is_empty() {
                    current_list_key = Some("context_files");
                } else {
                    context_files.push(parse_yaml_scalar(value));
                }
            }
            other => return Err(format!("line {line_no}: unknown key '{other}'")),
        }
    }

    let title = title.ok_or_else(|| "missing required key 'title'".to_string())?;
    Ok(YamlTaskSpec {
        title,
        model,
        priority,
        depends_on: if depends_seen { Some(depends_on) } else { None },
        labels: if labels_seen { Some(labels) } else { None },
        verify_command,
        context_files: if context_files_seen {
            Some(context_files)
        } else {
            None
        },
    })
}

pub fn load_task_specs_from_dir(dir: &std::path::Path) -> Vec<YamlTaskSpec> {
    let mut specs = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return specs;
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|value| value.path()))
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();

    for path in files {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(spec) = parse_yaml_task_spec(&content) {
            specs.push(spec);
        }
    }

    specs
}

pub fn yaml_spec_to_task(spec: &YamlTaskSpec, repo_id: &str) -> Task {
    let task_id = TaskId::new(format!(
        "chat-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let worktree_path = PathBuf::from(format!(".orch/wt/{}", task_id.0));
    let mut task = Task::new(
        task_id,
        RepoId(repo_id.to_string()),
        spec.title.clone(),
        worktree_path,
    );

    task.preferred_model = spec.model.as_deref().and_then(|name| match name.trim().to_lowercase().as_str() {
        "claude" => Some(ModelKind::Claude),
        "codex" => Some(ModelKind::Codex),
        "gemini" => Some(ModelKind::Gemini),
        _ => None,
    });

    task.priority = spec
        .priority
        .as_deref()
        .and_then(|value| value.parse::<TaskPriority>().ok())
        .unwrap_or_default();

    task.depends_on = spec
        .depends_on
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(TaskId::new)
        .collect();

    task.labels = spec.labels.clone().unwrap_or_default();
    task
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub task_ids: Vec<TaskId>,
    pub parent_session_id: Option<String>,
    pub status: SessionStatus,
}

/// A task (AI coding session) - simplified for MVP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub repo_id: RepoId,
    pub title: String,
    pub state: TaskState,
    pub preferred_model: Option<ModelKind>,
    #[serde(default)]
    pub priority: TaskPriority,
    pub depends_on: Vec<TaskId>,
    pub submit_mode: SubmitMode,
    #[serde(default)]
    pub labels: Vec<String>,
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
            priority: TaskPriority::default(),
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            labels: Vec::new(),
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

    #[test]
    fn task_priority_defaults_to_normal() {
        let task = Task::new(
            TaskId::new("T1"),
            RepoId("repo".to_string()),
            "Test".to_string(),
            PathBuf::from(".orch/wt/T1"),
        );
        assert_eq!(task.priority, TaskPriority::Normal);
    }

    #[test]
    fn task_priority_orders_critical_first_when_sorted_desc() {
        let mut values = [
            TaskPriority::Low,
            TaskPriority::Critical,
            TaskPriority::Normal,
            TaskPriority::High,
        ];
        values.sort_by(|a, b| b.cmp(a));
        assert_eq!(
            values,
            [
                TaskPriority::Critical,
                TaskPriority::High,
                TaskPriority::Normal,
                TaskPriority::Low
            ]
        );
    }

    #[test]
    fn parse_yaml_task_spec_parses_scalars_and_lists() {
        let spec = parse_yaml_task_spec(
            r#"
title: Add authentication middleware
model: claude
priority: high
depends_on:
  - T-001
labels:
  - auth
  - security
verify_command: cargo test -p auth
context_files:
  - src/auth.rs
"#,
        )
        .expect("parse yaml spec");

        assert_eq!(spec.title, "Add authentication middleware");
        assert_eq!(spec.model.as_deref(), Some("claude"));
        assert_eq!(spec.priority.as_deref(), Some("high"));
        assert_eq!(spec.depends_on, Some(vec!["T-001".to_string()]));
        assert_eq!(
            spec.labels,
            Some(vec!["auth".to_string(), "security".to_string()])
        );
        assert_eq!(
            spec.verify_command.as_deref(),
            Some("cargo test -p auth")
        );
        assert_eq!(
            spec.context_files,
            Some(vec!["src/auth.rs".to_string()])
        );
    }

    #[test]
    fn parse_yaml_task_spec_requires_title() {
        let err = parse_yaml_task_spec("model: codex").expect_err("missing title should fail");
        assert!(err.contains("missing required key 'title'"));
    }

    #[test]
    fn parse_yaml_task_spec_rejects_unknown_key() {
        let err = parse_yaml_task_spec(
            r#"
title: Hello
unknown: nope
"#,
        )
        .expect_err("unknown key should fail");
        assert!(err.contains("unknown key"));
    }

    #[test]
    fn load_task_specs_from_dir_reads_only_valid_yaml() {
        let root = std::env::temp_dir().join(format!(
            "othala-yaml-load-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("create test dir");

        std::fs::write(root.join("a.yaml"), "title: A\nmodel: codex\n").expect("write a");
        std::fs::write(root.join("b.yml"), "title: B\n").expect("write b");
        std::fs::write(root.join("c.yaml"), "model: missing-title\n").expect("write c");
        std::fs::write(root.join("ignore.txt"), "title: ignored\n").expect("write ignore");

        let specs = load_task_specs_from_dir(&root);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].title, "A");
        assert_eq!(specs[1].title, "B");

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn yaml_spec_to_task_maps_fields() {
        let spec = YamlTaskSpec {
            title: "Do thing".to_string(),
            model: Some("gemini".to_string()),
            priority: Some("critical".to_string()),
            depends_on: Some(vec!["T-1".to_string(), "T-2".to_string()]),
            labels: Some(vec!["backend".to_string()]),
            verify_command: Some("cargo test".to_string()),
            context_files: Some(vec!["src/lib.rs".to_string()]),
        };

        let task = yaml_spec_to_task(&spec, "repo-xyz");
        assert_eq!(task.repo_id.0, "repo-xyz");
        assert_eq!(task.title, "Do thing");
        assert_eq!(task.preferred_model, Some(ModelKind::Gemini));
        assert_eq!(task.priority, TaskPriority::Critical);
        assert_eq!(
            task.depends_on,
            vec![TaskId::new("T-1"), TaskId::new("T-2")]
        );
        assert_eq!(task.labels, vec!["backend".to_string()]);
    }

    #[test]
    fn yaml_spec_to_task_defaults_on_invalid_model_and_priority() {
        let spec = YamlTaskSpec {
            title: "Fallback".to_string(),
            model: Some("unknown".to_string()),
            priority: Some("urgent".to_string()),
            depends_on: None,
            labels: None,
            verify_command: None,
            context_files: None,
        };

        let task = yaml_spec_to_task(&spec, "repo");
        assert_eq!(task.preferred_model, None);
        assert_eq!(task.priority, TaskPriority::Normal);
        assert!(task.depends_on.is_empty());
        assert!(task.labels.is_empty());
    }
}
