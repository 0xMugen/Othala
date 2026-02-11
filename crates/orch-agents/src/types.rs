use chrono::{DateTime, Utc};
use orch_core::types::{ModelKind, RepoId, TaskId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCommand {
    pub executable: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochRequest {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub model: ModelKind,
    pub repo_path: PathBuf,
    pub prompt: String,
    pub timeout_secs: u64,
    pub extra_args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSignalKind {
    NeedHuman,
    PatchReady,
    ConflictResolved,
    RateLimited,
    ErrorHint,
    #[serde(rename = "qa_complete")]
    QAComplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSignal {
    pub kind: AgentSignalKind,
    pub at: DateTime<Utc>,
    pub message: String,
    pub source_line: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyChunk {
    pub at: DateTime<Utc>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpochStopReason {
    Completed,
    Failed,
    Timeout,
    NeedHuman,
    PatchReady,
    RateLimited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochResult {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub model: ModelKind,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub stop_reason: EpochStopReason,
    pub exit_code: Option<i32>,
    pub output: Vec<PtyChunk>,
    pub signals: Vec<AgentSignal>,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::types::{ModelKind, RepoId, TaskId};

    use super::{AgentSignal, AgentSignalKind, EpochResult, EpochStopReason, PtyChunk};

    #[test]
    fn agent_signal_kind_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&AgentSignalKind::NeedHuman).expect("serialize"),
            "\"need_human\""
        );
        assert_eq!(
            serde_json::to_string(&AgentSignalKind::PatchReady).expect("serialize"),
            "\"patch_ready\""
        );
        assert_eq!(
            serde_json::to_string(&AgentSignalKind::ConflictResolved).expect("serialize"),
            "\"conflict_resolved\""
        );
        assert_eq!(
            serde_json::to_string(&AgentSignalKind::RateLimited).expect("serialize"),
            "\"rate_limited\""
        );
        assert_eq!(
            serde_json::to_string(&AgentSignalKind::ErrorHint).expect("serialize"),
            "\"error_hint\""
        );
        assert_eq!(
            serde_json::to_string(&AgentSignalKind::QAComplete).expect("serialize"),
            "\"qa_complete\""
        );
    }

    #[test]
    fn epoch_stop_reason_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&EpochStopReason::Completed).expect("serialize"),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&EpochStopReason::NeedHuman).expect("serialize"),
            "\"need_human\""
        );
        assert_eq!(
            serde_json::to_string(&EpochStopReason::PatchReady).expect("serialize"),
            "\"patch_ready\""
        );
        assert_eq!(
            serde_json::to_string(&EpochStopReason::RateLimited).expect("serialize"),
            "\"rate_limited\""
        );
    }

    #[test]
    fn epoch_result_roundtrip_preserves_fields() {
        let now = Utc::now();
        let result = EpochResult {
            task_id: TaskId("T42".to_string()),
            repo_id: RepoId("example".to_string()),
            model: ModelKind::Codex,
            started_at: now,
            finished_at: now,
            stop_reason: EpochStopReason::PatchReady,
            exit_code: Some(0),
            output: vec![PtyChunk {
                at: now,
                text: "line one".to_string(),
            }],
            signals: vec![AgentSignal {
                kind: AgentSignalKind::PatchReady,
                at: now,
                message: "[patch_ready]".to_string(),
                source_line: "[patch_ready]".to_string(),
            }],
        };

        let encoded = serde_json::to_string(&result).expect("serialize");
        let decoded: EpochResult = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, result);
    }
}
