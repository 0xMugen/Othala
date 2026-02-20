//! Smart Context Manager — manages rich context passing to agents.
//!
//! Provides:
//! - Current focus and objectives
//! - Blockers and assumptions
//! - Task lineage (parent tasks, prior attempts)
//! - Error patterns from prior failures
//! - Repo-specific knowledge

use chrono::{DateTime, Utc};
use orch_core::state::TaskState;
use orch_core::types::{ModelKind, Task};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

// ─────────────────────────────────────────────────────────────────────────────
// Context State — Persisted context for the repo
// ─────────────────────────────────────────────────────────────────────────────

/// Persisted context state for a repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextState {
    /// Current focus/objective
    pub current_focus: Option<String>,
    /// Known blockers
    pub blockers: Vec<Blocker>,
    /// Working assumptions
    pub assumptions: Vec<String>,
    /// Recent decisions
    pub decisions: Vec<Decision>,
    /// Error patterns observed
    pub error_patterns: Vec<ErrorPattern>,
    /// Last updated
    pub updated_at: Option<DateTime<Utc>>,
}

/// A blocker that's preventing progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blocker {
    pub id: String,
    pub description: String,
    pub severity: BlockerSeverity,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// A decision made during task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub description: String,
    pub reasoning: String,
    pub task_id: Option<String>,
    pub made_at: DateTime<Utc>,
}

/// An error pattern observed across tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPattern {
    pub pattern: String,
    pub count: u32,
    pub last_seen: DateTime<Utc>,
    pub resolution: Option<String>,
}

impl ContextState {
    /// Load context state from file.
    pub fn load(path: &Path) -> Option<Self> {
        let contents = fs::read_to_string(path).ok()?;
        serde_json::from_str(&contents).ok()
    }

    /// Save context state to file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let contents = serde_json::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)
    }

    /// Get active (unresolved) blockers.
    pub fn active_blockers(&self) -> Vec<&Blocker> {
        self.blockers
            .iter()
            .filter(|b| b.resolved_at.is_none())
            .collect()
    }

    /// Record a new error pattern.
    pub fn record_error(&mut self, pattern: &str) {
        let now = Utc::now();
        if let Some(existing) = self.error_patterns.iter_mut().find(|p| p.pattern == pattern) {
            existing.count += 1;
            existing.last_seen = now;
        } else {
            self.error_patterns.push(ErrorPattern {
                pattern: pattern.to_string(),
                count: 1,
                last_seen: now,
                resolution: None,
            });
        }
        self.updated_at = Some(now);
    }

    /// Record an error resolution.
    pub fn record_resolution(&mut self, pattern: &str, resolution: &str) {
        if let Some(p) = self.error_patterns.iter_mut().find(|p| p.pattern == pattern) {
            p.resolution = Some(resolution.to_string());
        }
        self.updated_at = Some(Utc::now());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Task Lineage — History of task attempts
// ─────────────────────────────────────────────────────────────────────────────

/// History of a task's prior attempts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskLineage {
    /// Task ID
    pub task_id: String,
    /// Prior attempts with results
    pub attempts: Vec<AttemptRecord>,
    /// Parent task (if decomposed)
    pub parent_task: Option<String>,
    /// Child tasks (if orchestrated)
    pub child_tasks: Vec<String>,
}

/// Record of a single task attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub attempt_number: u32,
    pub model: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub outcome: AttemptOutcome,
    pub failure_reason: Option<String>,
    pub changes_made: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Success,
    Failed,
    NeedsHuman,
    Timeout,
    InProgress,
}

// ─────────────────────────────────────────────────────────────────────────────
// Rich Context — What we pass to agents
// ─────────────────────────────────────────────────────────────────────────────

/// Rich context for agent prompts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RichContext {
    /// Task-specific context
    pub task: TaskContext,
    /// Repository context
    pub repo: RepoContext,
    /// Prior attempts context
    pub history: HistoryContext,
    /// Error recovery context (if retrying)
    pub recovery: Option<RecoveryContext>,
}

/// Task-specific context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskContext {
    pub task_id: String,
    pub title: String,
    pub current_focus: Option<String>,
    pub blockers: Vec<String>,
    pub assumptions: Vec<String>,
    pub related_tasks: Vec<String>,
}

/// Repository context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoContext {
    pub repo_id: String,
    pub primary_language: Option<String>,
    pub recent_decisions: Vec<String>,
    pub active_branches: Vec<String>,
}

