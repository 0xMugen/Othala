use orch_core::types::TaskId;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    for line in lines {
        writeln!(file, "{}", line)?;
    }
    Ok(())
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
}
