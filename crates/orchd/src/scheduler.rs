//! MVP scheduler - simplified for single model per chat.

use chrono::{DateTime, Utc};
use orch_core::config::OrgConfig;
use orch_core::types::{ModelKind, RepoId, TaskId};
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
    pub preferred_model: Option<ModelKind>,
    pub priority: i32,
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
    pub enabled_models: Vec<ModelKind>,
    pub availability: Vec<ModelAvailability>,
}

/// Reason a task was blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
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

        let available_models = available_models_in_priority_order(
            &input.enabled_models,
            &input.availability,
        );
        let mut assignments = Vec::new();
        let mut blocked = Vec::new();

        for queued in input.queued {
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
        .into_iter()
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
        priority: i32,
        preferred_model: Option<ModelKind>,
    ) -> QueuedTask {
        QueuedTask {
            task_id: TaskId(id.to_string()),
            repo_id: RepoId(repo.to_string()),
            preferred_model,
            priority,
            enqueued_at: Utc::now(),
        }
    }

    #[test]
    fn plan_schedules_preferred_model() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Claude, 10), (ModelKind::Codex, 10)]);
        let plan = scheduler.plan(SchedulingInput {
            queued: vec![mk_queued("T1", "repo", 1, Some(ModelKind::Claude))],
            running: Vec::new(),
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
            queued: vec![mk_queued("T2", "repo-a", 1, Some(ModelKind::Claude))],
            running: vec![RunningTask {
                task_id: TaskId("T1".to_string()),
                repo_id: RepoId("repo-a".to_string()),
                model: ModelKind::Claude,
            }],
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
            queued: vec![mk_queued("T2", "repo-b", 1, Some(ModelKind::Claude))],
            running: vec![RunningTask {
                task_id: TaskId("T1".to_string()),
                repo_id: RepoId("repo-a".to_string()),
                model: ModelKind::Claude,
            }],
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
            queued: vec![mk_queued("T1", "repo", 1, None)],
            running: Vec::new(),
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
            queued: vec![mk_queued("T2", "repo-b", 1, Some(ModelKind::Claude))],
            running: vec![RunningTask {
                task_id: TaskId("T1".to_string()),
                repo_id: RepoId("repo-a".to_string()),
                model: ModelKind::Claude,
            }],
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
            queued: vec![mk_queued("T1", "repo", 1, None)],
            running: Vec::new(),
            enabled_models: vec![ModelKind::Gemini, ModelKind::Codex, ModelKind::Claude],
            availability: Vec::new(),
        });

        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.assignments[0].model, ModelKind::Gemini);
        assert!(plan.blocked.is_empty());
    }
}