/// History context from prior attempts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistoryContext {
    pub attempt_number: u32,
    pub max_attempts: u32,
    pub prior_models: Vec<String>,
    pub prior_failures: Vec<String>,
    pub changes_so_far: Vec<String>,
}

/// Error recovery context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecoveryContext {
    pub error_class: String,
    pub error_message: String,
    pub matched_pattern: Option<String>,
    pub suggested_approach: String,
    pub similar_past_errors: Vec<PastError>,
}

/// A past error and how it was resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PastError {
    pub pattern: String,
    pub resolution: String,
    pub success: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Context Manager
// ─────────────────────────────────────────────────────────────────────────────

/// The Context Manager — builds and maintains rich context.
pub struct ContextManager {
    /// Path to context storage
    pub context_dir: PathBuf,
    /// Cached context states per repo
    pub states: HashMap<String, ContextState>,
    /// Task lineages
    pub lineages: HashMap<String, TaskLineage>,
}

impl ContextManager {
    pub fn new(context_dir: PathBuf) -> Self {
        Self {
            context_dir,
            states: HashMap::new(),
            lineages: HashMap::new(),
        }
    }

    /// Get the context state for a repository.
    pub fn get_state(&mut self, repo_id: &str) -> &mut ContextState {
        if !self.states.contains_key(repo_id) {
            let path = self.context_dir.join(format!("{}/context.json", repo_id));
            let state = ContextState::load(&path).unwrap_or_default();
            self.states.insert(repo_id.to_string(), state);
        }
        self.states.get_mut(repo_id).unwrap()
    }

    /// Save all context states.
    pub fn save_all(&self) -> std::io::Result<()> {
        for (repo_id, state) in &self.states {
            let path = self.context_dir.join(format!("{}/context.json", repo_id));
            state.save(&path)?;
        }
        Ok(())
    }

    /// Get task lineage.
    pub fn get_lineage(&mut self, task_id: &str) -> &mut TaskLineage {
        if !self.lineages.contains_key(task_id) {
            let path = self
                .context_dir
                .join(format!("lineages/{}.json", task_id));
            let lineage = if path.exists() {
                fs::read_to_string(&path)
                    .ok()
                    .and_then(|c| serde_json::from_str(&c).ok())
                    .unwrap_or_else(|| TaskLineage {
                        task_id: task_id.to_string(),
                        ..Default::default()
                    })
            } else {
                TaskLineage {
                    task_id: task_id.to_string(),
                    ..Default::default()
                }
            };
            self.lineages.insert(task_id.to_string(), lineage);
        }
        self.lineages.get_mut(task_id).unwrap()
    }

    /// Record a task attempt.
    pub fn record_attempt(
        &mut self,
        task_id: &str,
        model: ModelKind,
        outcome: AttemptOutcome,
        failure_reason: Option<&str>,
    ) {
        let lineage = self.get_lineage(task_id);
        let attempt_number = lineage.attempts.len() as u32 + 1;
        let now = Utc::now();

        lineage.attempts.push(AttemptRecord {
            attempt_number,
            model: model.as_str().to_string(),
            started_at: now,
            ended_at: Some(now),
            outcome,
            failure_reason: failure_reason.map(|s| s.to_string()),
            changes_made: vec![],
        });
    }

