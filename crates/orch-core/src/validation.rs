//! Validation for MVP orchestrator types.

use serde::{Deserialize, Serialize};

use crate::config::{OrgConfig, RepoConfig};
use crate::types::TaskSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationLevel {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub level: ValidationLevel,
    pub code: &'static str,
    pub message: String,
}

pub trait Validate {
    fn validate(&self) -> Vec<ValidationIssue>;
}

impl Validate for OrgConfig {
    fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();

        if self.models.enabled.is_empty() {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "models.enabled.empty",
                message: "at least one model must be enabled".to_string(),
            });
        }

        if self.concurrency.per_repo == 0 {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "concurrency.per_repo.zero",
                message: "per_repo concurrency must be greater than zero".to_string(),
            });
        }

        issues
    }
}

impl Validate for RepoConfig {
    fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();

        if self.repo_id.trim().is_empty() {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "repo.repo_id.empty",
                message: "repo_id must not be empty".to_string(),
            });
        }

        if self.base_branch.trim().is_empty() {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "repo.base_branch.empty",
                message: "base_branch must not be empty".to_string(),
            });
        }

        if self.verify.command.trim().is_empty() {
            issues.push(ValidationIssue {
                level: ValidationLevel::Warning,
                code: "verify.command.empty",
                message: "verify command is empty; verification will be skipped".to_string(),
            });
        }

        issues
    }
}

impl Validate for TaskSpec {
    fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();

        if self.task_id.0.trim().is_empty() {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "task.task_id.empty",
                message: "task_id must not be empty".to_string(),
            });
        }

        if self.repo_id.0.trim().is_empty() {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "task.repo_id.empty",
                message: "repo_id must not be empty".to_string(),
            });
        }

        if self.title.trim().is_empty() {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "task.title.empty",
                message: "title must not be empty".to_string(),
            });
        }

        issues
    }
}

#[cfg(test)]
mod tests {
    use super::{Validate, ValidationLevel};
    use crate::config::{
        ConcurrencyConfig, GraphiteOrgConfig, ModelsConfig, MovePolicy, NixConfig,
        NotificationConfig, OrgConfig, RepoConfig, RepoGraphiteConfig, UiConfig, VerifyConfig,
    };
    use crate::types::{ModelKind, RepoId, SubmitMode, TaskId, TaskSpec};
    use std::path::PathBuf;

    fn valid_org_config() -> OrgConfig {
        OrgConfig {
            models: ModelsConfig {
                enabled: vec![ModelKind::Claude, ModelKind::Codex],
                default: Some(ModelKind::Claude),
            },
            concurrency: ConcurrencyConfig {
                per_repo: 10,
                claude: 10,
                codex: 10,
                gemini: 10,
            },
            graphite: GraphiteOrgConfig {
                auto_submit: true,
                submit_mode_default: SubmitMode::Single,
                allow_move: MovePolicy::Manual,
            },
            ui: UiConfig {
                web_bind: "127.0.0.1:9842".to_string(),
            },
            notifications: NotificationConfig::default(),
        }
    }

    fn valid_repo_config() -> RepoConfig {
        RepoConfig {
            repo_id: "example".to_string(),
            repo_path: PathBuf::from("/tmp/example"),
            base_branch: "main".to_string(),
            nix: NixConfig {
                dev_shell: "nix develop".to_string(),
            },
            verify: VerifyConfig {
                command: "cargo check && cargo test".to_string(),
            },
            graphite: RepoGraphiteConfig {
                draft_on_start: true,
                submit_mode: Some(SubmitMode::Single),
            },
        }
    }

    fn valid_task_spec() -> TaskSpec {
        TaskSpec {
            repo_id: RepoId("example".to_string()),
            task_id: TaskId::new("T123"),
            title: "Add profile endpoint".to_string(),
            preferred_model: Some(ModelKind::Codex),
            depends_on: vec![TaskId::new("T100")],
            submit_mode: Some(SubmitMode::Single),
        }
    }

    #[test]
    fn org_config_validation_reports_expected_errors() {
        let mut config = valid_org_config();
        config.concurrency.per_repo = 0;

        let issues = config.validate();
        assert_eq!(issues.len(), 1);
        assert!(issues.iter().any(|issue| {
            issue.level == ValidationLevel::Error && issue.code == "concurrency.per_repo.zero"
        }));
    }

    #[test]
    fn org_config_validation_reports_empty_enabled_models() {
        let mut config = valid_org_config();
        config.models.enabled.clear();

        let issues = config.validate();
        assert_eq!(issues.len(), 1);
        assert!(issues.iter().any(|issue| {
            issue.level == ValidationLevel::Error && issue.code == "models.enabled.empty"
        }));
    }

    #[test]
    fn repo_config_validation_reports_errors() {
        let mut config = valid_repo_config();
        config.repo_id = "  ".to_string();
        config.base_branch = "".to_string();

        let issues = config.validate();
        assert_eq!(issues.len(), 2);

        assert!(issues.iter().any(|issue| {
            issue.level == ValidationLevel::Error && issue.code == "repo.repo_id.empty"
        }));
        assert!(issues.iter().any(|issue| {
            issue.level == ValidationLevel::Error && issue.code == "repo.base_branch.empty"
        }));
    }

    #[test]
    fn task_spec_validation_reports_missing_identifiers_and_title() {
        let mut spec = valid_task_spec();
        spec.task_id = TaskId::new(" ");
        spec.repo_id = RepoId("".to_string());
        spec.title = "   ".to_string();

        let issues = spec.validate();
        assert_eq!(issues.len(), 3);

        assert!(issues
            .iter()
            .any(|issue| issue.code == "task.task_id.empty"));
        assert!(issues
            .iter()
            .any(|issue| issue.code == "task.repo_id.empty"));
        assert!(issues.iter().any(|issue| issue.code == "task.title.empty"));
    }
}
