use anyhow::{anyhow, Context, Result};
use orch_core::types::TaskId;
use orch_git::{current_branch, discover_repo, GitCli, RepoHandle, WorktreeManager, WorktreeSpec};
use orch_graphite::GraphiteClient;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatWorkspace {
    pub branch_name: String,
    pub worktree_path: PathBuf,
}

pub fn provision_chat_workspace(start_path: &Path, task_id: &TaskId) -> Result<ChatWorkspace> {
    let git = GitCli::default();
    let repo = discover_repo(start_path, &git).with_context(|| {
        format!(
            "failed to discover git repository from {}",
            start_path.display()
        )
    })?;

    let base_branch =
        current_branch(&repo, &git).context("failed to read current branch")?;
    let branch_name = branch_name_for_task(task_id);
    let commit_message = format!("start {}", task_id.0);

    provision_inner(&git, &repo, &base_branch, &branch_name, &commit_message, task_id)
}

fn provision_inner(
    git: &GitCli,
    repo: &RepoHandle,
    base_branch: &str,
    branch_name: &str,
    commit_message: &str,
    task_id: &TaskId,
) -> Result<ChatWorkspace> {
    // Create worktree + branch in one step.  This runs
    //   git worktree add -b <branch> <path>
    // which never touches the main worktree's HEAD, so editors that watch
    // .git/HEAD (VSCode, etc.) won't see a branch switch.
    let manager = WorktreeManager::default();
    let spec = WorktreeSpec {
        task_id: task_id.clone(),
        branch: branch_name.to_string(),
    };

    let info = match manager.create_with_new_branch(repo, &spec) {
        Ok(info) => info,
        Err(err) => {
            return Err(anyhow!(err)).context("failed to create task worktree");
        }
    };

    // Make an initial empty commit in the worktree so graphite has something
    // to track and the branch diverges from its parent.
    if let Err(e) = git.run(
        &info.path,
        [
            "-c", "user.name=Othala",
            "-c", "user.email=othala@localhost",
            "commit", "--allow-empty", "-m", commit_message,
        ],
    ) {
        // Clean up on failure.
        let _ = manager.remove(repo, task_id, true);
        return Err(anyhow!(e)).context("failed to create initial commit in worktree");
    }

    // Register the branch with Graphite so `gt submit` works later.
    // Run from the worktree directory so graphite sees the correct branch.
    let wt_graphite = GraphiteClient::new(info.path.clone());
    if let Err(e) = wt_graphite.track_branch(branch_name, base_branch) {
        // Non-fatal: graphite tracking can be done manually later, and the
        // worktree is already usable for the agent.
        eprintln!(
            "warning: failed to register {branch_name} with graphite: {e}; \
             submit may require manual `gt track`"
        );
    }

    Ok(ChatWorkspace {
        branch_name: branch_name.to_string(),
        worktree_path: info.path,
    })
}

pub fn branch_name_for_task(task_id: &TaskId) -> String {
    let sanitized = sanitize_branch_component(&task_id.0);
    format!("task/{sanitized}")
}

fn sanitize_branch_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }

    let trimmed = out.trim_matches(['-', '.']);
    if trimmed.is_empty() {
        "chat".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::branch_name_for_task;
    use orch_core::types::TaskId;

    #[test]
    fn branch_name_uses_task_prefix() {
        assert_eq!(
            branch_name_for_task(&TaskId::new("chat-123")),
            "task/chat-123"
        );
    }

    #[test]
    fn branch_name_sanitizes_invalid_characters() {
        assert_eq!(
            branch_name_for_task(&TaskId::new("chat with spaces/#42")),
            "task/chat-with-spaces--42"
        );
    }

    #[test]
    fn branch_name_falls_back_when_empty_after_sanitize() {
        assert_eq!(branch_name_for_task(&TaskId::new("...")), "task/chat");
    }
}
