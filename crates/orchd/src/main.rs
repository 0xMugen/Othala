use chrono::{Local, Utc};
use orch_agents::{
    default_adapter_for, detect_common_signal, probe_models, summarize_setup,
    validate_setup_selection, AgentSignalKind, EpochRequest, ModelSetupSelection, SetupError,
    SetupProbeConfig, SetupProbeReport, SetupSummary, ValidatedSetupSelection,
};
use orch_core::config::{
    apply_setup_selection_to_org_config, load_org_config, load_repo_config, save_org_config,
    ConfigError, MovePolicy, OrgConfig, RepoConfig, SetupApplyError,
};
use orch_core::events::{
    Event, EventKind, GraphiteHygieneReport, ReviewOutput, ReviewVerdict, TestAssessment,
};
use orch_core::state::{
    ReviewCapacityState, ReviewPolicy, ReviewStatus, TaskState, VerifyStatus, VerifyTier,
};
use orch_core::types::{EventId, Task, TaskId, TaskSpec};
use orch_core::types::{ModelKind, SubmitMode};
use orch_core::validation::{Validate, ValidationIssue, ValidationLevel};
use orch_git::{discover_repo, GitCli, GitError, WorktreeManager};
use orch_graphite::GraphiteClient;
use orch_tui::{
    effective_display_state, normalize_pane_line, run_tui_with_hook, AgentPane, AgentPaneStatus,
    TuiApp, TuiError, TuiEvent, UiAction,
};
use orchd::{
    ModelAvailability, OrchdService, RuntimeEngine, RuntimeError, Scheduler, SchedulerConfig,
    ServiceError,
};
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_ORG_CONFIG: &str = "config/org.toml";
const DEFAULT_REPOS_CONFIG_DIR: &str = "config/repos";
const DEFAULT_SQLITE_PATH: &str = ".orch/state.sqlite";
const DEFAULT_EVENT_LOG_ROOT: &str = ".orch/events";
const DEFAULT_TUI_TICK_MS: u64 = 250;
const TUI_ORCH_TICK_INTERVAL: Duration = Duration::from_secs(5);
const TUI_AGENT_TIMEOUT_SECS: u64 = 3600;
static TUI_EVENT_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TuiChatHistory {
    root: PathBuf,
}

impl TuiChatHistory {
    fn from_event_log_root(event_log_root: &Path) -> Self {
        let root = event_log_root
            .parent()
            .map(|parent| parent.join("chats"))
            .unwrap_or_else(|| PathBuf::from(".orch/chats"));
        Self { root }
    }

    fn ensure_layout(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)
    }

    fn load_lines(&self, task_id: &TaskId) -> io::Result<Vec<String>> {
        let path = self.task_log_path(task_id);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        reader.lines().collect()
    }

    fn append_lines(&self, task_id: &TaskId, lines: &[String]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        self.ensure_layout()?;
        let path = self.task_log_path(task_id);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        for line in lines {
            writeln!(file, "{line}")?;
        }
        Ok(())
    }

    fn task_log_path(&self, task_id: &TaskId) -> PathBuf {
        self.root
            .join(format!("{}.log", sanitize_task_id(&task_id.0)))
    }
}