    /// Build rich context for a task.
    pub fn build_context(
        &mut self,
        task: &Task,
        tasks: &[Task],
        failure_reason: Option<&str>,
    ) -> RichContext {
        // Get state data first
        let state = self.get_state(&task.repo_id.0);
        let current_focus = state.current_focus.clone();
        let blockers: Vec<String> = state
            .active_blockers()
            .iter()
            .map(|b| b.description.clone())
            .collect();
        let assumptions = state.assumptions.clone();
        let recent_decisions: Vec<String> = state
            .decisions
            .iter()
            .rev()
            .take(5)
            .map(|d| d.description.clone())
            .collect();
        let similar_past_errors: Vec<PastError> = if let Some(reason) = failure_reason {
            state
                .error_patterns
                .iter()
                .filter(|p| {
                    reason
                        .to_ascii_lowercase()
                        .contains(&p.pattern.to_ascii_lowercase())
                })
                .filter_map(|p| {
                    p.resolution.as_ref().map(|r| PastError {
                        pattern: p.pattern.clone(),
                        resolution: r.clone(),
                        success: true,
                    })
                })
                .collect()
        } else {
            vec![]
        };

        // Get lineage data
        let lineage = self.get_lineage(&task.id.0);
        let prior_models: Vec<String> = lineage
            .attempts
            .iter()
            .map(|a| a.model.clone())
            .collect();
        let prior_failures: Vec<String> = lineage
            .attempts
            .iter()
            .filter_map(|a| a.failure_reason.clone())
            .collect();
        let changes_so_far: Vec<String> = lineage
            .attempts
            .iter()
            .flat_map(|a| a.changes_made.clone())
            .collect();

        let mut ctx = RichContext::default();

        // Task context
        ctx.task = TaskContext {
            task_id: task.id.0.clone(),
            title: task.title.clone(),
            current_focus,
            blockers,
            assumptions,
            related_tasks: task.depends_on.iter().map(|id| id.0.clone()).collect(),
        };

        // Repo context
        ctx.repo = RepoContext {
            repo_id: task.repo_id.0.clone(),
            primary_language: None, // TODO: detect from repo
            recent_decisions,
            active_branches: tasks
                .iter()
                .filter(|t| t.repo_id == task.repo_id && t.state != TaskState::Merged)
                .filter_map(|t| t.branch_name.clone())
                .collect(),
        };

        // History context
        ctx.history = HistoryContext {
            attempt_number: task.retry_count + 1,
            max_attempts: task.max_retries,
            prior_models,
            prior_failures,
            changes_so_far,
        };

        // Recovery context (if this is a retry)
        if let Some(reason) = failure_reason {
            let mut recovery = RecoveryContext::default();
            recovery.error_message = reason.to_string();
            recovery.similar_past_errors = similar_past_errors;
            ctx.recovery = Some(recovery);
        }

        ctx
    }

    /// Render context as markdown for prompt injection.
    pub fn render_context(&self, ctx: &RichContext) -> String {
        let mut output = String::new();

        output.push_str("## Task Context\n\n");
        output.push_str(&format!("**Task:** {} - {}\n", ctx.task.task_id, ctx.task.title));

        if let Some(focus) = &ctx.task.current_focus {
            output.push_str(&format!("**Current Focus:** {}\n", focus));
        }

        if !ctx.task.blockers.is_empty() {
            output.push_str("\n**Known Blockers:**\n");
            for blocker in &ctx.task.blockers {
                output.push_str(&format!("- {}\n", blocker));
            }
        }

        if !ctx.task.assumptions.is_empty() {
            output.push_str("\n**Working Assumptions:**\n");
            for assumption in &ctx.task.assumptions {
                output.push_str(&format!("- {}\n", assumption));
            }
        }

        // History section
        if ctx.history.attempt_number > 1 {
            output.push_str(&format!(
                "\n## Prior Attempts\n\n**Attempt:** {}/{}\n",
                ctx.history.attempt_number, ctx.history.max_attempts
            ));

            if !ctx.history.prior_models.is_empty() {
                output.push_str(&format!(
                    "**Models tried:** {}\n",
                    ctx.history.prior_models.join(", ")
                ));
            }

            if !ctx.history.prior_failures.is_empty() {
                output.push_str("\n**What went wrong:**\n");
                for failure in &ctx.history.prior_failures {
                    output.push_str(&format!("- {}\n", truncate(failure, 200)));
                }
            }
        }

        // Recovery section
        if let Some(recovery) = &ctx.recovery {
            output.push_str("\n## Error Recovery Context\n\n");
            output.push_str(&format!("**Error Class:** {}\n", recovery.error_class));
            output.push_str(&format!(
                "**Error Message:**\n```\n{}\n```\n",
                truncate(&recovery.error_message, 500)
            ));

            if !recovery.suggested_approach.is_empty() {
                output.push_str(&format!(
                    "\n**Suggested Approach:**\n{}\n",
                    recovery.suggested_approach
                ));
            }

            if !recovery.similar_past_errors.is_empty() {
                output.push_str("\n**Similar Past Errors & Resolutions:**\n");
                for past in &recovery.similar_past_errors {
                    output.push_str(&format!(
                        "- Pattern `{}` was resolved by: {}\n",
                        past.pattern, past.resolution
                    ));
                }
            }
        }

        // Recent decisions
        if !ctx.repo.recent_decisions.is_empty() {
            output.push_str("\n## Recent Decisions\n\n");
            for decision in &ctx.repo.recent_decisions {
                output.push_str(&format!("- {}\n", decision));
            }
        }

        output
    }
}

fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..max_len]
    }
}

