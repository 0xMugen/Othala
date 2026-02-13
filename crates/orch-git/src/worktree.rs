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

    /// Create a worktree **and** a new branch in one step.
    ///
    /// Runs `git worktree add -b <branch> <path>`.  This never touches the main
    /// worktree's HEAD, so VSCode/other editors won't see a branch switch.
    pub fn create_with_new_branch(
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
            OsString::from("-b"),
            OsString::from(spec.branch.as_str()),
            path.as_os_str().to_os_string(),
        ];
        self.git.run(&repo.root, args)?;

        Ok(WorktreeInfo {
            task_id: spec.task_id.clone(),
            branch: spec.branch.clone(),
            path,
        })
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
        let output = self
            .git
            .run(&repo.root, ["worktree", "list", "--porcelain"])?;
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use orch_core::types::TaskId;

    use super::{parse_worktree_list, WorktreeManager, WorktreeSpec};
    use crate::command::GitCli;
    use crate::repo::discover_repo;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("othala-orch-git-worktree-{prefix}-{now}"))
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo_with_branch(branch: &str) -> PathBuf {
        let root = unique_temp_dir("repo");
        fs::create_dir_all(&root).expect("create temp repo");
        run_git(&root, &["init"]);
        fs::write(root.join("README.md"), "init\n").expect("write file");
        run_git(&root, &["add", "README.md"]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=Test User",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
        );
        run_git(&root, &["branch", branch]);
        root
    }

    #[test]
    fn parse_worktree_list_parses_multiple_entries_and_trims_refs_prefix() {
        let raw = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo/.orch/wt/T1
HEAD 2222222222222222222222222222222222222222
branch refs/heads/task/T1

";

        let parsed = parse_worktree_list(raw).expect("parse worktree list");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, PathBuf::from("/repo"));
        assert_eq!(
            parsed[0].head.as_deref(),
            Some("1111111111111111111111111111111111111111")
        );
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert_eq!(parsed[1].path, PathBuf::from("/repo/.orch/wt/T1"));
        assert_eq!(parsed[1].branch.as_deref(), Some("task/T1"));
    }

    #[test]
    fn parse_worktree_list_handles_entry_without_branch() {
        let raw = "\
worktree /repo/.orch/wt/T2
HEAD 3333333333333333333333333333333333333333
detached

";

        let parsed = parse_worktree_list(raw).expect("parse worktree list");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path, PathBuf::from("/repo/.orch/wt/T2"));
        assert_eq!(
            parsed[0].head.as_deref(),
            Some("3333333333333333333333333333333333333333")
        );
        assert_eq!(parsed[0].branch, None);
    }

    #[test]
    fn parse_worktree_list_rejects_non_empty_unparseable_output() {
        let err = parse_worktree_list("nonsense output").expect_err("expected parse error");
        assert!(matches!(err, crate::error::GitError::Parse { .. }));
    }

    #[test]
    fn task_worktree_path_joins_repo_root_relative_root_and_task_id() {
        let manager = WorktreeManager::default();
        let repo_root = PathBuf::from("/tmp/repo");
        let repo = crate::repo::RepoHandle {
            root: repo_root.clone(),
            git_dir: repo_root.join(".git"),
        };

        let path = manager.task_worktree_path(&repo, &TaskId("T77".to_string()));
        assert_eq!(path, repo_root.join(".orch/wt/T77"));
    }

    #[test]
    fn create_list_and_remove_worktree_for_existing_branch() {
        let root = init_repo_with_branch("task/T1");
        let git = GitCli::default();
        let repo = discover_repo(&root, &git).expect("discover repo");
        let manager = WorktreeManager::default();
        let spec = WorktreeSpec {
            task_id: TaskId("T1".to_string()),
            branch: "task/T1".to_string(),
        };

        let info = manager
            .create_for_existing_branch(&repo, &spec)
            .expect("create worktree");
        assert_eq!(info.task_id.0, "T1");
        assert_eq!(info.branch, "task/T1");
        assert!(info.path.exists(), "worktree path should exist");

        let listed = manager.list(&repo).expect("list worktrees");
        assert!(listed.iter().any(|entry| {
            entry.path == info.path && entry.branch.as_deref() == Some("task/T1")
        }));

        manager
            .remove(&repo, &TaskId("T1".to_string()), true)
            .expect("remove worktree");
        assert!(!info.path.exists(), "worktree path should be removed");

        let _ = fs::remove_dir_all(&root);
    }
}
