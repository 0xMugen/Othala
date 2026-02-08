use chrono::Utc;
use orch_verify::prepare_verify_command;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::error::WebError;
use crate::model::{
    SandboxCommandLog, SandboxRunView, SandboxSpawnRequest, SandboxSpawnResponse, SandboxStatus,
};
use crate::state::WebState;

pub async fn spawn_sandbox_run(
    state: WebState,
    request: SandboxSpawnRequest,
) -> Result<SandboxSpawnResponse, WebError> {
    if request.verify_full_commands.is_empty() {
        return Err(WebError::BadRequest {
            message: "verify_full_commands must not be empty".to_string(),
        });
    }
    if request.nix_dev_shell.trim().is_empty() {
        return Err(WebError::BadRequest {
            message: "nix_dev_shell must not be empty".to_string(),
        });
    }

    let sandbox_id = state.next_sandbox_id();
    let sandbox_path = sandbox_worktree_path(&request.repo_path, &sandbox_id);

    let run = SandboxRunView {
        sandbox_id: sandbox_id.clone(),
        target: request.target.clone(),
        status: SandboxStatus::Queued,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        sandbox_path: Some(sandbox_path),
        checkout_ref: request.checkout_ref.clone(),
        cleanup_worktree: request.cleanup_worktree,
        worktree_cleaned: false,
        worktree_cleanup_error: None,
        logs: Vec::new(),
        last_error: None,
    };
    state.upsert_sandbox(run).await;

    let worker_state = state.clone();
    let worker_sandbox_id = sandbox_id.clone();
    tokio::spawn(async move {
        execute_sandbox(worker_state, worker_sandbox_id, request).await;
    });

    Ok(SandboxSpawnResponse {
        sandbox_id,
        status: SandboxStatus::Queued,
    })
}

async fn execute_sandbox(state: WebState, sandbox_id: String, request: SandboxSpawnRequest) {
    let sandbox_path = sandbox_worktree_path(&request.repo_path, &sandbox_id);
    let checkout_ref = requested_checkout_ref(&request);

    let _ = state
        .update_sandbox(&sandbox_id, |run| {
            run.status = SandboxStatus::Running;
            run.last_error = None;
            run.worktree_cleanup_error = None;
            run.sandbox_path = Some(sandbox_path.clone());
            run.checkout_ref = Some(checkout_ref.clone());
        })
        .await;

    let add_result = create_sandbox_worktree(&request.repo_path, &sandbox_path, &checkout_ref).await;
    let add_log = to_command_log("git worktree add", &add_result.command_line, &add_result);
    let _ = state
        .update_sandbox(&sandbox_id, |run| {
            run.logs.push(add_log.clone());
            if !add_result.success {
                run.last_error = Some(format!(
                    "failed to create sandbox worktree (exit={:?})",
                    add_result.exit_code
                ));
                run.status = SandboxStatus::Failed;
            }
        })
        .await;

    if !add_result.success {
        return;
    }

    let mut failed = false;
    for command in &request.verify_full_commands {
        let prepared = prepare_verify_command(&request.nix_dev_shell, command);
        let result = run_one_command(&sandbox_path, &prepared.effective).await;

        let log = SandboxCommandLog {
            command: command.clone(),
            effective_command: prepared.effective.clone(),
            started_at: result.started_at,
            finished_at: result.finished_at,
            success: result.success,
            exit_code: result.exit_code,
            stdout: result.stdout.clone(),
            stderr: result.stderr.clone(),
        };

        let error_message = if result.success {
            None
        } else {
            Some(format!(
                "sandbox command failed (exit={:?}): {}",
                result.exit_code, prepared.effective
            ))
        };

        let _ = state
            .update_sandbox(&sandbox_id, |run| {
                run.logs.push(log.clone());
                if let Some(message) = &error_message {
                    run.last_error = Some(message.clone());
                }
            })
            .await;

        if !result.success {
            failed = true;
            break;
        }
    }

    let final_status = if failed {
        SandboxStatus::Failed
    } else {
        SandboxStatus::Passed
    };

    let mut cleanup_error = None;
    let mut cleaned = false;
    if request.cleanup_worktree {
        let cleanup = remove_sandbox_worktree(&request.repo_path, &sandbox_path).await;
        cleaned = cleanup.success;
        let cleanup_log = to_command_log("git worktree remove", &cleanup.command_line, &cleanup);
        if !cleanup.success {
            cleanup_error = Some(format!(
                "failed to clean sandbox worktree (exit={:?})",
                cleanup.exit_code
            ));
        }
        let _ = state
            .update_sandbox(&sandbox_id, |run| {
                run.logs.push(cleanup_log.clone());
            })
            .await;
    }

    let _ = state
        .update_sandbox(&sandbox_id, |run| {
            run.status = final_status.clone();
            run.worktree_cleaned = cleaned;
            run.worktree_cleanup_error = cleanup_error.clone();
            if run.last_error.is_none() {
                run.last_error = cleanup_error.clone();
            }
        })
        .await;
}

