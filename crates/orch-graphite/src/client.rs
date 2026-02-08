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
pub enum RestackOutcome {
    Restacked,
    Conflict { stdout: String, stderr: String },
}

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

    pub fn restack_with_outcome(&self) -> Result<RestackOutcome, GraphiteError> {
        let result = self.cli.run_allowed(
            self.repo_root.as_path(),
            AllowedAutoCommand::Restack,
            ["restack"],
        );
        classify_restack_result(result)
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
        Ok(infer_task_dependencies_from_stack(
            &snapshot,
            branch_to_task,
        ))
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

fn classify_restack_result(
    result: Result<crate::command::GraphiteOutput, GraphiteError>,
) -> Result<RestackOutcome, GraphiteError> {
    match result {
        Ok(_) => Ok(RestackOutcome::Restacked),
        Err(err @ GraphiteError::CommandFailed { .. }) if err.is_restack_conflict() => {
            if let GraphiteError::CommandFailed { stdout, stderr, .. } = err {
                Ok(RestackOutcome::Conflict { stdout, stderr })
            } else {
                unreachable!("guard guarantees CommandFailed");
            }
        }
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use orch_core::types::SubmitMode;

    use crate::command::GraphiteCli;
    use crate::command::GraphiteOutput;
    use crate::error::GraphiteError;

    use super::{classify_restack_result, GraphiteClient, RestackOutcome};

    #[test]
    fn classifies_successful_restack() {
        let outcome = classify_restack_result(Ok(GraphiteOutput {
            stdout: "ok".to_string(),
            stderr: "".to_string(),
        }))
        .expect("classify");
        assert_eq!(outcome, RestackOutcome::Restacked);
    }

    #[test]
    fn classifies_conflict_restack_failure() {
        let outcome = classify_restack_result(Err(GraphiteError::CommandFailed {
            command: "gt restack".to_string(),
            status: Some(1),
            stdout: "".to_string(),
            stderr: "CONFLICT (content)".to_string(),
        }))
        .expect("conflict becomes typed outcome");

        assert_eq!(
            outcome,
            RestackOutcome::Conflict {
                stdout: "".to_string(),
                stderr: "CONFLICT (content)".to_string(),
            }
        );
    }

    #[test]
    fn preserves_non_conflict_errors() {
        let err = classify_restack_result(Err(GraphiteError::ContractViolation {
            message: "bad args".to_string(),
        }))
        .expect_err("must be error");
        assert!(matches!(err, GraphiteError::ContractViolation { .. }));
    }

    #[test]
    fn preserves_non_conflict_command_failed_errors() {
        let err = classify_restack_result(Err(GraphiteError::CommandFailed {
            command: "gt restack".to_string(),
            status: Some(1),
            stdout: "".to_string(),
            stderr: "authentication failed".to_string(),
        }))
        .expect_err("non-conflict command failure should be preserved");
        assert!(matches!(err, GraphiteError::CommandFailed { .. }));
    }

    #[test]
    fn create_branch_rejects_blank_name_before_cli_invocation() {
        let client = GraphiteClient::with_cli(
            PathBuf::from("."),
            GraphiteCli::new("/definitely/missing/gt"),
        );
        let err = client
            .create_branch("   ")
            .expect_err("blank branch must fail contract check");
        assert!(matches!(err, GraphiteError::ContractViolation { .. }));
    }

    #[test]
    fn submit_stack_mode_passes_stack_flag_to_cli_command() {
        let client = GraphiteClient::with_cli(
            PathBuf::from("."),
            GraphiteCli::new("/definitely/missing/gt"),
        );
        let err = client
            .submit(SubmitMode::Stack)
            .expect_err("missing binary should surface io error");
        match err {
            GraphiteError::Io { command, .. } => {
                assert!(command.contains("submit"));
                assert!(command.contains("--stack"));
            }
            other => panic!("expected io error, got {other:?}"),
        }
    }

    #[test]
    fn submit_single_mode_does_not_include_stack_flag() {
        let client = GraphiteClient::with_cli(
            PathBuf::from("."),
            GraphiteCli::new("/definitely/missing/gt"),
        );
        let err = client
            .submit(SubmitMode::Single)
            .expect_err("missing binary should surface io error");
        match err {
            GraphiteError::Io { command, .. } => {
                assert!(command.contains("submit"));
                assert!(!command.contains("--stack"));
            }
            other => panic!("expected io error, got {other:?}"),
        }
    }

    #[test]
    fn conflict_resolution_commands_use_expected_gt_subcommands() {
        let client = GraphiteClient::with_cli(
            PathBuf::from("."),
            GraphiteCli::new("/definitely/missing/gt"),
        );

        let err = client
            .begin_conflict_resolution()
            .expect_err("missing binary should surface io error");
        match err {
            GraphiteError::Io { command, .. } => {
                assert!(command.contains("add"));
                assert!(command.contains("-A"));
            }
            other => panic!("expected io error, got {other:?}"),
        }

        let err = client
            .continue_conflict_resolution()
            .expect_err("missing binary should surface io error");
        match err {
            GraphiteError::Io { command, .. } => {
                assert!(command.contains("continue"));
            }
            other => panic!("expected io error, got {other:?}"),
        }
    }
}
