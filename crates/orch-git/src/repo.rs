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
