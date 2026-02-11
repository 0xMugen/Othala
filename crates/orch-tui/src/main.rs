use chrono::Utc;
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{EventId, ModelKind, RepoId, Task, TaskId};
use orch_tui::{run_tui_with_hook, AgentPaneStatus, QATestDisplay, TuiApp, TuiEvent, UiAction};
use orchd::qa_agent;
use orchd::supervisor::AgentSupervisor;
use orchd::{OrchdService, Scheduler, SchedulerConfig};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_TICK_MS: u64 = 250;
const DEFAULT_SQLITE_PATH: &str = ".orch/state.sqlite";
const DEFAULT_EVENT_LOG_PATH: &str = ".orch/events";
const CHAT_LOG_DIR: &str = ".orch/chat";

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliArgs {
    tick_ms: u64,
    sqlite_path: PathBuf,
    event_log_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("{0}")]
    Args(String),
    #[error(transparent)]
    Tui(#[from] orch_tui::TuiError),
    #[error(transparent)]
    Any(#[from] anyhow::Error),
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

fn run_context_gen_with_status(repo_root: &Path, template_dir: &Path, model: ModelKind) {
    use orchd::context_gen::{check_context_startup, parse_progress_line, ContextStartupStatus};

    match check_context_startup(repo_root) {
        ContextStartupStatus::UpToDate => {
            eprintln!("  \x1b[32mContext up to date \u{2713}\x1b[0m");
            return;
        }
        ContextStartupStatus::Stale => {
            eprintln!("  \x1b[33mContext stale — will regenerate in background\x1b[0m");
            return;
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
                match result {
                    Ok(()) => eprintln!("  \x1b[32mContext generated \u{2713}\x1b[0m"),
                    Err(e) => eprintln!("  \x1b[31mContext generation failed: {e}\x1b[0m"),
                }
                return;
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
                eprintln!("  \x1b[31mContext generation failed\x1b[0m");
                return;
            }
        }
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("orch-tui failed: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), MainError> {
    let mut argv = std::env::args();
    let program = argv.next().unwrap_or_else(|| "orch-tui".to_string());
    let args = parse_cli_args(argv.collect::<Vec<_>>(), &program)?;

    // Ensure directories exist.
    if let Some(parent) = args.sqlite_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::create_dir_all(&args.event_log_path).ok();
    let chat_log_dir = PathBuf::from(CHAT_LOG_DIR);
    std::fs::create_dir_all(&chat_log_dir).ok();

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

    let service = OrchdService::open(&args.sqlite_path, &args.event_log_path, scheduler)
        .map_err(|e| MainError::Any(e.into()))?;

    // Show banner and handle context gen before TUI takes over the terminal.
    print_banner();
    {
        let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let template_dir = PathBuf::from("templates/prompts");
        run_context_gen_with_status(&repo_root, &template_dir, ModelKind::Claude);
    }
    eprintln!();

    let tasks = service.list_tasks().unwrap_or_default();
    let mut app = TuiApp::from_tasks(&tasks);

    // Restore chat history from log files.
    for task in &tasks {
        let lines = load_chat_log(&chat_log_dir, &task.id);
        if !lines.is_empty() {
            let model = task.preferred_model.unwrap_or(ModelKind::Claude);
            let instance_id = format!("agent-{}", task.id.0);
            app.apply_event(TuiEvent::AgentPaneOutput {
                instance_id: instance_id.clone(),
                task_id: task.id.clone(),
                model,
                lines,
            });
            // Mark pane as exited so the user sees history, not "running".
            app.apply_event(TuiEvent::AgentPaneStatusChanged {
                instance_id,
                status: AgentPaneStatus::Exited,
            });
        }
    }

    app.state.status_line = format!(
        "orch-tui started tick_ms={} tasks={}",
        args.tick_ms,
        tasks.len()
    );

    let mut supervisor = AgentSupervisor::new(ModelKind::Claude);
    let mut tick_counter: u32 = 0;

    run_tui_with_hook(&mut app, Duration::from_millis(args.tick_ms), |app| {
        // Process queued actions from the UI.
        let actions = app.drain_actions();
        for queued in actions {
            match queued.action {
                UiAction::CreateTask => {
                    if let Some(prompt) = &queued.prompt {
                        let model = queued.model.unwrap_or(ModelKind::Claude);
                        let task_id =
                            TaskId::new(format!("chat-{}", Utc::now().timestamp_millis()));
                        let start_path =
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

                        // Try workspace provisioning; fall back to cwd on failure.
                        let (worktree_path, branch_name) =
                            match orchd::provision_chat_workspace(&start_path, &task_id) {
                                Ok(ws) => (ws.worktree_path, Some(ws.branch_name)),
                                Err(_) => (start_path.clone(), None),
                            };

                        let mut task = Task::new(
                            task_id.clone(),
                            RepoId("default".to_string()),
                            prompt.clone(),
                            worktree_path,
                        );
                        task.preferred_model = Some(model);
                        task.branch_name = branch_name.clone();
                        let event = Event {
                            id: EventId(format!("E-CREATE-{}", task_id.0)),
                            task_id: Some(task.id.clone()),
                            repo_id: Some(task.repo_id.clone()),
                            at: Utc::now(),
                            kind: EventKind::TaskCreated,
                        };
                        match service.create_task(&task, &event) {
                            Ok(()) => {
                                let detail = branch_name
                                    .as_deref()
                                    .unwrap_or("(no branch)");
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!(
                                        "created chat {} on {}",
                                        task_id.0, detail
                                    ),
                                });
                                // Immediately refresh task list so it appears.
                                if let Ok(tasks) = service.list_tasks() {
                                    app.apply_event(TuiEvent::TasksReplaced { tasks });
                                }
                            }
                            Err(e) => {
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!("create failed: {e}"),
                                });
                            }
                        }
                    }
                }
                UiAction::DeleteTask => {
                    if let Some(task_id) = &queued.task_id {
                        supervisor.stop(task_id);
                        // Remove chat log file.
                        let _ = std::fs::remove_file(chat_log_path(&chat_log_dir, task_id));
                        match service.delete_task(task_id) {
                            Ok(true) => {
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!("deleted {}", task_id.0),
                                });
                            }
                            Ok(false) => {
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!("not found: {}", task_id.0),
                                });
                            }
                            Err(e) => {
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!("delete failed: {e}"),
                                });
                            }
                        }
                    }
                }
                UiAction::StartAgent => {
                    if let Some(task_id) = &queued.task_id {
                        if let Ok(Some(task)) = service.task(task_id) {
                            let model = task.preferred_model.unwrap_or(ModelKind::Claude);
                            let instance_id = format!("agent-{}", task_id.0);
                            match supervisor.spawn_agent(
                                &task.id,
                                &task.repo_id,
                                &task.worktree_path,
                                &task.title,
                                Some(model),
                            ) {
                                Ok(()) => {
                                    app.apply_event(TuiEvent::AgentPaneOutput {
                                        instance_id: instance_id.clone(),
                                        task_id: task_id.clone(),
                                        model,
                                        lines: vec![],
                                    });
                                    app.apply_event(TuiEvent::AgentPaneStatusChanged {
                                        instance_id,
                                        status: AgentPaneStatus::Starting,
                                    });
                                    app.apply_event(TuiEvent::StatusLine {
                                        message: format!(
                                            "started {:?} agent for {}",
                                            model, task_id.0
                                        ),
                                    });
                                }
                                Err(e) => {
                                    app.apply_event(TuiEvent::StatusLine {
                                        message: format!("spawn failed: {e}"),
                                    });
                                }
                            }
                        }
                    }
                }
                UiAction::StopAgent => {
                    if let Some(task_id) = &queued.task_id {
                        supervisor.stop(task_id);
                        app.apply_event(TuiEvent::StatusLine {
                            message: format!("stopped agent for {}", task_id.0),
                        });
                    }
                }
                UiAction::SendChatMessage => {
                    if let (Some(task_id), Some(message)) = (&queued.task_id, &queued.prompt) {
                        // Auto-spawn interactive session if none exists.
                        if !supervisor.has_session(task_id) {
                            if let Ok(Some(task)) = service.task(task_id) {
                                let model = task.preferred_model.unwrap_or(ModelKind::Claude);
                                if let Err(e) = supervisor.spawn_interactive(
                                    &task.id,
                                    &task.repo_id,
                                    &task.worktree_path,
                                    message,
                                    Some(model),
                                ) {
                                    app.apply_event(TuiEvent::StatusLine {
                                        message: format!("interactive spawn failed: {e}"),
                                    });
                                } else {
                                    // Echo user message into the pane and log.
                                    let user_line = format!("> {message}");
                                    append_chat_log(&chat_log_dir, task_id, &[user_line.clone()]);
                                    let instance_id = format!("agent-{}", task_id.0);
                                    app.apply_event(TuiEvent::AgentPaneOutput {
                                        instance_id,
                                        task_id: task_id.clone(),
                                        model,
                                        lines: vec![user_line],
                                    });
                                    app.apply_event(TuiEvent::StatusLine {
                                        message: format!(
                                            "started interactive {:?} agent for {}",
                                            model, task_id.0
                                        ),
                                    });
                                }
                            }
                        } else {
                            // Session exists — send the message.
                            match supervisor.send_input(task_id, message) {
                                Ok(()) => {
                                    let user_line = format!("> {message}");
                                    append_chat_log(&chat_log_dir, task_id, &[user_line.clone()]);
                                    let instance_id = format!("agent-{}", task_id.0);
                                    let model = service
                                        .task(task_id)
                                        .ok()
                                        .flatten()
                                        .and_then(|t| t.preferred_model)
                                        .unwrap_or(ModelKind::Claude);
                                    app.apply_event(TuiEvent::AgentPaneOutput {
                                        instance_id,
                                        task_id: task_id.clone(),
                                        model,
                                        lines: vec![user_line],
                                    });
                                }
                                Err(e) => {
                                    app.apply_event(TuiEvent::StatusLine {
                                        message: format!("send failed: {e}"),
                                    });
                                }
                            }
                        }
                    }
                }
                UiAction::ApproveTask => {
                    if let Some(task_id) = &queued.task_id {
                        let now = Utc::now();
                        let event_id = EventId(format!("E-READY-{}", task_id.0));
                        match service.mark_ready(task_id, event_id, now) {
                            Ok(_) => {
                                let instance_id = format!("agent-{}", task_id.0);
                                app.apply_event(TuiEvent::AgentPaneStatusChanged {
                                    instance_id,
                                    status: AgentPaneStatus::Exited,
                                });
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!("approved {} -> Ready", task_id.0),
                                });
                            }
                            Err(e) => {
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!("approve failed: {e}"),
                                });
                            }
                        }
                    }
                }
                _ => {
                    app.apply_event(TuiEvent::StatusLine {
                        message: format!("action not yet implemented: {:?}", queued.action),
                    });
                }
            }
        }

        // Poll supervisor for output and completions.
        let result = supervisor.poll();
        for chunk in result.output {
            append_chat_log(&chat_log_dir, &chunk.task_id, &chunk.lines);
            let instance_id = format!("agent-{}", chunk.task_id.0);
            app.apply_event(TuiEvent::AgentPaneOutput {
                instance_id,
                task_id: chunk.task_id,
                model: chunk.model,
                lines: chunk.lines,
            });
        }
        for outcome in &result.completed {
            let instance_id = format!("agent-{}", outcome.task_id.0);
            let now = Utc::now();
            if outcome.patch_ready || outcome.success {
                let event = Event {
                    id: EventId(format!("E-DONE-{}", outcome.task_id.0)),
                    task_id: Some(outcome.task_id.clone()),
                    repo_id: None,
                    at: now,
                    kind: EventKind::NeedsHuman {
                        reason: "agent completed".to_string(),
                    },
                };
                let _ = service.record_event(&event);
                app.apply_event(TuiEvent::AgentPaneStatusChanged {
                    instance_id: instance_id.clone(),
                    status: AgentPaneStatus::Exited,
                });
                let task_label = &outcome.task_id.0;
                app.apply_event(TuiEvent::StatusLine {
                    message: format!(
                        "{task_label} done -- press 'a' to approve or 'i' to chat"
                    ),
                });
            } else if outcome.needs_human {
                let event = Event {
                    id: EventId(format!("E-HUMAN-{}", outcome.task_id.0)),
                    task_id: Some(outcome.task_id.clone()),
                    repo_id: None,
                    at: now,
                    kind: EventKind::NeedsHuman {
                        reason: "Agent requested human assistance".to_string(),
                    },
                };
                let _ = service.record_event(&event);
                app.apply_event(TuiEvent::AgentPaneStatusChanged {
                    instance_id,
                    status: AgentPaneStatus::Waiting,
                });
            } else {
                app.apply_event(TuiEvent::AgentPaneStatusChanged {
                    instance_id,
                    status: AgentPaneStatus::Failed,
                });
            }
        }

        // Auto-spawn agents for Chatting tasks without a running session.
        // Skip tasks whose pane is in a terminal state (Waiting/Failed/Exited).
        if let Ok(chatting) = service.list_tasks_by_state(TaskState::Chatting) {
            for task in &chatting {
                let pane_stopped = app.state.panes.iter().any(|pane| {
                    pane.task_id == task.id
                        && matches!(
                            pane.status,
                            AgentPaneStatus::Waiting
                                | AgentPaneStatus::Failed
                                | AgentPaneStatus::Exited
                        )
                });
                if !supervisor.has_session(&task.id) && !pane_stopped {
                    let model = task.preferred_model.unwrap_or(ModelKind::Claude);
                    let instance_id = format!("agent-{}", task.id.0);
                    match supervisor.spawn_agent(
                        &task.id,
                        &task.repo_id,
                        &task.worktree_path,
                        &task.title,
                        Some(model),
                    ) {
                        Ok(()) => {
                            // Create pane immediately so the UI shows "starting"
                            // instead of "no agent running for this task".
                            app.apply_event(TuiEvent::AgentPaneOutput {
                                instance_id: instance_id.clone(),
                                task_id: task.id.clone(),
                                model,
                                lines: vec![],
                            });
                            app.apply_event(TuiEvent::AgentPaneStatusChanged {
                                instance_id,
                                status: AgentPaneStatus::Starting,
                            });
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!(
                                    "auto-started {:?} agent for {}",
                                    model, task.id.0
                                ),
                            });
                        }
                        Err(e) => {
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!(
                                    "auto-spawn failed for {}: {}",
                                    task.id.0, e
                                ),
                            });
                        }
                    }
                }
            }
        }

        // Refresh task list periodically (every ~2s at 250ms tick).
        tick_counter = tick_counter.wrapping_add(1);
        if tick_counter.is_multiple_of(8) {
            if let Ok(tasks) = service.list_tasks() {
                app.apply_event(TuiEvent::TasksReplaced { tasks: tasks.clone() });
                // Load QA data from disk for each task.
                let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                for task in &tasks {
                    let branch = task.branch_name.as_deref().unwrap_or("-");
                    if let Some(qa_event) = load_qa_display(&repo_root, &task.id, branch) {
                        app.apply_event(qa_event);
                    }
                }
            }
        }
    })?;

    supervisor.stop_all();
    Ok(())
}

