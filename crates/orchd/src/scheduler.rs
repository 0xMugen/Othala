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

            let candidates =
                candidate_models_for_task(&queued, &input.enabled_models, &available_models);
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

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use orch_core::config::parse_org_config;
    use orch_core::state::ReviewPolicy;
    use orch_core::types::{ModelKind, RepoId, TaskId};
    use std::collections::HashMap;

    use super::{
        BlockReason, ModelAvailability, QueuedTask, RunningTask, Scheduler, SchedulerConfig,
        SchedulingInput,
    };

    fn mk_scheduler(per_repo_limit: usize, per_model_limit: &[(ModelKind, usize)]) -> Scheduler {
        Scheduler::new(SchedulerConfig {
            per_repo_limit,
            per_model_limit: per_model_limit.iter().copied().collect::<HashMap<_, _>>(),
        })
    }

    fn mk_queued(
        id: &str,
        repo: &str,
        priority: i32,
        enqueued_at: chrono::DateTime<Utc>,
        preferred_model: Option<ModelKind>,
        eligible_models: &[ModelKind],
    ) -> QueuedTask {
        QueuedTask {
            task_id: TaskId(id.to_string()),
            repo_id: RepoId(repo.to_string()),
            preferred_model,
            eligible_models: eligible_models.to_vec(),
            priority,
            enqueued_at,
        }
    }

    fn mk_running(id: &str, repo: &str, model: ModelKind) -> RunningTask {
        RunningTask {
            task_id: TaskId(id.to_string()),
            repo_id: RepoId(repo.to_string()),
            model,
        }
    }

    fn empty_plan_input(queued: Vec<QueuedTask>) -> SchedulingInput {
        SchedulingInput {
            queued,
            running: Vec::new(),
            enabled_models: vec![ModelKind::Codex, ModelKind::Claude, ModelKind::Gemini],
            availability: Vec::new(),
        }
    }

    #[test]
    fn plan_orders_by_priority_then_age_then_task_id() {
        let scheduler = mk_scheduler(
            10,
            &[
                (ModelKind::Codex, 10),
                (ModelKind::Claude, 10),
                (ModelKind::Gemini, 10),
            ],
        );
        let base = Utc::now();
        let queued = vec![
            mk_queued("T3", "repo", 5, base, Some(ModelKind::Codex), &[]),
            mk_queued(
                "T2",
                "repo",
                10,
                base + Duration::seconds(5),
                Some(ModelKind::Codex),
                &[],
            ),
            mk_queued(
                "T1",
                "repo",
                10,
                base - Duration::seconds(5),
                Some(ModelKind::Codex),
                &[],
            ),
        ];

        let plan = scheduler.plan(empty_plan_input(queued));
        let assigned = plan
            .assignments
            .iter()
            .map(|assignment| assignment.task_id.0.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            assigned,
            vec!["T1".to_string(), "T2".to_string(), "T3".to_string()]
        );
    }

    #[test]
    fn plan_blocks_when_repo_limit_reached() {
        let scheduler = mk_scheduler(
            1,
            &[
                (ModelKind::Codex, 10),
                (ModelKind::Claude, 10),
                (ModelKind::Gemini, 10),
            ],
        );
        let input = SchedulingInput {
            queued: vec![mk_queued(
                "TQ",
                "repo-a",
                1,
                Utc::now(),
                Some(ModelKind::Codex),
                &[],
            )],
            running: vec![mk_running("TR", "repo-a", ModelKind::Codex)],
            enabled_models: vec![ModelKind::Codex],
            availability: Vec::new(),
        };

        let plan = scheduler.plan(input);
        assert!(plan.assignments.is_empty());
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].task_id, TaskId("TQ".to_string()));
        assert_eq!(plan.blocked[0].reason, BlockReason::RepoLimitReached);
    }

    #[test]
    fn plan_blocks_when_model_limit_reached() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Codex, 1)]);
        let input = SchedulingInput {
            queued: vec![mk_queued(
                "TQ",
                "repo-a",
                1,
                Utc::now(),
                Some(ModelKind::Codex),
                &[],
            )],
            running: vec![mk_running("TR", "repo-b", ModelKind::Codex)],
            enabled_models: vec![ModelKind::Codex],
            availability: Vec::new(),
        };

        let plan = scheduler.plan(input);
        assert!(plan.assignments.is_empty());
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].reason, BlockReason::ModelLimitReached);
    }

    #[test]
    fn plan_uses_preferred_model_when_available_and_eligible() {
        let scheduler = mk_scheduler(
            10,
            &[
                (ModelKind::Codex, 10),
                (ModelKind::Claude, 10),
                (ModelKind::Gemini, 10),
            ],
        );
        let input = SchedulingInput {
            queued: vec![mk_queued(
                "TQ",
                "repo-a",
                1,
                Utc::now(),
                Some(ModelKind::Gemini),
                &[ModelKind::Gemini, ModelKind::Claude],
            )],
            running: Vec::new(),
            enabled_models: vec![ModelKind::Claude, ModelKind::Gemini],
            availability: vec![ModelAvailability {
                model: ModelKind::Gemini,
                available: true,
            }],
        };

        let plan = scheduler.plan(input);
        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.assignments[0].model, ModelKind::Gemini);
        assert!(plan.blocked.is_empty());
    }

    #[test]
    fn plan_blocks_with_no_available_model_when_eligibility_excludes_enabled_models() {
        let scheduler = mk_scheduler(
            10,
            &[
                (ModelKind::Codex, 10),
                (ModelKind::Claude, 10),
                (ModelKind::Gemini, 10),
            ],
        );
        let input = SchedulingInput {
            queued: vec![mk_queued(
                "TQ",
                "repo-a",
                1,
                Utc::now(),
                None,
                &[ModelKind::Gemini],
            )],
            running: Vec::new(),
            enabled_models: vec![ModelKind::Claude, ModelKind::Codex],
            availability: Vec::new(),
        };

        let plan = scheduler.plan(input);
        assert!(plan.assignments.is_empty());
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].reason, BlockReason::NoAvailableModel);
    }

    #[test]
    fn scheduler_config_from_org_config_maps_repo_and_model_limits() {
        let org = parse_org_config(
            r#"
[models]
enabled = ["claude", "codex", "gemini"]
policy = "strict"
min_approvals = 3

[concurrency]
per_repo = 7
claude = 11
codex = 13
gemini = 17

[graphite]
auto_submit = true
submit_mode_default = "single"
allow_move = "manual"

[ui]
web_bind = "127.0.0.1:9842"
"#,
        )
        .expect("parse org config");
        assert_eq!(org.models.policy, ReviewPolicy::Strict);

        let cfg = SchedulerConfig::from_org_config(&org);
        assert_eq!(cfg.per_repo_limit, 7);
        assert_eq!(cfg.per_model_limit.get(&ModelKind::Claude), Some(&11));
        assert_eq!(cfg.per_model_limit.get(&ModelKind::Codex), Some(&13));
        assert_eq!(cfg.per_model_limit.get(&ModelKind::Gemini), Some(&17));
    }

    #[test]
    fn plan_treats_missing_availability_entry_as_available() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Codex, 10), (ModelKind::Claude, 10)]);
        let input = SchedulingInput {
            queued: vec![mk_queued(
                "TQ",
                "repo-a",
                1,
                Utc::now(),
                Some(ModelKind::Codex),
                &[ModelKind::Codex],
            )],
            running: Vec::new(),
            enabled_models: vec![ModelKind::Codex, ModelKind::Claude],
            availability: vec![ModelAvailability {
                model: ModelKind::Claude,
                available: false,
            }],
        };

        let plan = scheduler.plan(input);
        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.assignments[0].model, ModelKind::Codex);
        assert!(plan.blocked.is_empty());
    }

    #[test]
    fn plan_falls_back_when_preferred_model_unavailable() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Codex, 10), (ModelKind::Claude, 10)]);
        let input = SchedulingInput {
            queued: vec![mk_queued(
                "TQ",
                "repo-a",
                1,
                Utc::now(),
                Some(ModelKind::Codex),
                &[ModelKind::Codex, ModelKind::Claude],
            )],
            running: Vec::new(),
            enabled_models: vec![ModelKind::Codex, ModelKind::Claude],
            availability: vec![ModelAvailability {
                model: ModelKind::Codex,
                available: false,
            }],
        };

        let plan = scheduler.plan(input);
        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.assignments[0].model, ModelKind::Claude);
        assert!(plan.blocked.is_empty());
    }

    #[test]
    fn plan_never_uses_models_not_enabled_even_if_marked_available() {
        let scheduler = mk_scheduler(10, &[(ModelKind::Gemini, 10)]);
        let input = SchedulingInput {
            queued: vec![mk_queued(
                "TQ",
                "repo-a",
                1,
                Utc::now(),
                Some(ModelKind::Gemini),
                &[ModelKind::Gemini],
            )],
            running: Vec::new(),
            enabled_models: vec![ModelKind::Codex],
            availability: vec![ModelAvailability {
                model: ModelKind::Gemini,
                available: true,
            }],
        };

        let plan = scheduler.plan(input);
        assert!(plan.assignments.is_empty());
        assert_eq!(plan.blocked.len(), 1);
        assert_eq!(plan.blocked[0].reason, BlockReason::NoAvailableModel);
    }
}
