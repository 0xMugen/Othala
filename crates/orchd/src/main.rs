//! Othala daemon - MVP version.
//!
//! Simplified CLI for managing AI coding sessions that auto-submit to Graphite.

use chrono::Utc;
use clap::{Parser, Subcommand};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{EventId, ModelKind, RepoId, Task, TaskId};
use orchd::{OrchdService, Scheduler, SchedulerConfig};
use std::collections::HashMap;
use std::path::PathBuf;

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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let db_path = PathBuf::from(".othala/db.sqlite");
    let event_log_path = PathBuf::from(".othala/events");
    
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
                let worktree_path = PathBuf::from(format!(".othala/wt/{}", task_id));
                
                let mut task = Task::new(
                    TaskId::new(&task_id),
                    RepoId(repo.clone()),
                    title.clone(),
                    worktree_path,
                );
                
                let model_kind = match model.to_lowercase().as_str() {
                    "claude" => ModelKind::Claude,
                    "codex" => ModelKind::Codex,
                    "gemini" => ModelKind::Gemini,
                    _ => ModelKind::Claude,
                };
                task.preferred_model = Some(model_kind);

                let event = Event {
                    id: EventId(format!("E-CREATE-{}", task_id)),
                    task_id: Some(task.id.clone()),
                    repo_id: Some(task.repo_id.clone()),
                    at: Utc::now(),
                    kind: EventKind::TaskCreated,
                };

                service.create_task(&task, &event)?;
                println!("Created chat: {} - {}", task_id, title);
            }
            ChatAction::List => {
                let tasks = service.list_tasks()?;
                if tasks.is_empty() {
                    println!("No chats found.");
                } else {
                    println!("{:<20} {:<10} {:<40}", "ID", "STATE", "TITLE");
                    println!("{}", "-".repeat(70));
                    for task in tasks {
                        println!("{:<20} {:<10} {:<40}", task.id.0, format!("{:?}", task.state), task.title);
                    }
                }
            }
        },
        Commands::List => {
            let tasks = service.list_tasks()?;
            if tasks.is_empty() {
                println!("No chats found.");
            } else {
                println!("{:<20} {:<10} {:<40}", "ID", "STATE", "TITLE");
                println!("{}", "-".repeat(70));
                for task in tasks {
                    println!("{:<20} {:<10} {:<40}", task.id.0, format!("{:?}", task.state), task.title);
                }
            }
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
                    println!("Created: {}", task.created_at);
                    println!("Updated: {}", task.updated_at);
                }
                None => {
                    println!("Chat not found: {}", id);
                }
            }
        }
        Commands::Daemon => {
            println!("Othala daemon starting...");
            println!("MVP daemon mode - monitoring for chat state changes");
            
            // Simple daemon loop
            loop {
                let tasks = service.list_tasks()?;
                let chatting = tasks.iter().filter(|t| t.state == TaskState::Chatting).count();
                let ready = tasks.iter().filter(|t| t.state == TaskState::Ready).count();
                let submitting = tasks.iter().filter(|t| t.state == TaskState::Submitting).count();
                let awaiting = tasks.iter().filter(|t| t.state == TaskState::AwaitingMerge).count();
                let merged = tasks.iter().filter(|t| t.state == TaskState::Merged).count();

                if tasks.is_empty() {
                    // Sleep quietly when no tasks
                } else {
                    println!(
                        "[{}] Chats: {} chatting, {} ready, {} submitting, {} awaiting merge, {} merged",
                        chrono::Local::now().format("%H:%M:%S"),
                        chatting,
                        ready,
                        submitting,
                        awaiting,
                        merged
                    );
                }

                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        }
    }

    Ok(())
}
