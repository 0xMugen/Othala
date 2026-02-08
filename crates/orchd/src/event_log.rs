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