fn sanitize_task_id(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "task".to_string()
    } else {
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunCliArgs {
    org_config_path: PathBuf,
    repos_config_dir: PathBuf,
    sqlite_path: PathBuf,
    event_log_root: PathBuf,
    once: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TuiCliArgs {
    tick_ms: u64,
    org_config_path: PathBuf,
    repos_config_dir: PathBuf,
    sqlite_path: PathBuf,
    event_log_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SetupCliArgs {
    org_config_path: PathBuf,
    enabled_models: Option<Vec<ModelKind>>,
    per_model_concurrency: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WizardCliArgs {
    org_config_path: PathBuf,
    per_model_concurrency: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CreateTaskCliArgs {
    org_config_path: PathBuf,
    repos_config_dir: PathBuf,
    sqlite_path: PathBuf,
    event_log_root: PathBuf,
    spec_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListTasksCliArgs {
    org_config_path: PathBuf,
    sqlite_path: PathBuf,
    event_log_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewApproveCliArgs {
    org_config_path: PathBuf,
    sqlite_path: PathBuf,
    event_log_root: PathBuf,
    task_id: TaskId,
    reviewer: ModelKind,
    verdict: ReviewVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliCommand {
    Run(RunCliArgs),
    Tui(TuiCliArgs),
    Setup(SetupCliArgs),
    Wizard(WizardCliArgs),
    CreateTask(CreateTaskCliArgs),
    ListTasks(ListTasksCliArgs),
    ReviewApprove(ReviewApproveCliArgs),
    Help(String),
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("{0}")]
    Args(String),
    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to load org config at {path}: {source}")]
    LoadConfig {
        path: PathBuf,
        #[source]
        source: ConfigError,
    },
    #[error("failed to save org config at {path}: {source}")]
    SaveConfig {
        path: PathBuf,
        #[source]
        source: ConfigError,
    },
    #[error("failed to read repo config directory {path}: {source}")]
    ReadRepoConfigDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read task spec file at {path}: {source}")]
    ReadTaskSpecFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse task spec json at {path}: {source}")]
    ParseTaskSpecJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("wizard requires an interactive terminal")]
    WizardNotInteractive,
    #[error("failed to read wizard input: {source}")]
    WizardRead {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write wizard output: {source}")]
    WizardWrite {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize task list as json: {source}")]
    SerializeTaskList {
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Setup(#[from] SetupError),
    #[error(transparent)]
    SetupApply(#[from] SetupApplyError),
    #[error("{0}")]
    InvalidConfig(String),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Tui(#[from] TuiError),
    #[error(transparent)]
    Web(#[from] std::io::Error),
}

fn main() {
    if let Err(err) = run() {
        eprintln!("othala startup failed: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), MainError> {
    let mut argv = env::args();
    let program = argv.next().unwrap_or_else(|| "othala".to_string());
    let command = parse_cli_args(argv.collect::<Vec<_>>(), &program)?;

    match command {
        CliCommand::Help(text) => {
            println!("{text}");
            Ok(())
        }
        CliCommand::Run(args) => run_daemon(args),
        CliCommand::Tui(args) => run_tui_command(args),
        CliCommand::Setup(args) => run_setup(args),
        CliCommand::Wizard(args) => run_wizard(args),
        CliCommand::CreateTask(args) => run_create_task(args),
        CliCommand::ListTasks(args) => run_list_tasks(args),
        CliCommand::ReviewApprove(args) => run_review_approve(args),
    }
}

fn run_tui_command(args: TuiCliArgs) -> Result<(), MainError> {
    ensure_parent_dir(&args.sqlite_path)?;
    ensure_dir(&args.event_log_root)?;
    let chat_history = TuiChatHistory::from_event_log_root(&args.event_log_root);
    if let Err(source) = chat_history.ensure_layout() {
        return Err(MainError::CreateDir {
            path: chat_history.root.clone(),
            source,
        });
    }

    let org = load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
        path: args.org_config_path.clone(),
        source,
    })?;
    validate_org_config(&org.validate())?;

    let repo_configs = load_repo_configs(&args.repos_config_dir)?;
    validate_repo_configs(&repo_configs)?;
    run_startup_preflight(&repo_configs)?;

    let scheduler = Scheduler::new(SchedulerConfig::from_org_config(&org));
    let service = OrchdService::open(&args.sqlite_path, &args.event_log_root, scheduler)?;
    let runtime = RuntimeEngine::default();
    let repo_config_by_id = repo_configs
        .iter()
        .map(|(_, cfg)| (cfg.repo_id.clone(), cfg.clone()))
        .collect::<HashMap<_, _>>();
    let enabled_models = org.models.enabled.clone();
    let probe_config = SetupProbeConfig::default();

    let tasks = service.list_tasks()?;
    let mut app = TuiApp::from_tasks(&tasks);
    let restored_chat_count = hydrate_chat_panes_from_history(&mut app, &tasks, &chat_history);
    app.state.status_line = format!(
        "tui ready tasks={} chats={} tick_ms={} (daemon tick every {}s)",
        tasks.len(),
        restored_chat_count,
        args.tick_ms,
        TUI_ORCH_TICK_INTERVAL.as_secs()
    );
    let mut agent_supervisor = TuiAgentSupervisor::default();

    let mut last_orch_tick = Instant::now()
        .checked_sub(TUI_ORCH_TICK_INTERVAL)
        .unwrap_or_else(Instant::now);
    let tui_result = run_tui_with_hook(&mut app, Duration::from_millis(args.tick_ms), |app| {
        let actions = app.drain_actions();
        let mut force_tick = false;

        for action in actions {
            let at = Utc::now();
            match execute_tui_action(
                action.action,
                action.task_id.as_ref(),
                action.prompt.as_deref(),
                action.model,
                &service,
                &org,
                &enabled_models,
                &probe_config,
                &repo_config_by_id,
                &mut agent_supervisor,
                at,
            ) {
                Ok(outcome) => {
                    app.state.status_line = outcome.message;
                    force_tick |= outcome.force_tick;
                    for event in outcome.events {
                        app.apply_event(event);
                    }
                }
                Err(err) => {
                    app.state.status_line = format!("action failed: {err}");
                }
            }
        }

        let now = Instant::now();
        if force_tick || now.duration_since(last_orch_tick) >= TUI_ORCH_TICK_INTERVAL {
            let at = Utc::now();
            match run_single_orchestrator_tick(
                &service,
                &runtime,
                &org,
                &repo_config_by_id,
                &enabled_models,
                &probe_config,
                at,
            ) {
                Ok(status) => {
                    if let Some(status) = status {
                        app.state.status_line = status;
                    }
                }
                Err(err) => {
                    app.state.status_line = format!("daemon tick failed: {err}");
                }
            }
            last_orch_tick = now;
        }

        match service.list_tasks() {
            Ok(tasks) => app.set_tasks(&tasks),
            Err(err) => {
                app.state.status_line = format!("failed to refresh tasks: {err}");
            }
        }

        for event in agent_supervisor.drain_pending_ready_starts(
            &service,
            &repo_config_by_id,
            &enabled_models,
        ) {
            app.apply_event(event);
        }

        for event in agent_supervisor.poll_events(&chat_history) {
            app.apply_event(event);
        }

        let mut signal_tick_requested = false;

        // Bridge: agent signals -> task lifecycle actions.
        for task_id in agent_supervisor.drain_need_human_tasks() {
            let at = Utc::now();
            if let Some(stopped) = agent_supervisor.stop_task_session(&task_id) {
                apply_stopped_agent_session_event(app, stopped, "needs_human");
            }
            match service.task(&task_id) {
                Ok(Some(task)) => {
                    if !orchd::is_transition_allowed(task.state, TaskState::NeedsHuman) {
                        app.state.status_line = format!(
                            "needs-human signal ignored for {} from state {}",
                            task_id.0,
                            orchd::task_state_tag(task.state)
                        );
                        continue;
                    }
                    match service.mark_needs_human(
                        &task_id,
                        "agent requested human input",
                        orchd::MarkNeedsHumanEventIds {
                            needs_human_state_changed: tui_event_id(&task_id, "SIG-NH-S", at),
                            needs_human_event: tui_event_id(&task_id, "SIG-NH-E", at),
                        },
                        at,
                    ) {
                        Ok(_) => {
                            signal_tick_requested = true;
                            app.state.status_line =
                                format!("marked {} as NEEDS_HUMAN from agent signal", task_id.0);
                        }
                        Err(err) => {
                            app.state.status_line =
                                format!("failed to mark {} needs-human: {err}", task_id.0);
                        }
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    app.state.status_line = format!(
                        "failed to load task {} for needs-human signal: {err}",
                        task_id.0
                    );
                }
            }
        }

        for task_id in agent_supervisor.drain_patch_ready_tasks() {
            let at = Utc::now();
            if let Some(stopped) = agent_supervisor.stop_task_session(&task_id) {
                apply_stopped_agent_session_event(app, stopped, "patch_ready");
            }
            match service.task(&task_id) {
                Ok(Some(task)) => {
                    // If we can't go directly to Submitting, try routing through Running first.
                    if !orchd::is_transition_allowed(task.state, TaskState::Submitting) {
                        if orchd::is_transition_allowed(task.state, TaskState::Running)
                            && orchd::is_transition_allowed(
                                TaskState::Running,
                                TaskState::Submitting,
                            )
                        {
                            if let Err(err) = service.transition_task_state(
                                &task_id,
                                TaskState::Running,
                                tui_event_id(&task_id, "SIG-PR-RUNNING", at),
                                at,
                            ) {
                                app.state.status_line = format!(
                                    "patch-ready: failed to transition {} to running: {err}",
                                    task_id.0,
                                );
                                continue;
                            }
                        } else {
                            app.state.status_line = format!(
                                "patch-ready signal ignored for {} from state {}",
                                task_id.0,
                                orchd::task_state_tag(task.state)
                            );
                            continue;
                        }
                    }
                    let mode = resolve_submit_mode_for_task(&task, &repo_config_by_id);
                    match service.start_submit(
                        &task_id,
                        mode,
                        orchd::StartSubmitEventIds {
                            submit_state_changed: tui_event_id(&task_id, "SIG-SUBMIT-S", at),
                            submit_started: tui_event_id(&task_id, "SIG-SUBMIT-E", at),
                        },
                        at,
                    ) {
                        Ok(_) => {
                            signal_tick_requested = true;
                            app.state.status_line =
                                format!("pushing {} to graphite...", task_id.0);
                        }
                        Err(err) => {
                            app.state.status_line =
                                format!("failed to start submit for {}: {err}", task_id.0);
                        }
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    app.state.status_line =
                        format!("failed to load task {} for submit signal: {err}", task_id.0);
                }
            }
        }

        // Bridge: clean agent exit (without explicit signal) -> submit for speed.
        for task_id in agent_supervisor.drain_completed_tasks() {
            let at = Utc::now();
            match service.task(&task_id) {
                Ok(Some(task)) => {
                    if !orchd::is_transition_allowed(task.state, TaskState::Submitting) {
                        if orchd::is_transition_allowed(task.state, TaskState::Running)
                            && orchd::is_transition_allowed(
                                TaskState::Running,
                                TaskState::Submitting,
                            )
                        {
                            if let Err(err) = service.transition_task_state(
                                &task_id,
                                TaskState::Running,
                                tui_event_id(&task_id, "EXIT-RUNNING", at),
                                at,
                            ) {
                                app.state.status_line = format!(
                                    "agent-complete: failed to transition {} to running: {err}",
                                    task_id.0,
                                );
                                continue;
                            }
                        } else {
                            app.state.status_line = format!(
                                "agent-complete auto-submit ignored for {} from state {}",
                                task_id.0,
                                orchd::task_state_tag(task.state)
                            );
                            continue;
                        }
                    }
                    let mode = resolve_submit_mode_for_task(&task, &repo_config_by_id);
                    match service.start_submit(
                        &task_id,
                        mode,
                        orchd::StartSubmitEventIds {
                            submit_state_changed: tui_event_id(&task_id, "EXIT-SUBMIT-S", at),
                            submit_started: tui_event_id(&task_id, "EXIT-SUBMIT-E", at),
                        },
                        at,
                    ) {
                        Ok(_) => {
                            signal_tick_requested = true;
                            app.state.status_line = format!(
                                "agent completed for {}; auto-started graphite submit ({mode:?})",
                                task_id.0
                            );
                        }
                        Err(err) => {
                            app.state.status_line =
                                format!("agent-complete submit failed for {}: {err}", task_id.0);
                        }
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    app.state.status_line = format!(
                        "failed to load task {} for agent-complete submit: {err}",
                        task_id.0
                    );
                }
            }
        }

        // Bridge: conflict_resolved agent signal -> gt add -A + gt continue + resolve restack.
        for task_id in agent_supervisor.drain_conflict_resolved_tasks() {
            let at = Utc::now();
            if let Some(stopped) = agent_supervisor.stop_task_session(&task_id) {
                apply_stopped_agent_session_event(app, stopped, "conflict_resolved");
            }
            match service.task(&task_id) {
                Ok(Some(task)) => {
                    let repo_config = repo_config_by_id.get(&task.repo_id.0);
                    let resolve_ok = (|| -> Result<(), String> {
                        let repo_cfg = repo_config.ok_or_else(|| {
                            format!("missing repo config for repo_id={}", task.repo_id.0)
                        })?;
                        let runtime_path = resolve_task_runtime_path(&task, repo_cfg);
                        let gt = GraphiteClient::new(runtime_path);
                        gt.begin_conflict_resolution()
                            .map_err(|e| format!("gt add -A failed: {e}"))?;
                        gt.continue_conflict_resolution()
                            .map_err(|e| format!("gt continue failed: {e}"))?;
                        service
                            .resolve_restack_conflict(
                                &task_id,
                                orchd::ResolveRestackConflictEventIds {
                                    restack_state_changed: tui_event_id(
                                        &task_id,
                                        "CR-RESOLVE-S",
                                        at,
                                    ),
                                    restack_resolved: tui_event_id(&task_id, "CR-RESOLVE-E", at),
                                },
                                at,
                            )
                            .map_err(|e| format!("resolve_restack_conflict failed: {e}"))?;
                        Ok(())
                    })();
                    match resolve_ok {
                        Ok(()) => {
                            signal_tick_requested = true;
                            app.state.status_line =
                                format!("conflict resolved for {}; restack continuing", task_id.0);
                        }
                        Err(reason) => {
                            app.state.status_line = format!(
                                "conflict resolution post-steps failed for {}: {reason}",
                                task_id.0
                            );
                            let _ = service.mark_needs_human(
                                &task_id,
                                &format!("conflict resolution failed: {reason}"),
                                orchd::MarkNeedsHumanEventIds {
                                    needs_human_state_changed: tui_event_id(
                                        &task_id, "CR-NH-S", at,
                                    ),
                                    needs_human_event: tui_event_id(&task_id, "CR-NH-E", at),
                                },
                                at,
                            );
                        }
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    app.state.status_line = format!(
                        "failed to load task {} for conflict-resolved signal: {err}",
                        task_id.0
                    );
                }
            }
        }

        // Auto-trigger: spawn conflict resolution agents for RestackConflict tasks.
        if let Ok(tasks) = service.list_tasks() {
            for task in &tasks {
                if task.state == TaskState::RestackConflict
                    && !agent_supervisor.has_task_session(&task.id)
                {
                    if let Some(repo_config) = repo_config_by_id.get(&task.repo_id.0) {
                        match agent_supervisor.start_conflict_resolution_session(
                            task,
                            repo_config,
                            &enabled_models,
                        ) {
                            Ok(started) => {
                                app.state.status_line =
                                    format!("auto-started conflict resolution for {}", task.id.0);
                                app.apply_event(TuiEvent::AgentPaneOutput {
                                    instance_id: started.instance_id,
                                    task_id: started.task_id,
                                    model: started.model,
                                    lines: vec!["[conflict resolution agent started]".to_string()],
                                });
                            }
                            Err(err) => {
                                app.state.status_line = format!(
                                    "failed to auto-start conflict resolution for {}: {err}",
                                    task.id.0
                                );
                            }
                        }
                    }
                }
            }
        }

        // Merge detection: check if AwaitingMerge tasks have been merged.
        if let Ok(tasks) = service.list_tasks() {
            for task in &tasks {
                if task.state != TaskState::AwaitingMerge {
                    continue;
                }
                let branch_name = match task.branch_name.as_deref() {
                    Some(name) if !name.trim().is_empty() => name,
                    _ => continue,
                };
                let repo_config = match repo_config_by_id.get(&task.repo_id.0) {
                    Some(cfg) => cfg,
                    None => continue,
                };
                if is_branch_merged(
                    &repo_config.repo_path,
                    branch_name,
                    &repo_config.base_branch,
                ) {
                    let at = Utc::now();
                    // Stop any running agent for this task.
                    if let Some(stopped) = agent_supervisor.stop_task_session(&task.id) {
                        app.apply_event(TuiEvent::AgentPaneOutput {
                            instance_id: stopped.instance_id.clone(),
                            task_id: stopped.task_id.clone(),
                            model: stopped.model,
                            lines: vec!["[agent stopped: task merged]".to_string()],
                        });
                        app.apply_event(TuiEvent::AgentPaneStatusChanged {
                            instance_id: stopped.instance_id,
                            status: AgentPaneStatus::Exited,
                        });
                    }
                    // Transition to Merged.
                    match service.transition_task_state(
                        &task.id,
                        TaskState::Merged,
                        tui_event_id(&task.id, "MERGE-DETECTED", at),
                        at,
                    ) {
                        Ok(_) => {
                            // Clean up worktree and branch.
                            match cleanup_task_git_resources(task, &repo_config_by_id) {
                                Ok(summary) => {
                                    app.state.status_line = format!(
                                        "task {} merged and cleaned up ({summary})",
                                        task.id.0
                                    );
                                }
                                Err(err) => {
                                    app.state.status_line = format!(
                                        "task {} merged but cleanup failed: {err}",
                                        task.id.0
                                    );
                                }
                            }
                            signal_tick_requested = true;
                        }
                        Err(err) => {
                            app.state.status_line = format!(
                                "merge detected for {} but transition failed: {err}",
                                task.id.0
                            );
                        }
                    }
                }
            }
        }

        if signal_tick_requested {
            let at = Utc::now();
            match run_single_orchestrator_tick(
                &service,
                &runtime,
                &org,
                &repo_config_by_id,
                &enabled_models,
                &probe_config,
                at,
            ) {
                Ok(status) => {
                    if let Some(status) = status {
                        app.state.status_line = status;
                    }
                }
                Err(err) => {
                    app.state.status_line = format!("daemon tick failed: {err}");
                }
            }
            last_orch_tick = Instant::now();
            match service.list_tasks() {
                Ok(tasks) => app.set_tasks(&tasks),
                Err(err) => {
                    app.state.status_line = format!("failed to refresh tasks: {err}");
                }
            }
        }

        refresh_selected_task_activity(app, &service);
    });
    agent_supervisor.stop_all();
    tui_result?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TuiActionOutcome {
    message: String,
    force_tick: bool,
    events: Vec<TuiEvent>,
}

fn apply_stopped_agent_session_event(
    app: &mut TuiApp,
    stopped: TuiStartedAgentSession,
    reason: &str,
) {
    app.apply_event(TuiEvent::AgentPaneOutput {
        instance_id: stopped.instance_id.clone(),
        task_id: stopped.task_id.clone(),
        model: stopped.model,
        lines: vec![format!("[agent session stopped: {reason}]")],
    });
    app.apply_event(TuiEvent::AgentPaneStatusChanged {
        instance_id: stopped.instance_id,
        status: AgentPaneStatus::Exited,
    });
}

#[derive(Debug)]
struct TuiAgentSession {
    instance_id: String,
    task_id: TaskId,
    model: ModelKind,
    child: Child,
    output_rx: Receiver<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TuiStartedAgentSession {
    instance_id: String,
    task_id: TaskId,
    model: ModelKind,
}

#[derive(Debug, Default)]
struct TuiAgentSupervisor {
    sessions_by_task: HashMap<String, TuiAgentSession>,
    pending_start_by_task: HashMap<String, String>,
    next_instance_nonce: u64,
    pending_patch_ready_tasks: Vec<TaskId>,
    pending_completed_tasks: Vec<TaskId>,
    pending_needs_human_tasks: Vec<TaskId>,
    pending_conflict_resolved_tasks: Vec<TaskId>,
    conflict_resolution_tasks: HashSet<String>,
}

impl TuiAgentSupervisor {
    fn has_task_session(&self, task_id: &TaskId) -> bool {
        self.sessions_by_task.contains_key(&task_id.0)
    }

    fn start_task_session(
        &mut self,
        task: &Task,
        repo_config: &RepoConfig,
        enabled_models: &[ModelKind],
        user_prompt: Option<&str>,
    ) -> Result<TuiStartedAgentSession, MainError> {
        if self.has_task_session(&task.id) {
            return Err(MainError::InvalidConfig(format!(
                "agent already running for task {}",
                task.id.0
            )));
        }

        let model = select_agent_model(task, enabled_models)?;
        let adapter = default_adapter_for(model).map_err(|err| {
            MainError::InvalidConfig(format!("failed to configure adapter: {err}"))
        })?;
        let repo_path = resolve_task_runtime_path(task, repo_config);
        let request = EpochRequest {
            task_id: task.id.clone(),
            repo_id: task.repo_id.clone(),
            model,
            repo_path: repo_path.clone(),
            prompt: task_agent_prompt(task, user_prompt),
            timeout_secs: TUI_AGENT_TIMEOUT_SECS,
            extra_args: Vec::new(),
            env: Vec::new(),
        };
        let command_spec = adapter.build_command(&request);

        let mut command = Command::new(&command_spec.executable);
        command
            .args(&command_spec.args)
            .current_dir(&request.repo_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &command_spec.env {
            if key.trim().is_empty() {
                continue;
            }
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(|source| {
            MainError::InvalidConfig(format!(
                "failed to spawn {} for task {} in {}: {source}",
                model_kind_tag(&model),
                task.id.0,
                request.repo_path.display()
            ))
        })?;

        let (tx, rx) = mpsc::channel::<String>();
        if let Some(stdout) = child.stdout.take() {
            spawn_pipe_reader(stdout, tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_pipe_reader(stderr, tx.clone());
        }
        drop(tx);

        let instance_id = format!("A-{}-{}", task.id.0, self.next_instance_nonce);
        self.next_instance_nonce = self.next_instance_nonce.saturating_add(1);
        self.sessions_by_task.insert(
            task.id.0.clone(),
            TuiAgentSession {
                instance_id: instance_id.clone(),
                task_id: task.id.clone(),
                model,
                child,
                output_rx: rx,
            },
        );

        Ok(TuiStartedAgentSession {
            instance_id,
            task_id: task.id.clone(),
            model,
        })
    }

    fn queue_start(&mut self, task_id: &TaskId, prompt: String) {
        self.pending_start_by_task.insert(task_id.0.clone(), prompt);
    }

    fn stop_task_session(&mut self, task_id: &TaskId) -> Option<TuiStartedAgentSession> {
        let mut session = self.sessions_by_task.remove(&task_id.0)?;
        let _ = session.child.kill();
        let _ = session.child.wait();
        self.pending_start_by_task.remove(&task_id.0);
        self.pending_completed_tasks
            .retain(|existing| *existing != session.task_id);
        self.conflict_resolution_tasks.remove(&task_id.0);
        Some(TuiStartedAgentSession {
            instance_id: session.instance_id,
            task_id: session.task_id,
            model: session.model,
        })
    }

    fn drain_pending_ready_starts(
        &mut self,
        service: &OrchdService,
        repo_config_by_id: &HashMap<String, RepoConfig>,
        enabled_models: &[ModelKind],
    ) -> Vec<TuiEvent> {
        let mut events = Vec::new();
        let task_ids = self
            .pending_start_by_task
            .keys()
            .cloned()
            .collect::<Vec<_>>();

        for task_id_value in task_ids {
            if self.sessions_by_task.contains_key(&task_id_value) {
                continue;
            }

            let task_id = TaskId(task_id_value.clone());
            let task = match service.task(&task_id) {
                Ok(Some(task)) => task,
                Ok(None) => {
                    self.pending_start_by_task.remove(&task_id_value);
                    continue;
                }
                Err(err) => {
                    events.push(TuiEvent::StatusLine {
                        message: format!("failed to load pending task {}: {err}", task_id.0),
                    });
                    continue;
                }
            };

            if task.state == TaskState::Queued || task.state == TaskState::Initializing {
                continue;
            }

            let repo_config = match repo_config_by_id.get(&task.repo_id.0) {
                Some(cfg) => cfg,
                None => {
                    self.pending_start_by_task.remove(&task_id_value);
                    events.push(TuiEvent::StatusLine {
                        message: format!(
                            "missing repo config for repo_id={} (task {})",
                            task.repo_id.0, task.id.0
                        ),
                    });
                    continue;
                }
            };

            let prompt = self
                .pending_start_by_task
                .remove(&task_id_value)
                .unwrap_or_else(|| task.title.clone());
            match self.start_task_session(&task, repo_config, enabled_models, Some(&prompt)) {
                Ok(started) => {
                    events.push(TuiEvent::AgentPaneOutput {
                        instance_id: started.instance_id,
                        task_id: started.task_id,
                        model: started.model,
                        lines: vec!["[agent session started]".to_string()],
                    });
                }
                Err(err) => {
                    events.push(TuiEvent::StatusLine {
                        message: format!("failed to start agent for {}: {err}", task.id.0),
                    });
                }
            }
        }

        events
    }

    fn poll_events(&mut self, chat_history: &TuiChatHistory) -> Vec<TuiEvent> {
        let mut events = Vec::new();
        let mut finished_task_keys = Vec::new();

        for (task_key, session) in &mut self.sessions_by_task {
            let mut saw_terminal_signal = false;
            let mut lines = Vec::new();
            while let Ok(line) = session.output_rx.try_recv() {
                if let Some(signal) = detect_common_signal(&line) {
                    match signal.kind {
                        AgentSignalKind::PatchReady => {
                            saw_terminal_signal = true;
                            push_unique_task_id(
                                &mut self.pending_patch_ready_tasks,
                                &session.task_id,
                            );
                        }
                        AgentSignalKind::NeedHuman => {
                            saw_terminal_signal = true;
                            push_unique_task_id(
                                &mut self.pending_needs_human_tasks,
                                &session.task_id,
                            );
                        }
                        AgentSignalKind::ConflictResolved => {
                            saw_terminal_signal = true;
                            push_unique_task_id(
                                &mut self.pending_conflict_resolved_tasks,
                                &session.task_id,
                            );
                        }
                        AgentSignalKind::RateLimited | AgentSignalKind::ErrorHint => {}
                    }
                }
                lines.push(line);
            }
            if !lines.is_empty() {
                if let Err(err) = chat_history.append_lines(&session.task_id, &lines) {
                    events.push(TuiEvent::StatusLine {
                        message: format!(
                            "failed to persist chat history for {}: {err}",
                            session.task_id.0
                        ),
                    });
                }
                events.push(TuiEvent::AgentPaneOutput {
                    instance_id: session.instance_id.clone(),
                    task_id: session.task_id.clone(),
                    model: session.model,
                    lines,
                });
            }

            let is_conflict_resolution_session = self.conflict_resolution_tasks.contains(task_key);
            match session.child.try_wait() {
                Ok(Some(status)) => {
                    let exit_code = status.code().unwrap_or(-1);
                    let success = status.success();
                    let exit_line = format!("[agent exited code={exit_code}]");
                    if let Err(err) = chat_history
                        .append_lines(&session.task_id, std::slice::from_ref(&exit_line))
                    {
                        events.push(TuiEvent::StatusLine {
                            message: format!(
                                "failed to persist chat history for {}: {err}",
                                session.task_id.0
                            ),
                        });
                    }
                    events.push(TuiEvent::AgentPaneOutput {
                        instance_id: session.instance_id.clone(),
                        task_id: session.task_id.clone(),
                        model: session.model,
                        lines: vec![exit_line],
                    });
                    events.push(TuiEvent::AgentPaneStatusChanged {
                        instance_id: session.instance_id.clone(),
                        status: if success {
                            AgentPaneStatus::Exited
                        } else {
                            AgentPaneStatus::Failed
                        },
                    });
                    if success && !saw_terminal_signal && !is_conflict_resolution_session {
                        push_unique_task_id(&mut self.pending_completed_tasks, &session.task_id);
                    }
                    finished_task_keys.push(task_key.clone());
                }
                Ok(None) => {}
                Err(err) => {
                    let error_line = format!("[agent status error: {err}]");
                    if let Err(write_err) = chat_history
                        .append_lines(&session.task_id, std::slice::from_ref(&error_line))
                    {
                        events.push(TuiEvent::StatusLine {
                            message: format!(
                                "failed to persist chat history for {}: {write_err}",
                                session.task_id.0
                            ),
                        });
                    }
                    events.push(TuiEvent::AgentPaneOutput {
                        instance_id: session.instance_id.clone(),
                        task_id: session.task_id.clone(),
                        model: session.model,
                        lines: vec![error_line],
                    });
                    events.push(TuiEvent::AgentPaneStatusChanged {
                        instance_id: session.instance_id.clone(),
                        status: AgentPaneStatus::Failed,
                    });
                    finished_task_keys.push(task_key.clone());
                }
            }
        }

        for task_key in finished_task_keys {
            self.sessions_by_task.remove(&task_key);
            if self.conflict_resolution_tasks.remove(&task_key) {
                let task_id = TaskId(task_key);
                // If no conflict_resolved signal was pending, the agent couldn't resolve it.
                if !self
                    .pending_conflict_resolved_tasks
                    .iter()
                    .any(|id| *id == task_id)
                {
                    push_unique_task_id(&mut self.pending_needs_human_tasks, &task_id);
                }
            }
        }

        events
    }

    fn drain_patch_ready_tasks(&mut self) -> Vec<TaskId> {
        std::mem::take(&mut self.pending_patch_ready_tasks)
    }

    fn drain_completed_tasks(&mut self) -> Vec<TaskId> {
        std::mem::take(&mut self.pending_completed_tasks)
    }

    fn drain_need_human_tasks(&mut self) -> Vec<TaskId> {
        std::mem::take(&mut self.pending_needs_human_tasks)
    }

    fn drain_conflict_resolved_tasks(&mut self) -> Vec<TaskId> {
        std::mem::take(&mut self.pending_conflict_resolved_tasks)
    }

    fn start_conflict_resolution_session(
        &mut self,
        task: &Task,
        repo_config: &RepoConfig,
        enabled_models: &[ModelKind],
    ) -> Result<TuiStartedAgentSession, MainError> {
        if self.has_task_session(&task.id) {
            return Err(MainError::InvalidConfig(format!(
                "agent already running for task {}",
                task.id.0
            )));
        }

        let model = select_agent_model(task, enabled_models)?;
        let adapter = default_adapter_for(model).map_err(|err| {
            MainError::InvalidConfig(format!("failed to configure adapter: {err}"))
        })?;
        let repo_path = resolve_task_runtime_path(task, repo_config);
        let request = EpochRequest {
            task_id: task.id.clone(),
            repo_id: task.repo_id.clone(),
            model,
            repo_path: repo_path.clone(),
            prompt: conflict_resolution_prompt(task),
            timeout_secs: TUI_AGENT_TIMEOUT_SECS,
            extra_args: Vec::new(),
            env: Vec::new(),
        };
        let command_spec = adapter.build_command(&request);

        let mut command = Command::new(&command_spec.executable);
        command
            .args(&command_spec.args)
            .current_dir(&request.repo_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &command_spec.env {
            if key.trim().is_empty() {
                continue;
            }
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(|source| {
            MainError::InvalidConfig(format!(
                "failed to spawn {} for conflict resolution on {} in {}: {source}",
                model_kind_tag(&model),
                task.id.0,
                request.repo_path.display()
            ))
        })?;

        let (tx, rx) = mpsc::channel::<String>();
        if let Some(stdout) = child.stdout.take() {
            spawn_pipe_reader(stdout, tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_pipe_reader(stderr, tx.clone());
        }
        drop(tx);

        let instance_id = format!("CR-{}-{}", task.id.0, self.next_instance_nonce);
        self.next_instance_nonce = self.next_instance_nonce.saturating_add(1);
        self.sessions_by_task.insert(
            task.id.0.clone(),
            TuiAgentSession {
                instance_id: instance_id.clone(),
                task_id: task.id.clone(),
                model,
                child,
                output_rx: rx,
            },
        );
        self.conflict_resolution_tasks.insert(task.id.0.clone());

        Ok(TuiStartedAgentSession {
            instance_id,
            task_id: task.id.clone(),
            model,
        })
    }

    fn stop_all(&mut self) {
        let keys = self
            .sessions_by_task
            .keys()
            .cloned()
            .collect::<Vec<String>>();
        for key in keys {
            let task_id = TaskId(key);
            let _ = self.stop_task_session(&task_id);
        }
        self.pending_start_by_task.clear();
        self.pending_completed_tasks.clear();
        self.conflict_resolution_tasks.clear();
    }
}

fn push_unique_task_id(tasks: &mut Vec<TaskId>, task_id: &TaskId) {
    if !tasks.iter().any(|existing| existing == task_id) {
        tasks.push(task_id.clone());
    }
}

fn hydrate_chat_panes_from_history(
    app: &mut TuiApp,
    tasks: &[Task],
    chat_history: &TuiChatHistory,
) -> usize {
    let mut restored = 0usize;
    for task in tasks {
        let lines = match chat_history.load_lines(&task.id) {
            Ok(lines) => lines,
            Err(err) => {
                app.state.status_line =
                    format!("failed to load chat history for {}: {err}", task.id.0);
                continue;
            }
        };
        if lines.is_empty() {
            continue;
        }

        let mut pane = AgentPane::new(
            format!("H-{}", task.id.0),
            task.id.clone(),
            task.preferred_model.unwrap_or(ModelKind::Codex),
        );
        pane.status = AgentPaneStatus::Exited;
        for line in lines {
            pane.append_line(line);
        }
        app.state.panes.push(pane);
        restored += 1;
    }

    if restored > 0 {
        app.state.selected_pane_idx = app
            .state
            .selected_pane_idx
            .min(app.state.panes.len().saturating_sub(1));
    }

    restored
}

fn spawn_pipe_reader<R>(reader: R, tx: mpsc::Sender<String>)
where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffered = BufReader::new(reader);
        loop {
            let mut line = String::new();
            match buffered.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if let Some(normalized) = normalize_pane_line(&line) {
                        let _ = tx.send(normalized);
                    }
                }
                Err(err) => {
                    let _ = tx.send(format!("[reader error: {err}]"));
                    break;
                }
            }
        }
    });
}

fn select_agent_model(task: &Task, enabled_models: &[ModelKind]) -> Result<ModelKind, MainError> {
    if enabled_models.is_empty() {
        return Err(MainError::InvalidConfig(
            "no enabled models configured for agent start".to_string(),
        ));
    }
    if let Some(preferred) = task.preferred_model {
        if enabled_models.contains(&preferred) {
            return Ok(preferred);
        }
    }
    Ok(enabled_models[0])
}

fn resolve_task_runtime_path(task: &Task, repo_config: &RepoConfig) -> PathBuf {
    if task.worktree_path.is_absolute() && task.worktree_path.exists() {
        return task.worktree_path.clone();
    }
    if task.worktree_path.is_relative() {
        let joined = repo_config.repo_path.join(&task.worktree_path);
        if joined.exists() {
            return joined;
        }
    }
    repo_config.repo_path.clone()
}

fn task_agent_prompt(task: &Task, user_prompt: Option<&str>) -> String {
    let requested_work = user_prompt
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .unwrap_or(task.title.as_str());
    format!(
        "You are working on task {id}: {title}\n\
State: {state:?}\n\
Role: {role:?}\n\
	Type: {task_type:?}\n\
	Requested work:\n{requested_work}\n\
	Work in this task worktree, make focused changes, and report progress.\n\
	When implementation is complete and ready to submit, print exactly [patch_ready].\n\
	If blocked and human input is required, print exactly [needs_human] with a short reason.",
        id = task.id.0,
        title = task.title,
        state = task.state,
        role = task.role,
        task_type = task.task_type,
        requested_work = requested_work
    )
}

fn conflict_resolution_prompt(task: &Task) -> String {
    format!(
        "You are resolving git merge/rebase conflicts in a worktree for task {id}: {title}\n\
        The current branch has rebase conflicts that need resolution.\n\
        \n\
        Steps:\n\
        1. Run `git status` to identify files with conflicts\n\
        2. Open each conflicted file and resolve the conflict markers (<<<<<<< / ======= / >>>>>>>)\n\
        3. Choose the correct resolution by understanding the intent of both sides\n\
        4. After resolving all conflicts, print exactly [conflict_resolved]\n\
        \n\
        If you cannot resolve the conflicts because you need human guidance, print exactly [needs_human] with a short reason.",
        id = task.id.0,
        title = task.title,
    )
}

fn resolve_submit_mode_for_task(
    task: &Task,
    repo_config_by_id: &HashMap<String, RepoConfig>,
) -> SubmitMode {
    repo_config_by_id
        .get(&task.repo_id.0)
        .and_then(|cfg| cfg.graphite.submit_mode)
        .unwrap_or(task.submit_mode)
}

fn execute_tui_action(
    action: UiAction,
    task_id: Option<&TaskId>,
    prompt: Option<&str>,
    model: Option<ModelKind>,
    service: &OrchdService,
    org: &OrgConfig,
    enabled_models: &[ModelKind],
    probe_config: &SetupProbeConfig,
    repo_config_by_id: &HashMap<String, RepoConfig>,
    agent_supervisor: &mut TuiAgentSupervisor,
    at: chrono::DateTime<Utc>,
) -> Result<TuiActionOutcome, MainError> {
    if action == UiAction::CreateTask {
        let prompt = prompt
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                MainError::InvalidConfig(
                    "new chat prompt is required when creating a task".to_string(),
                )
            })?;
        let task = create_tui_task(task_id, prompt, model, service, org, repo_config_by_id, at)?;
        agent_supervisor.queue_start(&task.id, prompt.to_string());
        return Ok(TuiActionOutcome {
            message: format!(
                "created task {} in repo {} (starting agent)",
                task.id.0, task.repo_id.0
            ),
            force_tick: true,
            events: Vec::new(),
        });
    }

    let selected_task_id = task_id.cloned().ok_or_else(|| {
        MainError::InvalidConfig("select a task before running this action".to_string())
    })?;

    let mut task = service
        .task(&selected_task_id)?
        .ok_or_else(|| ServiceError::TaskNotFound {
            task_id: selected_task_id.0.clone(),
        })?;

    let outcome = match action {
        UiAction::CreateTask => unreachable!("handled above"),
        UiAction::ApproveTask => {
            let review_config = orchd::ReviewGateConfig {
                enabled_models: org.models.enabled.clone(),
                policy: org.models.policy,
                min_approvals: org.models.min_approvals,
            };
            let (_, availability_map) = current_model_availability(enabled_models, probe_config);
            let availability = enabled_models
                .iter()
                .copied()
                .map(|model| orchd::ReviewerAvailability {
                    model,
                    available: availability_map.get(&model).copied().unwrap_or(false),
                })
                .collect::<Vec<_>>();

            let (_, computation) = service.recompute_task_review_status(
                &selected_task_id,
                &review_config,
                &availability,
                at,
            )?;
            let required_models = if computation.requirement.required_models.is_empty() {
                enabled_models.to_vec()
            } else {
                computation.requirement.required_models.clone()
            };

            for reviewer in &required_models {
                service.complete_review(
                    &selected_task_id,
                    *reviewer,
                    ReviewOutput {
                        verdict: ReviewVerdict::Approve,
                        issues: Vec::new(),
                        risk_flags: Vec::new(),
                        graphite_hygiene: GraphiteHygieneReport {
                            ok: true,
                            notes: "manual approval from tui".to_string(),
                        },
                        test_assessment: TestAssessment {
                            ok: true,
                            notes: "manual approval from tui".to_string(),
                        },
                    },
                    &review_config,
                    &availability,
                    orchd::CompleteReviewEventIds {
                        review_completed: tui_event_id(&selected_task_id, "APPROVE-DONE", at),
                        needs_human_state_changed: tui_event_id(
                            &selected_task_id,
                            "APPROVE-NH-S",
                            at,
                        ),
                        needs_human_event: tui_event_id(&selected_task_id, "APPROVE-NH-E", at),
                    },
                    at,
                )?;
            }

            TuiActionOutcome {
                message: format!(
                    "recorded APPROVE for {} reviewer(s) on {}",
                    required_models.len(),
                    selected_task_id.0
                ),
                force_tick: true,
                events: Vec::new(),
            }
        }
        UiAction::StartAgent => {
            if agent_supervisor.has_task_session(&selected_task_id) {
                TuiActionOutcome {
                    message: format!("agent already running for {}", selected_task_id.0),
                    force_tick: false,
                    events: Vec::new(),
                }
            } else if task.state == TaskState::Queued || task.state == TaskState::Initializing {
                let queued_prompt = prompt
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| task.title.clone());
                agent_supervisor.queue_start(&selected_task_id, queued_prompt);
                TuiActionOutcome {
                    message: format!(
                        "task {} is {}; agent start queued until task is ready",
                        selected_task_id.0,
                        orchd::task_state_tag(task.state)
                    ),
                    force_tick: true,
                    events: Vec::new(),
                }
            } else {
                if task.state == TaskState::Paused {
                    task = service.resume_task(
                        &selected_task_id,
                        orchd::ResumeTaskEventIds {
                            resume_state_changed: tui_event_id(&selected_task_id, "RESUME-S", at),
                        },
                        at,
                    )?;
                } else if orchd::is_transition_allowed(task.state, TaskState::Running) {
                    task = service.transition_task_state(
                        &selected_task_id,
                        TaskState::Running,
                        tui_event_id(&selected_task_id, "START-RUNNING", at),
                        at,
                    )?;
                } else {
                    return Ok(TuiActionOutcome {
                        message: format!(
                            "cannot start task {} from state {:?}",
                            selected_task_id.0, task.state
                        ),
                        force_tick: false,
                        events: Vec::new(),
                    });
                }
                let repo_config = repo_config_by_id.get(&task.repo_id.0).ok_or_else(|| {
                    MainError::InvalidConfig(format!(
                        "missing repo config for repo_id={}",
                        task.repo_id.0
                    ))
                })?;
                let started = agent_supervisor.start_task_session(
                    &task,
                    repo_config,
                    enabled_models,
                    prompt,
                )?;
                TuiActionOutcome {
                    message: format!(
                        "started {} agent for {}",
                        model_kind_tag(&started.model),
                        selected_task_id.0
                    ),
                    force_tick: true,
                    events: vec![TuiEvent::AgentPaneOutput {
                        instance_id: started.instance_id,
                        task_id: started.task_id,
                        model: started.model,
                        lines: vec!["[agent session started]".to_string()],
                    }],
                }
            }
        }
        UiAction::StopAgent | UiAction::PauseTask => {
            let stopped = agent_supervisor.stop_task_session(&selected_task_id);
            let _ = service.pause_task(
                &selected_task_id,
                orchd::PauseTaskEventIds {
                    pause_state_changed: tui_event_id(&selected_task_id, "PAUSE-S", at),
                },
                at,
            )?;
            let mut events = Vec::new();
            let mut stop_note = "no active agent session".to_string();
            if let Some(stopped) = stopped {
                stop_note = format!("stopped {}", model_kind_tag(&stopped.model));
                events.push(TuiEvent::AgentPaneOutput {
                    instance_id: stopped.instance_id.clone(),
                    task_id: stopped.task_id.clone(),
                    model: stopped.model,
                    lines: vec!["[agent session stopped]".to_string()],
                });
                events.push(TuiEvent::AgentPaneStatusChanged {
                    instance_id: stopped.instance_id,
                    status: AgentPaneStatus::Exited,
                });
            }
            TuiActionOutcome {
                message: format!("paused task {} ({stop_note})", selected_task_id.0),
                force_tick: false,
                events,
            }
        }
        UiAction::RestartAgent => {
            let mut events = Vec::new();
            if let Some(stopped) = agent_supervisor.stop_task_session(&selected_task_id) {
                events.push(TuiEvent::AgentPaneOutput {
                    instance_id: stopped.instance_id.clone(),
                    task_id: stopped.task_id.clone(),
                    model: stopped.model,
                    lines: vec!["[agent session restarting]".to_string()],
                });
                events.push(TuiEvent::AgentPaneStatusChanged {
                    instance_id: stopped.instance_id,
                    status: AgentPaneStatus::Exited,
                });
            }

            if task.state == TaskState::Queued || task.state == TaskState::Initializing {
                TuiActionOutcome {
                    message: format!(
                        "task {} is {}; scheduler/runtime will prepare worktree first",
                        selected_task_id.0,
                        orchd::task_state_tag(task.state)
                    ),
                    force_tick: true,
                    events,
                }
            } else {
                if orchd::is_transition_allowed(task.state, TaskState::Paused) {
                    let _ = service.pause_task(
                        &selected_task_id,
                        orchd::PauseTaskEventIds {
                            pause_state_changed: tui_event_id(
                                &selected_task_id,
                                "RESTART-PAUSE",
                                at,
                            ),
                        },
                        at,
                    )?;
                    task = service.resume_task(
                        &selected_task_id,
                        orchd::ResumeTaskEventIds {
                            resume_state_changed: tui_event_id(
                                &selected_task_id,
                                "RESTART-RESUME",
                                at,
                            ),
                        },
                        at,
                    )?;
                } else if orchd::is_transition_allowed(task.state, TaskState::Running) {
                    task = service.transition_task_state(
                        &selected_task_id,
                        TaskState::Running,
                        tui_event_id(&selected_task_id, "RESTART-RUNNING", at),
                        at,
                    )?;
                } else {
                    return Ok(TuiActionOutcome {
                        message: format!(
                            "cannot restart task {} from state {:?}",
                            selected_task_id.0, task.state
                        ),
                        force_tick: false,
                        events,
                    });
                }
                let repo_config = repo_config_by_id.get(&task.repo_id.0).ok_or_else(|| {
                    MainError::InvalidConfig(format!(
                        "missing repo config for repo_id={}",
                        task.repo_id.0
                    ))
                })?;
                let started = agent_supervisor.start_task_session(
                    &task,
                    repo_config,
                    enabled_models,
                    prompt,
                )?;
                events.push(TuiEvent::AgentPaneOutput {
                    instance_id: started.instance_id.clone(),
                    task_id: started.task_id.clone(),
                    model: started.model,
                    lines: vec!["[agent session started]".to_string()],
                });
                TuiActionOutcome {
                    message: format!(
                        "restarted {} agent for {}",
                        model_kind_tag(&started.model),
                        selected_task_id.0
                    ),
                    force_tick: true,
                    events,
                }
            }
        }
        UiAction::DeleteTask => {
            let mut events = Vec::new();
            if let Some(stopped) = agent_supervisor.stop_task_session(&selected_task_id) {
                events.push(TuiEvent::AgentPaneOutput {
                    instance_id: stopped.instance_id.clone(),
                    task_id: stopped.task_id.clone(),
                    model: stopped.model,
                    lines: vec!["[agent session stopped for task deletion]".to_string()],
                });
                events.push(TuiEvent::AgentPaneStatusChanged {
                    instance_id: stopped.instance_id,
                    status: AgentPaneStatus::Exited,
                });
            }

            let cleanup_summary = cleanup_task_git_resources(&task, repo_config_by_id)?;
            let deleted = service.delete_task(&selected_task_id)?;
            if !deleted {
                return Err(MainError::Service(ServiceError::TaskNotFound {
                    task_id: selected_task_id.0.clone(),
                }));
            }

            TuiActionOutcome {
                message: format!("deleted task {} ({cleanup_summary})", selected_task_id.0),
                force_tick: false,
                events,
            }
        }
        UiAction::RunVerifyQuick => {
            let _ = service.start_verify(
                &selected_task_id,
                VerifyTier::Quick,
                orchd::StartVerifyEventIds {
                    verify_state_changed: tui_event_id(&selected_task_id, "VERIFY-QUICK-S", at),
                    verify_requested: tui_event_id(&selected_task_id, "VERIFY-QUICK-E", at),
                },
                at,
            )?;
            TuiActionOutcome {
                message: format!("triggered quick verify for {}", selected_task_id.0),
                force_tick: true,
                events: Vec::new(),
            }
        }
        UiAction::SubmitTask => {
            if task.state == TaskState::Merged {
                TuiActionOutcome {
                    message: format!("task {} already merged; nothing to do", selected_task_id.0),
                    force_tick: false,
                    events: Vec::new(),
                }
            } else {
                // Route through Running if direct transition to Submitting isn't allowed.
                if !orchd::is_transition_allowed(task.state, TaskState::Submitting)
                    && orchd::is_transition_allowed(task.state, TaskState::Running)
                {
                    service.transition_task_state(
                        &selected_task_id,
                        TaskState::Running,
                        tui_event_id(&selected_task_id, "SUBMIT-VIA-RUNNING", at),
                        at,
                    )?;
                }
                let mode = resolve_submit_mode_for_task(&task, repo_config_by_id);
                let _ = service.start_submit(
                    &selected_task_id,
                    mode,
                    orchd::StartSubmitEventIds {
                        submit_state_changed: tui_event_id(&selected_task_id, "SUBMIT-S", at),
                        submit_started: tui_event_id(&selected_task_id, "SUBMIT-E", at),
                    },
                    at,
                )?;
                TuiActionOutcome {
                    message: format!(
                        "started graphite submit for {} ({mode:?})",
                        selected_task_id.0
                    ),
                    force_tick: true,
                    events: Vec::new(),
                }
            }
        }
        UiAction::RunVerifyFull => {
            let _ = service.start_verify(
                &selected_task_id,
                VerifyTier::Full,
                orchd::StartVerifyEventIds {
                    verify_state_changed: tui_event_id(&selected_task_id, "VERIFY-FULL-S", at),
                    verify_requested: tui_event_id(&selected_task_id, "VERIFY-FULL-E", at),
                },
                at,
            )?;
            TuiActionOutcome {
                message: format!("triggered full verify for {}", selected_task_id.0),
                force_tick: true,
                events: Vec::new(),
            }
        }
        UiAction::TriggerRestack => {
            if task.state == TaskState::Merged {
                TuiActionOutcome {
                    message: format!("task {} already merged; nothing to do", selected_task_id.0),
                    force_tick: false,
                    events: Vec::new(),
                }
            } else if task.state == TaskState::RestackConflict || task.state == TaskState::Failed {
                let mode = resolve_submit_mode_for_task(&task, repo_config_by_id);
                let _ = service.start_submit(
                    &selected_task_id,
                    mode,
                    orchd::StartSubmitEventIds {
                        submit_state_changed: tui_event_id(
                            &selected_task_id,
                            "RESTACK-SUBMIT-S",
                            at,
                        ),
                        submit_started: tui_event_id(&selected_task_id, "RESTACK-SUBMIT-E", at),
                    },
                    at,
                )?;
                TuiActionOutcome {
                    message: format!(
                        "task {} in {}; started graphite submit ({mode:?})",
                        selected_task_id.0,
                        orchd::task_state_tag(task.state)
                    ),
                    force_tick: true,
                    events: Vec::new(),
                }
            } else {
                // Route through Running if needed (e.g. AwaitingMerge → Running → Restacking).
                if !orchd::is_transition_allowed(task.state, TaskState::Restacking)
                    && orchd::is_transition_allowed(task.state, TaskState::Running)
                {
                    service.transition_task_state(
                        &selected_task_id,
                        TaskState::Running,
                        tui_event_id(&selected_task_id, "RESTACK-VIA-RUNNING", at),
                        at,
                    )?;
                }
                let _ = service.start_restack(
                    &selected_task_id,
                    orchd::StartRestackEventIds {
                        restack_state_changed: tui_event_id(&selected_task_id, "RESTACK-S", at),
                        restack_started: tui_event_id(&selected_task_id, "RESTACK-E", at),
                    },
                    at,
                )?;
                TuiActionOutcome {
                    message: format!("started restack for {}", selected_task_id.0),
                    force_tick: true,
                    events: Vec::new(),
                }
            }
        }
        UiAction::MarkNeedsHuman => {
            let _ = service.mark_needs_human(
                &selected_task_id,
                "marked as needs-human from tui",
                orchd::MarkNeedsHumanEventIds {
                    needs_human_state_changed: tui_event_id(&selected_task_id, "NH-S", at),
                    needs_human_event: tui_event_id(&selected_task_id, "NH-E", at),
                },
                at,
            )?;
            TuiActionOutcome {
                message: format!("marked task {} NEEDS_HUMAN", selected_task_id.0),
                force_tick: false,
                events: Vec::new(),
            }
        }
        UiAction::OpenWebUiForTask => {
            let task_url = if let Some(pr) = task.pr.as_ref() {
                format!("web={} pr={}", org.ui.web_bind, pr.url)
            } else {
                format!("web={} task={}", org.ui.web_bind, selected_task_id.0)
            };
            TuiActionOutcome {
                message: format!("open: http://{task_url}"),
                force_tick: false,
                events: Vec::new(),
            }
        }
        UiAction::ResumeTask => {
            match task.state {
                TaskState::Merged => TuiActionOutcome {
                    message: format!("task {} already merged; nothing to do", selected_task_id.0),
                    force_tick: false,
                    events: Vec::new(),
                },
                TaskState::Running if agent_supervisor.has_task_session(&selected_task_id) => {
                    TuiActionOutcome {
                        message: format!("agent already running for {}", selected_task_id.0),
                        force_tick: false,
                        events: Vec::new(),
                    }
                }
                TaskState::Paused | TaskState::Failed | TaskState::NeedsHuman => {
                    // Transition to Running and restart the agent.
                    if task.state == TaskState::Paused {
                        task = service.resume_task(
                            &selected_task_id,
                            orchd::ResumeTaskEventIds {
                                resume_state_changed: tui_event_id(
                                    &selected_task_id,
                                    "RESUME-S",
                                    at,
                                ),
                            },
                            at,
                        )?;
                    } else if orchd::is_transition_allowed(task.state, TaskState::Running) {
                        task = service.transition_task_state(
                            &selected_task_id,
                            TaskState::Running,
                            tui_event_id(&selected_task_id, "RESUME-RUNNING", at),
                            at,
                        )?;
                    }
                    let repo_config = repo_config_by_id.get(&task.repo_id.0).ok_or_else(|| {
                        MainError::InvalidConfig(format!(
                            "missing repo config for repo_id={}",
                            task.repo_id.0
                        ))
                    })?;
                    let started = agent_supervisor.start_task_session(
                        &task,
                        repo_config,
                        enabled_models,
                        prompt,
                    )?;
                    TuiActionOutcome {
                        message: format!(
                            "resumed {} with {} agent",
                            selected_task_id.0,
                            model_kind_tag(&started.model),
                        ),
                        force_tick: true,
                        events: vec![TuiEvent::AgentPaneOutput {
                            instance_id: started.instance_id,
                            task_id: started.task_id,
                            model: started.model,
                            lines: vec!["[agent session started]".to_string()],
                        }],
                    }
                }
                TaskState::Running => {
                    // Running but no agent — work is done, commit + submit.
                    let mode = resolve_submit_mode_for_task(&task, repo_config_by_id);
                    let _ = service.start_submit(
                        &selected_task_id,
                        mode,
                        orchd::StartSubmitEventIds {
                            submit_state_changed: tui_event_id(
                                &selected_task_id,
                                "RESUME-SUBMIT-S",
                                at,
                            ),
                            submit_started: tui_event_id(&selected_task_id, "RESUME-SUBMIT-E", at),
                        },
                        at,
                    )?;
                    TuiActionOutcome {
                        message: format!(
                            "resumed {} with commit+submit ({mode:?})",
                            selected_task_id.0,
                        ),
                        force_tick: true,
                        events: Vec::new(),
                    }
                }
                TaskState::AwaitingMerge => {
                    // Re-submit.
                    let mode = resolve_submit_mode_for_task(&task, repo_config_by_id);
                    let _ = service.start_submit(
                        &selected_task_id,
                        mode,
                        orchd::StartSubmitEventIds {
                            submit_state_changed: tui_event_id(
                                &selected_task_id,
                                "RESUME-RESUBMIT-S",
                                at,
                            ),
                            submit_started: tui_event_id(
                                &selected_task_id,
                                "RESUME-RESUBMIT-E",
                                at,
                            ),
                        },
                        at,
                    )?;
                    TuiActionOutcome {
                        message: format!("re-submitted {} ({mode:?})", selected_task_id.0,),
                        force_tick: true,
                        events: Vec::new(),
                    }
                }
                TaskState::RestackConflict => {
                    // Start conflict resolution agent.
                    let repo_config = repo_config_by_id.get(&task.repo_id.0).ok_or_else(|| {
                        MainError::InvalidConfig(format!(
                            "missing repo config for repo_id={}",
                            task.repo_id.0
                        ))
                    })?;
                    let started = agent_supervisor.start_conflict_resolution_session(
                        &task,
                        repo_config,
                        enabled_models,
                    )?;
                    TuiActionOutcome {
                        message: format!("started conflict resolution for {}", selected_task_id.0,),
                        force_tick: true,
                        events: vec![TuiEvent::AgentPaneOutput {
                            instance_id: started.instance_id,
                            task_id: started.task_id,
                            model: started.model,
                            lines: vec!["[conflict resolution agent started]".to_string()],
                        }],
                    }
                }
                _ => {
                    // Fallback: resume + start agent.
                    if task.state == TaskState::Paused {
                        task = service.resume_task(
                            &selected_task_id,
                            orchd::ResumeTaskEventIds {
                                resume_state_changed: tui_event_id(
                                    &selected_task_id,
                                    "RESUME-S",
                                    at,
                                ),
                            },
                            at,
                        )?;
                    } else if orchd::is_transition_allowed(task.state, TaskState::Running) {
                        task = service.transition_task_state(
                            &selected_task_id,
                            TaskState::Running,
                            tui_event_id(&selected_task_id, "RESUME-RUNNING", at),
                            at,
                        )?;
                    }
                    let repo_config = repo_config_by_id.get(&task.repo_id.0).ok_or_else(|| {
                        MainError::InvalidConfig(format!(
                            "missing repo config for repo_id={}",
                            task.repo_id.0
                        ))
                    })?;
                    match agent_supervisor.start_task_session(
                        &task,
                        repo_config,
                        enabled_models,
                        prompt,
                    ) {
                        Ok(started) => TuiActionOutcome {
                            message: format!("resumed task {}", selected_task_id.0),
                            force_tick: true,
                            events: vec![TuiEvent::AgentPaneOutput {
                                instance_id: started.instance_id,
                                task_id: started.task_id,
                                model: started.model,
                                lines: vec!["[agent session started]".to_string()],
                            }],
                        },
                        Err(err) => TuiActionOutcome {
                            message: format!(
                                "resumed task {} (agent start failed: {err})",
                                selected_task_id.0,
                            ),
                            force_tick: true,
                            events: Vec::new(),
                        },
                    }
                }
            }
        }
    };

    Ok(outcome)
}

fn is_branch_merged(repo_root: &Path, branch: &str, base_branch: &str) -> bool {
    let git = GitCli::default();
    // `git branch --merged <base>` lists branches whose tip is reachable from <base>.
    match git.run(repo_root, ["branch", "--merged", base_branch]) {
        Ok(output) => output
            .stdout
            .lines()
            .any(|line| line.trim().trim_start_matches("* ") == branch),
        Err(_) => false,
    }
}

fn cleanup_task_git_resources(
    task: &Task,
    repo_config_by_id: &HashMap<String, RepoConfig>,
) -> Result<String, MainError> {
    let repo_config = repo_config_by_id.get(&task.repo_id.0).ok_or_else(|| {
        MainError::InvalidConfig(format!(
            "missing repo config for repo_id={}",
            task.repo_id.0
        ))
    })?;

    let git = GitCli::default();
    let repo = discover_repo(&repo_config.repo_path, &git).map_err(|err| {
        MainError::InvalidConfig(format!(
            "failed to open git repo {} for task {}: {err}",
            repo_config.repo_path.display(),
            task.id.0
        ))
    })?;

    let branch_name = cleanup_branch_name(task);
    let mut removed_worktrees = 0usize;
    let mut removed_paths: Vec<PathBuf> = Vec::new();

    if let Some(branch_name) = branch_name.as_deref() {
        let listed = WorktreeManager::default().list(&repo).map_err(|err| {
            MainError::InvalidConfig(format!(
                "failed to list worktrees for task {}: {err}",
                task.id.0
            ))
        })?;

        for entry in listed {
            if entry.path == repo.root {
                continue;
            }
            if entry.branch.as_deref() != Some(branch_name) {
                continue;
            }
            if removed_paths.iter().any(|path| path == &entry.path) {
                continue;
            }
            run_git_worktree_remove_force(&git, &repo.root, &entry.path).map_err(|err| {
                MainError::InvalidConfig(format!(
                    "failed to remove worktree {} for task {}: {err}",
                    entry.path.display(),
                    task.id.0
                ))
            })?;
            removed_paths.push(entry.path);
            removed_worktrees += 1;
        }
    }

    let configured_worktree = if task.worktree_path.is_absolute() {
        task.worktree_path.clone()
    } else {
        repo.root.join(&task.worktree_path)
    };
    if configured_worktree != repo.root
        && configured_worktree.exists()
        && !removed_paths
            .iter()
            .any(|path| path == &configured_worktree)
    {
        run_git_worktree_remove_force(&git, &repo.root, &configured_worktree).map_err(|err| {
            MainError::InvalidConfig(format!(
                "failed to remove configured worktree {} for task {}: {err}",
                configured_worktree.display(),
                task.id.0
            ))
        })?;
        removed_worktrees += 1;
    }

    let branch_status = if let Some(branch_name) = branch_name.as_deref() {
        match git.run(&repo.root, ["branch", "-D", branch_name]) {
            Ok(_) => format!("branch={branch_name} deleted"),
            Err(err) if is_git_branch_missing(&err) => {
                format!("branch={branch_name} already_missing")
            }
            Err(err) => {
                return Err(MainError::InvalidConfig(format!(
                    "failed to delete branch {branch_name} for task {}: {err}",
                    task.id.0
                )))
            }
        }
    } else {
        "branch=(none)".to_string()
    };

    Ok(format!(
        "worktrees_removed={} {branch_status}",
        removed_worktrees
    ))
}

fn cleanup_branch_name(task: &Task) -> Option<String> {
    task.branch_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let task_id = task.id.0.trim();
            if task_id.is_empty() {
                None
            } else {
                Some(format!("task/{task_id}"))
            }
        })
}

fn run_git_worktree_remove_force(
    git: &GitCli,
    repo_root: &Path,
    worktree: &Path,
) -> Result<(), GitError> {
    let args = vec![
        OsString::from("worktree"),
        OsString::from("remove"),
        OsString::from("--force"),
        worktree.as_os_str().to_os_string(),
    ];
    git.run(repo_root, args)?;
    Ok(())
}

fn is_git_branch_missing(err: &GitError) -> bool {
    match err {
        GitError::CommandFailed { stdout, stderr, .. } => {
            let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
            combined.contains("branch") && combined.contains("not found")
        }
        _ => false,
    }
}

fn create_tui_task(
    selected_task_id: Option<&TaskId>,
    prompt: &str,
    model: Option<ModelKind>,
    service: &OrchdService,
    org: &OrgConfig,
    repo_config_by_id: &HashMap<String, RepoConfig>,
    at: chrono::DateTime<Utc>,
) -> Result<Task, MainError> {
    let repo_id = match selected_task_id {
        Some(task_id) => {
            let selected_task =
                service
                    .task(task_id)?
                    .ok_or_else(|| ServiceError::TaskNotFound {
                        task_id: task_id.0.clone(),
                    })?;
            selected_task.repo_id.0
        }
        None => {
            let mut repo_ids = repo_config_by_id.keys().cloned().collect::<Vec<_>>();
            repo_ids.sort();
            repo_ids.into_iter().next().ok_or_else(|| {
                MainError::InvalidConfig(
                    "no repo configs loaded; cannot create task from tui".to_string(),
                )
            })?
        }
    };

    let base_id = format!("T{}", at.timestamp_millis());
    let mut task_id = TaskId(base_id.clone());
    let mut suffix = 1usize;
    while service.task(&task_id)?.is_some() {
        task_id = TaskId(format!("{base_id}-{suffix}"));
        suffix += 1;
    }
    let title = summarize_prompt_as_title(prompt);

    let task = Task {
        id: task_id.clone(),
        repo_id: orch_core::types::RepoId(repo_id),
        title,
        state: TaskState::Queued,
        role: orch_core::types::TaskRole::General,
        task_type: orch_core::types::TaskType::Feature,
        preferred_model: model,
        depends_on: Vec::new(),
        submit_mode: org.graphite.submit_mode_default,
        branch_name: None,
        worktree_path: PathBuf::from(format!(".orch/wt/{}", task_id.0)),
        pr: None,
        verify_status: VerifyStatus::NotRun,
        review_status: ReviewStatus {
            required_models: Vec::new(),
            approvals_received: 0,
            approvals_required: 0,
            unanimous: false,
            capacity_state: ReviewCapacityState::Sufficient,
        },
        created_at: at,
        updated_at: at,
    };

    let event = Event {
        id: EventId(format!(
            "E-TUI-CREATE-{}-{}",
            task.id.0,
            at.timestamp_nanos_opt().unwrap_or_default()
        )),
        task_id: Some(task.id.clone()),
        repo_id: Some(task.repo_id.clone()),
        at,
        kind: EventKind::TaskCreated,
    };
    service.create_task(&task, &event)?;
    Ok(task)
}

fn summarize_prompt_as_title(prompt: &str) -> String {
    let first = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("TUI task");
    let mut title = first.to_string();
    if title.len() > 96 {
        title.truncate(93);
        title.push_str("...");
    }
    title
}

fn run_single_orchestrator_tick(
    service: &OrchdService,
    runtime: &RuntimeEngine,
    org: &OrgConfig,
    repo_config_by_id: &HashMap<String, RepoConfig>,
    enabled_models: &[ModelKind],
    probe_config: &SetupProbeConfig,
    at: chrono::DateTime<Utc>,
) -> Result<Option<String>, MainError> {
    let (scheduler_availability, runtime_availability) =
        current_model_availability(enabled_models, probe_config);
    let scheduling = service.schedule_queued_tasks(enabled_models, &scheduler_availability, at)?;
    let runtime_tick = runtime.tick(service, org, repo_config_by_id, &runtime_availability, at)?;

    if scheduling.scheduled.is_empty() && scheduling.blocked.is_empty() && !runtime_tick.touched() {
        return Ok(None);
    }

    // Produce a cleaner message when a submit/push occurred.
    let message = if runtime_tick.submitted > 0 {
        format!("pushed {} task(s) to graphite", runtime_tick.submitted)
    } else if runtime_tick.submit_failed > 0 {
        format!(
            "push failed for {} task(s)",
            runtime_tick.submit_failed
        )
    } else {
        format!(
            "tick: scheduled={} blocked={} init={} verify_start={} restacked={} conflicts={} verify_pass={} verify_fail={} submitted={} submit_fail={} errors={}",
            scheduling.scheduled.len(),
            scheduling.blocked.len(),
            runtime_tick.initialized,
            runtime_tick.verify_started,
            runtime_tick.restacked,
            runtime_tick.restack_conflicts,
            runtime_tick.verify_passed,
            runtime_tick.verify_failed,
            runtime_tick.submitted,
            runtime_tick.submit_failed,
            runtime_tick.errors
        )
    };

    Ok(Some(message))
}

fn refresh_selected_task_activity(app: &mut TuiApp, service: &OrchdService) {
    let Some(task_id) = app.state.selected_task().map(|task| task.task_id.clone()) else {
        app.state.selected_task_activity.clear();
        return;
    };

    let task = match service.task(&task_id) {
        Ok(Some(task)) => task,
        Ok(None) => {
            app.state.selected_task_activity = vec![format!("task {} not found", task_id.0)];
            return;
        }
        Err(err) => {
            app.state.selected_task_activity =
                vec![format!("failed to load task {}: {err}", task_id.0)];
            return;
        }
    };

    let events = match service.task_events(&task_id) {
        Ok(events) => events,
        Err(err) => {
            app.state.selected_task_activity =
                vec![format!("failed to load events for {}: {err}", task_id.0)];
            return;
        }
    };

    let approvals = match service.task_approvals(&task_id) {
        Ok(approvals) => approvals,
        Err(err) => {
            app.state.selected_task_activity =
                vec![format!("failed to load approvals for {}: {err}", task_id.0)];
            return;
        }
    };

    app.state.selected_task_activity = build_task_activity_lines(&task, &events, &approvals);
}

fn build_task_activity_lines(
    task: &Task,
    events: &[Event],
    approvals: &[orch_core::types::TaskApproval],
) -> Vec<String> {
    let mut lines = Vec::new();
    let display_state = effective_display_state(task.state, &task.verify_status);
    lines.push(format!(
        "task={} state={} repo={}",
        task.id.0, display_state, task.repo_id.0,
    ));
    lines.push(format!(
        "branch={}",
        task.branch_name.as_deref().unwrap_or("-")
    ));
    if let Some(pr) = &task.pr {
        lines.push(format!(
            "pr=#{} draft={} url={}",
            pr.number, pr.draft, pr.url
        ));
    }
    lines.push(format!(
        "verify={} review={}/{} unanimous={} cap={:?}",
        verify_summary(&task.verify_status),
        task.review_status.approvals_received,
        task.review_status.approvals_required,
        task.review_status.unanimous,
        task.review_status.capacity_state
    ));
    lines.push("".to_string());
    lines.push("approvals:".to_string());
    if approvals.is_empty() {
        lines.push("  - none".to_string());
    } else {
        let mut sorted = approvals.to_vec();
        sorted.sort_by_key(|approval| approval.issued_at);
        sorted.reverse();
        for approval in sorted {
            lines.push(format!(
                "  - {} {:?} at {}",
                model_kind_tag(&approval.reviewer),
                approval.verdict,
                format_ts(approval.issued_at)
            ));
        }
    }

    lines.push("".to_string());
    lines.push("events (latest first):".to_string());
    for event in events.iter().rev().take(24) {
        append_event_lines(&mut lines, event);
    }

    lines
}

fn append_event_lines(lines: &mut Vec<String>, event: &Event) {
    let ts = format_ts(event.at);
    match &event.kind {
        EventKind::TaskCreated => {
            lines.push(format!("{ts} task created"));
        }
        EventKind::TaskStateChanged { from, to } => {
            lines.push(format!("{ts} state {from} -> {to}"));
        }
        EventKind::DraftPrCreated { number, url } => {
            lines.push(format!("{ts} draft pr created #{number} {url}"));
        }
        EventKind::ParentHeadUpdated { parent_task_id } => {
            lines.push(format!("{ts} parent head updated {}", parent_task_id.0));
        }
        EventKind::RestackStarted => {
            lines.push(format!("{ts} restack started"));
        }
        EventKind::RestackCompleted => {
            lines.push(format!("{ts} restack completed"));
        }
        EventKind::RestackConflict => {
            lines.push(format!("{ts} restack conflict"));
        }
        EventKind::RestackResolved => {
            lines.push(format!("{ts} restack resolved"));
        }
        EventKind::VerifyRequested { tier } => {
            lines.push(format!("{ts} verify requested {:?}", tier));
        }
        EventKind::VerifyCompleted { tier, success } => {
            lines.push(format!(
                "{ts} verify completed {:?} success={success}",
                tier
            ));
        }
        EventKind::ReviewRequested { required_models } => {
            let models = required_models
                .iter()
                .map(model_kind_tag)
                .collect::<Vec<_>>()
                .join(",");
            lines.push(format!("{ts} review requested models={models}"));
        }
        EventKind::ReviewCompleted { reviewer, output } => {
            lines.push(format!(
                "{ts} review {} verdict={:?} issues={} risks={} hygiene_ok={} tests_ok={}",
                model_kind_tag(reviewer),
                output.verdict,
                output.issues.len(),
                output.risk_flags.len(),
                output.graphite_hygiene.ok,
                output.test_assessment.ok
            ));
            for issue in output.issues.iter().take(3) {
                let line = issue
                    .line
                    .map(|line| line.to_string())
                    .unwrap_or_else(|| "-".to_string());
                lines.push(format!(
                    "    issue {:?} {}:{} {}",
                    issue.severity, issue.file, line, issue.description
                ));
            }
            if !output.risk_flags.is_empty() {
                lines.push(format!("    risks {}", output.risk_flags.join(",")));
            }
        }
        EventKind::ReadyReached => {
            lines.push(format!("{ts} ready reached"));
        }
        EventKind::SubmitStarted { mode } => {
            lines.push(format!("{ts} submit started mode={:?}", mode));
        }
        EventKind::SubmitCompleted => {
            lines.push(format!("{ts} submit completed"));
        }
        EventKind::NeedsHuman { reason } => {
            lines.push(format!("{ts} needs human {}", reason));
        }
        EventKind::Error { code, message } => {
            lines.push(format!("{ts} error code={code} msg={message}"));
        }
    }
}

fn verify_summary(status: &VerifyStatus) -> String {
    match status {
        VerifyStatus::NotRun => "not_run".to_string(),
        VerifyStatus::Running { tier } => format!("running:{tier:?}").to_ascii_lowercase(),
        VerifyStatus::Passed { tier } => format!("passed:{tier:?}").to_ascii_lowercase(),
        VerifyStatus::Failed { tier, summary } => {
            format!("failed:{tier:?}:{}", summary.replace('\n', " ")).to_ascii_lowercase()
        }
    }
}

fn format_ts(value: chrono::DateTime<Utc>) -> String {
    value
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn tui_event_id(task_id: &TaskId, stage: &str, at: chrono::DateTime<Utc>) -> EventId {
    let nonce = TUI_EVENT_NONCE.fetch_add(1, Ordering::Relaxed);
    EventId(format!(
        "E-TUI-{stage}-{}-{}-{nonce}",
        task_id.0,
        at.timestamp_nanos_opt().unwrap_or_default()
    ))
}

fn run_daemon(args: RunCliArgs) -> Result<(), MainError> {
    ensure_parent_dir(&args.sqlite_path)?;
    ensure_dir(&args.event_log_root)?;

    let org = load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
        path: args.org_config_path.clone(),
        source,
    })?;
    validate_org_config(&org.validate())?;

    let repo_configs = load_repo_configs(&args.repos_config_dir)?;
    validate_repo_configs(&repo_configs)?;
    run_startup_preflight(&repo_configs)?;

    let scheduler = Scheduler::new(SchedulerConfig::from_org_config(&org));
    let service = OrchdService::open(&args.sqlite_path, &args.event_log_root, scheduler)?;
    let runtime = RuntimeEngine::default();
    let repo_config_by_id = repo_configs
        .iter()
        .map(|(_, cfg)| (cfg.repo_id.clone(), cfg.clone()))
        .collect::<HashMap<_, _>>();

    let task_count = service.list_tasks()?.len();
    println!(
        "orchd bootstrapped sqlite={} event_log_root={} tasks={}",
        args.sqlite_path.display(),
        args.event_log_root.display(),
        task_count
    );
    if repo_configs.is_empty() {
        println!(
            "orchd loaded 0 repo configs from {}",
            args.repos_config_dir.display()
        );
    } else {
        let repo_ids = repo_configs
            .iter()
            .map(|(_, cfg)| cfg.repo_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "orchd loaded {} repo configs from {} [{}]",
            repo_configs.len(),
            args.repos_config_dir.display(),
            repo_ids
        );
    }

    let enabled_models = org.models.enabled.clone();
    let probe_config = SetupProbeConfig::default();
    let (scheduler_availability, runtime_availability) =
        current_model_availability(&enabled_models, &probe_config);

    let first_tick =
        service.schedule_queued_tasks(&enabled_models, &scheduler_availability, Utc::now())?;
    if !first_tick.scheduled.is_empty() || !first_tick.blocked.is_empty() {
        println!(
            "orchd scheduler tick scheduled={} blocked={}",
            first_tick.scheduled.len(),
            first_tick.blocked.len()
        );
    }
    let runtime_tick = runtime.tick(
        &service,
        &org,
        &repo_config_by_id,
        &runtime_availability,
        Utc::now(),
    )?;
    if runtime_tick.touched() {
        println!(
            "orchd runtime tick initialized={} verify_started={} restacked={} restack_conflicts={} verify_passed={} verify_failed={} submitted={} submit_failed={} errors={}",
            runtime_tick.initialized,
            runtime_tick.verify_started,
            runtime_tick.restacked,
            runtime_tick.restack_conflicts,
            runtime_tick.verify_passed,
            runtime_tick.verify_failed,
            runtime_tick.submitted,
            runtime_tick.submit_failed,
            runtime_tick.errors
        );
    }

    if args.once {
        println!("orchd exiting after bootstrap + single scheduler tick (--once)");
        return Ok(());
    }

    println!("orchd running; press Ctrl+C to stop");
    loop {
        thread::sleep(Duration::from_secs(5));
        let (scheduler_availability, runtime_availability) =
            current_model_availability(&enabled_models, &probe_config);
        let tick =
            service.schedule_queued_tasks(&enabled_models, &scheduler_availability, Utc::now())?;
        if !tick.scheduled.is_empty() || !tick.blocked.is_empty() {
            println!(
                "orchd scheduler tick scheduled={} blocked={}",
                tick.scheduled.len(),
                tick.blocked.len()
            );
        }

        let runtime_tick = runtime.tick(
            &service,
            &org,
            &repo_config_by_id,
            &runtime_availability,
            Utc::now(),
        )?;
        if runtime_tick.touched() {
            println!(
                "orchd runtime tick initialized={} verify_started={} restacked={} restack_conflicts={} verify_passed={} verify_failed={} submitted={} submit_failed={} errors={}",
                runtime_tick.initialized,
                runtime_tick.verify_started,
                runtime_tick.restacked,
                runtime_tick.restack_conflicts,
                runtime_tick.verify_passed,
                runtime_tick.verify_failed,
                runtime_tick.submitted,
                runtime_tick.submit_failed,
                runtime_tick.errors
            );
        }
    }
}

fn run_setup(args: SetupCliArgs) -> Result<(), MainError> {
    let mut org =
        load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
            path: args.org_config_path.clone(),
            source,
        })?;
    validate_org_config(&org.validate())?;

    let probe_config = SetupProbeConfig::default();
    let report = probe_models(&probe_config);

    let selected_models = if let Some(enabled) = args.enabled_models.clone() {
        enabled
    } else {
        report
            .models
            .iter()
            .filter(|probe| probe.installed && probe.version_ok)
            .map(|probe| probe.model)
            .collect::<Vec<_>>()
    };

    if selected_models.is_empty() {
        return Err(MainError::Args(
            "no runnable model CLIs detected; pass --enable with explicit models".to_string(),
        ));
    }

    let validated = validate_setup_selection(
        &report,
        &ModelSetupSelection {
            enabled_models: selected_models,
        },
    )?;

    let per_model_concurrency = args
        .per_model_concurrency
        .unwrap_or_else(|| org.concurrency.codex.max(1));

    apply_setup_selection_to_org_config(
        &mut org,
        &validated.enabled_models,
        per_model_concurrency,
    )?;
    validate_org_config(&org.validate())?;

    save_org_config(&args.org_config_path, &org).map_err(|source| MainError::SaveConfig {
        path: args.org_config_path.clone(),
        source,
    })?;

    let summary = summarize_setup(&report, &validated);
    print_setup_summary(&summary, per_model_concurrency, &args.org_config_path);

    Ok(())
}

fn run_wizard(args: WizardCliArgs) -> Result<(), MainError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(MainError::WizardNotInteractive);
    }

    let mut org = load_or_default_org_config(&args.org_config_path)?;
    validate_org_config(&org.validate())?;

    println!("Othala setup wizard");
    println!("config path: {}", args.org_config_path.display());

    let probe_config = SetupProbeConfig::default();
    let report = probe_models(&probe_config);

    println!("detected model CLI health:");
    for probe in &report.models {
        let status = if probe.installed && probe.version_ok {
            "runnable"
        } else {
            "unavailable"
        };
        let installed = if probe.installed {
            "detected"
        } else {
            "not_detected"
        };
        println!(
            "- {}: {} {}",
            model_kind_tag(&probe.model),
            installed,
            status
        );

        if !probe.env_status.is_empty() {
            for env_status in probe.env_status.iter().filter(|status| !status.satisfied) {
                println!("  missing env any-of: {}", env_status.any_of.join("|"));
            }
        }

        if let Some(version_output) = &probe.version_output {
            println!("  version: {version_output}");
        }
    }

    let validated = prompt_wizard_model_selection(&report)?;
    let per_model_concurrency = match args.per_model_concurrency {
        Some(value) => value,
        None => {
            let default_value = org.concurrency.codex.max(1);
            prompt_wizard_per_model_concurrency(default_value)?
        }
    };

    apply_setup_selection_to_org_config(
        &mut org,
        &validated.enabled_models,
        per_model_concurrency,
    )?;
    validate_org_config(&org.validate())?;
    save_org_config(&args.org_config_path, &org).map_err(|source| MainError::SaveConfig {
        path: args.org_config_path.clone(),
        source,
    })?;

    let summary = summarize_setup(&report, &validated);
    print_setup_summary(&summary, per_model_concurrency, &args.org_config_path);
    println!("next: run `othala daemon --once` to execute one scheduler/runtime tick");

    Ok(())
}

fn load_or_default_org_config(path: &Path) -> Result<OrgConfig, MainError> {
    match load_org_config(path) {
        Ok(cfg) => Ok(cfg),
        Err(ConfigError::Read { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(default_org_config())
        }
        Err(source) => Err(MainError::LoadConfig {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn default_org_config() -> OrgConfig {
    OrgConfig {
        models: orch_core::config::ModelsConfig {
            enabled: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
            policy: ReviewPolicy::Adaptive,
            min_approvals: 2,
        },
        concurrency: orch_core::config::ConcurrencyConfig {
            per_repo: 10,
            claude: 10,
            codex: 10,
            gemini: 10,
        },
        graphite: orch_core::config::GraphiteOrgConfig {
            auto_submit: true,
            submit_mode_default: SubmitMode::Single,
            allow_move: MovePolicy::Manual,
        },
        ui: orch_core::config::UiConfig {
            web_bind: "127.0.0.1:9842".to_string(),
        },
    }
}

fn prompt_wizard_model_selection(
    report: &SetupProbeReport,
) -> Result<ValidatedSetupSelection, MainError> {
    let default_models = report
        .models
        .iter()
        .filter(|probe| probe.installed && probe.version_ok)
        .map(|probe| probe.model)
        .collect::<Vec<_>>();
    if default_models.is_empty() {
        return Err(MainError::Args(
            "wizard found no runnable model CLIs; install/log in and rerun".to_string(),
        ));
    }

    let default_text = default_models
        .iter()
        .map(model_kind_tag)
        .collect::<Vec<_>>()
        .join(",");

    loop {
        let input = prompt_wizard_line(&format!(
            "enable models (comma-separated claude,codex,gemini) [{}]: ",
            default_text
        ))?;

        let selected = if input.trim().is_empty() {
            default_models.clone()
        } else {
            match parse_enabled_models(input.trim()) {
                Ok(models) => models,
                Err(err) => {
                    println!("{err}");
                    continue;
                }
            }
        };

        let selection = ModelSetupSelection {
            enabled_models: selected,
        };
        match validate_setup_selection(report, &selection) {
            Ok(validated) => return Ok(validated),
            Err(err) => {
                println!("{err}");
            }
        }
    }
}

fn prompt_wizard_per_model_concurrency(default_value: usize) -> Result<usize, MainError> {
    loop {
        let input =
            prompt_wizard_line(&format!("per-model concurrency (>0) [{}]: ", default_value))?;
        if input.trim().is_empty() {
            return Ok(default_value);
        }

        match input.trim().parse::<usize>() {
            Ok(value) if value > 0 => return Ok(value),
            _ => println!("invalid value: expected positive integer"),
        }
    }
}

fn prompt_wizard_line(prompt: &str) -> Result<String, MainError> {
    print!("{prompt}");
    io::stdout()
        .flush()
        .map_err(|source| MainError::WizardWrite { source })?;

    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|source| MainError::WizardRead { source })?;
    Ok(line)
}

fn run_create_task(args: CreateTaskCliArgs) -> Result<(), MainError> {
    ensure_parent_dir(&args.sqlite_path)?;
    ensure_dir(&args.event_log_root)?;

    let org = load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
        path: args.org_config_path.clone(),
        source,
    })?;
    validate_org_config(&org.validate())?;

    let repo_configs = load_repo_configs(&args.repos_config_dir)?;
    validate_repo_configs(&repo_configs)?;

    let spec_raw =
        fs::read_to_string(&args.spec_path).map_err(|source| MainError::ReadTaskSpecFile {
            path: args.spec_path.clone(),
            source,
        })?;
    let spec: TaskSpec =
        serde_json::from_str(&spec_raw).map_err(|source| MainError::ParseTaskSpecJson {
            path: args.spec_path.clone(),
            source,
        })?;

    let spec_errors = spec
        .validate()
        .into_iter()
        .filter(|issue| issue.level == ValidationLevel::Error)
        .collect::<Vec<_>>();
    if !spec_errors.is_empty() {
        let rendered = spec_errors
            .iter()
            .map(|issue| format!("{}: {}", issue.code, issue.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(MainError::InvalidConfig(format!(
            "task spec validation failed ({})",
            rendered
        )));
    }

    if !repo_configs
        .iter()
        .any(|(_, cfg)| cfg.repo_id == spec.repo_id.0)
    {
        return Err(MainError::InvalidConfig(format!(
            "task repo_id '{}' is missing in {}",
            spec.repo_id.0,
            args.repos_config_dir.display()
        )));
    }

    let now = Utc::now();
    let task = Task {
        id: spec.task_id.clone(),
        repo_id: spec.repo_id.clone(),
        title: spec.title.clone(),
        state: TaskState::Queued,
        role: spec.role,
        task_type: spec.task_type.clone(),
        preferred_model: spec.preferred_model,
        depends_on: spec.depends_on.clone(),
        submit_mode: spec.submit_mode.unwrap_or(org.graphite.submit_mode_default),
        branch_name: None,
        worktree_path: PathBuf::from(format!(".orch/wt/{}", spec.task_id.0)),
        pr: None,
        verify_status: VerifyStatus::NotRun,
        review_status: ReviewStatus {
            required_models: Vec::new(),
            approvals_received: 0,
            approvals_required: 0,
            unanimous: false,
            capacity_state: ReviewCapacityState::Sufficient,
        },
        created_at: now,
        updated_at: now,
    };
    let created_event = Event {
        id: EventId(format!(
            "E-CREATE-{}-{}",
            task.id.0,
            now.timestamp_nanos_opt().unwrap_or_default()
        )),
        task_id: Some(task.id.clone()),
        repo_id: Some(task.repo_id.clone()),
        at: now,
        kind: EventKind::TaskCreated,
    };

    let scheduler = Scheduler::new(SchedulerConfig::from_org_config(&org));
    let service = OrchdService::open(&args.sqlite_path, &args.event_log_root, scheduler)?;
    service.create_task(&task, &created_event)?;

    println!(
        "created task id={} repo={} state={} spec={}",
        task.id.0,
        task.repo_id.0,
        "QUEUED",
        args.spec_path.display()
    );

    Ok(())
}

fn run_list_tasks(args: ListTasksCliArgs) -> Result<(), MainError> {
    ensure_parent_dir(&args.sqlite_path)?;
    ensure_dir(&args.event_log_root)?;

    let org = load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
        path: args.org_config_path.clone(),
        source,
    })?;
    validate_org_config(&org.validate())?;

    let scheduler = Scheduler::new(SchedulerConfig::from_org_config(&org));
    let service = OrchdService::open(&args.sqlite_path, &args.event_log_root, scheduler)?;
    let tasks = service.list_tasks()?;
    let rendered = serde_json::to_string_pretty(&tasks)
        .map_err(|source| MainError::SerializeTaskList { source })?;
    println!("{rendered}");

    Ok(())
}

fn run_review_approve(args: ReviewApproveCliArgs) -> Result<(), MainError> {
    ensure_parent_dir(&args.sqlite_path)?;
    ensure_dir(&args.event_log_root)?;

    let org = load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
        path: args.org_config_path.clone(),
        source,
    })?;
    validate_org_config(&org.validate())?;

    let scheduler = Scheduler::new(SchedulerConfig::from_org_config(&org));
    let service = OrchdService::open(&args.sqlite_path, &args.event_log_root, scheduler)?;

    let probe_config = SetupProbeConfig::default();
    let (_, availability_map) = current_model_availability(&org.models.enabled, &probe_config);
    let review_config = orchd::ReviewGateConfig {
        enabled_models: org.models.enabled.clone(),
        policy: org.models.policy,
        min_approvals: org.models.min_approvals,
    };
    let availability = org
        .models
        .enabled
        .iter()
        .copied()
        .map(|model| orchd::ReviewerAvailability {
            model,
            available: availability_map.get(&model).copied().unwrap_or(false),
        })
        .collect::<Vec<_>>();

    let output = ReviewOutput {
        verdict: args.verdict,
        issues: Vec::new(),
        risk_flags: Vec::new(),
        graphite_hygiene: GraphiteHygieneReport {
            ok: true,
            notes: "manual approval".to_string(),
        },
        test_assessment: TestAssessment {
            ok: true,
            notes: "manual approval".to_string(),
        },
    };

    let now = Utc::now();
    let event_nonce = now.timestamp_nanos_opt().unwrap_or_default();
    let outcome = service.complete_review(
        &args.task_id,
        args.reviewer,
        output,
        &review_config,
        &availability,
        orchd::CompleteReviewEventIds {
            review_completed: EventId(format!("E-REVIEW-DONE-{}-{event_nonce}", args.task_id.0)),
            needs_human_state_changed: EventId(format!(
                "E-REVIEW-NH-S-{}-{event_nonce}",
                args.task_id.0
            )),
            needs_human_event: EventId(format!("E-REVIEW-NH-E-{}-{event_nonce}", args.task_id.0)),
        },
        now,
    )?;

    println!(
        "review recorded task={} reviewer={} verdict={:?} approvals={}/{} approved={}",
        outcome.task.id.0,
        model_kind_tag(&args.reviewer),
        args.verdict,
        outcome.computation.evaluation.approvals_received,
        outcome.computation.requirement.approvals_required,
        outcome.computation.evaluation.approved
    );

    Ok(())
}

fn print_setup_summary(summary: &SetupSummary, per_model_concurrency: usize, path: &Path) {
    println!(
        "setup saved org config to {} (per-model concurrency={})",
        path.display(),
        per_model_concurrency
    );
    println!(
        "selected models: {}",
        summary
            .selected_models
            .iter()
            .map(model_kind_tag)
            .collect::<Vec<_>>()
            .join(", ")
    );

    for item in &summary.items {
        let status = if item.healthy {
            "healthy"
        } else if item.detected && !item.missing_env_any_of.is_empty() {
            "runnable (API env optional/missing)"
        } else {
            "unhealthy"
        };
        let detected = if item.detected {
            "detected"
        } else {
            "not_detected"
        };
        println!(
            "- {}: {} {} selected={}",
            model_kind_tag(&item.model),
            detected,
            status,
            item.selected
        );
    }
}

fn model_kind_tag(model: &ModelKind) -> &'static str {
    match model {
        ModelKind::Claude => "claude",
        ModelKind::Codex => "codex",
        ModelKind::Gemini => "gemini",
    }
}

fn current_model_availability(
    enabled_models: &[ModelKind],
    probe_config: &SetupProbeConfig,
) -> (Vec<ModelAvailability>, HashMap<ModelKind, bool>) {
    let report = probe_models(probe_config);
    let mut availability_map = report
        .models
        .iter()
        // Runtime scheduling only needs executable reachability.
        // Full env health remains visible in setup checks.
        .map(|probe| (probe.model, probe.installed && probe.version_ok))
        .collect::<HashMap<_, _>>();

    for model in enabled_models {
        availability_map.entry(*model).or_insert(false);
    }

    let scheduler_availability = enabled_models
        .iter()
        .copied()
        .map(|model| ModelAvailability {
            model,
            available: availability_map.get(&model).copied().unwrap_or(false),
        })
        .collect::<Vec<_>>();

    (scheduler_availability, availability_map)
}

fn ensure_dir(path: &Path) -> Result<(), MainError> {
    fs::create_dir_all(path).map_err(|source| MainError::CreateDir {
        path: path.to_path_buf(),
        source,
    })
}

fn ensure_parent_dir(path: &Path) -> Result<(), MainError> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        ensure_dir(parent)?;
    }
    Ok(())
}

fn validate_org_config(issues: &[ValidationIssue]) -> Result<(), MainError> {
    let errors = issues
        .iter()
        .filter(|issue| issue.level == ValidationLevel::Error)
        .collect::<Vec<_>>();

    if errors.is_empty() {
        return Ok(());
    }

    let rendered = errors
        .iter()
        .map(|issue| format!("{}: {}", issue.code, issue.message))
        .collect::<Vec<_>>()
        .join("; ");
    Err(MainError::InvalidConfig(format!(
        "org config validation failed ({})",
        rendered
    )))
}

fn load_repo_configs(repo_dir: &Path) -> Result<Vec<(PathBuf, RepoConfig)>, MainError> {
    if !repo_dir.exists() {
        return Ok(Vec::new());
    }

    let entries = fs::read_dir(repo_dir).map_err(|source| MainError::ReadRepoConfigDir {
        path: repo_dir.to_path_buf(),
        source,
    })?;

    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| MainError::ReadRepoConfigDir {
            path: repo_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) == Some("toml") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut repo_configs = Vec::new();
    for path in paths {
        let cfg = load_repo_config(&path).map_err(|source| MainError::LoadConfig {
            path: path.clone(),
            source,
        })?;
        repo_configs.push((path, cfg));
    }
    Ok(repo_configs)
}

fn validate_repo_configs(repo_configs: &[(PathBuf, RepoConfig)]) -> Result<(), MainError> {
    let mut errors = Vec::new();

    for (path, cfg) in repo_configs {
        for issue in cfg.validate() {
            if issue.level == ValidationLevel::Error {
                errors.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    issue.code,
                    issue.message
                ));
            }
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    Err(MainError::InvalidConfig(format!(
        "repo config validation failed ({})",
        errors.join("; ")
    )))
}

fn run_startup_preflight(repo_configs: &[(PathBuf, RepoConfig)]) -> Result<(), MainError> {
    if repo_configs.is_empty() {
        return Ok(());
    }

    let mut errors = Vec::new();
    if !command_in_path("nix") {
        errors.push("missing required CLI in PATH: nix".to_string());
    }
    if !command_in_path("gt") {
        errors.push("missing required CLI in PATH: gt (Graphite)".to_string());
    }

    let git = GitCli::default();
    for (_, repo) in repo_configs {
        if !repo.repo_path.exists() {
            errors.push(format!(
                "repo {} path does not exist: {}",
                repo.repo_id,
                repo.repo_path.display()
            ));
            continue;
        }

        if let Err(err) = discover_repo(&repo.repo_path, &git) {
            errors.push(format!(
                "repo {} is not a valid git repository at {}: {}",
                repo.repo_id,
                repo.repo_path.display(),
                err
            ));
            continue;
        }

        if let Err(err) = verify_nix_dev_shell(repo) {
            errors.push(format!(
                "repo {} nix dev shell check failed: {}",
                repo.repo_id, err
            ));
        }

        let gt_client = GraphiteClient::new(repo.repo_path.clone());
        if let Err(err) = gt_client.status_snapshot() {
            // Graphite may not be initialized in this repo — attempt auto-init.
            if let Err(init_err) = gt_client.repo_init(&repo.base_branch) {
                errors.push(format!(
                    "repo {} graphite check failed at {} (auto-init also failed: {}): {}",
                    repo.repo_id,
                    repo.repo_path.display(),
                    init_err,
                    err
                ));
            } else if let Err(retry_err) = gt_client.status_snapshot() {
                errors.push(format!(
                    "repo {} graphite check failed at {} (even after auto-init): {}",
                    repo.repo_id,
                    repo.repo_path.display(),
                    retry_err
                ));
            }
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    Err(MainError::InvalidConfig(format!(
        "startup preflight failed ({})",
        errors.join("; ")
    )))
}

fn command_in_path(binary: &str) -> bool {
    Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {} >/dev/null 2>&1", binary))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn verify_nix_dev_shell(repo: &RepoConfig) -> Result<(), String> {
    if repo.nix.dev_shell.trim().is_empty() {
        return Err("dev_shell command is empty".to_string());
    }

    let cmd = format!("{} -c true", repo.nix.dev_shell);
    let output = Command::new("bash")
        .arg("-lc")
        .arg(&cmd)
        .current_dir(&repo.repo_path)
        .output()
        .map_err(|err| format!("failed to run '{}': {}", cmd, err))?;

    if output.status.success() {
        return Ok(());
    }

    let status = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        "no output".to_string()
    };
    Err(format!("'{}' exited with {} ({})", cmd, status, detail))
}

fn parse_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    if args.is_empty() {
        return Ok(CliCommand::Tui(default_tui_args()));
    }

    match args[0].as_str() {
        "daemon" => parse_run_cli_args(args[1..].to_vec(), program),
        "tui" => parse_tui_cli_args(args[1..].to_vec(), program),
        "setup" => parse_setup_cli_args(args[1..].to_vec(), program),
        "wizard" => parse_wizard_cli_args(args[1..].to_vec(), program),
        "create-task" => parse_create_task_cli_args(args[1..].to_vec(), program),
        "list-tasks" => parse_list_tasks_cli_args(args[1..].to_vec(), program),
        "review-approve" => parse_review_approve_cli_args(args[1..].to_vec(), program),
        "help" | "--help" | "-h" => Ok(CliCommand::Help(usage(program))),
        _ if args[0].starts_with('-') => parse_tui_cli_args(args, program),
        other => Err(MainError::Args(format!(
            "unknown command: {other}\n\n{}",
            usage(program)
        ))),
    }
}

fn parse_tui_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = default_tui_args();

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(tui_usage(program))),
            "--tick-ms" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --tick-ms".to_string()))?;
                let tick_ms = value.parse::<u64>().map_err(|_| {
                    MainError::Args(format!("invalid --tick-ms value: {value} (expected u64)"))
                })?;
                if tick_ms == 0 {
                    return Err(MainError::Args(
                        "invalid --tick-ms value: 0 (must be > 0)".to_string(),
                    ));
                }
                parsed.tick_ms = tick_ms;
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                parsed.sqlite_path = PathBuf::from(value);
            }
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--repos-config-dir" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --repos-config-dir".to_string())
                })?;
                parsed.repos_config_dir = PathBuf::from(value);
            }
            "--event-log-root" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --event-log-root".to_string())
                })?;
                parsed.event_log_root = PathBuf::from(value);
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown tui argument: {other}\n\n{}",
                    tui_usage(program)
                )));
            }
        }
        idx += 1;
    }

    Ok(CliCommand::Tui(parsed))
}

