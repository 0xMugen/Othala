//! Daemon loop — structured tick function with the full autonomous pipeline.
//!
//! Replaces the inline loop in `main.rs` with a testable `daemon_tick()` that
//! returns actions for the caller to execute.

use chrono::{DateTime, Datelike, Duration, Utc};
use orch_core::config::{load_org_config, BudgetConfig, OrgConfig};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{EventId, ModelKind, Task, TaskId};
use orch_git::{discover_repo, has_uncommitted_changes, GitCli};
use orch_graphite::GraphiteClient;
use orch_notify::{notification_for_event, NotificationDispatcher};

use crate::agent_log;
use crate::context_gen::{
    build_context_gen_prompt, context_is_current, poll_context_gen, should_regenerate,
    spawn_context_gen, ContextGenConfig, ContextGenState, ContextGenStatus,
};
use crate::context_graph::{load_context_graph, ContextLoadConfig};
use crate::prompt_builder::{build_rich_prompt, PromptConfig, PromptRole, RetryContext};
use crate::qa_agent::{
    build_qa_failure_context, build_qa_prompt, load_baseline, load_latest_result,
    load_task_spec as load_qa_task_spec, poll_qa_agent, save_qa_result, spawn_qa_agent, QAResult,
    QAState, QAStatus, QAType,
};
use crate::retry::{evaluate_retry, pick_next_model_with_health, ModelHealthTracker};
use crate::stack_pipeline::{next_action, PipelineAction, PipelineStage, PipelineState};
use crate::supervisor::{AgentOutcome, AgentSupervisor};
use crate::test_spec::load_test_spec;
use crate::OrchdService;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    /// Skip all QA runs (baseline + validation). Prevents QA agent from
    /// mutating production state via TUI automation.
    pub skip_qa: bool,
    /// Skip background context regeneration during tick loop.
    pub skip_context_regen: bool,
    pub dry_run: bool,
    pub agent_timeout_secs: u64,
    pub drain_timeout_secs: u64,
}

/// Mutable state carried across daemon ticks.
pub struct DaemonState {
    /// Active pipeline states for tasks in the submit flow.
    pub pipelines: HashMap<String, PipelineState>,
    /// Context generation state.
    pub context_gen: ContextGenState,
    /// Per-task QA agent state (keyed by task_id).
    pub qa_agents: HashMap<String, QAState>,
    pub verify_cache: HashMap<String, String>,
    pub model_health: ModelHealthTracker,
    pub restack_retries: HashMap<String, RestackRetryState>,
    pub notification_dispatcher: Option<NotificationDispatcher>,
    pub config_last_modified: Option<std::time::SystemTime>,
    pub shutdown_requested: bool,
    pub shutdown_deadline: Option<std::time::Instant>,
    pub budget_used_today: u64,
    pub budget_used_month: u64,
    pub budget_last_reset_day: Option<u32>,
    pub budget_last_reset_month: Option<u32>,
    pub budget_output_chars_by_task: HashMap<String, u64>,
    pub token_trackers: HashMap<String, crate::auto_compact::TokenTracker>,
    pub auto_compact_config: crate::auto_compact::AutoCompactConfig,
}

const RESTACK_RETRY_MAX_RETRIES: u32 = 3;
const RESTACK_RETRY_INITIAL_BACKOFF_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub struct RestackRetryState {
    pub attempts: u32,
    pub max_retries: u32,
    pub last_attempt: DateTime<Utc>,
    pub backoff_secs: u64,
}

impl RestackRetryState {
    fn first(now: DateTime<Utc>) -> Self {
        Self {
            attempts: 1,
            max_retries: RESTACK_RETRY_MAX_RETRIES,
            last_attempt: now,
            backoff_secs: RESTACK_RETRY_INITIAL_BACKOFF_SECS,
        }
    }

    fn next(&self, now: DateTime<Utc>) -> Option<Self> {
        if self.attempts >= self.max_retries {
            return None;
        }

        Some(Self {
            attempts: self.attempts + 1,
            max_retries: self.max_retries,
            last_attempt: now,
            backoff_secs: self.backoff_secs.saturating_mul(2),
        })
    }

    fn retry_ready_at(&self) -> DateTime<Utc> {
        self.last_attempt + Duration::seconds(self.backoff_secs as i64)
    }

    fn is_ready(&self, now: DateTime<Utc>) -> bool {
        now >= self.retry_ready_at()
    }
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            pipelines: HashMap::new(),
            context_gen: ContextGenState::new(),
            qa_agents: HashMap::new(),
            verify_cache: HashMap::new(),
            model_health: ModelHealthTracker::new(),
            restack_retries: HashMap::new(),
            notification_dispatcher: None,
            config_last_modified: None,
            shutdown_requested: false,
            shutdown_deadline: None,
            budget_used_today: 0,
            budget_used_month: 0,
            budget_last_reset_day: None,
            budget_last_reset_month: None,
            budget_output_chars_by_task: HashMap::new(),
            token_trackers: HashMap::new(),
            auto_compact_config: crate::auto_compact::AutoCompactConfig::default(),
        }
    }

    pub fn request_shutdown(&mut self, drain_timeout_secs: u64) {
        self.shutdown_requested = true;
        self.shutdown_deadline = Some(
            std::time::Instant::now() + std::time::Duration::from_secs(drain_timeout_secs),
        );
    }
}

pub fn check_config_reload(config_path: &Path, daemon_state: &mut DaemonState) -> Option<OrgConfig> {
    let metadata = std::fs::metadata(config_path).ok()?;
    let mtime = metadata.modified().ok()?;
    if daemon_state.config_last_modified == Some(mtime) {
        return None;
    }
    daemon_state.config_last_modified = Some(mtime);
    match load_org_config(config_path) {
        Ok(config) => {
            eprintln!("[daemon] Config reloaded from {}", config_path.display());
            Some(config)
        }
        Err(e) => {
            eprintln!("[daemon] Failed to reload config: {}", e);
            None
        }
    }
}

fn schedule_restack_retry(
    daemon_state: &mut DaemonState,
    task_id: &TaskId,
    now: DateTime<Utc>,
) -> Option<RestackRetryState> {
    let next = match daemon_state.restack_retries.get(&task_id.0) {
        Some(existing) => existing.next(now),
        None => Some(RestackRetryState::first(now)),
    };

    match next {
        Some(state) => {
            daemon_state
                .restack_retries
                .insert(task_id.0.clone(), state.clone());
            Some(state)
        }
        None => {
            daemon_state.restack_retries.remove(&task_id.0);
            None
        }
    }
}

