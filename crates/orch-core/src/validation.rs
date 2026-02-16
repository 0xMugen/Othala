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
                message: "no models enabled — daemon cannot spawn agents".to_string(),
            });
        }

        if !self.models.enabled.is_empty() {
            if let Some(default) = &self.models.default {
                if !self.models.enabled.contains(default) {
                    issues.push(ValidationIssue {
                        level: ValidationLevel::Warning,
                        code: "models.default.not_enabled",
                        message: format!("default model {default:?} is not in the enabled list"),
                    });
                }
            }
        }

        for model in &self.models.enabled {
            let concurrency = match model {
                crate::types::ModelKind::Claude => self.concurrency.claude,
                crate::types::ModelKind::Codex => self.concurrency.codex,
                crate::types::ModelKind::Gemini => self.concurrency.gemini,
            };
            if concurrency == 0 {
                issues.push(ValidationIssue {
                    level: ValidationLevel::Error,
                    code: "concurrency.model.zero",
                    message: format!(
                        "concurrency is 0 for enabled model {model:?} — it can never be scheduled"
                    ),
                });
            }
        }

        if self.concurrency.per_repo == 0 {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "concurrency.per_repo.zero",
                message: "per_repo concurrency must be greater than zero".to_string(),
            });
        }

        if self.daemon.tick_interval_secs == 0 {
            issues.push(ValidationIssue {
                level: ValidationLevel::Error,
                code: "daemon.tick_interval.zero",
                message: "tick interval cannot be 0".to_string(),
            });
        }

        if self.daemon.agent_timeout_secs > 0 && self.daemon.agent_timeout_secs < 30 {
            issues.push(ValidationIssue {
                level: ValidationLevel::Warning,
                code: "daemon.agent_timeout.low",
                message: format!(
                    "agent timeout {}s is very low — agents may be killed before producing useful output",
                    self.daemon.agent_timeout_secs
                ),
            });
        }

        if self.notifications.slack_channel.is_some()
            && self.notifications.slack_webhook_url.is_none()
        {
            issues.push(ValidationIssue {
                level: ValidationLevel::Warning,
                code: "notifications.slack.incomplete",
                message: "slack_channel is set but slack_webhook_url is missing".to_string(),
            });
        }

        if let Some(url) = &self.notifications.webhook_url {
            if !url.starts_with("http://") && !url.starts_with("https://") {
                issues.push(ValidationIssue {
                    level: ValidationLevel::Warning,
                    code: "notifications.webhook_url.invalid",
                    message: "webhook URL should start with http:// or https://".to_string(),
                });
            }
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
        BudgetConfig, ConcurrencyConfig, DaemonOrgConfig, GraphiteOrgConfig, ModelsConfig,
        MovePolicy, NixConfig, NotificationConfig, OrgConfig, RepoConfig, RepoGraphiteConfig,
        UiConfig, VerifyConfig,
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
            daemon: DaemonOrgConfig::default(),
            budget: BudgetConfig::default(),
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