fn parse_run_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = default_run_args();

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(usage(program))),
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--repos-config-dir" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --repos-config-dir".to_string())
                })?;
                parsed.repos_config_dir = PathBuf::from(value);
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                parsed.sqlite_path = PathBuf::from(value);
            }
            "--event-log-root" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --event-log-root".to_string())
                })?;
                parsed.event_log_root = PathBuf::from(value);
            }
            "--once" => {
                parsed.once = true;
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown argument: {other}\n\n{}",
                    usage(program)
                )));
            }
        }
        idx += 1;
    }

    Ok(CliCommand::Run(parsed))
}

fn parse_setup_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = SetupCliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        enabled_models: None,
        per_model_concurrency: None,
    };
    let mut enabled_models = Vec::new();

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(setup_usage(program))),
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--enable" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --enable".to_string()))?;
                enabled_models.extend(parse_enabled_models(value)?);
            }
            "--per-model-concurrency" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --per-model-concurrency".to_string())
                })?;
                let parsed_value = value.parse::<usize>().map_err(|_| {
                    MainError::Args(format!(
                        "invalid --per-model-concurrency value: {value} (expected usize > 0)"
                    ))
                })?;
                if parsed_value == 0 {
                    return Err(MainError::Args(
                        "invalid --per-model-concurrency value: 0 (must be > 0)".to_string(),
                    ));
                }
                parsed.per_model_concurrency = Some(parsed_value);
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown setup argument: {other}\n\n{}",
                    setup_usage(program)
                )));
            }
        }
        idx += 1;
    }

    if !enabled_models.is_empty() {
        parsed.enabled_models = Some(enabled_models);
    }

    Ok(CliCommand::Setup(parsed))
}

