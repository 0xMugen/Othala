//! Agent supervisor — spawns and monitors AI agent processes.

use chrono::{DateTime, Utc};
use orch_agents::{
    default_adapter_for, detect_common_signal, AgentAdapter, AgentSignalKind, EpochRequest,
};
use orch_core::types::{ModelKind, RepoId, TaskId};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::context_graph::{load_context_graph, render_context_with_sources, ContextLoadConfig};

/// A running agent session.
pub struct AgentSession {
    pub child: Child,
    pub output_rx: mpsc::Receiver<String>,
    /// Sender for writing to the agent's stdin (interactive sessions only).
    pub input_tx: Option<mpsc::Sender<String>>,
    pub task_id: TaskId,
    pub model: ModelKind,
    pub started_at: DateTime<Utc>,
    pub patch_ready: bool,
    pub needs_human: bool,
    /// When the agent signaled completion (patch_ready or needs_human).
    /// Used to enforce a grace period before killing the process.
    pub signal_at: Option<Instant>,
}

/// Result returned when an agent session finishes.
#[derive(Debug)]
pub struct AgentOutcome {
    pub task_id: TaskId,
    pub model: ModelKind,
    pub exit_code: Option<i32>,
    pub patch_ready: bool,
    pub needs_human: bool,
    pub success: bool,
    pub duration_secs: u64,
}

/// A batch of output lines from one agent session.
#[derive(Debug)]
pub struct OutputChunk {
    pub task_id: TaskId,
    pub model: ModelKind,
    pub lines: Vec<String>,
}

/// Result of a single poll cycle.
pub struct PollResult {
    pub output: Vec<OutputChunk>,
    pub completed: Vec<AgentOutcome>,
}

