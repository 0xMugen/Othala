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