fn parse_wizard_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = WizardCliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        per_model_concurrency: None,
    };

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(wizard_usage(program))),
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--per-model-concurrency" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --per-model-concurrency".to_string())
                })?;
                let parsed_value = value.parse::<usize>().map_err(|_| {
                    MainError::Args(format!(
                        "invalid --per-model-concurrency value: {value} (expected usize > 0)"
                    ))
                })?;
                if parsed_value == 0 {
                    return Err(MainError::Args(
                        "invalid --per-model-concurrency value: 0 (must be > 0)".to_string(),
                    ));
                }
                parsed.per_model_concurrency = Some(parsed_value);
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown wizard argument: {other}\n\n{}",
                    wizard_usage(program)
                )));
            }
        }
        idx += 1;
    }

    Ok(CliCommand::Wizard(parsed))
}

fn parse_create_task_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = CreateTaskCliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        repos_config_dir: PathBuf::from(DEFAULT_REPOS_CONFIG_DIR),
        sqlite_path: PathBuf::from(DEFAULT_SQLITE_PATH),
        event_log_root: PathBuf::from(DEFAULT_EVENT_LOG_ROOT),
        spec_path: PathBuf::new(),
    };

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(create_task_usage(program))),
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--repos-config-dir" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --repos-config-dir".to_string())
                })?;
                parsed.repos_config_dir = PathBuf::from(value);
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                parsed.sqlite_path = PathBuf::from(value);
            }
            "--event-log-root" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --event-log-root".to_string())
                })?;
                parsed.event_log_root = PathBuf::from(value);
            }
            "--spec" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --spec".to_string()))?;
                parsed.spec_path = PathBuf::from(value);
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown create-task argument: {other}\n\n{}",
                    create_task_usage(program)
                )));
            }
        }
        idx += 1;
    }

    if parsed.spec_path.as_os_str().is_empty() {
        return Err(MainError::Args(
            "missing required --spec <path> for create-task".to_string(),
        ));
    }

    Ok(CliCommand::CreateTask(parsed))
}

