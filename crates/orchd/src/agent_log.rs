use orch_core::types::TaskId;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_LOG_SIZE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ROTATED_FILES: usize = 5;

pub fn agent_log_dir(repo_root: &Path, task_id: &TaskId) -> PathBuf {
    repo_root.join(".othala/agent-output").join(&task_id.0)
}

pub fn append_agent_output(
    repo_root: &Path,
    task_id: &TaskId,
    lines: &[String],
) -> std::io::Result<()> {
    let dir = agent_log_dir(repo_root, task_id);
    fs::create_dir_all(&dir)?;
    let path = dir.join("latest.log");
    let _ = rotate_log_if_needed(&path)?;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    for line in lines {
        writeln!(file, "{}", line)?;
    }
    Ok(())
}

pub fn rotate_log_if_needed(log_path: &Path) -> std::io::Result<bool> {
    let metadata = std::fs::metadata(log_path);
    match metadata {
        Ok(m) if m.len() > MAX_LOG_SIZE_BYTES => {
            rotate_log(log_path)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn rotate_log(log_path: &Path) -> std::io::Result<()> {
    for i in (1..MAX_ROTATED_FILES).rev() {
        let from = log_path.with_extension(format!("log.{i}"));
        let to = log_path.with_extension(format!("log.{}", i + 1));
        if from.exists() {
            std::fs::rename(&from, &to)?;
        }
    }

    let rotated = log_path.with_extension("log.1");
    std::fs::rename(log_path, &rotated)?;
    Ok(())
}

pub fn list_rotated_logs(log_dir: &Path) -> Vec<PathBuf> {
    let mut logs = Vec::new();
    let latest = log_dir.join("latest.log");
    if latest.exists() {
        logs.push(latest);
    }
    for i in 1..=MAX_ROTATED_FILES {
        let rotated = log_dir.join(format!("latest.log.{i}"));
        if rotated.exists() {
            logs.push(rotated);
        }
    }
    logs
}

pub fn total_log_size(log_dir: &Path) -> u64 {
    list_rotated_logs(log_dir)
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

pub fn read_agent_log(repo_root: &Path, task_id: &TaskId) -> std::io::Result<String> {
    let path = agent_log_dir(repo_root, task_id).join("latest.log");
    fs::read_to_string(&path)
}

pub fn tail_agent_log(
    repo_root: &Path,
    task_id: &TaskId,
    n: usize,
) -> std::io::Result<Vec<String>> {
    let content = read_agent_log(repo_root, task_id)?;
    let lines: Vec<String> = content.lines().map(String::from).collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_repo_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("othala-agent-log-test-{nanos}"))
    }

    #[test]
    fn append_creates_directory_and_file() {
        let repo_root = unique_test_repo_root();
        let task_id = TaskId::new("task-append");
        let lines = vec!["first".to_string(), "second".to_string()];

        append_agent_output(&repo_root, &task_id, &lines).expect("append should succeed");

        let log_path = agent_log_dir(&repo_root, &task_id).join("latest.log");
        assert!(log_path.exists(), "expected log file to exist");

        let content = fs::read_to_string(&log_path).expect("expected to read log file");
        assert_eq!(content, "first\nsecond\n");

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn tail_returns_last_n_lines() {
        let repo_root = unique_test_repo_root();
        let task_id = TaskId::new("task-tail");
        let lines = vec![
            "line1".to_string(),
            "line2".to_string(),
            "line3".to_string(),
            "line4".to_string(),
        ];

        append_agent_output(&repo_root, &task_id, &lines).expect("append should succeed");

        let tail = tail_agent_log(&repo_root, &task_id, 2).expect("tail should succeed");
        assert_eq!(tail, vec!["line3".to_string(), "line4".to_string()]);

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn read_nonexistent_log_returns_error() {
        let repo_root = unique_test_repo_root();
        let task_id = TaskId::new("task-missing");

        let result = read_agent_log(&repo_root, &task_id);
        assert!(result.is_err(), "expected missing log file error");
    }

    #[test]
    fn rotate_log_shifts_files() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-rotate-shift");
        fs::create_dir_all(&log_dir).expect("create log dir");

        fs::write(log_dir.join("latest.log"), "latest").expect("write latest");
        fs::write(log_dir.join("latest.log.1"), "one").expect("write one");
        fs::write(log_dir.join("latest.log.2"), "two").expect("write two");

        let latest_path = log_dir.join("latest.log");
        rotate_log(&latest_path).expect("rotate should succeed");

        assert!(!latest_path.exists());
        assert_eq!(fs::read_to_string(log_dir.join("latest.log.1")).expect("read .1"), "latest");
        assert_eq!(fs::read_to_string(log_dir.join("latest.log.2")).expect("read .2"), "one");
        assert_eq!(fs::read_to_string(log_dir.join("latest.log.3")).expect("read .3"), "two");

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn rotate_log_respects_max_files() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-rotate-max");
        fs::create_dir_all(&log_dir).expect("create log dir");

        fs::write(log_dir.join("latest.log"), "latest").expect("write latest");
        for i in 1..=MAX_ROTATED_FILES {
            fs::write(log_dir.join(format!("latest.log.{i}")), format!("old-{i}"))
                .expect("write rotated file");
        }

        let latest_path = log_dir.join("latest.log");
        rotate_log(&latest_path).expect("rotate should succeed");

        assert!(!log_dir.join(format!("latest.log.{}", MAX_ROTATED_FILES + 1)).exists());
        assert_eq!(
            fs::read_to_string(log_dir.join(format!("latest.log.{MAX_ROTATED_FILES}")))
                .expect("read max file"),
            format!("old-{}", MAX_ROTATED_FILES - 1)
        );

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn rotate_log_if_needed_skips_small_files() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-rotate-small");
        fs::create_dir_all(&log_dir).expect("create log dir");

        let latest_path = log_dir.join("latest.log");
        fs::write(&latest_path, "small").expect("write small log");

        let rotated = rotate_log_if_needed(&latest_path).expect("rotation check should succeed");
        assert!(!rotated);
        assert!(latest_path.exists());
        assert!(!log_dir.join("latest.log.1").exists());

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn list_rotated_logs_finds_all() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-list");
        fs::create_dir_all(&log_dir).expect("create log dir");

        fs::write(log_dir.join("latest.log"), "latest").expect("write latest");
        fs::write(log_dir.join("latest.log.1"), "one").expect("write one");
        fs::write(log_dir.join("latest.log.3"), "three").expect("write three");

        let logs = list_rotated_logs(&log_dir);
        let expected = vec![
            log_dir.join("latest.log"),
            log_dir.join("latest.log.1"),
            log_dir.join("latest.log.3"),
        ];
        assert_eq!(logs, expected);

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn total_log_size_sums_correctly() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-size");
        fs::create_dir_all(&log_dir).expect("create log dir");

        fs::write(log_dir.join("latest.log"), b"abc").expect("write latest");
        fs::write(log_dir.join("latest.log.1"), b"12345").expect("write rotated");
        fs::write(log_dir.join("latest.log.4"), b"xy").expect("write rotated");

        assert_eq!(total_log_size(&log_dir), 10);

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn rotate_log_if_needed_rotates_large_files() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-rotate-large");
        fs::create_dir_all(&log_dir).expect("create log dir");
        let latest_path = log_dir.join("latest.log");

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&latest_path)
            .expect("open latest");
        file.set_len(MAX_LOG_SIZE_BYTES + 1).expect("inflate file");
        std::io::Write::write_all(&mut file, b"x").expect("touch file");

        let rotated = rotate_log_if_needed(&latest_path).expect("rotation check should succeed");
        assert!(rotated);
        assert!(!latest_path.exists());
        assert!(log_dir.join("latest.log.1").exists());

        let _ = fs::remove_dir_all(&repo_root);
    }
}