// -- QA display loading -----------------------------------------------------

fn load_qa_display(repo_root: &Path, task_id: &TaskId, branch: &str) -> Option<TuiEvent> {
    if branch == "-" || branch.is_empty() {
        return None;
    }

    let result = qa_agent::load_latest_result(repo_root, branch)?;

    let status = if result.summary.failed > 0 {
        format!(
            "failed {}/{}",
            result.summary.passed, result.summary.total
        )
    } else {
        format!("passed {}/{}", result.summary.passed, result.summary.total)
    };

    let tests: Vec<QATestDisplay> = result
        .tests
        .iter()
        .map(|t| QATestDisplay {
            name: t.name.clone(),
            suite: t.suite.clone(),
            passed: t.passed,
            detail: t.detail.clone(),
        })
        .collect();

    // Load task-specific acceptance targets from spec file.
    let targets = qa_agent::load_task_spec(repo_root, task_id)
        .map(|spec| {
            spec.lines()
                .filter(|line| line.trim().starts_with("- "))
                .map(|line| line.trim().strip_prefix("- ").unwrap_or(line.trim()).to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(TuiEvent::QAUpdate {
        task_id: task_id.clone(),
        status,
        tests,
        targets,
    })
}

// -- Chat log persistence ---------------------------------------------------

fn chat_log_path(base: &Path, task_id: &TaskId) -> PathBuf {
    base.join(format!("{}.log", task_id.0))
}

fn append_chat_log(base: &Path, task_id: &TaskId, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let path = chat_log_path(base, task_id);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    for line in lines {
        let _ = writeln!(file, "{}", line);
    }
}

fn load_chat_log(base: &Path, task_id: &TaskId) -> Vec<String> {
    let path = chat_log_path(base, task_id);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    // Keep only the last 400 lines to match the in-memory limit.
    if lines.len() > 400 {
        lines[lines.len() - 400..].to_vec()
    } else {
        lines
    }
}

fn parse_cli_args(args: Vec<String>, program: &str) -> Result<CliArgs, MainError> {
    let mut tick_ms = DEFAULT_TICK_MS;
    let mut sqlite_path = PathBuf::from(DEFAULT_SQLITE_PATH);
    let mut event_log_path = PathBuf::from(DEFAULT_EVENT_LOG_PATH);
    let mut idx = 0usize;

    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Err(MainError::Args(usage(program))),
            "--tick-ms" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --tick-ms".to_string()))?;
                tick_ms = value.parse::<u64>().map_err(|_| {
                    MainError::Args(format!("invalid --tick-ms value: {value} (expected u64)"))
                })?;
                if tick_ms == 0 {
                    return Err(MainError::Args(
                        "invalid --tick-ms value: 0 (must be > 0)".to_string(),
                    ));
                }
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                sqlite_path = PathBuf::from(value);
            }
            "--event-log-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --event-log-path".to_string())
                })?;
                event_log_path = PathBuf::from(value);
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

    Ok(CliArgs {
        tick_ms,
        sqlite_path,
        event_log_path,
    })
}