fn parse_list_tasks_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = ListTasksCliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        sqlite_path: PathBuf::from(DEFAULT_SQLITE_PATH),
        event_log_root: PathBuf::from(DEFAULT_EVENT_LOG_ROOT),
    };

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(list_tasks_usage(program))),
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                parsed.sqlite_path = PathBuf::from(value);
            }
            "--event-log-root" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --event-log-root".to_string())
                })?;
                parsed.event_log_root = PathBuf::from(value);
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown list-tasks argument: {other}\n\n{}",
                    list_tasks_usage(program)
                )));
            }
        }
        idx += 1;
    }

    Ok(CliCommand::ListTasks(parsed))
}

fn parse_review_approve_cli_args(
    args: Vec<String>,
    program: &str,
) -> Result<CliCommand, MainError> {
    let mut parsed = ReviewApproveCliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        sqlite_path: PathBuf::from(DEFAULT_SQLITE_PATH),
        event_log_root: PathBuf::from(DEFAULT_EVENT_LOG_ROOT),
        task_id: TaskId(String::new()),
        reviewer: ModelKind::Codex,
        verdict: ReviewVerdict::Approve,
    };
    let mut reviewer_provided = false;

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(review_approve_usage(program))),
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                parsed.sqlite_path = PathBuf::from(value);
            }
            "--event-log-root" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --event-log-root".to_string())
                })?;
                parsed.event_log_root = PathBuf::from(value);
            }
            "--task-id" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --task-id".to_string()))?;
                parsed.task_id = TaskId(value.to_string());
            }
            "--reviewer" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --reviewer".to_string()))?;
                parsed.reviewer = parse_model_kind(value)?;
                reviewer_provided = true;
            }
            "--verdict" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --verdict".to_string()))?;
                parsed.verdict = parse_review_verdict(value)?;
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown review-approve argument: {other}\n\n{}",
                    review_approve_usage(program)
                )));
            }
        }
        idx += 1;
    }

    if parsed.task_id.0.trim().is_empty() {
        return Err(MainError::Args(
            "missing required --task-id <id> for review-approve".to_string(),
        ));
    }
    if !reviewer_provided {
        return Err(MainError::Args("missing value for --reviewer".to_string()));
    }

    Ok(CliCommand::ReviewApprove(parsed))
}

