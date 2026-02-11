//! Daemon loop — structured tick function with the full autonomous pipeline.
//!
//! Replaces the inline loop in `main.rs` with a testable `daemon_tick()` that
//! returns actions for the caller to execute.

use chrono::{DateTime, Utc};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{EventId, ModelKind, Task, TaskId};

use crate::context_gen::{
    build_context_gen_prompt, context_is_current, poll_context_gen, should_regenerate,
    spawn_context_gen, ContextGenConfig, ContextGenState,
};
use crate::context_graph::{load_context_graph, ContextLoadConfig};
use crate::prompt_builder::{build_rich_prompt, PromptConfig, PromptRole, RetryContext};
use crate::retry::evaluate_retry;
use crate::stack_pipeline::{PipelineAction, PipelineState, next_action};
use crate::supervisor::{AgentOutcome, AgentSupervisor};
use crate::test_spec::load_test_spec;
use crate::OrchdService;

use std::collections::HashMap;
use std::path::PathBuf;

/// Configuration for the daemon loop.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Root path of the repository.
    pub repo_root: PathBuf,
    /// Path to prompt templates directory.
    pub template_dir: PathBuf,
    /// Models that are enabled for use.
    pub enabled_models: Vec<ModelKind>,
    /// Context loading configuration.
    pub context_config: ContextLoadConfig,
    /// Default verify command.
    pub verify_command: Option<String>,
    /// Context generation configuration.
    pub context_gen_config: ContextGenConfig,
}

/// Mutable state carried across daemon ticks.
pub struct DaemonState {
    /// Active pipeline states for tasks in the submit flow.
    pub pipelines: HashMap<String, PipelineState>,
    /// Context generation state.
    pub context_gen: ContextGenState,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            pipelines: HashMap::new(),
            context_gen: ContextGenState::new(),
        }
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}

/// Actions that the daemon loop produces for the caller to handle.
#[derive(Debug)]
pub enum DaemonAction {
    /// Spawn an agent for a chatting task.
    SpawnAgent {
        task_id: TaskId,
        model: ModelKind,
        prompt: String,
        worktree_path: PathBuf,
    },
    /// Mark a task as ready (agent completed successfully).
    MarkReady { task_id: TaskId },
    /// Record that a task needs human intervention.
    RecordNeedsHuman { task_id: TaskId, reason: String },
    /// Retry a failed task with a different model.
    ScheduleRetry {
        task_id: TaskId,
        next_model: ModelKind,
        reason: String,
    },
    /// Task has permanently failed.
    TaskFailed { task_id: TaskId, reason: String },
    /// Execute a pipeline action (verify, stack, submit).
    ExecutePipeline { action: PipelineAction },
    /// Trigger background context regeneration.
    TriggerContextRegen,
    /// Log a message.
    Log { message: String },
}

/// Run a single daemon tick — the core of the orchestration loop.
///
/// Returns a list of actions for the caller to execute. This keeps the daemon
/// logic testable (pure data in, actions out).
pub fn daemon_tick(
    service: &OrchdService,
    supervisor: &mut AgentSupervisor,
    daemon_state: &mut DaemonState,
    config: &DaemonConfig,
) -> Vec<DaemonAction> {
    let mut actions = Vec::new();
    let now = Utc::now();

    // --- Phase 1: Spawn agents for Chatting tasks without sessions ---
    if let Ok(chatting) = service.list_tasks_by_state(TaskState::Chatting) {
        for task in &chatting {
            if !supervisor.has_session(&task.id) {
                if let Some(action) = build_spawn_action(task, config) {
                    actions.push(action);
                }
            }
        }
    }

    // --- Phase 2: Poll supervisor for completed agents ---
    let poll_result = supervisor.poll();

    for chunk in &poll_result.output {
        for line in &chunk.lines {
            actions.push(DaemonAction::Log {
                message: format!("[{}] {}", chunk.task_id.0, line),
            });
        }
    }

    for outcome in &poll_result.completed {
        let outcome_actions =
            handle_agent_completion(service, outcome, config, now);
        actions.extend(outcome_actions);
    }

    // --- Phase 3: Drive pipelines for Ready tasks ---
    if let Ok(ready_tasks) = service.list_tasks_by_state(TaskState::Ready) {
        for task in &ready_tasks {
            if !daemon_state.pipelines.contains_key(&task.id.0) {
                // Start a new pipeline for this task.
                let parent_branch = find_parent_branch(service, task);
                let pipeline = PipelineState::new(
                    task.id.clone(),
                    task.branch_name
                        .clone()
                        .unwrap_or_else(|| format!("task/{}", task.id.0)),
                    task.worktree_path.clone(),
                    task.submit_mode,
                    parent_branch,
                );
                daemon_state.pipelines.insert(task.id.0.clone(), pipeline);
            }
        }
    }

    // Drive each pipeline forward.
    let pipeline_keys: Vec<String> = daemon_state.pipelines.keys().cloned().collect();
    for key in pipeline_keys {
        if let Some(pipeline) = daemon_state.pipelines.get(&key) {
            if !pipeline.is_terminal() {
                let action = next_action(pipeline);
                actions.push(DaemonAction::ExecutePipeline { action });
            }
        }
    }

    // Clean up terminal pipelines.
    daemon_state
        .pipelines
        .retain(|_, p| !p.is_terminal());

    // --- Phase 4: Context generation ---

    // Poll any running context gen process.
    if let Some(paths) = poll_context_gen(&config.repo_root, &mut daemon_state.context_gen) {
        actions.push(DaemonAction::Log {
            message: format!(
                "[context-gen] Updated {} context files",
                paths.len()
            ),
        });
    }

    // Check if we should trigger a regen based on transitions or stale hash.
    // Triggers: MarkReady actions (task completed), pipeline Complete actions (merged),
    // or git hash mismatch (cheap check — file read + git rev-parse).
    let has_trigger = actions.iter().any(|a| {
        matches!(a, DaemonAction::MarkReady { .. })
            || matches!(
                a,
                DaemonAction::ExecutePipeline {
                    action: PipelineAction::Complete { .. }
                }
            )
    });

    let is_stale = !context_is_current(&config.repo_root);

    if (has_trigger || is_stale)
        && should_regenerate(&daemon_state.context_gen, &config.context_gen_config, now)
    {
        actions.push(DaemonAction::TriggerContextRegen);
    }

    actions
}

