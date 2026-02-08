use orch_core::events::Event;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum EventLogError {
    #[error("failed to create log directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize event: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to append to log file {path}: {source}")]
    Append {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlEventLog {
    pub root: PathBuf,
    pub global_file: PathBuf,
    pub task_dir: PathBuf,
}

impl JsonlEventLog {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let global_file = root.join("global.jsonl");
        let task_dir = root.join("tasks");
        Self {
            root,
            global_file,
            task_dir,
        }
    }

    pub fn ensure_layout(&self) -> Result<(), EventLogError> {
        fs::create_dir_all(&self.root).map_err(|source| EventLogError::CreateDir {
            path: self.root.clone(),
            source,
        })?;
        fs::create_dir_all(&self.task_dir).map_err(|source| EventLogError::CreateDir {
            path: self.task_dir.clone(),
            source,
        })?;
        Ok(())
    }

    pub fn append_global(&self, event: &Event) -> Result<(), EventLogError> {
        append_json_line(&self.global_file, event)
    }

    pub fn append_task(&self, event: &Event) -> Result<(), EventLogError> {
        if let Some(task_id) = &event.task_id {
            let file = self.task_dir.join(format!("{}.jsonl", task_id.0));
            append_json_line(&file, event)?;
        }
        Ok(())
    }

    pub fn append_both(&self, event: &Event) -> Result<(), EventLogError> {
        self.ensure_layout()?;
        self.append_global(event)?;
        self.append_task(event)?;
        Ok(())
    }

    pub fn task_log_path(&self, task_id: &str) -> PathBuf {
        self.task_dir.join(format!("{task_id}.jsonl"))
    }

    pub fn global_log_path(&self) -> &Path {
        self.global_file.as_path()
    }
}

fn append_json_line(path: &Path, event: &Event) -> Result<(), EventLogError> {
    let line =
        serde_json::to_string(event).map_err(|source| EventLogError::Serialize { source })?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| EventLogError::Append {
            path: path.to_path_buf(),
            source,
        })?;

    file.write_all(line.as_bytes())
        .map_err(|source| EventLogError::Append {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(b"\n")
        .map_err(|source| EventLogError::Append {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::events::{Event, EventKind};
    use orch_core::types::{EventId, RepoId, TaskId};
    use std::fs;

    use super::JsonlEventLog;

    fn mk_event(task_id: Option<&str>) -> Event {
        Event {
            id: EventId("E1".to_string()),
            task_id: task_id.map(|id| TaskId(id.to_string())),
            repo_id: Some(RepoId("example".to_string())),
            at: Utc::now(),
            kind: EventKind::TaskCreated,
        }
    }

    fn mk_log() -> JsonlEventLog {
        let root = std::env::temp_dir().join(format!(
            "othala-event-log-test-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        JsonlEventLog::new(root)
    }

    #[test]
    fn ensure_layout_creates_root_and_task_directories() {
        let log = mk_log();
        log.ensure_layout().expect("ensure layout");
        assert!(log.root.exists());
        assert!(log.task_dir.exists());
    }

    #[test]
    fn append_both_writes_global_and_task_log_when_task_id_present() {
        let log = mk_log();
        let event = mk_event(Some("T100"));

        log.append_both(&event).expect("append both");

        let global = fs::read_to_string(log.global_log_path()).expect("read global");
        let task = fs::read_to_string(log.task_log_path("T100")).expect("read task");
        assert!(global.contains("\"id\":\"E1\""));
        assert!(task.contains("\"id\":\"E1\""));
        assert_eq!(global.lines().count(), 1);
        assert_eq!(task.lines().count(), 1);
    }

    #[test]
    fn append_both_writes_only_global_when_task_id_missing() {
        let log = mk_log();
        let event = mk_event(None);

        log.append_both(&event).expect("append both");

        let global = fs::read_to_string(log.global_log_path()).expect("read global");
        assert!(global.contains("\"id\":\"E1\""));
        assert!(!log.task_log_path("T100").exists());
    }

    #[test]
    fn append_global_appends_multiple_lines() {
        let log = mk_log();
        log.ensure_layout().expect("ensure layout");

        let e1 = mk_event(Some("T1"));
        let mut e2 = mk_event(Some("T1"));
        e2.id = EventId("E2".to_string());

        log.append_global(&e1).expect("append e1");
        log.append_global(&e2).expect("append e2");

        let global = fs::read_to_string(log.global_log_path()).expect("read global");
        assert_eq!(global.lines().count(), 2);
        assert!(global.contains("\"id\":\"E1\""));
        assert!(global.contains("\"id\":\"E2\""));
    }
}
