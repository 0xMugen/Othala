use chrono::Utc;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::adapter::AgentAdapter;
use crate::error::AgentError;
use crate::types::{
    AgentSignal, AgentSignalKind, EpochRequest, EpochResult, EpochStopReason, PtyChunk,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerPtySize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for RunnerPtySize {
    fn default() -> Self {
        Self {
            rows: 40,
            cols: 120,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochRunner {
    pub shell_bin: String,
    pub pty_size: RunnerPtySize,
    pub poll_interval: Duration,
}

impl Default for EpochRunner {
    fn default() -> Self {
        Self {
            shell_bin: "bash".to_string(),
            pty_size: RunnerPtySize::default(),
            poll_interval: Duration::from_millis(50),
        }
    }
}

impl EpochRunner {
    pub fn run_epoch(
        &self,
        request: &EpochRequest,
        adapter: &dyn AgentAdapter,
    ) -> Result<EpochResult, AgentError> {
        if request.timeout_secs == 0 {
            return Err(AgentError::InvalidRequest {
                message: "timeout_secs must be greater than zero".to_string(),
            });
        }
        if request.prompt.trim().is_empty() {
            return Err(AgentError::InvalidRequest {
                message: "prompt must not be empty".to_string(),
            });
        }

        let started_at = Utc::now();
        let timeout = Duration::from_secs(request.timeout_secs);
        let deadline = Instant::now() + timeout;

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: self.pty_size.rows,
                cols: self.pty_size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| AgentError::PtySetup {
                message: err.to_string(),
            })?;

        let agent_command = adapter.build_command(request);
        let shell_invocation = render_shell_invocation(&request.repo_path, &agent_command);

        let mut command = CommandBuilder::new(self.shell_bin.clone());
        command.arg("-lc");
        command.arg(shell_invocation);

        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| AgentError::Spawn {
                message: err.to_string(),
            })?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| AgentError::PtySetup {
                message: err.to_string(),
            })?;
        let (tx, rx) = mpsc::channel::<String>();
        let reader_handle = thread::spawn(move || {
            let mut buf = BufReader::new(reader);
            loop {
                let mut line = String::new();
                match buf.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let _ = tx.send(line);
                    }
                    Err(_) => break,
                }
            }
        });

        let mut output = Vec::<PtyChunk>::new();
        let mut signals = Vec::<AgentSignal>::new();
        let mut stop_reason: Option<EpochStopReason> = None;
        let mut wait_status = None;

        while stop_reason.is_none() {
            drain_output(&rx, adapter, &mut output, &mut signals, &mut stop_reason);
            if stop_reason.is_some() {
                let _ = child.kill();
                break;
            }

            if Instant::now() >= deadline {
                stop_reason = Some(EpochStopReason::Timeout);
                let _ = child.kill();
                break;
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    wait_status = Some(status);
                    break;
                }
                Ok(None) => {}
                Err(err) => {
                    return Err(AgentError::Runtime {
                        message: err.to_string(),
                    });
                }
            }

            thread::sleep(self.poll_interval);
        }

        let final_status = match wait_status {
            Some(status) => status,
            None => child.wait().map_err(|err| AgentError::Runtime {
                message: err.to_string(),
            })?,
        };
        let exit_code = i32::try_from(final_status.exit_code()).ok();

        let _ = reader_handle.join();
        drain_output(&rx, adapter, &mut output, &mut signals, &mut stop_reason);

        let final_reason = stop_reason.unwrap_or_else(|| {
            if final_status.success() {
                EpochStopReason::Completed
            } else {
                EpochStopReason::Failed
            }
        });

        Ok(EpochResult {
            task_id: request.task_id.clone(),
            repo_id: request.repo_id.clone(),
            model: request.model,
            started_at,
            finished_at: Utc::now(),
            stop_reason: final_reason,
            exit_code,
            output,
            signals,
        })
    }
}

fn drain_output(
    rx: &mpsc::Receiver<String>,
    adapter: &dyn AgentAdapter,
    output: &mut Vec<PtyChunk>,
    signals: &mut Vec<AgentSignal>,
    stop_reason: &mut Option<EpochStopReason>,
) {
    while let Ok(line) = rx.try_recv() {
        output.push(PtyChunk {
            at: Utc::now(),
            text: line.clone(),
        });

        if let Some(signal) = adapter.detect_signal(&line) {
            if stop_reason.is_none() {
                *stop_reason = signal_to_stop_reason(signal.kind);
            }
            signals.push(signal);
        }
    }
}

