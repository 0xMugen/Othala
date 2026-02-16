use std::ffi::OsString;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::command::GitCli;
use crate::error::GitError;
use crate::repo::{current_branch, head_sha, RepoHandle};

const SNAPSHOT_LOG_FILE: &str = "othala-change-snapshots.log";

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSnapshot {
    pub task_id: String,
    pub commit_sha: String,
    pub parent_sha: String,
    pub files_changed: Vec<PathBuf>,
    pub timestamp: DateTime<Utc>,
}

pub fn capture_change_snapshot(
    repo: &RepoHandle,
    git: &GitCli,
    task_id: &str,
) -> Result<ChangeSnapshot, GitError> {
    let commit_sha = head_sha(repo, git)?;
    let parent_sha = match git.run(&repo.root, ["rev-parse", "HEAD^"]) {
        Ok(output) => output.stdout.trim().to_string(),
        Err(GitError::CommandFailed { .. }) => commit_sha.clone(),
        Err(err) => return Err(err),
    };

    let files_output = git.run(&repo.root, ["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])?;
    let files_changed = files_output
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();

    let snapshot = ChangeSnapshot {
        task_id: task_id.to_string(),
        commit_sha,
        parent_sha,
        files_changed,
        timestamp: Utc::now(),
    };

    append_snapshot_log(repo, &snapshot)?;
    Ok(snapshot)
}

pub fn capture_snapshot(
    repo: &RepoHandle,
    git: &GitCli,
    task_id: &str,
) -> Result<ChangeSnapshot, GitError> {
    capture_change_snapshot(repo, git, task_id)
}

pub fn undo_to_snapshot(
    repo: &RepoHandle,
    git: &GitCli,
    snapshot: &ChangeSnapshot,
) -> Result<(), GitError> {
    checkout_snapshot_files(repo, git, &snapshot.parent_sha, &snapshot.files_changed)
}

pub fn redo_snapshot(
    repo: &RepoHandle,
    git: &GitCli,
    snapshot: &ChangeSnapshot,
) -> Result<(), GitError> {
    checkout_snapshot_files(repo, git, &snapshot.commit_sha, &snapshot.files_changed)
}

pub fn list_change_snapshots(
    repo: &RepoHandle,
    _git: &GitCli,
    task_id: &str,
) -> Result<Vec<ChangeSnapshot>, GitError> {
    let path = snapshot_log_path(repo);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let raw = std::fs::read_to_string(&path).map_err(|err| GitError::Parse {
        context: format!("failed to read snapshot log {}: {err}", path.display()),
    })?;

    let mut snapshots = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let snapshot = parse_snapshot_line(line).map_err(|err| GitError::Parse {
            context: format!("invalid snapshot log line {}: {err}", index + 1),
        })?;
        if snapshot.task_id == task_id {
            snapshots.push(snapshot);
        }
    }

    snapshots.sort_by_key(|snapshot| snapshot.timestamp);
    Ok(snapshots)
}

pub fn list_snapshots_for_task(
    repo: &RepoHandle,
    git: &GitCli,
    task_id: &str,
) -> Result<Vec<ChangeSnapshot>, GitError> {
    list_change_snapshots(repo, git, task_id)
}

pub fn capture_status_snapshot(
    repo: &RepoHandle,
    git: &GitCli,
) -> Result<StatusSnapshot, GitError> {
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

fn snapshot_log_path(repo: &RepoHandle) -> PathBuf {
    repo.git_dir.join(SNAPSHOT_LOG_FILE)
}

fn append_snapshot_log(repo: &RepoHandle, snapshot: &ChangeSnapshot) -> Result<(), GitError> {
    let path = snapshot_log_path(repo);
    let record = format_snapshot_line(snapshot);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| GitError::Parse {
            context: format!("failed to open snapshot log {}: {err}", path.display()),
        })?;
    use std::io::Write;
    file.write_all(record.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|err| GitError::Parse {
            context: format!("failed to write snapshot log {}: {err}", path.display()),
        })
}

fn format_snapshot_line(snapshot: &ChangeSnapshot) -> String {
    let files = snapshot
        .files_changed
        .iter()
        .map(|path| encode_hex(&path.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(",");

    [
        encode_hex(&snapshot.task_id),
        encode_hex(&snapshot.commit_sha),
        encode_hex(&snapshot.parent_sha),
        encode_hex(&snapshot.timestamp.to_rfc3339()),
        files,
    ]
    .join("\t")
}

fn parse_snapshot_line(line: &str) -> Result<ChangeSnapshot, String> {
    let mut parts = line.split('\t');
    let task_id = decode_hex(
        parts
            .next()
            .ok_or_else(|| "missing task_id field".to_string())?,
    )?;
    let commit_sha = decode_hex(
        parts
            .next()
            .ok_or_else(|| "missing commit_sha field".to_string())?,
    )?;
    let parent_sha = decode_hex(
        parts
            .next()
            .ok_or_else(|| "missing parent_sha field".to_string())?,
    )?;
    let timestamp_raw = decode_hex(
        parts
            .next()
            .ok_or_else(|| "missing timestamp field".to_string())?,
    )?;
    let files_raw = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Err("too many fields".to_string());
    }

    let timestamp = DateTime::parse_from_rfc3339(&timestamp_raw)
        .map_err(|err| format!("invalid timestamp: {err}"))?
        .with_timezone(&Utc);

    let files_changed = if files_raw.trim().is_empty() {
        Vec::new()
    } else {
        files_raw
            .split(',')
            .map(decode_hex)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>()
    };

    Ok(ChangeSnapshot {
        task_id,
        commit_sha,
        parent_sha,
        files_changed,
        timestamp,
    })
}

fn encode_hex(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 2);
    for byte in raw.as_bytes() {
        out.push(hex_nibble((byte >> 4) & 0x0f));
        out.push(hex_nibble(byte & 0x0f));
    }
    out
}

fn hex_nibble(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'a' + (value - 10)) as char,
        _ => '0',
    }
}