fn parse_enabled_models(raw: &str) -> Result<Vec<ModelKind>, MainError> {
    let mut models = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        models.push(parse_model_kind(token)?);
    }

    if models.is_empty() {
        return Err(MainError::Args(
            "--enable requires at least one model (claude,codex,gemini)".to_string(),
        ));
    }

    Ok(models)
}

fn parse_model_kind(value: &str) -> Result<ModelKind, MainError> {
    match value.to_ascii_lowercase().as_str() {
        "claude" => Ok(ModelKind::Claude),
        "codex" => Ok(ModelKind::Codex),
        "gemini" => Ok(ModelKind::Gemini),
        other => Err(MainError::Args(format!(
            "unknown model '{other}' (expected claude|codex|gemini)"
        ))),
    }
}

fn parse_review_verdict(value: &str) -> Result<ReviewVerdict, MainError> {
    match value.to_ascii_lowercase().as_str() {
        "approve" => Ok(ReviewVerdict::Approve),
        "request_changes" => Ok(ReviewVerdict::RequestChanges),
        "block" => Ok(ReviewVerdict::Block),
        other => Err(MainError::Args(format!(
            "unknown verdict '{other}' (expected approve|request_changes|block)"
        ))),
    }
}

fn default_run_args() -> RunCliArgs {
    RunCliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        repos_config_dir: PathBuf::from(DEFAULT_REPOS_CONFIG_DIR),
        sqlite_path: PathBuf::from(DEFAULT_SQLITE_PATH),
        event_log_root: PathBuf::from(DEFAULT_EVENT_LOG_ROOT),
        once: false,
    }
}

