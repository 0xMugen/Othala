use std::collections::HashMap;
use std::path::{Path, PathBuf};

use orch_core::types::{SubmitMode, TaskId};

use crate::command::{AllowedAutoCommand, GraphiteCli};
use crate::error::GraphiteError;
use crate::types::{
    infer_task_dependencies_from_stack, parse_gt_log_short, GraphiteStackSnapshot,
    GraphiteStatusSnapshot, InferredStackDependency,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphiteClient {
    pub cli: GraphiteCli,
    pub repo_root: PathBuf,
}

impl GraphiteClient {
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            cli: GraphiteCli::default(),
            repo_root: repo_root.into(),
        }
    }

    pub fn with_cli(repo_root: impl Into<PathBuf>, cli: GraphiteCli) -> Self {
        Self {
            cli,
            repo_root: repo_root.into(),
        }
    }

    pub fn create_branch(&self, branch: &str) -> Result<(), GraphiteError> {
        if branch.trim().is_empty() {
            return Err(GraphiteError::ContractViolation {
                message: "branch name for gt create must not be empty".to_string(),
            });
        }
        self.cli.run_allowed(
            self.repo_root.as_path(),
            AllowedAutoCommand::Create,
            ["create", branch],
        )?;
        Ok(())
    }

    pub fn restack(&self) -> Result<(), GraphiteError> {
        self.cli.run_allowed(
            self.repo_root.as_path(),
            AllowedAutoCommand::Restack,
            ["restack"],
        )?;
        Ok(())
    }

    pub fn begin_conflict_resolution(&self) -> Result<(), GraphiteError> {
        self.cli.run_allowed(
            self.repo_root.as_path(),
            AllowedAutoCommand::AddAllForConflict,
            ["add", "-A"],
        )?;
        Ok(())
    }

    pub fn continue_conflict_resolution(&self) -> Result<(), GraphiteError> {
        self.cli.run_allowed(
            self.repo_root.as_path(),
            AllowedAutoCommand::ContinueConflict,
            ["continue"],
        )?;
        Ok(())
    }

    pub fn status_snapshot(&self) -> Result<GraphiteStatusSnapshot, GraphiteError> {
        let output = self.cli.run_allowed(
            self.repo_root.as_path(),
            AllowedAutoCommand::Status,
            ["status"],
        )?;
        Ok(GraphiteStatusSnapshot {
            captured_at: chrono::Utc::now(),
            raw: output.stdout,
        })
    }

    pub fn log_short_snapshot(&self) -> Result<GraphiteStackSnapshot, GraphiteError> {
        let output = self.cli.run_allowed(
            self.repo_root.as_path(),
            AllowedAutoCommand::LogShort,
            ["log", "short"],
        )?;
        Ok(parse_gt_log_short(&output.stdout))
    }

    pub fn infer_stack_dependencies(
        &self,
        branch_to_task: &HashMap<String, TaskId>,
    ) -> Result<Vec<InferredStackDependency>, GraphiteError> {
        let snapshot = self.log_short_snapshot()?;
        Ok(infer_task_dependencies_from_stack(&snapshot, branch_to_task))
    }

    pub fn submit(&self, mode: SubmitMode) -> Result<(), GraphiteError> {
        match mode {
            SubmitMode::Single => {
                self.cli.run_allowed(
                    self.repo_root.as_path(),
                    AllowedAutoCommand::Submit,
                    ["submit"],
                )?;
            }
            SubmitMode::Stack => {
                self.cli.run_allowed(
                    self.repo_root.as_path(),
                    AllowedAutoCommand::SubmitStack,
                    ["submit", "--stack"],
                )?;
            }
        }
        Ok(())
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }
}
