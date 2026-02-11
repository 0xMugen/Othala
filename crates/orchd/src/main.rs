//! Othala daemon - MVP version.
//!
//! Simplified CLI for managing AI coding sessions that auto-submit to Graphite.

use chrono::Utc;
use clap::{Parser, Subcommand};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{EventId, ModelKind, RepoId, Task, TaskId};
use orchd::supervisor::AgentSupervisor;
use orchd::{provision_chat_workspace, OrchdService, Scheduler, SchedulerConfig};
use std::collections::HashMap;
use std::path::{Path, PathBuf};


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
    List,
    /// Show chat status
    Status {
        /// Chat/task ID
        id: String,
    },
    /// Delete a chat
    Delete {
        /// Chat/task ID
        id: String,
    },
    /// Run the daemon (orchestration loop)
    Daemon,
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
    },
    /// List all chats
    List,
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
        let result = orchd::context_gen::ensure_context_exists_blocking(
            &repo,
            &tmpl,
            model,
            move |line| { let _ = ptx.send(line.to_string()); },
        );
        let _ = result_tx.send(result);
    });

    let spinner_frames = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280f}'];
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

fn print_task_list(tasks: &[Task]) {
    if tasks.is_empty() {
        println!("No chats found.");
    } else {
        println!("{:<20} {:<10} {:<40}", "ID", "STATE", "TITLE");
        println!("{}", "-".repeat(70));
        for task in tasks {
            println!(
                "{:<20} {:<10} {:<40}",
                task.id.0,
                format!("{:?}", task.state),
                task.title
            );
        }
    }
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
            ChatAction::New { repo, title, model } => {
                let task_id = format!("chat-{}", Utc::now().timestamp_millis());
                let task_id = TaskId::new(&task_id);
                let start_path = std::env::current_dir()?;
                let workspace = provision_chat_workspace(&start_path, &task_id)?;

                let mut task = Task::new(
                    task_id.clone(),
                    RepoId(repo.clone()),
                    title.clone(),
                    workspace.worktree_path.clone(),
                );
                task.branch_name = Some(workspace.branch_name.clone());

                let model_kind = match model.to_lowercase().as_str() {
                    "claude" => ModelKind::Claude,
                    "codex" => ModelKind::Codex,
                    "gemini" => ModelKind::Gemini,
                    _ => ModelKind::Claude,
                };
                task.preferred_model = Some(model_kind);

                let event = Event {
                    id: EventId(format!("E-CREATE-{}", task_id.0)),
                    task_id: Some(task.id.clone()),
                    repo_id: Some(task.repo_id.clone()),
                    at: Utc::now(),
                    kind: EventKind::TaskCreated,
                };

                service.create_task(&task, &event)?;
                println!(
                    "Created chat: {} - {} [{} @ {}]",
                    task_id.0,
                    title,
                    workspace.branch_name,
                    workspace.worktree_path.display()
                );
            }
            ChatAction::List => {
                print_task_list(&service.list_tasks()?);
            }
        },
        Commands::List => {
            print_task_list(&service.list_tasks()?);
        }
        Commands::Status { id } => {
            let task_id = TaskId::new(&id);
            match service.task(&task_id)? {
                Some(task) => {
                    println!("Chat: {}", task.id.0);
                    println!("Title: {}", task.title);
                    println!("Repo: {}", task.repo_id.0);
                    println!("State: {:?}", task.state);
                    if let Some(model) = task.preferred_model {
                        println!("Model: {:?}", model);
                    }
                    if let Some(pr) = task.pr {
                        println!("PR: {} ({})", pr.number, pr.url);
                    }
                    if let Some(branch) = &task.branch_name {
                        println!("Branch: {}", branch);
                    }
                    println!("Worktree: {}", task.worktree_path.display());
                    println!("Created: {}", task.created_at);
                    println!("Updated: {}", task.updated_at);
                }
                None => {
                    println!("Chat not found: {}", id);
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
        Commands::Daemon => {
            print_banner();

            let repo_root = std::env::current_dir()?;
            let template_dir = PathBuf::from("templates/prompts");

            // Ensure context files exist before entering the loop.
            if let Err(e) = run_context_gen_with_status(
                &repo_root,
                &template_dir,
                ModelKind::Claude,
            ) {
                eprintln!("[daemon] Context generation failed (non-fatal): {e}");
            }
            eprintln!();

            let context_gen_config = orchd::context_gen::ContextGenConfig::default();

            let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
            let mut daemon_state = orchd::daemon_loop::DaemonState::new();
            let daemon_config = orchd::daemon_loop::DaemonConfig {
                repo_root,
                template_dir,
                enabled_models: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
                context_config: orchd::context_graph::ContextLoadConfig::default(),
                verify_command: Some("cargo check && cargo test --workspace".to_string()),
                context_gen_config,
            };

            loop {
                orchd::daemon_loop::run_tick(
                    &service,
                    &mut supervisor,
                    &mut daemon_state,
                    &daemon_config,
                );

                // Print status summary.
                let tasks = service.list_tasks()?;
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
                    println!(
                        "[{}] {} chatting, {} ready, {} submitting, {} awaiting, {} merged",
                        chrono::Local::now().format("%H:%M:%S"),
                        chatting,
                        ready,
                        submitting,
                        awaiting,
                        merged
                    );
                }

                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }

    Ok(())
}