fn usage(program: &str) -> String {
    format!(
        "Usage: {program} [--tick-ms <u64>] [--sqlite-path <path>] [--event-log-path <path>]\n\
Defaults:\n\
  --tick-ms {DEFAULT_TICK_MS}\n\
  --sqlite-path {DEFAULT_SQLITE_PATH}\n\
  --event-log-path {DEFAULT_EVENT_LOG_PATH}"
    )
}

#[cfg(test)]
mod tests {
    use super::{append_chat_log, chat_log_path, load_chat_log, parse_cli_args, usage, CliArgs};
    use orch_core::types::TaskId;
    use std::path::PathBuf;

    #[test]
    fn parse_cli_args_uses_default_tick_rate() {
        let parsed = parse_cli_args(Vec::new(), "orch-tui").expect("parse");
        assert_eq!(
            parsed,
            CliArgs {
                tick_ms: 250,
                sqlite_path: PathBuf::from(".orch/state.sqlite"),
                event_log_path: PathBuf::from(".orch/events"),
            }
        );
    }

    #[test]
    fn parse_cli_args_applies_tick_rate_and_sqlite_override() {
        let parsed = parse_cli_args(
            vec![
                "--tick-ms".to_string(),
                "500".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
            ],
            "orch-tui",
        )
        .expect("parse");
        assert_eq!(
            parsed,
            CliArgs {
                tick_ms: 500,
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                event_log_path: PathBuf::from(".orch/events"),
            }
        );
    }