struct CommandResult {
    command_line: String,
    started_at: chrono::DateTime<Utc>,
    finished_at: chrono::DateTime<Utc>,
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

async fn run_one_command(repo_path: &Path, command_line: &str) -> CommandResult {
    let started_at = Utc::now();
    let outcome = Command::new("bash")
        .arg("-lc")
        .arg(command_line)
        .current_dir(repo_path)
        .output()
        .await;

    match outcome {
        Ok(output) => CommandResult {
            command_line: command_line.to_string(),
            started_at,
            finished_at: Utc::now(),
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(err) => CommandResult {
            command_line: command_line.to_string(),
            started_at,
            finished_at: Utc::now(),
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: err.to_string(),
        },
    }
}

async fn create_sandbox_worktree(repo_path: &Path, sandbox_path: &Path, checkout_ref: &str) -> CommandResult {
    let parent = sandbox_path.parent().map(Path::to_path_buf);
    if let Some(parent_dir) = parent {
        if let Err(err) = fs::create_dir_all(parent_dir) {
            return CommandResult {
                command_line: format!("mkdir -p {}", sandbox_path.display()),
                started_at: Utc::now(),
                finished_at: Utc::now(),
                success: false,
                exit_code: None,
                stdout: String::new(),
                stderr: err.to_string(),
            };
        }
    }

    run_git_command(repo_path, worktree_add_args(sandbox_path, checkout_ref)).await
}

async fn remove_sandbox_worktree(repo_path: &Path, sandbox_path: &Path) -> CommandResult {
    run_git_command(repo_path, worktree_remove_args(sandbox_path)).await
}

async fn run_git_command(repo_path: &Path, args: Vec<String>) -> CommandResult {
    let started_at = Utc::now();
    let command_line = format!("git {}", args.join(" "));
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_path);
    for arg in &args {
        cmd.arg(arg);
    }

    let outcome = cmd.output().await;
    match outcome {
        Ok(output) => CommandResult {
            command_line,
            started_at,
            finished_at: Utc::now(),
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(err) => CommandResult {
            command_line,
            started_at,
            finished_at: Utc::now(),
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: err.to_string(),
        },
    }
}

fn to_command_log(command: &str, effective_command: &str, result: &CommandResult) -> SandboxCommandLog {
    SandboxCommandLog {
        command: command.to_string(),
        effective_command: effective_command.to_string(),
        started_at: result.started_at,
        finished_at: result.finished_at,
        success: result.success,
        exit_code: result.exit_code,
        stdout: result.stdout.clone(),
        stderr: result.stderr.clone(),
    }
}

fn sandbox_worktree_path(repo_path: &Path, sandbox_id: &str) -> PathBuf {
    repo_path.join(".orch").join("sandbox").join(sandbox_id)
}

fn requested_checkout_ref(request: &SandboxSpawnRequest) -> String {
    request
        .checkout_ref
        .as_ref()
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .unwrap_or_else(|| "HEAD".to_string())
}

fn worktree_add_args(sandbox_path: &Path, checkout_ref: &str) -> Vec<String> {
    vec![
        "worktree".to_string(),
        "add".to_string(),
        "--detach".to_string(),
        sandbox_path.display().to_string(),
        checkout_ref.to_string(),
    ]
}

fn worktree_remove_args(sandbox_path: &Path) -> Vec<String> {
    vec![
        "worktree".to_string(),
        "remove".to_string(),
        "--force".to_string(),
        sandbox_path.display().to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::model::{SandboxSpawnRequest, SandboxTarget};

    use super::{requested_checkout_ref, sandbox_worktree_path, worktree_add_args, worktree_remove_args};

    #[test]
    fn defaults_checkout_ref_to_head() {
        let request = SandboxSpawnRequest {
            target: SandboxTarget::Task {
                task_id: "T1".to_string(),
            },
            repo_path: "/tmp/repo".into(),
            nix_dev_shell: "nix develop".to_string(),
            verify_full_commands: vec!["echo ok".to_string()],
            checkout_ref: None,
            cleanup_worktree: true,
        };
        assert_eq!(requested_checkout_ref(&request), "HEAD");
    }

    #[test]
    fn uses_checkout_ref_when_provided() {
        let request = SandboxSpawnRequest {
            target: SandboxTarget::Task {
                task_id: "T1".to_string(),
            },
            repo_path: "/tmp/repo".into(),
            nix_dev_shell: "nix develop".to_string(),
            verify_full_commands: vec!["echo ok".to_string()],
            checkout_ref: Some("feature/branch".to_string()),
            cleanup_worktree: true,
        };
        assert_eq!(requested_checkout_ref(&request), "feature/branch");
    }

    #[test]
    fn sandbox_path_is_under_orch_sandbox_directory() {
        let path = sandbox_worktree_path(Path::new("/repo"), "SBX-1");
        assert_eq!(path, Path::new("/repo/.orch/sandbox/SBX-1"));
    }

    #[test]
    fn git_worktree_args_are_constructed_correctly() {
        let add = worktree_add_args(Path::new("/repo/.orch/sandbox/SBX-1"), "HEAD");
        assert_eq!(
            add,
            vec![
                "worktree".to_string(),
                "add".to_string(),
                "--detach".to_string(),
                "/repo/.orch/sandbox/SBX-1".to_string(),
                "HEAD".to_string(),
            ]
        );

        let rm = worktree_remove_args(Path::new("/repo/.orch/sandbox/SBX-1"));
        assert_eq!(
            rm,
            vec![
                "worktree".to_string(),
                "remove".to_string(),
                "--force".to_string(),
                "/repo/.orch/sandbox/SBX-1".to_string(),
            ]
        );
    }
}
