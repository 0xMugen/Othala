use std::path::{Path, PathBuf};

use crate::command::GitCli;
use crate::error::GitError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoHandle {
    pub root: PathBuf,
    pub git_dir: PathBuf,
}

pub fn discover_repo(start_path: &Path, git: &GitCli) -> Result<RepoHandle, GitError> {
    let inside = match git.run(start_path, ["rev-parse", "--is-inside-work-tree"]) {
        Ok(output) => output.stdout.trim().eq("true"),
        Err(GitError::CommandFailed { .. }) => false,
        Err(err) => return Err(err),
    };

    if !inside {
        return Err(GitError::NotARepository {
            path: start_path.to_path_buf(),
        });
    }

    let root_raw = git.run(start_path, ["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root_raw.stdout.trim());

    let git_dir_raw = git.run(&root, ["rev-parse", "--git-dir"])?;
    let git_dir_rel = PathBuf::from(git_dir_raw.stdout.trim());
    let git_dir = if git_dir_rel.is_absolute() {
        git_dir_rel
    } else {
        root.join(git_dir_rel)
    };

    Ok(RepoHandle { root, git_dir })
}

pub fn current_branch(repo: &RepoHandle, git: &GitCli) -> Result<String, GitError> {
    let output = git.run(&repo.root, ["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(output.stdout.trim().to_string())
}

pub fn head_sha(repo: &RepoHandle, git: &GitCli) -> Result<String, GitError> {
    let output = git.run(&repo.root, ["rev-parse", "HEAD"])?;
    Ok(output.stdout.trim().to_string())
}

/// Check if the worktree has uncommitted changes (staged or unstaged).
pub fn has_uncommitted_changes(repo: &RepoHandle, git: &GitCli) -> Result<bool, GitError> {
    let output = git.run(&repo.root, ["status", "--porcelain"])?;
    Ok(!output.stdout.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{current_branch, discover_repo, head_sha};
    use crate::command::GitCli;
    use crate::error::GitError;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("othala-orch-git-{prefix}-{now}"))
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

    fn init_repo(with_commit: bool) -> PathBuf {
        let root = unique_temp_dir("repo");
        fs::create_dir_all(&root).expect("create temp repo");
        run_git(&root, &["init"]);

        if with_commit {
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
        }

        root
    }

    #[test]
    fn discover_repo_finds_root_from_nested_path() {
        let root = init_repo(false);
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).expect("create nested dir");

        let git = GitCli::default();
        let repo = discover_repo(&nested, &git).expect("discover repo");

        assert_eq!(repo.root, root);
        assert_eq!(repo.git_dir, repo.root.join(".git"));

        let _ = fs::remove_dir_all(&repo.root);
    }

    #[test]
    fn discover_repo_returns_not_a_repository_for_plain_directory() {
        let dir = unique_temp_dir("not-repo");
        fs::create_dir_all(&dir).expect("create plain dir");

        let git = GitCli::default();
        let err = discover_repo(&dir, &git).expect_err("expected not a repository");
        assert!(matches!(err, GitError::NotARepository { path } if path == dir));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn current_branch_and_head_sha_resolve_in_initialized_repository() {
        let root = init_repo(true);
        let git = GitCli::default();
        let repo = discover_repo(&root, &git).expect("discover repo");

        let branch = current_branch(&repo, &git).expect("current branch");
        assert!(!branch.trim().is_empty());

        let sha = head_sha(&repo, &git).expect("head sha");
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_repo_propagates_non_command_failed_git_errors() {
        let dir = unique_temp_dir("missing-git");
        fs::create_dir_all(&dir).expect("create plain dir");

        let git = GitCli::new("/definitely/missing/git-binary");
        let err = discover_repo(&dir, &git).expect_err("missing git binary should propagate io");
        assert!(matches!(err, GitError::Io { .. }));

        let _ = fs::remove_dir_all(&dir);
    }
}
