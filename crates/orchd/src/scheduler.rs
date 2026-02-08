use chrono::{DateTime, Utc};
use orch_core::config::OrgConfig;
use orch_core::types::{ModelKind, RepoId, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedTask {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub preferred_model: Option<ModelKind>,
    pub eligible_models: Vec<ModelKind>,
    pub priority: i32,
    pub enqueued_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningTask {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub model: ModelKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAvailability {
    pub model: ModelKind,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulingInput {
    pub queued: Vec<QueuedTask>,
    pub running: Vec<RunningTask>,
    pub enabled_models: Vec<ModelKind>,
    pub availability: Vec<ModelAvailability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    RepoLimitReached,
    ModelLimitReached,
    NoAvailableModel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledAssignment {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub model: ModelKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedTask {
    pub task_id: TaskId,
    pub reason: BlockReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulePlan {
    pub assignments: Vec<ScheduledAssignment>,
    pub blocked: Vec<BlockedTask>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduler {
    pub config: SchedulerConfig,
}

impl Scheduler {
    pub fn new(config: SchedulerConfig) -> Self {
        Self { config }
    }

    pub fn plan(&self, mut input: SchedulingInput) -> SchedulePlan {
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

        let available_models = available_model_set(&input.enabled_models, &input.availability);
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

            let candidates = candidate_models_for_task(&queued, &input.enabled_models, &available_models);
            if candidates.is_empty() {
                blocked.push(BlockedTask {
                    task_id: queued.task_id,
                    reason: BlockReason::NoAvailableModel,
                });
                continue;
            }

            let selected = candidates.into_iter().find(|model| {
                let current = model_counts.get(model).copied().unwrap_or(0);
                let limit = self
                    .config
                    .per_model_limit
                    .get(model)
                    .copied()
                    .unwrap_or(usize::MAX);
                current < limit
            });

            let Some(model) = selected else {
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

fn available_model_set(
    enabled_models: &[ModelKind],
    availability: &[ModelAvailability],
) -> HashSet<ModelKind> {
    let enabled: HashSet<ModelKind> = enabled_models.iter().copied().collect();
    let mut explicit_availability = HashMap::new();
    for status in availability {
        explicit_availability.insert(status.model, status.available);
    }

    enabled
        .into_iter()
        .filter(|model| explicit_availability.get(model).copied().unwrap_or(true))
        .collect()
}

fn candidate_models_for_task(
    task: &QueuedTask,
    enabled_models: &[ModelKind],
    available_models: &HashSet<ModelKind>,
) -> Vec<ModelKind> {
    if let Some(preferred) = task.preferred_model {
        if available_models.contains(&preferred)
            && (task.eligible_models.is_empty() || task.eligible_models.contains(&preferred))
        {
            return vec![preferred];
        }
    }

    let mut candidates = Vec::new();
    let eligible: HashSet<ModelKind> = task.eligible_models.iter().copied().collect();

    for model in enabled_models {
        if !available_models.contains(model) {
            continue;
        }
        if !eligible.is_empty() && !eligible.contains(model) {
            continue;
        }
        candidates.push(*model);
    }

    candidates
}
