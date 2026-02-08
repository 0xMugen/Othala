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
    TaskStateChanged {
        from: String,
        to: String,
    },
    DraftPrCreated {
        number: u64,
        url: String,
    },
    ParentHeadUpdated {
        parent_task_id: TaskId,
    },
    RestackStarted,
    RestackCompleted,
    RestackConflict,
    RestackResolved,
    VerifyRequested {
        tier: VerifyTier,
    },
    VerifyCompleted {
        tier: VerifyTier,
        success: bool,
    },
    ReviewRequested {
        required_models: Vec<ModelKind>,
    },
    ReviewCompleted {
        reviewer: ModelKind,
        output: ReviewOutput,
    },
    ReadyReached,
    SubmitStarted {
        mode: SubmitMode,
    },
    SubmitCompleted,
    NeedsHuman {
        reason: String,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub task_id: Option<TaskId>,
    pub repo_id: Option<RepoId>,
    pub at: DateTime<Utc>,
    pub kind: EventKind,
}

#[cfg(test)]
mod tests {
    use super::{
        Event, EventKind, GraphiteHygieneReport, IssueSeverity, ReviewIssue, ReviewOutput,
        ReviewVerdict, TestAssessment,
    };
    use crate::state::VerifyTier;
    use crate::types::{EventId, ModelKind, RepoId, SubmitMode, TaskId};
    use chrono::{TimeZone, Utc};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct EventKindDoc {
        kind: EventKind,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct ReviewDoc {
        output: ReviewOutput,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct SeverityDoc {
        severity: IssueSeverity,
        verdict: ReviewVerdict,
    }

    #[test]
    fn event_kind_serializes_with_snake_case_variant_names() {
        let doc = EventKindDoc {
            kind: EventKind::SubmitStarted {
                mode: SubmitMode::Stack,
            },
        };

        let encoded = toml::to_string(&doc).expect("serialize event kind");
        assert!(encoded.contains("submit_started"));
        assert!(encoded.contains("mode = \"stack\""));

        let decoded: EventKindDoc = toml::from_str(&encoded).expect("deserialize event kind");
        assert_eq!(decoded, doc);
    }

    #[test]
    fn review_output_defaults_issue_and_risk_lists_when_missing() {
        let doc: ReviewDoc = toml::from_str(
            r#"
[output]
verdict = "approve"

[output.graphite_hygiene]
ok = true
notes = "ok"

[output.test_assessment]
ok = true
notes = "covered"
"#,
        )
        .expect("deserialize review doc");

        assert_eq!(doc.output.verdict, ReviewVerdict::Approve);
        assert!(doc.output.issues.is_empty());
        assert!(doc.output.risk_flags.is_empty());
    }

    #[test]
    fn issue_severity_and_verdict_serialize_in_snake_case() {
        let doc = SeverityDoc {
            severity: IssueSeverity::Critical,
            verdict: ReviewVerdict::RequestChanges,
        };

        let encoded = toml::to_string(&doc).expect("serialize severity/verdict");
        assert!(encoded.contains("severity = \"critical\""));
        assert!(encoded.contains("verdict = \"request_changes\""));

        let decoded: SeverityDoc = toml::from_str(&encoded).expect("deserialize severity/verdict");
        assert_eq!(decoded, doc);
    }

    #[test]
    fn event_roundtrip_preserves_identifiers_timestamp_and_payload() {
        let event = Event {
            id: EventId("E100".to_string()),
            task_id: Some(TaskId("T200".to_string())),
            repo_id: Some(RepoId("example".to_string())),
            at: Utc
                .with_ymd_and_hms(2026, 2, 8, 12, 30, 45)
                .single()
                .expect("valid timestamp"),
            kind: EventKind::ReviewCompleted {
                reviewer: ModelKind::Codex,
                output: ReviewOutput {
                    verdict: ReviewVerdict::Approve,
                    issues: vec![ReviewIssue {
                        severity: IssueSeverity::Low,
                        file: "src/lib.rs".to_string(),
                        line: Some(10),
                        description: "nit".to_string(),
                        suggested_fix: None,
                    }],
                    risk_flags: vec!["PERF".to_string()],
                    graphite_hygiene: GraphiteHygieneReport {
                        ok: true,
                        notes: "clean stack".to_string(),
                    },
                    test_assessment: TestAssessment {
                        ok: true,
                        notes: "tests updated".to_string(),
                    },
                },
            },
        };

        let encoded = toml::to_string(&event).expect("serialize event");
        let decoded: Event = toml::from_str(&encoded).expect("deserialize event");
        assert_eq!(decoded, event);
    }

    #[test]
    fn verify_requested_variant_roundtrip_preserves_tier() {
        let doc = EventKindDoc {
            kind: EventKind::VerifyRequested {
                tier: VerifyTier::Quick,
            },
        };

        let encoded = toml::to_string(&doc).expect("serialize verify requested");
        assert!(encoded.contains("verify_requested"));
        assert!(encoded.contains("tier = \"quick\""));

        let decoded: EventKindDoc = toml::from_str(&encoded).expect("deserialize verify requested");
        assert_eq!(decoded, doc);
    }
}
