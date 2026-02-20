//! Othala daemon - MVP version.
//!
//! Simplified CLI for managing AI coding sessions that auto-submit to Graphite.

use chrono::{Datelike, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use orch_git::{discover_repo, list_change_snapshots, redo_snapshot, undo_to_snapshot, GitCli};
use orch_agents::setup::{
    probe_models, summarize_setup, validate_setup_selection, ModelSetupSelection, SetupProbeConfig,
};
use orch_core::config::{
    apply_profile_defaults, apply_setup_selection_to_org_config, load_org_config, save_org_config,
    ConfigProfile, NotificationConfig, OrgConfig,
};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{
    load_task_specs_from_dir, parse_yaml_task_spec, yaml_spec_to_task, EventId, ModelKind, RepoId,
    Session, Task, TaskId, TaskPriority,
};
use orch_core::types::SubmitMode;
use orch_notify::{NotificationDispatcher, NotificationSink, StdoutSink, WebhookSink};
use orchd::supervisor::AgentSupervisor;
use orchd::{
    provision_chat_workspace_on_base, AgentCostEstimate, OrchdService, PermissionPolicy,
    PermissionRule, Scheduler, SchedulerConfig, SkillRegistry, TaskCloneOverrides, ToolCategory,
    ToolPermission,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::io::ErrorKind;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

#[derive(Parser)]
#[command(name = "othala")]
#[command(about = "AI coding orchestrator")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init {
        #[arg(long)]
        force: bool,
    },
    CreateTask {
        /// Repository ID
        #[arg(short, long)]
        repo: String,
        #[arg(short, long)]
        title: String,
        /// Preferred model
        #[arg(short, long, default_value = "claude")]
        model: String,
        #[arg(long, default_value = "normal")]
        priority: String,
        /// Output as JSON (for scripting/E2E tests)
        #[arg(long)]
        json: bool,
    },
    LoadTasks {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    ValidateSpec {
        path: PathBuf,
    },
    SetPriority {
        /// Chat/task ID
        id: String,
        priority: String,
    },
    Tag {
        task_id: String,
        label: String,
    },
    Untag {
        task_id: String,
        label: String,
    },
    Search {
        query: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        state: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    Bulk {
        #[command(subcommand)]
        action: BulkAction,
    },
    /// Create a new chat (AI coding session)
    Chat {
        #[command(subcommand)]
        action: ChatAction,
    },
    /// List all chats
    List {
        /// Output as JSON (for scripting/E2E tests)
        #[arg(long)]
        json: bool,
    },
    Sessions {
        /// Output as JSON (for scripting/E2E tests)
        #[arg(long)]
        json: bool,
    },
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Show chat status
    Status {
        /// Chat/task ID
        id: String,
        /// Output as JSON (for scripting/E2E tests)
        #[arg(long)]
        json: bool,
    },
    /// Delete a chat
    Delete {
        /// Chat/task ID
        id: String,
    },
    Clone {
        task_id: String,
        /// Override title for the clone
        #[arg(long)]
        title: Option<String>,
        /// Override model for the clone
        #[arg(long)]
        model: Option<String>,
        /// Override priority for the clone
        #[arg(long)]
        priority: Option<String>,
    },
    Diff {
        task_id: String,
        /// Show stat summary instead of full diff
        #[arg(long)]
        stat: bool,
    },
    Undo {
        task_id: String,
    },
    Redo {
        task_id: String,
    },
    /// Run the daemon (orchestration loop)
    Daemon {
        /// Stop the daemon after N seconds
        #[arg(long)]
        timeout: Option<u64>,
        /// Exit when all tasks reach a terminal state (merged/stopped/awaiting_merge)
        #[arg(long)]
        exit_on_idle: bool,
        /// Skip initial context generation (faster startup for testing)
        #[arg(long)]
        skip_context_gen: bool,
        /// Override the default verify command
        #[arg(long)]
        verify_command: Option<String>,
        /// Skip all QA runs (baseline + validation)
        #[arg(long)]
        skip_qa: bool,
        /// Run a single daemon tick then exit
        #[arg(long)]
        once: bool,
        #[arg(long, value_enum)]
        profile: Option<ConfigProfileArg>,
    },
    Profiles,
    /// Interactive first-time setup wizard
    Wizard {
        /// Enable models non-interactively (comma-separated, e.g. claude,codex)
        #[arg(long)]
        enable: Option<String>,
        /// Per-model concurrency to set in org config
        #[arg(long)]
        per_model_concurrency: Option<usize>,
    },
    /// Validate Othala installation and environment
    SelfTest {
        /// Output as JSON (for scripting)
        #[arg(long)]
        json: bool,
    },
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Show event log for a task (or all tasks)
    Logs {
        /// Task/chat ID (omit for global events)
        id: Option<String>,
        /// Maximum number of events to show
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    Replay {
        /// Task ID to replay (omit for all tasks)
        task_id: Option<String>,
        /// Show events since this ISO timestamp
        #[arg(long)]
        since: Option<String>,
        /// Show events until this ISO timestamp
        #[arg(long)]
        until: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show all events (not just for one task)
        #[arg(long)]
        all: bool,
    },
    /// Show persisted agent output for a task
    Tail {
        /// Task/chat ID
        id: String,
        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
        /// Follow and print new lines
        #[arg(short, long)]
        follow: bool,
    },
    Compact {
        /// Task/chat ID
        task_id: String,
        #[arg(long)]
        max_lines: Option<usize>,
    },
    Watch {
        #[arg(long)]
        task: Option<String>,
        #[arg(short = 'n', long, default_value = "10")]
        lines: usize,
    },
    /// Show agent run history for a task
    Runs {
        /// Task/chat ID
        id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    Retries {
        /// Task/chat ID
        id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    DiffRetries {
        /// Task/chat ID
        task_id: String,
    },
    /// Show aggregate task and agent statistics
    Stats {
        #[arg(long)]
        json: bool,
    },
    Gc {
        #[arg(long, default_value = "30")]
        older_than_days: u64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop a running chat (agent will be killed)
    Stop {
        /// Task/chat ID
        id: String,
    },
    Cancel {
        /// Task/chat ID
        id: String,
    },
    /// Resume a stopped chat
    Resume {
        /// Task/chat ID
        id: String,
    },
    /// Show task dependency tree
    Deps {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    Template {
        #[command(subcommand)]
        action: TemplateAction,
    },
    Export {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        task_id: Option<String>,
    },
    Import {
        #[arg(long)]
        input: PathBuf,
    },
    Costs {
        #[arg(long = "task")]
        task: Option<String>,
        #[arg(long)]
        budget: bool,
    },
    /// Remove old completed/stopped tasks and their data
    Prune {
        /// Only prune tasks older than N days
        #[arg(long, default_value = "7")]
        older_than_days: i64,
        /// Actually delete (default is dry-run showing what would be pruned)
        #[arg(long)]
        force: bool,
    },
    Archive {
        /// Only archive tasks older than N days
        #[arg(long, default_value = "7")]
        older_than_days: i64,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List current permission rules
    Permissions {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Grant permission for a tool category
    Permit {
        /// Tool category (file_read, file_write, shell_exec, git_ops, network, process, package_install, env_access, graphite_ops)
        category: String,
        /// Optional path pattern (for file operations)
        #[arg(long)]
        path: Option<String>,
        /// Optional model to apply to (applies to all if omitted)
        #[arg(long)]
        model: Option<String>,
    },
    /// Deny permission for a tool category
    Deny {
        /// Tool category
        category: String,
        /// Optional path pattern
        #[arg(long)]
        path: Option<String>,
        /// Optional model
        #[arg(long)]
        model: Option<String>,
    },
    /// Start MCP (Model Context Protocol) server on stdin/stdout
    Mcp,
    Skills,
    Skill {
        name: String,
    },
    #[allow(clippy::enum_variant_names)]
    /// List available custom commands
    #[command(name = "commands")]
    ListCommands {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Run a custom command
    RunCommand {
        /// Command name (e.g., "user:review" or "project:deploy")
        name: String,
        /// Arguments as KEY=VALUE pairs
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Execute a single prompt non-interactively (for scripting/CI)
    Prompt {
        /// The prompt text
        text: String,
        /// Model to use
        #[arg(short, long, default_value = "claude")]
        model: String,
        /// Output format: text or json
        #[arg(short, long, default_value = "text")]
        format: String,
        /// Suppress progress output (for piping)
        #[arg(long)]
        quiet: bool,
    },
    /// Check for updates or upgrade Othala
    Upgrade {
        /// Actually install the update (default is check-only)
        #[arg(long)]
        install: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List available models with pricing and capabilities
    Models {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List provider information
    Providers {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show .othalaignore rules
    Ignore {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show usage metrics and telemetry summary
    Metrics {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Generate CI workflow files
    CiGen {
        /// Output directory (default: .github/workflows)
        #[arg(long)]
        output: Option<PathBuf>,
        /// Workflow type: full, verify, nix, basic
        #[arg(long, default_value = "full")]
        workflow: String,
        /// Print to stdout instead of writing file
        #[arg(long)]
        dry_run: bool,
    },
    /// Open $EDITOR to compose a prompt
    Edit {
        /// Task ID to edit prompt for
        task_id: Option<String>,
    },
    /// Show delegation plan for a task
    Delegate {
        /// Parent task ID
        task_id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List or instantiate task templates
    Templates {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show daemon health and status
    Health {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Search tasks with fuzzy matching
    #[command(name = "find")]
    Find {
        /// Search query (supports state:X label:Y syntax)
        query: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Manage LSP language servers
    Lsp {
        #[command(subcommand)]
        action: LspAction,
    },
    /// Show rate limit status
    RateLimits {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show task timeout status
    Timeouts {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show environment variable injection config
    Env {
        /// Task ID to show env for
        #[arg(long)]
        task_id: Option<String>,
        /// Model to show env for
        #[arg(long)]
        model: Option<String>,
        /// Show redacted values
        #[arg(long)]
        redacted: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Start MCP server with HTTP/SSE transport
    McpHttp {
        /// Bind address
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Port number
        #[arg(long, default_value = "9898")]
        port: u16,
    },
    /// Manage conversation history
    Conversations {
        #[command(subcommand)]
        action: ConversationAction,
    },
    /// Show or configure shell settings
    Shell {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ConversationAction {
    /// List conversations for a task
    List {
        #[arg(long)]
        task_id: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show a specific conversation
    Show {
        id: String,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Export conversation as JSON
    Export { id: String },
    /// Search across conversations
    Search {
        query: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum LspAction {
    /// List configured language servers
    List,
    /// Show LSP status and diagnostics cache
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ChatAction {
    /// Create a new chat
    New {
        /// Repository ID
        #[arg(short, long)]
        repo: String,
        /// Chat title/prompt
        #[arg(short, long)]
        title: String,
        /// Preferred model
        #[arg(short, long, default_value = "claude")]
        model: String,
        /// Output as JSON (for scripting/E2E tests)
        #[arg(long)]
        json: bool,
    },
    /// List all chats
    List {
        /// Output as JSON (for scripting/E2E tests)
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    Fork { id: String },
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Subcommand)]
enum BulkAction {
    Retry {
        #[arg(long)]
        state: Option<String>,
        ids: Vec<String>,
    },
    Cancel {
        #[arg(long)]
        state: Option<String>,
        ids: Vec<String>,
    },
    SetPriority {
        priority: String,
        #[arg(long)]
        state: Option<String>,
        ids: Vec<String>,
    },
}

#[derive(Subcommand)]
enum TemplateAction {
    List,
    Create {
        name: String,
        #[arg(long = "from-task")]
        from_task: String,
    },
    Show {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ConfigProfileArg {
    Dev,
    Staging,
    Prod,
    Custom,
}

impl From<ConfigProfileArg> for ConfigProfile {
    fn from(value: ConfigProfileArg) -> Self {
        match value {
            ConfigProfileArg::Dev => ConfigProfile::Dev,
            ConfigProfileArg::Staging => ConfigProfile::Staging,
            ConfigProfileArg::Prod => ConfigProfile::Prod,
            ConfigProfileArg::Custom => ConfigProfile::Custom("custom".to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TaskTemplate {
    title: String,
    description: Option<String>,
    repo_id: String,
    preferred_model: Option<ModelKind>,
    priority: String,
}

impl TaskTemplate {
    fn from_task(task: &Task) -> Self {
        Self {
            title: task.title.clone(),
            description: None,
            repo_id: task.repo_id.0.clone(),
            preferred_model: task.preferred_model,
            priority: task.priority.as_str().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TaskExportRecord {
    task_id: String,
    title: String,
    description: Option<String>,
    state: String,
    priority: String,
    branch_name: Option<String>,
    parent_branch: Option<String>,
    repo_id: String,
    preferred_model: Option<ModelKind>,
}

impl TaskExportRecord {
    fn from_task(task: &Task, parent_branch: Option<String>) -> Self {
        Self {
            task_id: task.id.0.clone(),
            title: task.title.clone(),
            description: None,
            state: format!("{}", task.state),
            priority: task.priority.as_str().to_string(),
            branch_name: task.branch_name.clone(),
            parent_branch,
            repo_id: task.repo_id.0.clone(),
            preferred_model: task.preferred_model,
        }
    }
}

fn validate_template_name(name: &str) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("template name cannot be empty");
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        anyhow::bail!("invalid template name: {name}");
    }
    Ok(())
}

fn templates_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".othala").join("templates")
}

fn template_path(repo_root: &Path, name: &str) -> PathBuf {
    templates_dir(repo_root).join(format!("{name}.json"))
}

fn save_template(repo_root: &Path, name: &str, template: &TaskTemplate) -> anyhow::Result<()> {
    validate_template_name(name)?;
    let dir = templates_dir(repo_root);
    std::fs::create_dir_all(&dir)?;
    let path = template_path(repo_root, name);
    let payload = serde_json::to_string_pretty(template)?;
    std::fs::write(path, payload)?;
    Ok(())
}

fn load_template(repo_root: &Path, name: &str) -> anyhow::Result<TaskTemplate> {
    validate_template_name(name)?;
    let path = template_path(repo_root, name);
    let payload = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str::<TaskTemplate>(&payload)?)
}

fn list_templates(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    let dir = templates_dir(repo_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            names.push(stem.to_string());
        }
    }
    names.sort();
    Ok(names)
}

fn parse_export_state(state: &str) -> anyhow::Result<TaskState> {
    match state.trim().to_uppercase().as_str() {
        "CHATTING" => Ok(TaskState::Chatting),
        "READY" => Ok(TaskState::Ready),
        "SUBMITTING" => Ok(TaskState::Submitting),
        "RESTACKING" => Ok(TaskState::Restacking),
        "AWAITING_MERGE" => Ok(TaskState::AwaitingMerge),
        "MERGED" => Ok(TaskState::Merged),
        "STOPPED" => Ok(TaskState::Stopped),
        other => anyhow::bail!("unknown task state in import: {other}"),
    }
}

fn import_record_to_task(record: TaskExportRecord, existing: Option<Task>) -> anyhow::Result<Task> {
    let now = Utc::now();
    let mut task = if let Some(existing_task) = existing {
        existing_task
    } else {
        Task::new(
            TaskId::new(record.task_id.clone()),
            RepoId(record.repo_id.clone()),
            record.title.clone(),
            PathBuf::from(format!(".orch/wt/{}", record.task_id)),
        )
    };

    task.repo_id = RepoId(record.repo_id);
    task.id = TaskId::new(record.task_id);
    task.title = record.title;
    task.state = parse_export_state(&record.state)?;
    task.priority = parse_task_priority(&record.priority)?;
    task.branch_name = record.branch_name;
    task.preferred_model = record.preferred_model;
    task.updated_at = now;
    Ok(task)
}

fn aggregate_cost_estimates(runs: &[orchd::TaskRunRecord]) -> Vec<AgentCostEstimate> {
    runs.iter()
        .filter_map(|run| {
            run.estimated_tokens.map(|tokens| AgentCostEstimate {
                model: run.model,
                input_tokens: tokens,
                duration_secs: run.duration_secs.unwrap_or(0.0),
            })
        })
        .collect()
}

fn aggregate_budget_usage(runs: &[orchd::TaskRunRecord], now: chrono::DateTime<Utc>) -> (u64, u64) {
    let mut today = 0u64;
    let mut month = 0u64;
    for run in runs {
        let Some(tokens) = run.estimated_tokens else {
            continue;
        };
        if run.started_at.year() == now.year() && run.started_at.month() == now.month() {
            month = month.saturating_add(tokens);
            if run.started_at.day() == now.day() {
                today = today.saturating_add(tokens);
            }
        }
    }
    (today, month)
}

#[derive(Debug, Clone, Serialize)]
struct RetryTimelineEntry {
    attempt: u32,
    model: String,
    started_at: String,
    status: String,
    finished_at: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct RetryHistoryOutput {
    task_id: String,
    retry_events: Vec<Event>,
    timeline: Vec<RetryTimelineEntry>,
}

fn is_retry_related_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::AgentSpawned { .. }
            | EventKind::RetryScheduled { .. }
            | EventKind::AgentCompleted { success: false, .. }
    )
}

fn collect_retry_events(events: &[Event]) -> Vec<Event> {
    let mut filtered: Vec<Event> = events
        .iter()
        .filter(|event| is_retry_related_event(&event.kind))
        .cloned()
        .collect();
    filtered.sort_by_key(|event| event.at);
    filtered
}

fn build_retry_timeline(events: &[Event], runs: &[orchd::TaskRunRecord]) -> Vec<RetryTimelineEntry> {
    let retry_events = collect_retry_events(events);
    let mut retry_reasons: HashMap<u32, String> = HashMap::new();
    for event in &retry_events {
        if let EventKind::RetryScheduled { attempt, reason, .. } = &event.kind {
            retry_reasons.insert(*attempt, reason.clone());
        }
    }

    let mut sorted_runs = runs.to_vec();
    sorted_runs.sort_by_key(|run| run.started_at);

    sorted_runs
        .iter()
        .enumerate()
        .map(|(index, run)| {
            let attempt = (index + 1) as u32;
            let started_at = run.started_at.format("%H:%M:%S").to_string();
            let finished_at = run.finished_at.map(|at| at.format("%H:%M:%S").to_string());

            let status = if run.finished_at.is_none() {
                "running".to_string()
            } else if run.stop_reason.as_deref() == Some("completed") || run.exit_code == Some(0) {
                "completed".to_string()
            } else {
                "failed".to_string()
            };

            let reason = if status == "failed" {
                run.stop_reason
                    .clone()
                    .filter(|r| r != "failed")
                    .or_else(|| retry_reasons.get(&attempt).cloned())
                    .or_else(|| run.stop_reason.clone())
            } else {
                None
            };

            RetryTimelineEntry {
                attempt,
                model: run.model.as_str().to_string(),
                started_at,
                status,
                finished_at,
                reason,
            }
        })
        .collect()
}

fn format_retries_timeline(task_id: &str, timeline: &[RetryTimelineEntry]) -> String {
    if timeline.is_empty() {
        return format!("No retry history for task: {task_id}");
    }

    let mut lines = vec![format!("Retry History for {task_id}:")];
    for entry in timeline {
        let end = entry.finished_at.as_deref().unwrap_or("-");
        let line = if entry.status == "failed" {
            let reason = entry.reason.as_deref().unwrap_or("unknown");
            format!(
                "#{:<2} {:<8} started {}  failed {}  reason: \"{}\"",
                entry.attempt, entry.model, entry.started_at, end, reason
            )
        } else if entry.status == "completed" {
            format!(
                "#{:<2} {:<8} started {}  completed {}",
                entry.attempt, entry.model, entry.started_at, end
            )
        } else {
            format!(
                "#{:<2} {:<8} started {}  running",
                entry.attempt, entry.model, entry.started_at
            )
        };
        lines.push(line);
    }

    lines.join("\n")
}

fn print_banner() {
    eprint!("\x1b[35m");
    eprintln!();
    eprintln!("       \u{2554}\u{2557}");
    eprintln!("      \u{2554}\u{255d}\u{2558}\u{2557}        \u{2554}\u{2550}\u{2557}\u{2554}\u{2566}\u{2557}\u{2566} \u{2566}\u{2554}\u{2550}\u{2557}\u{2566}  \u{2554}\u{2550}\u{2557}");
    eprintln!("     \u{2554}\u{255d}  \u{2558}\u{2557}       \u{2551} \u{2551} \u{2551} \u{2560}\u{2550}\u{2569}\u{2560}\u{2550}\u{2557}\u{2551}  \u{2560}\u{2550}\u{2557}");
    eprintln!("     \u{2558}\u{2557}  \u{2554}\u{255d}       \u{255a}\u{2550}\u{255d} \u{2569} \u{2569} \u{2569}\u{2569} \u{2569}\u{2569}\u{2550}\u{255d}\u{2569} \u{2569}");
    eprintln!("      \u{2558}\u{2557}\u{2554}\u{255d}");
    eprintln!("      \u{2554}\u{255d}\u{2558}\u{2557}        autonomous code orchestrator");
    eprintln!("     \u{2554}\u{255d}  \u{2558}\u{2557}");
    eprintln!();
    eprint!("\x1b[0m");
}

fn run_context_gen_with_status(
    repo_root: &Path,
    template_dir: &Path,
    model: ModelKind,
) -> anyhow::Result<()> {
    use orchd::context_gen::{check_context_startup, parse_progress_line, ContextStartupStatus};

    match check_context_startup(repo_root) {
        ContextStartupStatus::UpToDate => {
            eprintln!("  \x1b[32mContext up to date \u{2713}\x1b[0m");
            return Ok(());
        }
        ContextStartupStatus::Stale => {
            eprintln!("  \x1b[33mContext stale — will regenerate in background\x1b[0m");
            return Ok(());
        }
        ContextStartupStatus::Missing => {
            eprintln!("  \x1b[33mGenerating context...\x1b[0m");
        }
    }

    let repo = repo_root.to_path_buf();
    let tmpl = template_dir.to_path_buf();

    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let (progress_tx, progress_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let ptx = progress_tx;
        let result =
            orchd::context_gen::ensure_context_exists_blocking(&repo, &tmpl, model, move |line| {
                let _ = ptx.send(line.to_string());
            });
        let _ = result_tx.send(result);
    });

    let spinner_frames = [
        '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}',
        '\u{2827}', '\u{2807}', '\u{280f}',
    ];
    let mut frame = 0usize;
    let mut last_status = String::from("starting agent...");

    loop {
        while let Ok(raw) = progress_rx.try_recv() {
            if let Some(parsed) = parse_progress_line(&raw) {
                last_status = parsed;
            }
        }

        match result_rx.try_recv() {
            Ok(result) => {
                eprint!("\r\x1b[2K");
                match &result {
                    Ok(()) => eprintln!("  \x1b[32mContext generated \u{2713}\x1b[0m"),
                    Err(e) => eprintln!("  \x1b[31mContext generation failed: {e}\x1b[0m"),
                }
                return result;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                let spinner = spinner_frames[frame % spinner_frames.len()];
                let display = if last_status.len() > 70 {
                    format!("{}...", &last_status[..70])
                } else {
                    last_status.clone()
                };
                eprint!("\r\x1b[2K  \x1b[35m{spinner}\x1b[0m \x1b[2m{display}\x1b[0m");
                frame += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                eprint!("\r\x1b[2K");
                eprintln!("  \x1b[31mContext generation thread panicked\x1b[0m");
                anyhow::bail!("context generation thread panicked");
            }
        }
    }
}

fn print_task_list(tasks: &[Task], json: bool) {
    if json {
        let out = serde_json::to_string_pretty(tasks).unwrap_or_else(|_| "[]".to_string());
        println!("{out}");
        return;
    }
    if tasks.is_empty() {
        println!("No chats found.");
    } else {
        println!("{:<20} {:<16} {:<40}", "ID", "STATE", "TITLE");
        println!("{}", "-".repeat(76));
        for task in tasks {
            println!(
                "{:<20} {:<16} {:<40}",
                task.id.0,
                format!("{}", task.state),
                task.title
            );
        }
    }
}

fn print_task_json(task: &Task) {
    let out = serde_json::to_string_pretty(task).unwrap_or_else(|_| "{}".to_string());
    println!("{out}");
}

fn print_session_list(sessions: &[Session], json: bool) {
    if json {
        let out = serde_json::to_string_pretty(sessions).unwrap_or_else(|_| "[]".to_string());
        println!("{out}");
        return;
    }
    if sessions.is_empty() {
        println!("No sessions found.");
    } else {
        println!("{:<24} {:<12} {:<8} TITLE", "ID", "STATUS", "TASKS");
        println!("{}", "-".repeat(96));
        for session in sessions {
            println!(
                "{:<24} {:<12} {:<8} {}",
                session.id,
                session.status,
                session.task_ids.len(),
                session.title
            );
        }
    }
}

fn print_session_details(session: &Session, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(session).unwrap_or_else(|_| "{}".to_string())
        );
        return;
    }

    println!("Session: {}", session.id);
    println!("Title: {}", session.title);
    println!("Status: {}", session.status);
    if let Some(parent_session_id) = &session.parent_session_id {
        println!("Parent: {parent_session_id}");
    }
    println!("Task IDs: {}", session.task_ids.len());
    if !session.task_ids.is_empty() {
        println!(
            "Tasks: {}",
            session
                .task_ids
                .iter()
                .map(|task_id| task_id.0.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!("Created: {}", session.created_at);
    println!("Updated: {}", session.updated_at);
}

fn parse_model(s: &str) -> ModelKind {
    match s.to_lowercase().as_str() {
        "codex" => ModelKind::Codex,
        "gemini" => ModelKind::Gemini,
        _ => ModelKind::Claude,
    }
}

fn parse_model_name(s: &str) -> Option<ModelKind> {
    match s.trim().to_lowercase().as_str() {
        "claude" => Some(ModelKind::Claude),
        "codex" => Some(ModelKind::Codex),
        "gemini" => Some(ModelKind::Gemini),
        _ => None,
    }
}

fn default_repo_id_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("default")
        .to_string()
}

fn profile_label(profile: &ConfigProfile) -> String {
    match profile {
        ConfigProfile::Dev => "dev".to_string(),
        ConfigProfile::Staging => "staging".to_string(),
        ConfigProfile::Prod => "prod".to_string(),
        ConfigProfile::Custom(name) => format!("custom({name})"),
    }
}

fn print_profiles() {
    println!("{:<10} DEFAULT OVERRIDES", "PROFILE");
    println!("{}", "-".repeat(72));
    println!("{:<10} concurrency.per_repo=20, model concurrency=20", "dev");
    println!("{:<10} budget.enabled=true", "staging");
    println!("{:<10} budget.enabled=true", "prod");
    println!("{:<10} no built-in overrides", "custom");
}

fn parse_task_priority(s: &str) -> anyhow::Result<TaskPriority> {
    s.parse::<TaskPriority>().map_err(|e| anyhow::anyhow!(e))
}

fn parse_task_state_filter(value: &str) -> anyhow::Result<TaskState> {
    match value.trim().to_lowercase().replace('-', "_").as_str() {
        "chatting" => Ok(TaskState::Chatting),
        "ready" => Ok(TaskState::Ready),
        "submitting" => Ok(TaskState::Submitting),
        "restacking" => Ok(TaskState::Restacking),
        "awaiting_merge" => Ok(TaskState::AwaitingMerge),
        "merged" => Ok(TaskState::Merged),
        "stopped" => Ok(TaskState::Stopped),
        other => anyhow::bail!("unknown state filter: {other}"),
    }
}

fn set_priority(service: &OrchdService, task_id: &TaskId, priority: TaskPriority) -> anyhow::Result<()> {
    let Some(mut task) = service.task(task_id)? else {
        anyhow::bail!("task not found: {}", task_id.0);
    };
    task.priority = priority;
    task.updated_at = Utc::now();
    service.store.upsert_task(&task)?;
    Ok(())
}

fn add_task_label(service: &OrchdService, task_id: &TaskId, label: &str) -> anyhow::Result<()> {
    let normalized = label.trim();
    if normalized.is_empty() {
        anyhow::bail!("label cannot be empty");
    }
    let Some(mut task) = service.task(task_id)? else {
        anyhow::bail!("task not found: {}", task_id.0);
    };
    if !task.labels.iter().any(|existing| existing == normalized) {
        task.labels.push(normalized.to_string());
    }
    task.updated_at = Utc::now();
    service.store.upsert_task(&task)?;
    Ok(())
}

fn remove_task_label(service: &OrchdService, task_id: &TaskId, label: &str) -> anyhow::Result<()> {
    let normalized = label.trim();
    if normalized.is_empty() {
        anyhow::bail!("label cannot be empty");
    }
    let Some(mut task) = service.task(task_id)? else {
        anyhow::bail!("task not found: {}", task_id.0);
    };
    task.labels.retain(|existing| existing != normalized);
    task.updated_at = Utc::now();
    service.store.upsert_task(&task)?;
    Ok(())
}

fn print_search_results(tasks: &[Task], json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(tasks).unwrap_or_else(|_| "[]".to_string())
        );
        return;
    }

    if tasks.is_empty() {
        println!("No matching tasks.");
        return;
    }

    println!("{:<20} {:<16} {:<24} TITLE", "ID", "STATE", "LABELS");
    println!("{}", "-".repeat(110));
    for task in tasks {
        let labels = if task.labels.is_empty() {
            "-".to_string()
        } else {
            task.labels.join(",")
        };
        println!(
            "{:<20} {:<16} {:<24} {}",
            task.id.0,
            format!("{}", task.state),
            labels,
            task.title
        );
    }
}

fn init_project(repo_root: &Path, force: bool) -> anyhow::Result<Vec<String>> {
    let othala_dir = repo_root.join(".othala");
    let config_path = othala_dir.join("config.toml");
    let context_dir = othala_dir.join("context");
    let main_context_path = context_dir.join("MAIN.md");
    let templates_dir = othala_dir.join("templates");

    if config_path.exists() && !force {
        anyhow::bail!(
            "{} already exists (pass --force to overwrite)",
            config_path.display()
        );
    }

    let mut actions = Vec::new();

    if !othala_dir.exists() {
        std::fs::create_dir_all(&othala_dir)?;
        actions.push("Created .othala/".to_string());
    }
    if !context_dir.exists() {
        std::fs::create_dir_all(&context_dir)?;
        actions.push("Created .othala/context/".to_string());
    }
    if !templates_dir.exists() {
        std::fs::create_dir_all(&templates_dir)?;
        actions.push("Created .othala/templates/".to_string());
    }

    let config_existed = config_path.exists();
    let org_config = default_org_config(vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini]);
    save_org_config(&config_path, &org_config)?;
    if config_existed {
        actions.push("Overwrote .othala/config.toml".to_string());
    } else {
        actions.push("Created .othala/config.toml".to_string());
    }

    let context_existed = main_context_path.exists();
    if force || !context_existed {
        std::fs::write(
            &main_context_path,
            "# Project Context\n\nDescribe your project here.\n",
        )?;
        if context_existed {
            actions.push("Overwrote .othala/context/MAIN.md".to_string());
        } else {
            actions.push("Created .othala/context/MAIN.md".to_string());
        }
    }

    Ok(actions)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BulkSummary {
    processed: usize,
    succeeded: usize,
    skipped: usize,
}

fn select_bulk_tasks(
    service: &OrchdService,
    state: Option<&str>,
    ids: &[String],
) -> anyhow::Result<Vec<Task>> {
    let state_filter = match state {
        Some(value) => Some(parse_task_state_filter(value)?),
        None => None,
    };
    let id_filter: HashSet<&str> = ids.iter().map(String::as_str).collect();

    let tasks = service
        .list_tasks()?
        .into_iter()
        .filter(|task| match state_filter {
            Some(wanted) => task.state == wanted,
            None => true,
        })
        .filter(|task| id_filter.is_empty() || id_filter.contains(task.id.0.as_str()))
        .collect();

    Ok(tasks)
}

fn bulk_retry(
    service: &OrchdService,
    state: Option<&str>,
    ids: &[String],
) -> anyhow::Result<BulkSummary> {
    let tasks = select_bulk_tasks(service, state, ids)?;
    let mut summary = BulkSummary {
        processed: tasks.len(),
        succeeded: 0,
        skipped: 0,
    };

    for task in tasks {
        let now = Utc::now();
        let event_id = EventId(format!("E-BULK-RETRY-{}-{}", task.id.0, now.timestamp_millis()));
        if service
            .transition_task_state(&task.id, TaskState::Chatting, event_id, now)
            .is_err()
        {
            summary.skipped += 1;
            continue;
        }

        let Some(mut updated) = service.task(&task.id)? else {
            summary.skipped += 1;
            continue;
        };
        updated.retry_count = 0;
        updated.updated_at = Utc::now();
        service.store.upsert_task(&updated)?;
        summary.succeeded += 1;
    }

    Ok(summary)
}

fn bulk_cancel(
    service: &OrchdService,
    state: Option<&str>,
    ids: &[String],
) -> anyhow::Result<BulkSummary> {
    let tasks = select_bulk_tasks(service, state, ids)?;
    let mut summary = BulkSummary {
        processed: tasks.len(),
        succeeded: 0,
        skipped: 0,
    };

    for task in tasks {
        if cancel_task(service, &task.id, "requested by user").is_ok() {
            summary.succeeded += 1;
        } else {
            summary.skipped += 1;
        }
    }

    Ok(summary)
}

fn bulk_set_priority(
    service: &OrchdService,
    priority: TaskPriority,
    state: Option<&str>,
    ids: &[String],
) -> anyhow::Result<BulkSummary> {
    let tasks = select_bulk_tasks(service, state, ids)?;
    let mut summary = BulkSummary {
        processed: tasks.len(),
        succeeded: 0,
        skipped: 0,
    };

    for task in tasks {
        if set_priority(service, &task.id, priority).is_ok() {
            summary.succeeded += 1;
        } else {
            summary.skipped += 1;
        }
    }

    Ok(summary)
}

fn create_task_command(
    service: &OrchdService,
    repo: String,
    title: String,
    model: String,
    priority: TaskPriority,
    json: bool,
) -> anyhow::Result<()> {
    let task_id = format!("chat-{}", Utc::now().timestamp_millis());
    let task_id = TaskId::new(&task_id);
    let start_path = std::env::current_dir()?;
    let repo_id = RepoId(repo.clone());
    let parent = find_stack_parent(&service.list_tasks()?, &repo_id);
    let workspace = provision_chat_workspace_on_base(
        &start_path,
        &task_id,
        parent.as_ref().map(|(_, branch)| branch.as_str()),
    )?;

    let mut task = Task::new(
        task_id.clone(),
        repo_id.clone(),
        title.clone(),
        workspace.worktree_path.clone(),
    );
    task.branch_name = Some(workspace.branch_name.clone());
    task.priority = priority;
    task.submit_mode = submit_mode_from_repo_mode(&start_path);
    if let Some((parent_task_id, _)) = parent.as_ref() {
        task.parent_task_id = Some(parent_task_id.clone());
        if !task.depends_on.contains(parent_task_id) {
            task.depends_on.push(parent_task_id.clone());
        }
    }

    task.preferred_model = Some(parse_model(&model));

    let event = Event {
        id: EventId(format!("E-CREATE-{}", task_id.0)),
        task_id: Some(task.id.clone()),
        repo_id: Some(task.repo_id.clone()),
        at: Utc::now(),
        kind: EventKind::TaskCreated,
    };

    service.create_task(&task, &event)?;

    if json {
        print_task_json(&task);
    } else if let Some((parent_task_id, parent_branch)) = parent {
        println!(
            "Created chat: {} - {} [{} @ {}] (stacked on {} / {})",
            task_id.0,
            title,
            workspace.branch_name,
            workspace.worktree_path.display(),
            parent_task_id.0,
            parent_branch
        );
    } else {
        println!(
            "Created chat: {} - {} [{} @ {}]",
            task_id.0,
            title,
            workspace.branch_name,
            workspace.worktree_path.display()
        );
    }
    Ok(())
}

fn submit_mode_from_repo_mode(repo_root: &Path) -> SubmitMode {
    let mode_path = repo_root.join(".othala/repo-mode.toml");
    let Ok(contents) = std::fs::read_to_string(mode_path) else {
        return SubmitMode::Single;
    };

    if contents
        .lines()
        .map(str::trim)
        .any(|line| line == "mode = \"stack\"" || line == "mode=\"stack\"")
    {
        SubmitMode::Stack
    } else {
        SubmitMode::Single
    }
}

fn model_name(model: ModelKind) -> &'static str {
    match model {
        ModelKind::Claude => "claude",
        ModelKind::Codex => "codex",
        ModelKind::Gemini => "gemini",
    }
}

fn parse_enable_models_csv(raw: &str) -> anyhow::Result<Vec<ModelKind>> {
    let mut out = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let Some(model) = parse_model_name(token) else {
            anyhow::bail!("unknown model '{token}'. valid values: claude,codex,gemini");
        };
        out.push(model);
    }
    if out.is_empty() {
        anyhow::bail!("no models provided. pass --enable claude,codex,gemini");
    }
    Ok(out)
}

fn default_org_config(enabled_models: Vec<ModelKind>) -> OrgConfig {
    let mut config = OrgConfig::default();
    let default_model = enabled_models.first().copied();
    config.models.enabled = enabled_models;
    config.models.default = default_model;
    config
}

fn build_notification_dispatcher(config: &NotificationConfig) -> Option<NotificationDispatcher> {
    if !config.enabled {
        return None;
    }

    let mut sinks: Vec<Box<dyn NotificationSink>> = Vec::new();

    if config.stdout {
        sinks.push(Box::new(StdoutSink));
    }

    if let Some(url) = &config.webhook_url {
        if !url.trim().is_empty() {
            sinks.push(Box::new(WebhookSink {
                url: url.clone(),
                timeout_secs: 10,
            }));
        }
    }

    if let Some(url) = &config.slack_webhook_url {
        if !url.trim().is_empty() {
            sinks.push(Box::new(orch_notify::SlackSink {
                webhook_url: url.clone(),
                channel: config.slack_channel.clone(),
                timeout_secs: 10,
            }));
        }
    }

    if sinks.is_empty() {
        None
    } else {
        Some(NotificationDispatcher::new(sinks))
    }
}

fn prompt_enabled_models() -> anyhow::Result<Vec<ModelKind>> {
    let mut line = String::new();
    loop {
        eprint!("Enable models (comma-separated: claude,codex,gemini): ");
        std::io::stderr().flush()?;
        line.clear();
        std::io::stdin().read_line(&mut line)?;

        match parse_enable_models_csv(&line) {
            Ok(models) => return Ok(models),
            Err(err) => eprintln!("\x1b[31mInvalid input: {err}\x1b[0m"),
        }
    }
}

fn all_tasks_idle(service: &OrchdService) -> bool {
    match service.list_tasks() {
        Ok(tasks) if tasks.is_empty() => false,
        Ok(tasks) => tasks
            .iter()
            .all(|t| t.state.is_terminal() || t.state == TaskState::AwaitingMerge),
        Err(_) => false,
    }
}

fn find_stack_parent(tasks: &[Task], repo_id: &RepoId) -> Option<(TaskId, String)> {
    tasks
        .iter()
        .filter(|task| &task.repo_id == repo_id)
        .filter(|task| !matches!(task.state, TaskState::Merged | TaskState::Stopped))
        .filter_map(|task| {
            task.branch_name
                .as_ref()
                .map(|branch| (task.id.clone(), branch.clone(), task.updated_at))
        })
        .max_by(|a, b| a.2.cmp(&b.2))
        .map(|(task_id, branch, _)| (task_id, branch))
}

#[derive(Debug, serde::Serialize)]
struct SelfTestCheck {
    name: String,
    ok: bool,
    critical: bool,
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DoctorStatus {
    Ok,
    Missing,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DoctorCheck {
    name: String,
    ok: bool,
    status: DoctorStatus,
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DoctorReport {
    checks: Vec<DoctorCheck>,
    all_ok: bool,
}

fn command_available_via_which(executable: &str) -> bool {
    Command::new("which")
        .arg(executable)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn doctor_model_checks<F>(which_check: F) -> Vec<DoctorCheck>
where
    F: Fn(&str) -> bool,
{
    ["claude", "codex", "gemini"]
        .iter()
        .map(|model| {
            let found = which_check(model);
            DoctorCheck {
                name: format!("model_{model}"),
                ok: found,
                status: if found {
                    DoctorStatus::Ok
                } else {
                    DoctorStatus::Missing
                },
                detail: if found {
                    format!("{model} found on PATH")
                } else {
                    format!("{model} missing from PATH")
                },
            }
        })
        .collect()
}

fn collect_doctor_checks<F>(repo_root: &Path, which_check: F) -> Vec<DoctorCheck>
where
    F: Fn(&str) -> bool,
{
    let mut checks = doctor_model_checks(|model| which_check(model));

    let sqlite_path = repo_root.join(".othala/state.sqlite");
    let sqlite_ok = sqlite_path.is_file() && std::fs::File::open(&sqlite_path).is_ok();
    checks.push(DoctorCheck {
        name: "sqlite".to_string(),
        ok: sqlite_ok,
        status: if sqlite_ok {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Missing
        },
        detail: if sqlite_ok {
            format!("{} is readable", sqlite_path.display())
        } else {
            format!("{} is missing or unreadable", sqlite_path.display())
        },
    });

    let gt_found = which_check("gt");
    checks.push(DoctorCheck {
        name: "graphite".to_string(),
        ok: gt_found,
        status: if gt_found {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Missing
        },
        detail: if gt_found {
            "gt found on PATH".to_string()
        } else {
            "gt missing from PATH".to_string()
        },
    });

    let git_ok = Command::new("git")
        .arg("status")
        .current_dir(repo_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    checks.push(DoctorCheck {
        name: "git".to_string(),
        ok: git_ok,
        status: if git_ok {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Error
        },
        detail: if git_ok {
            "git status succeeded".to_string()
        } else {
            "git status failed".to_string()
        },
    });

    let config_path = repo_root.join(".othala/config.toml");
    let config_status = if !config_path.exists() {
        (false, DoctorStatus::Missing, "config file missing".to_string())
    } else {
        match load_org_config(&config_path) {
            Ok(_) => (true, DoctorStatus::Ok, "config parsed successfully".to_string()),
            Err(err) => (
                false,
                DoctorStatus::Error,
                format!("config parse failed: {err}"),
            ),
        }
    };
    checks.push(DoctorCheck {
        name: "config".to_string(),
        ok: config_status.0,
        status: config_status.1,
        detail: config_status.2,
    });

    let othala_dir = repo_root.join(".othala");
    let disk_ok = othala_dir.is_dir();
    checks.push(DoctorCheck {
        name: "disk".to_string(),
        ok: disk_ok,
        status: if disk_ok {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Missing
        },
        detail: if disk_ok {
            format!("{} exists", othala_dir.display())
        } else {
            format!("{} missing", othala_dir.display())
        },
    });

    checks
}

fn doctor_report(repo_root: &Path) -> DoctorReport {
    let checks = collect_doctor_checks(repo_root, command_available_via_which);
    let all_ok = checks.iter().all(|check| check.ok);
    DoctorReport { checks, all_ok }
}

fn doctor_status_label(status: &DoctorStatus) -> &'static str {
    match status {
        DoctorStatus::Ok => "ok",
        DoctorStatus::Missing => "missing",
        DoctorStatus::Error => "error",
    }
}

fn run_doctor(json: bool) -> anyhow::Result<bool> {
    let repo_root = std::env::current_dir()?;
    let report = doctor_report(&repo_root);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{:<20} {:<10} DETAIL", "CHECK", "STATUS");
        println!("{}", "-".repeat(80));
        for check in &report.checks {
            let status = doctor_status_label(&check.status);
            println!("{:<20} {:<10} {}", check.name, status, check.detail);
        }
        println!();
        println!(
            "Overall: {}",
            if report.all_ok { "healthy" } else { "issues found" }
        );
    }

    Ok(report.all_ok)
}

fn command_available(executable: &str) -> bool {
    Command::new(executable)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn run_self_test(json: bool) -> bool {
    let mut checks = Vec::new();

    let git_available = command_available("git");
    checks.push(SelfTestCheck {
        name: "git_available".to_string(),
        ok: git_available,
        critical: true,
        detail: if git_available {
            "git is available".to_string()
        } else {
            "git not found on PATH".to_string()
        },
    });

    let in_git_repo = if git_available {
        Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .map(|out| {
                out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true"
            })
            .unwrap_or(false)
    } else {
        false
    };
    checks.push(SelfTestCheck {
        name: "in_git_repo".to_string(),
        ok: in_git_repo,
        critical: true,
        detail: if in_git_repo {
            "current directory is inside a git repository".to_string()
        } else {
            "current directory is not inside a git repository".to_string()
        },
    });

    let gt_available = command_available("gt");
    checks.push(SelfTestCheck {
        name: "graphite_cli_available".to_string(),
        ok: gt_available,
        critical: true,
        detail: if gt_available {
            "gt is available".to_string()
        } else {
            "gt not found on PATH".to_string()
        },
    });

    let model_report = probe_models(&SetupProbeConfig::default());
    for probe in model_report.models {
        let missing_env = probe
            .env_status
            .iter()
            .filter(|status| !status.satisfied)
            .map(|status| format!("({})", status.any_of.join(" or ")))
            .collect::<Vec<_>>();

        let detail = if probe.healthy {
            probe
                .version_output
                .clone()
                .unwrap_or_else(|| "healthy".to_string())
        } else if !probe.installed {
            format!("{} not installed", probe.executable)
        } else if !probe.version_ok {
            probe
                .version_output
                .clone()
                .unwrap_or_else(|| "version check failed".to_string())
        } else if !missing_env.is_empty() {
            format!("missing env: {}", missing_env.join(", "))
        } else {
            "unhealthy".to_string()
        };

        checks.push(SelfTestCheck {
            name: format!("model_{:?}", probe.model).to_lowercase(),
            ok: probe.healthy,
            critical: false,
            detail,
        });
    }

    let required_dirs = [
        Path::new(".othala"),
        Path::new(".othala/context"),
        Path::new(".othala/qa"),
        Path::new(".othala/events"),
    ];
    let missing_dirs = required_dirs
        .iter()
        .filter(|path| !path.is_dir())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let structure_ok = missing_dirs.is_empty();
    checks.push(SelfTestCheck {
        name: "othala_directory_structure".to_string(),
        ok: structure_ok,
        critical: true,
        detail: if structure_ok {
            ".othala/ structure is present".to_string()
        } else {
            format!("missing: {}", missing_dirs.join(", "))
        },
    });

    let sqlite_open = rusqlite::Connection::open_with_flags(
        ".othala/db.sqlite",
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    );
    checks.push(SelfTestCheck {
        name: "sqlite_open".to_string(),
        ok: sqlite_open.is_ok(),
        critical: true,
        detail: match sqlite_open {
            Ok(_) => "opened .othala/db.sqlite".to_string(),
            Err(err) => format!("failed to open .othala/db.sqlite: {err}"),
        },
    });

    let cargo_available = command_available("cargo");
    checks.push(SelfTestCheck {
        name: "cargo_available".to_string(),
        ok: cargo_available,
        critical: true,
        detail: if cargo_available {
            "cargo is available".to_string()
        } else {
            "cargo not found on PATH".to_string()
        },
    });

    let critical_ok = checks.iter().all(|check| !check.critical || check.ok);

    if json {
        let out = serde_json::to_string_pretty(&checks).unwrap_or_else(|_| "[]".to_string());
        println!("{out}");
    } else {
        println!("Othala Self-Test");
        for check in &checks {
            let (symbol, color) = if check.ok {
                ("\u{2713}", "\x1b[32m")
            } else {
                ("\u{2717}", "\x1b[31m")
            };
            let suffix = if check.critical {
                ""
            } else {
                " (non-critical)"
            };
            println!(
                "  {color}{symbol}\x1b[0m {}{}: {}",
                check.name, suffix, check.detail
            );
        }

        if critical_ok {
            println!("\n\x1b[32mAll critical checks passed\x1b[0m");
        } else {
            println!("\n\x1b[31mOne or more critical checks failed\x1b[0m");
        }
    }

    critical_ok
}

const WATCH_PREFIX_COLORS: [&str; 6] = [
    "\x1b[31m",
    "\x1b[32m",
    "\x1b[33m",
    "\x1b[34m",
    "\x1b[36m",
    "\x1b[35m",
];

fn format_watch_line(task_id: &str, color: &str, line: &str) -> String {
    format!("[{color}{task_id}\x1b[0m] {line}")
}

fn read_all_log_lines_and_position(path: &Path) -> std::io::Result<(Vec<String>, u64)> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut buf = String::new();

    loop {
        let bytes_read = reader.read_line(&mut buf)?;
        if bytes_read == 0 {
            break;
        }
        lines.push(buf.trim_end_matches(&['\r', '\n'][..]).to_string());
        buf.clear();
    }

    let position = reader.stream_position()?;
    Ok((lines, position))
}

fn read_new_log_lines(path: &Path, position: &mut u64) -> std::io::Result<Vec<String>> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() < *position {
        *position = 0;
    }

    file.seek(SeekFrom::Start(*position))?;
    let mut reader = BufReader::new(file);
    let mut new_lines = Vec::new();
    let mut buf = String::new();

    loop {
        let bytes_read = reader.read_line(&mut buf)?;
        if bytes_read == 0 {
            break;
        }
        *position += bytes_read as u64;
        new_lines.push(buf.trim_end_matches(&['\r', '\n'][..]).to_string());
        buf.clear();
    }

    Ok(new_lines)
}

fn run_watch_command(service: &OrchdService, task_filter: Option<String>, lines: usize) -> anyhow::Result<()> {
    let repo_root = std::env::current_dir()?;
    let mut tasks = service.list_tasks_by_state(TaskState::Chatting)?;

    if let Some(task_id) = task_filter {
        tasks.retain(|task| task.id.0 == task_id);
    }

    if tasks.is_empty() {
        println!("No active chatting tasks to watch.");
        return Ok(());
    }

    tasks.sort_by(|a, b| a.id.0.cmp(&b.id.0));

    let mut watch_state: HashMap<String, (PathBuf, u64, &'static str)> = HashMap::new();
    let mut order = Vec::new();

    for (idx, task) in tasks.iter().enumerate() {
        let color = WATCH_PREFIX_COLORS[idx % WATCH_PREFIX_COLORS.len()];
        let log_path = orchd::agent_log::agent_log_dir(&repo_root, &task.id).join("latest.log");
        let mut position = 0u64;

        match read_all_log_lines_and_position(&log_path) {
            Ok((all_lines, end_position)) => {
                let start = all_lines.len().saturating_sub(lines);
                for line in &all_lines[start..] {
                    println!("{}", format_watch_line(&task.id.0, color, line));
                }
                position = end_position;
            }
            Err(err) => {
                if err.kind() != ErrorKind::NotFound {
                    return Err(err.into());
                }
            }
        }

        order.push(task.id.0.clone());
        watch_state.insert(task.id.0.clone(), (log_path, position, color));
    }

    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, shutdown.clone())?;

    while !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
        for task_id in &order {
            if let Some((log_path, position, color)) = watch_state.get_mut(task_id) {
                match read_new_log_lines(log_path, position) {
                    Ok(new_lines) => {
                        for line in new_lines {
                            println!("{}", format_watch_line(task_id, color, &line));
                        }
                    }
                    Err(err) => {
                        if err.kind() != ErrorKind::NotFound {
                            return Err(err.into());
                        }
                    }
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    Ok(())
}

fn cancel_task(service: &OrchdService, task_id: &TaskId, reason: &str) -> anyhow::Result<TaskState> {
    let Some(task) = service.task(task_id)? else {
        anyhow::bail!("task not found: {}", task_id.0);
    };

    if !matches!(task.state, TaskState::Chatting | TaskState::Ready) {
        anyhow::bail!("cannot cancel task in state {}", task.state);
    }

    let now = Utc::now();
    service.record_event(&Event {
        id: EventId(format!("E-CANCEL-{}-{}", task_id.0, now.timestamp_millis())),
        task_id: Some(task_id.clone()),
        repo_id: Some(task.repo_id.clone()),
        at: now,
        kind: EventKind::CancellationRequested {
            reason: reason.to_string(),
        },
    })?;

    let from_state = task.state;
    service.transition_task_state(
        task_id,
        TaskState::Stopped,
        EventId(format!("E-CANCEL-STATE-{}-{}", task_id.0, now.timestamp_millis())),
        now,
    )?;

    Ok(from_state)
}

fn resolve_base_branch() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !branch.is_empty() {
                return branch
                    .strip_prefix("origin/")
                    .unwrap_or(&branch)
                    .to_string();
            }
        }
    }

    "main".to_string()
}

fn build_diff_args(base_branch: &str, task_branch: &str, stat: bool) -> Vec<String> {
    let mut args = vec!["diff".to_string()];
    if stat {
        args.push("--stat".to_string());
    }
    args.push(format!("{base_branch}...{task_branch}"));
    args
}

fn archive_old_tasks(service: &OrchdService, older_than_days: i64) -> anyhow::Result<usize> {
    let now = Utc::now();
    let cutoff = now - chrono::Duration::days(older_than_days);
    let tasks = service.list_tasks()?;

    let mut archived = 0usize;
    for task in tasks
        .iter()
        .filter(|task| matches!(task.state, TaskState::Merged | TaskState::Stopped))
        .filter(|task| task.updated_at < cutoff)
    {
        service.store.archive_task(task, now)?;
        archived += 1;
    }

    Ok(archived)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct StatsSummary {
    total_tasks: usize,
    tasks_by_state: BTreeMap<String, i64>,
    tasks_by_model: BTreeMap<String, i64>,
    avg_time_to_merge_seconds: Option<f64>,
    success_rate: Option<f64>,
    total_events: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GcSummary {
    deleted_event_files: usize,
    deleted_agent_output_dirs: usize,
    bytes_freed: u64,
}

fn compute_stats_summary(tasks: &[Task], state_counts: Vec<(String, i64)>, total_events: i64) -> StatsSummary {
    const STATE_TAGS: [&str; 7] = [
        "CHATTING",
        "READY",
        "SUBMITTING",
        "RESTACKING",
        "AWAITING_MERGE",
        "MERGED",
        "STOPPED",
    ];

    let mut tasks_by_state = BTreeMap::new();
    for state in STATE_TAGS {
        tasks_by_state.insert(state.to_string(), 0);
    }
    for (state, count) in state_counts {
        tasks_by_state.insert(state, count);
    }

    let mut tasks_by_model = BTreeMap::new();
    for task in tasks {
        let model = task
            .preferred_model
            .map(|model| model.as_str().to_string())
            .unwrap_or_else(|| "unspecified".to_string());
        *tasks_by_model.entry(model).or_insert(0) += 1;
    }

    let merged = *tasks_by_state.get("MERGED").unwrap_or(&0);
    let stopped = *tasks_by_state.get("STOPPED").unwrap_or(&0);
    let denominator = merged + stopped;
    let success_rate = if denominator > 0 {
        Some((merged as f64 / denominator as f64) * 100.0)
    } else {
        None
    };

    let merged_durations: Vec<f64> = tasks
        .iter()
        .filter(|task| task.state == TaskState::Merged)
        .map(|task| (task.updated_at - task.created_at).num_milliseconds() as f64 / 1000.0)
        .collect();
    let avg_time_to_merge_seconds = if merged_durations.is_empty() {
        None
    } else {
        Some(merged_durations.iter().sum::<f64>() / merged_durations.len() as f64)
    };

    StatsSummary {
        total_tasks: tasks.len(),
        tasks_by_state,
        tasks_by_model,
        avg_time_to_merge_seconds,
        success_rate,
        total_events,
    }
}

fn print_stats_table(summary: &StatsSummary) {
    println!("{:<28} VALUE", "METRIC");
    println!("{}", "-".repeat(48));
    println!("{:<28} {}", "total_tasks", summary.total_tasks);
    println!("{:<28} {}", "total_events", summary.total_events);
    println!(
        "{:<28} {}",
        "avg_time_to_merge_secs",
        summary
            .avg_time_to_merge_seconds
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "n/a".to_string())
    );
    println!(
        "{:<28} {}",
        "success_rate_percent",
        summary
            .success_rate
            .map(|value| format!("{value:.2}%"))
            .unwrap_or_else(|| "n/a".to_string())
    );
    println!();

    println!("{:<20} COUNT", "TASK_STATE");
    println!("{}", "-".repeat(32));
    for (state, count) in &summary.tasks_by_state {
        println!("{:<20} {}", state, count);
    }
    println!();

    println!("{:<20} COUNT", "PREFERRED_MODEL");
    println!("{}", "-".repeat(32));
    if summary.tasks_by_model.is_empty() {
        println!("{:<20} {}", "(none)", 0);
    } else {
        for (model, count) in &summary.tasks_by_model {
            println!("{:<20} {}", model, count);
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{} B", bytes);
    }
    let kib = bytes as f64 / 1024.0;
    if kib < 1024.0 {
        return format!("{kib:.1} KiB");
    }
    let mib = kib / 1024.0;
    if mib < 1024.0 {
        return format!("{mib:.1} MiB");
    }
    format!("{:.1} GiB", mib / 1024.0)
}

fn collect_old_jsonl_files(root: &Path, cutoff: SystemTime, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !root.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_old_jsonl_files(&path, cutoff, out)?;
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }

        if metadata.modified().map(|modified| modified < cutoff).unwrap_or(false) {
            out.push(path);
        }
    }

    Ok(())
}

fn collect_old_agent_dirs(root: &Path, cutoff: SystemTime) -> std::io::Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut candidates = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if !metadata.is_dir() {
            continue;
        }
        if metadata.modified().map(|modified| modified < cutoff).unwrap_or(false) {
            candidates.push(path);
        }
    }
    Ok(candidates)
}

fn dir_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total += dir_size(&entry.path())?;
        } else {
            total += metadata.len();
        }
    }
    Ok(total)
}

fn gc_logs(repo_root: &Path, older_than_days: u64, dry_run: bool) -> anyhow::Result<GcSummary> {
    let age = Duration::from_secs(older_than_days.saturating_mul(24 * 60 * 60));
    let cutoff = SystemTime::now()
        .checked_sub(age)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let events_root = repo_root.join(".othala/events");
    let agent_output_root = repo_root.join(".othala/agent-output");

    let mut old_event_files = Vec::new();
    collect_old_jsonl_files(&events_root, cutoff, &mut old_event_files)?;
    let old_agent_dirs = collect_old_agent_dirs(&agent_output_root, cutoff)?;

    let mut bytes_freed = 0u64;
    for event_file in &old_event_files {
        bytes_freed += fs::metadata(event_file).map(|m| m.len()).unwrap_or(0);
    }
    for dir in &old_agent_dirs {
        bytes_freed += dir_size(dir).unwrap_or(0);
    }

    if dry_run {
        for event_file in &old_event_files {
            println!("[dry-run] would delete file {}", event_file.display());
        }
        for dir in &old_agent_dirs {
            println!("[dry-run] would delete dir  {}", dir.display());
        }
    } else {
        for event_file in &old_event_files {
            fs::remove_file(event_file)?;
        }
        for dir in &old_agent_dirs {
            fs::remove_dir_all(dir)?;
        }
    }

    Ok(GcSummary {
        deleted_event_files: old_event_files.len(),
        deleted_agent_output_dirs: old_agent_dirs.len(),
        bytes_freed,
    })
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let cwd = std::env::current_dir()?;
    let db_path = cwd.join(".orch/state.sqlite");
    let event_log_path = cwd.join(".orch/events");

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(&event_log_path)?;

    let scheduler = Scheduler::new(SchedulerConfig {
        per_repo_limit: 10,
        per_model_limit: vec![
            (ModelKind::Claude, 10),
            (ModelKind::Codex, 10),
            (ModelKind::Gemini, 10),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>(),
    });

    let mut service = OrchdService::open(&db_path, &event_log_path, scheduler)?;

    match cli.command {
        Commands::Init { force } => {
            let repo_root = std::env::current_dir()?;
            let actions = init_project(&repo_root, force)?;
            for action in actions {
                println!("{action}");
            }
        }
        Commands::CreateTask {
            repo,
            title,
            model,
            priority,
            json,
        } => {
            create_task_command(
                &service,
                repo,
                title,
                model,
                parse_task_priority(&priority)?,
                json,
            )?;
        }
        Commands::LoadTasks { dir } => {
            let repo_root = std::env::current_dir()?;
            let specs_dir = dir.unwrap_or_else(|| repo_root.join(".othala/tasks"));
            let repo_id = default_repo_id_from_path(&repo_root);
            let specs = load_task_specs_from_dir(&specs_dir);

            for spec in &specs {
                let task = yaml_spec_to_task(spec, &repo_id);
                let event = Event {
                    id: EventId(format!("E-CREATE-{}", task.id.0)),
                    task_id: Some(task.id.clone()),
                    repo_id: Some(task.repo_id.clone()),
                    at: Utc::now(),
                    kind: EventKind::TaskCreated,
                };
                service.create_task(&task, &event)?;
            }

            println!(
                "Loaded {} task spec(s) from {}",
                specs.len(),
                specs_dir.display()
            );
        }
        Commands::ValidateSpec { path } => {
            let content = std::fs::read_to_string(&path)?;
            let spec = parse_yaml_task_spec(&content)
                .map_err(|err| anyhow::anyhow!("invalid YAML task spec: {err}"))?;
            println!("Valid YAML task spec: {}", spec.title);
        }
        Commands::SetPriority { id, priority } => {
            let task_id = TaskId::new(&id);
            let parsed = parse_task_priority(&priority)?;
            set_priority(&service, &task_id, parsed)?;
            println!("Updated priority: {} -> {}", task_id.0, parsed);
        }
        Commands::Tag { task_id, label } => {
            let task_id = TaskId::new(&task_id);
            add_task_label(&service, &task_id, &label)?;
            println!("Tagged {} with '{}'", task_id.0, label);
        }
        Commands::Untag { task_id, label } => {
            let task_id = TaskId::new(&task_id);
            remove_task_label(&service, &task_id, &label)?;
            println!("Removed tag '{}' from {}", label, task_id.0);
        }
        Commands::Search {
            query,
            label,
            state,
            json,
        } => {
            let matches = service
                .store
                .search_tasks(&query, label.as_deref(), state.as_deref())?;
            print_search_results(&matches, json);
        }
        Commands::Bulk { action } => {
            let summary = match action {
                BulkAction::Retry { state, ids } => bulk_retry(&service, state.as_deref(), &ids)?,
                BulkAction::Cancel { state, ids } => bulk_cancel(&service, state.as_deref(), &ids)?,
                BulkAction::SetPriority {
                    priority,
                    state,
                    ids,
                } => {
                    let parsed = parse_task_priority(&priority)?;
                    bulk_set_priority(&service, parsed, state.as_deref(), &ids)?
                }
            };

            println!(
                "Processed {} tasks, {} succeeded, {} skipped",
                summary.processed, summary.succeeded, summary.skipped
            );
        }
        Commands::Chat { action } => match action {
            ChatAction::New {
                repo,
                title,
                model,
                json,
            } => {
                create_task_command(
                    &service,
                    repo,
                    title,
                    model,
                    TaskPriority::Normal,
                    json,
                )?;
            }
            ChatAction::List { json } => {
                print_task_list(&service.list_tasks()?, json);
            }
        },
        Commands::List { json } => {
            print_task_list(&service.list_tasks()?, json);
        }
        Commands::Sessions { json } => {
            let sessions = service.store.list_sessions()?;
            print_session_list(&sessions, json);
        }
        Commands::Session { action } => match action {
            SessionAction::Show { id, json } => match service.store.get_session(&id)? {
                Some(session) => print_session_details(&session, json),
                None => {
                    if json {
                        println!("null");
                    } else {
                        println!("Session not found: {}", id);
                    }
                }
            },
            SessionAction::Fork { id } => {
                let child = service.store.fork_session(&id)?;
                println!("Forked session: {} -> {}", id, child.id);
            }
            SessionAction::External(args) => {
                if args.len() != 1 {
                    anyhow::bail!("expected `othala session <id>` or `othala session fork <id>`");
                }
                let id = &args[0];
                match service.store.get_session(id)? {
                    Some(session) => print_session_details(&session, false),
                    None => println!("Session not found: {}", id),
                }
            }
        },
        Commands::Status { id, json } => {
            let task_id = TaskId::new(&id);
            match service.task(&task_id)? {
                Some(task) => {
                    if json {
                        print_task_json(&task);
                    } else {
                        println!("Chat: {}", task.id.0);
                        println!("Title: {}", task.title);
                        println!("Repo: {}", task.repo_id.0);
                        println!("State: {}", task.state);
                        if let Some(model) = task.preferred_model {
                            println!("Model: {:?}", model);
                        }
                        if let Some(pr) = &task.pr {
                            println!("PR: {} ({})", pr.number, pr.url);
                        }
                        if let Some(branch) = &task.branch_name {
                            println!("Branch: {}", branch);
                        }
                        println!("Worktree: {}", task.worktree_path.display());
                        println!("Created: {}", task.created_at);
                        println!("Updated: {}", task.updated_at);
                    }
                }
                None => {
                    if json {
                        println!("null");
                    } else {
                        println!("Chat not found: {}", id);
                    }
                }
            }
        }
        Commands::Delete { id } => {
            let task_id = TaskId::new(&id);
            if service.delete_task(&task_id)? {
                println!("Deleted chat: {}", id);
            } else {
                println!("Chat not found: {}", id);
            }
        }
        Commands::Clone {
            task_id,
            title,
            model,
            priority,
        } => {
            let source_id = TaskId::new(&task_id);
            if service.store.load_task(&source_id)?.is_none() {
                anyhow::bail!("task not found: {task_id}");
            }

            let model_override = if let Some(value) = model {
                let Some(parsed) = parse_model_name(&value) else {
                    anyhow::bail!("unknown model '{value}'. valid values: claude,codex,gemini");
                };
                Some(parsed)
            } else {
                None
            };
            let priority_override = priority
                .as_deref()
                .map(parse_task_priority)
                .transpose()?;

            let new_id = format!("{}-clone-{}", task_id, Utc::now().timestamp_millis());
            service.store.clone_task(
                &task_id,
                &new_id,
                TaskCloneOverrides {
                    title,
                    preferred_model: model_override,
                    priority: priority_override,
                },
            )?;

            println!("Cloned {} → {}", task_id, new_id);
        }
        Commands::Diff { task_id, stat } => {
            let task = service
                .store
                .load_task(&TaskId::new(&task_id))?
                .ok_or_else(|| anyhow::anyhow!("task not found: {task_id}"))?;

            let Some(task_branch) = task.branch_name else {
                println!("Task has no branch assigned");
                return Ok(());
            };

            let base_branch = resolve_base_branch();
            let args = build_diff_args(&base_branch, &task_branch, stat);
            let output = Command::new("git").args(&args).output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git diff failed: {stderr}");
            }
            print!("{}", String::from_utf8_lossy(&output.stdout));
        }
        Commands::Undo { task_id } => {
            let task = service
                .store
                .load_task(&TaskId::new(&task_id))?
                .ok_or_else(|| anyhow::anyhow!("task not found: {task_id}"))?;
            let git = GitCli::default();
            let repo = discover_repo(&task.worktree_path, &git)?;
            let snapshots = list_change_snapshots(&repo, &git, &task_id)?;
            let snapshot = snapshots
                .last()
                .ok_or_else(|| anyhow::anyhow!("no snapshots found for task: {task_id}"))?;
            undo_to_snapshot(&repo, &git, snapshot)?;
            println!(
                "Undo applied for {} ({} -> {})",
                task_id, snapshot.commit_sha, snapshot.parent_sha
            );
        }
        Commands::Redo { task_id } => {
            let task = service
                .store
                .load_task(&TaskId::new(&task_id))?
                .ok_or_else(|| anyhow::anyhow!("task not found: {task_id}"))?;
            let git = GitCli::default();
            let repo = discover_repo(&task.worktree_path, &git)?;
            let snapshots = list_change_snapshots(&repo, &git, &task_id)?;
            let snapshot = snapshots
                .last()
                .ok_or_else(|| anyhow::anyhow!("no snapshots found for task: {task_id}"))?;
            redo_snapshot(&repo, &git, snapshot)?;
            println!(
                "Redo applied for {} ({} -> {})",
                task_id, snapshot.parent_sha, snapshot.commit_sha
            );
        }
        Commands::Daemon {
            timeout,
            exit_on_idle,
            skip_context_gen,
            verify_command,
            skip_qa,
            once,
            profile,
        } => {
            print_banner();

            let repo_root = std::env::current_dir()?;
            let template_dir = PathBuf::from("templates/prompts");
            let selected_cli_profile = profile.map(ConfigProfile::from);

            let config_path = PathBuf::from(".othala/config.toml");
            let (enabled_models, default_model, notification_dispatcher, daemon_org_config) =
                if config_path.exists() {
                let mut org_config = load_org_config(&config_path)?;
                let effective_profile = selected_cli_profile
                    .clone()
                    .or_else(|| org_config.profile.clone());
                if let Some(profile) = &effective_profile {
                    apply_profile_defaults(profile, &mut org_config);
                    eprintln!("  Profile: {}", profile_label(profile));
                }
                use orch_core::validation::{Validate, ValidationLevel};
                let issues = org_config.validate();
                for issue in &issues {
                    let prefix = match issue.level {
                        ValidationLevel::Error => "\x1b[31mERROR\x1b[0m",
                        ValidationLevel::Warning => "\x1b[33mWARN\x1b[0m",
                    };
                    eprintln!("  [{prefix}] {}: {}", issue.code, issue.message);
                }
                if issues.iter().any(|i| i.level == ValidationLevel::Error) {
                    anyhow::bail!("config validation failed — run `othala wizard` to fix");
                }
                let default = org_config.models.default.unwrap_or(ModelKind::Claude);
                let notification_dispatcher =
                    build_notification_dispatcher(&org_config.notifications);
                (
                    org_config.models.enabled,
                    default,
                    notification_dispatcher,
                    org_config.daemon,
                )
            } else {
                eprintln!("  \x1b[33mNo .othala/config.toml — using defaults (run `othala wizard` to configure)\x1b[0m");
                let mut org_config = OrgConfig::default();
                if let Some(profile) = &selected_cli_profile {
                    apply_profile_defaults(profile, &mut org_config);
                    eprintln!("  Profile: {}", profile_label(profile));
                }
                (
                    org_config.models.enabled,
                    org_config.models.default.unwrap_or(ModelKind::Claude),
                    None,
                    org_config.daemon,
                )
            };
            eprintln!(
                "  Enabled models: {}",
                enabled_models
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            if !skip_context_gen {
                if let Err(e) =
                    run_context_gen_with_status(&repo_root, &template_dir, default_model)
                {
                    eprintln!("[daemon] Context generation failed (non-fatal): {e}");
                }
            } else {
                eprintln!("  Skipping context generation");
            }
            eprintln!();

            if let Some(secs) = timeout {
                eprintln!("[daemon] Timeout: {}s", secs);
            }
            if exit_on_idle {
                eprintln!("[daemon] Will exit when all tasks reach terminal state");
            }

            let context_gen_config = orchd::context_gen::ContextGenConfig::default();
            let mut supervisor = AgentSupervisor::new(default_model);
            let mut daemon_state = orchd::daemon_loop::DaemonState::new();
            daemon_state.notification_dispatcher = notification_dispatcher;

            let verify_cmd = verify_command
                .unwrap_or_else(|| "cargo check && cargo test --workspace".to_string());

            let mut daemon_config = orchd::daemon_loop::DaemonConfig {
                repo_root,
                template_dir,
                enabled_models,
                context_config: orchd::context_graph::ContextLoadConfig::default(),
                verify_command: Some(verify_cmd),
                context_gen_config,
                skip_qa,
                skip_context_regen: skip_context_gen,
                dry_run: false,
                agent_timeout_secs: daemon_org_config.agent_timeout_secs,
                drain_timeout_secs: 30,
            };
            let mut tick_interval_secs = daemon_org_config.tick_interval_secs;

            let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let flag = shutdown.clone();
                signal_hook::flag::register(signal_hook::consts::SIGINT, flag.clone())?;
                signal_hook::flag::register(signal_hook::consts::SIGTERM, flag)?;
            }

            let start = Instant::now();
            let mut idle_grace_ticks: u32 = 0;
            const IDLE_GRACE_MAX: u32 = 3;
            let mut prev_states: HashMap<String, TaskState> = HashMap::new();

            loop {
                if let Some(new_config) =
                    orchd::daemon_loop::check_config_reload(&config_path, &mut daemon_state)
                {
                    let mut new_config = new_config;
                    let effective_profile = selected_cli_profile
                        .clone()
                        .or_else(|| new_config.profile.clone());
                    if let Some(profile) = &effective_profile {
                        apply_profile_defaults(profile, &mut new_config);
                    }
                    let mut changes = Vec::new();

                    if daemon_config.enabled_models != new_config.models.enabled {
                        changes.push("enabled_models".to_string());
                        daemon_config.enabled_models = new_config.models.enabled.clone();
                    }

                    let scheduler_config = SchedulerConfig::from_org_config(&new_config);
                    if service.scheduler.config != scheduler_config {
                        changes.push("scheduler".to_string());
                        service.scheduler.config = scheduler_config;
                    }

                    if daemon_config.agent_timeout_secs != new_config.daemon.agent_timeout_secs {
                        changes.push("agent_timeout_secs".to_string());
                        daemon_config.agent_timeout_secs = new_config.daemon.agent_timeout_secs;
                    }

                    if tick_interval_secs != new_config.daemon.tick_interval_secs {
                        changes.push("tick_interval_secs".to_string());
                        tick_interval_secs = new_config.daemon.tick_interval_secs;
                    }

                    let now = Utc::now();
                    let change_summary = if changes.is_empty() {
                        "no effective changes".to_string()
                    } else {
                        changes.join(", ")
                    };
                    let event = Event {
                        id: EventId(format!(
                            "E-CONFIG-RELOADED-{}",
                            now.timestamp_nanos_opt().unwrap_or_default()
                        )),
                        task_id: None,
                        repo_id: None,
                        at: now,
                        kind: EventKind::ConfigReloaded {
                            changes: change_summary,
                        },
                    };
                    if let Err(err) = service.record_event(&event) {
                        eprintln!("[daemon] Failed to record config reload event: {err}");
                    }
                }

                orchd::daemon_loop::run_tick(
                    &service,
                    &mut supervisor,
                    &mut daemon_state,
                    &daemon_config,
                );

                let tasks = service.list_tasks()?;
                for task in &tasks {
                    let prev = prev_states.get(&task.id.0);
                    if prev != Some(&task.state) {
                        if let Some(old) = prev {
                            eprintln!("[daemon] {} -> {} ({})", task.id.0, task.state, old);
                        }
                        prev_states.insert(task.id.0.clone(), task.state);
                    }
                }
                if !tasks.is_empty() {
                    let chatting = tasks
                        .iter()
                        .filter(|t| t.state == TaskState::Chatting)
                        .count();
                    let ready = tasks.iter().filter(|t| t.state == TaskState::Ready).count();
                    let submitting = tasks
                        .iter()
                        .filter(|t| t.state == TaskState::Submitting)
                        .count();
                    let awaiting = tasks
                        .iter()
                        .filter(|t| t.state == TaskState::AwaitingMerge)
                        .count();
                    let merged = tasks
                        .iter()
                        .filter(|t| t.state == TaskState::Merged)
                        .count();
                    let stopped = tasks
                        .iter()
                        .filter(|t| t.state == TaskState::Stopped)
                        .count();
                    let mut status = format!(
                        "[{}] {} chatting, {} ready, {} submitting, {} awaiting, {} merged",
                        chrono::Local::now().format("%H:%M:%S"),
                        chatting,
                        ready,
                        submitting,
                        awaiting,
                        merged
                    );
                    if stopped > 0 {
                        status.push_str(&format!(", {} stopped", stopped));
                    }
                    eprintln!("{status}");
                }

                if let Some(secs) = timeout {
                    if start.elapsed().as_secs() >= secs {
                        eprintln!("[daemon] Timeout reached ({}s), shutting down", secs);
                        supervisor.stop_all();
                        break;
                    }
                }

                if exit_on_idle && all_tasks_idle(&service) {
                    idle_grace_ticks += 1;
                    if idle_grace_ticks >= IDLE_GRACE_MAX {
                        eprintln!("[daemon] All tasks idle, shutting down");
                        supervisor.stop_all();
                        break;
                    }
                } else {
                    idle_grace_ticks = 0;
                }

                if once {
                    eprintln!("[daemon] --once mode, exiting after single tick");
                    supervisor.stop_all();
                    break;
                }

                if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                    eprintln!("[daemon] Received signal, shutting down gracefully");
                    supervisor.stop_all();
                    break;
                }

                std::thread::sleep(std::time::Duration::from_secs(
                    tick_interval_secs,
                ));
            }

            let final_tasks = service.list_tasks()?;
            let json =
                serde_json::to_string_pretty(&final_tasks).unwrap_or_else(|_| "[]".to_string());
            println!("{json}");
        }
        Commands::Profiles => {
            print_profiles();
        }
        Commands::SelfTest { json } => {
            let critical_ok = run_self_test(json);
            std::process::exit(if critical_ok { 0 } else { 1 });
        }
        Commands::Doctor { json } => {
            let healthy = run_doctor(json)?;
            std::process::exit(if healthy { 0 } else { 1 });
        }
        Commands::Wizard {
            enable,
            per_model_concurrency,
        } => {
            print_banner();
            eprintln!("\x1b[35mWelcome to Othala first-time setup\x1b[0m");
            eprintln!();

            eprintln!("\x1b[33mProbing model availability...\x1b[0m");
            let report = probe_models(&SetupProbeConfig::default());
            for probe in &report.models {
                let detected_text = if probe.installed {
                    "\x1b[32mdetected\x1b[0m"
                } else {
                    "\x1b[31mnot detected\x1b[0m"
                };
                let health_text = if probe.healthy {
                    "\x1b[32mhealthy\x1b[0m"
                } else {
                    "\x1b[33munhealthy\x1b[0m"
                };

                eprintln!(
                    "  - {:<7} : {} / {}",
                    model_name(probe.model),
                    detected_text,
                    health_text
                );
            }
            eprintln!();

            let selected_models = if let Some(raw) = enable {
                parse_enable_models_csv(&raw)?
            } else {
                prompt_enabled_models()?
            };

            let validated = validate_setup_selection(
                &report,
                &ModelSetupSelection {
                    enabled_models: selected_models,
                },
            )
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;

            let setup_summary = summarize_setup(&report, &validated);
            eprintln!("\x1b[33mSetup summary\x1b[0m");
            for item in &setup_summary.items {
                if item.selected {
                    let status = if item.healthy {
                        "\x1b[32mready\x1b[0m"
                    } else {
                        "\x1b[33mselected with warnings\x1b[0m"
                    };
                    eprintln!("  - {:<7} : {}", model_name(item.model), status);
                }
            }
            eprintln!();

            let config_path = PathBuf::from(".othala/config.toml");
            let mut org_config = if config_path.exists() {
                load_org_config(&config_path)?
            } else {
                default_org_config(validated.enabled_models.clone())
            };

            let per_model = per_model_concurrency.unwrap_or(10);
            apply_setup_selection_to_org_config(
                &mut org_config,
                &validated.enabled_models,
                per_model,
            )
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;
            if org_config.models.default.is_none() {
                org_config.models.default = validated.enabled_models.first().copied();
            }

            save_org_config(&config_path, &org_config)?;

            let context_main_path = PathBuf::from(".othala/context/MAIN.md");
            let context_generated = if context_main_path.exists() {
                false
            } else {
                let repo_root = std::env::current_dir()?;
                let template_dir = PathBuf::from("templates/prompts");
                run_context_gen_with_status(
                    &repo_root,
                    &template_dir,
                    validated
                        .enabled_models
                        .first()
                        .copied()
                        .unwrap_or(ModelKind::Claude),
                )?;
                true
            };

            eprintln!("\x1b[32mSetup complete\x1b[0m");
            eprintln!("  - Config: {}", config_path.display());
            eprintln!(
                "  - Enabled models: {}",
                validated
                    .enabled_models
                    .iter()
                    .map(|m| model_name(*m))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            eprintln!("  - Per-model concurrency: {per_model}");
            if context_generated {
                eprintln!("  - Context: \x1b[32mgenerated\x1b[0m");
            } else {
                eprintln!("  - Context: \x1b[33malready present\x1b[0m");
            }
            if setup_summary.all_selected_healthy {
                eprintln!("  - Model health: \x1b[32mall selected models healthy\x1b[0m");
            } else {
                eprintln!("  - Model health: \x1b[33msome selected models have warnings\x1b[0m");
            }
        }
        Commands::Logs { id, limit, json } => {
            let events = if let Some(ref task_id_str) = id {
                service.task_events(&TaskId::new(task_id_str))?
            } else {
                service.global_events()?
            };

            let display_events: Vec<_> = if events.len() > limit {
                events[events.len() - limit..].to_vec()
            } else {
                events
            };

            if json {
                let out = serde_json::to_string_pretty(&display_events)
                    .unwrap_or_else(|_| "[]".to_string());
                println!("{out}");
            } else if display_events.is_empty() {
                println!("No events found.");
            } else {
                for event in &display_events {
                    let ts = event.at.format("%Y-%m-%d %H:%M:%S");
                    let task_label = event.task_id.as_ref().map(|t| t.0.as_str()).unwrap_or("-");
                    let kind_str = format_event_kind(&event.kind);
                    println!("{ts}  {task_label:<24} {kind_str}");
                }
            }
        }
        Commands::Replay {
            task_id,
            since,
            until,
            json,
            all,
        } => {
            let since_dt = parse_time_filter("since", since.as_deref())?;
            let until_dt = parse_time_filter("until", until.as_deref())?;

            let events = if all || task_id.is_none() {
                service
                    .store
                    .list_all_events(since.as_deref(), until.as_deref())?
            } else if let Some(ref tid) = task_id {
                let events = service.store.list_events_for_task(tid.as_str())?;
                filter_events_by_time(events, since_dt, until_dt)
            } else {
                vec![]
            };

            if json {
                println!("{}", serde_json::to_string_pretty(&events)?);
            } else if events.is_empty() {
                println!("No events found.");
            } else {
                for event in &events {
                    println!("{}", format_event(event));
                }
            }
        }
        Commands::Tail { id, lines, follow } => {
            let repo_root = std::env::current_dir()?;
            let task_id = TaskId::new(&id);

            let mut displayed_lines = 0usize;

            match orchd::agent_log::tail_agent_log(&repo_root, &task_id, lines) {
                Ok(tail) => {
                    displayed_lines = tail.len();
                    for line in &tail {
                        println!("{line}");
                    }
                }
                Err(err) => {
                    if !follow || err.kind() != ErrorKind::NotFound {
                        return Err(err.into());
                    }
                }
            }

            if follow {
                loop {
                    match orchd::agent_log::read_agent_log(&repo_root, &task_id) {
                        Ok(content) => {
                            let all_lines: Vec<&str> = content.lines().collect();
                            if all_lines.len() < displayed_lines {
                                displayed_lines = 0;
                            }
                            if all_lines.len() > displayed_lines {
                                for line in &all_lines[displayed_lines..] {
                                    println!("{line}");
                                }
                                displayed_lines = all_lines.len();
                            }
                        }
                        Err(err) => {
                            if err.kind() != ErrorKind::NotFound {
                                return Err(err.into());
                            }
                        }
                    }

                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
        Commands::Compact { task_id, max_lines } => {
            let repo_root = std::env::current_dir()?;
            let task = TaskId::new(&task_id);
            let content = orchd::agent_log::read_agent_log(&repo_root, &task)
                .map_err(|err| anyhow::anyhow!("failed to read latest agent output for {task_id}: {err}"))?;
            let lines: Vec<String> = content.lines().map(String::from).collect();

            let result = orchd::agent_log::compact_context(&lines, max_lines.unwrap_or(120));
            let compacted_path =
                orchd::agent_log::save_compacted_summary(&repo_root, &task, &result.summary)?;

            println!(
                "Compacted {task_id}: {} -> {} lines (ratio {:.3})",
                result.original_lines, result.compacted_lines, result.compression_ratio
            );
            println!("Saved compacted summary: {}", compacted_path.display());
            if !result.summary.is_empty() {
                println!("{}", result.summary);
            }
        }
        Commands::Watch { task, lines } => {
            run_watch_command(&service, task, lines)?;
        }
        Commands::Runs { id, json } => {
            let runs = service.task_runs(&TaskId::new(&id))?;
            if json {
                let out = serde_json::to_string_pretty(&runs).unwrap_or_else(|_| "[]".to_string());
                println!("{out}");
            } else if runs.is_empty() {
                println!("No runs found for task: {id}");
            } else {
                let header = format!(
                    "{:<36} {:<8} {:<20} {:<20} {:<12} {}",
                    "RUN ID", "MODEL", "STARTED", "FINISHED", "EXIT CODE", "STOP REASON"
                );
                println!("{header}");
                for run in &runs {
                    let started = run.started_at.format("%Y-%m-%d %H:%M:%S");
                    let finished = run
                        .finished_at
                        .map(|f| f.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_else(|| "\x1b[33mrunning\x1b[0m".to_string());
                    let exit_code = run
                        .exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let stop_reason = run.stop_reason.as_deref().unwrap_or("-");
                    println!(
                        "{:<36} {:<8} {:<20} {:<20} {:<12} {}",
                        run.run_id,
                        run.model.as_str(),
                        started,
                        finished,
                        exit_code,
                        stop_reason
                    );
                }
            }
        }
        Commands::Retries { id, json } => {
            let task_id = TaskId::new(&id);
            let events = service.task_events(&task_id)?;
            let runs = service.task_runs(&task_id)?;
            let retry_events = collect_retry_events(&events);
            let timeline = build_retry_timeline(&events, &runs);

            if json {
                let payload = RetryHistoryOutput {
                    task_id: id,
                    retry_events,
                    timeline,
                };
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!("{}", format_retries_timeline(&task_id.0, &timeline));
            }
        }
        Commands::DiffRetries { task_id } => {
            let repo_root = std::env::current_dir()?;
            let task = TaskId::new(&task_id);
            let log_dir = orchd::agent_log::agent_log_dir(&repo_root, &task);
            let latest_path = log_dir.join("latest.log");
            let previous_path = log_dir.join("latest.log.1");

            let latest_content = fs::read_to_string(&latest_path).map_err(|err| {
                anyhow::anyhow!("failed to read {}: {err}", latest_path.display())
            })?;
            let previous_content = fs::read_to_string(&previous_path).map_err(|err| {
                anyhow::anyhow!("failed to read {}: {err}", previous_path.display())
            })?;

            let latest_lines: Vec<String> = latest_content.lines().map(String::from).collect();
            let previous_lines: Vec<String> = previous_content.lines().map(String::from).collect();

            let diff = orchd::agent_log::diff_agent_outputs(&previous_lines, &latest_lines);
            let output = orchd::agent_log::format_diff(&diff);
            if output.is_empty() {
                println!("No differences between the last two retries for task: {task_id}");
            } else {
                println!("{output}");
            }

            let summary = orchd::agent_log::diff_summary(&diff);
            println!(
                "Summary: +{} -{} unchanged:{}",
                summary.added, summary.removed, summary.unchanged
            );
        }
        Commands::Stats { json } => {
            let tasks = service.list_tasks()?;
            let state_counts = service.store.task_count_by_state()?;
            let total_events = service.store.total_event_count()?;
            let summary = compute_stats_summary(&tasks, state_counts, total_events);

            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print_stats_table(&summary);
            }
        }
        Commands::Gc {
            older_than_days,
            dry_run,
        } => {
            let repo_root = std::env::current_dir()?;
            let summary = gc_logs(&repo_root, older_than_days, dry_run)?;
            let action = if dry_run { "Would delete" } else { "Deleted" };
            println!(
                "{action} {} event files, {} agent output dirs (freed ~{})",
                summary.deleted_event_files,
                summary.deleted_agent_output_dirs,
                format_bytes(summary.bytes_freed)
            );
        }
        Commands::Stop { id } => {
            let task_id = TaskId::new(&id);
            let now = Utc::now();
            let event_id = EventId(format!("E-STOP-{}-{}", id, now.timestamp_millis()));
            match service.transition_task_state(&task_id, TaskState::Stopped, event_id, now) {
                Ok(task) => println!("Stopped: {} (was {})", id, task.state),
                Err(e) => {
                    eprintln!("Failed to stop {id}: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Cancel { id } => {
            let task_id = TaskId::new(&id);
            match cancel_task(&service, &task_id, "requested by user") {
                Ok(from_state) => println!("Cancelled: {id} ({from_state} -> STOPPED)"),
                Err(e) => {
                    eprintln!("Failed to cancel {id}: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Resume { id } => {
            let task_id = TaskId::new(&id);
            let now = Utc::now();
            let event_id = EventId(format!("E-RESUME-{}-{}", id, now.timestamp_millis()));
            match service.transition_task_state(&task_id, TaskState::Chatting, event_id, now) {
                Ok(_) => println!("Resumed: {id} -> Chatting"),
                Err(e) => {
                    eprintln!("Failed to resume {id}: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Deps { json } => {
            let tasks = service.list_tasks()?;
            if json {
                let deps: Vec<serde_json::Value> = tasks
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "id": t.id.0,
                            "title": t.title,
                            "state": format!("{}", t.state),
                            "parent": t.parent_task_id.as_ref().map(|p| &p.0),
                            "depends_on": t.depends_on.iter().map(|d| &d.0).collect::<Vec<_>>(),
                            "branch": t.branch_name,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&deps).unwrap_or_else(|_| "[]".to_string())
                );
            } else {
                let root_tasks: Vec<_> = tasks
                    .iter()
                    .filter(|t| t.parent_task_id.is_none())
                    .collect();

                if tasks.is_empty() {
                    println!("No tasks.");
                } else {
                    for root in &root_tasks {
                        print_dep_tree(&tasks, root, 0);
                    }
                    let orphans: Vec<_> = tasks
                        .iter()
                        .filter(|t| {
                            t.parent_task_id.is_some()
                                && !tasks
                                    .iter()
                                    .any(|p| Some(&p.id) == t.parent_task_id.as_ref())
                        })
                        .collect();
                    for orphan in &orphans {
                        print_dep_tree(&tasks, orphan, 0);
                    }
                }
            }
        }
        Commands::Template { action } => {
            let repo_root = std::env::current_dir()?;
            match action {
                TemplateAction::List => {
                    let names = list_templates(&repo_root)?;
                    if names.is_empty() {
                        println!("No templates found.");
                    } else {
                        for name in names {
                            println!("{name}");
                        }
                    }
                }
                TemplateAction::Create { name, from_task } => {
                    let task_id = TaskId::new(&from_task);
                    let Some(task) = service.task(&task_id)? else {
                        anyhow::bail!("task not found: {from_task}");
                    };
                    let template = TaskTemplate::from_task(&task);
                    save_template(&repo_root, &name, &template)?;
                    println!("Saved template: {name}");
                }
                TemplateAction::Show { name } => {
                    let template = load_template(&repo_root, &name)?;
                    println!("{}", serde_json::to_string_pretty(&template)?);
                }
            }
        }
        Commands::Export { output, task_id } => {
            let tasks = service.list_tasks()?;
            let parents: HashMap<String, Option<String>> = tasks
                .iter()
                .map(|task| {
                    let parent_branch = task.parent_task_id.as_ref().and_then(|parent_id| {
                        tasks
                            .iter()
                            .find(|candidate| candidate.id == *parent_id)
                            .and_then(|parent| parent.branch_name.clone())
                    });
                    (task.id.0.clone(), parent_branch)
                })
                .collect();

            let records: Vec<TaskExportRecord> = if let Some(id) = task_id {
                let task = tasks
                    .iter()
                    .find(|task| task.id.0 == id)
                    .ok_or_else(|| anyhow::anyhow!("task not found: {id}"))?;
                vec![TaskExportRecord::from_task(
                    task,
                    parents.get(&task.id.0).cloned().flatten(),
                )]
            } else {
                tasks
                    .iter()
                    .map(|task| {
                        TaskExportRecord::from_task(
                            task,
                            parents.get(&task.id.0).cloned().flatten(),
                        )
                    })
                    .collect()
            };

            let payload = serde_json::to_string_pretty(&records)?;
            std::fs::write(&output, payload)?;
            println!("Exported {} task(s) to {}", records.len(), output.display());
        }
        Commands::Import { input } => {
            let payload = std::fs::read_to_string(&input)?;
            let records: Vec<TaskExportRecord> = serde_json::from_str(&payload)?;
            let mut imported = 0usize;

            for record in records {
                let existing = service.task(&TaskId::new(record.task_id.clone()))?;
                let task = import_record_to_task(record, existing)?;
                service.upsert_task(&task)?;
                imported += 1;
            }

            println!("Imported {} task(s) from {}", imported, input.display());
        }
        Commands::Costs { task, budget } => {
            if budget {
                let repo_root = std::env::current_dir()?;
                let config_path = repo_root.join(".othala/config.toml");
                let budget_config = load_org_config(&config_path)
                    .map(|config| config.budget)
                    .unwrap_or_default();

                let mut all_runs = Vec::new();
                for task in service.list_tasks()? {
                    all_runs.extend(service.task_runs(&task.id)?);
                }

                let now = Utc::now();
                let (used_today, used_month) = aggregate_budget_usage(&all_runs, now);
                println!("Budget enabled: {}", budget_config.enabled);
                println!(
                    "Daily: used={} limit={} remaining={}",
                    used_today,
                    budget_config.daily_token_limit,
                    budget_config.daily_token_limit.saturating_sub(used_today)
                );
                println!(
                    "Monthly: used={} limit={} remaining={}",
                    used_month,
                    budget_config.monthly_token_limit,
                    budget_config.monthly_token_limit.saturating_sub(used_month)
                );
            } else {
                let tasks = service.list_tasks()?;
                if let Some(task_id) = task {
                    let runs = service.task_runs(&TaskId::new(&task_id))?;
                    let estimates = aggregate_cost_estimates(&runs);
                    let total_tokens: u64 = estimates.iter().map(|e| e.input_tokens).sum();
                    let total_duration: f64 = estimates.iter().map(|e| e.duration_secs).sum();
                    println!("Task: {task_id}");
                    println!("Estimated tokens: {total_tokens}");
                    println!("Duration (secs): {:.2}", total_duration);
                    for estimate in estimates {
                        println!(
                            "  model={} tokens={} duration_secs={:.2}",
                            estimate.model.as_str(),
                            estimate.input_tokens,
                            estimate.duration_secs
                        );
                    }
                } else {
                    let mut total_tokens = 0u64;
                    let mut total_duration = 0.0f64;
                    for task in &tasks {
                        let runs = service.task_runs(&task.id)?;
                        let estimates = aggregate_cost_estimates(&runs);
                        let task_tokens: u64 = estimates.iter().map(|e| e.input_tokens).sum();
                        let task_duration: f64 = estimates.iter().map(|e| e.duration_secs).sum();
                        total_tokens += task_tokens;
                        total_duration += task_duration;
                        println!(
                            "{} tokens={} duration_secs={:.2}",
                            task.id.0, task_tokens, task_duration
                        );
                    }
                    println!("TOTAL tokens={} duration_secs={:.2}", total_tokens, total_duration);
                }
            }
        }
        Commands::Prune {
            older_than_days,
            force,
        } => {
            let now = Utc::now();
            let cutoff = now - chrono::Duration::days(older_than_days);
            let tasks = service.list_tasks()?;
            let prunable: Vec<_> = tasks
                .iter()
                .filter(|t| t.state.is_terminal() && t.updated_at < cutoff)
                .collect();

            if prunable.is_empty() {
                println!(
                    "No tasks to prune (older than {older_than_days} days in terminal state)."
                );
            } else {
                println!(
                    "{}",
                    if force {
                        "Pruning tasks:"
                    } else {
                        "Would prune (use --force to delete):"
                    }
                );
                for task in &prunable {
                    let age_days = (now - task.updated_at).num_days();
                    println!(
                        "  {} ({}, {} days old) - {}",
                        task.id.0, task.state, age_days, task.title
                    );
                    if force {
                        if let Err(e) = service.delete_task(&task.id) {
                            eprintln!("    Failed to delete: {e}");
                        }
                    }
                }
                if !force {
                    println!("\nRun with --force to actually delete.");
                }
            }
        }
        Commands::Archive {
            older_than_days,
            json,
        } => {
            let archived = archive_old_tasks(&service, older_than_days)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({ "archived": archived }))?
                );
            } else {
                println!("Archived {} tasks", archived);
            }
        }
        Commands::Permissions { json } => {
            let policy = PermissionPolicy::default_policy();
            if json {
                println!("{}", serde_json::to_string_pretty(&policy).unwrap_or_default());
            } else {
                println!("{}", policy.display_table());
            }
        }
        Commands::Permit {
            category,
            path,
            model,
        } => {
            let cat: ToolCategory = category
                .parse()
                .unwrap_or(ToolCategory::Custom(category.clone()));
            let rule = PermissionRule {
                category: cat.clone(),
                permission: ToolPermission::Allow,
                path_pattern: path.clone(),
                reason: None,
            };
            let model_scope = model
                .as_deref()
                .map(|m| format!(" for model {m}"))
                .unwrap_or_default();
            println!(
                "Granted: {} permission for {}{}{}",
                ToolPermission::Allow.as_str(),
                cat,
                model_scope,
                path.map(|p| format!(" (path: {p})")).unwrap_or_default()
            );
            let _ = rule;
        }
        Commands::Deny {
            category,
            path,
            model,
        } => {
            let cat: ToolCategory = category
                .parse()
                .unwrap_or(ToolCategory::Custom(category.clone()));
            let rule = PermissionRule {
                category: cat.clone(),
                permission: ToolPermission::Deny,
                path_pattern: path.clone(),
                reason: None,
            };
            println!(
                "Denied: {} for {}{}",
                cat,
                model.as_deref().unwrap_or("all models"),
                path.map(|p| format!(" (path: {p})")).unwrap_or_default()
            );
            let _ = rule;
        }
        Commands::Mcp => {
            use orchd::mcp::McpServer;

            let mut server = McpServer::new();
            server.register_builtin_tools();
            eprintln!("Othala MCP server started (stdin/stdout)");
            if let Err(e) = server.run_stdio() {
                eprintln!("MCP server error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Skills => {
            let repo_root = std::env::current_dir()?;
            let registry = SkillRegistry::discover(&repo_root);
            let skills = registry.list_skills();
            if skills.is_empty() {
                println!("No skills found.");
            } else {
                println!("{:<20} {:<40} TAGS", "NAME", "DESCRIPTION");
                println!("{}", "-".repeat(96));
                for skill in skills {
                    let tags = if skill.tags.is_empty() {
                        "-".to_string()
                    } else {
                        skill.tags.join(",")
                    };
                    println!("{:<20} {:<40} {}", skill.name, skill.description, tags);
                }
            }
        }
        Commands::Skill { name } => {
            let repo_root = std::env::current_dir()?;
            let registry = SkillRegistry::discover(&repo_root);
            if let Some(skill) = registry.load_skill(&name) {
                print!("{}", skill.content);
            } else {
                eprintln!("Skill not found: {name}");
                std::process::exit(1);
            }
        }
        Commands::ListCommands { json } => {
            let commands = orchd::custom_commands::discover_all_commands(Path::new("."));
            if json {
                println!("{}", serde_json::to_string_pretty(&commands).unwrap_or_default());
            } else if commands.is_empty() {
                println!("No custom commands found.");
                println!("Add .md files to:");
                println!("  ~/.config/othala/commands/  (user commands)");
                println!("  .othala/commands/           (project commands)");
            } else {
                println!("{}", orchd::custom_commands::display_commands_table(&commands));
            }
        }
        Commands::RunCommand { name, args, json } => {
            let commands = orchd::custom_commands::discover_all_commands(Path::new("."));
            let Some(cmd) = commands
                .iter()
                .find(|c| format!("{}:{}", c.prefix, c.name) == name || c.name == name)
            else {
                eprintln!("Command not found: {name}");
                std::process::exit(1);
            };
            let mut arg_map = std::collections::HashMap::new();
            for a in &args {
                if let Some((k, v)) = a.split_once('=') {
                    arg_map.insert(k.to_string(), v.to_string());
                }
            }
            match orchd::custom_commands::render_command(cmd, &arg_map) {
                Ok(rendered) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "command": name,
                                "rendered": rendered,
                            })
                        );
                    } else {
                        println!("{rendered}");
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Prompt {
            text,
            model,
            format,
            quiet,
        } => {
            let result = orchd::custom_commands::execute_prompt(&text, &model, &format);
            match format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default()),
                _ => {
                    if !quiet {
                        eprintln!("Model: {}", result.model);
                    }
                    println!("{}", result.response);
                }
            }
        }
        Commands::Upgrade { install, json } => {
            let info = orchd::upgrade::check_for_update();
            if json {
                println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
            } else if install && info.update_available {
                println!("Upgrading from {} to {} ...", info.current, info.latest.as_deref().unwrap_or("unknown"));
                match orchd::upgrade::perform_upgrade() {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => {
                        eprintln!("Upgrade failed: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                println!("{}", orchd::upgrade::display_version_check(&info));
            }
        }
        Commands::Models { json } => {
            let registry = orchd::provider_registry::ModelRegistry::new();
            if json {
                println!("{}", serde_json::to_string_pretty(&registry).unwrap_or_default());
            } else {
                println!("{}", registry.display_table());
            }
        }
        Commands::Providers { json } => {
            let registry = orchd::provider_registry::ModelRegistry::new();
            if json {
                let providers = registry.list_providers();
                println!("{}", serde_json::to_string_pretty(&providers).unwrap_or_default());
            } else {
                for p in registry.list_providers() {
                    println!("{} ({})", p.display_name, p.name);
                    println!("  API: {}", p.api_base);
                    println!("  Auth: ${}", p.auth_env_var);
                    let models = registry.models_for_provider(&p.name);
                    if !models.is_empty() {
                        println!("  Models:");
                        for m in models {
                            println!("    - {} ({}K ctx)", m.display_name, m.context_window / 1000);
                        }
                    }
                    println!();
                }
            }
        }
        Commands::Ignore { json } => {
            let rules = orchd::ignore::load_ignore_rules(Path::new("."));
            if json {
                let patterns: Vec<String> = rules.patterns().iter().map(|p| match p {
                    orchd::ignore::IgnorePattern::Include(s) => s.clone(),
                    orchd::ignore::IgnorePattern::Exclude(s) => format!("!{s}"),
                }).collect();
                println!("{}", serde_json::to_string_pretty(&patterns).unwrap_or_default());
            } else {
                println!("{}", orchd::ignore::display_ignore_rules(&rules));
            }
        }
        Commands::Metrics { json } => {
            let collector = orchd::metrics::MetricsCollector::new(
                orchd::metrics::MetricsConfig::default(),
            );
            if json {
                println!("{}", serde_json::to_string_pretty(&collector.summary()).unwrap_or_default());
            } else {
                println!("{}", collector.display_summary());
            }
        }
        Commands::CiGen { output, workflow, dry_run } => {
            let config = orchd::ci_gen::CiConfig::default();
            let content = match workflow.as_str() {
                "verify" => orchd::ci_gen::generate_verify_workflow(&config),
                "nix" => orchd::ci_gen::generate_nix_ci(&config),
                "basic" => orchd::ci_gen::generate_basic_ci(&config),
                _ => orchd::ci_gen::generate_github_actions(&config),
            };
            if dry_run {
                println!("{content}");
            } else {
                let out_dir = output.unwrap_or_else(|| PathBuf::from(".github/workflows"));
                fs::create_dir_all(&out_dir).ok();
                let file_path = out_dir.join("othala.yml");
                match fs::write(&file_path, &content) {
                    Ok(()) => println!("Written: {}", file_path.display()),
                    Err(e) => eprintln!("Failed to write {}: {e}", file_path.display()),
                }
            }
        }
        Commands::Edit { task_id } => {
            let config = orchd::editor::EditorConfig::default();
            let title = task_id.as_deref().unwrap_or("new prompt");
            match orchd::editor::open_editor_for_prompt(&config, title) {
                Ok(content) => {
                    if content.trim().is_empty() {
                        eprintln!("Empty prompt, aborting.");
                    } else {
                        println!("{content}");
                    }
                }
                Err(e) => eprintln!("Editor error: {e}"),
            }
        }
        Commands::Delegate { task_id, json } => {
            let plan = orchd::delegation::DelegationPlan::new(&task_id);
            if json {
                println!("{}", serde_json::to_string_pretty(&plan).unwrap_or_default());
            } else {
                println!("{}", plan.summary());
            }
        }
        Commands::Templates { json } => {
            let templates = orchd::task_templates::discover_templates(Path::new("."));
            if json {
                println!("{}", serde_json::to_string_pretty(&templates).unwrap_or_default());
            } else if templates.is_empty() {
                println!("No task templates found.");
                println!("Add .yaml files to .othala/templates/ or ~/.config/othala/templates/");
            } else {
                println!("{}", orchd::task_templates::display_templates_table(&templates));
            }
        }
        Commands::Health { json } => {
            let health = orchd::daemon_status::DaemonHealth::new();
            if json {
                println!("{}", serde_json::to_string_pretty(&health).unwrap_or_default());
            } else {
                println!("{}", health.display_full());
            }
        }
        Commands::Find { query, json } => {
            let search_query = orchd::search::parse_search_query(&query);
            let index = orchd::search::SearchIndex::new();
            let results = index.search(&search_query);
            if json {
                println!("{}", serde_json::to_string_pretty(&results).unwrap_or_default());
            } else if results.is_empty() {
                println!("No results for: {query}");
            } else {
                println!("{}", orchd::search::display_search_results(&results));
            }
        }
        Commands::Lsp { action } => match action {
            LspAction::List => {
                let config = orchd::lsp::LspConfig::default();
                for (lang_id, server_cfg) in &config.language_servers {
                    println!("{lang_id}: {} {}", server_cfg.command, server_cfg.args.join(" "));
                }
            }
            LspAction::Status { json } => {
                let manager = orchd::lsp::LspManager::new(orchd::lsp::LspConfig::default());
                let servers = manager.active_servers();
                if json {
                    println!("{}", serde_json::to_string_pretty(&servers).unwrap_or_default());
                } else if servers.is_empty() {
                    println!("No active LSP servers.");
                } else {
                    for (lang_id, initialized) in &servers {
                        let status = if *initialized { "initialized" } else { "starting" };
                        println!("{lang_id}: {status}");
                    }
                }
            }
        },
        Commands::RateLimits { json } => {
            let config = orchd::rate_limiter::RateLimitConfig::default();
            if json {
                println!("{}", serde_json::to_string_pretty(&config).unwrap_or_default());
            } else {
                println!("Rate Limits:");
                println!("  Per-minute: {}", config.requests_per_minute);
                println!("  Per-hour:   {}", config.requests_per_hour);
                println!("  Burst:      {}", config.burst_size);
            }
        }
        Commands::Timeouts { json } => {
            let config = orchd::task_timeout::TimeoutConfig::default();
            let tracker = orchd::task_timeout::TimeoutTracker::new(config.clone());
            if json {
                println!("{}", serde_json::to_string_pretty(&config).unwrap_or_default());
            } else {
                println!("Timeout Config:");
                println!("  Default:  {}s", config.default_timeout_secs);
                println!("  Maximum:  {}s", config.max_timeout_secs);
                println!("  Grace:    {}s", config.grace_period_secs);
                println!("  Interval: {}s", config.check_interval_secs);
                println!("  Tracked:  {}", tracker.active_count());
            }
        }
        Commands::Env { task_id, model, redacted, json } => {
            let config = orchd::env_inject::EnvConfig::default();
            let injector = orchd::env_inject::EnvInjector::new(config);
            let tid = task_id.as_deref().unwrap_or("example-task");
            let mdl = model.as_deref().unwrap_or("claude");
            let env_map = if redacted {
                injector.redacted_env(tid, mdl)
            } else {
                injector.build_env(tid, mdl)
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&env_map).unwrap_or_default());
            } else {
                let mut keys: Vec<_> = env_map.keys().collect();
                keys.sort();
                for key in keys {
                    println!("{}={}", key, env_map[key]);
                }
            }
        }
        Commands::McpHttp { bind, port } => {
            let config = orchd::mcp_transport::TransportConfig {
                kind: orchd::mcp_transport::TransportKind::Http {
                    bind_addr: bind.clone(),
                    port,
                },
                ..Default::default()
            };
            println!("Starting MCP HTTP/SSE server on {bind}:{port}");
            let transport = orchd::mcp_transport::HttpTransport::new(&config);
            match transport {
                Ok(t) => {
                    println!("MCP HTTP transport ready");
                    println!("  POST {bind}:{port}/rpc  - JSON-RPC endpoint");
                    println!("  GET  {bind}:{port}/sse  - SSE event stream");
                    println!("  GET  {bind}:{port}/health - Health check");
                    if let Err(e) = t.bind() {
                        eprintln!("Transport bind error: {e}");
                    }
                }
                Err(e) => eprintln!("Failed to start MCP HTTP transport: {e}"),
            }
        }
        Commands::Conversations { action } => match action {
            ConversationAction::List { task_id, json } => {
                let store = orchd::conversation::ConversationStore::new();
                if let Some(tid) = &task_id {
                    let convos = store.get_task_conversations(tid);
                    if json {
                        let info: Vec<_> = convos.iter().map(|c| serde_json::json!({
                            "id": c.id, "task_id": c.task_id, "messages": c.messages.len(),
                            "total_tokens": c.total_tokens
                        })).collect();
                        println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
                    } else if convos.is_empty() {
                        println!("No conversations for task {tid}");
                    } else {
                        for c in &convos {
                            println!("{} ({} messages, {} tokens)", c.id, c.messages.len(), c.total_tokens);
                        }
                    }
                } else {
                    println!("Use --task-id to filter conversations");
                }
            }
            ConversationAction::Show { id, limit, json } => {
                let store = orchd::conversation::ConversationStore::new();
                if let Some(convo) = store.get_conversation(&id) {
                    if json {
                        println!("{}", serde_json::to_string_pretty(convo).unwrap_or_default());
                    } else {
                        let msgs = store.get_messages(&id, limit, None);
                        for m in msgs {
                            println!("[{}] {:?}: {}", m.timestamp.format("%H:%M:%S"), m.role, &m.content[..m.content.len().min(200)]);
                        }
                    }
                } else {
                    eprintln!("Conversation not found: {id}");
                }
            }
            ConversationAction::Export { id } => {
                let store = orchd::conversation::ConversationStore::new();
                match store.export_conversation(&id) {
                    Ok(json_str) => println!("{json_str}"),
                    Err(e) => eprintln!("Export failed: {e}"),
                }
            }
            ConversationAction::Search { query, json } => {
                let store = orchd::conversation::ConversationStore::new();
                let results = store.search_messages(&query);
                if json {
                    let info: Vec<_> = results.iter().map(|(c, m)| serde_json::json!({
                        "conversation_id": c.id, "message_id": m.id, "role": format!("{:?}", m.role),
                        "content": &m.content[..m.content.len().min(200)]
                    })).collect();
                    println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
                } else if results.is_empty() {
                    println!("No messages matching: {query}");
                } else {
                    for (c, m) in &results {
                        println!("[{}] {:?}: {}...", c.id, m.role, &m.content[..m.content.len().min(100)]);
                    }
                }
            }
        },
        Commands::Shell { json } => {
            let config = orchd::shell_config::ShellConfig::default();
            let detected = orchd::shell_config::ShellRunner::detect_shell();
            if json {
                println!("{}", serde_json::to_string_pretty(&config).unwrap_or_default());
            } else {
                println!("Shell Config:");
                println!("  Path:      {}", config.path);
                println!("  Args:      {:?}", config.args);
                println!("  Timeout:   {}s", config.timeout_secs);
                println!("  Inherit:   {}", config.inherit_env);
                println!("  Detected:  {detected:?}");
            }
        }
    }

    Ok(())
}

fn print_dep_tree(tasks: &[Task], task: &Task, depth: usize) {
    let indent = "  ".repeat(depth);
    let state_color = match task.state {
        TaskState::Chatting => "\x1b[36m",
        TaskState::Ready => "\x1b[32m",
        TaskState::Merged => "\x1b[90m",
        TaskState::Stopped => "\x1b[31m",
        TaskState::AwaitingMerge => "\x1b[33m",
        _ => "\x1b[0m",
    };
    let branch = task.branch_name.as_deref().unwrap_or("-");
    println!(
        "{indent}{state_color}{}\x1b[0m {} [{}] ({})",
        task.state, task.id.0, task.title, branch
    );
    let children: Vec<_> = tasks
        .iter()
        .filter(|t| t.parent_task_id.as_ref() == Some(&task.id))
        .collect();
    for child in children {
        print_dep_tree(tasks, child, depth + 1);
    }
}

fn parse_time_filter(name: &str, value: Option<&str>) -> anyhow::Result<Option<chrono::DateTime<Utc>>> {
    value
        .map(|raw| {
            chrono::DateTime::parse_from_rfc3339(raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|err| anyhow::anyhow!("invalid --{name} timestamp '{raw}': {err}"))
        })
        .transpose()
}

fn filter_events_by_time(
    events: Vec<Event>,
    since: Option<chrono::DateTime<Utc>>,
    until: Option<chrono::DateTime<Utc>>,
) -> Vec<Event> {
    events
        .into_iter()
        .filter(|event| since.is_none_or(|start| event.at >= start))
        .filter(|event| until.is_none_or(|end| event.at <= end))
        .collect()
}

fn format_event(event: &Event) -> String {
    let (kind_name, kind_desc) = match &event.kind {
        EventKind::TaskCreated => ("TaskCreated", "Task created".to_string()),
        EventKind::TaskStateChanged { from, to } => {
            ("TaskStateChanged", format!("{from} -> {to}"))
        }
        EventKind::ParentHeadUpdated { parent_task_id } => {
            ("ParentHeadUpdated", format!("parent_task_id={}", parent_task_id.0))
        }
        EventKind::RestackStarted => ("RestackStarted", "Restack started".to_string()),
        EventKind::RestackCompleted => ("RestackCompleted", "Restack completed".to_string()),
        EventKind::RestackConflict => ("RestackConflict", "Restack conflict".to_string()),
        EventKind::VerifyStarted => ("VerifyStarted", "Verify started".to_string()),
        EventKind::VerifyCompleted { success } => {
            ("VerifyCompleted", format!("success={success}"))
        }
        EventKind::ReadyReached => ("ReadyReached", "Ready reached".to_string()),
        EventKind::SubmitStarted { mode } => ("SubmitStarted", format!("mode={mode:?}")),
        EventKind::SubmitCompleted => ("SubmitCompleted", "Submit completed".to_string()),
        EventKind::NeedsHuman { reason } => ("NeedsHuman", format!("reason={reason}")),
        EventKind::Error { code, message } => {
            ("Error", format!("code={code}, message={message}"))
        }
        EventKind::RetryScheduled {
            attempt,
            model,
            reason,
        } => (
            "RetryScheduled",
            format!("attempt={attempt}, model={model}, reason={reason}"),
        ),
        EventKind::AgentSpawned { model } => ("AgentSpawned", format!("model={model}")),
        EventKind::AgentCompleted {
            model,
            success,
            duration_secs,
        } => (
            "AgentCompleted",
            format!("model={model}, success={success}, duration_secs={duration_secs}"),
        ),
        EventKind::CancellationRequested { reason } => {
            ("CancellationRequested", format!("reason={reason}"))
        }
        EventKind::ModelFallback {
            from_model,
            to_model,
            reason,
        } => (
            "ModelFallback",
            format!("from={from_model}, to={to_model}, reason={reason}"),
        ),
        EventKind::ContextRegenStarted => {
            ("ContextRegenStarted", "Context regen started".to_string())
        }
        EventKind::ContextRegenCompleted { success } => {
            ("ContextRegenCompleted", format!("success={success}"))
        }
        EventKind::ConfigReloaded { changes } => ("ConfigReloaded", format!("changes={changes}")),
        EventKind::TaskFailed { reason, is_final } => {
            ("TaskFailed", format!("reason={reason}, is_final={is_final}"))
        }
        EventKind::TestSpecValidated { passed, details } => (
            "TestSpecValidated",
            format!("passed={passed}, details={details}"),
        ),
        EventKind::OrchestratorDecomposed { sub_task_ids } => (
            "OrchestratorDecomposed",
            format!("sub_task_ids={}", sub_task_ids.join(",")),
        ),
        EventKind::QAStarted { qa_type } => ("QAStarted", format!("qa_type={qa_type}")),
        EventKind::QACompleted {
            passed,
            failed,
            total,
        } => (
            "QACompleted",
            format!("passed={passed}, failed={failed}, total={total}"),
        ),
        EventKind::QAFailed { failures } => ("QAFailed", format!("failures={}", failures.join(";"))),
        EventKind::BudgetExceeded => ("BudgetExceeded", "budget exceeded".to_string()),
    };

    let timestamp = event.at.format("%Y-%m-%d %H:%M:%S");
    let task_label = event.task_id.as_ref().map(|id| id.0.as_str()).unwrap_or("-");
    format!("[{timestamp}] {task_label} | {kind_name} | {kind_desc}")
}

fn format_event_kind(kind: &EventKind) -> String {
    match kind {
        EventKind::TaskCreated => "task_created".to_string(),
        EventKind::TaskStateChanged { from, to } => format!("state: {from} -> {to}"),
        EventKind::ParentHeadUpdated { parent_task_id } => {
            format!("parent_head_updated: {}", parent_task_id.0)
        }
        EventKind::RestackStarted => "restack_started".to_string(),
        EventKind::RestackCompleted => "restack_completed".to_string(),
        EventKind::RestackConflict => "\x1b[31mrestack_conflict\x1b[0m".to_string(),
        EventKind::VerifyStarted => "verify_started".to_string(),
        EventKind::VerifyCompleted { success } => {
            if *success {
                "\x1b[32mverify_passed\x1b[0m".to_string()
            } else {
                "\x1b[31mverify_failed\x1b[0m".to_string()
            }
        }
        EventKind::ReadyReached => "\x1b[32mready\x1b[0m".to_string(),
        EventKind::SubmitStarted { mode } => format!("submit_started ({mode:?})"),
        EventKind::SubmitCompleted => "submit_completed".to_string(),
        EventKind::NeedsHuman { reason } => format!("\x1b[33mneeds_human\x1b[0m: {reason}"),
        EventKind::Error { code, message } => format!("\x1b[31merror\x1b[0m [{code}]: {message}"),
        EventKind::RetryScheduled {
            attempt,
            model,
            reason,
        } => format!("retry #{attempt} ({model}): {reason}"),
        EventKind::AgentSpawned { model } => format!("agent_spawned ({model})"),
        EventKind::AgentCompleted {
            model,
            success,
            duration_secs,
        } => {
            if *success {
                format!("\x1b[32magent_completed\x1b[0m ({model}, {duration_secs}s)")
            } else {
                format!("\x1b[31magent_failed\x1b[0m ({model}, {duration_secs}s)")
            }
        }
        EventKind::CancellationRequested { reason } => {
            format!("\x1b[33mcancellation_requested\x1b[0m: {reason}")
        }
        EventKind::ModelFallback {
            from_model,
            to_model,
            reason,
        } => format!("model_fallback ({from_model} -> {to_model}): {reason}"),
        EventKind::ContextRegenStarted => "context_regen_started".to_string(),
        EventKind::ContextRegenCompleted { success } => {
            if *success {
                "\x1b[32mcontext_regen_completed\x1b[0m".to_string()
            } else {
                "\x1b[31mcontext_regen_failed\x1b[0m".to_string()
            }
        }
        EventKind::ConfigReloaded { changes } => format!("config_reloaded: {changes}"),
        EventKind::TaskFailed { reason, is_final } => {
            let label = if *is_final { "FINAL_FAILURE" } else { "failed" };
            format!("\x1b[31m{label}\x1b[0m: {reason}")
        }
        EventKind::TestSpecValidated { passed, details } => {
            let status = if *passed { "passed" } else { "failed" };
            format!("test_spec_{status}: {details}")
        }
        EventKind::OrchestratorDecomposed { sub_task_ids } => {
            format!("decomposed -> [{}]", sub_task_ids.join(", "))
        }
        EventKind::QAStarted { qa_type } => format!("qa_started ({qa_type})"),
        EventKind::QACompleted {
            passed,
            failed,
            total,
        } => {
            if *failed == 0 {
                format!("\x1b[32mqa_passed\x1b[0m ({passed}/{total})")
            } else {
                format!("\x1b[31mqa_completed\x1b[0m ({passed}/{total}, {failed} failed)")
            }
        }
        EventKind::QAFailed { failures } => {
            format!("\x1b[31mqa_failed\x1b[0m: {}", failures.join("; "))
        }
        EventKind::BudgetExceeded => "\x1b[31mbudget_exceeded\x1b[0m".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use orchd::event_log::JsonlEventLog;
    use orchd::persistence::SqliteStore;
    use orchd::scheduler::SchedulerConfig;
    use orchd::TaskRunRecord;
    use serde_json::Value;
    use std::fs;

    #[test]
    fn create_task_cli_parses_priority_flag() {
        let cli = Cli::try_parse_from([
            "othala",
            "create-task",
            "--repo",
            "example",
            "--title",
            "Add feature",
            "--priority",
            "high",
        ])
        .expect("parse create-task");

        match cli.command {
            Commands::CreateTask { priority, .. } => assert_eq!(priority, "high"),
            _ => panic!("expected create-task command"),
        }
    }

    #[test]
    fn load_tasks_cli_parses_optional_dir() {
        let cli = Cli::try_parse_from(["othala", "load-tasks", "--dir", ".othala/tasks"])
            .expect("parse load-tasks");

        match cli.command {
            Commands::LoadTasks { dir } => {
                assert_eq!(dir, Some(PathBuf::from(".othala/tasks")));
            }
            _ => panic!("expected load-tasks command"),
        }
    }

    #[test]
    fn validate_spec_cli_parses_path() {
        let cli = Cli::try_parse_from(["othala", "validate-spec", "specs/task.yaml"])
            .expect("parse validate-spec");

        match cli.command {
            Commands::ValidateSpec { path } => {
                assert_eq!(path, PathBuf::from("specs/task.yaml"));
            }
            _ => panic!("expected validate-spec command"),
        }
    }

    #[test]
    fn set_priority_cli_parses_task_id_and_priority() {
        let cli = Cli::try_parse_from(["othala", "set-priority", "T-42", "critical"])
            .expect("parse set-priority");

        match cli.command {
            Commands::SetPriority { id, priority } => {
                assert_eq!(id, "T-42");
                assert_eq!(priority, "critical");
            }
            _ => panic!("expected set-priority command"),
        }
    }

    #[test]
    fn replay_cli_parses_task_and_filters() {
        let cli = Cli::try_parse_from([
            "othala",
            "replay",
            "T-100",
            "--since",
            "2026-02-10T08:00:00Z",
            "--until",
            "2026-02-10T09:00:00Z",
            "--json",
        ])
        .expect("parse replay");

        match cli.command {
            Commands::Replay {
                task_id,
                since,
                until,
                json,
                all,
            } => {
                assert_eq!(task_id.as_deref(), Some("T-100"));
                assert_eq!(since.as_deref(), Some("2026-02-10T08:00:00Z"));
                assert_eq!(until.as_deref(), Some("2026-02-10T09:00:00Z"));
                assert!(json);
                assert!(!all);
            }
            _ => panic!("expected replay command"),
        }
    }

    #[test]
    fn diff_stat_flag_works() {
        let cli = Cli::try_parse_from(["othala", "diff", "T-42", "--stat"]).expect("parse diff");

        match cli.command {
            Commands::Diff { task_id, stat } => {
                assert_eq!(task_id, "T-42");
                assert!(stat);
                assert_eq!(
                    build_diff_args("main", "task/T-42", stat),
                    vec![
                        "diff".to_string(),
                        "--stat".to_string(),
                        "main...task/T-42".to_string()
                    ]
                );
            }
            _ => panic!("expected diff command"),
        }
    }

    #[test]
    fn undo_cli_parses_task_id() {
        let cli = Cli::try_parse_from(["othala", "undo", "T-42"]).expect("parse undo");

        match cli.command {
            Commands::Undo { task_id } => assert_eq!(task_id, "T-42"),
            _ => panic!("expected undo command"),
        }
    }

    #[test]
    fn redo_cli_parses_task_id() {
        let cli = Cli::try_parse_from(["othala", "redo", "T-42"]).expect("parse redo");

        match cli.command {
            Commands::Redo { task_id } => assert_eq!(task_id, "T-42"),
            _ => panic!("expected redo command"),
        }
    }

    #[test]
    fn costs_cli_parses_budget_flag() {
        let cli = Cli::try_parse_from(["othala", "costs", "--budget"]).expect("parse costs");

        match cli.command {
            Commands::Costs { task, budget } => {
                assert!(task.is_none());
                assert!(budget);
            }
            _ => panic!("expected costs command"),
        }
    }

    #[test]
    fn daemon_cli_parses_profile_flag() {
        let cli = Cli::try_parse_from(["othala", "daemon", "--once", "--profile", "prod"])
            .expect("parse daemon with profile");

        match cli.command {
            Commands::Daemon { once, profile, .. } => {
                assert!(once);
                assert_eq!(profile, Some(ConfigProfileArg::Prod));
            }
            _ => panic!("expected daemon command"),
        }
    }

    #[test]
    fn profiles_command_parses() {
        let cli = Cli::try_parse_from(["othala", "profiles"]).expect("parse profiles command");
        assert!(matches!(cli.command, Commands::Profiles));
    }

    #[test]
    fn sessions_command_parses_json_flag() {
        let cli = Cli::try_parse_from(["othala", "sessions", "--json"]).expect("parse sessions");
        match cli.command {
            Commands::Sessions { json } => assert!(json),
            _ => panic!("expected sessions command"),
        }
    }

    #[test]
    fn session_show_subcommand_parses() {
        let cli = Cli::try_parse_from(["othala", "session", "show", "S-42", "--json"])
            .expect("parse session show");
        match cli.command {
            Commands::Session { action } => match action {
                SessionAction::Show { id, json } => {
                    assert_eq!(id, "S-42");
                    assert!(json);
                }
                _ => panic!("expected session show"),
            },
            _ => panic!("expected session command"),
        }
    }

    #[test]
    fn session_fork_subcommand_parses() {
        let cli =
            Cli::try_parse_from(["othala", "session", "fork", "S-42"]).expect("parse fork");
        match cli.command {
            Commands::Session { action } => match action {
                SessionAction::Fork { id } => assert_eq!(id, "S-42"),
                _ => panic!("expected session fork"),
            },
            _ => panic!("expected session command"),
        }
    }

    #[test]
    fn session_external_form_parses_as_single_id() {
        let cli = Cli::try_parse_from(["othala", "session", "S-88"]).expect("parse external show");
        match cli.command {
            Commands::Session { action } => match action {
                SessionAction::External(args) => assert_eq!(args, vec!["S-88".to_string()]),
                _ => panic!("expected external args for session"),
            },
            _ => panic!("expected session command"),
        }
    }

    #[test]
    fn diff_retries_cli_parses_task_id() {
        let cli = Cli::try_parse_from(["othala", "diff-retries", "T-77"])
            .expect("parse diff-retries");

        match cli.command {
            Commands::DiffRetries { task_id } => {
                assert_eq!(task_id, "T-77");
            }
            _ => panic!("expected diff-retries command"),
        }
    }

    #[test]
    fn compact_cli_parses_task_and_max_lines() {
        let cli = Cli::try_parse_from(["othala", "compact", "T-88", "--max-lines", "64"])
            .expect("parse compact");

        match cli.command {
            Commands::Compact { task_id, max_lines } => {
                assert_eq!(task_id, "T-88");
                assert_eq!(max_lines, Some(64));
            }
            _ => panic!("expected compact command"),
        }
    }

    #[test]
    fn stats_command_counts_by_state() {
        let mut chatting = mk_task("T-STATS-1", TaskState::Chatting);
        chatting.preferred_model = Some(ModelKind::Claude);
        let mut merged = mk_task("T-STATS-2", TaskState::Merged);
        merged.preferred_model = Some(ModelKind::Codex);
        let stopped = mk_task("T-STATS-3", TaskState::Stopped);
        let tasks = vec![chatting, merged, stopped];

        let summary = compute_stats_summary(
            &tasks,
            vec![
                ("CHATTING".to_string(), 1),
                ("MERGED".to_string(), 1),
                ("STOPPED".to_string(), 1),
            ],
            9,
        );

        assert_eq!(summary.total_tasks, 3);
        assert_eq!(summary.tasks_by_state.get("CHATTING"), Some(&1));
        assert_eq!(summary.tasks_by_state.get("MERGED"), Some(&1));
        assert_eq!(summary.tasks_by_state.get("STOPPED"), Some(&1));
        assert_eq!(summary.tasks_by_model.get("claude"), Some(&1));
        assert_eq!(summary.tasks_by_model.get("codex"), Some(&1));
        assert_eq!(summary.tasks_by_model.get("unspecified"), Some(&1));
    }

    #[test]
    fn stats_command_computes_success_rate() {
        let merged = mk_task("T-STATS-SR-1", TaskState::Merged);
        let stopped_a = mk_task("T-STATS-SR-2", TaskState::Stopped);
        let stopped_b = mk_task("T-STATS-SR-3", TaskState::Stopped);
        let tasks = vec![merged, stopped_a, stopped_b];

        let summary = compute_stats_summary(
            &tasks,
            vec![("MERGED".to_string(), 1), ("STOPPED".to_string(), 2)],
            0,
        );

        let rate = summary.success_rate.expect("success rate exists");
        assert!((rate - 33.3333).abs() < 0.01);
    }

    #[test]
    fn gc_dry_run_does_not_delete() {
        let root = std::env::temp_dir().join(format!(
            "othala-gc-dry-run-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let event_file = root.join(".othala/events/tasks/T-GC-1.jsonl");
        let agent_dir = root.join(".othala/agent-output/T-GC-1");
        fs::create_dir_all(event_file.parent().expect("event parent")).expect("create event dir");
        fs::create_dir_all(&agent_dir).expect("create agent dir");
        fs::write(&event_file, "{}\n").expect("write event file");
        fs::write(agent_dir.join("latest.log"), "hello\n").expect("write agent log");

        let summary = gc_logs(&root, 0, true).expect("dry run gc");
        assert_eq!(summary.deleted_event_files, 1);
        assert_eq!(summary.deleted_agent_output_dirs, 1);
        assert!(event_file.exists());
        assert!(agent_dir.exists());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn gc_deletes_old_files() {
        let root = std::env::temp_dir().join(format!(
            "othala-gc-delete-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let event_file = root.join(".othala/events/tasks/T-GC-2.jsonl");
        let ignored_file = root.join(".othala/events/keep.txt");
        let agent_dir = root.join(".othala/agent-output/T-GC-2");
        fs::create_dir_all(event_file.parent().expect("event parent")).expect("create event dir");
        fs::create_dir_all(agent_dir.parent().expect("agent parent")).expect("create agent parent");
        fs::create_dir_all(&agent_dir).expect("create agent dir");
        fs::write(&event_file, "{}\n").expect("write event file");
        fs::write(&ignored_file, "keep\n").expect("write ignored file");
        fs::write(agent_dir.join("latest.log"), "hello\n").expect("write agent log");

        let summary = gc_logs(&root, 0, false).expect("gc delete");
        assert_eq!(summary.deleted_event_files, 1);
        assert_eq!(summary.deleted_agent_output_dirs, 1);
        assert!(!event_file.exists());
        assert!(ignored_file.exists());
        assert!(!agent_dir.exists());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn init_creates_directory_structure() {
        let root = std::env::temp_dir().join(format!(
            "othala-init-structure-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create temp root");

        init_project(&root, false).expect("initialize project");

        assert!(root.join(".othala").is_dir());
        assert!(root.join(".othala/context").is_dir());
        assert!(root.join(".othala/templates").is_dir());
        assert!(root.join(".othala/config.toml").is_file());
        assert!(root.join(".othala/context/MAIN.md").is_file());
        assert_eq!(
            fs::read_to_string(root.join(".othala/context/MAIN.md")).expect("read main context"),
            "# Project Context\n\nDescribe your project here.\n"
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn init_refuses_overwrite_without_force() {
        let root = std::env::temp_dir().join(format!(
            "othala-init-no-force-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create temp root");

        init_project(&root, false).expect("initialize project");
        let err = init_project(&root, false).expect_err("should reject overwrite");
        assert!(err.to_string().contains("already exists"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn template_path_uses_othala_templates_directory() {
        let root = PathBuf::from("/tmp/othala-main-tests");
        let path = template_path(&root, "starter");
        assert_eq!(path, root.join(".othala/templates/starter.json"));
    }

    #[test]
    fn save_and_load_template_roundtrip() {
        let root = std::env::temp_dir().join(format!(
            "othala-template-roundtrip-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create temp root");

        let template = TaskTemplate {
            title: "Template Task".to_string(),
            description: None,
            repo_id: "example".to_string(),
            preferred_model: Some(ModelKind::Codex),
            priority: "high".to_string(),
        };
        save_template(&root, "starter", &template).expect("save template");
        let loaded = load_template(&root, "starter").expect("load template");
        assert_eq!(loaded, template);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn list_templates_returns_json_stems() {
        let root = std::env::temp_dir().join(format!(
            "othala-template-list-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(templates_dir(&root)).expect("create template dir");
        fs::write(template_path(&root, "a"), "{}").expect("write a");
        fs::write(template_path(&root, "b"), "{}").expect("write b");
        fs::write(templates_dir(&root).join("ignore.txt"), "x").expect("write ignored");

        let names = list_templates(&root).expect("list templates");
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn export_record_contains_expected_fields() {
        let mut task = Task::new(
            TaskId::new("T-EXP-1"),
            RepoId("repo-a".to_string()),
            "Export me".to_string(),
            PathBuf::from(".orch/wt/T-EXP-1"),
        );
        task.state = TaskState::Ready;
        task.priority = TaskPriority::Critical;
        task.branch_name = Some("task/T-EXP-1".to_string());
        task.preferred_model = Some(ModelKind::Claude);

        let record = TaskExportRecord::from_task(&task, Some("task/parent".to_string()));
        assert_eq!(record.task_id, "T-EXP-1");
        assert_eq!(record.title, "Export me");
        assert_eq!(record.state, "READY");
        assert_eq!(record.priority, "critical");
        assert_eq!(record.parent_branch.as_deref(), Some("task/parent"));
    }

    #[test]
    fn import_record_to_task_applies_export_values() {
        let record = TaskExportRecord {
            task_id: "T-IMP-1".to_string(),
            title: "Imported".to_string(),
            description: Some("ignored".to_string()),
            state: "CHATTING".to_string(),
            priority: "high".to_string(),
            branch_name: Some("task/T-IMP-1".to_string()),
            parent_branch: None,
            repo_id: "repo-b".to_string(),
            preferred_model: Some(ModelKind::Gemini),
        };

        let task = import_record_to_task(record, None).expect("import record");
        assert_eq!(task.id.0, "T-IMP-1");
        assert_eq!(task.repo_id.0, "repo-b");
        assert_eq!(task.title, "Imported");
        assert_eq!(task.state, TaskState::Chatting);
        assert_eq!(task.priority, TaskPriority::High);
        assert_eq!(task.branch_name.as_deref(), Some("task/T-IMP-1"));
        assert_eq!(task.preferred_model, Some(ModelKind::Gemini));
    }

    #[test]
    fn export_import_json_roundtrip_records() {
        let records = vec![TaskExportRecord {
            task_id: "T-JSON-1".to_string(),
            title: "Roundtrip".to_string(),
            description: None,
            state: "STOPPED".to_string(),
            priority: "normal".to_string(),
            branch_name: None,
            parent_branch: None,
            repo_id: "repo-json".to_string(),
            preferred_model: Some(ModelKind::Codex),
        }];
        let json = serde_json::to_string(&records).expect("serialize");
        let decoded: Vec<TaskExportRecord> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, records);
    }

    #[test]
    fn aggregate_cost_estimates_uses_run_metrics() {
        let run = TaskRunRecord {
            run_id: "R-COST-1".to_string(),
            task_id: TaskId::new("T-COST-1"),
            repo_id: RepoId("repo-cost".to_string()),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            finished_at: None,
            stop_reason: None,
            exit_code: None,
            estimated_tokens: Some(250),
            duration_secs: Some(8.0),
        };
        let estimates = aggregate_cost_estimates(&[run]);
        assert_eq!(estimates.len(), 1);
        assert_eq!(estimates[0].input_tokens, 250);
        assert_eq!(estimates[0].duration_secs, 8.0);
    }

    #[test]
    fn aggregate_cost_estimates_skips_runs_without_token_estimate() {
        let run = TaskRunRecord {
            run_id: "R-COST-2".to_string(),
            task_id: TaskId::new("T-COST-2"),
            repo_id: RepoId("repo-cost".to_string()),
            model: ModelKind::Codex,
            started_at: Utc::now(),
            finished_at: None,
            stop_reason: None,
            exit_code: None,
            estimated_tokens: None,
            duration_secs: Some(3.0),
        };
        let estimates = aggregate_cost_estimates(&[run]);
        assert!(estimates.is_empty());
    }

    #[test]
    fn doctor_detects_missing_model() {
        let checks = doctor_model_checks(|name| name != "codex");
        let codex = checks
            .iter()
            .find(|check| check.name == "model_codex")
            .expect("codex check present");
        assert!(!codex.ok);
        assert_eq!(codex.status, DoctorStatus::Missing);
    }

    #[test]
    fn doctor_json_output_format() {
        let report = DoctorReport {
            checks: vec![DoctorCheck {
                name: "model_claude".to_string(),
                ok: true,
                status: DoctorStatus::Ok,
                detail: "claude found on PATH".to_string(),
            }],
            all_ok: true,
        };

        let json = serde_json::to_string_pretty(&report).expect("serialize doctor report");
        let value: Value = serde_json::from_str(&json).expect("parse doctor report json");

        assert!(value.get("checks").is_some());
        assert_eq!(value.get("all_ok").and_then(Value::as_bool), Some(true));
        assert_eq!(value["checks"][0]["name"], "model_claude");
        assert_eq!(value["checks"][0]["status"], "ok");
    }

    #[test]
    fn retries_formats_timeline() {
        let task_id = TaskId::new("chat-123");
        let started_1 = Utc
            .with_ymd_and_hms(2026, 2, 16, 10, 30, 0)
            .single()
            .expect("valid timestamp");
        let failed_1 = Utc
            .with_ymd_and_hms(2026, 2, 16, 10, 35, 22)
            .single()
            .expect("valid timestamp");
        let started_2 = Utc
            .with_ymd_and_hms(2026, 2, 16, 10, 35, 25)
            .single()
            .expect("valid timestamp");
        let completed_2 = Utc
            .with_ymd_and_hms(2026, 2, 16, 10, 40, 11)
            .single()
            .expect("valid timestamp");

        let events = vec![
            Event {
                id: EventId("E-R-1".to_string()),
                task_id: Some(task_id.clone()),
                repo_id: Some(RepoId("repo-1".to_string())),
                at: failed_1,
                kind: EventKind::RetryScheduled {
                    attempt: 1,
                    model: "codex".to_string(),
                    reason: "verify failed".to_string(),
                },
            },
            Event {
                id: EventId("E-R-2".to_string()),
                task_id: Some(task_id.clone()),
                repo_id: Some(RepoId("repo-1".to_string())),
                at: started_2,
                kind: EventKind::AgentSpawned {
                    model: "codex".to_string(),
                },
            },
        ];

        let runs = vec![
            TaskRunRecord {
                run_id: "RUN-1".to_string(),
                task_id: task_id.clone(),
                repo_id: RepoId("repo-1".to_string()),
                model: ModelKind::Claude,
                started_at: started_1,
                finished_at: Some(failed_1),
                stop_reason: Some("failed".to_string()),
                exit_code: Some(1),
                estimated_tokens: None,
                duration_secs: Some(322.0),
            },
            TaskRunRecord {
                run_id: "RUN-2".to_string(),
                task_id,
                repo_id: RepoId("repo-1".to_string()),
                model: ModelKind::Codex,
                started_at: started_2,
                finished_at: Some(completed_2),
                stop_reason: Some("completed".to_string()),
                exit_code: Some(0),
                estimated_tokens: None,
                duration_secs: Some(286.0),
            },
        ];

        let timeline = build_retry_timeline(&events, &runs);
        let rendered = format_retries_timeline("chat-123", &timeline);

        assert!(rendered.contains("Retry History for chat-123:"));
        assert!(rendered.contains("#1"));
        assert!(rendered.contains("claude"));
        assert!(rendered.contains("failed 10:35:22"));
        assert!(rendered.contains("reason: \"verify failed\""));
        assert!(rendered.contains("codex"));
        assert!(rendered.contains("completed 10:40:11"));
    }

    #[test]
    fn replay_formats_events_chronologically() {
        let service = mk_test_service();
        let task_id = TaskId::new("T-REPLAY-ORDER");
        let repo_id = RepoId("repo-replay-order".to_string());

        let first = Utc
            .with_ymd_and_hms(2026, 2, 10, 10, 0, 0)
            .single()
            .expect("valid first timestamp");
        let second = Utc
            .with_ymd_and_hms(2026, 2, 10, 10, 1, 0)
            .single()
            .expect("valid second timestamp");

        service
            .store
            .append_event(&Event {
                id: EventId("E-REPLAY-ORDER-2".to_string()),
                task_id: Some(task_id.clone()),
                repo_id: Some(repo_id.clone()),
                at: second,
                kind: EventKind::VerifyStarted,
            })
            .expect("append second event");
        service
            .store
            .append_event(&Event {
                id: EventId("E-REPLAY-ORDER-1".to_string()),
                task_id: Some(task_id),
                repo_id: Some(repo_id),
                at: first,
                kind: EventKind::AgentSpawned {
                    model: "claude".to_string(),
                },
            })
            .expect("append first event");

        let replayed = service
            .store
            .list_all_events(None, None)
            .expect("list all events");
        let ids: Vec<_> = replayed.iter().map(|event| event.id.0.as_str()).collect();
        assert_eq!(ids, vec!["E-REPLAY-ORDER-1", "E-REPLAY-ORDER-2"]);
    }

    #[test]
    fn replay_filters_by_task_id() {
        let service = mk_test_service();
        let task_a = mk_task("T-REPLAY-FILTER-A", TaskState::Chatting);
        let task_b = mk_task("T-REPLAY-FILTER-B", TaskState::Chatting);
        service
            .create_task(&task_a, &mk_created_event(&task_a))
            .expect("create task a");
        service
            .create_task(&task_b, &mk_created_event(&task_b))
            .expect("create task b");

        service
            .record_event(&Event {
                id: EventId("E-REPLAY-FILTER-A-1".to_string()),
                task_id: Some(task_a.id.clone()),
                repo_id: Some(task_a.repo_id.clone()),
                at: Utc::now(),
                kind: EventKind::VerifyStarted,
            })
            .expect("record task a event");
        service
            .record_event(&Event {
                id: EventId("E-REPLAY-FILTER-B-1".to_string()),
                task_id: Some(task_b.id.clone()),
                repo_id: Some(task_b.repo_id.clone()),
                at: Utc::now(),
                kind: EventKind::VerifyStarted,
            })
            .expect("record task b event");

        let events = service
            .store
            .list_events_for_task(&task_a.id.0)
            .expect("list task a events");
        assert!(events.iter().all(|event| {
            event
                .task_id
                .as_ref()
                .is_some_and(|id| id.0 == task_a.id.0)
        }));
    }

    #[test]
    fn replay_filters_by_since() {
        let task_id = TaskId::new("T-REPLAY-SINCE");
        let repo_id = RepoId("repo-replay-since".to_string());
        let first = Utc
            .with_ymd_and_hms(2026, 2, 10, 8, 0, 0)
            .single()
            .expect("valid first timestamp");
        let second = Utc
            .with_ymd_and_hms(2026, 2, 10, 9, 0, 0)
            .single()
            .expect("valid second timestamp");
        let third = Utc
            .with_ymd_and_hms(2026, 2, 10, 10, 0, 0)
            .single()
            .expect("valid third timestamp");

        let events = vec![
            Event {
                id: EventId("E-REPLAY-SINCE-1".to_string()),
                task_id: Some(task_id.clone()),
                repo_id: Some(repo_id.clone()),
                at: first,
                kind: EventKind::TaskCreated,
            },
            Event {
                id: EventId("E-REPLAY-SINCE-2".to_string()),
                task_id: Some(task_id.clone()),
                repo_id: Some(repo_id.clone()),
                at: second,
                kind: EventKind::VerifyStarted,
            },
            Event {
                id: EventId("E-REPLAY-SINCE-3".to_string()),
                task_id: Some(task_id),
                repo_id: Some(repo_id),
                at: third,
                kind: EventKind::VerifyCompleted { success: true },
            },
        ];

        let filtered = filter_events_by_time(events, Some(second), None);
        let ids: Vec<_> = filtered.iter().map(|event| event.id.0.as_str()).collect();
        assert_eq!(ids, vec!["E-REPLAY-SINCE-2", "E-REPLAY-SINCE-3"]);
    }

    #[test]
    fn format_event_handles_all_kinds() {
        let task_id = TaskId::new("T-FMT-ALL");
        let repo_id = RepoId("repo-fmt-all".to_string());
        let kinds = vec![
            EventKind::TaskCreated,
            EventKind::TaskStateChanged {
                from: "CHATTING".to_string(),
                to: "READY".to_string(),
            },
            EventKind::ParentHeadUpdated {
                parent_task_id: TaskId::new("T-PARENT"),
            },
            EventKind::RestackStarted,
            EventKind::RestackCompleted,
            EventKind::RestackConflict,
            EventKind::VerifyStarted,
            EventKind::VerifyCompleted { success: true },
            EventKind::ReadyReached,
            EventKind::SubmitStarted {
                mode: SubmitMode::Single,
            },
            EventKind::SubmitCompleted,
            EventKind::NeedsHuman {
                reason: "manual step".to_string(),
            },
            EventKind::Error {
                code: "E-1".to_string(),
                message: "boom".to_string(),
            },
            EventKind::RetryScheduled {
                attempt: 2,
                model: "claude".to_string(),
                reason: "timeout".to_string(),
            },
            EventKind::AgentSpawned {
                model: "claude".to_string(),
            },
            EventKind::AgentCompleted {
                model: "claude".to_string(),
                success: false,
                duration_secs: 14,
            },
            EventKind::CancellationRequested {
                reason: "requested by user".to_string(),
            },
            EventKind::ModelFallback {
                from_model: "claude".to_string(),
                to_model: "codex".to_string(),
                reason: "timeout".to_string(),
            },
            EventKind::ContextRegenStarted,
            EventKind::ContextRegenCompleted { success: true },
            EventKind::ConfigReloaded {
                changes: "enabled_models".to_string(),
            },
            EventKind::TaskFailed {
                reason: "max retries".to_string(),
                is_final: true,
            },
            EventKind::TestSpecValidated {
                passed: true,
                details: "ok".to_string(),
            },
            EventKind::OrchestratorDecomposed {
                sub_task_ids: vec!["T-SUB-1".to_string(), "T-SUB-2".to_string()],
            },
            EventKind::QAStarted {
                qa_type: "baseline".to_string(),
            },
            EventKind::QACompleted {
                passed: 3,
                failed: 1,
                total: 4,
            },
            EventKind::QAFailed {
                failures: vec!["test_x".to_string()],
            },
        ];

        for (idx, kind) in kinds.into_iter().enumerate() {
            let event = Event {
                id: EventId(format!("E-FMT-{idx}")),
                task_id: Some(task_id.clone()),
                repo_id: Some(repo_id.clone()),
                at: Utc::now(),
                kind,
            };
            let rendered = format_event(&event);
            assert!(rendered.contains("T-FMT-ALL"));
            assert!(rendered.contains(" | "));
        }
    }

    fn mk_test_service() -> OrchdService {
        let store = SqliteStore::open_in_memory().expect("in-memory db");
        let dir = std::env::temp_dir().join(format!(
            "othala-main-cancel-test-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        let service = OrchdService::new(
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
        service.bootstrap().expect("bootstrap");
        service
    }

    fn mk_task(id: &str, state: TaskState) -> Task {
        let mut task = Task::new(
            TaskId::new(id),
            RepoId("repo-test".to_string()),
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
    fn cancel_transitions_chatting_to_stopped() {
        let service = mk_test_service();
        let task = mk_task("T-CANCEL-1", TaskState::Chatting);
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let from_state = cancel_task(&service, &task.id, "requested by user").expect("cancel task");
        assert_eq!(from_state, TaskState::Chatting);
        let updated = service
            .task(&task.id)
            .expect("load task")
            .expect("task exists");
        assert_eq!(updated.state, TaskState::Stopped);
    }

    #[test]
    fn cancel_rejects_merged_task() {
        let service = mk_test_service();
        let task = mk_task("T-CANCEL-2", TaskState::Merged);
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let result = cancel_task(&service, &task.id, "requested by user");
        assert!(result.is_err());
        let err = result.expect_err("error").to_string();
        assert!(err.contains("cannot cancel task in state MERGED"));
    }

    #[test]
    fn cancellation_event_created() {
        let service = mk_test_service();
        let task = mk_task("T-CANCEL-3", TaskState::Chatting);
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        cancel_task(&service, &task.id, "requested by user").expect("cancel task");
        let events = service.task_events(&task.id).expect("task events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::CancellationRequested { reason } if reason == "requested by user"
            )
        }));
    }

    #[test]
    fn bulk_cancel_by_state() {
        let service = mk_test_service();
        let task_a = mk_task("T-BULK-CANCEL-A", TaskState::Chatting);
        let task_b = mk_task("T-BULK-CANCEL-B", TaskState::Chatting);
        let task_c = mk_task("T-BULK-CANCEL-C", TaskState::Ready);
        service
            .create_task(&task_a, &mk_created_event(&task_a))
            .expect("create task a");
        service
            .create_task(&task_b, &mk_created_event(&task_b))
            .expect("create task b");
        service
            .create_task(&task_c, &mk_created_event(&task_c))
            .expect("create task c");

        let ids = Vec::new();
        let summary = bulk_cancel(&service, Some("chatting"), &ids).expect("bulk cancel");
        assert_eq!(summary.processed, 2);
        assert_eq!(summary.succeeded, 2);
        assert_eq!(summary.skipped, 0);

        assert_eq!(
            service
                .task(&task_a.id)
                .expect("load a")
                .expect("a exists")
                .state,
            TaskState::Stopped
        );
        assert_eq!(
            service
                .task(&task_b.id)
                .expect("load b")
                .expect("b exists")
                .state,
            TaskState::Stopped
        );
        assert_eq!(
            service
                .task(&task_c.id)
                .expect("load c")
                .expect("c exists")
                .state,
            TaskState::Ready
        );
    }

    #[test]
    fn bulk_retry_by_ids() {
        let service = mk_test_service();
        let mut task_a = mk_task("T-BULK-RETRY-A", TaskState::Stopped);
        task_a.retry_count = 3;
        let mut task_b = mk_task("T-BULK-RETRY-B", TaskState::Ready);
        task_b.retry_count = 1;
        let mut task_c = mk_task("T-BULK-RETRY-C", TaskState::Merged);
        task_c.retry_count = 2;

        service
            .create_task(&task_a, &mk_created_event(&task_a))
            .expect("create task a");
        service
            .create_task(&task_b, &mk_created_event(&task_b))
            .expect("create task b");
        service
            .create_task(&task_c, &mk_created_event(&task_c))
            .expect("create task c");

        let ids = vec!["T-BULK-RETRY-A".to_string(), "T-BULK-RETRY-B".to_string()];
        let summary = bulk_retry(&service, None, &ids).expect("bulk retry");
        assert_eq!(summary.processed, 2);
        assert_eq!(summary.succeeded, 2);
        assert_eq!(summary.skipped, 0);

        let updated_a = service.task(&task_a.id).expect("load a").expect("a exists");
        let updated_b = service.task(&task_b.id).expect("load b").expect("b exists");
        let updated_c = service.task(&task_c.id).expect("load c").expect("c exists");
        assert_eq!(updated_a.state, TaskState::Chatting);
        assert_eq!(updated_a.retry_count, 0);
        assert_eq!(updated_b.state, TaskState::Chatting);
        assert_eq!(updated_b.retry_count, 0);
        assert_eq!(updated_c.state, TaskState::Merged);
        assert_eq!(updated_c.retry_count, 2);
    }

    #[test]
    fn bulk_set_priority_by_state() {
        let service = mk_test_service();
        let mut task_a = mk_task("T-BULK-PRIO-A", TaskState::Stopped);
        task_a.priority = TaskPriority::Low;
        let mut task_b = mk_task("T-BULK-PRIO-B", TaskState::Stopped);
        task_b.priority = TaskPriority::Normal;
        let mut task_c = mk_task("T-BULK-PRIO-C", TaskState::Ready);
        task_c.priority = TaskPriority::Low;

        service
            .create_task(&task_a, &mk_created_event(&task_a))
            .expect("create task a");
        service
            .create_task(&task_b, &mk_created_event(&task_b))
            .expect("create task b");
        service
            .create_task(&task_c, &mk_created_event(&task_c))
            .expect("create task c");

        let ids = Vec::new();
        let summary =
            bulk_set_priority(&service, TaskPriority::Critical, Some("stopped"), &ids)
                .expect("bulk set-priority");
        assert_eq!(summary.processed, 2);
        assert_eq!(summary.succeeded, 2);
        assert_eq!(summary.skipped, 0);

        assert_eq!(
            service
                .task(&task_a.id)
                .expect("load a")
                .expect("a exists")
                .priority,
            TaskPriority::Critical
        );
        assert_eq!(
            service
                .task(&task_b.id)
                .expect("load b")
                .expect("b exists")
                .priority,
            TaskPriority::Critical
        );
        assert_eq!(
            service
                .task(&task_c.id)
                .expect("load c")
                .expect("c exists")
                .priority,
            TaskPriority::Low
        );
    }

    #[test]
    fn tag_adds_label_to_task() {
        let service = mk_test_service();
        let task = mk_task("T-TAG-1", TaskState::Chatting);
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        add_task_label(&service, &task.id, "bug").expect("tag task");
        let updated = service
            .task(&task.id)
            .expect("load task")
            .expect("task exists");
        assert_eq!(updated.labels, vec!["bug".to_string()]);
    }

    #[test]
    fn tag_deduplicates_labels() {
        let service = mk_test_service();
        let task = mk_task("T-TAG-2", TaskState::Chatting);
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        add_task_label(&service, &task.id, "urgent").expect("first tag");
        add_task_label(&service, &task.id, "urgent").expect("second tag");
        let updated = service
            .task(&task.id)
            .expect("load task")
            .expect("task exists");
        assert_eq!(updated.labels, vec!["urgent".to_string()]);
    }

    #[test]
    fn untag_removes_label() {
        let service = mk_test_service();
        let task = mk_task("T-TAG-3", TaskState::Chatting);
        service
            .create_task(&task, &mk_created_event(&task))
            .expect("create task");

        add_task_label(&service, &task.id, "feature").expect("tag task");
        remove_task_label(&service, &task.id, "feature").expect("untag task");
        let updated = service
            .task(&task.id)
            .expect("load task")
            .expect("task exists");
        assert!(updated.labels.is_empty());
    }

    #[test]
    fn search_by_title() {
        let service = mk_test_service();
        let task_a = mk_task("T-SEARCH-TITLE-A", TaskState::Chatting);
        let mut task_b = mk_task("T-SEARCH-TITLE-B", TaskState::Chatting);
        task_b.title = "Fix urgent regression".to_string();
        service
            .create_task(&task_a, &mk_created_event(&task_a))
            .expect("create task a");
        service
            .create_task(&task_b, &mk_created_event(&task_b))
            .expect("create task b");

        let result = service
            .store
            .search_tasks("regression", None, None)
            .expect("search tasks");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id.0, task_b.id.0);
    }

    #[test]
    fn search_by_label() {
        let service = mk_test_service();
        let task_a = mk_task("T-SEARCH-LABEL-A", TaskState::Chatting);
        let task_b = mk_task("T-SEARCH-LABEL-B", TaskState::Chatting);
        service
            .create_task(&task_a, &mk_created_event(&task_a))
            .expect("create task a");
        service
            .create_task(&task_b, &mk_created_event(&task_b))
            .expect("create task b");

        add_task_label(&service, &task_a.id, "bug").expect("tag task a");
        add_task_label(&service, &task_b.id, "feature").expect("tag task b");

        let result = service
            .store
            .search_tasks("bug", Some("bug"), None)
            .expect("search tasks");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id.0, task_a.id.0);
    }

    #[test]
    fn search_by_state() {
        let service = mk_test_service();
        let task_a = mk_task("T-SEARCH-STATE-A", TaskState::Ready);
        let task_b = mk_task("T-SEARCH-STATE-B", TaskState::Stopped);
        service
            .create_task(&task_a, &mk_created_event(&task_a))
            .expect("create task a");
        service
            .create_task(&task_b, &mk_created_event(&task_b))
            .expect("create task b");

        let result = service
            .store
            .search_tasks("T-SEARCH-STATE", None, Some("ready"))
            .expect("search tasks");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id.0, task_a.id.0);
    }

    #[test]
    fn archive_cli_parses_flags() {
        let cli = Cli::try_parse_from([
            "othala",
            "archive",
            "--older-than-days",
            "14",
            "--json",
        ])
        .expect("parse archive");

        match cli.command {
            Commands::Archive {
                older_than_days,
                json,
            } => {
                assert_eq!(older_than_days, 14);
                assert!(json);
            }
            _ => panic!("expected archive command"),
        }
    }

    #[test]
    fn archive_moves_old_tasks() {
        let service = mk_test_service();
        let mut old_task = mk_task("T-ARCH-MOVE-1", TaskState::Merged);
        old_task.updated_at = Utc::now() - Duration::days(30);
        service
            .create_task(&old_task, &mk_created_event(&old_task))
            .expect("create task");

        let archived = archive_old_tasks(&service, 7).expect("archive should succeed");
        assert_eq!(archived, 1);
        assert!(service.task(&old_task.id).expect("load task").is_none());

        let archived_rows = service.store.list_archived().expect("list archived");
        assert_eq!(archived_rows.len(), 1);
        assert_eq!(archived_rows[0].task_id, old_task.id);
    }

    #[test]
    fn archive_skips_recent_tasks() {
        let service = mk_test_service();
        let mut recent_task = mk_task("T-ARCH-SKIP-1", TaskState::Stopped);
        recent_task.updated_at = Utc::now() - Duration::days(1);
        service
            .create_task(&recent_task, &mk_created_event(&recent_task))
            .expect("create task");

        let archived = archive_old_tasks(&service, 7).expect("archive should succeed");
        assert_eq!(archived, 0);
        assert!(service
            .task(&recent_task.id)
            .expect("load task")
            .is_some());
        assert!(service
            .store
            .list_archived()
            .expect("list archived")
            .is_empty());
    }

    #[test]
    fn watch_formats_output_with_prefix() {
        let line = format_watch_line("task-1", "\x1b[31m", "hello world");
        assert_eq!(line, "[\x1b[31mtask-1\x1b[0m] hello world");
    }

    #[test]
    fn watch_and_cancel_cli_parse() {
        let watch = Cli::try_parse_from(["othala", "watch", "--task", "task-1", "-n", "5"])
            .expect("parse watch");
        match watch.command {
            Commands::Watch { task, lines } => {
                assert_eq!(task.as_deref(), Some("task-1"));
                assert_eq!(lines, 5);
            }
            _ => panic!("expected watch command"),
        }

        let cancel = Cli::try_parse_from(["othala", "cancel", "task-2"]).expect("parse cancel");
        match cancel.command {
            Commands::Cancel { id } => assert_eq!(id, "task-2"),
            _ => panic!("expected cancel command"),
        }
    }

    #[test]
    fn skills_command_parses() {
        let cli = Cli::try_parse_from(["othala", "skills"]).expect("parse skills");
        assert!(matches!(cli.command, Commands::Skills));
    }

    #[test]
    fn mcp_command_parses() {
        let cli = Cli::try_parse_from(["othala", "mcp"]).expect("parse mcp");
        assert!(matches!(cli.command, Commands::Mcp));
    }

    #[test]
    fn skill_command_parses_name() {
        let cli = Cli::try_parse_from(["othala", "skill", "playwright"]).expect("parse skill");
        match cli.command {
            Commands::Skill { name } => assert_eq!(name, "playwright"),
            _ => panic!("expected skill command"),
        }
    }
}