fn signal_to_stop_reason(kind: AgentSignalKind) -> Option<EpochStopReason> {
    match kind {
        AgentSignalKind::NeedHuman => Some(EpochStopReason::NeedHuman),
        AgentSignalKind::PatchReady => Some(EpochStopReason::PatchReady),
        AgentSignalKind::ConflictResolved => None,
        AgentSignalKind::RateLimited => Some(EpochStopReason::RateLimited),
        AgentSignalKind::ErrorHint => None,
    }
}

fn render_shell_invocation(
    repo_path: &std::path::Path,
    command: &crate::types::AgentCommand,
) -> String {
    let mut rendered = String::new();
    rendered.push_str("cd ");
    rendered.push_str(&shell_quote(&repo_path.display().to_string()));
    rendered.push_str(" && ");

    for (key, value) in &command.env {
        if key.trim().is_empty() {
            continue;
        }
        rendered.push_str(key);
        rendered.push('=');
        rendered.push_str(&shell_quote(value));
        rendered.push(' ');
    }

    rendered.push_str(&shell_quote(&command.executable));
    for arg in &command.args {
        rendered.push(' ');
        rendered.push_str(&shell_quote(arg));
    }
    rendered
}

fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use orch_core::types::{ModelKind, RepoId, TaskId};

    use crate::adapter::ClaudeAdapter;
    use crate::error::AgentError;
    use crate::types::{AgentCommand, AgentSignalKind, EpochRequest, EpochStopReason};

    use super::{render_shell_invocation, shell_quote, signal_to_stop_reason, EpochRunner};

    fn mk_request() -> EpochRequest {
        EpochRequest {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Claude,
            repo_path: PathBuf::from("/tmp/repo"),
            prompt: "do work".to_string(),
            timeout_secs: 30,
            extra_args: vec!["--json".to_string()],
            env: vec![("FOO".to_string(), "BAR".to_string())],
        }
    }

    #[test]
    fn signal_to_stop_reason_maps_supported_signals() {
        assert_eq!(
            signal_to_stop_reason(AgentSignalKind::NeedHuman),
            Some(EpochStopReason::NeedHuman)
        );
        assert_eq!(
            signal_to_stop_reason(AgentSignalKind::PatchReady),
            Some(EpochStopReason::PatchReady)
        );
        assert_eq!(
            signal_to_stop_reason(AgentSignalKind::RateLimited),
            Some(EpochStopReason::RateLimited)
        );
        assert_eq!(signal_to_stop_reason(AgentSignalKind::ErrorHint), None);
    }

    #[test]
    fn shell_quote_wraps_and_escapes_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("O'Reilly"), "'O'\"'\"'Reilly'");
    }

    #[test]
    fn render_shell_invocation_renders_cd_env_and_command() {
        let repo_path = PathBuf::from("/tmp/repo path");
        let command = AgentCommand {
            executable: "codex".to_string(),
            args: vec!["--flag".to_string(), "it's".to_string()],
            env: vec![
                ("FOO".to_string(), "BAR".to_string()),
                ("".to_string(), "SKIP".to_string()),
                ("A_B".to_string(), "x y".to_string()),
            ],
        };

        let rendered = render_shell_invocation(&repo_path, &command);
        assert!(rendered.starts_with("cd '/tmp/repo path' && "));
        assert!(rendered.contains("FOO='BAR' "));
        assert!(rendered.contains("A_B='x y' "));
        assert!(!rendered.contains("SKIP"));
        assert!(rendered.contains("'codex' '--flag' 'it'\"'\"'s'"));
    }

    #[test]
    fn run_epoch_rejects_zero_timeout_before_spawning() {
        let mut request = mk_request();
        request.timeout_secs = 0;

        let runner = EpochRunner::default();
        let adapter = ClaudeAdapter::default();
        let err = runner
            .run_epoch(&request, &adapter)
            .expect_err("zero timeout must fail");

        assert!(matches!(
            err,
            AgentError::InvalidRequest { message } if message.contains("timeout_secs")
        ));
    }

    #[test]
    fn run_epoch_rejects_empty_prompt_before_spawning() {
        let mut request = mk_request();
        request.prompt = "   ".to_string();

        let runner = EpochRunner::default();
        let adapter = ClaudeAdapter::default();
        let err = runner
            .run_epoch(&request, &adapter)
            .expect_err("empty prompt must fail");

        assert!(matches!(
            err,
            AgentError::InvalidRequest { message } if message.contains("prompt")
        ));
    }
}