/// Spawn background threads that pipe stdout and stderr lines into `tx`.
///
/// Consumes `tx` (the last clone goes to the stderr thread).
fn pipe_child_output(child: &mut Child, tx: mpsc::Sender<String>) {
    if let Some(stdout) = child.stdout.take() {
        let tx_out = tx.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx_out.send(line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }
}

/// Manages running agent sessions.
pub struct AgentSupervisor {
    sessions: HashMap<String, AgentSession>,
    default_model: ModelKind,
}

impl AgentSupervisor {
    pub fn new(default_model: ModelKind) -> Self {
        Self {
            sessions: HashMap::new(),
            default_model,
        }
    }

    pub fn has_session(&self, task_id: &TaskId) -> bool {
        self.sessions.contains_key(&task_id.0)
    }

    /// Spawn an agent process for a task.
    pub fn spawn_agent(
        &mut self,
        task_id: &TaskId,
        repo_id: &RepoId,
        repo_path: &PathBuf,
        prompt: &str,
        model: Option<ModelKind>,
    ) -> anyhow::Result<()> {
        let model = model.unwrap_or(self.default_model);
        let adapter: Box<dyn AgentAdapter> = default_adapter_for(model)?;

        let request = EpochRequest {
            task_id: task_id.clone(),
            repo_id: repo_id.clone(),
            model,
            repo_path: repo_path.clone(),
            prompt: build_prompt(task_id, prompt, repo_path),
            timeout_secs: 600,
            extra_args: vec![],
            env: vec![],
        };

        let cmd = adapter.build_command(&request);

        let mut child = Command::new(&cmd.executable)
            .args(&cmd.args)
            .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .env_remove("CLAUDECODE")
            .current_dir(repo_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let (tx, rx) = mpsc::channel();
        pipe_child_output(&mut child, tx);

        let session = AgentSession {
            child,
            output_rx: rx,
            input_tx: None,
            task_id: task_id.clone(),
            model,
            started_at: Utc::now(),
            patch_ready: false,
            needs_human: false,
            signal_at: None,
        };

        self.sessions.insert(task_id.0.clone(), session);
        Ok(())
    }

    /// Spawn an interactive agent process for a task.
    ///
    /// Unlike `spawn_agent`, the process's stdin is piped and `initial_prompt`
    /// is written as the first message.  The caller can subsequently send
    /// follow-up messages via `send_input`.
    pub fn spawn_interactive(
        &mut self,
        task_id: &TaskId,
        repo_id: &RepoId,
        repo_path: &PathBuf,
        initial_prompt: &str,
        model: Option<ModelKind>,
    ) -> anyhow::Result<()> {
        let model = model.unwrap_or(self.default_model);
        let adapter: Box<dyn AgentAdapter> = default_adapter_for(model)?;

        let request = EpochRequest {
            task_id: task_id.clone(),
            repo_id: repo_id.clone(),
            model,
            repo_path: repo_path.clone(),
            prompt: build_prompt(task_id, initial_prompt, repo_path),
            timeout_secs: 600,
            extra_args: vec![],
            env: vec![],
        };

        let cmd = adapter.build_interactive_command(&request);

        let mut child = Command::new(&cmd.executable)
            .args(&cmd.args)
            .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .env_remove("CLAUDECODE")
            .current_dir(repo_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let (out_tx, out_rx) = mpsc::channel();
        pipe_child_output(&mut child, out_tx);

        // Create a channel + background thread for stdin writes.
        let (in_tx, in_rx) = mpsc::channel::<String>();
        if let Some(stdin) = child.stdin.take() {
            use std::io::Write;
            thread::spawn(move || {
                let mut stdin = stdin;
                while let Ok(msg) = in_rx.recv() {
                    if writeln!(stdin, "{msg}").is_err() {
                        break;
                    }
                    if stdin.flush().is_err() {
                        break;
                    }
                }
            });
        }

        // Send the initial prompt as the first message.
        let _ = in_tx.send(request.prompt.clone());

        let session = AgentSession {
            child,
            output_rx: out_rx,
            input_tx: Some(in_tx),
            task_id: task_id.clone(),
            model,
            started_at: Utc::now(),
            patch_ready: false,
            needs_human: false,
            signal_at: None,
        };

        self.sessions.insert(task_id.0.clone(), session);
        Ok(())
    }

    /// Send a message to the stdin of a running interactive agent session.
    pub fn send_input(&self, task_id: &TaskId, message: &str) -> anyhow::Result<()> {
        let session = self
            .sessions
            .get(&task_id.0)
            .ok_or_else(|| anyhow::anyhow!("no session for task {}", task_id.0))?;
        let tx = session
            .input_tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("session for {} is not interactive", task_id.0))?;
        tx.send(message.to_string())
            .map_err(|_| anyhow::anyhow!("stdin channel closed for {}", task_id.0))
    }

    /// Non-blocking poll: drain output, detect signals, collect finished sessions.
    pub fn poll(&mut self) -> PollResult {
        let mut output = Vec::new();
        let mut completed = Vec::new();
        let mut finished_keys = Vec::new();

        for (key, session) in self.sessions.iter_mut() {
            // Drain output lines and check for signals.
            let mut lines = Vec::new();
            while let Ok(line) = session.output_rx.try_recv() {
                if let Some(signal) = detect_common_signal(&line) {
                    match signal.kind {
                        AgentSignalKind::PatchReady => {
                            session.patch_ready = true;
                            if session.signal_at.is_none() {
                                session.signal_at = Some(Instant::now());
                            }
                        }
                        AgentSignalKind::NeedHuman => {
                            session.needs_human = true;
                            if session.signal_at.is_none() {
                                session.signal_at = Some(Instant::now());
                            }
                        }
                        _ => {}
                    }
                }
                lines.push(line);
            }
            if !lines.is_empty() {
                output.push(OutputChunk {
                    task_id: session.task_id.clone(),
                    model: session.model,
                    lines,
                });
            }

            // Kill process if it signaled completion but hasn't exited.
            if let Some(t) = session.signal_at {
                if t.elapsed() > Duration::from_secs(5) {
                    let _ = session.child.kill();
                }
            }

            // Check if process has exited.
            match session.child.try_wait() {
                Ok(Some(status)) => {
                    let exit_code = status.code();
                    let success = session.patch_ready || exit_code == Some(0);
                    let duration_secs = Utc::now()
                        .signed_duration_since(session.started_at)
                        .num_seconds()
                        .max(0) as u64;
                    completed.push(AgentOutcome {
                        task_id: session.task_id.clone(),
                        model: session.model,
                        exit_code,
                        patch_ready: session.patch_ready,
                        needs_human: session.needs_human,
                        success,
                        duration_secs,
                    });
                    finished_keys.push(key.clone());
                }
                Ok(None) => {} // still running
                Err(_) => {
                    let duration_secs = Utc::now()
                        .signed_duration_since(session.started_at)
                        .num_seconds()
                        .max(0) as u64;
                    completed.push(AgentOutcome {
                        task_id: session.task_id.clone(),
                        model: session.model,
                        exit_code: None,
                        patch_ready: false,
                        needs_human: false,
                        success: false,
                        duration_secs,
                    });
                    finished_keys.push(key.clone());
                }
            }
        }

        for key in finished_keys {
            self.sessions.remove(&key);
        }

        PollResult { output, completed }
    }

    /// Kill all running agent processes.
    pub fn stop_all(&mut self) {
        for (_key, mut session) in self.sessions.drain() {
            let _ = session.child.kill();
            let _ = session.child.wait();
        }
    }

    /// Stop the agent for a specific task.
    pub fn stop(&mut self, task_id: &TaskId) {
        if let Some(mut session) = self.sessions.remove(&task_id.0) {
            let _ = session.child.kill();
            let _ = session.child.wait();
        }
    }
}

/// Build the prompt sent to the agent CLI.
///
/// Loads the `.othala/context/` graph (if present) and injects it so the agent
/// understands the project architecture, patterns, and conventions.
pub fn build_prompt(task_id: &TaskId, title: &str, repo_root: &Path) -> String {
    let mut sections = Vec::new();

    if let Some(graph) = load_context_graph(repo_root, &ContextLoadConfig::default()) {
        if !graph.nodes.is_empty() {
            const SOURCE_BUDGET: usize = 64_000;
            sections.push(render_context_with_sources(&graph, repo_root, SOURCE_BUDGET));
        }
    }

    // Task assignment.
    sections.push(format!(
        "# Task Assignment\n\n\
         **Task ID:** {}\n\
         **Title:** {}\n",
        task_id.0, title,
    ));

    // Signal definitions.
    sections.push(
        "# Signals\n\n\
         - When you are done and the code is ready, print exactly: `[patch_ready]`\n\
         - If you are blocked and need human help, print exactly: `[needs_human]`\n"
            .to_string(),
    );

    sections.join("\n---\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::types::{ModelKind, TaskId};
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    // -----------------------------------------------------------------------
    // build_prompt
    // -----------------------------------------------------------------------

    #[test]
    fn build_prompt_includes_task_id_and_title() {
        let task_id = TaskId::new("T-42");
        let prompt = build_prompt(&task_id, "Add auth endpoint", Path::new("/tmp/nonexistent"));

        assert!(prompt.contains("T-42"), "prompt must include task id");
        assert!(
            prompt.contains("Add auth endpoint"),
            "prompt must include title"
        );
    }

    #[test]
    fn build_prompt_includes_signal_definitions() {
        let task_id = TaskId::new("T-1");
        let prompt = build_prompt(&task_id, "Fix bug", Path::new("/tmp/nonexistent"));

        assert!(
            prompt.contains("[patch_ready]"),
            "prompt must document patch_ready signal"
        );
        assert!(
            prompt.contains("[needs_human]"),
            "prompt must document needs_human signal"
        );
    }

    #[test]
    fn build_prompt_includes_section_separators() {
        let task_id = TaskId::new("T-1");
        let prompt = build_prompt(&task_id, "Task", Path::new("/tmp/nonexistent"));

        assert!(
            prompt.contains("# Task Assignment"),
            "prompt must have task assignment section"
        );
        assert!(
            prompt.contains("# Signals"),
            "prompt must have signals section"
        );
    }

    #[test]
    fn build_prompt_gracefully_handles_missing_context_dir() {
        // When repo root has no .othala/context/ directory, prompt still works.
        let task_id = TaskId::new("T-99");
        let prompt = build_prompt(&task_id, "Title", Path::new("/tmp/no-such-dir-12345"));

        assert!(prompt.contains("T-99"));
        assert!(prompt.contains("Title"));
    }

    // -----------------------------------------------------------------------
    // AgentSupervisor basic operations
    // -----------------------------------------------------------------------

    #[test]
    fn new_supervisor_starts_with_no_sessions() {
        let sup = AgentSupervisor::new(ModelKind::Claude);
        assert!(!sup.has_session(&TaskId::new("T-1")));
    }

    #[test]
    fn has_session_returns_false_for_unknown_task() {
        let sup = AgentSupervisor::new(ModelKind::Codex);
        assert!(!sup.has_session(&TaskId::new("nonexistent")));
    }

    #[test]
    fn poll_empty_supervisor_returns_empty_results() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);
        let result = sup.poll();

        assert!(result.output.is_empty());
        assert!(result.completed.is_empty());
    }

    #[test]
    fn stop_nonexistent_task_is_noop() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);
        // Should not panic or error.
        sup.stop(&TaskId::new("T-missing"));
    }

    #[test]
    fn stop_all_on_empty_supervisor_is_noop() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);
        sup.stop_all();
    }

    #[test]
    fn send_input_fails_for_missing_session() {
        let sup = AgentSupervisor::new(ModelKind::Claude);
        let err = sup
            .send_input(&TaskId::new("T-missing"), "hello")
            .expect_err("should fail for missing session");
        assert!(err.to_string().contains("no session"));
    }

    // -----------------------------------------------------------------------
    // AgentSupervisor with real child processes (using simple shell commands)
    // -----------------------------------------------------------------------

    #[test]
    fn poll_detects_completed_process_and_removes_session() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);
        let task_id = TaskId::new("T-poll-complete");

        // Manually insert a session with a simple process.
        let mut child = Command::new("echo")
            .arg("hello")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn echo");

        let (tx, rx) = mpsc::channel();
        pipe_child_output(&mut child, tx);

        let session = AgentSession {
            child,
            output_rx: rx,
            input_tx: None,
            task_id: task_id.clone(),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            patch_ready: false,
            needs_human: false,
            signal_at: None,
        };
        sup.sessions.insert(task_id.0.clone(), session);
        assert!(sup.has_session(&task_id));

        // Wait for the process to finish.
        std::thread::sleep(std::time::Duration::from_millis(200));

        let result = sup.poll();

        // Session should be completed and removed.
        assert!(!sup.has_session(&task_id));
        assert_eq!(result.completed.len(), 1);
        assert_eq!(result.completed[0].task_id, task_id);
        assert!(result.completed[0].success);
        assert_eq!(result.completed[0].exit_code, Some(0));
    }

    #[test]
    fn poll_captures_output_lines() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);
        let task_id = TaskId::new("T-output");

        let mut child = Command::new("echo")
            .arg("test output line")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn echo");

        let (tx, rx) = mpsc::channel();
        pipe_child_output(&mut child, tx);

        let session = AgentSession {
            child,
            output_rx: rx,
            input_tx: None,
            task_id: task_id.clone(),
            model: ModelKind::Codex,
            started_at: Utc::now(),
            patch_ready: false,
            needs_human: false,
            signal_at: None,
        };
        sup.sessions.insert(task_id.0.clone(), session);

        std::thread::sleep(std::time::Duration::from_millis(200));

        let result = sup.poll();

        // Should have captured output.
        let total_lines: usize = result.output.iter().map(|c| c.lines.len()).sum();
        assert!(total_lines >= 1, "expected at least one output line");
    }

    #[test]
    fn poll_detects_patch_ready_signal() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);
        let task_id = TaskId::new("T-patch-ready");

        let mut child = Command::new("echo")
            .arg("[patch_ready]")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn echo");

        let (tx, rx) = mpsc::channel();
        pipe_child_output(&mut child, tx);

        let session = AgentSession {
            child,
            output_rx: rx,
            input_tx: None,
            task_id: task_id.clone(),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            patch_ready: false,
            needs_human: false,
            signal_at: None,
        };
        sup.sessions.insert(task_id.0.clone(), session);

        std::thread::sleep(std::time::Duration::from_millis(200));

        let result = sup.poll();
        assert_eq!(result.completed.len(), 1);
        assert!(result.completed[0].patch_ready);
        assert!(result.completed[0].success);
    }

    #[test]
    fn poll_detects_needs_human_signal() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);
        let task_id = TaskId::new("T-needs-human");

        let mut child = Command::new("echo")
            .arg("[needs_human]")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn echo");

        let (tx, rx) = mpsc::channel();
        pipe_child_output(&mut child, tx);

        let session = AgentSession {
            child,
            output_rx: rx,
            input_tx: None,
            task_id: task_id.clone(),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            patch_ready: false,
            needs_human: false,
            signal_at: None,
        };
        sup.sessions.insert(task_id.0.clone(), session);

        std::thread::sleep(std::time::Duration::from_millis(200));

        let result = sup.poll();
        assert_eq!(result.completed.len(), 1);
        assert!(result.completed[0].needs_human);
    }

    #[test]
    fn stop_kills_running_session() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);
        let task_id = TaskId::new("T-stop");

        // Use `sleep` for a long-running process.
        let mut child = Command::new("sleep")
            .arg("60")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep");

        let (tx, rx) = mpsc::channel();
        pipe_child_output(&mut child, tx);

        let session = AgentSession {
            child,
            output_rx: rx,
            input_tx: None,
            task_id: task_id.clone(),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            patch_ready: false,
            needs_human: false,
            signal_at: None,
        };
        sup.sessions.insert(task_id.0.clone(), session);
        assert!(sup.has_session(&task_id));

        sup.stop(&task_id);
        assert!(!sup.has_session(&task_id));
    }

    #[test]
    fn stop_all_kills_multiple_sessions() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);

        for i in 0..3 {
            let task_id = TaskId::new(format!("T-stopall-{i}"));
            let mut child = Command::new("sleep")
                .arg("60")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn sleep");

            let (tx, rx) = mpsc::channel();
            pipe_child_output(&mut child, tx);

            let session = AgentSession {
                child,
                output_rx: rx,
                input_tx: None,
                task_id: task_id.clone(),
                model: ModelKind::Claude,
                started_at: Utc::now(),
                patch_ready: false,
                needs_human: false,
                signal_at: None,
            };
            sup.sessions.insert(task_id.0.clone(), session);
        }

        assert!(sup.has_session(&TaskId::new("T-stopall-0")));
        assert!(sup.has_session(&TaskId::new("T-stopall-1")));
        assert!(sup.has_session(&TaskId::new("T-stopall-2")));

        sup.stop_all();

        assert!(!sup.has_session(&TaskId::new("T-stopall-0")));
        assert!(!sup.has_session(&TaskId::new("T-stopall-1")));
        assert!(!sup.has_session(&TaskId::new("T-stopall-2")));
    }

    #[test]
    fn poll_reports_failure_for_nonzero_exit() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);
        let task_id = TaskId::new("T-fail");

        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exit 1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh");

        let (tx, rx) = mpsc::channel();
        pipe_child_output(&mut child, tx);

        let session = AgentSession {
            child,
            output_rx: rx,
            input_tx: None,
            task_id: task_id.clone(),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            patch_ready: false,
            needs_human: false,
            signal_at: None,
        };
        sup.sessions.insert(task_id.0.clone(), session);

        std::thread::sleep(std::time::Duration::from_millis(200));

        let result = sup.poll();
        assert_eq!(result.completed.len(), 1);
        assert!(!result.completed[0].success);
        assert_eq!(result.completed[0].exit_code, Some(1));
    }

    #[test]
    fn send_input_fails_for_non_interactive_session() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);
        let task_id = TaskId::new("T-nointeractive");

        let mut child = Command::new("sleep")
            .arg("60")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep");

        let (tx, rx) = mpsc::channel();
        pipe_child_output(&mut child, tx);

        let session = AgentSession {
            child,
            output_rx: rx,
            input_tx: None, // Not interactive
            task_id: task_id.clone(),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            patch_ready: false,
            needs_human: false,
            signal_at: None,
        };
        sup.sessions.insert(task_id.0.clone(), session);

        let err = sup
            .send_input(&task_id, "hello")
            .expect_err("should fail for non-interactive session");
        assert!(err.to_string().contains("not interactive"));

        sup.stop(&task_id);
    }

    #[test]
    fn poll_handles_multiple_sessions_simultaneously() {
        let mut sup = AgentSupervisor::new(ModelKind::Claude);

        // One fast task, one still running.
        let fast_id = TaskId::new("T-fast");
        let slow_id = TaskId::new("T-slow");

        let mut fast_child = Command::new("echo")
            .arg("done")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fast");
        let (fast_tx, fast_rx) = mpsc::channel();
        pipe_child_output(&mut fast_child, fast_tx);

        let mut slow_child = Command::new("sleep")
            .arg("60")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn slow");
        let (slow_tx, slow_rx) = mpsc::channel();
        pipe_child_output(&mut slow_child, slow_tx);

        sup.sessions.insert(
            fast_id.0.clone(),
            AgentSession {
                child: fast_child,
                output_rx: fast_rx,
                input_tx: None,
                task_id: fast_id.clone(),
                model: ModelKind::Claude,
                started_at: Utc::now(),
                patch_ready: false,
                needs_human: false,
                signal_at: None,
            },
        );
        sup.sessions.insert(
            slow_id.0.clone(),
            AgentSession {
                child: slow_child,
                output_rx: slow_rx,
                input_tx: None,
                task_id: slow_id.clone(),
                model: ModelKind::Codex,
                started_at: Utc::now(),
                patch_ready: false,
                needs_human: false,
                signal_at: None,
            },
        );

        std::thread::sleep(std::time::Duration::from_millis(200));

        let result = sup.poll();

        // Fast task should be completed, slow should still be running.
        assert_eq!(result.completed.len(), 1);
        assert_eq!(result.completed[0].task_id, fast_id);
        assert!(!sup.has_session(&fast_id));
        assert!(sup.has_session(&slow_id));

        // Cleanup.
        sup.stop_all();
    }

    // -----------------------------------------------------------------------
    // AgentOutcome and OutputChunk types
    // -----------------------------------------------------------------------

    #[test]
    fn agent_outcome_fields_are_accessible() {
        let outcome = AgentOutcome {
            task_id: TaskId::new("T-1"),
            model: ModelKind::Gemini,
            exit_code: Some(0),
            patch_ready: true,
            needs_human: false,
            success: true,
            duration_secs: 12,
        };
        assert_eq!(outcome.task_id.0, "T-1");
        assert_eq!(outcome.model, ModelKind::Gemini);
        assert_eq!(outcome.exit_code, Some(0));
        assert!(outcome.patch_ready);
        assert!(!outcome.needs_human);
        assert!(outcome.success);
        assert_eq!(outcome.duration_secs, 12);
    }

    #[test]
    fn output_chunk_collects_lines() {
        let chunk = OutputChunk {
            task_id: TaskId::new("T-1"),
            model: ModelKind::Claude,
            lines: vec!["line 1".to_string(), "line 2".to_string()],
        };
        assert_eq!(chunk.lines.len(), 2);
        assert_eq!(chunk.task_id.0, "T-1");
    }
}
