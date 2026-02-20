//! MVP scheduler - simplified for single model per chat.

use chrono::{DateTime, Utc};
use orch_core::config::OrgConfig;
use orch_core::state::TaskState;
use orch_core::types::{ModelKind, RepoId, SubmitMode, TaskId, TaskPriority};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Scheduler configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulerConfig {
    pub per_repo_limit: usize,
    pub per_model_limit: HashMap<ModelKind, usize>,
}

impl SchedulerConfig {
    pub fn from_org_config(config: &OrgConfig) -> Self {
        let mut per_model_limit = HashMap::new();
        per_model_limit.insert(ModelKind::Claude, config.concurrency.claude);
        per_model_limit.insert(ModelKind::Codex, config.concurrency.codex);
        per_model_limit.insert(ModelKind::Gemini, config.concurrency.gemini);

        Self {
            per_repo_limit: config.concurrency.per_repo,
            per_model_limit,
        }
    }
}

/// A queued task waiting to be scheduled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedTask {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub depends_on: Vec<TaskId>,
    pub submit_mode: SubmitMode,
    pub preferred_model: Option<ModelKind>,
    pub priority: TaskPriority,
    pub enqueued_at: DateTime<Utc>,
}

/// A currently running task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningTask {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub model: ModelKind,
}

/// Model availability status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAvailability {
    pub model: ModelKind,
    pub available: bool,
}

/// Input for scheduling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulingInput {
    pub queued: Vec<QueuedTask>,
    pub running: Vec<RunningTask>,
    pub all_task_states: HashMap<TaskId, TaskState>,
    pub enabled_models: Vec<ModelKind>,
    pub availability: Vec<ModelAvailability>,
}

/// Reason a task was blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    DependenciesUnresolved,
    RepoLimitReached,
    ModelLimitReached,
    NoAvailableModel,
}

/// A scheduled assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledAssignment {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub model: ModelKind,
}

/// A blocked task with reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedTask {
    pub task_id: TaskId,
    pub reason: BlockReason,
}

/// Result of scheduling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulePlan {
    pub assignments: Vec<ScheduledAssignment>,
    pub blocked: Vec<BlockedTask>,
}

/// The scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduler {
    pub config: SchedulerConfig,
}

impl Scheduler {
    pub fn new(config: SchedulerConfig) -> Self {
        Self { config }
    }

    /// Create a scheduling plan.
    pub fn plan(&self, mut input: SchedulingInput) -> SchedulePlan {
        // Sort by priority (higher first), then by enqueue time (older first)
        input.queued.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.enqueued_at.cmp(&b.enqueued_at))
                .then_with(|| a.task_id.0.cmp(&b.task_id.0))
        });

        let mut repo_counts: HashMap<RepoId, usize> = HashMap::new();
        let mut model_counts: HashMap<ModelKind, usize> = HashMap::new();

        for running in &input.running {
            *repo_counts.entry(running.repo_id.clone()).or_insert(0) += 1;
            *model_counts.entry(running.model).or_insert(0) += 1;
        }

        let available_models =
            available_models_in_priority_order(&input.enabled_models, &input.availability);
        let mut assignments = Vec::new();
        let mut blocked = Vec::new();

        for queued in input.queued {
            let deps_resolved = queued.depends_on.iter().all(|dep| {
                matches!(input.all_task_states.get(dep), Some(TaskState::Merged))
                    || (queued.submit_mode == SubmitMode::Stack
                        && matches!(input.all_task_states.get(dep), Some(TaskState::AwaitingMerge)))
            });
            if !deps_resolved {
                blocked.push(BlockedTask {
                    task_id: queued.task_id,
                    reason: BlockReason::DependenciesUnresolved,
                });
                continue;
            }

            let repo_inflight = repo_counts.get(&queued.repo_id).copied().unwrap_or(0);
            if repo_inflight >= self.config.per_repo_limit {
                blocked.push(BlockedTask {
                    task_id: queued.task_id,
                    reason: BlockReason::RepoLimitReached,
                });
                continue;
            }

            if available_models.is_empty() {
                blocked.push(BlockedTask {
                    task_id: queued.task_id,
                    reason: BlockReason::NoAvailableModel,
                });
                continue;
            }

            let Some(model) = select_model_with_capacity(
                queued.preferred_model,
                &available_models,
                &model_counts,
                &self.config.per_model_limit,
            ) else {
                blocked.push(BlockedTask {
                    task_id: queued.task_id,
                    reason: BlockReason::ModelLimitReached,
                });
                continue;
            };

            *repo_counts.entry(queued.repo_id.clone()).or_insert(0) += 1;
            *model_counts.entry(model).or_insert(0) += 1;
            assignments.push(ScheduledAssignment {
                task_id: queued.task_id,
                repo_id: queued.repo_id,
                model,
            });
        }

        SchedulePlan {
            assignments,
            blocked,
        }
    }
}

