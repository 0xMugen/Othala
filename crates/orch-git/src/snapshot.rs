use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::command::GitCli;
use crate::error::GitError;
use crate::repo::{current_branch, RepoHandle};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Unmerged,
    Untracked,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub state: FileState,
    pub status_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub branch: String,
    pub clean: bool,
    pub changed_files: Vec<ChangedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSnapshot {
    pub files: Vec<PathBuf>,
    pub shortstat: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSnapshot {
    pub captured_at: DateTime<Utc>,
    pub status: StatusSnapshot,
    pub diff: DiffSnapshot,
}

pub fn capture_status_snapshot(repo: &RepoHandle, git: &GitCli) -> Result<StatusSnapshot, GitError> {
    let branch = current_branch(repo, git)?;
    let output = git.run(&repo.root, ["status", "--porcelain=v1"])?;
    let changed_files = parse_porcelain_status(&output.stdout)?;

    Ok(StatusSnapshot {
        branch,
        clean: changed_files.is_empty(),
        changed_files,
    })
}

pub fn capture_diff_snapshot(
    repo: &RepoHandle,
    git: &GitCli,
    against_ref: Option<&str>,
) -> Result<DiffSnapshot, GitError> {
    let mut args = vec!["diff", "--name-only"];
    if let Some(reference) = against_ref {
        args.push(reference);
    }
    let files_output = git.run(&repo.root, args)?;
    let files = files_output
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();

    let mut shortstat_args = vec!["diff", "--shortstat"];
    if let Some(reference) = against_ref {
        shortstat_args.push(reference);
    }
    let shortstat_output = git.run(&repo.root, shortstat_args)?;
    let shortstat = match shortstat_output.stdout.trim() {
        "" => None,
        text => Some(text.to_string()),
    };

    Ok(DiffSnapshot { files, shortstat })
}

pub fn capture_repo_snapshot(
    repo: &RepoHandle,
    git: &GitCli,
    against_ref: Option<&str>,
) -> Result<RepoSnapshot, GitError> {
    let status = capture_status_snapshot(repo, git)?;
    let diff = capture_diff_snapshot(repo, git, against_ref)?;

    Ok(RepoSnapshot {
        captured_at: Utc::now(),
        status,
        diff,
    })
}

fn parse_porcelain_status(raw: &str) -> Result<Vec<ChangedFile>, GitError> {
    let mut files = Vec::new();

    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }

        if line.len() < 4 {
            return Err(GitError::Parse {
                context: format!("invalid porcelain status line: {line}"),
            });
        }

        let code = &line[0..2];
        let path = line[3..].to_string();
        let state = file_state_from_code(code);

        files.push(ChangedFile {
            path: PathBuf::from(path),
            state,
            status_code: code.to_string(),
        });
    }

    Ok(files)
}

fn file_state_from_code(code: &str) -> FileState {
    if code == "??" {
        return FileState::Untracked;
    }
    if code.contains('A') {
        return FileState::Added;
    }
    if code.contains('M') {
        return FileState::Modified;
    }
    if code.contains('D') {
        return FileState::Deleted;
    }
    if code.contains('R') {
        return FileState::Renamed;
    }
    if code.contains('C') {
        return FileState::Copied;
    }
    if code.contains('U') {
        return FileState::Unmerged;
    }
    FileState::Unknown
}
