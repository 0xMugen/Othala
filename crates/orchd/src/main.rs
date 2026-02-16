//! Othala daemon - MVP version.
//!
//! Simplified CLI for managing AI coding sessions that auto-submit to Graphite.

use chrono::Utc;
use clap::{Parser, Subcommand};
use orch_agents::setup::{
    probe_models, summarize_setup, validate_setup_selection, ModelSetupSelection, SetupProbeConfig,
};
use orch_core::config::{
    apply_setup_selection_to_org_config, load_org_config, save_org_config, ConcurrencyConfig,
    DaemonOrgConfig, GraphiteOrgConfig, ModelsConfig, MovePolicy, NotificationConfig, OrgConfig,
    UiConfig,
};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{EventId, ModelKind, RepoId, SubmitMode, Task, TaskId, TaskPriority};
use orch_notify::{NotificationDispatcher, NotificationSink, StdoutSink, WebhookSink};
use orchd::supervisor::AgentSupervisor;
use orchd::{
    provision_chat_workspace_on_base, AgentCostEstimate, OrchdService, Scheduler, SchedulerConfig,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::io::ErrorKind;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

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
    SetPriority {
        /// Chat/task ID
        id: String,
        priority: String,
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
    },
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
    /// Show aggregate task and agent statistics
    Stats,
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
    let default_model = enabled_models.first().copied();
    OrgConfig {
        models: ModelsConfig {
            enabled: enabled_models,
            default: default_model,
        },
        concurrency: ConcurrencyConfig {
            per_repo: 10,
            claude: 10,
            codex: 10,
            gemini: 10,
        },
        graphite: GraphiteOrgConfig {
            auto_submit: true,
            submit_mode_default: SubmitMode::Single,
            allow_move: MovePolicy::Manual,
        },
        ui: UiConfig {
            web_bind: "127.0.0.1:9842".to_string(),
        },
        notifications: NotificationConfig::default(),
        daemon: DaemonOrgConfig::default(),
    }
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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let db_path = PathBuf::from(".orch/state.sqlite");
    let event_log_path = PathBuf::from(".orch/events");

    // Ensure directories exist
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
        Commands::SetPriority { id, priority } => {
            let task_id = TaskId::new(&id);
            let parsed = parse_task_priority(&priority)?;
            set_priority(&service, &task_id, parsed)?;
            println!("Updated priority: {} -> {}", task_id.0, parsed);
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
        Commands::Daemon {
            timeout,
            exit_on_idle,
            skip_context_gen,
            verify_command,
            skip_qa,
            once,
        } => {
            print_banner();

            let repo_root = std::env::current_dir()?;
            let template_dir = PathBuf::from("templates/prompts");

            let config_path = PathBuf::from(".othala/config.toml");
            let (enabled_models, default_model, notification_dispatcher, daemon_org_config) =
                if config_path.exists() {
                let org_config = load_org_config(&config_path)?;
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
                (
                    vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
                    ModelKind::Claude,
                    None,
                    DaemonOrgConfig::default(),
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
        Commands::Stats => {
            let tasks = service.list_tasks()?;
            let total = tasks.len();
            let by_state = |s: TaskState| tasks.iter().filter(|t| t.state == s).count();

            println!("Othala Statistics");
            println!("=================");
            println!();
            println!("Tasks:");
            println!("  Total:          {total}");
            println!("  Chatting:       {}", by_state(TaskState::Chatting));
            println!("  Ready:          {}", by_state(TaskState::Ready));
            println!("  Submitting:     {}", by_state(TaskState::Submitting));
            println!("  Restacking:     {}", by_state(TaskState::Restacking));
            println!("  Awaiting Merge: {}", by_state(TaskState::AwaitingMerge));
            println!("  Merged:         {}", by_state(TaskState::Merged));
            println!("  Stopped:        {}", by_state(TaskState::Stopped));
            println!();

            let model_counts = service.runs_by_model()?;
            if !model_counts.is_empty() {
                println!("Agent Runs by Model:");
                for (model, count) in &model_counts {
                    println!("  {model:<10} {count}");
                }
                println!();
            }

            let events = service.global_events()?;
            println!("Events:           {}", events.len());
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
        Commands::Costs { task } => {
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
}