fn default_tui_args() -> TuiCliArgs {
    TuiCliArgs {
        tick_ms: DEFAULT_TUI_TICK_MS,
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        repos_config_dir: PathBuf::from(DEFAULT_REPOS_CONFIG_DIR),
        sqlite_path: PathBuf::from(DEFAULT_SQLITE_PATH),
        event_log_root: PathBuf::from(DEFAULT_EVENT_LOG_ROOT),
    }
}

fn usage(program: &str) -> String {
    format!(
        "Usage:\n  {program}\n  {program} tui [--tick-ms <u64>] [--org-config <path>] [--repos-config-dir <path>] [--sqlite-path <path>] [--event-log-root <path>]\n  {program} daemon [--org-config <path>] [--repos-config-dir <path>] [--sqlite-path <path>] [--event-log-root <path>] [--once]\n  {program} setup [--org-config <path>] [--enable <models>] [--per-model-concurrency <n>]\n  {program} wizard [--org-config <path>] [--per-model-concurrency <n>]\n  {program} create-task --spec <path> [--org-config <path>] [--repos-config-dir <path>] [--sqlite-path <path>] [--event-log-root <path>]\n  {program} list-tasks [--org-config <path>] [--sqlite-path <path>] [--event-log-root <path>]\n  {program} review-approve --task-id <id> --reviewer <claude|codex|gemini> [--verdict <approve|request_changes|block>] [--org-config <path>] [--sqlite-path <path>] [--event-log-root <path>]\n\
\nDefaults:\n  {program}: launches TUI\n  tui --tick-ms 250\n  --org-config config/org.toml\n  --repos-config-dir config/repos\n  --sqlite-path .orch/state.sqlite\n  --event-log-root .orch/events"
    )
}

fn tui_usage(program: &str) -> String {
    format!(
        "Usage: {program} tui [--tick-ms <u64>] [--org-config <path>] [--repos-config-dir <path>] [--sqlite-path <path>] [--event-log-root <path>]\n\
Defaults:\n\
  --tick-ms 250\n\
  --org-config config/org.toml\n\
  --repos-config-dir config/repos\n\
  --sqlite-path .orch/state.sqlite"
    )
}

fn setup_usage(program: &str) -> String {
    format!(
        "Usage: {program} setup [--org-config <path>] [--enable <models>] [--per-model-concurrency <n>]\n\
\nExamples:\n  {program} setup\n  {program} setup --enable claude,codex --per-model-concurrency 5\n\
\nNotes:\n  --enable accepts comma-separated values from claude|codex|gemini\n  if --enable is omitted, all runnable detected model CLIs are selected"
    )
}

fn wizard_usage(program: &str) -> String {
    format!(
        "Usage: {program} wizard [--org-config <path>] [--per-model-concurrency <n>]\n\
\nExamples:\n  {program} wizard\n  {program} wizard --org-config ~/.config/othala/org.toml\n\
\nNotes:\n  wizard is interactive and requires a TTY\n  prompts for enabled models from detected runnable CLIs"
    )
}

fn create_task_usage(program: &str) -> String {
    format!(
        "Usage: {program} create-task --spec <path> [--org-config <path>] [--repos-config-dir <path>] [--sqlite-path <path>] [--event-log-root <path>]\n\
\nNotes:\n  --spec expects JSON matching orch_core::types::TaskSpec"
    )
}

fn list_tasks_usage(program: &str) -> String {
    format!(
        "Usage: {program} list-tasks [--org-config <path>] [--sqlite-path <path>] [--event-log-root <path>]"
    )
}