fn decode_hex(raw: &str) -> Result<String, String> {
    if !raw.len().is_multiple_of(2) {
        return Err("hex field has odd length".to_string());
    }

    let mut bytes = Vec::with_capacity(raw.len() / 2);
    let chars = raw.as_bytes();
    for chunk in chars.chunks(2) {
        let hi = decode_hex_nibble(chunk[0])?;
        let lo = decode_hex_nibble(chunk[1])?;
        bytes.push((hi << 4) | lo);
    }

    String::from_utf8(bytes).map_err(|err| format!("hex decode utf8 failed: {err}"))
}

fn decode_hex_nibble(raw: u8) -> Result<u8, String> {
    match raw {
        b'0'..=b'9' => Ok(raw - b'0'),
        b'a'..=b'f' => Ok(raw - b'a' + 10),
        b'A'..=b'F' => Ok(raw - b'A' + 10),
        _ => Err(format!("invalid hex digit: {}", raw as char)),
    }
}

fn checkout_snapshot_files(
    repo: &RepoHandle,
    git: &GitCli,
    target_sha: &str,
    files: &[PathBuf],
) -> Result<(), GitError> {
    if files.is_empty() {
        return Ok(());
    }

    let mut existing_in_target = Vec::new();
    for path in files {
        if path_exists_in_commit(repo, git, target_sha, path)? {
            existing_in_target.push(path.clone());
        } else {
            remove_worktree_path(&repo.root, path).map_err(|err| GitError::Parse {
                context: format!("failed to remove path {}: {err}", path.display()),
            })?;
        }
    }

    if existing_in_target.is_empty() {
        return Ok(());
    }

    let mut args = vec![
        OsString::from("checkout"),
        OsString::from(target_sha),
        OsString::from("--"),
    ];
    for path in &existing_in_target {
        args.push(path.as_os_str().to_os_string());
    }

    git.run(&repo.root, args)?;
    Ok(())
}

fn path_exists_in_commit(
    repo: &RepoHandle,
    git: &GitCli,
    commit_sha: &str,
    path: &Path,
) -> Result<bool, GitError> {
    let spec = format!("{commit_sha}:{}", path.to_string_lossy());
    match git.run(&repo.root, ["cat-file", "-e", spec.as_str()]) {
        Ok(_) => Ok(true),
        Err(GitError::CommandFailed { .. }) => Ok(false),
        Err(err) => Err(err),
    }
}