fn available_models_in_priority_order(
    enabled_models: &[ModelKind],
    availability: &[ModelAvailability],
) -> Vec<ModelKind> {
    let mut explicit_availability = HashMap::new();
    for status in availability {
        explicit_availability.insert(status.model, status.available);
    }

    let mut seen = HashSet::new();
    enabled_models
        .iter()
        .copied()
        .filter(|model| seen.insert(*model))
        .filter(|model| explicit_availability.get(model).copied().unwrap_or(true))
        .collect()
}

fn select_model_with_capacity(
    preferred_model: Option<ModelKind>,
    available_models: &[ModelKind],
    model_counts: &HashMap<ModelKind, usize>,
    per_model_limit: &HashMap<ModelKind, usize>,
) -> Option<ModelKind> {
    let preferred = preferred_model.filter(|model| available_models.contains(model));
    if let Some(model) = preferred {
        if model_has_capacity(model, model_counts, per_model_limit) {
            return Some(model);
        }
    }

    available_models
        .iter()
        .copied()
        .filter(|model| Some(*model) != preferred)
        .find(|model| model_has_capacity(*model, model_counts, per_model_limit))
}

fn model_has_capacity(
    model: ModelKind,
    model_counts: &HashMap<ModelKind, usize>,
    per_model_limit: &HashMap<ModelKind, usize>,
) -> bool {
    let current = model_counts.get(&model).copied().unwrap_or(0);
    let limit = per_model_limit.get(&model).copied().unwrap_or(usize::MAX);
    current < limit
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn mk_scheduler(per_repo_limit: usize, per_model_limit: &[(ModelKind, usize)]) -> Scheduler {
        Scheduler::new(SchedulerConfig {
            per_repo_limit,
            per_model_limit: per_model_limit.iter().copied().collect(),
        })
    }

    fn mk_queued(
        id: &str,
        repo: &str,
        priority: TaskPriority,
        preferred_model: Option<ModelKind>,
    ) -> QueuedTask {
        QueuedTask {
            task_id: TaskId(id.to_string()),
            repo_id: RepoId(repo.to_string()),
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            preferred_model,
            priority,
            enqueued_at: Utc::now(),
        }
    }

    #[test]
    fn plan_schedules_preferred_model() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Claude, 10), (ModelKind::Codex, 10)]);
        let plan = scheduler.plan(SchedulingInput {
            queued: vec![mk_queued(
                "T1",
                "repo",
                TaskPriority::Normal,
                Some(ModelKind::Claude),
            )],
            running: Vec::new(),
            all_task_states: HashMap::new(),
            enabled_models: vec![ModelKind::Claude, ModelKind::Codex],
            availability: Vec::new(),
        });

        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.assignments[0].model, ModelKind::Claude);
        assert!(plan.blocked.is_empty());
    }

    #[test]
    fn plan_blocks_when_repo_limit_reached() {
        let scheduler = mk_scheduler(1, &[(ModelKind::Claude, 10)]);
        let plan = scheduler.plan(SchedulingInput {
            queued: vec![mk_queued(
                "T2",
                "repo-a",
                TaskPriority::Normal,
                Some(ModelKind::Claude),
            )],
            running: vec![RunningTask {
                task_id: TaskId("T1".to_string()),
                repo_id: RepoId("repo-a".to_string()),
                model: ModelKind::Claude,
            }],
            all_task_states: HashMap::new(),
            enabled_models: vec![ModelKind::Claude],
            availability: Vec::new(),
        });

        assert!(plan.assignments.is_empty());
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].reason, BlockReason::RepoLimitReached);
    }

    #[test]
    fn plan_blocks_when_model_limit_reached() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Claude, 1)]);
        let plan = scheduler.plan(SchedulingInput {
            queued: vec![mk_queued(
                "T2",
                "repo-b",
                TaskPriority::Normal,
                Some(ModelKind::Claude),
            )],
            running: vec![RunningTask {
                task_id: TaskId("T1".to_string()),
                repo_id: RepoId("repo-a".to_string()),
                model: ModelKind::Claude,
            }],
            all_task_states: HashMap::new(),
            enabled_models: vec![ModelKind::Claude],
            availability: Vec::new(),
        });

        assert!(plan.assignments.is_empty());
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].reason, BlockReason::ModelLimitReached);
    }

    #[test]
    fn plan_blocks_when_no_models_available() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Claude, 10)]);
        let plan = scheduler.plan(SchedulingInput {
            queued: vec![mk_queued("T1", "repo", TaskPriority::Normal, None)],
            running: Vec::new(),
            all_task_states: HashMap::new(),
            enabled_models: vec![ModelKind::Claude],
            availability: vec![ModelAvailability {
                model: ModelKind::Claude,
                available: false,
            }],
        });

        assert!(plan.assignments.is_empty());
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].reason, BlockReason::NoAvailableModel);
    }

    #[test]
    fn plan_falls_back_when_preferred_model_is_saturated() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Claude, 1), (ModelKind::Codex, 2)]);
        let plan = scheduler.plan(SchedulingInput {
            queued: vec![mk_queued(
                "T2",
                "repo-b",
                TaskPriority::Normal,
                Some(ModelKind::Claude),
            )],
            running: vec![RunningTask {
                task_id: TaskId("T1".to_string()),
                repo_id: RepoId("repo-a".to_string()),
                model: ModelKind::Claude,
            }],
            all_task_states: HashMap::new(),
            enabled_models: vec![ModelKind::Claude, ModelKind::Codex],
            availability: Vec::new(),
        });

        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.assignments[0].model, ModelKind::Codex);
        assert!(plan.blocked.is_empty());
    }

    #[test]
    fn plan_uses_enabled_model_order_for_default_selection() {
        let scheduler = mk_scheduler(
            10,
            &[
                (ModelKind::Claude, 10),
                (ModelKind::Codex, 10),
                (ModelKind::Gemini, 10),
            ],
        );
        let plan = scheduler.plan(SchedulingInput {
            queued: vec![mk_queued("T1", "repo", TaskPriority::Normal, None)],
            running: Vec::new(),
            all_task_states: HashMap::new(),
            enabled_models: vec![ModelKind::Gemini, ModelKind::Codex, ModelKind::Claude],
            availability: Vec::new(),
        });

        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.assignments[0].model, ModelKind::Gemini);
        assert!(plan.blocked.is_empty());
    }

    #[test]
    fn plan_prioritizes_critical_tasks_before_lower_priority() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Claude, 10)]);
        let now = Utc::now();
        let mut low = mk_queued("T-low", "repo", TaskPriority::Low, Some(ModelKind::Claude));
        low.enqueued_at = now - chrono::Duration::seconds(30);
        let mut critical = mk_queued(
            "T-critical",
            "repo",
            TaskPriority::Critical,
            Some(ModelKind::Claude),
        );
        critical.enqueued_at = now;

        let plan = scheduler.plan(SchedulingInput {
            queued: vec![low, critical],
            running: Vec::new(),
            all_task_states: HashMap::new(),
            enabled_models: vec![ModelKind::Claude],
            availability: Vec::new(),
        });

        assert_eq!(plan.assignments.len(), 2);
        assert_eq!(plan.assignments[0].task_id.0, "T-critical");
    }

    #[test]
    fn dependency_blocks_task() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Claude, 10)]);
        let mut queued = mk_queued("T2", "repo", TaskPriority::Normal, None);
        queued.depends_on = vec![TaskId("T1".to_string())];

        let mut all_task_states = HashMap::new();
        all_task_states.insert(TaskId("T1".to_string()), TaskState::Ready);

        let plan = scheduler.plan(SchedulingInput {
            queued: vec![queued],
            running: Vec::new(),
            all_task_states,
            enabled_models: vec![ModelKind::Claude],
            availability: Vec::new(),
        });

        assert!(plan.assignments.is_empty());
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].reason, BlockReason::DependenciesUnresolved);
    }

    #[test]
    fn dependency_allows_when_resolved() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Claude, 10)]);
        let mut queued = mk_queued("T2", "repo", TaskPriority::Normal, None);
        queued.depends_on = vec![TaskId("T1".to_string())];

        let mut all_task_states = HashMap::new();
        all_task_states.insert(TaskId("T1".to_string()), TaskState::Merged);

        let plan = scheduler.plan(SchedulingInput {
            queued: vec![queued],
            running: Vec::new(),
            all_task_states,
            enabled_models: vec![ModelKind::Claude],
            availability: Vec::new(),
        });

        assert_eq!(plan.assignments.len(), 1);
        assert!(plan.blocked.is_empty());
    }

    #[test]
    fn stack_dependency_allows_awaiting_merge_parent() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Claude, 10)]);
        let mut queued = mk_queued("T2", "repo", TaskPriority::Normal, None);
        queued.submit_mode = SubmitMode::Stack;
        queued.depends_on = vec![TaskId("T1".to_string())];

        let mut all_task_states = HashMap::new();
        all_task_states.insert(TaskId("T1".to_string()), TaskState::AwaitingMerge);

        let plan = scheduler.plan(SchedulingInput {
            queued: vec![queued],
            running: Vec::new(),
            all_task_states,
            enabled_models: vec![ModelKind::Claude],
            availability: Vec::new(),
        });

        assert_eq!(plan.assignments.len(), 1);
        assert!(plan.blocked.is_empty());
    }
}
