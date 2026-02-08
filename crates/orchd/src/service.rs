use chrono::{DateTime, Utc};
use orch_core::events::{Event, EventKind};
use orch_core::state::TaskState;
use orch_core::types::{EventId, Task, TaskApproval, TaskId};
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
use crate::scheduler::{SchedulePlan, Scheduler, SchedulingInput};
use crate::state_machine::{task_state_tag, transition_task, StateMachineError};

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

#[derive(Debug)]
pub struct OrchdService {
    pub store: SqliteStore,
    pub event_log: JsonlEventLog,
    pub scheduler: Scheduler,
}

impl OrchdService {
    pub fn new(store: SqliteStore, event_log: JsonlEventLog, scheduler: Scheduler) -> Self {
        Self {
            store,
            event_log,
            scheduler,
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
        let mut task = self
            .store
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

    pub fn transition_task_state(
        &self,
        task_id: &TaskId,
        to: TaskState,
        event_id: EventId,
        at: DateTime<Utc>,
    ) -> Result<Task, ServiceError> {
        let mut task = self
            .store
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
        let mut task = self
            .store
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
                    mode: auto_submit.mode.expect("mode must exist when should_submit"),
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
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::events::{EventKind, ReviewVerdict};
    use orch_core::state::{ReviewCapacityState, ReviewPolicy, ReviewStatus, TaskState, VerifyStatus};
    use orch_core::types::{
        EventId, ModelKind, RepoId, SubmitMode, Task, TaskApproval, TaskId, TaskRole, TaskType,
    };
    use std::fs;
    use std::path::PathBuf;

    use crate::dependency_graph::InferredDependency;
    use crate::event_log::JsonlEventLog;
    use crate::lifecycle_gate::{ReadyGateInput, SubmitPolicy};
    use crate::review_gate::{
        ReviewEvaluation, ReviewGateConfig, ReviewRequirement, ReviewerAvailability,
    };
    use crate::scheduler::{Scheduler, SchedulerConfig};

    use super::{OrchdService, PromoteTaskEventIds};

    fn mk_service() -> OrchdService {
        let store = crate::persistence::SqliteStore::open_in_memory().expect("in-memory db");
        let dir = std::env::temp_dir().join(format!(
            "othala-orchd-test-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");

        let svc = OrchdService::new(
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
            depends_on: depends_on.iter().map(|x| TaskId((*x).to_string())).collect(),
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

    #[test]
    fn restack_targets_for_parent_update_uses_union_graph() {
        let svc = mk_service();
        let t1 = mk_task("T1", TaskState::Running, &[]);
        let t2 = mk_task("T2", TaskState::Running, &["T1"]);
        let t3 = mk_task("T3", TaskState::Running, &[]);
        svc.create_task(&t1, &mk_created_event(&t1)).expect("create t1");
        svc.create_task(&t2, &mk_created_event(&t2)).expect("create t2");
        svc.create_task(&t3, &mk_created_event(&t3)).expect("create t3");

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
    fn promote_task_after_review_moves_to_submitting_when_ready_and_auto_submit_enabled() {
        let svc = mk_service();
        let task = mk_task("T9", TaskState::Reviewing, &[]);
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

        let stored = svc.task(&task.id).expect("load task").expect("task exists");
        assert_eq!(stored.state, TaskState::Submitting);

        let events = svc.task_events(&task.id).expect("events");
        assert!(events.iter().any(|e| matches!(e.kind, EventKind::ReadyReached)));
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::SubmitStarted { .. })));
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
        assert_eq!(updated.review_status.required_models, vec![ModelKind::Claude]);
        assert_eq!(updated.review_status.approvals_required, 0);
        assert_eq!(updated.review_status.approvals_received, 0);
        assert!(computation.evaluation.needs_human);
        assert!(!computation.evaluation.approved);
    }
}
