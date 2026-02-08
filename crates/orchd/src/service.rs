use chrono::{DateTime, Utc};
use orch_core::events::{Event, EventKind};
use orch_core::state::{TaskState, VerifyStatus, VerifyTier};
use orch_core::types::{
    EventId, ModelKind, PullRequestRef, SubmitMode, Task, TaskApproval, TaskId,
};
use orch_notify::{notification_for_event, NotificationDispatcher};
use std::path::PathBuf;

use crate::dependency_graph::{
    build_effective_dependency_graph, parent_head_update_trigger,
    restack_descendants_for_parent_head_update, InferredDependency,
};
use crate::event_log::{EventLogError, JsonlEventLog};
use crate::lifecycle_gate::{
    decide_auto_submit, evaluate_ready_gate, AutoSubmitDecision, ReadyGateDecision, ReadyGateInput,
    SubmitPolicy,
};
use crate::persistence::{PersistenceError, SqliteStore};
use crate::review_gate::{
    compute_review_requirement, evaluate_review_gate, ReviewEvaluation, ReviewGateConfig,
    ReviewRequirement, ReviewerAvailability,
};
use crate::scheduler::{
    BlockedTask, ModelAvailability, QueuedTask, RunningTask, SchedulePlan, ScheduledAssignment,
    Scheduler, SchedulingInput,
};
use crate::state_machine::{task_state_tag, transition_task, StateMachineError};
use crate::types::TaskRunRecord;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    EventLog(#[from] EventLogError),
    #[error(transparent)]
    StateMachine(#[from] StateMachineError),
    #[error("task not found: {task_id}")]
    TaskNotFound { task_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromoteTaskEventIds {
    pub ready_state_changed: EventId,
    pub ready_reached: EventId,
    pub submit_state_changed: EventId,
    pub submit_started: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftPrEventIds {
    pub draft_pr_state_changed: EventId,
    pub draft_pr_created: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteQuickVerifyEventIds {
    pub verify_completed: EventId,
    pub success_state_changed: EventId,
    pub failure_state_changed: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteFullVerifyEventIds {
    pub verify_completed: EventId,
    pub success_state_changed: EventId,
    pub failure_state_changed: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteRestackEventIds {
    pub restack_completed: EventId,
    pub success_state_changed: EventId,
    pub conflict_event: EventId,
    pub conflict_state_changed: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromoteTaskOutcome {
    pub task: Task,
    pub ready_gate: ReadyGateDecision,
    pub auto_submit: AutoSubmitDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReviewComputation {
    pub requirement: ReviewRequirement,
    pub evaluation: ReviewEvaluation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteReviewEventIds {
    pub review_completed: EventId,
    pub needs_human_state_changed: EventId,
    pub needs_human_event: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteReviewOutcome {
    pub task: Task,
    pub computation: TaskReviewComputation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestReviewEventIds {
    pub review_requested: EventId,
    pub needs_human_state_changed: EventId,
    pub needs_human_event: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestReviewOutcome {
    pub task: Task,
    pub computation: TaskReviewComputation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRestackEventIds {
    pub restack_state_changed: EventId,
    pub restack_started: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartVerifyEventIds {
    pub verify_state_changed: EventId,
    pub verify_requested: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveRestackConflictEventIds {
    pub restack_state_changed: EventId,
    pub restack_resolved: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartSubmitEventIds {
    pub submit_state_changed: EventId,
    pub submit_started: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkNeedsHumanEventIds {
    pub needs_human_state_changed: EventId,
    pub needs_human_event: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PauseTaskEventIds {
    pub pause_state_changed: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTaskEventIds {
    pub resume_state_changed: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentHeadUpdateEventIds {
    pub parent_head_updated: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteSubmitEventIds {
    pub submit_completed: EventId,
    pub success_state_changed: EventId,
    pub failure_state_changed: EventId,
    pub failure_error_event: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulingTickOutcome {
    pub scheduled: Vec<ScheduledAssignment>,
    pub blocked: Vec<BlockedTask>,
}

pub struct OrchdService {
    pub store: SqliteStore,
    pub event_log: JsonlEventLog,
    pub scheduler: Scheduler,
    pub notifier: Option<NotificationDispatcher>,
}

impl OrchdService {
    pub fn new(store: SqliteStore, event_log: JsonlEventLog, scheduler: Scheduler) -> Self {
        Self::new_with_notifier(store, event_log, scheduler, None)
    }

    pub fn new_with_notifier(
        store: SqliteStore,
        event_log: JsonlEventLog,
        scheduler: Scheduler,
        notifier: Option<NotificationDispatcher>,
    ) -> Self {
        Self {
            store,
            event_log,
            scheduler,
            notifier,
        }
    }

    pub fn open(
        sqlite_path: impl Into<PathBuf>,
        event_log_root: impl Into<PathBuf>,
        scheduler: Scheduler,
    ) -> Result<Self, ServiceError> {
        let store = SqliteStore::open(sqlite_path.into())?;
        let event_log = JsonlEventLog::new(event_log_root.into());
        let svc = Self::new(store, event_log, scheduler);
        svc.bootstrap()?;
        Ok(svc)
    }

    pub fn bootstrap(&self) -> Result<(), ServiceError> {
        self.store.migrate()?;
        self.event_log.ensure_layout()?;
        Ok(())
    }

    pub fn create_task(&self, task: &Task, created_event: &Event) -> Result<(), ServiceError> {
        self.store.upsert_task(task)?;
        self.record_event(created_event)?;
        Ok(())
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, ServiceError> {
        Ok(self.store.list_tasks()?)
    }

    pub fn list_tasks_by_state(&self, state: TaskState) -> Result<Vec<Task>, ServiceError> {
        Ok(self.store.list_tasks_by_state(state)?)
    }

    pub fn task(&self, task_id: &TaskId) -> Result<Option<Task>, ServiceError> {
        Ok(self.store.load_task(task_id)?)
    }

    pub fn record_event(&self, event: &Event) -> Result<(), ServiceError> {
        self.store.append_event(event)?;
        self.event_log.append_both(event)?;
        self.dispatch_notification_for_event(event);
        Ok(())
    }

    pub fn task_events(&self, task_id: &TaskId) -> Result<Vec<Event>, ServiceError> {
        Ok(self.store.list_events_for_task(task_id)?)
    }

    pub fn global_events(&self) -> Result<Vec<Event>, ServiceError> {
        Ok(self.store.list_events_global()?)
    }

    pub fn record_approval(&self, approval: &TaskApproval) -> Result<(), ServiceError> {
        self.store.upsert_approval(approval)?;
        Ok(())
    }

    pub fn task_approvals(&self, task_id: &TaskId) -> Result<Vec<TaskApproval>, ServiceError> {
        Ok(self.store.list_approvals_for_task(task_id)?)
    }

    pub fn evaluate_task_reviews(
        &self,
        task_id: &TaskId,
        requirement: &ReviewRequirement,
    ) -> Result<ReviewEvaluation, ServiceError> {
        let approvals = self.store.list_approvals_for_task(task_id)?;
        Ok(evaluate_review_gate(requirement, &approvals))
    }

    pub fn compute_task_review_from_config(
        &self,
        task_id: &TaskId,
        config: &ReviewGateConfig,
        availability: &[ReviewerAvailability],
    ) -> Result<TaskReviewComputation, ServiceError> {
        let requirement = compute_review_requirement(config, availability);
        let evaluation = self.evaluate_task_reviews(task_id, &requirement)?;
        Ok(TaskReviewComputation {
            requirement,
            evaluation,
        })
    }

    pub fn recompute_task_review_status(
        &self,
        task_id: &TaskId,
        config: &ReviewGateConfig,
        availability: &[ReviewerAvailability],
        at: DateTime<Utc>,
    ) -> Result<(Task, TaskReviewComputation), ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        let computation = self.compute_task_review_from_config(task_id, config, availability)?;
        task.review_status.required_models = computation.requirement.required_models.clone();
        task.review_status.approvals_required = computation.requirement.approvals_required;
        task.review_status.approvals_received = computation.evaluation.approvals_received;
        task.review_status.unanimous = computation.requirement.unanimous_required;
        task.review_status.capacity_state = computation.requirement.capacity_state.clone();
        task.updated_at = at;

        self.store.upsert_task(&task)?;
        Ok((task, computation))
    }

    pub fn request_review(
        &self,
        task_id: &TaskId,
        config: &ReviewGateConfig,
        availability: &[ReviewerAvailability],
        event_ids: RequestReviewEventIds,
        at: DateTime<Utc>,
    ) -> Result<RequestReviewOutcome, ServiceError> {
        let (mut task, computation) =
            self.recompute_task_review_status(task_id, config, availability, at)?;

        self.record_event(&Event {
            id: event_ids.review_requested,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::ReviewRequested {
                required_models: computation.requirement.required_models.clone(),
            },
        })?;

        if computation.evaluation.needs_human && task.state == TaskState::Reviewing {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::NeedsHuman,
                event_ids.needs_human_state_changed,
                at,
            )?;
            self.record_event(&Event {
                id: event_ids.needs_human_event,
                task_id: Some(task.id.clone()),
                repo_id: Some(task.repo_id.clone()),
                at,
                kind: EventKind::NeedsHuman {
                    reason: "review capacity insufficient for required approvals".to_string(),
                },
            })?;
        }

        Ok(RequestReviewOutcome { task, computation })
    }

    pub fn complete_review(
        &self,
        task_id: &TaskId,
        reviewer: orch_core::types::ModelKind,
        output: orch_core::events::ReviewOutput,
        config: &ReviewGateConfig,
        availability: &[ReviewerAvailability],
        event_ids: CompleteReviewEventIds,
        at: DateTime<Utc>,
    ) -> Result<CompleteReviewOutcome, ServiceError> {
        let task = self
            .store
            .load_task(task_id)?
            .ok_or_else(|| ServiceError::TaskNotFound {
                task_id: task_id.0.clone(),
            })?;

        self.record_approval(&TaskApproval {
            task_id: task.id.clone(),
            reviewer,
            verdict: output.verdict,
            issued_at: at,
        })?;

        self.record_event(&Event {
            id: event_ids.review_completed,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::ReviewCompleted {
                reviewer,
                output: output.clone(),
            },
        })?;

        let (mut updated, computation) =
            self.recompute_task_review_status(task_id, config, availability, at)?;

        if computation.evaluation.needs_human && updated.state == TaskState::Reviewing {
            self.apply_transition_with_state_event(
                &mut updated,
                TaskState::NeedsHuman,
                event_ids.needs_human_state_changed,
                at,
            )?;
            self.record_event(&Event {
                id: event_ids.needs_human_event,
                task_id: Some(updated.id.clone()),
                repo_id: Some(updated.repo_id.clone()),
                at,
                kind: EventKind::NeedsHuman {
                    reason: "review capacity insufficient for required approvals".to_string(),
                },
            })?;
        }

        Ok(CompleteReviewOutcome {
            task: updated,
            computation,
        })
    }

    pub fn transition_task_state(
        &self,
        task_id: &TaskId,
        to: TaskState,
        event_id: EventId,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;
        let transition = transition_task(&mut task, to, at)?;
        self.store.upsert_task(&task)?;

        let event = Event {
            id: event_id,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::TaskStateChanged {
                from: task_state_tag(transition.from).to_string(),
                to: task_state_tag(transition.to).to_string(),
            },
        };
        self.record_event(&event)?;
        Ok(task)
    }

    pub fn mark_task_draft_pr_open(
        &self,
        task_id: &TaskId,
        pr_number: u64,
        pr_url: String,
        event_ids: DraftPrEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        task.pr = Some(PullRequestRef {
            number: pr_number,
            url: pr_url.clone(),
            draft: true,
        });

        if task.state != TaskState::DraftPrOpen {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::DraftPrOpen,
                event_ids.draft_pr_state_changed,
                at,
            )?;
        } else {
            task.updated_at = at;
            self.store.upsert_task(&task)?;
        }

        self.record_event(&Event {
            id: event_ids.draft_pr_created,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::DraftPrCreated {
                number: pr_number,
                url: pr_url,
            },
        })?;

        Ok(task)
    }

    pub fn complete_quick_verify(
        &self,
        task_id: &TaskId,
        success: bool,
        failure_summary: Option<String>,
        event_ids: CompleteQuickVerifyEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        task.verify_status = if success {
            VerifyStatus::Passed {
                tier: VerifyTier::Quick,
            }
        } else {
            VerifyStatus::Failed {
                tier: VerifyTier::Quick,
                summary: failure_summary.unwrap_or_else(|| "verify.quick failed".to_string()),
            }
        };
        task.updated_at = at;
        self.store.upsert_task(&task)?;

        self.record_event(&Event {
            id: event_ids.verify_completed,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::VerifyCompleted {
                tier: VerifyTier::Quick,
                success,
            },
        })?;

        if task.state == TaskState::VerifyingQuick {
            let (target_state, event_id) = if success {
                (TaskState::Reviewing, event_ids.success_state_changed)
            } else {
                (TaskState::Running, event_ids.failure_state_changed)
            };
            self.apply_transition_with_state_event(&mut task, target_state, event_id, at)?;
        }

        Ok(task)
    }

    pub fn complete_full_verify(
        &self,
        task_id: &TaskId,
        success: bool,
        failure_summary: Option<String>,
        success_state_if_verifying_full: TaskState,
        failure_state_if_verifying_full: TaskState,
        event_ids: CompleteFullVerifyEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        task.verify_status = if success {
            VerifyStatus::Passed {
                tier: VerifyTier::Full,
            }
        } else {
            VerifyStatus::Failed {
                tier: VerifyTier::Full,
                summary: failure_summary.unwrap_or_else(|| "verify.full failed".to_string()),
            }
        };
        task.updated_at = at;
        self.store.upsert_task(&task)?;

        self.record_event(&Event {
            id: event_ids.verify_completed,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::VerifyCompleted {
                tier: VerifyTier::Full,
                success,
            },
        })?;

        if task.state == TaskState::VerifyingFull {
            let (target_state, event_id) = if success {
                (
                    success_state_if_verifying_full,
                    event_ids.success_state_changed,
                )
            } else {
                (
                    failure_state_if_verifying_full,
                    event_ids.failure_state_changed,
                )
            };
            self.apply_transition_with_state_event(&mut task, target_state, event_id, at)?;
        }

        Ok(task)
    }

    pub fn complete_restack(
        &self,
        task_id: &TaskId,
        conflict: bool,
        event_ids: CompleteRestackEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        if conflict {
            self.record_event(&Event {
                id: event_ids.conflict_event,
                task_id: Some(task.id.clone()),
                repo_id: Some(task.repo_id.clone()),
                at,
                kind: EventKind::RestackConflict,
            })?;

            if task.state == TaskState::Restacking {
                self.apply_transition_with_state_event(
                    &mut task,
                    TaskState::RestackConflict,
                    event_ids.conflict_state_changed,
                    at,
                )?;
            }
            return Ok(task);
        }

        self.record_event(&Event {
            id: event_ids.restack_completed,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::RestackCompleted,
        })?;

        if task.state == TaskState::Restacking {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::VerifyingQuick,
                event_ids.success_state_changed,
                at,
            )?;
        }

        Ok(task)
    }

    pub fn start_restack(
        &self,
        task_id: &TaskId,
        event_ids: StartRestackEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        if task.state != TaskState::Restacking {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::Restacking,
                event_ids.restack_state_changed,
                at,
            )?;
        }

        self.record_event(&Event {
            id: event_ids.restack_started,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::RestackStarted,
        })?;

        Ok(task)
    }

    pub fn resolve_restack_conflict(
        &self,
        task_id: &TaskId,
        event_ids: ResolveRestackConflictEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        let current_state = task.state;
        if !matches!(
            current_state,
            TaskState::RestackConflict | TaskState::Restacking
        ) {
            return Err(ServiceError::StateMachine(
                StateMachineError::InvalidTransition {
                    from: current_state,
                    to: TaskState::Restacking,
                },
            ));
        }

        if current_state == TaskState::RestackConflict {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::Restacking,
                event_ids.restack_state_changed,
                at,
            )?;
        }

        self.record_event(&Event {
            id: event_ids.restack_resolved,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::RestackResolved,
        })?;

        Ok(task)
    }

    pub fn start_verify(
        &self,
        task_id: &TaskId,
        tier: VerifyTier,
        event_ids: StartVerifyEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        let target_state = match tier {
            VerifyTier::Quick => TaskState::VerifyingQuick,
            VerifyTier::Full => TaskState::VerifyingFull,
        };

        if task.state != target_state {
            self.apply_transition_with_state_event(
                &mut task,
                target_state,
                event_ids.verify_state_changed,
                at,
            )?;
        }

        task.verify_status = VerifyStatus::Running { tier };
        task.updated_at = at;
        self.store.upsert_task(&task)?;

        self.record_event(&Event {
            id: event_ids.verify_requested,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::VerifyRequested { tier },
        })?;

        Ok(task)
    }

    pub fn start_submit(
        &self,
        task_id: &TaskId,
        mode: SubmitMode,
        event_ids: StartSubmitEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        if task.state != TaskState::Submitting {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::Submitting,
                event_ids.submit_state_changed,
                at,
            )?;
        }

        self.record_event(&Event {
            id: event_ids.submit_started,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::SubmitStarted { mode },
        })?;

        Ok(task)
    }

    pub fn mark_needs_human(
        &self,
        task_id: &TaskId,
        reason: impl Into<String>,
        event_ids: MarkNeedsHumanEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        if task.state != TaskState::NeedsHuman {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::NeedsHuman,
                event_ids.needs_human_state_changed,
                at,
            )?;
        }

        let reason = reason.into();
        let normalized_reason = if reason.trim().is_empty() {
            "manual intervention required".to_string()
        } else {
            reason.trim().to_string()
        };

        self.record_event(&Event {
            id: event_ids.needs_human_event,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::NeedsHuman {
                reason: normalized_reason,
            },
        })?;

        Ok(task)
    }

    pub fn pause_task(
        &self,
        task_id: &TaskId,
        event_ids: PauseTaskEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        if task.state != TaskState::Paused {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::Paused,
                event_ids.pause_state_changed,
                at,
            )?;
        }

        Ok(task)
    }

    pub fn resume_task(
        &self,
        task_id: &TaskId,
        event_ids: ResumeTaskEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        if task.state != TaskState::Running {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::Running,
                event_ids.resume_state_changed,
                at,
            )?;
        }

        Ok(task)
    }

    pub fn complete_submit(
        &self,
        task_id: &TaskId,
        success: bool,
        failure_message: Option<String>,
        event_ids: CompleteSubmitEventIds,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        if success {
            self.record_event(&Event {
                id: event_ids.submit_completed,
                task_id: Some(task.id.clone()),
                repo_id: Some(task.repo_id.clone()),
                at,
                kind: EventKind::SubmitCompleted,
            })?;

            if task.state == TaskState::Submitting {
                self.apply_transition_with_state_event(
                    &mut task,
                    TaskState::AwaitingMerge,
                    event_ids.success_state_changed,
                    at,
                )?;
            }
            return Ok(task);
        }

        self.record_event(&Event {
            id: event_ids.failure_error_event,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::Error {
                code: "submit_failed".to_string(),
                message: failure_message.unwrap_or_else(|| "graphite submit failed".to_string()),
            },
        })?;

        if task.state == TaskState::Submitting {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::Failed,
                event_ids.failure_state_changed,
                at,
            )?;
        }

        Ok(task)
    }

    pub fn restack_targets_for_parent_update(
        &self,
        parent_task_id: &TaskId,
        inferred: &[InferredDependency],
    ) -> Result<Vec<TaskId>, ServiceError> {
        let tasks = self.store.list_tasks()?;
        let graph = build_effective_dependency_graph(&tasks, inferred);
        Ok(restack_descendants_for_parent_head_update(
            &graph,
            parent_task_id,
        ))
    }

    pub fn handle_parent_head_updated(
        &self,
        parent_task_id: &TaskId,
        inferred: &[InferredDependency],
        event_ids: ParentHeadUpdateEventIds,
        at: DateTime<Utc>,
    ) -> Result<Vec<TaskId>, ServiceError> {
        let parent_task =
            self.store
                .load_task(parent_task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: parent_task_id.0.clone(),
                })?;

        self.record_event(&Event {
            id: event_ids.parent_head_updated,
            task_id: Some(parent_task.id.clone()),
            repo_id: Some(parent_task.repo_id.clone()),
            at,
            kind: EventKind::ParentHeadUpdated {
                parent_task_id: parent_task_id.clone(),
            },
        })?;

        self.restack_targets_for_parent_update(parent_task_id, inferred)
    }

    pub fn restack_targets_for_event(
        &self,
        event: &EventKind,
        inferred: &[InferredDependency],
    ) -> Result<Vec<TaskId>, ServiceError> {
        let Some(parent_task_id) = parent_head_update_trigger(event) else {
            return Ok(Vec::new());
        };
        self.restack_targets_for_parent_update(&parent_task_id, inferred)
    }

    pub fn promote_task_after_review(
        &self,
        task_id: &TaskId,
        ready_input: ReadyGateInput,
        submit_policy: SubmitPolicy,
        event_ids: PromoteTaskEventIds,
        at: DateTime<Utc>,
    ) -> Result<PromoteTaskOutcome, ServiceError> {
        let mut task =
            self.store
                .load_task(task_id)?
                .ok_or_else(|| ServiceError::TaskNotFound {
                    task_id: task_id.0.clone(),
                })?;

        let ready_gate = evaluate_ready_gate(&ready_input);
        let auto_submit = decide_auto_submit(&task, submit_policy, &ready_gate);

        if !ready_gate.ready {
            return Ok(PromoteTaskOutcome {
                task,
                ready_gate,
                auto_submit,
            });
        }

        // Ready gate promotes draft PRs to ready-for-review metadata.
        if let Some(pr) = task.pr.as_mut() {
            if pr.draft {
                pr.draft = false;
                task.updated_at = at;
                self.store.upsert_task(&task)?;
            }
        }

        if task.state != TaskState::Ready {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::Ready,
                event_ids.ready_state_changed,
                at,
            )?;
        }

        self.record_event(&Event {
            id: event_ids.ready_reached,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::ReadyReached,
        })?;

        if auto_submit.should_submit {
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::Submitting,
                event_ids.submit_state_changed,
                at,
            )?;

            self.record_event(&Event {
                id: event_ids.submit_started,
                task_id: Some(task.id.clone()),
                repo_id: Some(task.repo_id.clone()),
                at,
                kind: EventKind::SubmitStarted {
                    mode: auto_submit
                        .mode
                        .expect("mode must exist when should_submit"),
                },
            })?;
        }

        Ok(PromoteTaskOutcome {
            task,
            ready_gate,
            auto_submit,
        })
    }

    pub fn schedule(&self, input: SchedulingInput) -> SchedulePlan {
        self.scheduler.plan(input)
    }

    pub fn schedule_queued_tasks(
        &self,
        enabled_models: &[ModelKind],
        availability: &[ModelAvailability],
        at: DateTime<Utc>,
    ) -> Result<SchedulingTickOutcome, ServiceError> {
        let queued_tasks = self.store.list_tasks_by_state(TaskState::Queued)?;
        if queued_tasks.is_empty() {
            return Ok(SchedulingTickOutcome {
                scheduled: Vec::new(),
                blocked: Vec::new(),
            });
        }

        let running = self
            .store
            .list_open_runs()?
            .into_iter()
            .map(|run| RunningTask {
                task_id: run.task_id,
                repo_id: run.repo_id,
                model: run.model,
            })
            .collect::<Vec<_>>();

        let queued = queued_tasks
            .iter()
            .map(|task| QueuedTask {
                task_id: task.id.clone(),
                repo_id: task.repo_id.clone(),
                preferred_model: task.preferred_model,
                eligible_models: Vec::new(),
                priority: 0,
                enqueued_at: task.created_at,
            })
            .collect::<Vec<_>>();

        let plan = self.scheduler.plan(SchedulingInput {
            queued,
            running,
            enabled_models: enabled_models.to_vec(),
            availability: availability.to_vec(),
        });

        let tick_nonce = at.timestamp_nanos_opt().unwrap_or_default();
        for assignment in &plan.assignments {
            let mut task = self.store.load_task(&assignment.task_id)?.ok_or_else(|| {
                ServiceError::TaskNotFound {
                    task_id: assignment.task_id.0.clone(),
                }
            })?;
            if task.state != TaskState::Queued {
                continue;
            }

            let task_id_value = task.id.0.clone();
            self.apply_transition_with_state_event(
                &mut task,
                TaskState::Initializing,
                EventId(format!("E-SCHED-INIT-{task_id_value}-{tick_nonce}")),
                at,
            )?;

            let run = TaskRunRecord {
                run_id: format!(
                    "RUN-{}-{}-{tick_nonce}",
                    task_id_value,
                    model_kind_slug(assignment.model)
                ),
                task_id: task.id.clone(),
                repo_id: task.repo_id.clone(),
                model: assignment.model,
                started_at: at,
                finished_at: None,
                stop_reason: None,
                exit_code: None,
            };
            self.store.insert_run(&run)?;
        }

        Ok(SchedulingTickOutcome {
            scheduled: plan.assignments,
            blocked: plan.blocked,
        })
    }

    fn apply_transition_with_state_event(
        &self,
        task: &mut Task,
        to: TaskState,
        event_id: EventId,
        at: DateTime<Utc>,
    ) -> Result<(), ServiceError> {
        let transition = transition_task(task, to, at)?;
        self.store.upsert_task(task)?;
        self.record_event(&Event {
            id: event_id,
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::TaskStateChanged {
                from: task_state_tag(transition.from).to_string(),
                to: task_state_tag(transition.to).to_string(),
            },
        })?;
        Ok(())
    }

    fn dispatch_notification_for_event(&self, event: &Event) {
        let Some(dispatcher) = &self.notifier else {
            return;
        };
        let Some(message) = notification_for_event(event) else {
            return;
        };
        let _ = dispatcher.dispatch(&message);
    }
}

fn model_kind_slug(model: ModelKind) -> &'static str {
    match model {
        ModelKind::Claude => "claude",
        ModelKind::Codex => "codex",
        ModelKind::Gemini => "gemini",
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use orch_core::events::{
        EventKind, GraphiteHygieneReport, ReviewOutput, ReviewVerdict, TestAssessment,
    };
    use orch_core::state::{
        ReviewCapacityState, ReviewPolicy, ReviewStatus, TaskState, VerifyStatus, VerifyTier,
    };
    use orch_core::types::{
        EventId, ModelKind, PullRequestRef, RepoId, SubmitMode, Task, TaskApproval, TaskId,
        TaskRole, TaskType,
    };
    use orch_notify::{
        NotificationDispatcher, NotificationMessage, NotificationSink, NotificationSinkKind,
        NotificationTopic, NotifyError,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use crate::dependency_graph::InferredDependency;
    use crate::event_log::JsonlEventLog;
    use crate::lifecycle_gate::{ReadyGateInput, SubmitPolicy};
    use crate::review_gate::{
        ReviewEvaluation, ReviewGateConfig, ReviewRequirement, ReviewerAvailability,
    };
    use crate::scheduler::{
        BlockReason, ModelAvailability, QueuedTask, RunningTask, Scheduler, SchedulerConfig,
        SchedulingInput,
    };

    use super::{
        CompleteFullVerifyEventIds, CompleteQuickVerifyEventIds, CompleteRestackEventIds,
        CompleteReviewEventIds, CompleteSubmitEventIds, DraftPrEventIds, MarkNeedsHumanEventIds,
        OrchdService, ParentHeadUpdateEventIds, PauseTaskEventIds, PromoteTaskEventIds,
        RequestReviewEventIds, ResolveRestackConflictEventIds, ResumeTaskEventIds,
        StartRestackEventIds, StartSubmitEventIds, StartVerifyEventIds,
    };

    fn mk_service() -> OrchdService {
        mk_service_with_notifier(None)
    }

    fn mk_service_with_notifier(notifier: Option<NotificationDispatcher>) -> OrchdService {
        let store = crate::persistence::SqliteStore::open_in_memory().expect("in-memory db");
        let dir = std::env::temp_dir().join(format!(
            "othala-orchd-test-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");

        let svc = OrchdService::new_with_notifier(
            store,
            JsonlEventLog::new(dir),
            Scheduler::new(SchedulerConfig {
                per_repo_limit: 10,
                per_model_limit: vec![
                    (ModelKind::Claude, 10),
                    (ModelKind::Codex, 10),
                    (ModelKind::Gemini, 10),
                ]
                .into_iter()
                .collect(),
            }),
            notifier,
        );
        svc.bootstrap().expect("bootstrap");
        svc
    }

    fn mk_task(id: &str, state: TaskState, depends_on: &[&str]) -> Task {
        Task {
            id: TaskId(id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Task {id}"),
            state,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: depends_on
                .iter()
                .map(|x| TaskId((*x).to_string()))
                .collect(),
            submit_mode: SubmitMode::Single,
            branch_name: Some(format!("task/{id}")),
            worktree_path: PathBuf::from(format!(".orch/wt/{id}")),
            pr: None,
            verify_status: VerifyStatus::NotRun,
            review_status: ReviewStatus {
                required_models: vec![ModelKind::Claude],
                approvals_received: 0,
                approvals_required: 1,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn mk_created_event(task: &Task) -> orch_core::events::Event {
        orch_core::events::Event {
            id: EventId(format!("E-CREATE-{}", task.id.0)),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at: Utc::now(),
            kind: EventKind::TaskCreated,
        }
    }

    fn approved_review_eval() -> ReviewEvaluation {
        ReviewEvaluation {
            requirement: ReviewRequirement {
                required_models: vec![ModelKind::Claude],
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

    fn single_claude_requirement() -> ReviewRequirement {
        ReviewRequirement {
            required_models: vec![ModelKind::Claude],
            approvals_required: 1,
            unanimous_required: true,
            capacity_state: ReviewCapacityState::Sufficient,
        }
    }

    fn mk_review_output(verdict: ReviewVerdict) -> ReviewOutput {
        ReviewOutput {
            verdict,
            issues: Vec::new(),
            risk_flags: Vec::new(),
            graphite_hygiene: GraphiteHygieneReport {
                ok: true,
                notes: "ok".to_string(),
            },
            test_assessment: TestAssessment {
                ok: true,
                notes: "ok".to_string(),
            },
        }
    }

    struct CaptureSink {
        captured: Arc<Mutex<Vec<NotificationMessage>>>,
    }

    impl NotificationSink for CaptureSink {
        fn kind(&self) -> NotificationSinkKind {
            NotificationSinkKind::Stdout
        }

        fn send(&self, message: &NotificationMessage) -> Result<(), NotifyError> {
            self.captured
                .lock()
                .expect("capture lock")
                .push(message.clone());
            Ok(())
        }
    }

    #[test]
    fn record_event_dispatches_notifications_for_mapped_events_only() {
        let captured = Arc::new(Mutex::new(Vec::<NotificationMessage>::new()));
        let dispatcher = NotificationDispatcher::new(vec![Box::new(CaptureSink {
            captured: captured.clone(),
        })]);
        let svc = mk_service_with_notifier(Some(dispatcher));

        let task = mk_task("TN", TaskState::Reviewing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");
        assert!(
            captured.lock().expect("capture lock").is_empty(),
            "TaskCreated should not emit notification"
        );

        let verify_failed_event = orch_core::events::Event {
            id: EventId("E-VERIFY-FAILED".to_string()),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at: Utc::now(),
            kind: EventKind::VerifyCompleted {
                tier: orch_core::state::VerifyTier::Quick,
                success: false,
            },
        };
        svc.record_event(&verify_failed_event)
            .expect("record verify failed");

        let messages = captured.lock().expect("capture lock");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].topic, NotificationTopic::VerifyFailed);
    }

    #[test]
    fn schedule_assigns_queued_task_to_available_eligible_model() {
        let svc = mk_service();
        let plan = svc.schedule(SchedulingInput {
            queued: vec![QueuedTask {
                task_id: TaskId("TSCH1".to_string()),
                repo_id: RepoId("repo-a".to_string()),
                preferred_model: Some(ModelKind::Codex),
                eligible_models: vec![ModelKind::Codex],
                priority: 10,
                enqueued_at: Utc::now(),
            }],
            running: Vec::new(),
            enabled_models: vec![ModelKind::Codex],
            availability: vec![ModelAvailability {
                model: ModelKind::Codex,
                available: true,
            }],
        });

        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.assignments[0].task_id, TaskId("TSCH1".to_string()));
        assert_eq!(plan.assignments[0].repo_id, RepoId("repo-a".to_string()));
        assert_eq!(plan.assignments[0].model, ModelKind::Codex);
        assert!(plan.blocked.is_empty());
    }

    #[test]
    fn schedule_blocks_when_repo_limit_already_full() {
        let svc = mk_service();
        let running = (0..10)
            .map(|i| RunningTask {
                task_id: TaskId(format!("TRUN{i}")),
                repo_id: RepoId("repo-full".to_string()),
                model: ModelKind::Codex,
            })
            .collect::<Vec<_>>();

        let plan = svc.schedule(SchedulingInput {
            queued: vec![QueuedTask {
                task_id: TaskId("TSCH2".to_string()),
                repo_id: RepoId("repo-full".to_string()),
                preferred_model: Some(ModelKind::Codex),
                eligible_models: vec![ModelKind::Codex],
                priority: 1,
                enqueued_at: Utc::now(),
            }],
            running,
            enabled_models: vec![ModelKind::Codex],
            availability: vec![ModelAvailability {
                model: ModelKind::Codex,
                available: true,
            }],
        });

        assert!(plan.assignments.is_empty());
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].task_id, TaskId("TSCH2".to_string()));
        assert_eq!(plan.blocked[0].reason, BlockReason::RepoLimitReached);
    }

    #[test]
    fn schedule_queued_tasks_transitions_to_initializing_and_records_run() {
        let svc = mk_service();
        let task = mk_task("TSCHED", TaskState::Queued, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create queued task");

        let now = Utc::now();
        let outcome = svc
            .schedule_queued_tasks(
                &[ModelKind::Codex],
                &[ModelAvailability {
                    model: ModelKind::Codex,
                    available: true,
                }],
                now,
            )
            .expect("schedule queued tasks");

        assert_eq!(outcome.scheduled.len(), 1);
        assert!(outcome.blocked.is_empty());
        assert_eq!(outcome.scheduled[0].task_id, task.id);
        assert_eq!(outcome.scheduled[0].model, ModelKind::Codex);

        let updated = svc
            .task(&TaskId("TSCHED".to_string()))
            .expect("load updated task")
            .expect("task exists");
        assert_eq!(updated.state, TaskState::Initializing);

        let runs = svc.store.list_open_runs().expect("list open runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].task_id, TaskId("TSCHED".to_string()));
        assert_eq!(runs[0].model, ModelKind::Codex);
        assert_eq!(runs[0].finished_at, None);
    }

    #[test]
    fn schedule_queued_tasks_reports_blocked_without_state_change() {
        let svc = mk_service();
        let mut task = mk_task("TBLOCK", TaskState::Queued, &[]);
        task.preferred_model = Some(ModelKind::Gemini);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create queued task");

        let outcome = svc
            .schedule_queued_tasks(
                &[ModelKind::Gemini],
                &[ModelAvailability {
                    model: ModelKind::Gemini,
                    available: false,
                }],
                Utc::now(),
            )
            .expect("schedule queued tasks");

        assert!(outcome.scheduled.is_empty());
        assert_eq!(outcome.blocked.len(), 1);
        assert_eq!(outcome.blocked[0].task_id, TaskId("TBLOCK".to_string()));
        assert_eq!(outcome.blocked[0].reason, BlockReason::NoAvailableModel);

        let updated = svc
            .task(&TaskId("TBLOCK".to_string()))
            .expect("load updated task")
            .expect("task exists");
        assert_eq!(updated.state, TaskState::Queued);
        assert!(svc
            .store
            .list_open_runs()
            .expect("list open runs")
            .is_empty());
    }

    #[test]
    fn task_returns_none_when_task_is_missing() {
        let svc = mk_service();
        let task = svc
            .task(&TaskId("MISSING-TASK".to_string()))
            .expect("query task");
        assert_eq!(task, None);
    }

    #[test]
    fn list_tasks_and_list_tasks_by_state_use_store_ordering_and_filtering() {
        let svc = mk_service();
        let base = Utc::now();

        let mut t1 = mk_task("TLIST1", TaskState::Running, &[]);
        t1.created_at = base;
        t1.updated_at = base + Duration::seconds(1);
        let mut t2 = mk_task("TLIST2", TaskState::Reviewing, &[]);
        t2.created_at = base;
        t2.updated_at = base + Duration::seconds(2);
        let mut t3 = mk_task("TLIST3", TaskState::Running, &[]);
        t3.created_at = base;
        t3.updated_at = base + Duration::seconds(3);

        svc.create_task(&t1, &mk_created_event(&t1))
            .expect("create t1");
        svc.create_task(&t2, &mk_created_event(&t2))
            .expect("create t2");
        svc.create_task(&t3, &mk_created_event(&t3))
            .expect("create t3");

        let listed = svc.list_tasks().expect("list tasks");
        let listed_ids = listed.into_iter().map(|task| task.id.0).collect::<Vec<_>>();
        assert_eq!(
            listed_ids,
            vec![
                "TLIST3".to_string(),
                "TLIST2".to_string(),
                "TLIST1".to_string()
            ]
        );

        let running = svc
            .list_tasks_by_state(TaskState::Running)
            .expect("list running tasks");
        let running_ids = running
            .into_iter()
            .map(|task| task.id.0)
            .collect::<Vec<_>>();
        assert_eq!(
            running_ids,
            vec!["TLIST3".to_string(), "TLIST1".to_string()]
        );
    }

    #[test]
    fn global_events_returns_events_in_store_order() {
        let svc = mk_service();
        let base = Utc::now();

        let mut t1 = mk_task("TEVENT1", TaskState::Running, &[]);
        t1.created_at = base;
        t1.updated_at = base;
        svc.create_task(&t1, &mk_created_event(&t1))
            .expect("create task");

        let event_2 = orch_core::events::Event {
            id: EventId("E-GLOBAL-2".to_string()),
            task_id: Some(t1.id.clone()),
            repo_id: Some(t1.repo_id.clone()),
            at: base + Duration::seconds(5),
            kind: EventKind::RestackStarted,
        };
        let event_1 = orch_core::events::Event {
            id: EventId("E-GLOBAL-1".to_string()),
            task_id: Some(t1.id.clone()),
            repo_id: Some(t1.repo_id.clone()),
            at: base + Duration::seconds(5),
            kind: EventKind::VerifyRequested {
                tier: orch_core::state::VerifyTier::Quick,
            },
        };
        svc.record_event(&event_2).expect("record event 2");
        svc.record_event(&event_1).expect("record event 1");

        let events = svc.global_events().expect("list global events");
        let ids = events
            .into_iter()
            .map(|event| event.id.0)
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                format!("E-CREATE-{}", t1.id.0),
                "E-GLOBAL-1".to_string(),
                "E-GLOBAL-2".to_string()
            ]
        );
    }

    #[test]
    fn transition_task_state_updates_task_and_records_event() {
        let svc = mk_service();
        let task = mk_task("TTRANS1", TaskState::Running, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let at = Utc::now();
        let updated = svc
            .transition_task_state(
                &task.id,
                TaskState::Paused,
                EventId("E-TRANS1".to_string()),
                at,
            )
            .expect("transition task");

        assert_eq!(updated.state, TaskState::Paused);

        let stored = svc.task(&task.id).expect("load task").expect("task exists");
        assert_eq!(stored.state, TaskState::Paused);

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::TaskStateChanged { from, to } if from == "RUNNING" && to == "PAUSED"
            )
        }));
    }

    #[test]
    fn transition_task_state_rejects_invalid_transition() {
        let svc = mk_service();
        let task = mk_task("TTRANS2", TaskState::Queued, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let err = svc
            .transition_task_state(
                &task.id,
                TaskState::Ready,
                EventId("E-TRANS2".to_string()),
                Utc::now(),
            )
            .expect_err("queued -> ready should fail");
        assert!(matches!(err, crate::service::ServiceError::StateMachine(_)));

        let stored = svc.task(&task.id).expect("load task").expect("task exists");
        assert_eq!(stored.state, TaskState::Queued);
    }

    #[test]
    fn transition_task_state_requires_existing_task() {
        let svc = mk_service();
        let err = svc
            .transition_task_state(
                &TaskId("MISSING-TRANS".to_string()),
                TaskState::Paused,
                EventId("E-TRANS3".to_string()),
                Utc::now(),
            )
            .expect_err("missing task should fail");

        assert!(matches!(
            err,
            crate::service::ServiceError::TaskNotFound { task_id } if task_id == "MISSING-TRANS"
        ));
    }

    #[test]
    fn mark_task_draft_pr_open_sets_pr_and_transitions_state() {
        let svc = mk_service();
        let task = mk_task("TPR", TaskState::Initializing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .mark_task_draft_pr_open(
                &task.id,
                42,
                "https://github.com/example/repo/pull/42".to_string(),
                DraftPrEventIds {
                    draft_pr_state_changed: EventId("E-DRAFT-STATE".to_string()),
                    draft_pr_created: EventId("E-DRAFT-CREATED".to_string()),
                },
                Utc::now(),
            )
            .expect("mark draft pr open");

        assert_eq!(updated.state, TaskState::DraftPrOpen);
        let pr = updated.pr.expect("pr must be set");
        assert_eq!(pr.number, 42);
        assert_eq!(pr.url, "https://github.com/example/repo/pull/42");
        assert!(pr.draft);

        let stored = svc.task(&task.id).expect("load task").expect("task exists");
        assert_eq!(stored.state, TaskState::DraftPrOpen);
        assert_eq!(stored.pr, Some(pr.clone()));

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "INITIALIZING" && to == "DRAFT_PR_OPEN"
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::DraftPrCreated { number, url }
                    if *number == 42 && url == "https://github.com/example/repo/pull/42"
            )
        }));
    }

    #[test]
    fn mark_task_draft_pr_open_is_idempotent_on_state_transition() {
        let svc = mk_service();
        let task = mk_task("TPR2", TaskState::DraftPrOpen, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let _updated = svc
            .mark_task_draft_pr_open(
                &task.id,
                77,
                "https://github.com/example/repo/pull/77".to_string(),
                DraftPrEventIds {
                    draft_pr_state_changed: EventId("E-DRAFT-STATE-2".to_string()),
                    draft_pr_created: EventId("E-DRAFT-CREATED-2".to_string()),
                },
                Utc::now(),
            )
            .expect("mark draft pr open");

        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
        assert!(events.iter().any(|event| {
            matches!(&event.kind, EventKind::DraftPrCreated { number, .. } if *number == 77)
        }));
    }

    #[test]
    fn complete_quick_verify_success_transitions_to_reviewing() {
        let svc = mk_service();
        let task = mk_task("TVQ1", TaskState::VerifyingQuick, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_quick_verify(
                &task.id,
                true,
                None,
                CompleteQuickVerifyEventIds {
                    verify_completed: EventId("E-VQ1-DONE".to_string()),
                    success_state_changed: EventId("E-VQ1-REVIEWING".to_string()),
                    failure_state_changed: EventId("E-VQ1-RUNNING".to_string()),
                },
                Utc::now(),
            )
            .expect("complete quick verify");

        assert_eq!(updated.state, TaskState::Reviewing);
        assert_eq!(
            updated.verify_status,
            VerifyStatus::Passed {
                tier: VerifyTier::Quick
            }
        );

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| matches!(
            e.kind,
            EventKind::VerifyCompleted {
                tier: VerifyTier::Quick,
                success: true
            }
        )));
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "VERIFYING_QUICK" && to == "REVIEWING"
            )
        }));
    }

    #[test]
    fn complete_quick_verify_failure_transitions_back_to_running() {
        let svc = mk_service();
        let task = mk_task("TVQ2", TaskState::VerifyingQuick, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_quick_verify(
                &task.id,
                false,
                Some("lint failed".to_string()),
                CompleteQuickVerifyEventIds {
                    verify_completed: EventId("E-VQ2-DONE".to_string()),
                    success_state_changed: EventId("E-VQ2-REVIEWING".to_string()),
                    failure_state_changed: EventId("E-VQ2-RUNNING".to_string()),
                },
                Utc::now(),
            )
            .expect("complete quick verify");

        assert_eq!(updated.state, TaskState::Running);
        assert_eq!(
            updated.verify_status,
            VerifyStatus::Failed {
                tier: VerifyTier::Quick,
                summary: "lint failed".to_string()
            }
        );

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| matches!(
            e.kind,
            EventKind::VerifyCompleted {
                tier: VerifyTier::Quick,
                success: false
            }
        )));
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "VERIFYING_QUICK" && to == "RUNNING"
            )
        }));
    }

    #[test]
    fn complete_quick_verify_updates_status_without_state_jump_when_not_verifying() {
        let svc = mk_service();
        let task = mk_task("TVQ3", TaskState::Running, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_quick_verify(
                &task.id,
                true,
                None,
                CompleteQuickVerifyEventIds {
                    verify_completed: EventId("E-VQ3-DONE".to_string()),
                    success_state_changed: EventId("E-VQ3-REVIEWING".to_string()),
                    failure_state_changed: EventId("E-VQ3-RUNNING".to_string()),
                },
                Utc::now(),
            )
            .expect("complete quick verify");

        assert_eq!(updated.state, TaskState::Running);
        assert_eq!(
            updated.verify_status,
            VerifyStatus::Passed {
                tier: VerifyTier::Quick
            }
        );

        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
        assert!(events.iter().any(|e| matches!(
            e.kind,
            EventKind::VerifyCompleted {
                tier: VerifyTier::Quick,
                success: true
            }
        )));
    }

    #[test]
    fn complete_full_verify_success_transitions_to_target_state() {
        let svc = mk_service();
        let task = mk_task("TVF1", TaskState::VerifyingFull, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_full_verify(
                &task.id,
                true,
                None,
                TaskState::AwaitingMerge,
                TaskState::Running,
                CompleteFullVerifyEventIds {
                    verify_completed: EventId("E-VF1-DONE".to_string()),
                    success_state_changed: EventId("E-VF1-AWAITING".to_string()),
                    failure_state_changed: EventId("E-VF1-RUNNING".to_string()),
                },
                Utc::now(),
            )
            .expect("complete full verify");

        assert_eq!(updated.state, TaskState::AwaitingMerge);
        assert_eq!(
            updated.verify_status,
            VerifyStatus::Passed {
                tier: VerifyTier::Full
            }
        );

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| matches!(
            e.kind,
            EventKind::VerifyCompleted {
                tier: VerifyTier::Full,
                success: true
            }
        )));
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "VERIFYING_FULL" && to == "AWAITING_MERGE"
            )
        }));
    }

    #[test]
    fn complete_full_verify_failure_transitions_to_target_state() {
        let svc = mk_service();
        let task = mk_task("TVF2", TaskState::VerifyingFull, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_full_verify(
                &task.id,
                false,
                Some("integration failed".to_string()),
                TaskState::AwaitingMerge,
                TaskState::Running,
                CompleteFullVerifyEventIds {
                    verify_completed: EventId("E-VF2-DONE".to_string()),
                    success_state_changed: EventId("E-VF2-AWAITING".to_string()),
                    failure_state_changed: EventId("E-VF2-RUNNING".to_string()),
                },
                Utc::now(),
            )
            .expect("complete full verify");

        assert_eq!(updated.state, TaskState::Running);
        assert_eq!(
            updated.verify_status,
            VerifyStatus::Failed {
                tier: VerifyTier::Full,
                summary: "integration failed".to_string()
            }
        );

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| matches!(
            e.kind,
            EventKind::VerifyCompleted {
                tier: VerifyTier::Full,
                success: false
            }
        )));
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "VERIFYING_FULL" && to == "RUNNING"
            )
        }));
    }

    #[test]
    fn complete_full_verify_updates_status_without_state_jump_when_not_verifying() {
        let svc = mk_service();
        let task = mk_task("TVF3", TaskState::Ready, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_full_verify(
                &task.id,
                true,
                None,
                TaskState::AwaitingMerge,
                TaskState::Running,
                CompleteFullVerifyEventIds {
                    verify_completed: EventId("E-VF3-DONE".to_string()),
                    success_state_changed: EventId("E-VF3-AWAITING".to_string()),
                    failure_state_changed: EventId("E-VF3-RUNNING".to_string()),
                },
                Utc::now(),
            )
            .expect("complete full verify");

        assert_eq!(updated.state, TaskState::Ready);
        assert_eq!(
            updated.verify_status,
            VerifyStatus::Passed {
                tier: VerifyTier::Full
            }
        );

        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
        assert!(events.iter().any(|e| matches!(
            e.kind,
            EventKind::VerifyCompleted {
                tier: VerifyTier::Full,
                success: true
            }
        )));
    }

    #[test]
    fn start_verify_quick_transitions_to_verifying_quick_and_sets_running_status() {
        let svc = mk_service();
        let task = mk_task("TVSTART1", TaskState::Running, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .start_verify(
                &task.id,
                VerifyTier::Quick,
                StartVerifyEventIds {
                    verify_state_changed: EventId("E-VSTART1-STATE".to_string()),
                    verify_requested: EventId("E-VSTART1-REQUESTED".to_string()),
                },
                Utc::now(),
            )
            .expect("start verify");

        assert_eq!(updated.state, TaskState::VerifyingQuick);
        assert_eq!(
            updated.verify_status,
            VerifyStatus::Running {
                tier: VerifyTier::Quick
            }
        );
        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "RUNNING" && to == "VERIFYING_QUICK"
            )
        }));
        assert!(events.iter().any(|e| {
            matches!(
                e.kind,
                EventKind::VerifyRequested {
                    tier: VerifyTier::Quick
                }
            )
        }));
    }

    #[test]
    fn start_verify_full_transitions_to_verifying_full_and_sets_running_status() {
        let svc = mk_service();
        let task = mk_task("TVSTART2", TaskState::Ready, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .start_verify(
                &task.id,
                VerifyTier::Full,
                StartVerifyEventIds {
                    verify_state_changed: EventId("E-VSTART2-STATE".to_string()),
                    verify_requested: EventId("E-VSTART2-REQUESTED".to_string()),
                },
                Utc::now(),
            )
            .expect("start verify");

        assert_eq!(updated.state, TaskState::VerifyingFull);
        assert_eq!(
            updated.verify_status,
            VerifyStatus::Running {
                tier: VerifyTier::Full
            }
        );
        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "READY" && to == "VERIFYING_FULL"
            )
        }));
        assert!(events.iter().any(|e| {
            matches!(
                e.kind,
                EventKind::VerifyRequested {
                    tier: VerifyTier::Full
                }
            )
        }));
    }

    #[test]
    fn start_verify_in_current_state_emits_request_without_state_jump() {
        let svc = mk_service();
        let task = mk_task("TVSTART3", TaskState::VerifyingQuick, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .start_verify(
                &task.id,
                VerifyTier::Quick,
                StartVerifyEventIds {
                    verify_state_changed: EventId("E-VSTART3-STATE".to_string()),
                    verify_requested: EventId("E-VSTART3-REQUESTED".to_string()),
                },
                Utc::now(),
            )
            .expect("start verify");

        assert_eq!(updated.state, TaskState::VerifyingQuick);
        assert_eq!(
            updated.verify_status,
            VerifyStatus::Running {
                tier: VerifyTier::Quick
            }
        );
        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
        assert!(events.iter().any(|e| {
            matches!(
                e.kind,
                EventKind::VerifyRequested {
                    tier: VerifyTier::Quick
                }
            )
        }));
    }

    #[test]
    fn start_verify_rejects_invalid_transition() {
        let svc = mk_service();
        let task = mk_task("TVSTART4", TaskState::Reviewing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let err = svc
            .start_verify(
                &task.id,
                VerifyTier::Quick,
                StartVerifyEventIds {
                    verify_state_changed: EventId("E-VSTART4-STATE".to_string()),
                    verify_requested: EventId("E-VSTART4-REQUESTED".to_string()),
                },
                Utc::now(),
            )
            .expect_err("quick verify from reviewing should be invalid");

        assert!(matches!(
            err,
            crate::service::ServiceError::StateMachine(
                crate::state_machine::StateMachineError::InvalidTransition {
                    from: TaskState::Reviewing,
                    to: TaskState::VerifyingQuick
                }
            )
        ));
    }

    #[test]
    fn start_submit_transitions_ready_to_submitting_and_emits_started() {
        let svc = mk_service();
        let task = mk_task("TSSTART1", TaskState::Ready, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .start_submit(
                &task.id,
                SubmitMode::Stack,
                StartSubmitEventIds {
                    submit_state_changed: EventId("E-SSTART1-STATE".to_string()),
                    submit_started: EventId("E-SSTART1-STARTED".to_string()),
                },
                Utc::now(),
            )
            .expect("start submit");

        assert_eq!(updated.state, TaskState::Submitting);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "READY" && to == "SUBMITTING"
            )
        }));
        assert!(events.iter().any(|e| {
            matches!(
                e.kind,
                EventKind::SubmitStarted {
                    mode: SubmitMode::Stack
                }
            )
        }));
    }

    #[test]
    fn start_submit_emits_started_without_state_jump_when_already_submitting() {
        let svc = mk_service();
        let task = mk_task("TSSTART2", TaskState::Submitting, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .start_submit(
                &task.id,
                SubmitMode::Single,
                StartSubmitEventIds {
                    submit_state_changed: EventId("E-SSTART2-STATE".to_string()),
                    submit_started: EventId("E-SSTART2-STARTED".to_string()),
                },
                Utc::now(),
            )
            .expect("start submit");

        assert_eq!(updated.state, TaskState::Submitting);
        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
        assert!(events.iter().any(|e| {
            matches!(
                e.kind,
                EventKind::SubmitStarted {
                    mode: SubmitMode::Single
                }
            )
        }));
    }

    #[test]
    fn start_submit_rejects_invalid_transition() {
        let svc = mk_service();
        let task = mk_task("TSSTART3", TaskState::Running, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let err = svc
            .start_submit(
                &task.id,
                SubmitMode::Single,
                StartSubmitEventIds {
                    submit_state_changed: EventId("E-SSTART3-STATE".to_string()),
                    submit_started: EventId("E-SSTART3-STARTED".to_string()),
                },
                Utc::now(),
            )
            .expect_err("submit from running should be invalid");

        assert!(matches!(
            err,
            crate::service::ServiceError::StateMachine(
                crate::state_machine::StateMachineError::InvalidTransition {
                    from: TaskState::Running,
                    to: TaskState::Submitting
                }
            )
        ));
    }

    #[test]
    fn mark_needs_human_transitions_and_emits_reason_event() {
        let svc = mk_service();
        let task = mk_task("TNH1", TaskState::Running, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .mark_needs_human(
                &task.id,
                " waiting for product decision ",
                MarkNeedsHumanEventIds {
                    needs_human_state_changed: EventId("E-NH1-STATE".to_string()),
                    needs_human_event: EventId("E-NH1-EVENT".to_string()),
                },
                Utc::now(),
            )
            .expect("mark needs human");

        assert_eq!(updated.state, TaskState::NeedsHuman);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "RUNNING" && to == "NEEDS_HUMAN"
            )
        }));
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::NeedsHuman { reason } if reason == "waiting for product decision"
            )
        }));
    }

    #[test]
    fn mark_needs_human_emits_event_without_state_jump_when_already_needs_human() {
        let svc = mk_service();
        let task = mk_task("TNH2", TaskState::NeedsHuman, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .mark_needs_human(
                &task.id,
                "",
                MarkNeedsHumanEventIds {
                    needs_human_state_changed: EventId("E-NH2-STATE".to_string()),
                    needs_human_event: EventId("E-NH2-EVENT".to_string()),
                },
                Utc::now(),
            )
            .expect("mark needs human");

        assert_eq!(updated.state, TaskState::NeedsHuman);
        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::NeedsHuman { reason } if reason == "manual intervention required"
            )
        }));
    }

    #[test]
    fn mark_needs_human_rejects_invalid_transition() {
        let svc = mk_service();
        let task = mk_task("TNH3", TaskState::Ready, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let err = svc
            .mark_needs_human(
                &task.id,
                "manual hold",
                MarkNeedsHumanEventIds {
                    needs_human_state_changed: EventId("E-NH3-STATE".to_string()),
                    needs_human_event: EventId("E-NH3-EVENT".to_string()),
                },
                Utc::now(),
            )
            .expect_err("ready->needs_human should be invalid");

        assert!(matches!(
            err,
            crate::service::ServiceError::StateMachine(
                crate::state_machine::StateMachineError::InvalidTransition {
                    from: TaskState::Ready,
                    to: TaskState::NeedsHuman
                }
            )
        ));
    }

    #[test]
    fn pause_task_transitions_active_task_to_paused() {
        let svc = mk_service();
        let task = mk_task("TPAUSE1", TaskState::Running, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .pause_task(
                &task.id,
                PauseTaskEventIds {
                    pause_state_changed: EventId("E-PAUSE1-STATE".to_string()),
                },
                Utc::now(),
            )
            .expect("pause task");

        assert_eq!(updated.state, TaskState::Paused);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "RUNNING" && to == "PAUSED"
            )
        }));
    }

    #[test]
    fn pause_task_is_idempotent_when_already_paused() {
        let svc = mk_service();
        let task = mk_task("TPAUSE2", TaskState::Paused, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .pause_task(
                &task.id,
                PauseTaskEventIds {
                    pause_state_changed: EventId("E-PAUSE2-STATE".to_string()),
                },
                Utc::now(),
            )
            .expect("pause task");

        assert_eq!(updated.state, TaskState::Paused);
        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
    }

    #[test]
    fn resume_task_transitions_paused_task_to_running() {
        let svc = mk_service();
        let task = mk_task("TRESUME1", TaskState::Paused, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .resume_task(
                &task.id,
                ResumeTaskEventIds {
                    resume_state_changed: EventId("E-RESUME1-STATE".to_string()),
                },
                Utc::now(),
            )
            .expect("resume task");

        assert_eq!(updated.state, TaskState::Running);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "PAUSED" && to == "RUNNING"
            )
        }));
    }

    #[test]
    fn resume_task_is_idempotent_when_already_running() {
        let svc = mk_service();
        let task = mk_task("TRESUME2", TaskState::Running, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .resume_task(
                &task.id,
                ResumeTaskEventIds {
                    resume_state_changed: EventId("E-RESUME2-STATE".to_string()),
                },
                Utc::now(),
            )
            .expect("resume task");

        assert_eq!(updated.state, TaskState::Running);
        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
    }

    #[test]
    fn resume_task_rejects_invalid_transition() {
        let svc = mk_service();
        let task = mk_task("TRESUME3", TaskState::Submitting, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let err = svc
            .resume_task(
                &task.id,
                ResumeTaskEventIds {
                    resume_state_changed: EventId("E-RESUME3-STATE".to_string()),
                },
                Utc::now(),
            )
            .expect_err("submitting -> running is invalid");

        assert!(matches!(
            err,
            crate::service::ServiceError::StateMachine(
                crate::state_machine::StateMachineError::InvalidTransition {
                    from: TaskState::Submitting,
                    to: TaskState::Running
                }
            )
        ));
    }

    #[test]
    fn start_restack_transitions_running_to_restacking_and_emits_started() {
        let svc = mk_service();
        let task = mk_task("TRSTART1", TaskState::Running, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .start_restack(
                &task.id,
                StartRestackEventIds {
                    restack_state_changed: EventId("E-RSTART1-STATE".to_string()),
                    restack_started: EventId("E-RSTART1-START".to_string()),
                },
                Utc::now(),
            )
            .expect("start restack");

        assert_eq!(updated.state, TaskState::Restacking);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::RestackStarted)));
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "RUNNING" && to == "RESTACKING"
            )
        }));
    }

    #[test]
    fn start_restack_transitions_from_restack_conflict() {
        let svc = mk_service();
        let task = mk_task("TRSTART2", TaskState::RestackConflict, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .start_restack(
                &task.id,
                StartRestackEventIds {
                    restack_state_changed: EventId("E-RSTART2-STATE".to_string()),
                    restack_started: EventId("E-RSTART2-START".to_string()),
                },
                Utc::now(),
            )
            .expect("start restack");

        assert_eq!(updated.state, TaskState::Restacking);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "RESTACK_CONFLICT" && to == "RESTACKING"
            )
        }));
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::RestackStarted)));
    }

    #[test]
    fn start_restack_emits_started_without_state_jump_when_already_restacking() {
        let svc = mk_service();
        let task = mk_task("TRSTART3", TaskState::Restacking, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .start_restack(
                &task.id,
                StartRestackEventIds {
                    restack_state_changed: EventId("E-RSTART3-STATE".to_string()),
                    restack_started: EventId("E-RSTART3-START".to_string()),
                },
                Utc::now(),
            )
            .expect("start restack");

        assert_eq!(updated.state, TaskState::Restacking);
        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::RestackStarted)));
    }

    #[test]
    fn resolve_restack_conflict_transitions_to_restacking_and_emits_resolved() {
        let svc = mk_service();
        let task = mk_task("TRRES1", TaskState::RestackConflict, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .resolve_restack_conflict(
                &task.id,
                ResolveRestackConflictEventIds {
                    restack_state_changed: EventId("E-RRES1-STATE".to_string()),
                    restack_resolved: EventId("E-RRES1-RESOLVED".to_string()),
                },
                Utc::now(),
            )
            .expect("resolve restack conflict");

        assert_eq!(updated.state, TaskState::Restacking);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "RESTACK_CONFLICT" && to == "RESTACKING"
            )
        }));
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::RestackResolved)));
    }

    #[test]
    fn resolve_restack_conflict_is_idempotent_when_already_restacking() {
        let svc = mk_service();
        let task = mk_task("TRRES2", TaskState::Restacking, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .resolve_restack_conflict(
                &task.id,
                ResolveRestackConflictEventIds {
                    restack_state_changed: EventId("E-RRES2-STATE".to_string()),
                    restack_resolved: EventId("E-RRES2-RESOLVED".to_string()),
                },
                Utc::now(),
            )
            .expect("resolve restack conflict");

        assert_eq!(updated.state, TaskState::Restacking);
        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::RestackResolved)));
    }

    #[test]
    fn resolve_restack_conflict_rejects_invalid_transition() {
        let svc = mk_service();
        let task = mk_task("TRRES3", TaskState::Running, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let err = svc
            .resolve_restack_conflict(
                &task.id,
                ResolveRestackConflictEventIds {
                    restack_state_changed: EventId("E-RRES3-STATE".to_string()),
                    restack_resolved: EventId("E-RRES3-RESOLVED".to_string()),
                },
                Utc::now(),
            )
            .expect_err("running cannot resolve a restack conflict");

        assert!(matches!(
            err,
            crate::service::ServiceError::StateMachine(
                crate::state_machine::StateMachineError::InvalidTransition {
                    from: TaskState::Running,
                    to: TaskState::Restacking
                }
            )
        ));
    }

    #[test]
    fn complete_restack_success_transitions_to_verifying_quick() {
        let svc = mk_service();
        let task = mk_task("TRS1", TaskState::Restacking, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_restack(
                &task.id,
                false,
                CompleteRestackEventIds {
                    restack_completed: EventId("E-RS1-DONE".to_string()),
                    success_state_changed: EventId("E-RS1-VQ".to_string()),
                    conflict_event: EventId("E-RS1-CONFLICT".to_string()),
                    conflict_state_changed: EventId("E-RS1-CONFLICT-STATE".to_string()),
                },
                Utc::now(),
            )
            .expect("complete restack");

        assert_eq!(updated.state, TaskState::VerifyingQuick);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::RestackCompleted)));
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "RESTACKING" && to == "VERIFYING_QUICK"
            )
        }));
    }

    #[test]
    fn complete_restack_conflict_transitions_to_restack_conflict() {
        let svc = mk_service();
        let task = mk_task("TRS2", TaskState::Restacking, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_restack(
                &task.id,
                true,
                CompleteRestackEventIds {
                    restack_completed: EventId("E-RS2-DONE".to_string()),
                    success_state_changed: EventId("E-RS2-VQ".to_string()),
                    conflict_event: EventId("E-RS2-CONFLICT".to_string()),
                    conflict_state_changed: EventId("E-RS2-CONFLICT-STATE".to_string()),
                },
                Utc::now(),
            )
            .expect("complete restack");

        assert_eq!(updated.state, TaskState::RestackConflict);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::RestackConflict)));
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "RESTACKING" && to == "RESTACK_CONFLICT"
            )
        }));
    }

    #[test]
    fn complete_restack_emits_events_without_state_jump_when_not_restacking() {
        let svc = mk_service();
        let task = mk_task("TRS3", TaskState::Running, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_restack(
                &task.id,
                false,
                CompleteRestackEventIds {
                    restack_completed: EventId("E-RS3-DONE".to_string()),
                    success_state_changed: EventId("E-RS3-VQ".to_string()),
                    conflict_event: EventId("E-RS3-CONFLICT".to_string()),
                    conflict_state_changed: EventId("E-RS3-CONFLICT-STATE".to_string()),
                },
                Utc::now(),
            )
            .expect("complete restack");

        assert_eq!(updated.state, TaskState::Running);
        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::RestackCompleted)));
    }

    #[test]
    fn complete_submit_success_transitions_to_awaiting_merge() {
        let svc = mk_service();
        let task = mk_task("TSUB1", TaskState::Submitting, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_submit(
                &task.id,
                true,
                None,
                CompleteSubmitEventIds {
                    submit_completed: EventId("E-SUB1-DONE".to_string()),
                    success_state_changed: EventId("E-SUB1-AWAITING".to_string()),
                    failure_state_changed: EventId("E-SUB1-FAILED".to_string()),
                    failure_error_event: EventId("E-SUB1-ERR".to_string()),
                },
                Utc::now(),
            )
            .expect("complete submit");

        assert_eq!(updated.state, TaskState::AwaitingMerge);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::SubmitCompleted)));
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "SUBMITTING" && to == "AWAITING_MERGE"
            )
        }));
    }

    #[test]
    fn complete_submit_failure_transitions_to_failed_and_records_error() {
        let svc = mk_service();
        let task = mk_task("TSUB2", TaskState::Submitting, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_submit(
                &task.id,
                false,
                Some("gt submit exited with status 1".to_string()),
                CompleteSubmitEventIds {
                    submit_completed: EventId("E-SUB2-DONE".to_string()),
                    success_state_changed: EventId("E-SUB2-AWAITING".to_string()),
                    failure_state_changed: EventId("E-SUB2-FAILED".to_string()),
                    failure_error_event: EventId("E-SUB2-ERR".to_string()),
                },
                Utc::now(),
            )
            .expect("complete submit");

        assert_eq!(updated.state, TaskState::Failed);
        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::Error { code, message }
                    if code == "submit_failed" && message.contains("status 1")
            )
        }));
        assert!(events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "SUBMITTING" && to == "FAILED"
            )
        }));
    }

    #[test]
    fn complete_submit_emits_event_without_state_jump_when_not_submitting() {
        let svc = mk_service();
        let task = mk_task("TSUB3", TaskState::Ready, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let updated = svc
            .complete_submit(
                &task.id,
                true,
                None,
                CompleteSubmitEventIds {
                    submit_completed: EventId("E-SUB3-DONE".to_string()),
                    success_state_changed: EventId("E-SUB3-AWAITING".to_string()),
                    failure_state_changed: EventId("E-SUB3-FAILED".to_string()),
                    failure_error_event: EventId("E-SUB3-ERR".to_string()),
                },
                Utc::now(),
            )
            .expect("complete submit");

        assert_eq!(updated.state, TaskState::Ready);
        let events = svc.task_events(&task.id).expect("events");
        let state_change_count = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TaskStateChanged { .. }))
            .count();
        assert_eq!(state_change_count, 0);
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::SubmitCompleted)));
    }

    #[test]
    fn restack_targets_for_parent_update_uses_union_graph() {
        let svc = mk_service();
        let t1 = mk_task("T1", TaskState::Running, &[]);
        let t2 = mk_task("T2", TaskState::Running, &["T1"]);
        let t3 = mk_task("T3", TaskState::Running, &[]);
        svc.create_task(&t1, &mk_created_event(&t1))
            .expect("create t1");
        svc.create_task(&t2, &mk_created_event(&t2))
            .expect("create t2");
        svc.create_task(&t3, &mk_created_event(&t3))
            .expect("create t3");

        let targets = svc
            .restack_targets_for_parent_update(
                &TaskId("T1".to_string()),
                &[InferredDependency {
                    parent_task_id: TaskId("T2".to_string()),
                    child_task_id: TaskId("T3".to_string()),
                }],
            )
            .expect("restack targets");

        let ids = targets.into_iter().map(|x| x.0).collect::<Vec<_>>();
        assert_eq!(ids, vec!["T2".to_string(), "T3".to_string()]);
    }

    #[test]
    fn handle_parent_head_updated_records_event_and_returns_descendant_targets() {
        let svc = mk_service();
        let t1 = mk_task("TPHU1", TaskState::Running, &[]);
        let t2 = mk_task("TPHU2", TaskState::Running, &["TPHU1"]);
        let t3 = mk_task("TPHU3", TaskState::Running, &[]);
        svc.create_task(&t1, &mk_created_event(&t1))
            .expect("create t1");
        svc.create_task(&t2, &mk_created_event(&t2))
            .expect("create t2");
        svc.create_task(&t3, &mk_created_event(&t3))
            .expect("create t3");

        let targets = svc
            .handle_parent_head_updated(
                &TaskId("TPHU1".to_string()),
                &[InferredDependency {
                    parent_task_id: TaskId("TPHU2".to_string()),
                    child_task_id: TaskId("TPHU3".to_string()),
                }],
                ParentHeadUpdateEventIds {
                    parent_head_updated: EventId("E-PHU1".to_string()),
                },
                Utc::now(),
            )
            .expect("handle parent head update");

        let ids = targets.into_iter().map(|x| x.0).collect::<Vec<_>>();
        assert_eq!(ids, vec!["TPHU2".to_string(), "TPHU3".to_string()]);

        let events = svc.task_events(&t1.id).expect("events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::ParentHeadUpdated { parent_task_id }
                    if parent_task_id.0 == "TPHU1"
            )
        }));
    }

    #[test]
    fn handle_parent_head_updated_returns_empty_for_leaf_parent() {
        let svc = mk_service();
        let t1 = mk_task("TPHU4", TaskState::Running, &[]);
        svc.create_task(&t1, &mk_created_event(&t1))
            .expect("create t1");

        let targets = svc
            .handle_parent_head_updated(
                &TaskId("TPHU4".to_string()),
                &[],
                ParentHeadUpdateEventIds {
                    parent_head_updated: EventId("E-PHU2".to_string()),
                },
                Utc::now(),
            )
            .expect("handle parent head update");
        assert!(targets.is_empty());
    }

    #[test]
    fn handle_parent_head_updated_requires_existing_parent_task() {
        let svc = mk_service();
        let err = svc
            .handle_parent_head_updated(
                &TaskId("MISSING".to_string()),
                &[],
                ParentHeadUpdateEventIds {
                    parent_head_updated: EventId("E-PHU3".to_string()),
                },
                Utc::now(),
            )
            .expect_err("missing parent task should error");

        assert!(matches!(
            err,
            crate::service::ServiceError::TaskNotFound { task_id } if task_id == "MISSING"
        ));
    }

    #[test]
    fn restack_targets_for_event_returns_empty_for_non_parent_update_event() {
        let svc = mk_service();
        let t1 = mk_task("TEVT1", TaskState::Running, &[]);
        let t2 = mk_task("TEVT2", TaskState::Running, &["TEVT1"]);
        svc.create_task(&t1, &mk_created_event(&t1))
            .expect("create t1");
        svc.create_task(&t2, &mk_created_event(&t2))
            .expect("create t2");

        let targets = svc
            .restack_targets_for_event(&EventKind::RestackStarted, &[])
            .expect("restack targets for non-parent event");
        assert!(targets.is_empty());
    }

    #[test]
    fn restack_targets_for_event_uses_parent_head_updated_task_id() {
        let svc = mk_service();
        let t1 = mk_task("TEVT3", TaskState::Running, &[]);
        let t2 = mk_task("TEVT4", TaskState::Running, &["TEVT3"]);
        let t3 = mk_task("TEVT5", TaskState::Running, &[]);
        svc.create_task(&t1, &mk_created_event(&t1))
            .expect("create t1");
        svc.create_task(&t2, &mk_created_event(&t2))
            .expect("create t2");
        svc.create_task(&t3, &mk_created_event(&t3))
            .expect("create t3");

        let targets = svc
            .restack_targets_for_event(
                &EventKind::ParentHeadUpdated {
                    parent_task_id: TaskId("TEVT3".to_string()),
                },
                &[InferredDependency {
                    parent_task_id: TaskId("TEVT4".to_string()),
                    child_task_id: TaskId("TEVT5".to_string()),
                }],
            )
            .expect("restack targets for parent update");

        let ids = targets.into_iter().map(|x| x.0).collect::<Vec<_>>();
        assert_eq!(ids, vec!["TEVT4".to_string(), "TEVT5".to_string()]);
    }

    #[test]
    fn promote_task_after_review_moves_to_submitting_when_ready_and_auto_submit_enabled() {
        let svc = mk_service();
        let mut task = mk_task("T9", TaskState::Reviewing, &[]);
        task.pr = Some(PullRequestRef {
            number: 9,
            url: "https://github.com/example/repo/pull/9".to_string(),
            draft: true,
        });
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let outcome = svc
            .promote_task_after_review(
                &task.id,
                ReadyGateInput {
                    verify_status: VerifyStatus::Passed {
                        tier: orch_core::state::VerifyTier::Quick,
                    },
                    review_evaluation: approved_review_eval(),
                    graphite_hygiene_ok: true,
                },
                SubmitPolicy {
                    org_default: SubmitMode::Single,
                    repo_override: None,
                    auto_submit: true,
                },
                PromoteTaskEventIds {
                    ready_state_changed: EventId("E-READY-STATE".to_string()),
                    ready_reached: EventId("E-READY".to_string()),
                    submit_state_changed: EventId("E-SUBMIT-STATE".to_string()),
                    submit_started: EventId("E-SUBMIT".to_string()),
                },
                Utc::now(),
            )
            .expect("promote");

        assert!(outcome.ready_gate.ready);
        assert!(outcome.auto_submit.should_submit);
        assert_eq!(outcome.task.state, TaskState::Submitting);
        assert_eq!(outcome.task.pr.as_ref().map(|pr| pr.draft), Some(false));

        let stored = svc.task(&task.id).expect("load task").expect("task exists");
        assert_eq!(stored.state, TaskState::Submitting);
        assert_eq!(stored.pr.as_ref().map(|pr| pr.draft), Some(false));

        let events = svc.task_events(&task.id).expect("events");
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ReadyReached)));
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::SubmitStarted { .. })));
    }

    #[test]
    fn promote_task_after_review_keeps_pr_draft_when_not_ready() {
        let svc = mk_service();
        let mut task = mk_task("T9NR", TaskState::Reviewing, &[]);
        task.pr = Some(PullRequestRef {
            number: 91,
            url: "https://github.com/example/repo/pull/91".to_string(),
            draft: true,
        });
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let outcome = svc
            .promote_task_after_review(
                &task.id,
                ReadyGateInput {
                    verify_status: VerifyStatus::NotRun,
                    review_evaluation: approved_review_eval(),
                    graphite_hygiene_ok: true,
                },
                SubmitPolicy {
                    org_default: SubmitMode::Single,
                    repo_override: None,
                    auto_submit: true,
                },
                PromoteTaskEventIds {
                    ready_state_changed: EventId("E-NR-READY-STATE".to_string()),
                    ready_reached: EventId("E-NR-READY".to_string()),
                    submit_state_changed: EventId("E-NR-SUBMIT-STATE".to_string()),
                    submit_started: EventId("E-NR-SUBMIT".to_string()),
                },
                Utc::now(),
            )
            .expect("promote");

        assert!(!outcome.ready_gate.ready);
        assert_eq!(outcome.task.pr.as_ref().map(|pr| pr.draft), Some(true));
        let stored = svc.task(&task.id).expect("load task").expect("task exists");
        assert_eq!(stored.pr.as_ref().map(|pr| pr.draft), Some(true));
    }

    #[test]
    fn task_approvals_roundtrip_and_eval_gate_from_store() {
        let svc = mk_service();
        let task = mk_task("TA", TaskState::Reviewing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let approval = TaskApproval {
            task_id: task.id.clone(),
            reviewer: ModelKind::Claude,
            verdict: ReviewVerdict::Approve,
            issued_at: Utc::now(),
        };
        svc.record_approval(&approval).expect("record approval");

        let stored = svc.task_approvals(&task.id).expect("load approvals");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].verdict, ReviewVerdict::Approve);

        let eval = svc
            .evaluate_task_reviews(&task.id, &single_claude_requirement())
            .expect("evaluate");
        assert!(eval.approved);
        assert_eq!(eval.approvals_received, 1);
    }

    #[test]
    fn upsert_approval_replaces_verdict_for_same_reviewer() {
        let svc = mk_service();
        let task = mk_task("TB", TaskState::Reviewing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        svc.record_approval(&TaskApproval {
            task_id: task.id.clone(),
            reviewer: ModelKind::Claude,
            verdict: ReviewVerdict::Approve,
            issued_at: Utc::now(),
        })
        .expect("record first");

        svc.record_approval(&TaskApproval {
            task_id: task.id.clone(),
            reviewer: ModelKind::Claude,
            verdict: ReviewVerdict::RequestChanges,
            issued_at: Utc::now(),
        })
        .expect("record second");

        let eval = svc
            .evaluate_task_reviews(&task.id, &single_claude_requirement())
            .expect("evaluate");
        assert!(!eval.approved);
        assert_eq!(eval.blocking_verdicts.len(), 1);
        assert_eq!(eval.blocking_verdicts[0].1, ReviewVerdict::RequestChanges);
    }

    #[test]
    fn recompute_task_review_status_updates_task_from_policy_and_approvals() {
        let svc = mk_service();
        let task = mk_task("TC", TaskState::Reviewing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        svc.record_approval(&TaskApproval {
            task_id: task.id.clone(),
            reviewer: ModelKind::Claude,
            verdict: ReviewVerdict::Approve,
            issued_at: Utc::now(),
        })
        .expect("record approval 1");
        svc.record_approval(&TaskApproval {
            task_id: task.id.clone(),
            reviewer: ModelKind::Codex,
            verdict: ReviewVerdict::RequestChanges,
            issued_at: Utc::now(),
        })
        .expect("record approval 2");

        let (updated, computation) = svc
            .recompute_task_review_status(
                &task.id,
                &ReviewGateConfig {
                    enabled_models: vec![ModelKind::Claude, ModelKind::Codex],
                    policy: ReviewPolicy::Adaptive,
                    min_approvals: 2,
                },
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
                Utc::now(),
            )
            .expect("recompute");

        assert_eq!(
            updated.review_status.required_models,
            vec![ModelKind::Claude, ModelKind::Codex]
        );
        assert_eq!(updated.review_status.approvals_required, 2);
        assert_eq!(updated.review_status.approvals_received, 1);
        assert!(updated.review_status.unanimous);
        assert_eq!(
            updated.review_status.capacity_state,
            ReviewCapacityState::Sufficient
        );
        assert!(!computation.evaluation.approved);
        assert_eq!(computation.evaluation.blocking_verdicts.len(), 1);
    }

    #[test]
    fn recompute_task_review_status_marks_needs_human_when_adaptive_capacity_too_low() {
        let svc = mk_service();
        let task = mk_task("TD", TaskState::Reviewing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let (updated, computation) = svc
            .recompute_task_review_status(
                &task.id,
                &ReviewGateConfig {
                    enabled_models: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
                    policy: ReviewPolicy::Adaptive,
                    min_approvals: 2,
                },
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
                Utc::now(),
            )
            .expect("recompute");

        assert_eq!(
            updated.review_status.capacity_state,
            ReviewCapacityState::NeedsHuman
        );
        assert_eq!(
            updated.review_status.required_models,
            vec![ModelKind::Claude]
        );
        assert_eq!(updated.review_status.approvals_required, 0);
        assert_eq!(updated.review_status.approvals_received, 0);
        assert!(computation.evaluation.needs_human);
        assert!(!computation.evaluation.approved);
    }

    #[test]
    fn request_review_emits_required_models_and_keeps_reviewing_when_capacity_is_sufficient() {
        let svc = mk_service();
        let task = mk_task("TREQ1", TaskState::Reviewing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let outcome = svc
            .request_review(
                &task.id,
                &ReviewGateConfig {
                    enabled_models: vec![ModelKind::Claude, ModelKind::Codex],
                    policy: ReviewPolicy::Adaptive,
                    min_approvals: 2,
                },
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
                RequestReviewEventIds {
                    review_requested: EventId("E-REQ1-REQUESTED".to_string()),
                    needs_human_state_changed: EventId("E-REQ1-NH-STATE".to_string()),
                    needs_human_event: EventId("E-REQ1-NH".to_string()),
                },
                Utc::now(),
            )
            .expect("request review");

        assert_eq!(outcome.task.state, TaskState::Reviewing);
        assert_eq!(
            outcome.task.review_status.required_models,
            vec![ModelKind::Claude, ModelKind::Codex]
        );
        assert_eq!(outcome.task.review_status.approvals_required, 2);
        assert!(!outcome.computation.evaluation.needs_human);

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::ReviewRequested { required_models }
                    if required_models == &vec![ModelKind::Claude, ModelKind::Codex]
            )
        }));
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, EventKind::NeedsHuman { .. })));
    }

    #[test]
    fn request_review_transitions_to_needs_human_when_capacity_is_insufficient() {
        let svc = mk_service();
        let task = mk_task("TREQ2", TaskState::Reviewing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let outcome = svc
            .request_review(
                &task.id,
                &ReviewGateConfig {
                    enabled_models: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
                    policy: ReviewPolicy::Adaptive,
                    min_approvals: 2,
                },
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
                RequestReviewEventIds {
                    review_requested: EventId("E-REQ2-REQUESTED".to_string()),
                    needs_human_state_changed: EventId("E-REQ2-NH-STATE".to_string()),
                    needs_human_event: EventId("E-REQ2-NH".to_string()),
                },
                Utc::now(),
            )
            .expect("request review");

        assert_eq!(outcome.task.state, TaskState::NeedsHuman);
        assert_eq!(
            outcome.task.review_status.capacity_state,
            ReviewCapacityState::NeedsHuman
        );
        assert!(outcome.computation.evaluation.needs_human);

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::ReviewRequested { required_models }
                    if required_models == &vec![ModelKind::Claude]
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "REVIEWING" && to == "NEEDS_HUMAN"
            )
        }));
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, EventKind::NeedsHuman { .. })));
    }

    #[test]
    fn complete_review_records_approval_event_and_recomputes_status() {
        let svc = mk_service();
        let task = mk_task("TREV1", TaskState::Reviewing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let outcome = svc
            .complete_review(
                &task.id,
                ModelKind::Claude,
                mk_review_output(ReviewVerdict::Approve),
                &ReviewGateConfig {
                    enabled_models: vec![ModelKind::Claude, ModelKind::Codex],
                    policy: ReviewPolicy::Adaptive,
                    min_approvals: 2,
                },
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
                CompleteReviewEventIds {
                    review_completed: EventId("E-REV1-COMPLETE".to_string()),
                    needs_human_state_changed: EventId("E-REV1-NH-STATE".to_string()),
                    needs_human_event: EventId("E-REV1-NH".to_string()),
                },
                Utc::now(),
            )
            .expect("complete review");

        assert_eq!(outcome.task.state, TaskState::Reviewing);
        assert_eq!(outcome.task.review_status.approvals_received, 1);
        assert_eq!(outcome.task.review_status.approvals_required, 2);
        assert!(!outcome.computation.evaluation.approved);

        let approvals = svc.task_approvals(&task.id).expect("approvals");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].reviewer, ModelKind::Claude);
        assert_eq!(approvals[0].verdict, ReviewVerdict::Approve);

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::ReviewCompleted { reviewer, .. } if *reviewer == ModelKind::Claude
            )
        }));
    }

    #[test]
    fn complete_review_moves_to_needs_human_when_capacity_is_insufficient() {
        let svc = mk_service();
        let task = mk_task("TREV2", TaskState::Reviewing, &[]);
        svc.create_task(&task, &mk_created_event(&task))
            .expect("create task");

        let outcome = svc
            .complete_review(
                &task.id,
                ModelKind::Claude,
                mk_review_output(ReviewVerdict::Approve),
                &ReviewGateConfig {
                    enabled_models: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
                    policy: ReviewPolicy::Adaptive,
                    min_approvals: 2,
                },
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
                CompleteReviewEventIds {
                    review_completed: EventId("E-REV2-COMPLETE".to_string()),
                    needs_human_state_changed: EventId("E-REV2-NH-STATE".to_string()),
                    needs_human_event: EventId("E-REV2-NH".to_string()),
                },
                Utc::now(),
            )
            .expect("complete review");

        assert_eq!(outcome.task.state, TaskState::NeedsHuman);
        assert_eq!(
            outcome.task.review_status.capacity_state,
            ReviewCapacityState::NeedsHuman
        );
        assert!(outcome.computation.evaluation.needs_human);

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::TaskStateChanged { from, to }
                    if from == "REVIEWING" && to == "NEEDS_HUMAN"
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(&event.kind, EventKind::NeedsHuman { reason } if reason.contains("review capacity"))
        }));
    }
}
