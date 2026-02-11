//! Agent supervisor — spawns and monitors AI agent processes.

use chrono::{DateTime, Utc};
use orch_agents::{
    default_adapter_for, detect_common_signal, AgentAdapter, AgentSignalKind, EpochRequest,
};
use orch_core::types::{ModelKind, RepoId, TaskId};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

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
            prompt: build_prompt(task_id, prompt),
            timeout_secs: 600,
            extra_args: vec![],
            env: vec![],
        };

        let cmd = adapter.build_command(&request);

        let mut child = Command::new(&cmd.executable)
            .args(&cmd.args)
            .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(repo_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let (tx, rx) = mpsc::channel();

        // Pipe stdout in a background thread.
        if let Some(stdout) = child.stdout.take() {
            let tx_out = tx.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    if let Ok(line) = line {
                        let _ = tx_out.send(line);
                    }
                }
            });
        }

        // Pipe stderr in a background thread.
        if let Some(stderr) = child.stderr.take() {
            let tx_err = tx;
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    if let Ok(line) = line {
                        let _ = tx_err.send(line);
                    }
                }
            });
        }

        let session = AgentSession {
            child,
            output_rx: rx,
            input_tx: None,
            task_id: task_id.clone(),
            model,
            started_at: Utc::now(),
            patch_ready: false,
            needs_human: false,
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
            prompt: build_prompt(task_id, initial_prompt),
            timeout_secs: 600,
            extra_args: vec![],
            env: vec![],
        };

        let cmd = adapter.build_interactive_command(&request);

        let mut child = Command::new(&cmd.executable)
            .args(&cmd.args)
            .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(repo_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let (out_tx, out_rx) = mpsc::channel();

        // Pipe stdout in a background thread.
        if let Some(stdout) = child.stdout.take() {
            let tx_out = out_tx.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    if let Ok(line) = line {
                        let _ = tx_out.send(line);
                    }
                }
            });
        }

        // Pipe stderr in a background thread.
        if let Some(stderr) = child.stderr.take() {
            let tx_err = out_tx;
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    if let Ok(line) = line {
                        let _ = tx_err.send(line);
                    }
                }
            });
        }

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
                        AgentSignalKind::PatchReady => session.patch_ready = true,
                        AgentSignalKind::NeedHuman => session.needs_human = true,
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

            // Check if process has exited.
            match session.child.try_wait() {
                Ok(Some(status)) => {
                    let exit_code = status.code();
                    let success = session.patch_ready || exit_code == Some(0);
                    completed.push(AgentOutcome {
                        task_id: session.task_id.clone(),
                        model: session.model,
                        exit_code,
                        patch_ready: session.patch_ready,
                        needs_human: session.needs_human,
                        success,
                    });
                    finished_keys.push(key.clone());
                }
                Ok(None) => {} // still running
                Err(_) => {
                    completed.push(AgentOutcome {
                        task_id: session.task_id.clone(),
                        model: session.model,
                        exit_code: None,
                        patch_ready: false,
                        needs_human: false,
                        success: false,
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
pub fn build_prompt(task_id: &TaskId, title: &str) -> String {
    format!(
        "Task {task_id}: {title}\n\n\
         Instructions:\n\
         - Complete the task described above.\n\
         - When you are done and the code is ready, print exactly: [patch_ready]\n\
         - If you are blocked and need human help, print exactly: [needs_human]\n",
        task_id = task_id.0,
        title = title,
    )
}
