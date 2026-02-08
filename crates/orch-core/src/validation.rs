use serde::{Deserialize, Serialize};

use crate::config::{OrgConfig, RepoConfig};
use crate::state::ReviewPolicy;
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

        if matches!(self.models.policy, ReviewPolicy::Adaptive)
            && self.models.enabled.len() >= 2
            && self.models.min_approvals < 2
        {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "models.min_approvals.too_low",
                message: "adaptive policy requires min_approvals >= 2 when two or more models are enabled".to_string(),
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

        if self.verify.quick.commands.is_empty() {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "verify.quick.commands.empty",
                message: "verify.quick.commands must contain at least one command".to_string(),
            });
        }

        if self.verify.full.commands.is_empty() {
            issues.push(ValidationIssue {
                level: ValidationLevel::Warning,
                code: "verify.full.commands.empty",
                message: "verify.full.commands is empty; merge sandbox full verification will be unavailable".to_string(),
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
        ConcurrencyConfig, GraphiteOrgConfig, ModelsConfig, MovePolicy, NixConfig, OrgConfig,
        RepoConfig, RepoGraphiteConfig, UiConfig, VerifyCommands, VerifyConfig,
    };
    use crate::state::ReviewPolicy;
    use crate::types::{ModelKind, RepoId, SubmitMode, TaskId, TaskRole, TaskSpec, TaskType};
    use std::path::PathBuf;

    fn valid_org_config() -> OrgConfig {
        OrgConfig {
            models: ModelsConfig {
                enabled: vec![ModelKind::Claude, ModelKind::Codex],
                policy: ReviewPolicy::Adaptive,
                min_approvals: 2,
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
                quick: VerifyCommands {
                    commands: vec!["nix develop -c cargo test".to_string()],
                },
                full: VerifyCommands {
                    commands: vec!["nix develop -c cargo test --all-targets".to_string()],
                },
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
            task_id: TaskId("T123".to_string()),
            title: "Add profile endpoint".to_string(),
            task_type: TaskType::Feature,
            role: TaskRole::General,
            preferred_model: Some(ModelKind::Codex),
            depends_on: vec![TaskId("T100".to_string())],
            submit_mode: Some(SubmitMode::Single),
        }
    }

    #[test]
    fn org_config_validation_reports_expected_errors() {
        let mut config = valid_org_config();
        config.concurrency.per_repo = 0;
        config.models.min_approvals = 1;

        let issues = config.validate();
        assert_eq!(issues.len(), 2);

        assert!(issues.iter().any(|issue| {
            issue.level == ValidationLevel::Error && issue.code == "concurrency.per_repo.zero"
        }));
        assert!(issues.iter().any(|issue| {
            issue.level == ValidationLevel::Error && issue.code == "models.min_approvals.too_low"
        }));
    }

    #[test]
    fn org_config_validation_allows_single_model_with_min_approvals_one() {
        let mut config = valid_org_config();
        config.models.enabled = vec![ModelKind::Claude];
        config.models.min_approvals = 1;

        let issues = config.validate();
        assert!(issues.is_empty());
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
    fn org_config_validation_allows_low_min_approvals_in_strict_policy() {
        let mut config = valid_org_config();
        config.models.policy = ReviewPolicy::Strict;
        config.models.min_approvals = 1;

        let issues = config.validate();
        assert!(issues
            .iter()
            .all(|issue| issue.code != "models.min_approvals.too_low"));
    }

    #[test]
    fn repo_config_validation_reports_errors_and_warning() {
        let mut config = valid_repo_config();
        config.repo_id = "  ".to_string();
        config.base_branch = "".to_string();
        config.verify.quick.commands.clear();
        config.verify.full.commands.clear();

        let issues = config.validate();
        assert_eq!(issues.len(), 4);

        assert!(issues.iter().any(|issue| {
            issue.level == ValidationLevel::Error && issue.code == "repo.repo_id.empty"
        }));
        assert!(issues.iter().any(|issue| {
            issue.level == ValidationLevel::Error && issue.code == "repo.base_branch.empty"
        }));
        assert!(issues.iter().any(|issue| {
            issue.level == ValidationLevel::Error && issue.code == "verify.quick.commands.empty"
        }));
        assert!(issues.iter().any(|issue| {
            issue.level == ValidationLevel::Warning && issue.code == "verify.full.commands.empty"
        }));
    }

    #[test]
    fn task_spec_validation_reports_missing_identifiers_and_title() {
        let mut spec = valid_task_spec();
        spec.task_id = TaskId(" ".to_string());
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
