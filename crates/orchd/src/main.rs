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
    GraphiteOrgConfig, ModelsConfig, MovePolicy, OrgConfig, UiConfig,
};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{EventId, ModelKind, RepoId, SubmitMode, Task, TaskId};
use orchd::supervisor::AgentSupervisor;
use orchd::{provision_chat_workspace_on_base, OrchdService, Scheduler, SchedulerConfig};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Parser)]
#[command(name = "othala")]
#[command(about = "AI coding orchestrator - MVP")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
            anyhow::bail!(
                "unknown model '{token}'. valid values: claude,codex,gemini"
            );
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
            .map(|out| out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true")
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
            let suffix = if check.critical { "" } else { " (non-critical)" };
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

    let service = OrchdService::open(&db_path, &event_log_path, scheduler)?;

    match cli.command {
        Commands::Chat { action } => match action {
            ChatAction::New {
                repo,
                title,
                model,
                json,
            } => {
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

            if !skip_context_gen {
                if let Err(e) =
                    run_context_gen_with_status(&repo_root, &template_dir, ModelKind::Claude)
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
            let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
            let mut daemon_state = orchd::daemon_loop::DaemonState::new();

            let verify_cmd = verify_command
                .unwrap_or_else(|| "cargo check && cargo test --workspace".to_string());

            let daemon_config = orchd::daemon_loop::DaemonConfig {
                repo_root,
                template_dir,
                enabled_models: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
                context_config: orchd::context_graph::ContextLoadConfig::default(),
                verify_command: Some(verify_cmd),
                context_gen_config,
                skip_qa,
                skip_context_regen: skip_context_gen,
            };

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
                        prev_states.insert(task.id.0.clone(), task.state.clone());
                    }
                }
                if !tasks.is_empty() {
                    let chatting = tasks.iter().filter(|t| t.state == TaskState::Chatting).count();
                    let ready = tasks.iter().filter(|t| t.state == TaskState::Ready).count();
                    let submitting = tasks.iter().filter(|t| t.state == TaskState::Submitting).count();
                    let awaiting = tasks.iter().filter(|t| t.state == TaskState::AwaitingMerge).count();
                    let merged = tasks.iter().filter(|t| t.state == TaskState::Merged).count();
                    let stopped = tasks.iter().filter(|t| t.state == TaskState::Stopped).count();
                    let mut status = format!(
                        "[{}] {} chatting, {} ready, {} submitting, {} awaiting, {} merged",
                        chrono::Local::now().format("%H:%M:%S"),
                        chatting, ready, submitting, awaiting, merged
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

                std::thread::sleep(std::time::Duration::from_secs(2));
            }

            let final_tasks = service.list_tasks()?;
            let json = serde_json::to_string_pretty(&final_tasks)
                .unwrap_or_else(|_| "[]".to_string());
            println!("{json}");
        }
        Commands::SelfTest { json } => {
            let critical_ok = run_self_test(json);
            std::process::exit(if critical_ok { 0 } else { 1 });
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
            } else {
                if display_events.is_empty() {
                    println!("No events found.");
                } else {
                    for event in &display_events {
                        let ts = event.at.format("%Y-%m-%d %H:%M:%S");
                        let task_label = event
                            .task_id
                            .as_ref()
                            .map(|t| t.0.as_str())
                            .unwrap_or("-");
                        let kind_str = format_event_kind(&event.kind);
                        println!("{ts}  {task_label:<24} {kind_str}");
                    }
                }
            }
        }
    }

    Ok(())
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
        _ => "event".to_string(),
    }
}
