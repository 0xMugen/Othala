use std::collections::HashMap;

use orch_core::events::ReviewVerdict;
use orch_core::state::{ReviewCapacityState, ReviewPolicy};
use orch_core::types::{ModelKind, TaskApproval};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerAvailability {
    pub model: ModelKind,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewGateConfig {
    pub enabled_models: Vec<ModelKind>,
    pub policy: ReviewPolicy,
    pub min_approvals: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequirement {
    pub required_models: Vec<ModelKind>,
    pub approvals_required: usize,
    pub unanimous_required: bool,
    pub capacity_state: ReviewCapacityState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewEvaluation {
    pub requirement: ReviewRequirement,
    pub approvals_received: usize,
    pub blocking_verdicts: Vec<(ModelKind, ReviewVerdict)>,
    pub approved: bool,
    pub needs_human: bool,
}

pub fn compute_review_requirement(
    config: &ReviewGateConfig,
    availability: &[ReviewerAvailability],
) -> ReviewRequirement {
    let availability_map = availability
        .iter()
        .map(|entry| (entry.model, entry.available))
        .collect::<HashMap<_, _>>();

    let enabled = dedupe_models(&config.enabled_models);
    let available_enabled = enabled
        .iter()
        .copied()
        .filter(|model| model_is_available(*model, &availability_map))
        .collect::<Vec<_>>();

    match config.policy {
        ReviewPolicy::Adaptive => {
            let enabled_count = enabled.len();
            let available_count = available_enabled.len();

            if enabled_count >= 2 && available_count < 2 {
                return ReviewRequirement {
                    required_models: available_enabled,
                    approvals_required: 0,
                    unanimous_required: true,
                    capacity_state: ReviewCapacityState::NeedsHuman,
                };
            }

            if enabled_count == 1 && available_count == 0 {
                return ReviewRequirement {
                    required_models: Vec::new(),
                    approvals_required: 0,
                    unanimous_required: true,
                    capacity_state: ReviewCapacityState::NeedsHuman,
                };
            }

            let required_models = available_enabled;
            let approvals_required = required_models.len();

            ReviewRequirement {
                required_models,
                approvals_required,
                unanimous_required: true,
                capacity_state: ReviewCapacityState::Sufficient,
            }
        }
        ReviewPolicy::Strict => {
            let has_unavailable = enabled
                .iter()
                .any(|model| !model_is_available(*model, &availability_map));

            if has_unavailable {
                return ReviewRequirement {
                    required_models: enabled,
                    approvals_required: 0,
                    unanimous_required: true,
                    capacity_state: ReviewCapacityState::WaitingForReviewCapacity,
                };
            }

            let required_models = enabled;
            let approvals_required = required_models.len();
            ReviewRequirement {
                required_models,
                approvals_required,
                unanimous_required: true,
                capacity_state: ReviewCapacityState::Sufficient,
            }
        }
    }
}

pub fn evaluate_review_gate(
    requirement: &ReviewRequirement,
    approvals: &[TaskApproval],
) -> ReviewEvaluation {
    if requirement.capacity_state == ReviewCapacityState::NeedsHuman {
        return ReviewEvaluation {
            requirement: requirement.clone(),
            approvals_received: 0,
            blocking_verdicts: Vec::new(),
            approved: false,
            needs_human: true,
        };
    }

    if requirement.capacity_state == ReviewCapacityState::WaitingForReviewCapacity {
        return ReviewEvaluation {
            requirement: requirement.clone(),
            approvals_received: 0,
            blocking_verdicts: Vec::new(),
            approved: false,
            needs_human: false,
        };
    }

    let required_set = requirement
        .required_models
        .iter()
        .copied()
        .collect::<Vec<_>>();
    let mut latest_by_model = HashMap::<ModelKind, ReviewVerdict>::new();
    for approval in approvals {
        if required_set.contains(&approval.reviewer) {
            latest_by_model.insert(approval.reviewer, approval.verdict);
        }
    }

    let mut approvals_received = 0usize;
    let mut blocking_verdicts = Vec::<(ModelKind, ReviewVerdict)>::new();
    for model in &required_set {
        match latest_by_model.get(model).copied() {
            Some(ReviewVerdict::Approve) => approvals_received += 1,
            Some(verdict @ (ReviewVerdict::RequestChanges | ReviewVerdict::Block)) => {
                blocking_verdicts.push((*model, verdict));
            }
            None => {}
        }
    }

    let approved = blocking_verdicts.is_empty()
        && approvals_received >= requirement.approvals_required
        && (if requirement.unanimous_required {
            approvals_received == requirement.required_models.len()
        } else {
            true
        });

    ReviewEvaluation {
        requirement: requirement.clone(),
        approvals_received,
        blocking_verdicts,
        approved,
        needs_human: false,
    }
}

pub fn dedupe_models(models: &[ModelKind]) -> Vec<ModelKind> {
    let mut seen = HashMap::<ModelKind, ()>::new();
    let mut out = Vec::new();
    for model in models {
        if seen.insert(*model, ()).is_none() {
            out.push(*model);
        }
    }
    out
}

fn model_is_available(model: ModelKind, availability: &HashMap<ModelKind, bool>) -> bool {
    availability.get(&model).copied().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::events::ReviewVerdict;
    use orch_core::state::{ReviewCapacityState, ReviewPolicy};
    use orch_core::types::{ModelKind, TaskApproval, TaskId};

    use super::{
        compute_review_requirement, evaluate_review_gate, ReviewGateConfig, ReviewerAvailability,
    };

    #[test]
    fn adaptive_requires_two_available_or_needs_human() {
        let cfg = ReviewGateConfig {
            enabled_models: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
            policy: ReviewPolicy::Adaptive,
            min_approvals: 2,
        };
        let requirement = compute_review_requirement(
            &cfg,
            &[
                ReviewerAvailability {
                    model: ModelKind::Claude,
                    available: true,
                },
                ReviewerAvailability {
                    model: ModelKind::Codex,
                    available: false,
                },
                ReviewerAvailability {
                    model: ModelKind::Gemini,
                    available: false,
                },
            ],
        );

        assert_eq!(requirement.capacity_state, ReviewCapacityState::NeedsHuman);
        assert_eq!(requirement.required_models, vec![ModelKind::Claude]);
        assert_eq!(requirement.approvals_required, 0);
    }

    #[test]
    fn strict_waits_for_unavailable_models() {
        let cfg = ReviewGateConfig {
            enabled_models: vec![ModelKind::Claude, ModelKind::Codex],
            policy: ReviewPolicy::Strict,
            min_approvals: 2,
        };
        let requirement = compute_review_requirement(
            &cfg,
            &[ReviewerAvailability {
                model: ModelKind::Claude,
                available: true,
            }],
        );

        assert_eq!(
            requirement.capacity_state,
            ReviewCapacityState::WaitingForReviewCapacity
        );
        assert_eq!(
            requirement.required_models,
            vec![ModelKind::Claude, ModelKind::Codex]
        );
    }

    #[test]
    fn unanimous_approval_required_for_required_set() {
        let cfg = ReviewGateConfig {
            enabled_models: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
            policy: ReviewPolicy::Adaptive,
            min_approvals: 2,
        };
        let requirement = compute_review_requirement(
            &cfg,
            &[
                ReviewerAvailability {
                    model: ModelKind::Claude,
                    available: true,
                },
                ReviewerAvailability {
                    model: ModelKind::Codex,
                    available: true,
                },
                ReviewerAvailability {
                    model: ModelKind::Gemini,
                    available: false,
                },
            ],
        );
        assert_eq!(requirement.required_models.len(), 2);
        assert_eq!(requirement.approvals_required, 2);
        assert_eq!(requirement.capacity_state, ReviewCapacityState::Sufficient);

        let task_id = TaskId("T123".to_string());
        let approvals = vec![
            TaskApproval {
                task_id: task_id.clone(),
                reviewer: ModelKind::Claude,
                verdict: ReviewVerdict::Approve,
                issued_at: Utc::now(),
            },
            TaskApproval {
                task_id,
                reviewer: ModelKind::Codex,
                verdict: ReviewVerdict::Approve,
                issued_at: Utc::now(),
            },
        ];
        let eval = evaluate_review_gate(&requirement, &approvals);
        assert!(eval.approved);
        assert_eq!(eval.approvals_received, 2);
    }

    #[test]
    fn request_changes_blocks_gate() {
        let cfg = ReviewGateConfig {
            enabled_models: vec![ModelKind::Claude, ModelKind::Codex],
            policy: ReviewPolicy::Adaptive,
            min_approvals: 2,
        };
        let requirement = compute_review_requirement(
            &cfg,
            &[
                ReviewerAvailability {
                    model: ModelKind::Claude,
                    available: true,
                },
                ReviewerAvailability {
                    model: ModelKind::Codex,
                    available: true,
                },
            ],
        );

        let approvals = vec![
            TaskApproval {
                task_id: TaskId("T1".to_string()),
                reviewer: ModelKind::Claude,
                verdict: ReviewVerdict::Approve,
                issued_at: Utc::now(),
            },
            TaskApproval {
                task_id: TaskId("T1".to_string()),
                reviewer: ModelKind::Codex,
                verdict: ReviewVerdict::RequestChanges,
                issued_at: Utc::now(),
            },
        ];
        let eval = evaluate_review_gate(&requirement, &approvals);
        assert!(!eval.approved);
        assert_eq!(eval.blocking_verdicts.len(), 1);
    }
}