fn review_approve_usage(program: &str) -> String {
    format!(
        "Usage: {program} review-approve --task-id <id> --reviewer <claude|codex|gemini> [--verdict <approve|request_changes|block>] [--org-config <path>] [--sqlite-path <path>] [--event-log-root <path>]"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        build_task_activity_lines, create_task_usage, execute_tui_action, list_tasks_usage,
        load_repo_configs, parse_cli_args, parse_enabled_models, setup_usage, tui_usage, usage,
        wizard_usage, CliCommand, CreateTaskCliArgs, ListTasksCliArgs, ReviewApproveCliArgs,
        RunCliArgs, SetupCliArgs, TuiCliArgs, WizardCliArgs,
    };
    use chrono::Utc;
    use orch_core::config::{
        NixConfig, RepoConfig, RepoGraphiteConfig, VerifyCommands, VerifyConfig,
    };
    use orch_core::events::{
        Event, EventKind, GraphiteHygieneReport, ReviewOutput, ReviewVerdict, TestAssessment,
    };
    use orch_core::state::{
        ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus, VerifyTier,
    };
    use orch_core::types::{EventId, RepoId, SubmitMode, Task, TaskRole, TaskType};
    use orch_core::types::{ModelKind, TaskId};
    use orch_tui::UiAction;
    use orchd::{OrchdService, Scheduler, SchedulerConfig};
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn parse_cli_args_uses_defaults_when_no_flags_are_passed() {
        let parsed = parse_cli_args(Vec::new(), "orchd").expect("parse");
        assert_eq!(
            parsed,
            CliCommand::Tui(TuiCliArgs {
                tick_ms: 250,
                org_config_path: PathBuf::from("config/org.toml"),
                repos_config_dir: PathBuf::from("config/repos"),
                sqlite_path: PathBuf::from(".orch/state.sqlite"),
                event_log_root: PathBuf::from(".orch/events"),
            })
        );
    }

    #[test]
    fn parse_cli_args_daemon_subcommand_uses_run_defaults() {
        let parsed = parse_cli_args(vec!["daemon".to_string()], "orchd").expect("parse daemon");
        assert_eq!(
            parsed,
            CliCommand::Run(RunCliArgs {
                org_config_path: PathBuf::from("config/org.toml"),
                repos_config_dir: PathBuf::from("config/repos"),
                sqlite_path: PathBuf::from(".orch/state.sqlite"),
                event_log_root: PathBuf::from(".orch/events"),
                once: false,
            })
        );
    }

    #[test]
    fn parse_cli_args_tui_subcommand_parses_overrides() {
        let parsed = parse_cli_args(
            vec![
                "tui".to_string(),
                "--tick-ms".to_string(),
                "500".to_string(),
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--repos-config-dir".to_string(),
                "/tmp/repos".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
                "--event-log-root".to_string(),
                "/tmp/events".to_string(),
            ],
            "orchd",
        )
        .expect("parse tui");
        assert_eq!(
            parsed,
            CliCommand::Tui(TuiCliArgs {
                tick_ms: 500,
                org_config_path: PathBuf::from("/tmp/org.toml"),
                repos_config_dir: PathBuf::from("/tmp/repos"),
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                event_log_root: PathBuf::from("/tmp/events"),
            })
        );
    }

    #[test]
    fn parse_cli_args_tui_help_returns_usage() {
        let parsed =
            parse_cli_args(vec!["tui".to_string(), "--help".to_string()], "orchd").expect("help");
        assert_eq!(parsed, CliCommand::Help(tui_usage("orchd")));
    }

    #[test]
    fn parse_cli_args_applies_explicit_paths_and_once_mode() {
        let parsed = parse_cli_args(
            vec![
                "daemon".to_string(),
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--repos-config-dir".to_string(),
                "/tmp/repos".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
                "--event-log-root".to_string(),
                "/tmp/events".to_string(),
                "--once".to_string(),
            ],
            "orchd",
        )
        .expect("parse");

        assert_eq!(
            parsed,
            CliCommand::Run(RunCliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                repos_config_dir: PathBuf::from("/tmp/repos"),
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                event_log_root: PathBuf::from("/tmp/events"),
                once: true,
            })
        );
    }

    #[test]
    fn parse_cli_args_reports_unknown_arguments_with_usage() {
        let err = parse_cli_args(vec!["--bad-flag".to_string()], "orchd")
            .expect_err("unknown arg should fail");
        let rendered = err.to_string();
        assert!(rendered.contains("unknown tui argument: --bad-flag"));
        assert!(rendered.contains("Usage:"));
    }

    #[test]
    fn parse_cli_args_reports_unknown_command_with_usage() {
        let err = parse_cli_args(vec!["unknown".to_string()], "orchd")
            .expect_err("unknown command should fail");
        let rendered = err.to_string();
        assert!(rendered.contains("unknown command: unknown"));
        assert!(rendered.contains("Usage:"));
    }

    #[test]
    fn parse_cli_args_requires_values_for_path_flags() {
        let err = parse_cli_args(vec!["--sqlite-path".to_string()], "orchd")
            .expect_err("missing sqlite path should fail");
        assert_eq!(err.to_string(), "missing value for --sqlite-path");

        let err = parse_cli_args(vec!["--org-config".to_string()], "orchd")
            .expect_err("missing org config should fail");
        assert_eq!(err.to_string(), "missing value for --org-config");

        let err = parse_cli_args(vec!["--event-log-root".to_string()], "orchd")
            .expect_err("missing event log root should fail");
        assert_eq!(err.to_string(), "missing value for --event-log-root");

        let err = parse_cli_args(vec!["--repos-config-dir".to_string()], "orchd")
            .expect_err("missing repos config dir should fail");
        assert_eq!(err.to_string(), "missing value for --repos-config-dir");
    }

    #[test]
    fn parse_cli_args_help_returns_usage_message() {
        let parsed = parse_cli_args(vec!["--help".to_string()], "orchd")
            .expect("help should return command");
        assert_eq!(parsed, CliCommand::Help(usage("orchd")));
    }

    #[test]
    fn parse_cli_args_setup_defaults() {
        let parsed = parse_cli_args(vec!["setup".to_string()], "orchd").expect("parse setup");
        assert_eq!(
            parsed,
            CliCommand::Setup(SetupCliArgs {
                org_config_path: PathBuf::from("config/org.toml"),
                enabled_models: None,
                per_model_concurrency: None,
            })
        );
    }

    #[test]
    fn parse_cli_args_setup_with_explicit_models_and_concurrency() {
        let parsed = parse_cli_args(
            vec![
                "setup".to_string(),
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--enable".to_string(),
                "claude,codex".to_string(),
                "--enable".to_string(),
                "gemini".to_string(),
                "--per-model-concurrency".to_string(),
                "7".to_string(),
            ],
            "orchd",
        )
        .expect("parse setup");

        assert_eq!(
            parsed,
            CliCommand::Setup(SetupCliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                enabled_models: Some(vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini]),
                per_model_concurrency: Some(7),
            })
        );
    }

    #[test]
    fn parse_cli_args_setup_help_returns_setup_usage() {
        let parsed = parse_cli_args(vec!["setup".to_string(), "--help".to_string()], "orchd")
            .expect("setup help");
        assert_eq!(parsed, CliCommand::Help(setup_usage("orchd")));
    }

    #[test]
    fn parse_cli_args_wizard_defaults() {
        let parsed = parse_cli_args(vec!["wizard".to_string()], "orchd").expect("wizard parse");
        assert_eq!(
            parsed,
            CliCommand::Wizard(WizardCliArgs {
                org_config_path: PathBuf::from("config/org.toml"),
                per_model_concurrency: None,
            })
        );
    }

    #[test]
    fn parse_cli_args_wizard_with_overrides() {
        let parsed = parse_cli_args(
            vec![
                "wizard".to_string(),
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--per-model-concurrency".to_string(),
                "11".to_string(),
            ],
            "orchd",
        )
        .expect("wizard parse");
        assert_eq!(
            parsed,
            CliCommand::Wizard(WizardCliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                per_model_concurrency: Some(11),
            })
        );
    }

    #[test]
    fn parse_cli_args_wizard_help_returns_usage() {
        let parsed = parse_cli_args(vec!["wizard".to_string(), "--help".to_string()], "orchd")
            .expect("wizard help");
        assert_eq!(parsed, CliCommand::Help(wizard_usage("orchd")));
    }

    #[test]
    fn parse_cli_args_create_task_requires_spec() {
        let err = parse_cli_args(vec!["create-task".to_string()], "orchd")
            .expect_err("missing --spec should fail");
        assert_eq!(
            err.to_string(),
            "missing required --spec <path> for create-task"
        );
    }

    #[test]
    fn parse_cli_args_create_task_parses_paths() {
        let parsed = parse_cli_args(
            vec![
                "create-task".to_string(),
                "--spec".to_string(),
                "/tmp/task.json".to_string(),
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--repos-config-dir".to_string(),
                "/tmp/repos".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
                "--event-log-root".to_string(),
                "/tmp/events".to_string(),
            ],
            "orchd",
        )
        .expect("create-task parse");

        assert_eq!(
            parsed,
            CliCommand::CreateTask(CreateTaskCliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                repos_config_dir: PathBuf::from("/tmp/repos"),
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                event_log_root: PathBuf::from("/tmp/events"),
                spec_path: PathBuf::from("/tmp/task.json"),
            })
        );
    }

    #[test]
    fn parse_cli_args_create_task_help_returns_usage() {
        let parsed = parse_cli_args(
            vec!["create-task".to_string(), "--help".to_string()],
            "orchd",
        )
        .expect("create-task help");
        assert_eq!(parsed, CliCommand::Help(create_task_usage("orchd")));
    }

    #[test]
    fn parse_cli_args_list_tasks_defaults() {
        let parsed =
            parse_cli_args(vec!["list-tasks".to_string()], "orchd").expect("list-tasks parse");
        assert_eq!(
            parsed,
            CliCommand::ListTasks(ListTasksCliArgs {
                org_config_path: PathBuf::from("config/org.toml"),
                sqlite_path: PathBuf::from(".orch/state.sqlite"),
                event_log_root: PathBuf::from(".orch/events"),
            })
        );
    }

    #[test]
    fn parse_cli_args_list_tasks_overrides_paths() {
        let parsed = parse_cli_args(
            vec![
                "list-tasks".to_string(),
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
                "--event-log-root".to_string(),
                "/tmp/events".to_string(),
            ],
            "orchd",
        )
        .expect("list-tasks parse");
        assert_eq!(
            parsed,
            CliCommand::ListTasks(ListTasksCliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                event_log_root: PathBuf::from("/tmp/events"),
            })
        );
    }

    #[test]
    fn parse_cli_args_review_approve_parses_required_and_optional_fields() {
        let parsed = parse_cli_args(
            vec![
                "review-approve".to_string(),
                "--task-id".to_string(),
                "T9".to_string(),
                "--reviewer".to_string(),
                "gemini".to_string(),
                "--verdict".to_string(),
                "request_changes".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
                "--event-log-root".to_string(),
                "/tmp/events".to_string(),
            ],
            "orchd",
        )
        .expect("parse review-approve");

        assert_eq!(
            parsed,
            CliCommand::ReviewApprove(ReviewApproveCliArgs {
                org_config_path: PathBuf::from("config/org.toml"),
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                event_log_root: PathBuf::from("/tmp/events"),
                task_id: TaskId("T9".to_string()),
                reviewer: ModelKind::Gemini,
                verdict: ReviewVerdict::RequestChanges,
            })
        );
    }

    #[test]
    fn parse_cli_args_review_approve_requires_task_id_and_reviewer() {
        let err = parse_cli_args(
            vec![
                "review-approve".to_string(),
                "--reviewer".to_string(),
                "codex".to_string(),
            ],
            "orchd",
        )
        .expect_err("missing task id should fail");
        assert_eq!(
            err.to_string(),
            "missing required --task-id <id> for review-approve"
        );

        let err = parse_cli_args(
            vec![
                "review-approve".to_string(),
                "--task-id".to_string(),
                "T1".to_string(),
            ],
            "orchd",
        )
        .expect_err("missing reviewer should fail");
        assert_eq!(err.to_string(), "missing value for --reviewer");
    }

    #[test]
    fn parse_cli_args_list_tasks_help_returns_usage() {
        let parsed = parse_cli_args(
            vec!["list-tasks".to_string(), "--help".to_string()],
            "orchd",
        )
        .expect("list-tasks help");
        assert_eq!(parsed, CliCommand::Help(list_tasks_usage("orchd")));
    }

    #[test]
    fn parse_enabled_models_rejects_unknown_model() {
        let err = parse_enabled_models("claude,unknown").expect_err("unknown model should fail");
        assert!(err.to_string().contains("unknown model 'unknown'"));
    }

    #[test]
    fn load_repo_configs_reads_only_toml_files_sorted_by_path() {
        let root = std::env::temp_dir().join(format!(
            "othala-orchd-load-repos-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create temp repo dir");
        fs::write(
            root.join("b.toml"),
            r#"
repo_id = "b"
repo_path = "/tmp/b"
base_branch = "main"

[nix]
dev_shell = "nix develop"

[verify.quick]
commands = ["nix develop -c just test"]

[verify.full]
commands = ["nix develop -c just test-all"]

[graphite]
draft_on_start = true
submit_mode = "single"
"#,
        )
        .expect("write b.toml");
        fs::write(
            root.join("a.toml"),
            r#"
repo_id = "a"
repo_path = "/tmp/a"
base_branch = "main"

[nix]
dev_shell = "nix develop"

[verify.quick]
commands = ["nix develop -c just test"]

[verify.full]
commands = ["nix develop -c just test-all"]

[graphite]
draft_on_start = true
submit_mode = "single"
"#,
        )
        .expect("write a.toml");
        fs::write(root.join("notes.txt"), "ignore me").expect("write non-toml");

        let loaded = load_repo_configs(&root).expect("load repo configs");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].1.repo_id, "a");
        assert_eq!(loaded[1].1.repo_id, "b");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_repo_configs_returns_empty_when_directory_is_missing() {
        let root = std::env::temp_dir().join(format!(
            "othala-orchd-missing-repos-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let loaded = load_repo_configs(&root).expect("missing dir should be treated as empty");
        assert!(loaded.is_empty());
    }

    fn mk_reviewing_task(task_id: &str) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId(task_id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: "review".to_string(),
            state: TaskState::Reviewing,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            branch_name: Some(format!("task/{task_id}")),
            worktree_path: PathBuf::from(format!(".orch/wt/{task_id}")),
            pr: None,
            verify_status: VerifyStatus::Passed {
                tier: VerifyTier::Quick,
            },
            review_status: ReviewStatus {
                required_models: Vec::new(),
                approvals_received: 0,
                approvals_required: 0,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            created_at: now,
            updated_at: now,
        }
    }

    fn mk_task_with_state(task_id: &str, state: TaskState) -> Task {
        let mut task = mk_reviewing_task(task_id);
        task.state = state;
        task
    }

    fn mk_service(root: &Path) -> OrchdService {
        let sqlite = root.join("state.sqlite");
        let events = root.join("events");
        let org = super::default_org_config();
        let scheduler = Scheduler::new(SchedulerConfig::from_org_config(&org));
        OrchdService::open(sqlite, events, scheduler).expect("open service")
    }

    fn mk_repo_config(repo_id: &str, root: &Path) -> RepoConfig {
        RepoConfig {
            repo_id: repo_id.to_string(),
            repo_path: root.to_path_buf(),
            base_branch: "main".to_string(),
            nix: NixConfig {
                dev_shell: "nix develop".to_string(),
            },
            verify: VerifyConfig {
                quick: VerifyCommands {
                    commands: vec!["nix develop -c true".to_string()],
                },
                full: VerifyCommands {
                    commands: vec!["nix develop -c true".to_string()],
                },
            },
            graphite: RepoGraphiteConfig {
                draft_on_start: true,
                submit_mode: Some(SubmitMode::Single),
            },
        }
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git_repo(root: &Path) {
        run_git(root, &["init"]);
        fs::write(root.join("README.md"), "init\n").expect("write readme");
        run_git(root, &["add", "README.md"]);
        run_git(
            root,
            &[
                "-c",
                "user.name=Test User",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
        );
    }

    fn mk_chat_history(root: &Path) -> super::TuiChatHistory {
        let history = super::TuiChatHistory {
            root: root.to_path_buf(),
        };
        history.ensure_layout().expect("create chat history root");
        history
    }

    fn spawn_test_agent_session(task_id: &TaskId, script: &str) -> super::TuiAgentSession {
        let mut command = Command::new("bash");
        command
            .args(["-lc", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn test agent session");
        let (tx, rx) = mpsc::channel::<String>();
        if let Some(stdout) = child.stdout.take() {
            super::spawn_pipe_reader(stdout, tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            super::spawn_pipe_reader(stderr, tx.clone());
        }
        drop(tx);

        super::TuiAgentSession {
            instance_id: format!("A-{}", task_id.0),
            task_id: task_id.clone(),
            model: ModelKind::Codex,
            child,
            output_rx: rx,
        }
    }

    fn poll_supervisor_until_session_finishes(
        supervisor: &mut super::TuiAgentSupervisor,
        history: &super::TuiChatHistory,
    ) {
        for _ in 0..60 {
            let _ = supervisor.poll_events(history);
            if supervisor.sessions_by_task.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn poll_events_enqueues_completed_task_for_auto_submit_when_agent_exits_cleanly() {
        let root = std::env::temp_dir().join(format!(
            "othala-main-agent-clean-exit-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let history = mk_chat_history(&root);
        let mut supervisor = super::TuiAgentSupervisor::default();
        let task_id = TaskId("T-CLEAN-EXIT".to_string());
        supervisor.sessions_by_task.insert(
            task_id.0.clone(),
            spawn_test_agent_session(&task_id, "printf 'done\\n'"),
        );

        poll_supervisor_until_session_finishes(&mut supervisor, &history);

        assert!(supervisor.sessions_by_task.is_empty());
        assert_eq!(supervisor.drain_completed_tasks(), vec![task_id.clone()]);
        assert!(supervisor.drain_patch_ready_tasks().is_empty());
        assert!(supervisor.drain_need_human_tasks().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn poll_events_uses_patch_ready_signal_without_clean_exit_fallback() {
        let root = std::env::temp_dir().join(format!(
            "othala-main-agent-patch-ready-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let history = mk_chat_history(&root);
        let mut supervisor = super::TuiAgentSupervisor::default();
        let task_id = TaskId("T-PATCH-READY".to_string());
        supervisor.sessions_by_task.insert(
            task_id.0.clone(),
            spawn_test_agent_session(&task_id, "printf '[patch_ready]\\n'"),
        );

        poll_supervisor_until_session_finishes(&mut supervisor, &history);

        assert!(supervisor.sessions_by_task.is_empty());
        assert_eq!(supervisor.drain_patch_ready_tasks(), vec![task_id.clone()]);
        assert!(supervisor.drain_completed_tasks().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn poll_events_keeps_conflict_sessions_out_of_auto_submit_fallback() {
        let root = std::env::temp_dir().join(format!(
            "othala-main-agent-conflict-exit-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let history = mk_chat_history(&root);
        let mut supervisor = super::TuiAgentSupervisor::default();
        let task_id = TaskId("T-CONFLICT-EXIT".to_string());
        supervisor.sessions_by_task.insert(
            task_id.0.clone(),
            spawn_test_agent_session(&task_id, "printf 'done\\n'"),
        );
        supervisor
            .conflict_resolution_tasks
            .insert(task_id.0.clone());

        poll_supervisor_until_session_finishes(&mut supervisor, &history);

        assert!(supervisor.sessions_by_task.is_empty());
        assert!(supervisor.drain_completed_tasks().is_empty());
        assert_eq!(supervisor.drain_need_human_tasks(), vec![task_id.clone()]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn execute_tui_action_approve_task_records_manual_approvals() {
        let root = std::env::temp_dir().join(format!(
            "othala-main-approve-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create test root");
        let service = mk_service(&root);
        let org = super::default_org_config();
        let task = mk_reviewing_task("T-APPROVE");
        service
            .create_task(
                &task,
                &super::Event {
                    id: EventId("E-TEST-CREATE".to_string()),
                    task_id: Some(task.id.clone()),
                    repo_id: Some(task.repo_id.clone()),
                    at: Utc::now(),
                    kind: EventKind::TaskCreated,
                },
            )
            .expect("create task");
        let repo_configs = HashMap::new();
        let mut agent_supervisor = super::TuiAgentSupervisor::default();

        let outcome = execute_tui_action(
            UiAction::ApproveTask,
            Some(&task.id),
            None,
            None,
            &service,
            &org,
            &org.models.enabled,
            &super::SetupProbeConfig::default(),
            &repo_configs,
            &mut agent_supervisor,
            Utc::now(),
        )
        .expect("approve action");

        let approvals = service.task_approvals(&task.id).expect("approvals");
        assert_eq!(approvals.len(), org.models.enabled.len());
        assert!(outcome.force_tick);
        assert!(outcome.message.contains("recorded APPROVE"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn execute_tui_action_create_task_creates_queued_task() {
        let root = std::env::temp_dir().join(format!(
            "othala-main-create-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create test root");
        let service = mk_service(&root);
        let org = super::default_org_config();
        let mut repo_configs = HashMap::new();
        repo_configs.insert("example".to_string(), mk_repo_config("example", &root));
        let mut agent_supervisor = super::TuiAgentSupervisor::default();

        let outcome = execute_tui_action(
            UiAction::CreateTask,
            None,
            Some("Build OAuth login with callback flow"),
            Some(ModelKind::Claude),
            &service,
            &org,
            &org.models.enabled,
            &super::SetupProbeConfig::default(),
            &repo_configs,
            &mut agent_supervisor,
            Utc::now(),
        )
        .expect("create action");

        assert!(outcome.message.contains("created task"));
        let tasks = service.list_tasks().expect("list tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Queued);
        assert_eq!(tasks[0].repo_id.0, "example");
        assert_eq!(tasks[0].title, "Build OAuth login with callback flow");
        assert_eq!(tasks[0].preferred_model, Some(ModelKind::Claude));
        assert_eq!(agent_supervisor.pending_start_by_task.len(), 1);
        assert!(agent_supervisor
            .pending_start_by_task
            .contains_key(&tasks[0].id.0));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn execute_tui_action_delete_task_removes_task_branch_and_worktree() {
        let root = std::env::temp_dir().join(format!(
            "othala-main-delete-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create test root");
        init_git_repo(&root);
        run_git(&root, &["branch", "task/T-DEL"]);
        fs::create_dir_all(root.join(".orch/wt")).expect("create worktree root");
        run_git(&root, &["worktree", "add", ".orch/wt/T-DEL", "task/T-DEL"]);

        let service = mk_service(&root);
        let org = super::default_org_config();
        let task = mk_reviewing_task("T-DEL");
        service
            .create_task(
                &task,
                &super::Event {
                    id: EventId("E-TEST-CREATE-DEL".to_string()),
                    task_id: Some(task.id.clone()),
                    repo_id: Some(task.repo_id.clone()),
                    at: Utc::now(),
                    kind: EventKind::TaskCreated,
                },
            )
            .expect("create task");

        let mut repo_configs = HashMap::new();
        repo_configs.insert("example".to_string(), mk_repo_config("example", &root));
        let mut agent_supervisor = super::TuiAgentSupervisor::default();

        let outcome = execute_tui_action(
            UiAction::DeleteTask,
            Some(&task.id),
            None,
            None,
            &service,
            &org,
            &org.models.enabled,
            &super::SetupProbeConfig::default(),
            &repo_configs,
            &mut agent_supervisor,
            Utc::now(),
        )
        .expect("delete action");

        assert!(outcome.message.contains("deleted task T-DEL"));
        assert!(service.task(&task.id).expect("load task").is_none());
        assert!(!root.join(".orch/wt/T-DEL").exists());

        let branch_list = Command::new("git")
            .args(["branch", "--list", "task/T-DEL"])
            .current_dir(&root)
            .output()
            .expect("list branch");
        assert!(
            branch_list.status.success(),
            "git branch --list failed: {}",
            String::from_utf8_lossy(&branch_list.stderr)
        );
        assert!(
            String::from_utf8_lossy(&branch_list.stdout)
                .trim()
                .is_empty(),
            "branch should be removed"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn execute_tui_action_delete_task_infers_default_branch_when_branch_name_missing() {
        let root = std::env::temp_dir().join(format!(
            "othala-main-delete-missing-branch-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create test root");
        init_git_repo(&root);
        run_git(&root, &["branch", "task/T-MISSING"]);
        fs::create_dir_all(root.join(".orch/wt")).expect("create worktree root");
        run_git(
            &root,
            &["worktree", "add", ".orch/wt/T-MISSING", "task/T-MISSING"],
        );

        let service = mk_service(&root);
        let org = super::default_org_config();
        let mut task = mk_reviewing_task("T-MISSING");
        task.branch_name = None;
        service
            .create_task(
                &task,
                &super::Event {
                    id: EventId("E-TEST-CREATE-DEL-MISSING".to_string()),
                    task_id: Some(task.id.clone()),
                    repo_id: Some(task.repo_id.clone()),
                    at: Utc::now(),
                    kind: EventKind::TaskCreated,
                },
            )
            .expect("create task");

        let mut repo_configs = HashMap::new();
        repo_configs.insert("example".to_string(), mk_repo_config("example", &root));
        let mut agent_supervisor = super::TuiAgentSupervisor::default();

        let outcome = execute_tui_action(
            UiAction::DeleteTask,
            Some(&task.id),
            None,
            None,
            &service,
            &org,
            &org.models.enabled,
            &super::SetupProbeConfig::default(),
            &repo_configs,
            &mut agent_supervisor,
            Utc::now(),
        )
        .expect("delete action");

        assert!(outcome.message.contains("deleted task T-MISSING"));
        assert!(service.task(&task.id).expect("load task").is_none());
        assert!(!root.join(".orch/wt/T-MISSING").exists());

        let branch_list = Command::new("git")
            .args(["branch", "--list", "task/T-MISSING"])
            .current_dir(&root)
            .output()
            .expect("list branch");
        assert!(
            branch_list.status.success(),
            "git branch --list failed: {}",
            String::from_utf8_lossy(&branch_list.stderr)
        );
        assert!(
            String::from_utf8_lossy(&branch_list.stdout)
                .trim()
                .is_empty(),
            "branch should be removed"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn execute_tui_action_trigger_restack_submits_when_task_in_restack_conflict() {
        let root = std::env::temp_dir().join(format!(
            "othala-main-restack-submit-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create test root");
        let service = mk_service(&root);
        let org = super::default_org_config();
        let task = mk_task_with_state("T-RESTACK-CONFLICT", TaskState::RestackConflict);
        service
            .create_task(
                &task,
                &super::Event {
                    id: EventId("E-TEST-RESTACK-SUBMIT-CREATE".to_string()),
                    task_id: Some(task.id.clone()),
                    repo_id: Some(task.repo_id.clone()),
                    at: Utc::now(),
                    kind: EventKind::TaskCreated,
                },
            )
            .expect("create task");
        let repo_configs = HashMap::new();
        let mut agent_supervisor = super::TuiAgentSupervisor::default();

        let outcome = execute_tui_action(
            UiAction::TriggerRestack,
            Some(&task.id),
            None,
            None,
            &service,
            &org,
            &org.models.enabled,
            &super::SetupProbeConfig::default(),
            &repo_configs,
            &mut agent_supervisor,
            Utc::now(),
        )
        .expect("trigger restack action");

        assert!(outcome.force_tick);
        assert!(outcome.message.contains("started graphite submit"));

        let updated = service
            .task(&task.id)
            .expect("load task")
            .expect("task exists");
        assert_eq!(updated.state, TaskState::Submitting);
        let events = service.task_events(&task.id).expect("events");
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, EventKind::SubmitStarted { .. })));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn execute_tui_action_trigger_restack_submits_when_task_failed() {
        let root = std::env::temp_dir().join(format!(
            "othala-main-failed-submit-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create test root");
        let service = mk_service(&root);
        let org = super::default_org_config();
        let task = mk_task_with_state("T-FAILED-SUBMIT", TaskState::Failed);
        service
            .create_task(
                &task,
                &super::Event {
                    id: EventId("E-TEST-FAILED-SUBMIT-CREATE".to_string()),
                    task_id: Some(task.id.clone()),
                    repo_id: Some(task.repo_id.clone()),
                    at: Utc::now(),
                    kind: EventKind::TaskCreated,
                },
            )
            .expect("create task");
        let repo_configs = HashMap::new();
        let mut agent_supervisor = super::TuiAgentSupervisor::default();

        let outcome = execute_tui_action(
            UiAction::TriggerRestack,
            Some(&task.id),
            None,
            None,
            &service,
            &org,
            &org.models.enabled,
            &super::SetupProbeConfig::default(),
            &repo_configs,
            &mut agent_supervisor,
            Utc::now(),
        )
        .expect("trigger restack action");

        assert!(outcome.force_tick);
        assert!(outcome.message.contains("started graphite submit"));

        let updated = service
            .task(&task.id)
            .expect("load task")
            .expect("task exists");
        assert_eq!(updated.state, TaskState::Submitting);
        let events = service.task_events(&task.id).expect("events");
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, EventKind::SubmitStarted { .. })));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn build_task_activity_lines_contains_review_details_from_events() {
        let task = mk_reviewing_task("T-LINES");
        let event = Event {
            id: EventId("E-LINES".to_string()),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at: Utc::now(),
            kind: EventKind::ReviewCompleted {
                reviewer: ModelKind::Codex,
                output: ReviewOutput {
                    verdict: ReviewVerdict::RequestChanges,
                    issues: vec![orch_core::events::ReviewIssue {
                        severity: orch_core::events::IssueSeverity::High,
                        file: "src/lib.rs".to_string(),
                        line: Some(42),
                        description: "fix logic".to_string(),
                        suggested_fix: None,
                    }],
                    risk_flags: vec!["API_BREAK".to_string()],
                    graphite_hygiene: GraphiteHygieneReport {
                        ok: false,
                        notes: "stack needs restack".to_string(),
                    },
                    test_assessment: TestAssessment {
                        ok: false,
                        notes: "missing regression test".to_string(),
                    },
                },
            },
        };
        let approval = orch_core::types::TaskApproval {
            task_id: task.id.clone(),
            reviewer: ModelKind::Codex,
            verdict: ReviewVerdict::RequestChanges,
            issued_at: Utc::now(),
        };

        let lines = build_task_activity_lines(&task, &[event], &[approval]);
        let rendered = lines.join("\n");
        assert!(rendered.contains("approvals:"));
        assert!(rendered.contains("events (latest first):"));
        assert!(rendered.contains("review codex verdict=RequestChanges"));
        assert!(rendered.contains("issue High src/lib.rs:42 fix logic"));
        assert!(rendered.contains("risks API_BREAK"));
    }

    #[test]
    fn tui_chat_history_roundtrips_lines_per_task() {
        let root = std::env::temp_dir().join(format!(
            "othala-chat-history-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let history = super::TuiChatHistory { root: root.clone() };
        history.ensure_layout().expect("create history dir");
        let task_id = TaskId("T-HISTORY-1".to_string());
        history
            .append_lines(
                &task_id,
                &["first line".to_string(), "[stderr] second line".to_string()],
            )
            .expect("append lines");

        let loaded = history.load_lines(&task_id).expect("load lines");
        assert_eq!(
            loaded,
            vec!["first line".to_string(), "[stderr] second line".to_string()]
        );

        let _ = fs::remove_dir_all(root);
    }
}
