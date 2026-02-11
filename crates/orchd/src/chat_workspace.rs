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
        current_branch(&repo, &git).context("failed to read current branch before gt create")?;
    let branch_name = branch_name_for_task(task_id);
    let commit_message = format!("start {}", task_id.0);
    let graphite = GraphiteClient::new(repo.root.clone());

    // Stash any uncommitted changes so branch switching works.
    let stash_output = git.run(&repo.root, ["stash", "--include-untracked"]);
    let did_stash = stash_output
        .as_ref()
        .map(|o| !o.stdout.contains("No local changes"))
        .unwrap_or(false);

    let result = provision_inner(&git, &graphite, &repo, &base_branch, &branch_name, &commit_message, task_id);

    // Always restore stashed changes.
    if did_stash {
        let _ = git.run(&repo.root, ["stash", "pop"]);
    }

    result
}

fn provision_inner(
    git: &GitCli,
    graphite: &GraphiteClient,
    repo: &RepoHandle,
    base_branch: &str,
    branch_name: &str,
    commit_message: &str,
    task_id: &TaskId,
) -> Result<ChatWorkspace> {
    graphite
        .create_branch(branch_name, commit_message)
        .with_context(|| format!("failed to create graphite branch {branch_name}"))?;

    // `gt create` leaves the new branch checked out in the main worktree.
    // Move back so the branch can be checked out in its dedicated task worktree.
    git.run(&repo.root, ["switch", base_branch])
        .with_context(|| format!("failed to switch back to base branch {base_branch}"))?;

    let manager = WorktreeManager::default();
    let spec = WorktreeSpec {
        task_id: task_id.clone(),
        branch: branch_name.to_string(),
    };

    let info = match manager.create_for_existing_branch(repo, &spec) {
        Ok(info) => info,
        Err(err) => {
            let _ = git.run(&repo.root, ["branch", "-D", branch_name]);
            return Err(anyhow!(err)).context("failed to create task worktree");
        }
    };

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
