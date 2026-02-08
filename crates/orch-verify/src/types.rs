use chrono::{DateTime, Utc};
use orch_core::state::VerifyTier;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyOutcome {
    Passed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyFailureClass {
    Tests,
    Lint,
    Format,
    Build,
    Environment,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedVerifyCommand {
    pub original: String,
    pub effective: String,
    pub wrapped_with_dev_shell: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyCommandResult {
    pub command: PreparedVerifyCommand,
    pub outcome: VerifyOutcome,
    pub failure_class: Option<VerifyFailureClass>,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyResult {
    pub tier: VerifyTier,
    pub outcome: VerifyOutcome,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub commands: Vec<VerifyCommandResult>,
}

#[cfg(test)]
mod tests {
    use super::{
        PreparedVerifyCommand, VerifyCommandResult, VerifyFailureClass, VerifyOutcome, VerifyResult,
    };
    use chrono::{TimeZone, Utc};
    use orch_core::state::VerifyTier;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct EnumDoc {
        outcome: VerifyOutcome,
        class: VerifyFailureClass,
    }

    #[test]
    fn enums_serialize_in_snake_case() {
        let doc = EnumDoc {
            outcome: VerifyOutcome::Failed,
            class: VerifyFailureClass::Environment,
        };

        let encoded = serde_json::to_string(&doc).expect("serialize enum doc");
        assert!(encoded.contains("\"outcome\":\"failed\""));
        assert!(encoded.contains("\"class\":\"environment\""));

        let decoded: EnumDoc = serde_json::from_str(&encoded).expect("deserialize enum doc");
        assert_eq!(decoded, doc);
    }

    #[test]
    fn verify_command_result_roundtrip_preserves_optional_failure_fields() {
        let result = VerifyCommandResult {
            command: PreparedVerifyCommand {
                original: "just test".to_string(),
                effective: "nix develop -c just test".to_string(),
                wrapped_with_dev_shell: true,
            },
            outcome: VerifyOutcome::Failed,
            failure_class: Some(VerifyFailureClass::Tests),
            exit_code: Some(101),
            started_at: Utc
                .with_ymd_and_hms(2026, 2, 8, 18, 0, 0)
                .single()
                .expect("valid started_at"),
            finished_at: Utc
                .with_ymd_and_hms(2026, 2, 8, 18, 0, 5)
                .single()
                .expect("valid finished_at"),
            stdout: "running tests".to_string(),
            stderr: "1 failed".to_string(),
        };

        let encoded = serde_json::to_string(&result).expect("serialize command result");
        let decoded: VerifyCommandResult =
            serde_json::from_str(&encoded).expect("deserialize command");
        assert_eq!(decoded, result);
    }

    #[test]
    fn verify_result_roundtrip_preserves_tier_outcome_and_commands() {
        let result = VerifyResult {
            tier: VerifyTier::Quick,
            outcome: VerifyOutcome::Passed,
            started_at: Utc
                .with_ymd_and_hms(2026, 2, 8, 18, 1, 0)
                .single()
                .expect("valid started_at"),
            finished_at: Utc
                .with_ymd_and_hms(2026, 2, 8, 18, 2, 30)
                .single()
                .expect("valid finished_at"),
            commands: vec![VerifyCommandResult {
                command: PreparedVerifyCommand {
                    original: "just lint".to_string(),
                    effective: "nix develop -c just lint".to_string(),
                    wrapped_with_dev_shell: true,
                },
                outcome: VerifyOutcome::Passed,
                failure_class: None,
                exit_code: Some(0),
                started_at: Utc
                    .with_ymd_and_hms(2026, 2, 8, 18, 1, 10)
                    .single()
                    .expect("valid command started_at"),
                finished_at: Utc
                    .with_ymd_and_hms(2026, 2, 8, 18, 1, 40)
                    .single()
                    .expect("valid command finished_at"),
                stdout: "ok".to_string(),
                stderr: String::new(),
            }],
        };

        let encoded = serde_json::to_string(&result).expect("serialize verify result");
        let decoded: VerifyResult =
            serde_json::from_str(&encoded).expect("deserialize verify result");
        assert_eq!(decoded, result);
    }
}
