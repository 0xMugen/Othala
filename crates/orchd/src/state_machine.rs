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
        (Running, Restacking | VerifyingQuick | NeedsHuman | Failed | Paused) => true,
        (Restacking, VerifyingQuick | RestackConflict | Failed | Paused) => true,
        (RestackConflict, Restacking | NeedsHuman | Failed | Paused) => true,
        (VerifyingQuick, Reviewing | Failed | NeedsHuman | Paused) => true,
        (VerifyingFull, AwaitingMerge | Failed | NeedsHuman | Paused) => true,
        (Reviewing, Ready | Running | NeedsHuman | Failed | Paused) => true,
        (Ready, Submitting | AwaitingMerge | Failed | Paused) => true,
        (Submitting, AwaitingMerge | Failed | Paused) => true,
        (AwaitingMerge, Merged | Running | Failed | Paused) => true,
        (NeedsHuman, Running | Paused | Failed) => true,
        (Paused, Running | Failed) => true,
        (Failed, Running | Paused) => true,
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
