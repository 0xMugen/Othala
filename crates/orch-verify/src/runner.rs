//! MVP verify runner - runs a single verification command.

use std::path::Path;
use std::process::Command;
use std::time::Instant;
use std::{collections::VecDeque, thread};

use orch_core::config::RepoConfig;

use crate::error::VerifyError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyResult {
    pub success: bool,
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiVerifyResult {
    pub overall_success: bool,
    pub results: Vec<VerifyResult>,
    pub total_duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyStatus {
    Queued,
    Running,
    Passed,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct VerifyQueueEntry {
    pub task_id: String,
    pub status: VerifyStatus,
    pub queued_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub duration_ms: Option<u64>,
}

pub struct VerifyQueue {
    entries: Vec<VerifyQueueEntry>,
    max_concurrent: usize,
}

impl VerifyQueue {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_concurrent,
        }
    }

    pub fn enqueue(&mut self, task_id: String) -> bool {
        if self.entries.iter().any(|e| e.task_id == task_id) {
            return false;
        }
        self.entries.push(VerifyQueueEntry {
            task_id,
            status: VerifyStatus::Queued,
            queued_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            duration_ms: None,
        });
        true
    }

    pub fn can_start_more(&self) -> bool {
        let running = self
            .entries
            .iter()
            .filter(|e| e.status == VerifyStatus::Running)
            .count();
        running < self.max_concurrent
    }

    pub fn next_pending(&mut self) -> Option<&mut VerifyQueueEntry> {
        self.entries
            .iter_mut()
            .find(|e| e.status == VerifyStatus::Queued)
    }

    pub fn mark_running(&mut self, task_id: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.task_id == task_id) {
            entry.status = VerifyStatus::Running;
            entry.started_at = Some(chrono::Utc::now());
        }
    }

    pub fn mark_complete(
        &mut self,
        task_id: &str,
        passed: bool,
        reason: Option<String>,
        duration_ms: u64,
    ) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.task_id == task_id) {
            entry.status = if passed {
                VerifyStatus::Passed
            } else {
                VerifyStatus::Failed(reason.unwrap_or_default())
            };
            entry.completed_at = Some(chrono::Utc::now());
            entry.duration_ms = Some(duration_ms);
        }
    }

    pub fn running_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == VerifyStatus::Running)
            .count()
    }

    pub fn pending_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == VerifyStatus::Queued)
            .count()
    }

    pub fn completed_entries(&self) -> Vec<&VerifyQueueEntry> {
        self.entries
            .iter()
            .filter(|e| matches!(e.status, VerifyStatus::Passed | VerifyStatus::Failed(_)))
            .collect()
    }
}