fn stop_task_with_failure_reason(
    service: &OrchdService,
    notification_dispatcher: Option<&NotificationDispatcher>,
    task_id: &TaskId,
    reason: &str,
    at: DateTime<Utc>,
) {
    let event = Event {
        id: EventId(format!(
            "E-FAILED-{}-{}",
            task_id.0,
            at.timestamp_nanos_opt().unwrap_or_default()
        )),
        task_id: Some(task_id.clone()),
        repo_id: None,
        at,
        kind: EventKind::TaskFailed {
            reason: reason.to_string(),
            is_final: true,
        },
    };
    let _ = record_event_with_notification(service, notification_dispatcher, &event);

    if let Ok(Some(mut task)) = service.task(task_id) {
        task.state = TaskState::Stopped;
        task.updated_at = at;
        task.last_failure_reason = Some(reason.to_string());
        let _ = service.store.upsert_task(&task);
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}

fn dispatch_notification(dispatcher: Option<&NotificationDispatcher>, event: &Event) {
    let Some(dispatcher) = dispatcher else {
        return;
    };

    let Some(notification) = notification_for_event(event) else {
        return;
    };

    for (sink_kind, result) in dispatcher.dispatch(&notification) {
        if let Err(err) = result {
            eprintln!(
                "[daemon] notification dispatch failed for {:?}: {}",
                sink_kind, err
            );
        }
    }
}

fn record_event_with_notification(
    service: &OrchdService,
    dispatcher: Option<&NotificationDispatcher>,
    event: &Event,
) -> Result<(), crate::service::ServiceError> {
    service.record_event(event)?;
    dispatch_notification(dispatcher, event);
    Ok(())
}

fn load_budget_config_for_tick(repo_root: &Path) -> BudgetConfig {
    let config_path = repo_root.join(".othala/config.toml");
    load_org_config(config_path)
        .map(|org| org.budget)
        .unwrap_or_default()
}

fn check_budget(state: &DaemonState, config: &BudgetConfig) -> bool {
    if !config.enabled {
        return true;
    }
    state.budget_used_today < config.daily_token_limit
        && state.budget_used_month < config.monthly_token_limit
}

fn maybe_reset_budget(state: &mut DaemonState) {
    let now = chrono::Utc::now();
    let today = now.day();
    let month = now.month();
    if state.budget_last_reset_day != Some(today) {
        state.budget_used_today = 0;
        state.budget_last_reset_day = Some(today);
    }
    if state.budget_last_reset_month != Some(month) {
        state.budget_used_month = 0;
        state.budget_last_reset_month = Some(month);
    }
}

fn track_output_chars(state: &mut DaemonState, task_id: &TaskId, lines: &[String]) {
    let chars = lines.iter().map(|line| line.chars().count() as u64).sum::<u64>();
    *state
        .budget_output_chars_by_task
        .entry(task_id.0.clone())
        .or_insert(0) += chars;
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
    MarkReady {
        task_id: TaskId,
    },
    MarkMerged {
        task_id: TaskId,
    },
    /// Record that a task needs human intervention.
    RecordNeedsHuman {
        task_id: TaskId,
        reason: String,
    },
    /// Retry a failed task with a different model.
    ScheduleRetry {
        task_id: TaskId,
        next_model: ModelKind,
        reason: String,
    },
    /// Task has permanently failed.
    TaskFailed {
        task_id: TaskId,
        reason: String,
    },
    /// Execute a pipeline action (verify, stack, submit).
    ExecutePipeline {
        action: PipelineAction,
    },
    /// Trigger background context regeneration.
    TriggerContextRegen,
    ContextRegenCompleted {
        success: bool,
    },
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
    Log {
        message: String,
    },
    EmitEvent {
        task_id: Option<TaskId>,
        repo_id: Option<orch_core::types::RepoId>,
        kind: EventKind,
    },
    ShutdownComplete,
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
    maybe_reset_budget(daemon_state);

    if daemon_state.shutdown_requested {
        let poll_result = supervisor.poll();

        for chunk in &poll_result.output {
            track_output_chars(daemon_state, &chunk.task_id, &chunk.lines);
            let output = chunk.lines.join("\n");
            if let Some(tracker) = daemon_state.token_trackers.get_mut(&chunk.task_id.0) {
                let estimated = crate::auto_compact::estimate_tokens(&output);
                tracker.record_usage(estimated, 0);
            }
            if let Err(err) =
                agent_log::append_agent_output(&config.repo_root, &chunk.task_id, &chunk.lines)
            {
                eprintln!(
                    "[daemon] Failed to persist agent output for {}: {err}",
                    chunk.task_id.0
                );
            }

            for line in &chunk.lines {
                actions.push(DaemonAction::Log {
                    message: format!("[{}] {}", chunk.task_id.0, line),
                });
            }
        }

        let notification_dispatcher = daemon_state.notification_dispatcher.take();
        for outcome in &poll_result.completed {
            let outcome_actions = handle_agent_completion(
                service,
                notification_dispatcher.as_ref(),
                outcome,
                config,
                daemon_state,
                now,
            );
            actions.extend(outcome_actions);
        }
        daemon_state.notification_dispatcher = notification_dispatcher;

        let deadline_reached = daemon_state
            .shutdown_deadline
            .map(|deadline| std::time::Instant::now() >= deadline)
            .unwrap_or(false);
        let running_agents = supervisor.running_count();

        if running_agents == 0 {
            actions.push(DaemonAction::ShutdownComplete);
            return actions;
        }

        if deadline_reached {
            let unfinished = supervisor.drain_agents(std::time::Duration::from_millis(10));
            if !unfinished.is_empty() {
                supervisor.terminate_all_agents();
            }
            actions.push(DaemonAction::ShutdownComplete);
            return actions;
        }

        return actions;
    }

    // --- Phase 1: Spawn agents for Chatting tasks without sessions ---
    let budget_config = load_budget_config_for_tick(&config.repo_root);
    if let Ok(chatting) = service.list_tasks_by_state(TaskState::Chatting) {
        for task in &chatting {
            if !supervisor.has_session(&task.id) {
                if !check_budget(daemon_state, &budget_config) {
                    actions.push(DaemonAction::EmitEvent {
                        task_id: Some(task.id.clone()),
                        repo_id: Some(task.repo_id.clone()),
                        kind: EventKind::BudgetExceeded,
                    });
                    continue;
                }
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
    if !config.skip_qa {
        if let Ok(chatting) = service.list_tasks_by_state(TaskState::Chatting) {
            for task in &chatting {
                let default_branch = format!("task/{}", task.id.0);
                let branch = task.branch_name.as_deref().unwrap_or(&default_branch);

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
    } // end skip_qa_baseline guard

    // --- Phase 2: Poll supervisor for completed agents ---
    let poll_result = supervisor.poll();

    for chunk in &poll_result.output {
        track_output_chars(daemon_state, &chunk.task_id, &chunk.lines);
        let output = chunk.lines.join("\n");
        if let Some(tracker) = daemon_state.token_trackers.get_mut(&chunk.task_id.0) {
            let estimated = crate::auto_compact::estimate_tokens(&output);
            tracker.record_usage(estimated, 0);
        }
        if let Err(err) =
            agent_log::append_agent_output(&config.repo_root, &chunk.task_id, &chunk.lines)
        {
            eprintln!(
                "[daemon] Failed to persist agent output for {}: {err}",
                chunk.task_id.0
            );
        }

        for line in &chunk.lines {
            actions.push(DaemonAction::Log {
                message: format!("[{}] {}", chunk.task_id.0, line),
            });
        }
    }

    let notification_dispatcher = daemon_state.notification_dispatcher.take();
    for outcome in &poll_result.completed {
        let outcome_actions = handle_agent_completion(
            service,
            notification_dispatcher.as_ref(),
            outcome,
            config,
            daemon_state,
            now,
        );
        actions.extend(outcome_actions);
    }
    daemon_state.notification_dispatcher = notification_dispatcher;

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
                if pipeline.stage == PipelineStage::StackOnParent {
                    if let Some(retry_state) = daemon_state.restack_retries.get(&key) {
                        if !retry_state.is_ready(now) {
                            continue;
                        }
                    }
                }
                let action = next_action(pipeline);
                actions.push(DaemonAction::ExecutePipeline { action });
            }
        }
    }

    // Clean up terminal pipelines.
    daemon_state.pipelines.retain(|_, p| !p.is_terminal());
    daemon_state
        .restack_retries
        .retain(|task_id, _| daemon_state.pipelines.contains_key(task_id));

    if let Ok(awaiting) = service.list_tasks_by_state(TaskState::AwaitingMerge) {
        let auto_merge_mode = repo_mode_is_merge(&config.repo_root);

        for task in &awaiting {
            if let Some(pr) = &task.pr {
                if auto_merge_mode {
                    if let Some(branch) = task.branch_name.as_deref() {
                        if auto_merge_branch_into_trunk(&config.repo_root, branch) {
                            actions.push(DaemonAction::MarkMerged {
                                task_id: task.id.clone(),
                            });
                            continue;
                        }
                    }
                }

                let merged_via_pr = check_pr_merged(pr.number, &config.repo_root);
                let merged_via_graphite_stack = pr.number == 0
                    && pr.url.starts_with("graphite://")
                    && task
                        .branch_name
                        .as_deref()
                        .map(|b| is_branch_merged_into_trunk(&config.repo_root, b))
                        .unwrap_or(false)
                    && !worktree_has_uncommitted_changes(&task.worktree_path);

                if merged_via_pr || merged_via_graphite_stack {
                    actions.push(DaemonAction::MarkMerged {
                        task_id: task.id.clone(),
                    });
                }
            }
        }
    }

    // Poll any running context gen process.
    let was_context_regen_running = daemon_state.context_gen.status == ContextGenStatus::Running;
    if let Some(paths) = poll_context_gen(&config.repo_root, &mut daemon_state.context_gen) {
        actions.push(DaemonAction::Log {
            message: format!("[context-gen] Updated {} context files", paths.len()),
        });
        actions.push(DaemonAction::ContextRegenCompleted { success: true });
    } else if was_context_regen_running
        && daemon_state.context_gen.status == ContextGenStatus::Failed
    {
        actions.push(DaemonAction::ContextRegenCompleted { success: false });
    }

    // Check if we should trigger a regen based on transitions or stale hash.
    // Triggers: MarkReady actions (task completed), pipeline Complete actions (merged),
    // or git hash mismatch (cheap check — file read + git rev-parse).
    let has_trigger = actions.iter().any(|a| {
        matches!(a, DaemonAction::MarkReady { .. })
            || matches!(a, DaemonAction::MarkMerged { .. })
            || matches!(
                a,
                DaemonAction::ExecutePipeline {
                    action: PipelineAction::Complete { .. }
                }
            )
    });

    let is_stale = !context_is_current(&config.repo_root);

    if !config.skip_context_regen
        && (has_trigger || is_stale)
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
        task.last_failure_reason
            .as_ref()
            .map(|reason| RetryContext {
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

    // If the last failure reason looks like structured QA output, inject it
    // through the dedicated qa_failure_context field so the prompt builder
    // renders it in its own section rather than buried inside retry context.
    let qa_failure_context = task
        .last_failure_reason
        .as_ref()
        .filter(|r| r.starts_with("## QA Failures"))
        .cloned();

    let prompt_config = PromptConfig {
        task_id: task.id.clone(),
        task_title: task.title.clone(),
        role,
        context,
        test_spec: test_spec_content,
        retry,
        verify_command: config.verify_command.clone(),
        qa_failure_context,
        repo_root: Some(config.repo_root.clone()),
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
    notification_dispatcher: Option<&NotificationDispatcher>,
    outcome: &AgentOutcome,
    config: &DaemonConfig,
    daemon_state: &mut DaemonState,
    now: DateTime<Utc>,
) -> Vec<DaemonAction> {
    let mut actions = Vec::new();
    daemon_state.verify_cache.remove(&outcome.task_id.0);
    let output_chars = daemon_state
        .budget_output_chars_by_task
        .remove(&outcome.task_id.0)
        .unwrap_or_default();
    let output_tokens = estimate_tokens_from_char_count(output_chars);
    daemon_state.budget_used_today = daemon_state.budget_used_today.saturating_add(output_tokens);
    daemon_state.budget_used_month = daemon_state.budget_used_month.saturating_add(output_tokens);

    let completion_event = Event {
        id: EventId(format!(
            "E-AGENT-COMPLETED-{}-{}",
            outcome.task_id.0,
            now.timestamp_nanos_opt().unwrap_or_default()
        )),
        task_id: Some(outcome.task_id.clone()),
        repo_id: service
            .task(&outcome.task_id)
            .ok()
            .flatten()
            .map(|t| t.repo_id),
        at: now,
        kind: EventKind::AgentCompleted {
            model: outcome.model.as_str().to_string(),
            success: outcome.success,
            duration_secs: outcome.duration_secs,
        },
    };
    if let Err(e) = record_event_with_notification(service, notification_dispatcher, &completion_event)
    {
        eprintln!(
            "[daemon] Failed to record agent completion for {}: {}",
            outcome.task_id.0, e
        );
    }

    let stop_reason = if outcome.patch_ready || outcome.success {
        "completed"
    } else if outcome.needs_human {
        "needs_human"
    } else {
        "failed"
    };
    if let Err(e) = service.store.finish_open_runs_for_task(
        &outcome.task_id,
        now,
        stop_reason,
        outcome.exit_code,
        Some(outcome.duration_secs as f64),
    ) {
        eprintln!(
            "[daemon] Failed to persist run completion for {}: {}",
            outcome.task_id.0, e
        );
    }

    if outcome.patch_ready || outcome.success {
        daemon_state.model_health.record_success(outcome.model);
        // If a QA baseline spec exists and QA is enabled, spawn a validation
        // QA run instead of immediately marking ready.
        if !config.skip_qa && load_baseline(&config.repo_root).is_some() {
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

    // Check for uncommitted changes as fallback success indicator.
    // If the agent made code changes but didn't signal [patch_ready], treat as success.
    if let Ok(Some(task)) = service.task(&outcome.task_id) {
        let git = GitCli::default();
        if let Ok(repo) = discover_repo(&task.worktree_path, &git) {
            if let Ok(true) = has_uncommitted_changes(&repo, &git) {
                eprintln!(
                    "[daemon] {} has uncommitted changes — treating as success despite missing [patch_ready]",
                    outcome.task_id.0
                );
                daemon_state.model_health.record_success(outcome.model);
                if !config.skip_qa && load_baseline(&config.repo_root).is_some() {
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
        }
    }

    // Agent failed — evaluate retry.
    daemon_state
        .model_health
        .record_failure_at(outcome.model, now);

    if let Ok(Some(task)) = service.task(&outcome.task_id) {
        let decision = evaluate_retry(&task, outcome, &config.enabled_models);

        if decision.should_retry {
            let next_model = pick_next_model_with_health(
                &task,
                outcome.model,
                &config.enabled_models,
                &daemon_state.model_health,
                now,
            )
            .or(decision.next_model);

            if let Some(next_model) = next_model {
                actions.push(DaemonAction::ScheduleRetry {
                    task_id: outcome.task_id.clone(),
                    next_model,
                    reason: format!(
                        "retrying (attempt {}/{}) with {}",
                        task.retry_count + 1,
                        task.max_retries,
                        next_model.as_str()
                    ),
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

fn estimate_tokens_from_prompt(prompt: &str) -> u64 {
    let chars = prompt.chars().count() as u64;
    estimate_tokens_from_char_count(chars)
}

fn estimate_tokens_from_char_count(chars: u64) -> u64 {
    chars.div_ceil(4)
}

fn run_verify_command(cwd: &Path, command: &str) -> Result<(), String> {
    let output = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to spawn verify command `{command}`: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(format!(
        "verify command `{command}` failed (exit={:?})\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code()
    ))
}

fn is_gh_pr_state_merged(stdout: &[u8]) -> bool {
    String::from_utf8_lossy(stdout).trim() == "MERGED"
}

fn check_pr_merged(pr_number: u64, repo_root: &Path) -> bool {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "state",
            "--jq",
            ".state",
        ])
        .current_dir(repo_root)
        .output();
    match output {
        Ok(o) if o.status.success() => is_gh_pr_state_merged(&o.stdout),
        _ => false,
    }
}

fn is_branch_merged_into_trunk(repo_root: &Path, branch: &str) -> bool {
    if branch.trim().is_empty() {
        return false;
    }

    // Prefer remote trunk if available, otherwise fallback to local main.
    for trunk in ["origin/main", "main"] {
        let status = Command::new("git")
            .args(["merge-base", "--is-ancestor", branch, trunk])
            .current_dir(repo_root)
            .status();
        if matches!(status, Ok(s) if s.success()) {
            return true;
        }
    }

    false
}

fn repo_mode_is_merge(repo_root: &Path) -> bool {
    let mode_path = repo_root.join(".othala/repo-mode.toml");
    let Ok(contents) = fs::read_to_string(mode_path) else {
        return false;
    };

    contents
        .lines()
        .map(str::trim)
        .any(|line| line == "mode = \"merge\"" || line == "mode=\"merge\"")
}

fn auto_merge_branch_into_trunk(repo_root: &Path, branch: &str) -> bool {
    if branch.trim().is_empty() {
        return false;
    }

    if is_branch_merged_into_trunk(repo_root, branch) {
        return true;
    }

    let status = Command::new("bash")
        .arg("-lc")
        .arg(format!(
            "git fetch origin && git checkout -q main && git pull --ff-only origin main && git merge --ff-only {branch} && git push origin main"
        ))
        .current_dir(repo_root)
        .status();

    matches!(status, Ok(s) if s.success()) && is_branch_merged_into_trunk(repo_root, branch)
}

fn worktree_has_uncommitted_changes(path: &Path) -> bool {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output();

    match output {
        Ok(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
        _ => false,
    }
}

fn get_worktree_head_sha(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

fn apply_retry_transition(
    service: &OrchdService,
    notification_dispatcher: Option<&NotificationDispatcher>,
    task_id: &TaskId,
    next_model: ModelKind,
    reason: &str,
    at: DateTime<Utc>,
) -> Result<bool, String> {
    let Some(mut task) = service.task(task_id).map_err(|e| e.to_string())? else {
        return Err(format!("task not found for retry: {}", task_id.0));
    };
    let previous_model = task.preferred_model;

    if task.retry_count >= task.max_retries {
        let failed_event = Event {
            id: EventId(format!(
                "E-FAILED-{}-{}",
                task_id.0,
                at.timestamp_nanos_opt().unwrap_or_default()
            )),
            task_id: Some(task_id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::TaskFailed {
                reason: format!(
                    "{} (max retries reached: {}/{})",
                    reason, task.retry_count, task.max_retries
                ),
                is_final: true,
            },
        };
        let _ = record_event_with_notification(service, notification_dispatcher, &failed_event);

        task.state = TaskState::Stopped;
        task.last_failure_reason = Some(reason.to_string());
        task.updated_at = at;
        service
            .store
            .upsert_task(&task)
            .map_err(|e| format!("failed to persist stopped task {}: {e}", task_id.0))?;
        return Ok(false);
    }

    task.retry_count += 1;
    if let Some(prev_model) = previous_model {
        if prev_model != next_model && !task.failed_models.contains(&prev_model) {
            task.failed_models.push(prev_model);
        }
    }
    task.preferred_model = Some(next_model);
    task.last_failure_reason = Some(reason.to_string());
    service
        .store
        .upsert_task(&task)
        .map_err(|e| format!("failed to update task for retry {}: {e}", task_id.0))?;

    if task.state != TaskState::Chatting {
        let _ = service.transition_task_state(
            task_id,
            TaskState::Chatting,
            EventId(format!(
                "E-RETRY-STATE-{}-{}",
                task_id.0,
                at.timestamp_nanos_opt().unwrap_or_default()
            )),
            at,
        );
    }

    let retry_event = Event {
        id: EventId(format!(
            "E-RETRY-{}-{}",
            task_id.0,
            at.timestamp_nanos_opt().unwrap_or_default()
        )),
        task_id: Some(task_id.clone()),
        repo_id: Some(task.repo_id.clone()),
        at,
        kind: EventKind::RetryScheduled {
            attempt: task.retry_count,
            model: next_model.as_str().to_string(),
            reason: reason.to_string(),
        },
    };
    let _ = record_event_with_notification(service, notification_dispatcher, &retry_event);

    if let Some(prev_model) = previous_model {
        if prev_model != next_model {
            let fallback_event = Event {
                id: EventId(format!(
                    "E-MODEL-FALLBACK-{}-{}",
                    task_id.0,
                    at.timestamp_nanos_opt().unwrap_or_default()
                )),
                task_id: Some(task_id.clone()),
                repo_id: Some(task.repo_id.clone()),
                at,
                kind: EventKind::ModelFallback {
                    from_model: prev_model.as_str().to_string(),
                    to_model: next_model.as_str().to_string(),
                    reason: reason.to_string(),
                },
            };
            let _ =
                record_event_with_notification(service, notification_dispatcher, &fallback_event);
        }
    }

    Ok(true)
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
) -> bool {
    let now = Utc::now();
    let mut should_exit = false;

    for action in actions {
        match action {
            DaemonAction::SpawnAgent {
                task_id,
                model,
                prompt,
                worktree_path,
            } => {
                if config.dry_run {
                    eprintln!(
                        "[dry-run] Would spawn agent for {} with model {}",
                        task_id.0, model
                    );
                    continue;
                }
                if let Ok(Some(task)) = service.task(task_id) {
                    if let Err(e) = supervisor.spawn_agent(
                        task_id,
                        &task.repo_id,
                        worktree_path,
                        prompt,
                        task.preferred_model,
                        std::time::Duration::from_secs(config.agent_timeout_secs),
                    ) {
                        eprintln!("[daemon] Failed to spawn agent for {}: {}", task_id.0, e);
                    } else {
                        let estimated_tokens = estimate_tokens_from_prompt(prompt);
                        if let Err(e) = service
                            .store
                            .set_open_run_estimated_tokens(task_id, estimated_tokens)
                        {
                            eprintln!(
                                "[daemon] Failed to store token estimate for {}: {}",
                                task_id.0, e
                            );
                        }
                        let event = Event {
                            id: EventId(format!(
                                "E-AGENT-SPAWNED-{}-{}",
                                task_id.0,
                                now.timestamp_nanos_opt().unwrap_or_default()
                            )),
                            task_id: Some(task_id.clone()),
                            repo_id: Some(task.repo_id.clone()),
                            at: now,
                            kind: EventKind::AgentSpawned {
                                model: model.as_str().to_string(),
                            },
                        };
                        if let Err(e) = record_event_with_notification(
                            service,
                            daemon_state.notification_dispatcher.as_ref(),
                            &event,
                        ) {
                            eprintln!(
                                "[daemon] Failed to record agent_spawned for {}: {}",
                                task_id.0, e
                            );
                        }
                    }
                }
            }
            DaemonAction::MarkReady { task_id } => {
                if config.dry_run {
                    eprintln!("[dry-run] Would mark {} ready", task_id.0);
                    continue;
                }
                let event_id = EventId(format!(
                    "E-READY-{}-{}",
                    task_id.0,
                    now.timestamp_nanos_opt().unwrap_or_default()
                ));
                match service.mark_ready(task_id, event_id, now) {
                    Ok(_) => eprintln!("[daemon] {} -> Ready", task_id.0),
                    Err(e) => eprintln!("[daemon] Failed to mark {} ready: {}", task_id.0, e),
                }
            }
            DaemonAction::MarkMerged { task_id } => {
                if config.dry_run {
                    eprintln!("[dry-run] Would mark {} merged", task_id.0);
                    continue;
                }
                let event_id = EventId(format!(
                    "E-MERGED-{}-{}",
                    task_id.0,
                    now.timestamp_nanos_opt().unwrap_or_default()
                ));
                match service.mark_merged(task_id, event_id, now) {
                    Ok(_) => eprintln!("[daemon] {} -> Merged", task_id.0),
                    Err(e) => eprintln!("[daemon] Failed to mark {} merged: {}", task_id.0, e),
                }
            }
            DaemonAction::RecordNeedsHuman { task_id, reason } => {
                let event = Event {
                    id: EventId(format!(
                        "E-HUMAN-{}-{}",
                        task_id.0,
                        now.timestamp_nanos_opt().unwrap_or_default()
                    )),
                    task_id: Some(task_id.clone()),
                    repo_id: None,
                    at: now,
                    kind: EventKind::NeedsHuman {
                        reason: reason.clone(),
                    },
                };
                if let Err(e) = record_event_with_notification(
                    service,
                    daemon_state.notification_dispatcher.as_ref(),
                    &event,
                ) {
                    eprintln!(
                        "[daemon] Failed to record needs_human for {}: {}",
                        task_id.0, e
                    );
                }
            }
            DaemonAction::ScheduleRetry {
                task_id,
                next_model,
                reason,
            } => {
                if config.dry_run {
                    eprintln!(
                        "[dry-run] Would schedule retry for {} with {} ({})",
                        task_id.0, next_model, reason
                    );
                    continue;
                }
                match apply_retry_transition(
                    service,
                    daemon_state.notification_dispatcher.as_ref(),
                    task_id,
                    *next_model,
                    reason,
                    now,
                ) {
                    Ok(true) => {
                        daemon_state.pipelines.remove(&task_id.0);
                        daemon_state.restack_retries.remove(&task_id.0);
                        eprintln!("[daemon] {} scheduled retry with {}", task_id.0, next_model);
                    }
                    Ok(false) => {
                        daemon_state.pipelines.remove(&task_id.0);
                        daemon_state.restack_retries.remove(&task_id.0);
                        eprintln!(
                            "[daemon] {} reached max retries; task moved to STOPPED",
                            task_id.0
                        );
                    }
                    Err(e) => {
                        eprintln!("[daemon] Failed to schedule retry for {}: {}", task_id.0, e);
                    }
                }
            }
            DaemonAction::TaskFailed { task_id, reason } => {
                if config.dry_run {
                    eprintln!("[dry-run] Would stop {} with failure: {}", task_id.0, reason);
                    continue;
                }
                stop_task_with_failure_reason(
                    service,
                    daemon_state.notification_dispatcher.as_ref(),
                    task_id,
                    reason,
                    now,
                );
                daemon_state.restack_retries.remove(&task_id.0);

                eprintln!("[daemon] {} failed → stopped: {}", task_id.0, reason);
            }
            DaemonAction::ExecutePipeline { action } => {
                if config.dry_run {
                    eprintln!("[dry-run] Would execute pipeline action: {:?}", action);
                    continue;
                }
                match action {
                PipelineAction::RunVerify {
                    task_id,
                    worktree_path,
                } => {
                    let current_sha = get_worktree_head_sha(worktree_path);
                    if let Some(sha) = current_sha.as_ref() {
                        if daemon_state.verify_cache.get(&task_id.0) == Some(sha) {
                            eprintln!("[daemon] verify cache hit for {}, skipping", task_id.0);
                            if let Some(pipeline) = daemon_state.pipelines.get_mut(&task_id.0) {
                                pipeline.advance();
                            }
                            continue;
                        }
                    }

                    let verify_cmd = config
                        .verify_command
                        .as_deref()
                        .unwrap_or("cargo check && cargo test --workspace");

                    let _ = record_event_with_notification(
                        service,
                        daemon_state.notification_dispatcher.as_ref(),
                        &Event {
                        id: EventId(format!(
                            "E-VERIFY-START-{}-{}",
                            task_id.0,
                            now.timestamp_nanos_opt().unwrap_or_default()
                        )),
                        task_id: Some(task_id.clone()),
                        repo_id: service.task(task_id).ok().flatten().map(|t| t.repo_id),
                        at: now,
                        kind: EventKind::VerifyStarted,
                    },
                    );

                    match run_verify_command(worktree_path, verify_cmd) {
                        Ok(()) => {
                            if let Some(sha) = current_sha {
                                daemon_state.verify_cache.insert(task_id.0.clone(), sha);
                            }
                            let _ = record_event_with_notification(
                                service,
                                daemon_state.notification_dispatcher.as_ref(),
                                &Event {
                                id: EventId(format!(
                                    "E-VERIFY-DONE-{}-{}",
                                    task_id.0,
                                    now.timestamp_nanos_opt().unwrap_or_default()
                                )),
                                task_id: Some(task_id.clone()),
                                repo_id: service.task(task_id).ok().flatten().map(|t| t.repo_id),
                                at: now,
                                kind: EventKind::VerifyCompleted { success: true },
                            },
                            );
                            if let Some(pipeline) = daemon_state.pipelines.get_mut(&task_id.0) {
                                pipeline.advance();
                            }
                        }
                        Err(error) => {
                            daemon_state.verify_cache.remove(&task_id.0);
                            let _ = record_event_with_notification(
                                service,
                                daemon_state.notification_dispatcher.as_ref(),
                                &Event {
                                id: EventId(format!(
                                    "E-VERIFY-DONE-{}-{}",
                                    task_id.0,
                                    now.timestamp_nanos_opt().unwrap_or_default()
                                )),
                                task_id: Some(task_id.clone()),
                                repo_id: service.task(task_id).ok().flatten().map(|t| t.repo_id),
                                at: now,
                                kind: EventKind::VerifyCompleted { success: false },
                            },
                            );

                            let retry_model = service
                                .task(task_id)
                                .ok()
                                .flatten()
                                .and_then(|t| t.preferred_model)
                                .or_else(|| config.enabled_models.first().copied())
                                .unwrap_or(ModelKind::Claude);
                            if let Some(pipeline) = daemon_state.pipelines.get_mut(&task_id.0) {
                                pipeline.fail(error.clone());
                            }
                            match apply_retry_transition(
                                service,
                                daemon_state.notification_dispatcher.as_ref(),
                                task_id,
                                retry_model,
                                &error,
                                now,
                            ) {
                                Ok(true) => {}
                                Ok(false) => {
                                    eprintln!(
                                        "[daemon] Verify failed for {}; retries exhausted",
                                        task_id.0
                                    );
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[daemon] Verify failure retry handling failed for {}: {}",
                                        task_id.0, e
                                    );
                                }
                            }
                            daemon_state.pipelines.remove(&task_id.0);
                            daemon_state.restack_retries.remove(&task_id.0);
                        }
                    }
                }
                PipelineAction::StackOnParent {
                    task_id,
                    worktree_path,
                    parent_branch,
                } => {
                    let event_seed = now.timestamp_nanos_opt().unwrap_or_default();
                    let _ = service.start_restack(
                        task_id,
                        EventId(format!("E-RESTACK-START-{}-{event_seed}", task_id.0)),
                        now,
                    );

                    let graphite = GraphiteClient::new(worktree_path.clone());
                    match graphite.move_current_branch_onto(parent_branch) {
                        Ok(()) => {
                            let _ = service.complete_restack(
                                task_id,
                                EventId(format!("E-RESTACK-DONE-{}-{event_seed}", task_id.0)),
                                now,
                            );
                            if let Some(pipeline) = daemon_state.pipelines.get_mut(&task_id.0) {
                                pipeline.advance();
                            }
                            daemon_state.restack_retries.remove(&task_id.0);
                        }
                        Err(error) => {
                            let err_msg = format!("restack onto `{parent_branch}` failed: {error}");

                            if error.is_restack_conflict() {
                                let _ = record_event_with_notification(
                                    service,
                                    daemon_state.notification_dispatcher.as_ref(),
                                    &Event {
                                    id: EventId(format!(
                                        "E-RESTACK-CONFLICT-{}-{}",
                                        task_id.0, event_seed
                                    )),
                                    task_id: Some(task_id.clone()),
                                    repo_id: service
                                        .task(task_id)
                                        .ok()
                                        .flatten()
                                        .map(|t| t.repo_id),
                                    at: now,
                                    kind: EventKind::RestackConflict,
                                    },
                                );

                                if let Some(retry_state) =
                                    schedule_restack_retry(daemon_state, task_id, now)
                                {
                                    let _ = service.transition_task_state(
                                        task_id,
                                        TaskState::Ready,
                                        EventId(format!(
                                            "E-RESTACK-RETRY-WAIT-{}-{}",
                                            task_id.0, event_seed
                                        )),
                                        now,
                                    );
                                    eprintln!(
                                        "[daemon] Restack conflict for {}; retry {}/{} in {}s",
                                        task_id.0,
                                        retry_state.attempts,
                                        retry_state.max_retries,
                                        retry_state.backoff_secs
                                    );
                                } else {
                                    let final_msg = format!(
                                        "restack conflict retries exhausted for {} (max retries: {})",
                                        task_id.0, RESTACK_RETRY_MAX_RETRIES
                                    );
                                    if let Some(pipeline) =
                                        daemon_state.pipelines.get_mut(&task_id.0)
                                    {
                                        pipeline.fail(final_msg.clone());
                                    }
                                    daemon_state.restack_retries.remove(&task_id.0);
                                    daemon_state.pipelines.remove(&task_id.0);
                                    stop_task_with_failure_reason(
                                        service,
                                        daemon_state.notification_dispatcher.as_ref(),
                                        task_id,
                                        &final_msg,
                                        now,
                                    );
                                    eprintln!("[daemon] {}", final_msg);
                                }
                            } else {
                                let retry_model = service
                                    .task(task_id)
                                    .ok()
                                    .flatten()
                                    .and_then(|t| t.preferred_model)
                                    .or_else(|| config.enabled_models.first().copied())
                                    .unwrap_or(ModelKind::Claude);
                                if let Some(pipeline) = daemon_state.pipelines.get_mut(&task_id.0) {
                                    pipeline.fail(err_msg.clone());
                                }
                                match apply_retry_transition(
                                    service,
                                    daemon_state.notification_dispatcher.as_ref(),
                                    task_id,
                                    retry_model,
                                    &err_msg,
                                    now,
                                ) {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        eprintln!(
                                            "[daemon] Restack failed for {}; retries exhausted",
                                            task_id.0
                                        );
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "[daemon] Restack failure retry handling failed for {}: {}",
                                            task_id.0, e
                                        );
                                    }
                                }
                                daemon_state.pipelines.remove(&task_id.0);
                                daemon_state.restack_retries.remove(&task_id.0);
                            }
                        }
                    }
                }
                PipelineAction::Submit {
                    task_id,
                    worktree_path,
                    mode,
                } => {
                    let seed = now.timestamp_nanos_opt().unwrap_or_default();
                    if let Err(e) = service.start_submit(
                        task_id,
                        *mode,
                        EventId(format!("E-SUBMIT-START-{}-{seed}", task_id.0)),
                        now,
                    ) {
                        eprintln!("[daemon] Failed to mark {} submitting: {}", task_id.0, e);
                        continue;
                    }

                    let graphite = GraphiteClient::new(worktree_path.clone());

                    // Ensure agent changes are committed before submit. This captures
                    // untracked/modified files in the task branch so "merged" state
                    // actually reflects landed content.
                    if worktree_has_uncommitted_changes(worktree_path) {
                        let message = format!("task {}: save pending changes", task_id.0);
                        if let Err(error) = graphite.commit_pending(&message) {
                            let err_msg = format!("graphite commit pending failed: {error}");
                            let retry_model = service
                                .task(task_id)
                                .ok()
                                .flatten()
                                .and_then(|t| t.preferred_model)
                                .or_else(|| config.enabled_models.first().copied())
                                .unwrap_or(ModelKind::Claude);
                            if let Some(pipeline) = daemon_state.pipelines.get_mut(&task_id.0) {
                                pipeline.fail(err_msg.clone());
                            }
                            match apply_retry_transition(
                                service,
                                daemon_state.notification_dispatcher.as_ref(),
                                task_id,
                                retry_model,
                                &err_msg,
                                now,
                            ) {
                                Ok(true) => {}
                                Ok(false) => {
                                    eprintln!(
                                        "[daemon] Commit pending failed for {}; retries exhausted",
                                        task_id.0
                                    );
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[daemon] Commit pending retry handling failed for {}: {}",
                                        task_id.0, e
                                    );
                                }
                            }
                            daemon_state.pipelines.remove(&task_id.0);
                            daemon_state.restack_retries.remove(&task_id.0);
                            continue;
                        }
                    }

                    // Fetch latest trunk before submitting to avoid
                    // "trunk branch is out of date" errors from Graphite.
                    let _ = Command::new("git")
                        .args(["fetch", "origin"])
                        .current_dir(worktree_path)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();

                    match graphite.submit(*mode) {
                        Ok(()) => {
                            if let Err(e) = service.complete_submit(
                                task_id,
                                format!("graphite://submit/{}", task_id.0),
                                0,
                                EventId(format!("E-SUBMIT-DONE-{}-{seed}", task_id.0)),
                                now,
                            ) {
                                eprintln!(
                                    "[daemon] Submit succeeded but state update failed for {}: {}",
                                    task_id.0, e
                                );
                            } else if let Some(pipeline) =
                                daemon_state.pipelines.get_mut(&task_id.0)
                            {
                                pipeline.advance();
                            }
                        }
                        Err(error) => {
                            let err_msg = format!("graphite submit failed: {error}");

                            if error.is_auth_failure() {
                                let reason = format!(
                                    "{err_msg}. Fix once globally with: gt auth --token <token>"
                                );
                                eprintln!(
                                    "[daemon] Submit auth failed for {}; stopping without retries",
                                    task_id.0
                                );
                                stop_task_with_failure_reason(
                                    service,
                                    daemon_state.notification_dispatcher.as_ref(),
                                    task_id,
                                    &reason,
                                    now,
                                );
                                if let Some(pipeline) = daemon_state.pipelines.get_mut(&task_id.0) {
                                    pipeline.fail(reason);
                                }
                                daemon_state.pipelines.remove(&task_id.0);
                                daemon_state.restack_retries.remove(&task_id.0);
                                continue;
                            }

                            if error.is_trunk_outdated_failure() {
                                let reason = format!(
                                    "{err_msg}. Run gt sync (or git pull --rebase on trunk) and retry this task"
                                );
                                eprintln!(
                                    "[daemon] Submit trunk-sync failed for {}; stopping without retries",
                                    task_id.0
                                );
                                stop_task_with_failure_reason(
                                    service,
                                    daemon_state.notification_dispatcher.as_ref(),
                                    task_id,
                                    &reason,
                                    now,
                                );
                                if let Some(pipeline) = daemon_state.pipelines.get_mut(&task_id.0) {
                                    pipeline.fail(reason);
                                }
                                daemon_state.pipelines.remove(&task_id.0);
                                daemon_state.restack_retries.remove(&task_id.0);
                                continue;
                            }

                            let retry_model = service
                                .task(task_id)
                                .ok()
                                .flatten()
                                .and_then(|t| t.preferred_model)
                                .or_else(|| config.enabled_models.first().copied())
                                .unwrap_or(ModelKind::Claude);
                            if let Some(pipeline) = daemon_state.pipelines.get_mut(&task_id.0) {
                                pipeline.fail(err_msg.clone());
                            }
                            match apply_retry_transition(
                                service,
                                daemon_state.notification_dispatcher.as_ref(),
                                task_id,
                                retry_model,
                                &err_msg,
                                now,
                            ) {
                                Ok(true) => {}
                                Ok(false) => {
                                    eprintln!(
                                        "[daemon] Submit failed for {}; retries exhausted",
                                        task_id.0
                                    );
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[daemon] Submit failure retry handling failed for {}: {}",
                                        task_id.0, e
                                    );
                                }
                            }
                            daemon_state.pipelines.remove(&task_id.0);
                            daemon_state.restack_retries.remove(&task_id.0);
                        }
                    }
                }
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
                }
            }
            DaemonAction::TriggerContextRegen => {
                if should_regenerate(
                    &daemon_state.context_gen,
                    &config.context_gen_config,
                    Utc::now(),
                ) {
                    let prompt = build_context_gen_prompt(&config.repo_root, &config.template_dir);
                    if let Err(e) = spawn_context_gen(
                        &config.repo_root,
                        &prompt,
                        config.context_gen_config.model,
                        &mut daemon_state.context_gen,
                    ) {
                        eprintln!("[daemon] Failed to spawn context gen: {e}");
                    } else {
                        eprintln!("[daemon] Background context regeneration started");
                        let event = Event {
                            id: EventId(format!(
                                "E-CTX-REGEN-START-{}",
                                now.timestamp_nanos_opt().unwrap_or_default()
                            )),
                            task_id: None,
                            repo_id: None,
                            at: now,
                            kind: EventKind::ContextRegenStarted,
                        };
                        let _ = record_event_with_notification(
                            service,
                            daemon_state.notification_dispatcher.as_ref(),
                            &event,
                        );
                    }
                }
            }
            DaemonAction::ContextRegenCompleted { success } => {
                let event = Event {
                    id: EventId(format!(
                        "E-CTX-REGEN-DONE-{}",
                        now.timestamp_nanos_opt().unwrap_or_default()
                    )),
                    task_id: None,
                    repo_id: None,
                    at: now,
                    kind: EventKind::ContextRegenCompleted { success: *success },
                };
                let _ = record_event_with_notification(
                    service,
                    daemon_state.notification_dispatcher.as_ref(),
                    &event,
                );
            }
            DaemonAction::SpawnQA { task_id, qa_type } => {
                if config.dry_run {
                    eprintln!("[dry-run] Would spawn QA {} for {}", qa_type, task_id.0);
                    continue;
                }
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
                    if let Err(e) = spawn_qa_agent(&cwd, &prompt, model, &mut qa_state) {
                        eprintln!(
                            "[daemon] Failed to spawn QA {} for {}: {}",
                            qa_type, task_id.0, e
                        );
                    } else {
                        eprintln!("[daemon] QA {} started for {}", qa_type, task_id.0);
                        daemon_state.qa_agents.insert(task_id.0.clone(), qa_state);

                        // Record event.
                        let event = Event {
                            id: EventId(format!("E-QA-{}-{}-{}", qa_type, task_id.0, now.timestamp_nanos_opt().unwrap_or_default())),
                            task_id: Some(task_id.clone()),
                            repo_id: None,
                            at: now,
                            kind: EventKind::QAStarted {
                                qa_type: qa_type.to_string(),
                            },
                        };
                        let _ = record_event_with_notification(
                            service,
                            daemon_state.notification_dispatcher.as_ref(),
                            &event,
                        );
                    }
                }
            }
            DaemonAction::QACompleted { task_id, result } => {
                if config.dry_run {
                    eprintln!(
                        "[dry-run] Would mark QA completed for {} ({}/{})",
                        task_id.0, result.summary.passed, result.summary.total
                    );
                    continue;
                }
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
                    id: EventId(format!("E-QA-DONE-{}-{}", task_id.0, now.timestamp_nanos_opt().unwrap_or_default())),
                    task_id: Some(task_id.clone()),
                    repo_id: None,
                    at: now,
                    kind: EventKind::QACompleted {
                        passed: result.summary.passed,
                        failed: result.summary.failed,
                        total: result.summary.total,
                    },
                };
                let _ = record_event_with_notification(
                    service,
                    daemon_state.notification_dispatcher.as_ref(),
                    &event,
                );
            }
            DaemonAction::QAFailed { task_id, result } => {
                if config.dry_run {
                    eprintln!("[dry-run] Would record QA failure for {}", task_id.0);
                    continue;
                }
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
                    id: EventId(format!("E-QA-FAIL-{}-{}", task_id.0, now.timestamp_nanos_opt().unwrap_or_default())),
                    task_id: Some(task_id.clone()),
                    repo_id: None,
                    at: now,
                    kind: EventKind::QAFailed { failures },
                };
                let _ = record_event_with_notification(
                    service,
                    daemon_state.notification_dispatcher.as_ref(),
                    &event,
                );
            }
            DaemonAction::Log { message } => {
                println!("{}", message);
            }
            DaemonAction::EmitEvent {
                task_id,
                repo_id,
                kind,
            } => {
                let event = Event {
                    id: EventId(format!(
                        "E-EVENT-{}",
                        now.timestamp_nanos_opt().unwrap_or_default()
                    )),
                    task_id: task_id.clone(),
                    repo_id: repo_id.clone(),
                    at: now,
                    kind: kind.clone(),
                };
                if let Err(e) = record_event_with_notification(
                    service,
                    daemon_state.notification_dispatcher.as_ref(),
                    &event,
                ) {
                    eprintln!("[daemon] Failed to record emitted event: {}", e);
                }
            }
            DaemonAction::ShutdownComplete => {
                eprintln!("[daemon] Daemon shutdown complete");
                if !config.dry_run {
                    let interrupted_reason = "interrupted by daemon shutdown";
                    if let Ok(open_runs) = service.store.list_open_runs() {
                        for run in open_runs {
                            let task_id = run.task_id.clone();
                            if let Err(err) = service.store.finish_open_runs_for_task(
                                &task_id,
                                now,
                                "interrupted",
                                None,
                                None,
                            ) {
                                eprintln!(
                                    "[daemon] Failed to finish open run for {}: {}",
                                    task_id.0, err
                                );
                            }

                            if let Ok(Some(mut task)) = service.task(&task_id) {
                                if matches!(
                                    task.state,
                                    TaskState::Chatting
                                        | TaskState::Ready
                                        | TaskState::Submitting
                                        | TaskState::Restacking
                                        | TaskState::AwaitingMerge
                                ) {
                                    task.state = TaskState::Stopped;
                                    task.updated_at = now;
                                    task.last_failure_reason =
                                        Some(interrupted_reason.to_string());
                                    if let Err(err) = service.store.upsert_task(&task) {
                                        eprintln!(
                                            "[daemon] Failed to persist interrupted task {}: {}",
                                            task_id.0, err
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                should_exit = true;
            }
        }
    }

    should_exit
}

/// Convenience: run a single daemon tick and execute its actions.
pub fn run_tick(
    service: &OrchdService,
    supervisor: &mut AgentSupervisor,
    daemon_state: &mut DaemonState,
    config: &DaemonConfig,
) -> bool {
    let actions = daemon_tick(service, supervisor, daemon_state, config);
    execute_actions(&actions, service, supervisor, daemon_state, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::JsonlEventLog;
    use crate::persistence::SqliteStore;
    use crate::scheduler::{Scheduler, SchedulerConfig};
    use crate::supervisor::AgentSession;
    use chrono::Duration;
    use orch_core::events::{Event, EventKind};
    use orch_core::types::{EventId, ModelKind, RepoId, SubmitMode, Task, TaskId};
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration as StdDuration;

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
            skip_qa: false,
            skip_context_regen: false,
            dry_run: false,
            agent_timeout_secs: 1_800,
            drain_timeout_secs: 30,
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

    fn insert_sleep_session(supervisor: &mut AgentSupervisor, task_id: &TaskId) {
        let child = Command::new("sleep")
            .arg("60")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep session");
        let (_tx, rx) = mpsc::channel();
        supervisor.insert_session_for_test(AgentSession {
            child,
            output_rx: rx,
            input_tx: None,
            task_id: task_id.clone(),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            timeout: StdDuration::from_secs(1_800),
            patch_ready: false,
            needs_human: false,
            signal_at: None,
        });
    }

    fn sample_org_config_toml(tick_interval_secs: u64, agent_timeout_secs: u64) -> String {
        format!(
            r#"
[models]
enabled = ["claude", "codex", "gemini"]

[concurrency]
per_repo = 10
claude = 10
codex = 10
gemini = 10

[graphite]
auto_submit = true
submit_mode_default = "single"
allow_move = "manual"

[ui]
web_bind = "127.0.0.1:9842"

[notifications]
enabled = false
stdout = true

[daemon]
tick_interval_secs = {tick_interval_secs}
agent_timeout_secs = {agent_timeout_secs}
"#
        )
    }

    fn sample_org_config_with_budget_toml(
        tick_interval_secs: u64,
        agent_timeout_secs: u64,
        daily_token_limit: u64,
        monthly_token_limit: u64,
    ) -> String {
        format!(
            r#"
[models]
enabled = ["claude", "codex", "gemini"]

[concurrency]
per_repo = 10
claude = 10
codex = 10
gemini = 10

[graphite]
auto_submit = true
submit_mode_default = "single"
allow_move = "manual"

[ui]
web_bind = "127.0.0.1:9842"

[notifications]
enabled = false
stdout = true

[daemon]
tick_interval_secs = {tick_interval_secs}
agent_timeout_secs = {agent_timeout_secs}

[budget]
enabled = true
daily_token_limit = {daily_token_limit}
monthly_token_limit = {monthly_token_limit}
"#
        )
    }

    fn init_git_repo_with_commit() -> (PathBuf, String) {
        let repo = std::env::temp_dir().join(format!(
            "othala-daemon-git-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&repo).expect("create repo dir");
        fs::write(repo.join("README.md"), "# test\n").expect("write readme");

        let init_status = Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .status()
            .expect("git init");
        assert!(init_status.success(), "git init should succeed");

        let add_status = Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&repo)
            .status()
            .expect("git add");
        assert!(add_status.success(), "git add should succeed");

        let commit_status = Command::new("git")
            .args([
                "-c",
                "user.name=Othala Tests",
                "-c",
                "user.email=tests@example.com",
                "commit",
                "-m",
                "initial",
            ])
            .current_dir(&repo)
            .status()
            .expect("git commit");
        assert!(commit_status.success(), "git commit should succeed");

        let sha = get_worktree_head_sha(&repo).expect("head sha");
        (repo, sha)
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
            duration_secs: 5,
        };

        let mut daemon_state = DaemonState::new();
        let actions = handle_agent_completion(
            &service,
            None,
            &outcome,
            &config,
            &mut daemon_state,
            Utc::now(),
        );
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
            duration_secs: 5,
        };

        let mut daemon_state = DaemonState::new();
        let actions = handle_agent_completion(
            &service,
            None,
            &outcome,
            &config,
            &mut daemon_state,
            Utc::now(),
        );
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
        assert!(state.restack_retries.is_empty());
        assert!(state.config_last_modified.is_none());
    }

    #[test]
    fn check_budget_passes_when_disabled() {
        let mut state = DaemonState::new();
        state.budget_used_today = u64::MAX;
        state.budget_used_month = u64::MAX;
        let config = BudgetConfig {
            enabled: false,
            daily_token_limit: 1,
            monthly_token_limit: 1,
        };

        assert!(check_budget(&state, &config));
    }

    #[test]
    fn check_budget_fails_when_daily_exceeded() {
        let mut state = DaemonState::new();
        state.budget_used_today = 100;
        state.budget_used_month = 10;
        let config = BudgetConfig {
            enabled: true,
            daily_token_limit: 100,
            monthly_token_limit: 1_000,
        };

        assert!(!check_budget(&state, &config));
    }

    #[test]
    fn check_budget_fails_when_monthly_exceeded() {
        let mut state = DaemonState::new();
        state.budget_used_today = 10;
        state.budget_used_month = 200;
        let config = BudgetConfig {
            enabled: true,
            daily_token_limit: 1_000,
            monthly_token_limit: 200,
        };

        assert!(!check_budget(&state, &config));
    }

    #[test]
    fn maybe_reset_budget_resets_on_new_day() {
        let now = Utc::now();
        let mut state = DaemonState::new();
        state.budget_used_today = 123;
        state.budget_last_reset_day = Some(if now.day() == 1 { 2 } else { now.day() - 1 });

        maybe_reset_budget(&mut state);

        assert_eq!(state.budget_used_today, 0);
        assert_eq!(state.budget_last_reset_day, Some(now.day()));
    }

    #[test]
    fn daemon_tick_refuses_spawn_when_budget_exceeded() {
        let service = mk_service();
        let repo_root = std::env::temp_dir().join(format!(
            "othala-budget-exceeded-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(repo_root.join(".othala")).expect("create .othala dir");
        fs::write(
            repo_root.join(".othala/config.toml"),
            sample_org_config_with_budget_toml(2, 1_800, 1, 1),
        )
        .expect("write org config");

        let mut config = mk_config();
        config.repo_root = repo_root.clone();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();
        daemon_state.budget_used_today = 1;
        daemon_state.budget_used_month = 1;
        daemon_state.budget_last_reset_day = Some(Utc::now().day());
        daemon_state.budget_last_reset_month = Some(Utc::now().month());

        let task = mk_task("T-BUDGET-1");
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, DaemonAction::SpawnAgent { .. })));
        assert!(actions.iter().any(|a| {
            matches!(
                a,
                DaemonAction::EmitEvent {
                    kind: EventKind::BudgetExceeded,
                    ..
                }
            )
        }));

        fs::remove_dir_all(repo_root).ok();
    }

    #[test]
    fn handle_agent_completion_updates_budget_usage_from_output() {
        let service = mk_service();
        let config = mk_config();
        let task = mk_task("T-BUDGET-2");
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let mut daemon_state = DaemonState::new();
        daemon_state
            .budget_output_chars_by_task
            .insert(task.id.0.clone(), 80);

        let outcome = AgentOutcome {
            task_id: task.id.clone(),
            model: ModelKind::Claude,
            exit_code: Some(0),
            patch_ready: true,
            needs_human: false,
            success: true,
            duration_secs: 1,
        };

        let _ = handle_agent_completion(
            &service,
            None,
            &outcome,
            &config,
            &mut daemon_state,
            Utc::now(),
        );

        assert_eq!(daemon_state.budget_used_today, 20);
        assert_eq!(daemon_state.budget_used_month, 20);
        assert!(!daemon_state
            .budget_output_chars_by_task
            .contains_key(&task.id.0));
    }

    #[test]
    fn request_shutdown_sets_deadline() {
        let mut state = DaemonState::new();
        state.request_shutdown(30);
        assert!(state.shutdown_requested);
        assert!(state.shutdown_deadline.is_some());
    }

    #[test]
    fn daemon_tick_skips_spawn_during_shutdown() {
        let service = mk_service();
        let config = mk_config();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        let task = mk_task("T-SD-1");
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        daemon_state.request_shutdown(30);
        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);

        assert!(!actions
            .iter()
            .any(|a| matches!(a, DaemonAction::SpawnAgent { .. })));
    }

    #[test]
    fn shutdown_complete_when_no_agents_running() {
        let service = mk_service();
        let config = mk_config();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        daemon_state.request_shutdown(30);
        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);

        assert!(actions
            .iter()
            .any(|a| matches!(a, DaemonAction::ShutdownComplete)));
    }

    #[test]
    fn shutdown_waits_for_running_agents() {
        let service = mk_service();
        let config = mk_config();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();
        let task_id = TaskId::new("T-SD-2");

        insert_sleep_session(&mut supervisor, &task_id);
        daemon_state.request_shutdown(30);
        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);

        assert!(!actions
            .iter()
            .any(|a| matches!(a, DaemonAction::ShutdownComplete)));
        assert!(supervisor.has_session(&task_id));
        supervisor.stop_all();
    }

    #[test]
    fn drain_timeout_forces_shutdown() {
        let service = mk_service();
        let config = mk_config();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        insert_sleep_session(&mut supervisor, &TaskId::new("T-SD-3"));
        daemon_state.request_shutdown(0);
        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);

        assert!(actions
            .iter()
            .any(|a| matches!(a, DaemonAction::ShutdownComplete)));
        assert_eq!(supervisor.running_count(), 0);
    }

    #[test]
    fn config_reload_detects_change() {
        let mut daemon_state = DaemonState::new();
        let config_dir = std::env::temp_dir().join(format!(
            "othala-config-reload-detects-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&config_dir).expect("create config dir");
        let config_path = config_dir.join("config.toml");

        fs::write(&config_path, sample_org_config_toml(2, 1800)).expect("write initial config");
        let first = check_config_reload(&config_path, &mut daemon_state);
        assert!(first.is_some(), "initial load should reload config");

        thread::sleep(StdDuration::from_millis(1100));
        fs::write(&config_path, sample_org_config_toml(7, 90)).expect("write updated config");
        let second = check_config_reload(&config_path, &mut daemon_state).expect("config reload");
        assert_eq!(second.daemon.tick_interval_secs, 7);
        assert_eq!(second.daemon.agent_timeout_secs, 90);

        fs::remove_dir_all(config_dir).ok();
    }

    #[test]
    fn config_reload_skips_unchanged() {
        let mut daemon_state = DaemonState::new();
        let config_dir = std::env::temp_dir().join(format!(
            "othala-config-reload-unchanged-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&config_dir).expect("create config dir");
        let config_path = config_dir.join("config.toml");

        fs::write(&config_path, sample_org_config_toml(2, 1800)).expect("write initial config");
        let first = check_config_reload(&config_path, &mut daemon_state);
        assert!(first.is_some(), "first read should load config");

        let second = check_config_reload(&config_path, &mut daemon_state);
        assert!(second.is_none(), "unchanged mtime should skip reload");

        fs::remove_dir_all(config_dir).ok();
    }

    #[test]
    fn config_reload_handles_missing_file() {
        let mut daemon_state = DaemonState::new();
        let missing_path = std::env::temp_dir().join(format!(
            "othala-config-reload-missing-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));

        let result = check_config_reload(&missing_path, &mut daemon_state);
        assert!(result.is_none());
    }

    #[test]
    fn restack_retry_state_uses_exponential_backoff_and_max_retries() {
        let mut daemon_state = DaemonState::new();
        let task_id = TaskId::new("T-RS-1");
        let t0 = Utc::now();

        let first = schedule_restack_retry(&mut daemon_state, &task_id, t0).expect("first retry");
        assert_eq!(first.attempts, 1);
        assert_eq!(first.max_retries, 3);
        assert_eq!(first.backoff_secs, 5);

        let second = schedule_restack_retry(&mut daemon_state, &task_id, t0 + Duration::seconds(1))
            .expect("second retry");
        assert_eq!(second.attempts, 2);
        assert_eq!(second.backoff_secs, 10);

        let third = schedule_restack_retry(&mut daemon_state, &task_id, t0 + Duration::seconds(2))
            .expect("third retry");
        assert_eq!(third.attempts, 3);
        assert_eq!(third.backoff_secs, 20);

        let exhausted =
            schedule_restack_retry(&mut daemon_state, &task_id, t0 + Duration::seconds(3));
        assert!(exhausted.is_none());
        assert!(!daemon_state.restack_retries.contains_key(&task_id.0));
    }

    #[test]
    fn daemon_tick_defers_stack_action_while_restack_backoff_is_pending() {
        let service = mk_service();
        let config = mk_config();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();
        let task_id = TaskId::new("T-RS-2");

        let mut pipeline = PipelineState::new(
            task_id.clone(),
            "task/T-RS-2".to_string(),
            PathBuf::from(".orch/wt/T-RS-2"),
            SubmitMode::Single,
            Some("task/T-parent".to_string()),
        );
        pipeline.stage = PipelineStage::StackOnParent;
        daemon_state.pipelines.insert(task_id.0.clone(), pipeline);
        daemon_state.restack_retries.insert(
            task_id.0.clone(),
            RestackRetryState {
                attempts: 1,
                max_retries: 3,
                last_attempt: Utc::now(),
                backoff_secs: 60,
            },
        );

        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);
        assert!(!actions.iter().any(|action| {
            matches!(
                action,
                DaemonAction::ExecutePipeline {
                    action: PipelineAction::StackOnParent { task_id: id, .. }
                } if id.0 == task_id.0
            )
        }));
    }

    #[test]
    fn daemon_tick_releases_stack_action_when_restack_backoff_elapsed() {
        let service = mk_service();
        let config = mk_config();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();
        let task_id = TaskId::new("T-RS-3");

        let mut pipeline = PipelineState::new(
            task_id.clone(),
            "task/T-RS-3".to_string(),
            PathBuf::from(".orch/wt/T-RS-3"),
            SubmitMode::Single,
            Some("task/T-parent".to_string()),
        );
        pipeline.stage = PipelineStage::StackOnParent;
        daemon_state.pipelines.insert(task_id.0.clone(), pipeline);
        daemon_state.restack_retries.insert(
            task_id.0.clone(),
            RestackRetryState {
                attempts: 1,
                max_retries: 3,
                last_attempt: Utc::now() - Duration::seconds(10),
                backoff_secs: 5,
            },
        );

        let actions = daemon_tick(&service, &mut supervisor, &mut daemon_state, &config);
        assert!(actions.iter().any(|action| {
            matches!(
                action,
                DaemonAction::ExecutePipeline {
                    action: PipelineAction::StackOnParent { task_id: id, .. }
                } if id.0 == task_id.0
            )
        }));
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
            skip_qa: false,
            skip_context_regen: false,
            dry_run: false,
            agent_timeout_secs: 1_800,
            drain_timeout_secs: 30,
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
            .filter(|a| {
                matches!(
                    a,
                    DaemonAction::SpawnQA {
                        qa_type: QAType::Baseline,
                        ..
                    }
                )
            })
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
        assert_eq!(
            qa_spawns.len(),
            0,
            "should NOT spawn QA when one already running"
        );

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
            duration_secs: 5,
        };

        let mut daemon_state = DaemonState::new();
        let actions = handle_agent_completion(
            &service,
            None,
            &outcome,
            &config,
            &mut daemon_state,
            Utc::now(),
        );

        // Should spawn QA Validation, NOT MarkReady.
        assert!(actions.iter().any(|a| matches!(
            a,
            DaemonAction::SpawnQA {
                qa_type: QAType::Validation,
                ..
            }
        )));
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
        daemon_state.qa_agents.insert("T-fail".to_string(), failed);

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
            duration_secs: 5,
        };

        let mut daemon_state = DaemonState::new();
        let actions = handle_agent_completion(
            &service,
            None,
            &outcome,
            &config,
            &mut daemon_state,
            Utc::now(),
        );
        assert!(actions
            .iter()
            .any(|a| matches!(a, DaemonAction::RecordNeedsHuman { .. })));
    }

    #[test]
    fn check_pr_merged_parses_state() {
        assert!(is_gh_pr_state_merged(b"MERGED\n"));
        assert!(!is_gh_pr_state_merged(b"OPEN\n"));
        assert!(!is_gh_pr_state_merged(b"CLOSED\n"));
    }

    #[test]
    fn mark_merged_action_transitions_state() {
        let service = mk_service();
        let config = mk_config();
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        let mut task = mk_task("T-MERGE-1");
        task.state = TaskState::AwaitingMerge;
        task.pr = Some(orch_core::types::PullRequestRef {
            number: 42,
            url: "https://example.test/pr/42".to_string(),
            draft: false,
        });
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let actions = vec![DaemonAction::MarkMerged {
            task_id: task.id.clone(),
        }];
        execute_actions(&actions, &service, &mut supervisor, &mut daemon_state, &config);

        let updated = service.task(&task.id).expect("load task").expect("task exists");
        assert_eq!(updated.state, TaskState::Merged);
    }

    #[test]
    fn build_spawn_action_injects_qa_failure_context_on_retry() {
        let config = mk_config();

        let mut task = mk_task("T-QA-RETRY");
        task.retry_count = 1;
        task.max_retries = 3;
        task.preferred_model = Some(ModelKind::Claude);
        task.last_failure_reason = Some(
            "## QA Failures (from previous attempt)\n\n\
             - startup.banner: PASS\n\
             - tui.create_chat: FAIL — branch not created\n\n\
             Fix the failing tests before signaling [patch_ready].\n"
                .to_string(),
        );

        let action = build_spawn_action(&task, &config).expect("should produce action");
        match action {
            DaemonAction::SpawnAgent { prompt, .. } => {
                assert!(
                    prompt.contains("QA Failures"),
                    "prompt should contain QA failure context section"
                );
                assert!(
                    prompt.contains("tui.create_chat: FAIL"),
                    "prompt should contain specific failure details"
                );
            }
            other => panic!("expected SpawnAgent, got {:?}", other),
        }
    }

    #[test]
    fn build_spawn_action_no_qa_context_for_non_qa_failure() {
        let config = mk_config();

        let mut task = mk_task("T-PLAIN-RETRY");
        task.retry_count = 1;
        task.max_retries = 3;
        task.preferred_model = Some(ModelKind::Claude);
        task.last_failure_reason = Some("cargo test failed: assertion error".to_string());

        let action = build_spawn_action(&task, &config).expect("should produce action");
        match action {
            DaemonAction::SpawnAgent { prompt, .. } => {
                // Should include retry context but NOT a separate QA Failures section.
                assert!(
                    prompt.contains("assertion error"),
                    "prompt should contain the failure reason in retry context"
                );
                // The QA Failures section header should NOT appear as a standalone section.
                // (It may appear in retry context, but not as a dedicated section.)
                assert!(
                    !prompt.contains("## QA Failures"),
                    "prompt should not have a standalone QA failure section for non-QA failures"
                );
            }
            other => panic!("expected SpawnAgent, got {:?}", other),
        }
    }

    #[test]
    fn get_worktree_head_sha_returns_value_for_git_repo() {
        let (repo, sha) = init_git_repo_with_commit();
        let detected = get_worktree_head_sha(&repo).expect("head sha");
        assert_eq!(detected, sha);
        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn dry_run_skips_agent_spawn() {
        let service = mk_service();
        let mut config = mk_config();
        config.dry_run = true;
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        let task = mk_task("T-DRY-1");
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let task_id = task.id.clone();
        let actions = vec![DaemonAction::SpawnAgent {
            task_id: task_id.clone(),
            model: ModelKind::Claude,
            prompt: "dry-run agent".to_string(),
            worktree_path: task.worktree_path,
        }];

        execute_actions(&actions, &service, &mut supervisor, &mut daemon_state, &config);

        assert!(!supervisor.has_session(&task_id));
    }

    #[test]
    fn dry_run_still_logs() {
        let service = mk_service();
        let mut config = mk_config();
        config.dry_run = true;
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();

        let actions = vec![DaemonAction::Log {
            message: "dry-run-log-smoke".to_string(),
        }];

        execute_actions(&actions, &service, &mut supervisor, &mut daemon_state, &config);
    }

    #[test]
    fn execute_actions_skips_verify_on_cache_hit() {
        let service = mk_service();
        let (repo, sha) = init_git_repo_with_commit();
        let mut config = mk_config();
        config.verify_command = Some("touch SHOULD_NOT_EXIST".to_string());
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();
        let task_id = TaskId::new("T-VC-1");

        let mut task = mk_task("T-VC-1");
        task.state = TaskState::Ready;
        task.worktree_path = repo.clone();
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        daemon_state.verify_cache.insert(task_id.0.clone(), sha);
        daemon_state.pipelines.insert(
            task_id.0.clone(),
            PipelineState::new(
                task_id.clone(),
                "task/T-VC-1".to_string(),
                repo.clone(),
                SubmitMode::Single,
                None,
            ),
        );

        let actions = vec![DaemonAction::ExecutePipeline {
            action: PipelineAction::RunVerify {
                task_id: task_id.clone(),
                worktree_path: repo.clone(),
            },
        }];
        execute_actions(&actions, &service, &mut supervisor, &mut daemon_state, &config);

        let pipeline = daemon_state
            .pipelines
            .get(&task_id.0)
            .expect("pipeline exists");
        assert_eq!(pipeline.stage, PipelineStage::Submit);
        assert!(!repo.join("SHOULD_NOT_EXIST").exists());
        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn execute_actions_stores_verify_sha_on_success() {
        let service = mk_service();
        let (repo, sha) = init_git_repo_with_commit();
        let mut config = mk_config();
        config.verify_command = Some("touch VERIFY_RAN".to_string());
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();
        let task_id = TaskId::new("T-VC-2");

        let mut task = mk_task("T-VC-2");
        task.state = TaskState::Ready;
        task.worktree_path = repo.clone();
        task.preferred_model = Some(ModelKind::Claude);
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        daemon_state.pipelines.insert(
            task_id.0.clone(),
            PipelineState::new(
                task_id.clone(),
                "task/T-VC-2".to_string(),
                repo.clone(),
                SubmitMode::Single,
                None,
            ),
        );

        let actions = vec![DaemonAction::ExecutePipeline {
            action: PipelineAction::RunVerify {
                task_id: task_id.clone(),
                worktree_path: repo.clone(),
            },
        }];
        execute_actions(&actions, &service, &mut supervisor, &mut daemon_state, &config);

        assert_eq!(daemon_state.verify_cache.get(&task_id.0), Some(&sha));
        assert!(repo.join("VERIFY_RAN").exists());
        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn execute_actions_removes_verify_cache_on_failure() {
        let service = mk_service();
        let (repo, _) = init_git_repo_with_commit();
        let mut config = mk_config();
        config.verify_command = Some("false".to_string());
        let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
        let mut daemon_state = DaemonState::new();
        let task_id = TaskId::new("T-VC-3");

        let mut task = mk_task("T-VC-3");
        task.state = TaskState::Ready;
        task.worktree_path = repo.clone();
        task.preferred_model = Some(ModelKind::Claude);
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        daemon_state
            .verify_cache
            .insert(task_id.0.clone(), "stale-sha".to_string());
        daemon_state.pipelines.insert(
            task_id.0.clone(),
            PipelineState::new(
                task_id.clone(),
                "task/T-VC-3".to_string(),
                repo.clone(),
                SubmitMode::Single,
                None,
            ),
        );

        let actions = vec![DaemonAction::ExecutePipeline {
            action: PipelineAction::RunVerify {
                task_id: task_id.clone(),
                worktree_path: repo.clone(),
            },
        }];
        execute_actions(&actions, &service, &mut supervisor, &mut daemon_state, &config);

        assert!(!daemon_state.verify_cache.contains_key(&task_id.0));
        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn handle_agent_completion_invalidates_verify_cache_entry() {
        let service = mk_service();
        let config = mk_config();
        let task = mk_task("T-VC-4");
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create");

        let mut daemon_state = DaemonState::new();
        daemon_state
            .verify_cache
            .insert("T-VC-4".to_string(), "abc123".to_string());

        let outcome = AgentOutcome {
            task_id: TaskId::new("T-VC-4"),
            model: ModelKind::Claude,
            exit_code: Some(0),
            patch_ready: true,
            needs_human: false,
            success: true,
            duration_secs: 5,
        };

        let _ = handle_agent_completion(
            &service,
            None,
            &outcome,
            &config,
            &mut daemon_state,
            Utc::now(),
        );

        assert!(!daemon_state.verify_cache.contains_key("T-VC-4"));
    }
}
