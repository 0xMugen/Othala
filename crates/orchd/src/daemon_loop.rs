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
use crate::qa_agent::{
    build_qa_failure_context, build_qa_prompt, load_baseline, load_latest_result,
    load_task_spec as load_qa_task_spec, poll_qa_agent, save_qa_result, spawn_qa_agent,
    QAResult, QAState, QAStatus, QAType,
};
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
    /// Per-task QA agent state (keyed by task_id).
    pub qa_agents: HashMap<String, QAState>,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            pipelines: HashMap::new(),
            context_gen: ContextGenState::new(),
            qa_agents: HashMap::new(),
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
    /// Spawn a QA agent (baseline or validation).
    SpawnQA {
        task_id: TaskId,
        qa_type: QAType,
    },
    /// QA run completed successfully — all tests passed.
    QACompleted {
        task_id: TaskId,
        result: QAResult,
    },
    /// QA run found failures.
    QAFailed {
        task_id: TaskId,
        result: QAResult,
    },
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

    // --- Phase 1.5: Check if baseline QA exists for chatting tasks ---
    //
    // For tasks about to be spawned, check if a baseline QA result exists for
    // the task's branch. If no baseline exists and we have a QA spec, spawn a
    // baseline QA agent first.
    if let Ok(chatting) = service.list_tasks_by_state(TaskState::Chatting) {
        for task in &chatting {
            let default_branch = format!("task/{}", task.id.0);
            let branch = task
                .branch_name
                .as_deref()
                .unwrap_or(&default_branch);

            // Skip if QA agent already running for this task.
            if daemon_state.qa_agents.contains_key(&task.id.0) {
                continue;
            }

            // Skip if no baseline spec exists.
            if load_baseline(&config.repo_root).is_none() {
                continue;
            }

            // Skip if we already have a baseline result for this branch.
            if load_latest_result(&config.repo_root, branch).is_some() {
                continue;
            }

            actions.push(DaemonAction::SpawnQA {
                task_id: task.id.clone(),
                qa_type: QAType::Baseline,
            });
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

    // --- Phase 2.5: Poll QA agents ---
    //
    // Check running QA agents for completion. On completion:
    // - Baseline run → store result, allow implementation to proceed
    // - Validation run → check regression + acceptance
    //   - All pass → MarkReady
    //   - Fail → ScheduleRetry with QA failure details
    let qa_keys: Vec<String> = daemon_state.qa_agents.keys().cloned().collect();
    for key in qa_keys {
        if let Some(qa_state) = daemon_state.qa_agents.get_mut(&key) {
            // Only poll if there is an actual child process to check.
            if qa_state.child_handle.is_none() {
                continue;
            }
            if let Some(result) = poll_qa_agent(qa_state) {
                let task_id = TaskId::new(&key);
                let all_passed = result.summary.failed == 0;
                let qa_type = qa_state.qa_type;

                if all_passed {
                    actions.push(DaemonAction::QACompleted {
                        task_id: task_id.clone(),
                        result: result.clone(),
                    });

                    if qa_type == QAType::Validation {
                        // Validation passed — mark ready.
                        actions.push(DaemonAction::MarkReady { task_id });
                    }
                } else {
                    actions.push(DaemonAction::QAFailed {
                        task_id: task_id.clone(),
                        result: result.clone(),
                    });

                    if qa_type == QAType::Validation {
                        // Validation failed — retry implementation with QA context.
                        // Re-use the task's current preferred model — QA failure
                        // means the code needs fixing, not that the model is broken.
                        let failure_ctx = build_qa_failure_context(&result);
                        let retry_model = service
                            .task(&task_id)
                            .ok()
                            .flatten()
                            .and_then(|t| t.preferred_model)
                            .unwrap_or(ModelKind::Claude);
                        actions.push(DaemonAction::ScheduleRetry {
                            task_id,
                            next_model: retry_model,
                            reason: failure_ctx,
                        });
                    }
                }
            }

            // Clean up completed/failed QA states.
            if qa_state.status == QAStatus::Completed || qa_state.status == QAStatus::Failed {
                // Will be removed below.
            }
        }
    }

    // Remove finished QA agents.
    daemon_state
        .qa_agents
        .retain(|_, s| s.status != QAStatus::Completed && s.status != QAStatus::Failed);

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
        qa_failure_context: None,
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
        // If a QA baseline spec exists, spawn a validation QA run instead of
        // immediately marking ready. The QA Phase 2.5 will mark ready on pass.
        if load_baseline(&config.repo_root).is_some() {
            actions.push(DaemonAction::SpawnQA {
                task_id: outcome.task_id.clone(),
                qa_type: QAType::Validation,
            });
        } else {
            actions.push(DaemonAction::MarkReady {
                task_id: outcome.task_id.clone(),
            });
        }
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

                // Transition the task to Stopped so it's no longer active.
                if let Ok(Some(mut task)) = service.task(task_id) {
                    task.state = TaskState::Stopped;
                    let _ = service.store.upsert_task(&task);
                }

                eprintln!("[daemon] {} failed → stopped: {}", task_id.0, reason);
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
            DaemonAction::SpawnQA { task_id, qa_type } => {
                // Load baseline spec and build QA prompt.
                if let Some(baseline) = load_baseline(&config.repo_root) {
                    let branch = format!("task/{}", task_id.0);
                    let task_spec = load_qa_task_spec(&config.repo_root, task_id);
                    let previous = load_latest_result(&config.repo_root, &branch);

                    // Determine cwd: baseline runs from repo root,
                    // validation runs from the task's worktree.
                    let cwd = if *qa_type == QAType::Validation {
                        service
                            .task(task_id)
                            .ok()
                            .flatten()
                            .map(|t| t.worktree_path.clone())
                            .unwrap_or_else(|| config.repo_root.clone())
                    } else {
                        config.repo_root.clone()
                    };

                    let prompt = build_qa_prompt(
                        &baseline,
                        task_spec.as_deref(),
                        previous.as_ref(),
                        &cwd,
                        &config.template_dir,
                    );

                    let model = config
                        .enabled_models
                        .first()
                        .copied()
                        .unwrap_or(ModelKind::Claude);

                    let mut qa_state = QAState::new(*qa_type);
                    if let Err(e) =
                        spawn_qa_agent(&cwd, &prompt, model, &mut qa_state)
                    {
                        eprintln!(
                            "[daemon] Failed to spawn QA {} for {}: {}",
                            qa_type, task_id.0, e
                        );
                    } else {
                        eprintln!(
                            "[daemon] QA {} started for {}",
                            qa_type, task_id.0
                        );
                        daemon_state
                            .qa_agents
                            .insert(task_id.0.clone(), qa_state);

                        // Record event.
                        let event = Event {
                            id: EventId(format!("E-QA-{}-{}", qa_type, task_id.0)),
                            task_id: Some(task_id.clone()),
                            repo_id: None,
                            at: now,
                            kind: EventKind::QAStarted {
                                qa_type: qa_type.to_string(),
                            },
                        };
                        let _ = service.record_event(&event);
                    }
                }
            }
            DaemonAction::QACompleted { task_id, result } => {
                // Save QA result.
                match save_qa_result(&config.repo_root, result) {
                    Ok(path) => {
                        eprintln!(
                            "[daemon] QA completed for {} ({}/{} passed) → {}",
                            task_id.0,
                            result.summary.passed,
                            result.summary.total,
                            path.display()
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "[daemon] QA completed for {} but failed to save result: {}",
                            task_id.0, e
                        );
                    }
                }

                let event = Event {
                    id: EventId(format!("E-QA-DONE-{}", task_id.0)),
                    task_id: Some(task_id.clone()),
                    repo_id: None,
                    at: now,
                    kind: EventKind::QACompleted {
                        passed: result.summary.passed,
                        failed: result.summary.failed,
                        total: result.summary.total,
                    },
                };
                let _ = service.record_event(&event);
            }
            DaemonAction::QAFailed { task_id, result } => {
                let failures: Vec<String> = result
                    .tests
                    .iter()
                    .filter(|t| !t.passed)
                    .map(|t| format!("{}.{}: {}", t.suite, t.name, t.detail))
                    .collect();

                // Save the failed result too (for debugging / history).
                let _ = save_qa_result(&config.repo_root, result);

                eprintln!(
                    "[daemon] QA failed for {} — {} failures: {:?}",
                    task_id.0,
                    failures.len(),
                    failures
                );

                let event = Event {
                    id: EventId(format!("E-QA-FAIL-{}", task_id.0)),
                    task_id: Some(task_id.clone()),
                    repo_id: None,
                    at: now,
                    kind: EventKind::QAFailed { failures },
                };
                let _ = service.record_event(&event);
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

    /// Helper: create a DaemonConfig whose repo_root has a `.othala/qa/baseline.md`.
    fn mk_config_with_baseline() -> (DaemonConfig, PathBuf) {
        let tmp = std::env::temp_dir().join(format!(
            "othala-daemon-qa-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let qa_dir = tmp.join(".othala/qa");
        fs::create_dir_all(&qa_dir).expect("create qa dir");
        fs::write(
            qa_dir.join("baseline.md"),
            "# QA Baseline\n\n## Build\n- run cargo build\n",
        )
        .expect("write baseline");

        let config = DaemonConfig {
            repo_root: tmp.clone(),
            template_dir: PathBuf::from("/tmp/nonexistent-templates"),
            enabled_models: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
            context_config: ContextLoadConfig::default(),
            verify_command: Some("cargo test --workspace".to_string()),
            context_gen_config: ContextGenConfig::default(),
        };
        (config, tmp)
    }

    #[test]
    fn daemon_tick_spawns_baseline_qa_when_spec_exists_and_no_result() {
        let service = mk_service();
        let (config, tmp) = mk_config_with_baseline();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        let mut task = mk_task("T-QA-1");
        task.branch_name = Some("task/T-QA-1".to_string());
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);

        let qa_spawns: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, DaemonAction::SpawnQA { qa_type: QAType::Baseline, .. }))
            .collect();
        assert_eq!(qa_spawns.len(), 1, "should spawn exactly one baseline QA");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn daemon_tick_skips_baseline_qa_when_result_already_exists() {
        let service = mk_service();
        let (config, tmp) = mk_config_with_baseline();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        let mut task = mk_task("T-QA-2");
        task.branch_name = Some("task/T-QA-2".to_string());
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        // Write a pre-existing QA result for this branch.
        let result = QAResult {
            branch: "task/T-QA-2".to_string(),
            commit: "abc1234".to_string(),
            timestamp: Utc::now(),
            tests: vec![],
            summary: crate::qa_agent::QASummary {
                total: 0,
                passed: 0,
                failed: 0,
            },
        };
        crate::qa_agent::save_qa_result(&tmp, &result).expect("save result");

        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);

        let qa_spawns: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, DaemonAction::SpawnQA { .. }))
            .collect();
        assert_eq!(qa_spawns.len(), 0, "should NOT spawn QA when result exists");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn daemon_tick_skips_baseline_qa_when_qa_agent_already_running() {
        let service = mk_service();
        let (config, tmp) = mk_config_with_baseline();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        let mut task = mk_task("T-QA-3");
        task.branch_name = Some("task/T-QA-3".to_string());
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        // Simulate a QA agent already running for this task.
        daemon_state
            .qa_agents
            .insert("T-QA-3".to_string(), QAState::new(QAType::Baseline));

        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);

        let qa_spawns: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, DaemonAction::SpawnQA { .. }))
            .collect();
        assert_eq!(qa_spawns.len(), 0, "should NOT spawn QA when one already running");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn handle_successful_outcome_spawns_qa_validation_when_baseline_exists() {
        let service = mk_service();
        let (config, tmp) = mk_config_with_baseline();

        let task = mk_task("T-QA-4");
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        let outcome = AgentOutcome {
            task_id: TaskId::new("T-QA-4"),
            model: ModelKind::Claude,
            exit_code: Some(0),
            patch_ready: true,
            needs_human: false,
            success: true,
        };

        let actions = handle_agent_completion(&service, &outcome, &config, Utc::now());

        // Should spawn QA Validation, NOT MarkReady.
        assert!(actions
            .iter()
            .any(|a| matches!(a, DaemonAction::SpawnQA { qa_type: QAType::Validation, .. })));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, DaemonAction::MarkReady { .. })));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn qa_validation_pass_produces_completed_and_mark_ready_actions() {
        // When QA validation completes with all tests passing, the daemon
        // should emit QACompleted + MarkReady.
        let result = QAResult {
            branch: "task/T-QA-5".to_string(),
            commit: "def5678".to_string(),
            timestamp: Utc::now(),
            tests: vec![crate::qa_agent::QATestResult {
                name: "build_check".to_string(),
                suite: "build".to_string(),
                passed: true,
                detail: "ok".to_string(),
                duration_ms: 100,
            }],
            summary: crate::qa_agent::QASummary {
                total: 1,
                passed: 1,
                failed: 0,
            },
        };

        let all_passed = result.summary.failed == 0;
        assert!(all_passed, "all tests should pass");

        // Simulate the Phase 2.5 logic: validation pass -> QACompleted + MarkReady.
        let task_id = TaskId::new("T-QA-5");
        let qa_type = QAType::Validation;
        let mut actions = Vec::new();
        if all_passed {
            actions.push(DaemonAction::QACompleted {
                task_id: task_id.clone(),
                result: result.clone(),
            });
            if qa_type == QAType::Validation {
                actions.push(DaemonAction::MarkReady { task_id });
            }
        }

        assert!(actions
            .iter()
            .any(|a| matches!(a, DaemonAction::QACompleted { .. })));
        assert!(actions
            .iter()
            .any(|a| matches!(a, DaemonAction::MarkReady { .. })));
    }

    #[test]
    fn qa_validation_fail_produces_failed_and_retry_actions() {
        let result = QAResult {
            branch: "task/T-QA-6".to_string(),
            commit: "abc123".to_string(),
            timestamp: Utc::now(),
            tests: vec![
                crate::qa_agent::QATestResult {
                    name: "build_check".to_string(),
                    suite: "build".to_string(),
                    passed: true,
                    detail: "ok".to_string(),
                    duration_ms: 100,
                },
                crate::qa_agent::QATestResult {
                    name: "tui_start".to_string(),
                    suite: "tui".to_string(),
                    passed: false,
                    detail: "timeout after 5s".to_string(),
                    duration_ms: 5000,
                },
            ],
            summary: crate::qa_agent::QASummary {
                total: 2,
                passed: 1,
                failed: 1,
            },
        };

        let all_passed = result.summary.failed == 0;
        assert!(!all_passed, "should have failures");

        // Simulate the Phase 2.5 logic: validation fail -> QAFailed + ScheduleRetry.
        let task_id = TaskId::new("T-QA-6");
        let qa_type = QAType::Validation;
        let mut actions = Vec::new();
        if !all_passed {
            actions.push(DaemonAction::QAFailed {
                task_id: task_id.clone(),
                result: result.clone(),
            });
            if qa_type == QAType::Validation {
                let failure_ctx = build_qa_failure_context(&result);
                actions.push(DaemonAction::ScheduleRetry {
                    task_id,
                    next_model: ModelKind::Claude,
                    reason: failure_ctx.clone(),
                });
                assert!(failure_ctx.contains("tui.tui_start: FAIL"));
                assert!(failure_ctx.contains("timeout after 5s"));
            }
        }

        assert!(actions
            .iter()
            .any(|a| matches!(a, DaemonAction::QAFailed { .. })));
        assert!(actions
            .iter()
            .any(|a| matches!(a, DaemonAction::ScheduleRetry { .. })));
    }

    #[test]
    fn daemon_tick_cleans_up_completed_qa_agents() {
        let service = mk_service();
        let config = mk_config();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        // Insert completed and failed QA states.
        let mut completed = QAState::new(QAType::Baseline);
        completed.status = QAStatus::Completed;
        daemon_state
            .qa_agents
            .insert("T-done".to_string(), completed);

        let mut failed = QAState::new(QAType::Validation);
        failed.status = QAStatus::Failed;
        daemon_state
            .qa_agents
            .insert("T-fail".to_string(), failed);

        let mut running = QAState::new(QAType::Baseline);
        running.status = QAStatus::RunningBaseline;
        daemon_state
            .qa_agents
            .insert("T-running".to_string(), running);

        let _ = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);

        // Completed and failed states should be cleaned up.
        assert!(!daemon_state.qa_agents.contains_key("T-done"));
        assert!(!daemon_state.qa_agents.contains_key("T-fail"));
        // Running state should remain.
        assert!(daemon_state.qa_agents.contains_key("T-running"));
    }

    #[test]
    fn handle_needs_human_outcome_produces_record_needs_human() {
        let service = mk_service();
        let config = mk_config();

        let task = mk_task("T-NH");
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        let outcome = AgentOutcome {
            task_id: TaskId::new("T-NH"),
            model: ModelKind::Claude,
            exit_code: Some(0),
            patch_ready: false,
            needs_human: true,
            success: false,
        };

        let actions = handle_agent_completion(&service, &outcome, &config, Utc::now());
        assert!(actions
            .iter()
            .any(|a| matches!(a, DaemonAction::RecordNeedsHuman { .. })));
    }
}