    #[test]
    fn parse_cli_args_rejects_missing_tick_rate_value() {
        let err =
            parse_cli_args(vec!["--tick-ms".to_string()], "orch-tui").expect_err("should fail");
        assert_eq!(err.to_string(), "missing value for --tick-ms");

        let err =
            parse_cli_args(vec!["--sqlite-path".to_string()], "orch-tui").expect_err("should fail");
        assert_eq!(err.to_string(), "missing value for --sqlite-path");
    }

    #[test]
    fn parse_cli_args_rejects_invalid_tick_rate_values() {
        let err = parse_cli_args(vec!["--tick-ms".to_string(), "abc".to_string()], "orch-tui")
            .expect_err("should fail");
        assert_eq!(
            err.to_string(),
            "invalid --tick-ms value: abc (expected u64)"
        );

        let err = parse_cli_args(vec!["--tick-ms".to_string(), "0".to_string()], "orch-tui")
            .expect_err("should fail");
        assert_eq!(err.to_string(), "invalid --tick-ms value: 0 (must be > 0)");
    }

    #[test]
    fn parse_cli_args_help_returns_usage() {
        let err = parse_cli_args(vec!["--help".to_string()], "orch-tui").expect_err("help path");
        assert_eq!(err.to_string(), usage("orch-tui"));
    }