fn remove_worktree_path(repo_root: &Path, path: &Path) -> std::io::Result<()> {
    let full_path = repo_root.join(path);

    if full_path.is_file() {
        std::fs::remove_file(&full_path)?;
    } else if full_path.is_dir() {
        std::fs::remove_dir_all(&full_path)?;
    } else {
        return Ok(());
    }

    let mut current = full_path.parent().map(Path::to_path_buf);
    while let Some(dir) = current {
        if dir == repo_root {
            break;
        }
        match std::fs::read_dir(&dir)?.next() {
            None => {
                std::fs::remove_dir(&dir)?;
                current = dir.parent().map(Path::to_path_buf);
            }
            Some(_) => break,
        }
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        capture_change_snapshot, file_state_from_code, list_change_snapshots, parse_porcelain_status,
        redo_snapshot, undo_to_snapshot, FileState,
    };
    use crate::command::GitCli;
    use crate::repo::discover_repo;

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

    fn init_repo() -> PathBuf {
        let root = unique_temp_dir("snapshot-repo");
        fs::create_dir_all(&root).expect("create temp repo");
        run_git(&root, &["init"]);
        run_git(&root, &["config", "user.name", "Test User"]);
        run_git(&root, &["config", "user.email", "test@example.com"]);
        root
    }

    fn commit_file(repo: &Path, relative_path: &str, contents: &str, message: &str) {
        let full_path = repo.join(relative_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(&full_path, contents).expect("write file");
        run_git(repo, &["add", relative_path]);
        run_git(repo, &["commit", "-m", message]);
    }

    fn head_sha(repo: &Path) -> String {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .expect("head sha");
        assert!(output.status.success(), "rev-parse HEAD should succeed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn parse_porcelain_status_parses_common_status_codes() {
        let raw = concat!(
            " M src/lib.rs\n",
            "A  src/new.rs\n",
            "D  src/old.rs\n",
            "R  src/renamed.rs\n",
            "C  src/copied.rs\n",
            "UU src/conflict.rs\n",
            "?? src/untracked.rs\n",
        );
        let parsed = parse_porcelain_status(raw).expect("parse porcelain");
        assert_eq!(parsed.len(), 7);
        assert_eq!(parsed[0].state, FileState::Modified);
        assert_eq!(parsed[0].path, PathBuf::from("src/lib.rs"));
        assert_eq!(parsed[1].state, FileState::Added);
        assert_eq!(parsed[2].state, FileState::Deleted);
        assert_eq!(parsed[3].state, FileState::Renamed);
        assert_eq!(parsed[4].state, FileState::Copied);
        assert_eq!(parsed[5].state, FileState::Unmerged);
        assert_eq!(parsed[6].state, FileState::Untracked);
    }

    #[test]
    fn parse_porcelain_status_rejects_short_invalid_lines() {
        let err = parse_porcelain_status("M\n").expect_err("expected parse error");
        assert!(matches!(err, crate::error::GitError::Parse { .. }));
    }

    #[test]
    fn parse_porcelain_status_preserves_rename_arrow_payload() {
        let raw = "R  src/old_name.rs -> src/new_name.rs\n";
        let parsed = parse_porcelain_status(raw).expect("parse porcelain");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].state, FileState::Renamed);
        assert_eq!(
            parsed[0].path,
            PathBuf::from("src/old_name.rs -> src/new_name.rs")
        );
        assert_eq!(parsed[0].status_code, "R ");
    }

    #[test]
    fn file_state_from_code_returns_unknown_for_unhandled_codes() {
        assert_eq!(file_state_from_code("!!"), FileState::Unknown);
        assert_eq!(file_state_from_code("  "), FileState::Unknown);
    }

    #[test]
    fn capture_change_snapshot_records_head_parent_and_files() {
        let root = init_repo();
        commit_file(&root, "README.md", "base\n", "base");
        let parent_sha = head_sha(&root);
        commit_file(&root, "README.md", "base\nnext\n", "update readme");

        let git = GitCli::default();
        let repo = discover_repo(&root, &git).expect("discover repo");
        let snapshot = capture_change_snapshot(&repo, &git, "task-1").expect("capture snapshot");

        assert_eq!(snapshot.task_id, "task-1");
        assert_eq!(snapshot.parent_sha, parent_sha);
        assert_eq!(snapshot.commit_sha, head_sha(&root));
        assert_eq!(snapshot.files_changed, vec![PathBuf::from("README.md")]);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn list_change_snapshots_filters_task_id() {
        let root = init_repo();
        commit_file(&root, "README.md", "base\n", "base");

        let git = GitCli::default();
        let repo = discover_repo(&root, &git).expect("discover repo");

        commit_file(&root, "README.md", "v1\n", "task one commit");
        let first_for_task_a = capture_change_snapshot(&repo, &git, "task-a")
            .expect("capture snapshot for task-a first");

        commit_file(&root, "README.md", "v2\n", "task b commit");
        capture_change_snapshot(&repo, &git, "task-b").expect("capture snapshot for task-b");

        commit_file(&root, "README.md", "v3\n", "task one second commit");
        let second_for_task_a = capture_change_snapshot(&repo, &git, "task-a")
            .expect("capture snapshot for task-a second");

        let task_a_snapshots = list_change_snapshots(&repo, &git, "task-a")
            .expect("list snapshots for task-a");
        assert_eq!(task_a_snapshots.len(), 2);
        assert_eq!(task_a_snapshots[0].commit_sha, first_for_task_a.commit_sha);
        assert_eq!(task_a_snapshots[1].commit_sha, second_for_task_a.commit_sha);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn undo_to_snapshot_restores_parent_content() {
        let root = init_repo();
        commit_file(&root, "README.md", "before\n", "base");
        commit_file(&root, "README.md", "after\n", "update");

        let git = GitCli::default();
        let repo = discover_repo(&root, &git).expect("discover repo");
        let snapshot = capture_change_snapshot(&repo, &git, "task-undo")
            .expect("capture snapshot");

        undo_to_snapshot(&repo, &git, &snapshot).expect("undo snapshot");
        let contents = fs::read_to_string(root.join("README.md")).expect("read file after undo");
        assert_eq!(contents, "before\n");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn redo_snapshot_reapplies_snapshot_content() {
        let root = init_repo();
        commit_file(&root, "README.md", "before\n", "base");
        commit_file(&root, "README.md", "after\n", "update");

        let git = GitCli::default();
        let repo = discover_repo(&root, &git).expect("discover repo");
        let snapshot = capture_change_snapshot(&repo, &git, "task-redo")
            .expect("capture snapshot");

        undo_to_snapshot(&repo, &git, &snapshot).expect("undo snapshot");
        redo_snapshot(&repo, &git, &snapshot).expect("redo snapshot");

        let contents = fs::read_to_string(root.join("README.md")).expect("read file after redo");
        assert_eq!(contents, "after\n");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn undo_and_redo_handle_added_file() {
        let root = init_repo();
        commit_file(&root, "README.md", "base\n", "base");
        commit_file(&root, "notes/new.txt", "created\n", "add file");

        let git = GitCli::default();
        let repo = discover_repo(&root, &git).expect("discover repo");
        let snapshot = capture_change_snapshot(&repo, &git, "task-add")
            .expect("capture snapshot");

        assert!(root.join("notes/new.txt").exists());
        undo_to_snapshot(&repo, &git, &snapshot).expect("undo snapshot");
        assert!(!root.join("notes/new.txt").exists());

        redo_snapshot(&repo, &git, &snapshot).expect("redo snapshot");
        assert_eq!(
            fs::read_to_string(root.join("notes/new.txt")).expect("read recreated file"),
            "created\n"
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn list_change_snapshots_returns_empty_when_log_absent() {
        let root = init_repo();
        commit_file(&root, "README.md", "base\n", "base");

        let git = GitCli::default();
        let repo = discover_repo(&root, &git).expect("discover repo");
        let snapshots = list_change_snapshots(&repo, &git, "task-none")
            .expect("listing snapshots should succeed");

        assert!(snapshots.is_empty());
        fs::remove_dir_all(root).ok();
    }
}
