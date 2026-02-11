use chrono::Utc;
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{EventId, ModelKind, RepoId, SubmitMode, Task, TaskId};
use orch_tui::{run_tui_with_hook, AgentPaneStatus, QATestDisplay, TuiApp, TuiEvent, UiAction};
use orchd::qa_agent;
use orchd::stack_pipeline::{self, PipelineState};
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

// -- Pipeline subprocess tracking -------------------------------------------

enum PipelineProcMsg {
    Output(String),
    Done { success: bool, detail: String },
}

struct PipelineProc {
    output_rx: std::sync::mpsc::Receiver<PipelineProcMsg>,
}

fn spawn_pipeline_cmd(cmd: &str, args: &[String], cwd: &Path) -> PipelineProc {
    let (tx, rx) = std::sync::mpsc::channel();
    let cmd_owned = cmd.to_string();
    let args_owned = args.to_vec();
    let cwd_owned = cwd.to_path_buf();

    std::thread::spawn(move || {
        use std::io::BufRead;

        let child = std::process::Command::new(&cmd_owned)
            .args(&args_owned)
            .current_dir(&cwd_owned)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(PipelineProcMsg::Done {
                    success: false,
                    detail: format!("spawn failed: {e}"),
                });
                return;
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let tx_out = tx.clone();
        let h1 = std::thread::spawn(move || {
            if let Some(out) = stdout {
                for line in std::io::BufReader::new(out).lines().flatten() {
                    let _ = tx_out.send(PipelineProcMsg::Output(line));
                }
            }
        });

        let tx_err = tx.clone();
        let h2 = std::thread::spawn(move || {
            if let Some(err) = stderr {
                for line in std::io::BufReader::new(err).lines().flatten() {
                    let _ = tx_err.send(PipelineProcMsg::Output(line));
                }
            }
        });

        let _ = h1.join();
        let _ = h2.join();

        let (success, detail) = match child.wait() {
            Ok(s) => (s.success(), format!("exit {}", s.code().unwrap_or(-1))),
            Err(e) => (false, format!("wait error: {e}")),
        };
        let _ = tx.send(PipelineProcMsg::Done { success, detail });
    });

    PipelineProc { output_rx: rx }
}

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
            // Tasks still Chatting were killed when TUI closed — show "stopped".
            // Tasks that completed (Ready+) show "exited".
            let status = if task.state == TaskState::Chatting {
                AgentPaneStatus::Stopped
            } else {
                AgentPaneStatus::Exited
            };
            app.apply_event(TuiEvent::AgentPaneStatusChanged {
                instance_id,
                status,
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
    let mut qa_agents: HashMap<String, qa_agent::QAState> = HashMap::new();
    let mut pipelines: HashMap<String, PipelineState> = HashMap::new();
    let mut pipeline_procs: HashMap<String, PipelineProc> = HashMap::new();
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let template_dir = PathBuf::from("templates/prompts");

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
                UiAction::SubmitTask => {
                    if let Some(task_id) = &queued.task_id {
                        if let Ok(Some(task)) = service.task(task_id) {
                            if task.state == TaskState::Ready
                                && !pipelines.contains_key(&task_id.0)
                            {
                                let parent_branch = task
                                    .parent_task_id
                                    .as_ref()
                                    .and_then(|pid| service.task(pid).ok().flatten())
                                    .and_then(|p| p.branch_name);
                                let pipeline = PipelineState::new(
                                    task_id.clone(),
                                    task.branch_name
                                        .clone()
                                        .unwrap_or_else(|| format!("task/{}", task_id.0)),
                                    task.worktree_path.clone(),
                                    task.submit_mode,
                                    parent_branch,
                                );
                                pipelines.insert(task_id.0.clone(), pipeline);
                                let event_id =
                                    EventId(format!("E-SUBMITTING-{}", task_id.0));
                                let _ = service.transition_task_state(
                                    task_id,
                                    TaskState::Submitting,
                                    event_id,
                                    Utc::now(),
                                );
                                let pipe_instance = format!("pipeline-{}", task_id.0);
                                let model =
                                    task.preferred_model.unwrap_or(ModelKind::Claude);
                                app.apply_event(TuiEvent::AgentPaneOutput {
                                    instance_id: pipe_instance.clone(),
                                    task_id: task_id.clone(),
                                    model,
                                    lines: vec![
                                        "[Submit pipeline starting...]".to_string(),
                                    ],
                                });
                                app.apply_event(TuiEvent::AgentPaneStatusChanged {
                                    instance_id: pipe_instance,
                                    status: AgentPaneStatus::Starting,
                                });
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!(
                                        "{} -> Submitting (manual)",
                                        task_id.0
                                    ),
                                });
                                if let Ok(tasks) = service.list_tasks() {
                                    app.apply_event(TuiEvent::TasksReplaced { tasks });
                                }
                            } else {
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!(
                                        "{} not Ready or pipeline already running",
                                        task_id.0
                                    ),
                                });
                            }
                        }
                    }
                }
                _ => {
                    app.apply_event(TuiEvent::StatusLine {
                        message: format!(
                            "action not yet implemented: {:?}",
                            queued.action
                        ),
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
                app.apply_event(TuiEvent::AgentPaneStatusChanged {
                    instance_id: instance_id.clone(),
                    status: AgentPaneStatus::Exited,
                });

                // If baseline.md exists, spawn QA validation before advancing.
                // The task stays at Chatting until QA passes.
                let qa_key = format!("qa-{}", outcome.task_id.0);
                let has_baseline = qa_agent::load_baseline(&repo_root).is_some();
                if has_baseline && !qa_agents.contains_key(&qa_key) {
                    let baseline = qa_agent::load_baseline(&repo_root).unwrap();
                    let task_spec =
                        qa_agent::load_task_spec(&repo_root, &outcome.task_id);
                    let previous = service
                        .task(&outcome.task_id)
                        .ok()
                        .flatten()
                        .and_then(|t| t.branch_name)
                        .as_deref()
                        .and_then(|b| qa_agent::load_latest_result(&repo_root, b));
                    let prompt = qa_agent::build_qa_prompt(
                        &baseline,
                        task_spec.as_deref(),
                        previous.as_ref(),
                        &repo_root,
                        &template_dir,
                    );
                    let mut qa_state =
                        qa_agent::QAState::new(qa_agent::QAType::Validation);
                    match qa_agent::spawn_qa_agent(
                        &repo_root,
                        &prompt,
                        outcome.model,
                        &mut qa_state,
                    ) {
                        Ok(()) => {
                            qa_agents.insert(qa_key, qa_state);
                            let targets = task_spec
                                .as_deref()
                                .map(|s| extract_targets(s))
                                .unwrap_or_default();
                            // Create a TUI pane for QA agent output.
                            let qa_instance = format!("qa-{}", outcome.task_id.0);
                            app.apply_event(TuiEvent::AgentPaneOutput {
                                instance_id: qa_instance.clone(),
                                task_id: outcome.task_id.clone(),
                                model: outcome.model,
                                lines: vec!["[QA validation starting...]".to_string()],
                            });
                            app.apply_event(TuiEvent::AgentPaneStatusChanged {
                                instance_id: qa_instance,
                                status: AgentPaneStatus::Starting,
                            });
                            app.apply_event(TuiEvent::QAUpdate {
                                task_id: outcome.task_id.clone(),
                                status: "validation running".to_string(),
                                tests: vec![],
                                targets,
                            });
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!(
                                    "QA validation started for {} (patch_ready)",
                                    outcome.task_id.0
                                ),
                            });
                        }
                        Err(e) => {
                            // QA spawn failed — advance anyway.
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!(
                                    "QA validation spawn failed for {}: {e} — advancing",
                                    outcome.task_id.0
                                ),
                            });
                            let event_id =
                                EventId(format!("E-READY-{}", outcome.task_id.0));
                            let _ = service.mark_ready(&outcome.task_id, event_id, now);
                        }
                    }
                } else {
                    // No baseline.md — advance directly to Ready.
                    let event_id = EventId(format!("E-READY-{}", outcome.task_id.0));
                    match service.mark_ready(&outcome.task_id, event_id, now) {
                        Ok(_) => {
                            let reason = if outcome.patch_ready {
                                "patch_ready"
                            } else {
                                "exit 0"
                            };
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!(
                                    "{} -> Ready ({reason})",
                                    outcome.task_id.0
                                ),
                            });
                        }
                        Err(e) => {
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!(
                                    "{} done but mark_ready failed: {e}",
                                    outcome.task_id.0
                                ),
                            });
                        }
                    }
                }
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

        // Refresh task list immediately when any agents completed so
        // state changes (Chatting → Ready) show up without delay.
        if !result.completed.is_empty() {
            if let Ok(tasks) = service.list_tasks() {
                app.apply_event(TuiEvent::TasksReplaced { tasks });
            }
        }

        // Auto-spawn agents for Chatting tasks without a running session.
        // Skip tasks whose pane is in a terminal state (Waiting/Failed/Exited/Stopped).
        if let Ok(chatting) = service.list_tasks_by_state(TaskState::Chatting) {
            for task in &chatting {
                let pane_stopped = app.state.panes.iter().any(|pane| {
                    pane.task_id == task.id
                        && matches!(
                            pane.status,
                            AgentPaneStatus::Waiting
                                | AgentPaneStatus::Failed
                                | AgentPaneStatus::Exited
                                | AgentPaneStatus::Stopped
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

                            // Spawn QA baseline in parallel if baseline.md exists
                            // and no QA agent is already running for this task.
                            let qa_key = format!("qa-{}", task.id.0);
                            if !qa_agents.contains_key(&qa_key) {
                                if let Some(baseline) = qa_agent::load_baseline(&repo_root) {
                                    let task_spec =
                                        qa_agent::load_task_spec(&repo_root, &task.id);
                                    let previous =
                                        task.branch_name.as_deref().and_then(|b| {
                                            qa_agent::load_latest_result(&repo_root, b)
                                        });
                                    let prompt = qa_agent::build_qa_prompt(
                                        &baseline,
                                        task_spec.as_deref(),
                                        previous.as_ref(),
                                        &repo_root,
                                        &template_dir,
                                    );
                                    let mut qa_state =
                                        qa_agent::QAState::new(qa_agent::QAType::Baseline);
                                    match qa_agent::spawn_qa_agent(
                                        &repo_root,
                                        &prompt,
                                        model,
                                        &mut qa_state,
                                    ) {
                                        Ok(()) => {
                                            qa_agents.insert(qa_key, qa_state);
                                            // Create a TUI pane for QA agent output.
                                            let qa_instance = format!("qa-{}", task.id.0);
                                            app.apply_event(TuiEvent::AgentPaneOutput {
                                                instance_id: qa_instance.clone(),
                                                task_id: task.id.clone(),
                                                model,
                                                lines: vec!["[QA baseline starting...]".to_string()],
                                            });
                                            app.apply_event(TuiEvent::AgentPaneStatusChanged {
                                                instance_id: qa_instance,
                                                status: AgentPaneStatus::Starting,
                                            });
                                            app.apply_event(TuiEvent::QAUpdate {
                                                task_id: task.id.clone(),
                                                status: "baseline running".to_string(),
                                                tests: vec![],
                                                targets: task_spec
                                                    .as_deref()
                                                    .map(|s| extract_targets(s))
                                                    .unwrap_or_default(),
                                            });
                                            app.apply_event(TuiEvent::StatusLine {
                                                message: format!(
                                                    "QA baseline started for {}",
                                                    task.id.0
                                                ),
                                            });
                                        }
                                        Err(e) => {
                                            app.apply_event(TuiEvent::StatusLine {
                                                message: format!(
                                                    "QA spawn failed for {}: {e}",
                                                    task.id.0
                                                ),
                                            });
                                        }
                                    }
                                }
                            }
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

        // Spawn QA baseline for any chatting task that has an agent
        // running but no QA agent yet (catches tasks started before
        // baseline.md existed or before this code was deployed).
        if let Ok(chatting) = service.list_tasks_by_state(TaskState::Chatting) {
            for task in &chatting {
                let qa_key = format!("qa-{}", task.id.0);
                if !qa_agents.contains_key(&qa_key) && supervisor.has_session(&task.id) {
                    if let Some(baseline) = qa_agent::load_baseline(&repo_root) {
                        let model = task.preferred_model.unwrap_or(ModelKind::Claude);
                        let task_spec = qa_agent::load_task_spec(&repo_root, &task.id);
                        let previous = task
                            .branch_name
                            .as_deref()
                            .and_then(|b| qa_agent::load_latest_result(&repo_root, b));
                        let prompt = qa_agent::build_qa_prompt(
                            &baseline,
                            task_spec.as_deref(),
                            previous.as_ref(),
                            &repo_root,
                            &template_dir,
                        );
                        let mut qa_state =
                            qa_agent::QAState::new(qa_agent::QAType::Baseline);
                        if let Ok(()) = qa_agent::spawn_qa_agent(
                            &repo_root, &prompt, model, &mut qa_state,
                        ) {
                            qa_agents.insert(qa_key, qa_state);
                            // Create a TUI pane for QA agent output.
                            let qa_instance = format!("qa-{}", task.id.0);
                            app.apply_event(TuiEvent::AgentPaneOutput {
                                instance_id: qa_instance.clone(),
                                task_id: task.id.clone(),
                                model,
                                lines: vec!["[QA baseline starting...]".to_string()],
                            });
                            app.apply_event(TuiEvent::AgentPaneStatusChanged {
                                instance_id: qa_instance,
                                status: AgentPaneStatus::Starting,
                            });
                            app.apply_event(TuiEvent::QAUpdate {
                                task_id: task.id.clone(),
                                status: "baseline running".to_string(),
                                tests: vec![],
                                targets: task_spec
                                    .as_deref()
                                    .map(|s| extract_targets(s))
                                    .unwrap_or_default(),
                            });
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!(
                                    "QA baseline started for {} (catch-up)",
                                    task.id.0
                                ),
                            });
                        }
                    }
                }
            }
        }

        // Drain QA agent output lines into their TUI panes.
        for (qa_key, qa_state) in qa_agents.iter_mut() {
            let task_id_str = qa_key.strip_prefix("qa-").unwrap_or(qa_key);
            let qa_lines = qa_agent::drain_qa_output(qa_state);
            if !qa_lines.is_empty() {
                let qa_instance = format!("qa-{task_id_str}");
                let task_id = TaskId(task_id_str.to_string());
                let model = service
                    .task(&task_id)
                    .ok()
                    .flatten()
                    .and_then(|t| t.preferred_model)
                    .unwrap_or(ModelKind::Claude);
                app.apply_event(TuiEvent::AgentPaneOutput {
                    instance_id: qa_instance,
                    task_id,
                    model,
                    lines: qa_lines,
                });
            }
        }

        // Poll QA agents and handle completions.
        let qa_keys: Vec<String> = qa_agents.keys().cloned().collect();
        for qa_key in qa_keys {
            let task_id_str = qa_key.strip_prefix("qa-").unwrap_or(&qa_key);
            let task_id = TaskId(task_id_str.to_string());

            let qa_state = qa_agents.get_mut(&qa_key).unwrap();
            let qa_type = qa_state.qa_type;
            if let Some(qa_result) = qa_agent::poll_qa_agent(qa_state) {
                // QA agent completed — save result and update UI.
                let _ = qa_agent::save_qa_result(&repo_root, &qa_result);

                let all_passed = qa_result.summary.failed == 0;
                let status = if all_passed {
                    format!(
                        "{} passed {}/{}",
                        qa_type,
                        qa_result.summary.passed,
                        qa_result.summary.total
                    )
                } else {
                    format!(
                        "{} failed {}/{}",
                        qa_type,
                        qa_result.summary.passed,
                        qa_result.summary.total
                    )
                };

                let tests: Vec<QATestDisplay> = qa_result
                    .tests
                    .iter()
                    .map(|t| QATestDisplay {
                        name: t.name.clone(),
                        suite: t.suite.clone(),
                        passed: t.passed,
                        detail: t.detail.clone(),
                    })
                    .collect();

                let targets = qa_agent::load_task_spec(&repo_root, &task_id)
                    .map(|s| extract_targets(&s))
                    .unwrap_or_default();

                // Mark the QA pane as exited.
                let qa_instance = format!("qa-{}", task_id.0);
                app.apply_event(TuiEvent::AgentPaneStatusChanged {
                    instance_id: qa_instance,
                    status: if all_passed {
                        AgentPaneStatus::Exited
                    } else {
                        AgentPaneStatus::Failed
                    },
                });

                app.apply_event(TuiEvent::QAUpdate {
                    task_id: task_id.clone(),
                    status: status.clone(),
                    tests,
                    targets,
                });
                app.apply_event(TuiEvent::StatusLine {
                    message: format!("QA {status} for {}", task_id.0),
                });

                // If this was a validation run, advance the task based on result.
                if qa_type == qa_agent::QAType::Validation {
                    let now = Utc::now();
                    if all_passed {
                        let event_id =
                            EventId(format!("E-READY-QA-{}", task_id.0));
                        match service.mark_ready(&task_id, event_id, now) {
                            Ok(_) => {
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!(
                                        "{} -> Ready (QA validation passed)",
                                        task_id.0
                                    ),
                                });
                            }
                            Err(e) => {
                                app.apply_event(TuiEvent::StatusLine {
                                    message: format!(
                                        "{} QA passed but mark_ready failed: {e}",
                                        task_id.0
                                    ),
                                });
                            }
                        }
                        // Refresh tasks so UI shows the state change.
                        if let Ok(tasks) = service.list_tasks() {
                            app.apply_event(TuiEvent::TasksReplaced { tasks });
                        }
                    } else {
                        // QA failed — task stays at Chatting for retry.
                        app.apply_event(TuiEvent::StatusLine {
                            message: format!(
                                "{} QA validation failed — needs fixes",
                                task_id.0
                            ),
                        });
                    }
                }

                qa_agents.remove(&qa_key);
            }
        }

        // --- Drive submit pipelines for Ready tasks ---

        // Auto-create pipelines for Ready tasks that don't have one yet.
        if let Ok(ready_tasks) = service.list_tasks_by_state(TaskState::Ready) {
            for task in &ready_tasks {
                if !pipelines.contains_key(&task.id.0) {
                    let parent_branch = task
                        .parent_task_id
                        .as_ref()
                        .and_then(|pid| service.task(pid).ok().flatten())
                        .and_then(|p| p.branch_name);
                    let pipeline = PipelineState::new(
                        task.id.clone(),
                        task.branch_name
                            .clone()
                            .unwrap_or_else(|| format!("task/{}", task.id.0)),
                        task.worktree_path.clone(),
                        task.submit_mode,
                        parent_branch,
                    );
                    pipelines.insert(task.id.0.clone(), pipeline);

                    let event_id = EventId(format!("E-SUBMITTING-{}", task.id.0));
                    let _ = service.transition_task_state(
                        &task.id,
                        TaskState::Submitting,
                        event_id,
                        Utc::now(),
                    );

                    let pipe_instance = format!("pipeline-{}", task.id.0);
                    let model = task.preferred_model.unwrap_or(ModelKind::Claude);
                    app.apply_event(TuiEvent::AgentPaneOutput {
                        instance_id: pipe_instance.clone(),
                        task_id: task.id.clone(),
                        model,
                        lines: vec!["[Submit pipeline starting...]".to_string()],
                    });
                    app.apply_event(TuiEvent::AgentPaneStatusChanged {
                        instance_id: pipe_instance,
                        status: AgentPaneStatus::Starting,
                    });
                    app.apply_event(TuiEvent::StatusLine {
                        message: format!("{} -> Submitting (auto-pipeline)", task.id.0),
                    });
                }
            }
        }

        // Drive each pipeline: spawn subprocess for next step if none running.
        {
            let keys: Vec<String> = pipelines.keys().cloned().collect();
            for key in keys {
                let pipeline = pipelines.get(&key).unwrap();
                if pipeline.is_terminal() || pipeline_procs.contains_key(&key) {
                    continue;
                }

                let action = stack_pipeline::next_action(pipeline);
                let spawn_info: Option<(String, Vec<String>, PathBuf, String, TaskId)> =
                    match &action {
                        stack_pipeline::PipelineAction::RunVerify {
                            worktree_path,
                            task_id,
                        } => {
                            let stage = pipeline.stage.to_string();
                            Some((
                                "cargo".to_string(),
                                vec!["test".to_string(), "--workspace".to_string()],
                                worktree_path.clone(),
                                format!("[{stage}: cargo test --workspace]"),
                                task_id.clone(),
                            ))
                        }
                        stack_pipeline::PipelineAction::StackOnParent {
                            worktree_path,
                            parent_branch,
                            task_id,
                        } => Some((
                            "gt".to_string(),
                            vec![
                                "upstack".to_string(),
                                "onto".to_string(),
                                parent_branch.clone(),
                            ],
                            worktree_path.clone(),
                            format!("[stack: gt upstack onto {parent_branch}]"),
                            task_id.clone(),
                        )),
                        stack_pipeline::PipelineAction::Submit {
                            worktree_path,
                            mode,
                            task_id,
                        } => {
                            let (args, label) = match mode {
                                SubmitMode::Single => (
                                    vec!["submit".to_string(), "--no-edit".to_string()],
                                    "[submit: gt submit --no-edit]".to_string(),
                                ),
                                SubmitMode::Stack => (
                                    vec!["ss".to_string(), "--no-edit".to_string()],
                                    "[submit: gt ss --no-edit]".to_string(),
                                ),
                            };
                            Some((
                                "gt".to_string(),
                                args,
                                worktree_path.clone(),
                                label,
                                task_id.clone(),
                            ))
                        }
                        stack_pipeline::PipelineAction::Complete { task_id } => {
                            let event_id =
                                EventId(format!("E-AWAIT-{}", task_id.0));
                            let _ = service.transition_task_state(
                                task_id,
                                TaskState::AwaitingMerge,
                                event_id,
                                Utc::now(),
                            );
                            let pipe_instance = format!("pipeline-{key}");
                            app.apply_event(TuiEvent::AgentPaneStatusChanged {
                                instance_id: pipe_instance,
                                status: AgentPaneStatus::Exited,
                            });
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!(
                                    "{} -> AwaitingMerge (submitted)",
                                    task_id.0
                                ),
                            });
                            if let Ok(tasks) = service.list_tasks() {
                                app.apply_event(TuiEvent::TasksReplaced { tasks });
                            }
                            None
                        }
                        stack_pipeline::PipelineAction::Failed {
                            task_id,
                            stage,
                            error,
                        } => {
                            let pipe_instance = format!("pipeline-{key}");
                            app.apply_event(TuiEvent::AgentPaneStatusChanged {
                                instance_id: pipe_instance,
                                status: AgentPaneStatus::Failed,
                            });
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!(
                                    "{} pipeline failed at {}: {}",
                                    task_id.0, stage, error
                                ),
                            });
                            None
                        }
                    };

                if let Some((cmd, args, cwd, label, task_id)) = spawn_info {
                    let pipe_instance = format!("pipeline-{key}");
                    let model = service
                        .task(&task_id)
                        .ok()
                        .flatten()
                        .and_then(|t| t.preferred_model)
                        .unwrap_or(ModelKind::Claude);
                    app.apply_event(TuiEvent::AgentPaneOutput {
                        instance_id: pipe_instance,
                        task_id,
                        model,
                        lines: vec![label],
                    });
                    let proc = spawn_pipeline_cmd(&cmd, &args, &cwd);
                    pipeline_procs.insert(key, proc);
                }
            }
        }

        // Poll pipeline subprocess output and completions.
        {
            let keys: Vec<String> = pipeline_procs.keys().cloned().collect();
            for key in keys {
                let proc = pipeline_procs.get(&key).unwrap();
                let mut lines_buf = Vec::new();
                let mut done = None;

                while let Ok(msg) = proc.output_rx.try_recv() {
                    match msg {
                        PipelineProcMsg::Output(line) => lines_buf.push(line),
                        PipelineProcMsg::Done { success, detail } => {
                            done = Some((success, detail));
                            break;
                        }
                    }
                }

                if !lines_buf.is_empty() {
                    let pipe_instance = format!("pipeline-{key}");
                    let task_id = TaskId(key.clone());
                    let model = service
                        .task(&task_id)
                        .ok()
                        .flatten()
                        .and_then(|t| t.preferred_model)
                        .unwrap_or(ModelKind::Claude);
                    app.apply_event(TuiEvent::AgentPaneOutput {
                        instance_id: pipe_instance,
                        task_id,
                        model,
                        lines: lines_buf,
                    });
                }

                if let Some((success, detail)) = done {
                    pipeline_procs.remove(&key);
                    if let Some(pipeline) = pipelines.get_mut(&key) {
                        let stage = pipeline.stage;
                        if success {
                            pipeline.advance();
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!("{key} pipeline: {stage} passed"),
                            });
                        } else {
                            pipeline.fail(detail.clone());
                            let pipe_instance = format!("pipeline-{key}");
                            app.apply_event(TuiEvent::AgentPaneStatusChanged {
                                instance_id: pipe_instance,
                                status: AgentPaneStatus::Failed,
                            });
                            app.apply_event(TuiEvent::StatusLine {
                                message: format!(
                                    "{key} pipeline: {stage} failed — {detail}"
                                ),
                            });
                        }
                    }
                }
            }
        }

        // Clean up terminal pipelines.
        pipelines.retain(|_, p| !p.is_terminal());

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
    // Drop pipeline subprocess trackers (threads will clean up).
    pipeline_procs.clear();
    pipelines.clear();
    // Kill any running QA agents.
    for (_key, mut qa_state) in qa_agents.drain() {
        if let Some(mut child) = qa_state.child_handle.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    Ok(())
}

// -- QA helpers -------------------------------------------------------------

/// Extract acceptance target lines from a task QA spec.
fn extract_targets(spec: &str) -> Vec<String> {
    spec.lines()
        .filter(|line| line.trim().starts_with("- "))
        .map(|line| line.trim().strip_prefix("- ").unwrap_or(line.trim()).to_string())
        .collect()
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
