//! Auto-stack pipeline — orchestrates the sequence of operations needed to
//! stack a task's branch on its parent, verify, and submit.
//!
//! Pipeline stages: VerifyBranch → StackOnParent → VerifyStack → Submit

use orch_core::types::{SubmitMode, TaskId};
use std::path::PathBuf;

/// Pipeline stage identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PipelineStage {
    /// Run verification on the task's own branch.
    VerifyBranch,
    /// Stack the branch onto its parent (rebase/move).
    StackOnParent,
    /// Re-run verification after stacking to catch integration issues.
    VerifyStack,
    /// Submit the PR via Graphite.
    Submit,
    /// Pipeline completed successfully.
    Done,
    /// Pipeline failed at some stage.
    Failed,
}

impl std::fmt::Display for PipelineStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineStage::VerifyBranch => write!(f, "verify_branch"),
            PipelineStage::StackOnParent => write!(f, "stack_on_parent"),
            PipelineStage::VerifyStack => write!(f, "verify_stack"),
            PipelineStage::Submit => write!(f, "submit"),
            PipelineStage::Done => write!(f, "done"),
            PipelineStage::Failed => write!(f, "failed"),
        }
    }
}

/// Tracks the state of a pipeline run for one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineState {
    pub task_id: TaskId,
    /// Current stage in the pipeline.
    pub stage: PipelineStage,
    /// The branch to stack on top of (parent's branch).
    pub parent_branch: Option<String>,
    /// The task's own branch name.
    pub branch_name: String,
    /// Path to the worktree for this task.
    pub worktree_path: PathBuf,
    /// Submit mode to use.
    pub submit_mode: SubmitMode,
    /// Error message if pipeline failed.
    pub error: Option<String>,
}

impl PipelineState {
    pub fn new(
        task_id: TaskId,
        branch_name: String,
        worktree_path: PathBuf,
        submit_mode: SubmitMode,
        parent_branch: Option<String>,
    ) -> Self {
        Self {
            task_id,
            stage: PipelineStage::VerifyBranch,
            parent_branch,
            branch_name,
            worktree_path,
            submit_mode,
            error: None,
        }
    }

    /// Is the pipeline finished (either done or failed)?
    pub fn is_terminal(&self) -> bool {
        matches!(self.stage, PipelineStage::Done | PipelineStage::Failed)
    }

    /// Advance to the next stage after a successful step.
    pub fn advance(&mut self) {
        self.stage = match self.stage {
            PipelineStage::VerifyBranch => {
                if self.parent_branch.is_some() {
                    PipelineStage::StackOnParent
                } else {
                    // No parent to stack on — skip straight to submit.
                    PipelineStage::Submit
                }
            }
            PipelineStage::StackOnParent => PipelineStage::VerifyStack,
            PipelineStage::VerifyStack => PipelineStage::Submit,
            PipelineStage::Submit => PipelineStage::Done,
            PipelineStage::Done | PipelineStage::Failed => self.stage,
        };
    }

    /// Mark the pipeline as failed with an error message.
    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.stage = PipelineStage::Failed;
    }
}

/// Action returned by the pipeline, telling the daemon what to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineAction {
    /// Run verification command in the worktree.
    RunVerify {
        task_id: TaskId,
        worktree_path: PathBuf,
    },
    /// Stack the branch onto a parent branch via Graphite.
    StackOnParent {
        task_id: TaskId,
        worktree_path: PathBuf,
        parent_branch: String,
    },
    /// Submit the branch via Graphite.
    Submit {
        task_id: TaskId,
        worktree_path: PathBuf,
        mode: SubmitMode,
    },
    /// Pipeline is complete — mark task as submitted.
    Complete {
        task_id: TaskId,
    },
    /// Pipeline failed — needs retry or human intervention.
    Failed {
        task_id: TaskId,
        stage: PipelineStage,
        error: String,
    },
}