/// Run the verification command for a repo.
pub fn run_verify(
    repo_config: &RepoConfig,
    worktree_path: &Path,
) -> Result<VerifyResult, VerifyError> {
    let command = &repo_config.verify.command;

    if command.trim().is_empty() {
        return Ok(VerifyResult {
            success: true,
            command: "(no verify command configured)".to_string(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            duration_ms: 0,
        });
    }

    let start = Instant::now();
    let output = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(worktree_path)
        .output()
        .map_err(|source| VerifyError::Io {
            command: command.clone(),
            source,
        })?;
    let duration_ms = start.elapsed().as_millis() as u64;

    let stdout = String::from_utf8(output.stdout).map_err(|source| VerifyError::NonUtf8Output {
        command: command.clone(),
        stream: "stdout",
        source,
    })?;

    let stderr = String::from_utf8(output.stderr).map_err(|source| VerifyError::NonUtf8Output {
        command: command.clone(),
        stream: "stderr",
        source,
    })?;

    Ok(VerifyResult {
        success: output.status.success(),
        command: command.clone(),
        stdout,
        stderr,
        exit_code: output.status.code(),
        duration_ms,
    })
}

pub fn run_multi_verify(
    commands: &[String],
    worktree_path: &Path,
) -> Result<MultiVerifyResult, VerifyError> {
    let total_start = Instant::now();
    let mut results = Vec::new();
    let mut overall_success = true;

    for cmd in commands {
        let config = RepoConfig {
            repo_id: String::new(),
            repo_path: worktree_path.to_path_buf(),
            base_branch: String::new(),
            nix: orch_core::config::NixConfig {
                dev_shell: String::new(),
            },
            verify: orch_core::config::VerifyConfig {
                command: cmd.clone(),
            },
            graphite: orch_core::config::RepoGraphiteConfig {
                draft_on_start: false,
                submit_mode: None,
            },
        };

        let result = run_verify(&config, worktree_path)?;
        if !result.success {
            overall_success = false;
            results.push(result);
            break;
        }
        results.push(result);
    }

    Ok(MultiVerifyResult {
        overall_success,
        results,
        total_duration_ms: total_start.elapsed().as_millis() as u64,
    })
}

pub fn run_multi_verify_parallel(
    commands: &[String],
    worktree_path: &Path,
    max_concurrent: usize,
) -> Result<MultiVerifyResult, VerifyError> {
    let total_start = Instant::now();
    let mut queue = VerifyQueue::new(max_concurrent.max(1));
    let mut pending = VecDeque::new();

    for (idx, _) in commands.iter().enumerate() {
        let task_id = format!("verify-{idx}");
        let _ = queue.enqueue(task_id.clone());
        pending.push_back((idx, task_id));
    }

    type VerifyJoinResult = (usize, String, Result<VerifyResult, VerifyError>, u64);
    let mut running: Vec<thread::JoinHandle<VerifyJoinResult>> = Vec::new();
    let mut ordered_results: Vec<Option<VerifyResult>> = vec![None; commands.len()];
    let mut overall_success = true;
    let mut first_error: Option<VerifyError> = None;

    while !pending.is_empty() || !running.is_empty() {
        while queue.can_start_more() {
            let Some((idx, task_id)) = pending.pop_front() else {
                break;
            };
            queue.mark_running(&task_id);

            let command = commands[idx].clone();
            let worktree = worktree_path.to_path_buf();
            running.push(thread::spawn(move || {
                let verify_start = Instant::now();
                let config = RepoConfig {
                    repo_id: String::new(),
                    repo_path: worktree.clone(),
                    base_branch: String::new(),
                    nix: orch_core::config::NixConfig {
                        dev_shell: String::new(),
                    },
                    verify: orch_core::config::VerifyConfig { command },
                    graphite: orch_core::config::RepoGraphiteConfig {
                        draft_on_start: false,
                        submit_mode: None,
                    },
                };
                let result = run_verify(&config, &worktree);
                let elapsed_ms = verify_start.elapsed().as_millis() as u64;
                (idx, task_id, result, elapsed_ms)
            }));
        }

        if running.is_empty() {
            continue;
        }

        let handle = running.swap_remove(0);
        let join_result = handle.join().map_err(|_| VerifyError::InvalidConfig {
            message: "parallel verify worker panicked".to_string(),
        })?;

        let (idx, task_id, verify_result, elapsed_ms) = join_result;
        match verify_result {
            Ok(result) => {
                let failure_reason = if result.success {
                    None
                } else if result.stderr.trim().is_empty() {
                    Some(result.stdout.clone())
                } else {
                    Some(result.stderr.clone())
                };
                queue.mark_complete(&task_id, result.success, failure_reason, elapsed_ms);
                overall_success &= result.success;
                ordered_results[idx] = Some(result);
            }
            Err(err) => {
                queue.mark_complete(&task_id, false, Some(err.to_string()), elapsed_ms);
                overall_success = false;
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }

    Ok(MultiVerifyResult {
        overall_success,
        results: ordered_results.into_iter().flatten().collect(),
        total_duration_ms: total_start.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::config::{NixConfig, RepoGraphiteConfig, VerifyConfig};
    use orch_core::types::SubmitMode;
    use std::path::PathBuf;

    fn mk_repo_config(verify_command: &str) -> RepoConfig {
        RepoConfig {
            repo_id: "test".to_string(),
            repo_path: PathBuf::from("/tmp/test"),
            base_branch: "main".to_string(),
            nix: NixConfig {
                dev_shell: "nix develop".to_string(),
            },
            verify: VerifyConfig {
                command: verify_command.to_string(),
            },
            graphite: RepoGraphiteConfig {
                draft_on_start: false,
                submit_mode: Some(SubmitMode::Single),
            },
        }
    }

    #[test]
    fn run_verify_succeeds_with_true() {
        let config = mk_repo_config("true");
        let result = run_verify(&config, Path::new("/tmp")).expect("run verify");
        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn run_verify_fails_with_false() {
        let config = mk_repo_config("false");
        let result = run_verify(&config, Path::new("/tmp")).expect("run verify");
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(1));
    }

    #[test]
    fn run_verify_captures_output() {
        let config = mk_repo_config("echo hello && echo world >&2");
        let result = run_verify(&config, Path::new("/tmp")).expect("run verify");
        assert!(result.success);
        assert!(result.stdout.contains("hello"));
        assert!(result.stderr.contains("world"));
    }

    #[test]
    fn run_verify_passes_with_empty_command() {
        let config = mk_repo_config("");
        let result = run_verify(&config, Path::new("/tmp")).expect("run verify");
        assert!(result.success);
        assert_eq!(result.duration_ms, 0);
    }

    #[test]
    fn run_verify_includes_duration() {
        let config = mk_repo_config("true");
        let result = run_verify(&config, Path::new("/tmp")).expect("run verify");
        assert!(result.success);
        assert!(result.duration_ms < 5_000);
    }

    #[test]
    fn multi_verify_runs_all_on_success() {
        let commands = vec!["true".to_string(), "echo ok".to_string()];
        let result =
            super::run_multi_verify(&commands, Path::new("/tmp")).expect("run multi verify");
        assert!(result.overall_success);
        assert_eq!(result.results.len(), 2);
        assert!(result.results[0].success);
        assert!(result.results[1].success);
    }

    #[test]
    fn multi_verify_stops_on_first_failure() {
        let commands = vec![
            "true".to_string(),
            "false".to_string(),
            "echo should_not_run".to_string(),
        ];
        let result =
            super::run_multi_verify(&commands, Path::new("/tmp")).expect("run multi verify");
        assert!(!result.overall_success);
        assert_eq!(result.results.len(), 2);
        assert!(result.results[0].success);
        assert!(!result.results[1].success);
    }

    #[test]
    fn multi_verify_empty_commands_succeeds() {
        let commands: Vec<String> = vec![];
        let result =
            super::run_multi_verify(&commands, Path::new("/tmp")).expect("run multi verify");
        assert!(result.overall_success);
        assert!(result.results.is_empty());
    }

    #[test]
    fn verify_queue_enqueue_and_dequeue() {
        let mut queue = VerifyQueue::new(2);
        assert!(queue.enqueue("task-1".to_string()));
        assert!(queue.enqueue("task-2".to_string()));
        assert_eq!(queue.pending_count(), 2);

        let pending = queue.next_pending().expect("pending entry");
        assert_eq!(pending.task_id, "task-1");

        queue.mark_running("task-1");
        assert_eq!(queue.running_count(), 1);
        assert_eq!(queue.pending_count(), 1);

        queue.mark_complete("task-1", true, None, 10);
        assert_eq!(queue.running_count(), 0);
        assert_eq!(queue.completed_entries().len(), 1);
    }

    #[test]
    fn verify_queue_respects_concurrency() {
        let mut queue = VerifyQueue::new(2);
        assert!(queue.enqueue("task-1".to_string()));
        assert!(queue.enqueue("task-2".to_string()));
        assert!(queue.enqueue("task-3".to_string()));

        assert!(queue.can_start_more());
        queue.mark_running("task-1");
        assert!(queue.can_start_more());
        queue.mark_running("task-2");
        assert!(!queue.can_start_more());

        queue.mark_complete("task-1", true, None, 12);
        assert!(queue.can_start_more());
    }

    #[test]
    fn verify_queue_prevents_duplicates() {
        let mut queue = VerifyQueue::new(1);
        assert!(queue.enqueue("task-1".to_string()));
        assert!(!queue.enqueue("task-1".to_string()));
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn verify_queue_tracks_timing() {
        let mut queue = VerifyQueue::new(1);
        assert!(queue.enqueue("task-1".to_string()));

        queue.mark_running("task-1");
        let started_at = queue.entries[0].started_at.clone();
        assert!(started_at.is_some());

        queue.mark_complete("task-1", false, Some("failed".to_string()), 33);
        let entry = &queue.entries[0];
        assert!(entry.completed_at.is_some());
        assert_eq!(entry.duration_ms, Some(33));
        assert_eq!(entry.status, VerifyStatus::Failed("failed".to_string()));
    }

    #[test]
    fn verify_queue_completed_entries() {
        let mut queue = VerifyQueue::new(3);
        assert!(queue.enqueue("task-1".to_string()));
        assert!(queue.enqueue("task-2".to_string()));
        assert!(queue.enqueue("task-3".to_string()));

        queue.mark_running("task-1");
        queue.mark_complete("task-1", true, None, 20);
        queue.mark_running("task-2");
        queue.mark_complete("task-2", false, Some("err".to_string()), 25);

        let completed = queue.completed_entries();
        assert_eq!(completed.len(), 2);
        assert!(completed.iter().any(|entry| entry.task_id == "task-1"));
        assert!(completed.iter().any(|entry| entry.task_id == "task-2"));
        assert!(completed.iter().all(|entry| entry.task_id != "task-3"));
    }
}
