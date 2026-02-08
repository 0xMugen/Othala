use chrono::{DateTime, Utc};
use orch_core::types::{ModelKind, RepoId, TaskId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRunRecord {
    pub run_id: String,
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub model: ModelKind,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub stop_reason: Option<String>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub artifact_id: String,
    pub task_id: TaskId,
    pub kind: String,
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub metadata_json: Option<String>,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::types::{ModelKind, RepoId, TaskId};

    use super::{ArtifactRecord, TaskRunRecord};

    #[test]
    fn task_run_record_roundtrip_preserves_optional_fields() {
        let now = Utc::now();
        let record = TaskRunRecord {
            run_id: "R1".to_string(),
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Codex,
            started_at: now,
            finished_at: Some(now),
            stop_reason: Some("completed".to_string()),
            exit_code: Some(0),
        };

        let encoded = serde_json::to_string(&record).expect("serialize");
        let decoded: TaskRunRecord = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, record);
    }

    #[test]
    fn task_run_record_roundtrip_with_none_optional_fields() {
        let record = TaskRunRecord {
            run_id: "R2".to_string(),
            task_id: TaskId("T2".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Claude,
            started_at: Utc::now(),
            finished_at: None,
            stop_reason: None,
            exit_code: None,
        };

        let encoded = serde_json::to_string(&record).expect("serialize");
        let decoded: TaskRunRecord = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, record);
    }

    #[test]
    fn artifact_record_roundtrip_preserves_metadata() {
        let record = ArtifactRecord {
            artifact_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            kind: "patch".to_string(),
            path: "/tmp/patch.diff".to_string(),
            created_at: Utc::now(),
            metadata_json: Some("{\"size\":123}".to_string()),
        };

        let encoded = serde_json::to_string(&record).expect("serialize");
        let decoded: ArtifactRecord = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, record);
    }
}