/// Build the spawn action for a chatting task.
fn build_spawn_action(task: &Task, config: &DaemonConfig) -> Option<DaemonAction> {
    let model = task.preferred_model.unwrap_or(ModelKind::Claude);

    let context = load_context_graph(&config.repo_root, &config.context_config);

    let test_spec_content = task
        .test_spec_path
        .as_ref()
        .and_then(|_| load_test_spec(&config.repo_root, &task.id))
        .map(|spec| spec.raw);

    let retry = if task.retry_count > 0 {
        task.last_failure_reason.as_ref().map(|reason| RetryContext {
            attempt: task.retry_count + 1,
            max_retries: task.max_retries,
            previous_failure: reason.clone(),
            previous_model: *task.failed_models.last().unwrap_or(&model),
        })
    } else {
        None
    };

    let role = match task.task_type {
        orch_core::types::TaskType::Implement => PromptRole::Implement,
        orch_core::types::TaskType::TestSpecWrite => PromptRole::TestSpecWrite,
        orch_core::types::TaskType::TestValidate => PromptRole::Review,
        orch_core::types::TaskType::Orchestrate => PromptRole::Implement,
    };

    let prompt_config = PromptConfig {
        task_id: task.id.clone(),
        task_title: task.title.clone(),
        role,
        context,
        test_spec: test_spec_content,
        retry,
        verify_command: config.verify_command.clone(),
    };

    let prompt = build_rich_prompt(&prompt_config, &config.template_dir);

    Some(DaemonAction::SpawnAgent {
        task_id: task.id.clone(),
        model,
        prompt,
        worktree_path: task.worktree_path.clone(),
    })
}

/// Handle an agent completion — decide whether to mark ready, retry, or fail.
fn handle_agent_completion(
    service: &OrchdService,
    outcome: &AgentOutcome,
    config: &DaemonConfig,
    _now: DateTime<Utc>,
) -> Vec<DaemonAction> {
    let mut actions = Vec::new();

    if outcome.patch_ready || outcome.success {
        actions.push(DaemonAction::MarkReady {
            task_id: outcome.task_id.clone(),
        });
        return actions;
    }

    if outcome.needs_human {
        actions.push(DaemonAction::RecordNeedsHuman {
            task_id: outcome.task_id.clone(),
            reason: "Agent requested human assistance".to_string(),
        });
        return actions;
    }

    // Agent failed — evaluate retry.
    if let Ok(Some(task)) = service.task(&outcome.task_id) {
        let decision = evaluate_retry(&task, outcome, &config.enabled_models);

        if decision.should_retry {
            if let Some(next_model) = decision.next_model {
                actions.push(DaemonAction::ScheduleRetry {
                    task_id: outcome.task_id.clone(),
                    next_model,
                    reason: decision.reason,
                });
            } else {
                actions.push(DaemonAction::TaskFailed {
                    task_id: outcome.task_id.clone(),
                    reason: decision.reason,
                });
            }
        } else {
            actions.push(DaemonAction::TaskFailed {
                task_id: outcome.task_id.clone(),
                reason: decision.reason,
            });
        }
    } else {
        actions.push(DaemonAction::TaskFailed {
            task_id: outcome.task_id.clone(),
            reason: "task not found for retry evaluation".to_string(),
        });
    }

    actions
}

