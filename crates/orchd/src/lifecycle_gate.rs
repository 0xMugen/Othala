use orch_core::state::{ReviewCapacityState, VerifyStatus};
use orch_core::types::{SubmitMode, Task};

use crate::review_gate::ReviewEvaluation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitPolicy {
    pub org_default: SubmitMode,
    pub repo_override: Option<SubmitMode>,
    pub auto_submit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyFailureReason {
    VerifyQuickNotPassed,
    ReviewNotApproved,
    WaitingForReviewCapacity,
    NeedsHumanReviewerCapacity,
    GraphiteHygieneFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyGateInput {
    pub verify_status: VerifyStatus,
    pub review_evaluation: ReviewEvaluation,
    pub graphite_hygiene_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyGateDecision {
    pub ready: bool,
    pub reasons: Vec<ReadyFailureReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitBlockReason {
    NotReady,
    AutoSubmitDisabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoSubmitDecision {
    pub should_submit: bool,
    pub mode: Option<SubmitMode>,
    pub blocked_reason: Option<SubmitBlockReason>,
}

pub fn evaluate_ready_gate(input: &ReadyGateInput) -> ReadyGateDecision {
    let mut reasons = Vec::<ReadyFailureReason>::new();

    let verify_quick_passed = matches!(input.verify_status, VerifyStatus::Passed { tier } if tier == orch_core::state::VerifyTier::Quick);
    if !verify_quick_passed {
        reasons.push(ReadyFailureReason::VerifyQuickNotPassed);
    }

    match input.review_evaluation.requirement.capacity_state {
        ReviewCapacityState::Sufficient => {}
        ReviewCapacityState::WaitingForReviewCapacity => {
            reasons.push(ReadyFailureReason::WaitingForReviewCapacity);
        }
        ReviewCapacityState::NeedsHuman => {
            reasons.push(ReadyFailureReason::NeedsHumanReviewerCapacity);
        }
    }

    if !input.review_evaluation.approved {
        reasons.push(ReadyFailureReason::ReviewNotApproved);
    }

    if !input.graphite_hygiene_ok {
        reasons.push(ReadyFailureReason::GraphiteHygieneFailed);
    }

    reasons.sort_by_key(ready_failure_rank);
    reasons.dedup();

    ReadyGateDecision {
        ready: reasons.is_empty(),
        reasons,
    }
}

pub fn resolve_submit_mode(task: &Task, policy: SubmitPolicy) -> SubmitMode {
    policy.repo_override.unwrap_or(task.submit_mode)
}

pub fn decide_auto_submit(
    task: &Task,
    policy: SubmitPolicy,
    ready_gate: &ReadyGateDecision,
) -> AutoSubmitDecision {
    if !ready_gate.ready {
        return AutoSubmitDecision {
            should_submit: false,
            mode: None,
            blocked_reason: Some(SubmitBlockReason::NotReady),
        };
    }
    if !policy.auto_submit {
        return AutoSubmitDecision {
            should_submit: false,
            mode: None,
            blocked_reason: Some(SubmitBlockReason::AutoSubmitDisabled),
        };
    }

    AutoSubmitDecision {
        should_submit: true,
        mode: Some(resolve_submit_mode(task, policy)),
        blocked_reason: None,
    }
}

fn ready_failure_rank(reason: &ReadyFailureReason) -> u8 {
    match reason {
        ReadyFailureReason::VerifyQuickNotPassed => 0,
        ReadyFailureReason::WaitingForReviewCapacity => 1,
        ReadyFailureReason::NeedsHumanReviewerCapacity => 2,
        ReadyFailureReason::ReviewNotApproved => 3,
        ReadyFailureReason::GraphiteHygieneFailed => 4,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::state::{ReviewCapacityState, TaskState, VerifyStatus};
    use orch_core::types::{RepoId, SubmitMode, Task, TaskRole, TaskType};
    use std::path::PathBuf;

    use crate::review_gate::{ReviewEvaluation, ReviewRequirement};

    use super::{
        decide_auto_submit, evaluate_ready_gate, resolve_submit_mode, AutoSubmitDecision,
        ReadyFailureReason, ReadyGateInput, SubmitBlockReason, SubmitPolicy,
    };

    fn mk_task(submit_mode: SubmitMode) -> Task {
        Task {
            id: orch_core::types::TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Example".to_string(),
            state: TaskState::Ready,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: Vec::new(),
            submit_mode,
            branch_name: Some("task/T1".to_string()),
            worktree_path: PathBuf::from(".orch/wt/T1"),
            pr: None,
            verify_status: VerifyStatus::NotRun,
            review_status: orch_core::state::ReviewStatus {
                required_models: Vec::new(),
                approvals_received: 0,
                approvals_required: 0,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            patch_ready: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn approved_review() -> ReviewEvaluation {
        ReviewEvaluation {
            requirement: ReviewRequirement {
                required_models: vec![orch_core::types::ModelKind::Claude],
                approvals_required: 1,
                unanimous_required: true,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            approvals_received: 1,
            blocking_verdicts: Vec::new(),
            approved: true,
            needs_human: false,
        }
    }

    #[test]
    fn ready_gate_requires_quick_verify_review_and_hygiene() {
        let input = ReadyGateInput {
            verify_status: VerifyStatus::NotRun,
            review_evaluation: approved_review(),
            graphite_hygiene_ok: false,
        };
        let decision = evaluate_ready_gate(&input);
        assert!(!decision.ready);
        assert_eq!(
            decision.reasons,
            vec![
                ReadyFailureReason::VerifyQuickNotPassed,
                ReadyFailureReason::GraphiteHygieneFailed
            ]
        );
    }

    #[test]
    fn ready_gate_passes_when_all_conditions_hold() {
        let input = ReadyGateInput {
            verify_status: VerifyStatus::Passed {
                tier: orch_core::state::VerifyTier::Quick,
            },
            review_evaluation: approved_review(),
            graphite_hygiene_ok: true,
        };
        let decision = evaluate_ready_gate(&input);
        assert!(decision.ready);
        assert!(decision.reasons.is_empty());
    }

    #[test]
    fn ready_gate_rejects_full_verify_when_quick_is_required() {
        let input = ReadyGateInput {
            verify_status: VerifyStatus::Passed {
                tier: orch_core::state::VerifyTier::Full,
            },
            review_evaluation: approved_review(),
            graphite_hygiene_ok: true,
        };
        let decision = evaluate_ready_gate(&input);
        assert!(!decision.ready);
        assert_eq!(
            decision.reasons,
            vec![ReadyFailureReason::VerifyQuickNotPassed]
        );
    }

    #[test]
    fn auto_submit_blocked_when_not_ready_or_disabled() {
        let task = mk_task(SubmitMode::Single);
        let not_ready = super::ReadyGateDecision {
            ready: false,
            reasons: vec![ReadyFailureReason::ReviewNotApproved],
        };

        let decision = decide_auto_submit(
            &task,
            SubmitPolicy {
                org_default: SubmitMode::Single,
                repo_override: None,
                auto_submit: true,
            },
            &not_ready,
        );
        assert_eq!(
            decision,
            AutoSubmitDecision {
                should_submit: false,
                mode: None,
                blocked_reason: Some(SubmitBlockReason::NotReady)
            }
        );

        let ready = super::ReadyGateDecision {
            ready: true,
            reasons: Vec::new(),
        };
        let decision = decide_auto_submit(
            &task,
            SubmitPolicy {
                org_default: SubmitMode::Single,
                repo_override: None,
                auto_submit: false,
            },
            &ready,
        );
        assert_eq!(
            decision.blocked_reason,
            Some(SubmitBlockReason::AutoSubmitDisabled)
        );
    }

    #[test]
    fn auto_submit_uses_repo_override_mode() {
        let task = mk_task(SubmitMode::Single);
        let ready = super::ReadyGateDecision {
            ready: true,
            reasons: Vec::new(),
        };
        let decision = decide_auto_submit(
            &task,
            SubmitPolicy {
                org_default: SubmitMode::Single,
                repo_override: Some(SubmitMode::Stack),
                auto_submit: true,
            },
            &ready,
        );
        assert!(decision.should_submit);
        assert_eq!(decision.mode, Some(SubmitMode::Stack));
    }

    #[test]
    fn auto_submit_uses_task_submit_mode_when_no_override() {
        let task = mk_task(SubmitMode::Stack);
        let ready = super::ReadyGateDecision {
            ready: true,
            reasons: Vec::new(),
        };
        let decision = decide_auto_submit(
            &task,
            SubmitPolicy {
                org_default: SubmitMode::Single,
                repo_override: None,
                auto_submit: true,
            },
            &ready,
        );
        assert!(decision.should_submit);
        assert_eq!(decision.mode, Some(SubmitMode::Stack));
        assert_eq!(decision.blocked_reason, None);
    }

    #[test]
    fn ready_gate_reports_waiting_for_review_capacity_reason() {
        let mut review = approved_review();
        review.requirement.capacity_state = ReviewCapacityState::WaitingForReviewCapacity;
        review.approved = false;

        let input = ReadyGateInput {
            verify_status: VerifyStatus::Passed {
                tier: orch_core::state::VerifyTier::Quick,
            },
            review_evaluation: review,
            graphite_hygiene_ok: true,
        };
        let decision = evaluate_ready_gate(&input);
        assert!(!decision.ready);
        assert_eq!(
            decision.reasons,
            vec![
                ReadyFailureReason::WaitingForReviewCapacity,
                ReadyFailureReason::ReviewNotApproved
            ]
        );
    }

    #[test]
    fn ready_gate_reports_needs_human_capacity_reason() {
        let mut review = approved_review();
        review.requirement.capacity_state = ReviewCapacityState::NeedsHuman;
        review.approved = false;
        review.needs_human = true;

        let input = ReadyGateInput {
            verify_status: VerifyStatus::Passed {
                tier: orch_core::state::VerifyTier::Quick,
            },
            review_evaluation: review,
            graphite_hygiene_ok: true,
        };
        let decision = evaluate_ready_gate(&input);
        assert!(!decision.ready);
        assert_eq!(
            decision.reasons,
            vec![
                ReadyFailureReason::NeedsHumanReviewerCapacity,
                ReadyFailureReason::ReviewNotApproved
            ]
        );
    }

    #[test]
    fn ready_gate_blocks_on_capacity_even_when_reviews_are_approved() {
        let mut review = approved_review();
        review.requirement.capacity_state = ReviewCapacityState::WaitingForReviewCapacity;
        review.approved = true;

        let input = ReadyGateInput {
            verify_status: VerifyStatus::Passed {
                tier: orch_core::state::VerifyTier::Quick,
            },
            review_evaluation: review,
            graphite_hygiene_ok: true,
        };
        let decision = evaluate_ready_gate(&input);
        assert!(!decision.ready);
        assert_eq!(
            decision.reasons,
            vec![ReadyFailureReason::WaitingForReviewCapacity]
        );
    }

    #[test]
    fn ready_gate_dedupes_reason_when_capacity_needs_human_and_review_not_approved() {
        let input = ReadyGateInput {
            verify_status: VerifyStatus::NotRun,
            review_evaluation: ReviewEvaluation {
                requirement: ReviewRequirement {
                    required_models: vec![],
                    approvals_required: 0,
                    unanimous_required: true,
                    capacity_state: ReviewCapacityState::NeedsHuman,
                },
                approvals_received: 0,
                blocking_verdicts: Vec::new(),
                approved: false,
                needs_human: true,
            },
            graphite_hygiene_ok: false,
        };
        let decision = evaluate_ready_gate(&input);
        assert_eq!(
            decision.reasons,
            vec![
                ReadyFailureReason::VerifyQuickNotPassed,
                ReadyFailureReason::NeedsHumanReviewerCapacity,
                ReadyFailureReason::ReviewNotApproved,
                ReadyFailureReason::GraphiteHygieneFailed
            ]
        );
    }

    #[test]
    fn resolve_submit_mode_uses_task_mode_when_no_repo_override() {
        let task = mk_task(SubmitMode::Stack);
        let mode = resolve_submit_mode(
            &task,
            SubmitPolicy {
                org_default: SubmitMode::Single,
                repo_override: None,
                auto_submit: true,
            },
        );
        assert_eq!(mode, SubmitMode::Stack);
    }

    #[test]
    fn resolve_submit_mode_prefers_repo_override() {
        let task = mk_task(SubmitMode::Single);
        let mode = resolve_submit_mode(
            &task,
            SubmitPolicy {
                org_default: SubmitMode::Single,
                repo_override: Some(SubmitMode::Stack),
                auto_submit: true,
            },
        );
        assert_eq!(mode, SubmitMode::Stack);
    }
}
