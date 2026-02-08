use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskState {
    Queued,
    Initializing,
    DraftPrOpen,
    Running,
    Restacking,
    RestackConflict,
    VerifyingQuick,
    VerifyingFull,
    Reviewing,
    NeedsHuman,
    Ready,
    Submitting,
    AwaitingMerge,
    Merged,
    Failed,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyTier {
    Quick,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    NotRun,
    Running { tier: VerifyTier },
    Passed { tier: VerifyTier },
    Failed { tier: VerifyTier, summary: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewPolicy {
    Adaptive,
    Strict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewCapacityState {
    Sufficient,
    WaitingForReviewCapacity,
    NeedsHuman,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReviewStatus {
    pub required_models: Vec<crate::types::ModelKind>,
    pub approvals_received: usize,
    pub approvals_required: usize,
    pub unanimous: bool,
    pub capacity_state: ReviewCapacityState,
}
