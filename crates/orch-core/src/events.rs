use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::state::VerifyTier;
use crate::types::{EventId, ModelKind, RepoId, SubmitMode, TaskId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    RequestChanges,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewIssue {
    pub severity: IssueSeverity,
    pub file: String,
    pub line: Option<u64>,
    pub description: String,
    pub suggested_fix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphiteHygieneReport {
    pub ok: bool,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestAssessment {
    pub ok: bool,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewOutput {
    pub verdict: ReviewVerdict,
    #[serde(default)]
    pub issues: Vec<ReviewIssue>,
    #[serde(default)]
    pub risk_flags: Vec<String>,
    pub graphite_hygiene: GraphiteHygieneReport,
    pub test_assessment: TestAssessment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    TaskCreated,
    TaskStateChanged { from: String, to: String },
    DraftPrCreated { number: u64, url: String },
    ParentHeadUpdated { parent_task_id: TaskId },
    RestackStarted,
    RestackCompleted,
    RestackConflict,
    RestackResolved,
    VerifyRequested { tier: VerifyTier },
    VerifyCompleted { tier: VerifyTier, success: bool },
    ReviewRequested { required_models: Vec<ModelKind> },
    ReviewCompleted { reviewer: ModelKind, output: ReviewOutput },
    ReadyReached,
    SubmitStarted { mode: SubmitMode },
    SubmitCompleted,
    NeedsHuman { reason: String },
    Error { code: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub task_id: Option<TaskId>,
    pub repo_id: Option<RepoId>,
    pub at: DateTime<Utc>,
    pub kind: EventKind,
}