impl Default for ContextManager {
    fn default() -> Self {
        Self::new(PathBuf::from(".othala/context"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::types::{RepoId, TaskId};

    fn make_task(id: &str, title: &str) -> Task {
        Task::new(
            TaskId::new(id),
            RepoId("test-repo".to_string()),
            title.to_string(),
            PathBuf::from(format!(".orch/wt/{}", id)),
        )
    }

    #[test]
    fn context_state_records_errors() {
        let mut state = ContextState::default();

        state.record_error("mismatched types");
        state.record_error("mismatched types");
        state.record_error("timeout");

        assert_eq!(state.error_patterns.len(), 2);
        assert_eq!(
            state
                .error_patterns
                .iter()
                .find(|p| p.pattern == "mismatched types")
                .unwrap()
                .count,
            2
        );
    }

    #[test]
    fn context_state_tracks_resolutions() {
        let mut state = ContextState::default();

        state.record_error("compile error");
        state.record_resolution("compile error", "Fixed the type annotation");

        let pattern = state
            .error_patterns
            .iter()
            .find(|p| p.pattern == "compile error")
            .unwrap();
        assert_eq!(
            pattern.resolution.as_deref(),
            Some("Fixed the type annotation")
        );
    }

    #[test]
    fn build_context_includes_history() {
        let mut manager = ContextManager::new(PathBuf::from("/tmp/test-context"));

        // Record some attempts
        manager.record_attempt("T1", ModelKind::Claude, AttemptOutcome::Failed, Some("compile error"));
        manager.record_attempt(
            "T1",
            ModelKind::Codex,
            AttemptOutcome::Failed,
            Some("test failure"),
        );

        let mut task = make_task("T1", "Fix the bug");
        task.retry_count = 2;

        let ctx = manager.build_context(&task, &[], Some("test failure"));

        assert_eq!(ctx.history.attempt_number, 3);
        assert_eq!(ctx.history.prior_models.len(), 2);
        assert_eq!(ctx.history.prior_failures.len(), 2);
        assert!(ctx.recovery.is_some());
    }

    #[test]
    fn render_context_produces_markdown() {
        let ctx = RichContext {
            task: TaskContext {
                task_id: "T1".to_string(),
                title: "Fix the bug".to_string(),
                current_focus: Some("Type errors".to_string()),
                blockers: vec!["Database connection".to_string()],
                assumptions: vec!["Using Postgres".to_string()],
                related_tasks: vec![],
            },
            history: HistoryContext {
                attempt_number: 2,
                max_attempts: 3,
                prior_models: vec!["claude".to_string()],
                prior_failures: vec!["compile error".to_string()],
                changes_so_far: vec![],
            },
            recovery: Some(RecoveryContext {
                error_class: "compile".to_string(),
                error_message: "mismatched types".to_string(),
                matched_pattern: None,
                suggested_approach: "Check the return type".to_string(),
                similar_past_errors: vec![],
            }),
            repo: RepoContext::default(),
        };

        let manager = ContextManager::default();
        let rendered = manager.render_context(&ctx);

        assert!(rendered.contains("## Task Context"));
        assert!(rendered.contains("Fix the bug"));
        assert!(rendered.contains("Type errors"));
        assert!(rendered.contains("Prior Attempts"));
        assert!(rendered.contains("Error Recovery Context"));
    }

    #[test]
    fn task_lineage_tracks_attempts() {
        let mut lineage = TaskLineage {
            task_id: "T1".to_string(),
            ..Default::default()
        };

        lineage.attempts.push(AttemptRecord {
            attempt_number: 1,
            model: "claude".to_string(),
            started_at: Utc::now(),
            ended_at: Some(Utc::now()),
            outcome: AttemptOutcome::Failed,
            failure_reason: Some("compile error".to_string()),
            changes_made: vec!["src/lib.rs".to_string()],
        });

        assert_eq!(lineage.attempts.len(), 1);
        assert_eq!(lineage.attempts[0].outcome, AttemptOutcome::Failed);
    }

    #[test]
    fn active_blockers_filters_resolved() {
        let mut state = ContextState::default();

        state.blockers.push(Blocker {
            id: "B1".to_string(),
            description: "Active blocker".to_string(),
            severity: BlockerSeverity::High,
            created_at: Utc::now(),
            resolved_at: None,
        });

        state.blockers.push(Blocker {
            id: "B2".to_string(),
            description: "Resolved blocker".to_string(),
            severity: BlockerSeverity::Low,
            created_at: Utc::now(),
            resolved_at: Some(Utc::now()),
        });

        let active = state.active_blockers();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].description, "Active blocker");
    }
}