/// Determine the next action for a pipeline given its current state.
///
/// This is a pure function — the daemon is responsible for executing the
/// action and calling `advance()` or `fail()` on success/failure.
pub fn next_action(state: &PipelineState) -> PipelineAction {
    match state.stage {
        PipelineStage::VerifyBranch | PipelineStage::VerifyStack => PipelineAction::RunVerify {
            task_id: state.task_id.clone(),
            worktree_path: state.worktree_path.clone(),
        },
        PipelineStage::StackOnParent => {
            let parent = state
                .parent_branch
                .clone()
                .unwrap_or_else(|| "main".to_string());
            PipelineAction::StackOnParent {
                task_id: state.task_id.clone(),
                worktree_path: state.worktree_path.clone(),
                parent_branch: parent,
            }
        }
        PipelineStage::Submit => PipelineAction::Submit {
            task_id: state.task_id.clone(),
            worktree_path: state.worktree_path.clone(),
            mode: state.submit_mode,
        },
        PipelineStage::Done => PipelineAction::Complete {
            task_id: state.task_id.clone(),
        },
        PipelineStage::Failed => PipelineAction::Failed {
            task_id: state.task_id.clone(),
            stage: state.stage,
            error: state.error.clone().unwrap_or_default(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_pipeline(parent: Option<&str>) -> PipelineState {
        PipelineState::new(
            TaskId::new("T-1"),
            "task/T-1".to_string(),
            PathBuf::from(".orch/wt/T-1"),
            SubmitMode::Single,
            parent.map(|s| s.to_string()),
        )
    }

    #[test]
    fn pipeline_starts_at_verify_branch() {
        let p = mk_pipeline(Some("task/T-0"));
        assert_eq!(p.stage, PipelineStage::VerifyBranch);
    }

    #[test]
    fn full_pipeline_with_parent() {
        let mut p = mk_pipeline(Some("task/T-0"));

        // VerifyBranch -> StackOnParent
        assert!(matches!(
            next_action(&p),
            PipelineAction::RunVerify { .. }
        ));
        p.advance();
        assert_eq!(p.stage, PipelineStage::StackOnParent);

        // StackOnParent -> VerifyStack
        assert!(matches!(
            next_action(&p),
            PipelineAction::StackOnParent { .. }
        ));
        p.advance();
        assert_eq!(p.stage, PipelineStage::VerifyStack);

        // VerifyStack -> Submit
        assert!(matches!(
            next_action(&p),
            PipelineAction::RunVerify { .. }
        ));
        p.advance();
        assert_eq!(p.stage, PipelineStage::Submit);

        // Submit -> Done
        assert!(matches!(next_action(&p), PipelineAction::Submit { .. }));
        p.advance();
        assert_eq!(p.stage, PipelineStage::Done);
        assert!(p.is_terminal());
    }

    #[test]
    fn pipeline_without_parent_skips_stack() {
        let mut p = mk_pipeline(None);

        // VerifyBranch -> Submit (skip StackOnParent)
        p.advance();
        assert_eq!(p.stage, PipelineStage::Submit);

        // Submit -> Done
        p.advance();
        assert_eq!(p.stage, PipelineStage::Done);
    }

    #[test]
    fn pipeline_failure() {
        let mut p = mk_pipeline(Some("task/T-0"));
        p.fail("verification failed: cargo test had 3 failures".to_string());

        assert_eq!(p.stage, PipelineStage::Failed);
        assert!(p.is_terminal());
        assert!(p.error.as_ref().unwrap().contains("3 failures"));

        assert!(matches!(next_action(&p), PipelineAction::Failed { .. }));
    }

    #[test]
    fn pipeline_stage_display() {
        assert_eq!(PipelineStage::VerifyBranch.to_string(), "verify_branch");
        assert_eq!(PipelineStage::StackOnParent.to_string(), "stack_on_parent");
        assert_eq!(PipelineStage::Submit.to_string(), "submit");
        assert_eq!(PipelineStage::Done.to_string(), "done");
        assert_eq!(PipelineStage::Failed.to_string(), "failed");
    }

    #[test]
    fn done_and_failed_are_terminal() {
        let mut p = mk_pipeline(None);
        assert!(!p.is_terminal());

        p.stage = PipelineStage::Done;
        assert!(p.is_terminal());

        p.stage = PipelineStage::Failed;
        assert!(p.is_terminal());
    }

    #[test]
    fn advance_from_terminal_is_idempotent() {
        let mut p = mk_pipeline(None);
        p.stage = PipelineStage::Done;
        p.advance();
        assert_eq!(p.stage, PipelineStage::Done);

        p.stage = PipelineStage::Failed;
        p.advance();
        assert_eq!(p.stage, PipelineStage::Failed);
    }
}
