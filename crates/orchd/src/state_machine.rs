use chrono::{DateTime, Utc};
use orch_core::state::TaskState;
use orch_core::types::Task;

#[derive(Debug, thiserror::Error)]
pub enum StateMachineError {
    #[error("invalid task state transition: {from:?} -> {to:?}")]
    InvalidTransition { from: TaskState, to: TaskState },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateTransition {
    pub from: TaskState,
    pub to: TaskState,
    pub at: DateTime<Utc>,
}

pub fn transition_task(
    task: &mut Task,
    to: TaskState,
    at: DateTime<Utc>,
) -> Result<StateTransition, StateMachineError> {
    let from = task.state;
    if !is_transition_allowed(from, to) {
        return Err(StateMachineError::InvalidTransition { from, to });
    }

    task.state = to;
    task.updated_at = at;

    Ok(StateTransition { from, to, at })
}

pub fn is_transition_allowed(from: TaskState, to: TaskState) -> bool {
    use TaskState::*;

    if from == to {
        return true;
    }

    match (from, to) {
        (Queued, Initializing) => true,
        (Initializing, DraftPrOpen | Failed | Paused) => true,
        (DraftPrOpen, Running | Failed | Paused) => true,
        (
            Running,
            Restacking | VerifyingQuick | VerifyingFull | NeedsHuman | Submitting | Failed
                | Paused,
        ) => true,
        (Restacking, VerifyingQuick | RestackConflict | Failed | Paused) => true,
        (RestackConflict, Restacking | NeedsHuman | Submitting | Failed | Paused) => true,
        (VerifyingQuick, Reviewing | Running | Failed | NeedsHuman | Paused) => true,
        (
            VerifyingFull,
            Running | Reviewing | Ready | AwaitingMerge | Failed | NeedsHuman | Paused,
        ) => true,
        (Reviewing, Ready | Running | VerifyingFull | NeedsHuman | Failed | Paused) => true,
        (Ready, VerifyingFull | Submitting | AwaitingMerge | Failed | Paused) => true,
        (Submitting, AwaitingMerge | Failed | Paused) => true,
        (AwaitingMerge, VerifyingFull | Submitting | Merged | Running | Failed | Paused) => true,
        (NeedsHuman, Running | Paused | Failed) => true,
        (Paused, Running | Failed) => true,
        (Failed, Running | Submitting | Paused) => true,
        _ => false,
    }
}

pub fn task_state_tag(state: TaskState) -> &'static str {
    use TaskState::*;
    match state {
        Queued => "QUEUED",
        Initializing => "INITIALIZING",
        DraftPrOpen => "DRAFT_PR_OPEN",
        Running => "RUNNING",
        Restacking => "RESTACKING",
        RestackConflict => "RESTACK_CONFLICT",
        VerifyingQuick => "VERIFYING_QUICK",
        VerifyingFull => "VERIFYING_FULL",
        Reviewing => "REVIEWING",
        NeedsHuman => "NEEDS_HUMAN",
        Ready => "READY",
        Submitting => "SUBMITTING",
        AwaitingMerge => "AWAITING_MERGE",
        Merged => "MERGED",
        Failed => "FAILED",
        Paused => "PAUSED",
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::state::{ReviewCapacityState, ReviewStatus, VerifyStatus};
    use orch_core::types::{RepoId, SubmitMode, Task, TaskId, TaskRole, TaskType};

    use super::{is_transition_allowed, transition_task};
    use orch_core::state::TaskState;

    fn mk_task(state: TaskState) -> Task {
        Task {
            id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "task".to_string(),
            state,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            branch_name: Some("task/T1".to_string()),
            worktree_path: ".orch/wt/T1".into(),
            pr: None,
            verify_status: VerifyStatus::NotRun,
            review_status: ReviewStatus {
                required_models: Vec::new(),
                approvals_received: 0,
                approvals_required: 0,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn allows_full_verify_from_active_lifecycle_states() {
        assert!(is_transition_allowed(
            TaskState::Running,
            TaskState::VerifyingFull
        ));
        assert!(is_transition_allowed(
            TaskState::Reviewing,
            TaskState::VerifyingFull
        ));
        assert!(is_transition_allowed(
            TaskState::Ready,
            TaskState::VerifyingFull
        ));
        assert!(is_transition_allowed(
            TaskState::AwaitingMerge,
            TaskState::VerifyingFull
        ));
    }

    #[test]
    fn allows_return_from_full_verify_to_prior_progress_states() {
        assert!(is_transition_allowed(
            TaskState::VerifyingFull,
            TaskState::Running
        ));
        assert!(is_transition_allowed(
            TaskState::VerifyingFull,
            TaskState::Reviewing
        ));
        assert!(is_transition_allowed(
            TaskState::VerifyingFull,
            TaskState::Ready
        ));
        assert!(is_transition_allowed(
            TaskState::VerifyingFull,
            TaskState::AwaitingMerge
        ));
    }

    #[test]
    fn disallows_full_verify_from_queued() {
        assert!(!is_transition_allowed(
            TaskState::Queued,
            TaskState::VerifyingFull
        ));
    }

    #[test]
    fn transition_updates_task_state_for_new_verify_full_path() {
        let mut task = mk_task(TaskState::Ready);
        let at = Utc::now();
        let transition =
            transition_task(&mut task, TaskState::VerifyingFull, at).expect("valid transition");
        assert_eq!(transition.from, TaskState::Ready);
        assert_eq!(transition.to, TaskState::VerifyingFull);
        assert_eq!(task.state, TaskState::VerifyingFull);
        assert_eq!(task.updated_at, at);
    }

    #[test]
    fn allows_pause_from_active_states_and_resume_from_failed() {
        assert!(is_transition_allowed(TaskState::Running, TaskState::Paused));
        assert!(is_transition_allowed(
            TaskState::Running,
            TaskState::Submitting
        ));
        assert!(is_transition_allowed(
            TaskState::RestackConflict,
            TaskState::Submitting
        ));
        assert!(is_transition_allowed(
            TaskState::Failed,
            TaskState::Submitting
        ));
        assert!(is_transition_allowed(
            TaskState::Reviewing,
            TaskState::Paused
        ));
        assert!(is_transition_allowed(
            TaskState::Submitting,
            TaskState::Paused
        ));
        assert!(is_transition_allowed(TaskState::Failed, TaskState::Paused));
    }

    #[test]
    fn disallows_invalid_shortcuts_between_lifecycle_states() {
        assert!(!is_transition_allowed(
            TaskState::Queued,
            TaskState::Running
        ));
        assert!(!is_transition_allowed(TaskState::Ready, TaskState::Running));
        assert!(!is_transition_allowed(
            TaskState::Submitting,
            TaskState::Ready
        ));
        assert!(!is_transition_allowed(
            TaskState::Merged,
            TaskState::Running
        ));
        assert!(!is_transition_allowed(
            TaskState::Paused,
            TaskState::Reviewing
        ));
    }

    #[test]
    fn transition_rejects_invalid_target_state() {
        let mut task = mk_task(TaskState::Queued);
        let at = Utc::now();
        let err = transition_task(&mut task, TaskState::Running, at)
            .expect_err("queued -> running should be invalid");
        assert!(matches!(
            err,
            super::StateMachineError::InvalidTransition {
                from: TaskState::Queued,
                to: TaskState::Running
            }
        ));
        assert_eq!(task.state, TaskState::Queued);
    }

    #[test]
    fn transition_allows_noop_self_transition_and_updates_timestamp() {
        let mut task = mk_task(TaskState::Running);
        let at = Utc::now();
        let transition =
            transition_task(&mut task, TaskState::Running, at).expect("self transition");
        assert_eq!(transition.from, TaskState::Running);
        assert_eq!(transition.to, TaskState::Running);
        assert_eq!(task.state, TaskState::Running);
        assert_eq!(task.updated_at, at);
    }

    #[test]
    fn merged_is_terminal_except_self_transition() {
        assert!(is_transition_allowed(TaskState::Merged, TaskState::Merged));
        assert!(!is_transition_allowed(TaskState::Merged, TaskState::Paused));
        assert!(!is_transition_allowed(TaskState::Merged, TaskState::Failed));
        assert!(!is_transition_allowed(
            TaskState::Merged,
            TaskState::Running
        ));
    }

    #[test]
    fn task_state_tag_covers_restack_conflict_and_paused() {
        assert_eq!(
            super::task_state_tag(TaskState::RestackConflict),
            "RESTACK_CONFLICT"
        );
        assert_eq!(super::task_state_tag(TaskState::Paused), "PAUSED");
    }
}