/// Look up the parent task's branch name for stacking.
fn find_parent_branch(service: &OrchdService, task: &Task) -> Option<String> {
    let parent_id = task.parent_task_id.as_ref()?;
    let parent = service.task(parent_id).ok()??;
    parent.branch_name
}

/// Execute the actions produced by a daemon tick.
///
/// This is the side-effectful counterpart to `daemon_tick()`. The daemon
/// main loop calls this to apply actions.
pub fn execute_actions(
    actions: &[DaemonAction],
    service: &OrchdService,
    supervisor: &mut AgentSupervisor,
    daemon_state: &mut DaemonState,
    config: &DaemonConfig,
) {
    let now = Utc::now();

    for action in actions {
        match action {
            DaemonAction::SpawnAgent {
                task_id,
                model: _,
                prompt,
                worktree_path,
            } => {
                if let Ok(Some(task)) = service.task(task_id) {
                    if let Err(e) = supervisor.spawn_agent(
                        task_id,
                        &task.repo_id,
                        worktree_path,
                        prompt,
                        task.preferred_model,
                    ) {
                        eprintln!("[daemon] Failed to spawn agent for {}: {}", task_id.0, e);
                    }
                }
            }
            DaemonAction::MarkReady { task_id } => {
                let event_id = EventId(format!("E-READY-{}", task_id.0));
                match service.mark_ready(task_id, event_id, now) {
                    Ok(_) => eprintln!("[daemon] {} -> Ready", task_id.0),
                    Err(e) => eprintln!("[daemon] Failed to mark {} ready: {}", task_id.0, e),
                }
            }
            DaemonAction::RecordNeedsHuman { task_id, reason } => {
                let event = Event {
                    id: EventId(format!("E-HUMAN-{}", task_id.0)),
                    task_id: Some(task_id.clone()),
                    repo_id: None,
                    at: now,
                    kind: EventKind::NeedsHuman {
                        reason: reason.clone(),
                    },
                };
                if let Err(e) = service.record_event(&event) {
                    eprintln!("[daemon] Failed to record needs_human for {}: {}", task_id.0, e);
                }
            }
            DaemonAction::ScheduleRetry {
                task_id,
                next_model,
                reason,
            } => {
                // Update the task for retry: bump retry_count, add failed model,
                // transition back to Chatting.
                if let Ok(Some(mut task)) = service.task(task_id) {
                    task.retry_count += 1;
                    if let Some(prev_model) = task.preferred_model {
                        if !task.failed_models.contains(&prev_model) {
                            task.failed_models.push(prev_model);
                        }
                    }
                    task.preferred_model = Some(*next_model);
                    task.last_failure_reason = Some(reason.clone());

                    // Persist updated task and transition to Chatting.
                    if let Err(e) = service.store.upsert_task(&task) {
                        eprintln!("[daemon] Failed to update task for retry {}: {}", task_id.0, e);
                        return;
                    }

                    let event = Event {
                        id: EventId(format!("E-RETRY-{}-{}", task_id.0, task.retry_count)),
                        task_id: Some(task_id.clone()),
                        repo_id: Some(task.repo_id.clone()),
                        at: now,
                        kind: EventKind::RetryScheduled {
                            attempt: task.retry_count,
                            model: next_model.as_str().to_string(),
                            reason: reason.clone(),
                        },
                    };
                    let _ = service.record_event(&event);
                    eprintln!(
                        "[daemon] {} retry #{} with {}",
                        task_id.0, task.retry_count, next_model.as_str()
                    );
                }
            }
            DaemonAction::TaskFailed { task_id, reason } => {
                let event = Event {
                    id: EventId(format!("E-FAILED-{}", task_id.0)),
                    task_id: Some(task_id.clone()),
                    repo_id: None,
                    at: now,
                    kind: EventKind::TaskFailed {
                        reason: reason.clone(),
                        is_final: true,
                    },
                };
                let _ = service.record_event(&event);
                eprintln!("[daemon] {} failed: {}", task_id.0, reason);
            }
            DaemonAction::ExecutePipeline { action } => {
                // Pipeline execution is handled by specific pipeline executors.
                // For now, log the action.
                match action {
                    PipelineAction::Complete { task_id } => {
                        eprintln!("[daemon] Pipeline complete for {}", task_id.0);
                    }
                    PipelineAction::Failed {
                        task_id,
                        stage,
                        error,
                    } => {
                        eprintln!(
                            "[daemon] Pipeline failed for {} at {}: {}",
                            task_id.0, stage, error
                        );
                    }
                    _ => {
                        eprintln!("[daemon] Pipeline action: {:?}", action);
                    }
                }
            }
            DaemonAction::TriggerContextRegen => {
                if should_regenerate(
                    &daemon_state.context_gen,
                    &config.context_gen_config,
                    Utc::now(),
                ) {
                    let prompt = build_context_gen_prompt(
                        &config.repo_root,
                        &config.template_dir,
                    );
                    if let Err(e) = spawn_context_gen(
                        &config.repo_root,
                        &prompt,
                        config.context_gen_config.model,
                        &mut daemon_state.context_gen,
                    ) {
                        eprintln!("[daemon] Failed to spawn context gen: {e}");
                    } else {
                        eprintln!("[daemon] Background context regeneration started");
                    }
                }
            }
            DaemonAction::Log { message } => {
                println!("{}", message);
            }
        }
    }
}

