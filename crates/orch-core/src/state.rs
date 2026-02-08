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

#[cfg(test)]
mod tests {
    use super::{
        ReviewCapacityState, ReviewPolicy, ReviewStatus, TaskState, VerifyStatus, VerifyTier,
    };
    use crate::types::ModelKind;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TaskStateDoc {
        state: TaskState,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct PolicyDoc {
        verify_tier: VerifyTier,
        review_policy: ReviewPolicy,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct VerifyStatusDoc {
        status: VerifyStatus,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct ReviewStatusDoc {
        review: ReviewStatus,
    }

    #[test]
    fn task_state_serializes_as_screaming_snake_case() {
        let doc = TaskStateDoc {
            state: TaskState::DraftPrOpen,
        };

        let encoded = toml::to_string(&doc).expect("serialize task state");
        assert!(encoded.contains("state = \"DRAFT_PR_OPEN\""));

        let decoded: TaskStateDoc = toml::from_str(&encoded).expect("deserialize task state");
        assert_eq!(decoded, doc);
    }

    #[test]
    fn policy_and_tier_serialize_as_snake_case() {
        let doc = PolicyDoc {
            verify_tier: VerifyTier::Full,
            review_policy: ReviewPolicy::Strict,
        };

        let encoded = toml::to_string(&doc).expect("serialize policy doc");
        assert!(encoded.contains("verify_tier = \"full\""));
        assert!(encoded.contains("review_policy = \"strict\""));

        let decoded: PolicyDoc = toml::from_str(&encoded).expect("deserialize policy doc");
        assert_eq!(decoded, doc);
    }

    #[test]
    fn verify_status_failed_roundtrip_preserves_tier_and_summary() {
        let doc = VerifyStatusDoc {
            status: VerifyStatus::Failed {
                tier: VerifyTier::Quick,
                summary: "lint failed".to_string(),
            },
        };

        let encoded = toml::to_string(&doc).expect("serialize verify status");
        let decoded: VerifyStatusDoc = toml::from_str(&encoded).expect("deserialize verify status");
        assert_eq!(decoded, doc);
    }

    #[test]
    fn review_status_roundtrip_preserves_capacity_and_models() {
        let doc = ReviewStatusDoc {
            review: ReviewStatus {
                required_models: vec![ModelKind::Claude, ModelKind::Gemini],
                approvals_received: 2,
                approvals_required: 2,
                unanimous: true,
                capacity_state: ReviewCapacityState::Sufficient,
            },
        };

        let encoded = toml::to_string(&doc).expect("serialize review status");
        assert!(encoded.contains("capacity_state = \"sufficient\""));
        assert!(encoded.contains("required_models = [\"claude\", \"gemini\"]"));

        let decoded: ReviewStatusDoc = toml::from_str(&encoded).expect("deserialize review status");
        assert_eq!(decoded, doc);
    }
}
