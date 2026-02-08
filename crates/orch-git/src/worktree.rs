use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use orch_core::types::TaskId;
use serde::{Deserialize, Serialize};

use crate::command::GitCli;
use crate::error::GitError;
use crate::repo::RepoHandle;

pub const DEFAULT_WORKTREE_ROOT: &str = ".orch/wt";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSpec {
    pub task_id: TaskId,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub task_id: TaskId,
    pub branch: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeManager {
    git: GitCli,
    relative_root: PathBuf,
}

impl Default for WorktreeManager {
    fn default() -> Self {
        Self {
            git: GitCli::default(),
            relative_root: PathBuf::from(DEFAULT_WORKTREE_ROOT),
        }
    }
}

impl WorktreeManager {
    pub fn new(git: GitCli, relative_root: impl Into<PathBuf>) -> Self {
        Self {
            git,
            relative_root: relative_root.into(),
        }
    }

    pub fn task_worktree_path(&self, repo: &RepoHandle, task_id: &TaskId) -> PathBuf {
        repo.root.join(&self.relative_root).join(&task_id.0)
    }

    pub fn create_for_existing_branch(
        &self,
        repo: &RepoHandle,
        spec: &WorktreeSpec,
    ) -> Result<WorktreeInfo, GitError> {
        let root = repo.root.join(&self.relative_root);
        fs::create_dir_all(&root).map_err(|source| GitError::Io {
            command: format!("create_dir_all {}", root.display()),
            source,
        })?;

        let path = self.task_worktree_path(repo, &spec.task_id);
        let args = vec![
            OsString::from("worktree"),
            OsString::from("add"),
            path.as_os_str().to_os_string(),
            OsString::from(spec.branch.as_str()),
        ];
        self.git.run(&repo.root, args)?;

        Ok(WorktreeInfo {
            task_id: spec.task_id.clone(),
            branch: spec.branch.clone(),
            path,
        })
    }

    pub fn remove(&self, repo: &RepoHandle, task_id: &TaskId, force: bool) -> Result<(), GitError> {
        let path = self.task_worktree_path(repo, task_id);
        let mut args = vec![OsString::from("worktree"), OsString::from("remove")];
        if force {
            args.push(OsString::from("--force"));
        }
        args.push(path.as_os_str().to_os_string());

        self.git.run(&repo.root, args)?;
        Ok(())
    }

    pub fn list(&self, repo: &RepoHandle) -> Result<Vec<ListedWorktree>, GitError> {
        let output = self.git.run(&repo.root, ["worktree", "list", "--porcelain"])?;
        parse_worktree_list(&output.stdout)
    }
}

fn parse_worktree_list(raw: &str) -> Result<Vec<ListedWorktree>, GitError> {
    let mut listed = Vec::new();

    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;
    let mut current_head: Option<String> = None;

    for line in raw.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if let Some(path) = current_path.take() {
                listed.push(ListedWorktree {
                    path,
                    branch: current_branch.take(),
                    head: current_head.take(),
                });
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(rest.trim()));
            continue;
        }
        if let Some(rest) = line.strip_prefix("branch ") {
            let branch = rest.trim().trim_start_matches("refs/heads/").to_string();
            current_branch = Some(branch);
            continue;
        }
        if let Some(rest) = line.strip_prefix("HEAD ") {
            current_head = Some(rest.trim().to_string());
            continue;
        }
    }

    if listed.is_empty() && !raw.trim().is_empty() {
        return Err(GitError::Parse {
            context: "unable to parse git worktree list output".to_string(),
        });
    }

    Ok(listed)
}
