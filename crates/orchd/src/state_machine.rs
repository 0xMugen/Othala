//! MVP state machine for task transitions.

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

/// Transition a task to a new state.
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

/// Check if a state transition is valid.
///
/// MVP state transitions:
/// ```text
/// Chatting → Ready → Submitting → AwaitingMerge → Merged
///                ↓
///           Restacking
/// ```
pub fn is_transition_allowed(from: TaskState, to: TaskState) -> bool {
    use TaskState::*;

    if from == to {
        return true;
    }

    match (from, to) {
        // Normal flow: Chatting → Ready
        (Chatting, Ready) => true,
        // Ready → Submitting (auto-submit)
        (Ready, Submitting) => true,
        // Ready → Restacking (parent merged, need rebase)
        (Ready, Restacking) => true,
        // Submitting → AwaitingMerge (submit success)
        (Submitting, AwaitingMerge) => true,
        // Submitting → Restacking (stack needs rebase during submit)
        (Submitting, Restacking) => true,
        // Restacking → Ready (restack complete)
        (Restacking, Ready) => true,
        // AwaitingMerge → Merged (PR merged)
        (AwaitingMerge, Merged) => true,
        // AwaitingMerge → Restacking (parent merged, need rebase)
        (AwaitingMerge, Restacking) => true,
        // Any state can go back to Chatting (retry/fix)
        (_, Chatting) => true,
        _ => false,
    }
}

/// Get a string tag for a task state (for event logging).
///
/// Delegates to `TaskState::Display`. Kept for backward compatibility.
pub fn task_state_tag(state: TaskState) -> &'static str {
    match state {
        TaskState::Chatting => "CHATTING",
        TaskState::Ready => "READY",
        TaskState::Submitting => "SUBMITTING",
        TaskState::Restacking => "RESTACKING",
        TaskState::AwaitingMerge => "AWAITING_MERGE",
        TaskState::Merged => "MERGED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mk_task(state: TaskState) -> Task {
        let mut task = Task::new(
            orch_core::TaskId::new("T1"),
            orch_core::RepoId("example".to_string()),
            "Test task".to_string(),
            PathBuf::from(".orch/wt/T1"),
        );
        task.state = state;
        task
    }

    #[test]
    fn allows_normal_flow_transitions() {
        assert!(is_transition_allowed(TaskState::Chatting, TaskState::Ready));
        assert!(is_transition_allowed(
            TaskState::Ready,
            TaskState::Submitting
        ));
        assert!(is_transition_allowed(
            TaskState::Submitting,
            TaskState::AwaitingMerge
        ));
        assert!(is_transition_allowed(
            TaskState::AwaitingMerge,
            TaskState::Merged
        ));
    }

    #[test]
    fn allows_restacking_transitions() {
        assert!(is_transition_allowed(
            TaskState::Ready,
            TaskState::Restacking
        ));
        assert!(is_transition_allowed(
            TaskState::Submitting,
            TaskState::Restacking
        ));
        assert!(is_transition_allowed(
            TaskState::AwaitingMerge,
            TaskState::Restacking
        ));
        assert!(is_transition_allowed(
            TaskState::Restacking,
            TaskState::Ready
        ));
    }

    #[test]
    fn allows_retry_to_chatting() {
        assert!(is_transition_allowed(TaskState::Ready, TaskState::Chatting));
        assert!(is_transition_allowed(
            TaskState::Submitting,
            TaskState::Chatting
        ));
        assert!(is_transition_allowed(
            TaskState::Merged,
            TaskState::Chatting
        ));
    }

    #[test]
    fn disallows_invalid_transitions() {
        assert!(!is_transition_allowed(
            TaskState::Chatting,
            TaskState::Merged
        ));
        assert!(!is_transition_allowed(TaskState::Ready, TaskState::Merged));
        assert!(!is_transition_allowed(
            TaskState::Restacking,
            TaskState::Merged
        ));
    }

    #[test]
    fn transition_updates_task_state() {
        let mut task = mk_task(TaskState::Chatting);
        let at = Utc::now();
        let result = transition_task(&mut task, TaskState::Ready, at).expect("valid transition");

        assert_eq!(result.from, TaskState::Chatting);
        assert_eq!(result.to, TaskState::Ready);
        assert_eq!(task.state, TaskState::Ready);
        assert_eq!(task.updated_at, at);
    }

    #[test]
    fn transition_rejects_invalid() {
        let mut task = mk_task(TaskState::Chatting);
        let at = Utc::now();
        let err = transition_task(&mut task, TaskState::Merged, at).expect_err("should fail");

        assert!(matches!(
            err,
            StateMachineError::InvalidTransition {
                from: TaskState::Chatting,
                to: TaskState::Merged
            }
        ));
        assert_eq!(task.state, TaskState::Chatting);
    }

    #[test]
    fn self_transition_allowed() {
        assert!(is_transition_allowed(
            TaskState::Chatting,
            TaskState::Chatting
        ));
        assert!(is_transition_allowed(TaskState::Merged, TaskState::Merged));
    }
}