    fn temp_chat_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "othala-chat-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn chat_log_roundtrip() {
        let dir = temp_chat_dir();
        let task_id = TaskId("T1".to_string());

        // Empty before any writes.
        assert!(load_chat_log(&dir, &task_id).is_empty());

        // Write some lines.
        append_chat_log(
            &dir,
            &task_id,
            &["line one".to_string(), "line two".to_string()],
        );
        let loaded = load_chat_log(&dir, &task_id);
        assert_eq!(loaded, vec!["line one", "line two"]);

        // Append more lines.
        append_chat_log(&dir, &task_id, &["line three".to_string()]);
        let loaded = load_chat_log(&dir, &task_id);
        assert_eq!(loaded, vec!["line one", "line two", "line three"]);
    }

    #[test]
    fn chat_log_truncates_on_load() {
        let dir = temp_chat_dir();
        let task_id = TaskId("T-big".to_string());

        let lines: Vec<String> = (0..500).map(|i| format!("line {i}")).collect();
        append_chat_log(&dir, &task_id, &lines);

        let loaded = load_chat_log(&dir, &task_id);
        assert_eq!(loaded.len(), 400);
        assert_eq!(loaded[0], "line 100");
        assert_eq!(loaded[399], "line 499");
    }

    #[test]
    fn chat_log_path_uses_task_id() {
        let dir = PathBuf::from("/tmp/chat");
        let task_id = TaskId("chat-123".to_string());
        assert_eq!(chat_log_path(&dir, &task_id), PathBuf::from("/tmp/chat/chat-123.log"));
    }

    #[test]
    fn append_chat_log_skips_empty_lines_vec() {
        let dir = temp_chat_dir();
        let task_id = TaskId("T-empty".to_string());

        // Appending empty vec should not create file.
        append_chat_log(&dir, &task_id, &[]);
        assert!(!chat_log_path(&dir, &task_id).exists());
    }
}