/// Convenience: run a single daemon tick and execute its actions.
pub fn run_tick(
    service: &OrchdService,
    supervisor: &mut AgentSupervisor,
    daemon_state: &mut DaemonState,
    config: &DaemonConfig,
) {
    let actions = daemon_tick(service, supervisor, daemon_state, config);
    execute_actions(&actions, service, supervisor, daemon_state, config);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::SqliteStore;
    use crate::event_log::JsonlEventLog;
    use crate::scheduler::{Scheduler, SchedulerConfig};
    use orch_core::events::{Event, EventKind};
    use orch_core::types::{EventId, ModelKind, RepoId, Task, TaskId};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;

    fn mk_service() -> OrchdService {
        let store = SqliteStore::open_in_memory().expect("in-memory db");
        let dir = std::env::temp_dir().join(format!(
            "othala-daemon-test-{}",
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

    fn mk_config() -> DaemonConfig {
        DaemonConfig {
            repo_root: PathBuf::from("/tmp/nonexistent-repo"),
            template_dir: PathBuf::from("/tmp/nonexistent-templates"),
            enabled_models: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
            context_config: ContextLoadConfig::default(),
            verify_command: Some("cargo test --workspace".to_string()),
            context_gen_config: ContextGenConfig::default(),
        }
    }

    fn mk_task(id: &str) -> Task {
        Task::new(
            TaskId::new(id),
            RepoId("repo".to_string()),
            format!("Task {}", id),
            PathBuf::from(format!(".orch/wt/{}", id)),
        )
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
    fn daemon_tick_produces_spawn_for_chatting_task() {
        let service = mk_service();
        let config = mk_config();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        let task = mk_task("T-1");
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);

        let spawn_count = actions
            .iter()
            .filter(|a| matches!(a, DaemonAction::SpawnAgent { .. }))
            .count();
        assert_eq!(spawn_count, 1);
    }

    #[test]
    fn daemon_tick_creates_pipeline_for_ready_task() {
        let service = mk_service();
        let config = mk_config();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        let mut task = mk_task("T-2");
        task.state = TaskState::Ready;
        task.branch_name = Some("task/T-2".to_string());
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);

        // Should have created a pipeline and produced a pipeline action.
        let pipeline_count = actions
            .iter()
            .filter(|a| matches!(a, DaemonAction::ExecutePipeline { .. }))
            .count();
        assert!(pipeline_count > 0);
    }

    #[test]
    fn handle_successful_outcome_produces_mark_ready() {
        let service = mk_service();
        let config = mk_config();

        let task = mk_task("T-3");
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        let outcome = AgentOutcome {
            task_id: TaskId::new("T-3"),
            model: ModelKind::Claude,
            exit_code: Some(0),
            patch_ready: true,
            needs_human: false,
            success: true,
        };

        let actions = handle_agent_completion(&service, &outcome, &config, Utc::now());
        assert!(actions
            .iter()
            .any(|a| matches!(a, DaemonAction::MarkReady { .. })));
    }

    #[test]
    fn handle_failed_outcome_evaluates_retry() {
        let service = mk_service();
        let config = mk_config();

        let mut task = mk_task("T-4");
        task.preferred_model = Some(ModelKind::Claude);
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        let outcome = AgentOutcome {
            task_id: TaskId::new("T-4"),
            model: ModelKind::Claude,
            exit_code: Some(1),
            patch_ready: false,
            needs_human: false,
            success: false,
        };

        let actions = handle_agent_completion(&service, &outcome, &config, Utc::now());
        // Should produce a retry or failure action.
        assert!(actions.iter().any(|a| matches!(
            a,
            DaemonAction::ScheduleRetry { .. } | DaemonAction::TaskFailed { .. }
        )));
    }

    #[test]
    fn daemon_state_default() {
        let state = DaemonState::default();
        assert!(state.pipelines.is_empty());
    }
}
