use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use orch_core::config::{OrgConfig, RepoConfig};
use orch_core::events::{Event, EventKind};
use orch_core::state::{TaskState, VerifyTier};
use orch_core::types::{EventId, ModelKind, Task, TaskId};
use orch_git::{current_branch, discover_repo, GitCli, GitError, WorktreeManager, WorktreeSpec};
use orch_graphite::{GraphiteClient, GraphiteError, RestackOutcome};
use orch_verify::{commands_for_tier, resolve_verify_commands, VerifyOutcome, VerifyRunner};

use crate::service::{
    CompleteFullVerifyEventIds, CompleteQuickVerifyEventIds, CompleteRestackEventIds,
    CompleteSubmitEventIds, OrchdService, ServiceError,
};

static EVENT_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeTickSummary {
    pub initialized: usize,
    pub verify_started: usize,
    pub restacked: usize,
    pub restack_conflicts: usize,
    pub verify_passed: usize,
    pub verify_failed: usize,
    pub submitted: usize,
    pub submit_failed: usize,
    pub errors: usize,
}

impl RuntimeTickSummary {
    pub fn touched(&self) -> bool {
        self.initialized > 0
            || self.verify_started > 0
            || self.restacked > 0
            || self.restack_conflicts > 0
            || self.verify_passed > 0
            || self.verify_failed > 0
            || self.submitted > 0
            || self.submit_failed > 0
            || self.errors > 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Service(#[from] ServiceError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEngine {
    pub git: GitCli,
    pub worktrees: WorktreeManager,
    pub verify: VerifyRunner,
}

impl Default for RuntimeEngine {
    fn default() -> Self {
        Self {
            git: GitCli::default(),
            worktrees: WorktreeManager::default(),
            verify: VerifyRunner::default(),
        }
    }
}

impl RuntimeEngine {
    pub fn tick(
        &self,
        service: &OrchdService,
        org_config: &OrgConfig,
        repo_configs: &HashMap<String, RepoConfig>,
        model_availability: &HashMap<ModelKind, bool>,
        at: DateTime<Utc>,
    ) -> Result<RuntimeTickSummary, RuntimeError> {
        let mut summary = RuntimeTickSummary::default();

        let initializing = service.list_tasks_by_state(TaskState::Initializing)?;
        for task in initializing {
            if self.initialize_task(service, repo_configs, &task, at)? {
                summary.initialized += 1;
            } else if matches!(service.task(&task.id)?, Some(ref t) if t.state == TaskState::Failed)
            {
                summary.errors += 1;
            }
        }

        let restacking = service.list_tasks_by_state(TaskState::Restacking)?;
        for task in restacking {
            match self.restack_task(service, repo_configs, &task, at)? {
                RestackTickOutcome::Restacked => summary.restacked += 1,
                RestackTickOutcome::Conflict => summary.restack_conflicts += 1,
                RestackTickOutcome::Failed => summary.errors += 1,
            }
        }

        let running = service.list_tasks_by_state(TaskState::Running)?;
        for task in running {
            if self.maybe_start_quick_verify(service, &task, at)? {
                summary.verify_started += 1;
            }
        }

        let verifying_quick = service.list_tasks_by_state(TaskState::VerifyingQuick)?;
        for task in verifying_quick {
            if self.verify_task(service, repo_configs, &task, VerifyTier::Quick, at)? {
                summary.verify_passed += 1;
            } else {
                summary.verify_failed += 1;
            }
        }

        let verifying_full = service.list_tasks_by_state(TaskState::VerifyingFull)?;
        for task in verifying_full {
            if self.verify_task(service, repo_configs, &task, VerifyTier::Full, at)? {
                summary.verify_passed += 1;
            } else {
                summary.verify_failed += 1;
            }
        }

        let submitting = service.list_tasks_by_state(TaskState::Submitting)?;
        for task in submitting {
            if self.submit_task(service, repo_configs, &task, at)? {
                summary.submitted += 1;
            } else {
                summary.submit_failed += 1;
            }
        }

        self.promote_ready_tasks(service, org_config, repo_configs, model_availability, at)?;

        Ok(summary)
    }

    fn initialize_task(
        &self,
        service: &OrchdService,
        repo_configs: &HashMap<String, RepoConfig>,
        task: &Task,
        at: DateTime<Utc>,
    ) -> Result<bool, RuntimeError> {
        let repo_config = match repo_configs.get(&task.repo_id.0) {
            Some(cfg) => cfg,
            None => {
                self.mark_task_failed(
                    service,
                    task,
                    "repo_config_missing",
                    format!("repo config not found for repo_id={}", task.repo_id.0),
                    at,
                )?;
                return Ok(false);
            }
        };

        let repo = match discover_repo(&repo_config.repo_path, &self.git) {
            Ok(repo) => repo,
            Err(err) => {
                self.mark_task_failed(
                    service,
                    task,
                    "repo_discovery_failed",
                    format!(
                        "failed to discover repository at {}: {err}",
                        repo_config.repo_path.display()
                    ),
                    at,
                )?;
                return Ok(false);
            }
        };

        let branch_before_create = match current_branch(&repo, &self.git) {
            Ok(branch_name) => branch_name,
            Err(err) => {
                self.mark_task_failed(
                    service,
                    task,
                    "current_branch_failed",
                    format!("failed to resolve current branch before gt create: {err}"),
                    at,
                )?;
                return Ok(false);
            }
        };

        let branch = task
            .branch_name
            .clone()
            .unwrap_or_else(|| default_branch_name(task));
        let graphite = GraphiteClient::new(repo.root.clone());
        if let Err(err) = graphite.create_branch(&branch) {
            if !is_branch_already_present_error(&err) {
                self.mark_task_failed(
                    service,
                    task,
                    "graphite_create_failed",
                    format!("failed to create graphite branch '{branch}': {err}"),
                    at,
                )?;
                return Ok(false);
            }
        }

        if branch_before_create != branch {
            match current_branch(&repo, &self.git) {
                Ok(active_branch) if active_branch == branch => {
                    if let Err(err) = self
                        .git
                        .run(&repo.root, ["switch", branch_before_create.as_str()])
                    {
                        self.mark_task_failed(
                            service,
                            task,
                            "restore_branch_failed",
                            format!(
                                "failed to switch primary worktree back to '{before}': {err}",
                                before = branch_before_create
                            ),
                            at,
                        )?;
                        return Ok(false);
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    self.mark_task_failed(
                        service,
                        task,
                        "current_branch_failed",
                        format!("failed to resolve current branch after gt create: {err}"),
                        at,
                    )?;
                    return Ok(false);
                }
            }
        }

        let target_worktree = self.worktrees.task_worktree_path(&repo, &task.id);
        if !target_worktree.exists() {
            let spec = WorktreeSpec {
                task_id: task.id.clone(),
                branch: branch.clone(),
            };
            if let Err(err) = self.worktrees.create_for_existing_branch(&repo, &spec) {
                if !is_worktree_already_present_error(&err) {
                    self.mark_task_failed(
                        service,
                        task,
                        "worktree_create_failed",
                        format!(
                            "failed to create worktree for branch '{branch}' at {}: {err}",
                            target_worktree.display()
                        ),
                        at,
                    )?;
                    return Ok(false);
                }
            }
        }

        let mut latest = service
            .task(&task.id)?
            .ok_or_else(|| ServiceError::TaskNotFound {
                task_id: task.id.0.clone(),
            })?;
        if latest.branch_name.as_deref() != Some(branch.as_str()) {
            latest.branch_name = Some(branch);
            latest.updated_at = at;
            service
                .store
                .upsert_task(&latest)
                .map_err(ServiceError::from)?;
        }

        if latest.state == TaskState::Initializing {
            if latest.pr.is_none() && repo_config.graphite.draft_on_start {
                latest = service.mark_task_draft_pr_open(
                    &latest.id,
                    0,
                    synthetic_draft_pr_url(&latest.id),
                    crate::service::DraftPrEventIds {
                        draft_pr_state_changed: event_id(&latest.id, "INIT-DRAFT-S", at),
                        draft_pr_created: event_id(&latest.id, "INIT-DRAFT-E", at),
                    },
                    at,
                )?;
            } else {
                latest = service.transition_task_state(
                    &latest.id,
                    TaskState::DraftPrOpen,
                    event_id(&latest.id, "INIT-DRAFT", at),
                    at,
                )?;
            }
        }

        if latest.state == TaskState::DraftPrOpen {
            let _ = service.transition_task_state(
                &latest.id,
                TaskState::Running,
                event_id(&latest.id, "DRAFT-RUNNING", at),
                at,
            )?;
        }

        let _ = service
            .store
            .finish_open_runs_for_task(&task.id, at, "initialized", Some(0))
            .map_err(ServiceError::from)?;

        Ok(true)
    }

    fn restack_task(
        &self,
        service: &OrchdService,
        repo_configs: &HashMap<String, RepoConfig>,
        task: &Task,
        at: DateTime<Utc>,
    ) -> Result<RestackTickOutcome, RuntimeError> {
        let repo_config = match repo_configs.get(&task.repo_id.0) {
            Some(cfg) => cfg,
            None => {
                self.mark_task_failed(
                    service,
                    task,
                    "repo_config_missing",
                    format!("repo config not found for repo_id={}", task.repo_id.0),
                    at,
                )?;
                return Ok(RestackTickOutcome::Failed);
            }
        };

        let runtime_path = task_runtime_path(task, repo_config);
        let client = GraphiteClient::new(runtime_path);

        match client.restack_with_outcome() {
            Ok(RestackOutcome::Restacked) => {
                let _ = service.complete_restack(
                    &task.id,
                    false,
                    CompleteRestackEventIds {
                        restack_completed: event_id(&task.id, "RESTACK-COMPLETED", at),
                        success_state_changed: event_id(&task.id, "RESTACK-VERIFY", at),
                        conflict_event: event_id(&task.id, "RESTACK-CONFLICT-E", at),
                        conflict_state_changed: event_id(&task.id, "RESTACK-CONFLICT-S", at),
                    },
                    at,
                )?;
                Ok(RestackTickOutcome::Restacked)
            }
            Ok(RestackOutcome::Conflict { .. }) => {
                let _ = service.complete_restack(
                    &task.id,
                    true,
                    CompleteRestackEventIds {
                        restack_completed: event_id(&task.id, "RESTACK-COMPLETED", at),
                        success_state_changed: event_id(&task.id, "RESTACK-VERIFY", at),
                        conflict_event: event_id(&task.id, "RESTACK-CONFLICT-E", at),
                        conflict_state_changed: event_id(&task.id, "RESTACK-CONFLICT-S", at),
                    },
                    at,
                )?;
                Ok(RestackTickOutcome::Conflict)
            }
            Err(err) => {
                self.mark_task_failed(
                    service,
                    task,
                    "restack_failed",
                    format!("failed to restack task {}: {err}", task.id.0),
                    at,
                )?;
                Ok(RestackTickOutcome::Failed)
            }
        }
    }

    fn verify_task(
        &self,
        service: &OrchdService,
        repo_configs: &HashMap<String, RepoConfig>,
        task: &Task,
        tier: VerifyTier,
        at: DateTime<Utc>,
    ) -> Result<bool, RuntimeError> {
        let repo_config = match repo_configs.get(&task.repo_id.0) {
            Some(cfg) => cfg,
            None => {
                self.mark_task_failed(
                    service,
                    task,
                    "repo_config_missing",
                    format!("repo config not found for repo_id={}", task.repo_id.0),
                    at,
                )?;
                return Ok(false);
            }
        };

        let runtime_path = task_runtime_path(task, repo_config);
        let configured = commands_for_tier(repo_config, tier);
        let commands = resolve_verify_commands(&runtime_path, tier, &configured);
        let verify_result =
            self.verify
                .run_tier(&runtime_path, &repo_config.nix.dev_shell, tier, &commands);

        let verify_result = match verify_result {
            Ok(result) => result,
            Err(err) => {
                self.mark_task_failed(
                    service,
                    task,
                    "verify_runner_failed",
                    format!("verify runner failed for task {}: {err}", task.id.0),
                    at,
                )?;
                return Ok(false);
            }
        };
        let success = verify_result.outcome == VerifyOutcome::Passed;
        let failure_summary = if success {
            None
        } else {
            Some(render_verify_failure_summary(&verify_result))
        };

        match tier {
            VerifyTier::Quick => {
                let _ = service.complete_quick_verify(
                    &task.id,
                    success,
                    failure_summary,
                    CompleteQuickVerifyEventIds {
                        verify_completed: event_id(&task.id, "VERIFY-QUICK-DONE", at),
                        success_state_changed: event_id(&task.id, "VERIFY-QUICK-REVIEW", at),
                        failure_state_changed: event_id(&task.id, "VERIFY-QUICK-RUNNING", at),
                    },
                    at,
                )?;
            }
            VerifyTier::Full => {
                let _ = service.complete_full_verify(
                    &task.id,
                    success,
                    failure_summary,
                    TaskState::Running,
                    TaskState::Failed,
                    CompleteFullVerifyEventIds {
                        verify_completed: event_id(&task.id, "VERIFY-FULL-DONE", at),
                        success_state_changed: event_id(&task.id, "VERIFY-FULL-SUCCESS", at),
                        failure_state_changed: event_id(&task.id, "VERIFY-FULL-FAILED", at),
                    },
                    at,
                )?;
            }
        }

        Ok(success)
    }

    fn maybe_start_quick_verify(
        &self,
        service: &OrchdService,
        task: &Task,
        at: DateTime<Utc>,
    ) -> Result<bool, RuntimeError> {
        let _ = (service, task, at);
        // Keep tasks in RUNNING until verify is explicitly requested from UI/CLI.
        Ok(false)
    }

    fn submit_task(
        &self,
        service: &OrchdService,
        repo_configs: &HashMap<String, RepoConfig>,
        task: &Task,
        at: DateTime<Utc>,
    ) -> Result<bool, RuntimeError> {
        let repo_config = match repo_configs.get(&task.repo_id.0) {
            Some(cfg) => cfg,
            None => {
                self.mark_task_failed(
                    service,
                    task,
                    "repo_config_missing",
                    format!("repo config not found for repo_id={}", task.repo_id.0),
                    at,
                )?;
                return Ok(false);
            }
        };

        let runtime_path = task_runtime_path(task, repo_config);
        let mode = repo_config.graphite.submit_mode.unwrap_or(task.submit_mode);
        let client = GraphiteClient::new(runtime_path);
        let submit_result = client.submit(mode);

        let success = submit_result.is_ok();
        let failure_message = submit_result.err().map(|err| err.to_string());
        let _ = service.complete_submit(
            &task.id,
            success,
            failure_message,
            CompleteSubmitEventIds {
                submit_completed: event_id(&task.id, "SUBMIT-DONE", at),
                success_state_changed: event_id(&task.id, "SUBMIT-AWAITING", at),
                failure_state_changed: event_id(&task.id, "SUBMIT-FAILED", at),
                failure_error_event: event_id(&task.id, "SUBMIT-ERROR", at),
            },
            at,
        )?;
        Ok(success)
    }

    fn promote_ready_tasks(
        &self,
        service: &OrchdService,
        org_config: &OrgConfig,
        repo_configs: &HashMap<String, RepoConfig>,
        model_availability: &HashMap<ModelKind, bool>,
        at: DateTime<Utc>,
    ) -> Result<(), RuntimeError> {
        let review_config = crate::review_gate::ReviewGateConfig {
            enabled_models: org_config.models.enabled.clone(),
            policy: org_config.models.policy,
            min_approvals: org_config.models.min_approvals,
        };
        let availability = org_config
            .models
            .enabled
            .iter()
            .copied()
            .map(|model| crate::review_gate::ReviewerAvailability {
                model,
                available: model_availability.get(&model).copied().unwrap_or(false),
            })
            .collect::<Vec<_>>();

        let reviewing = service.list_tasks_by_state(TaskState::Reviewing)?;
        for task in reviewing {
            let (_updated_task, computation) = service.recompute_task_review_status(
                &task.id,
                &review_config,
                &availability,
                at,
            )?;

            if computation.requirement.capacity_state
                == orch_core::state::ReviewCapacityState::NeedsHuman
            {
                let _ = service.mark_needs_human(
                    &task.id,
                    "review capacity requires human intervention",
                    crate::service::MarkNeedsHumanEventIds {
                        needs_human_state_changed: event_id(&task.id, "REVIEW-NEEDS-HUMAN-S", at),
                        needs_human_event: event_id(&task.id, "REVIEW-NEEDS-HUMAN-E", at),
                    },
                    at,
                )?;
                continue;
            }

            let ready_input = crate::lifecycle_gate::ReadyGateInput {
                verify_status: task.verify_status.clone(),
                review_evaluation: computation.evaluation.clone(),
                graphite_hygiene_ok: true,
            };

            let repo_override = repo_configs
                .get(&task.repo_id.0)
                .and_then(|cfg| cfg.graphite.submit_mode);
            let submit_policy = crate::lifecycle_gate::SubmitPolicy {
                org_default: org_config.graphite.submit_mode_default,
                auto_submit: org_config.graphite.auto_submit,
                repo_override,
            };

            let _ = service.promote_task_after_review(
                &task.id,
                ready_input,
                submit_policy,
                crate::service::PromoteTaskEventIds {
                    ready_state_changed: event_id(&task.id, "READY-S", at),
                    ready_reached: event_id(&task.id, "READY-E", at),
                    submit_state_changed: event_id(&task.id, "READY-SUBMIT-S", at),
                    submit_started: event_id(&task.id, "READY-SUBMIT-E", at),
                },
                at,
            )?;
        }

        Ok(())
    }

    fn mark_task_failed(
        &self,
        service: &OrchdService,
        task: &Task,
        code: &str,
        message: String,
        at: DateTime<Utc>,
    ) -> Result<(), RuntimeError> {
        service.record_event(&Event {
            id: event_id(&task.id, "RUNTIME-ERROR", at),
            task_id: Some(task.id.clone()),
            repo_id: Some(task.repo_id.clone()),
            at,
            kind: EventKind::Error {
                code: code.to_string(),
                message,
            },
        })?;
        let _ = service.transition_task_state(
            &task.id,
            TaskState::Failed,
            event_id(&task.id, "RUNTIME-FAILED", at),
            at,
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestackTickOutcome {
    Restacked,
    Conflict,
    Failed,
}

fn default_branch_name(task: &Task) -> String {
    format!("task/{}", task.id.0)
}

fn task_runtime_path(task: &Task, repo_config: &RepoConfig) -> PathBuf {
    let worktree = if task.worktree_path.is_absolute() {
        task.worktree_path.clone()
    } else {
        repo_config.repo_path.join(&task.worktree_path)
    };

    if worktree.exists() {
        return worktree;
    }
    repo_config.repo_path.clone()
}

fn render_verify_failure_summary(result: &orch_verify::VerifyResult) -> String {
    if result.commands.is_empty() {
        return "verification failed without command output".to_string();
    }

    let failed = result
        .commands
        .iter()
        .filter(|cmd| cmd.outcome == orch_verify::VerifyOutcome::Failed)
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return "verification failed".to_string();
    }

    failed
        .iter()
        .map(|cmd| {
            let class = cmd
                .failure_class
                .map(|x| format!("{x:?}"))
                .unwrap_or_else(|| "Unknown".to_string());
            format!(
                "{} (class={}, exit={:?})",
                cmd.command.original, class, cmd.exit_code
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn is_branch_already_present_error(err: &GraphiteError) -> bool {
    match err {
        GraphiteError::CommandFailed { stdout, stderr, .. } => {
            let combined = format!("{}\n{}", stdout, stderr).to_ascii_lowercase();
            combined.contains("already exists")
                || combined.contains("already on")
                || combined.contains("already have a branch")
        }
        _ => false,
    }
}

fn is_worktree_already_present_error(err: &GitError) -> bool {
    match err {
        GitError::CommandFailed { stdout, stderr, .. } => {
            let combined = format!("{}\n{}", stdout, stderr).to_ascii_lowercase();
            combined.contains("already exists")
                || combined.contains("already checked out")
                || combined.contains("is already used by worktree")
        }
        _ => false,
    }
}

fn event_id(task_id: &TaskId, stage: &str, at: DateTime<Utc>) -> EventId {
    let nonce = EVENT_NONCE.fetch_add(1, Ordering::Relaxed);
    EventId(format!(
        "E-{stage}-{}-{}-{nonce}",
        task_id.0,
        at.timestamp_nanos_opt().unwrap_or_default()
    ))
}

fn synthetic_draft_pr_url(task_id: &TaskId) -> String {
    format!("othala://draft/{}", task_id.0)
}

#[cfg(test)]
mod tests {
    use super::{default_branch_name, synthetic_draft_pr_url, task_runtime_path};
    use chrono::Utc;
    use orch_core::config::{
        NixConfig, RepoConfig, RepoGraphiteConfig, VerifyCommands, VerifyConfig,
    };
    use orch_core::state::{ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus};
    use orch_core::types::{RepoId, SubmitMode, Task, TaskId, TaskRole, TaskType};
    use std::fs;
    use std::path::PathBuf;

    fn repo_config(repo_path: PathBuf) -> RepoConfig {
        RepoConfig {
            repo_id: "example".to_string(),
            repo_path,
            base_branch: "main".to_string(),
            nix: NixConfig {
                dev_shell: "nix develop".to_string(),
            },
            verify: VerifyConfig {
                quick: VerifyCommands {
                    commands: vec!["nix develop -c cargo test".to_string()],
                },
                full: VerifyCommands {
                    commands: vec!["nix develop -c cargo test --all-targets".to_string()],
                },
            },
            graphite: RepoGraphiteConfig {
                draft_on_start: true,
                submit_mode: Some(SubmitMode::Single),
            },
        }
    }

    fn task_with_worktree(id: &str, worktree_path: PathBuf) -> Task {
        Task {
            id: TaskId(id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: "task".to_string(),
            state: TaskState::Initializing,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            branch_name: None,
            worktree_path,
            pr: None,
            verify_status: VerifyStatus::NotRun,
            review_status: ReviewStatus {
                required_models: Vec::new(),
                approvals_received: 0,
                approvals_required: 0,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn default_branch_name_uses_task_prefix() {
        let task = task_with_worktree("T77", PathBuf::from(".orch/wt/T77"));
        assert_eq!(default_branch_name(&task), "task/T77");
    }

    #[test]
    fn synthetic_draft_pr_url_uses_custom_scheme_and_task_id() {
        let task_id = TaskId("T88".to_string());
        assert_eq!(synthetic_draft_pr_url(&task_id), "othala://draft/T88");
    }

    #[test]
    fn task_runtime_path_prefers_existing_relative_worktree() {
        let base = std::env::temp_dir().join(format!(
            "othala-runtime-worktree-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let worktree_rel = PathBuf::from(".orch/wt/T55");
        let worktree_abs = base.join(&worktree_rel);
        fs::create_dir_all(&worktree_abs).expect("create worktree");

        let cfg = repo_config(base.clone());
        let task = task_with_worktree("T55", worktree_rel);
        assert_eq!(task_runtime_path(&task, &cfg), worktree_abs);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn task_runtime_path_falls_back_to_repo_root_when_worktree_missing() {
        let base = std::env::temp_dir().join(format!(
            "othala-runtime-missing-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&base).expect("create repo root");

        let cfg = repo_config(base.clone());
        let task = task_with_worktree("T99", PathBuf::from(".orch/wt/T99"));
        assert_eq!(task_runtime_path(&task, &cfg), base);

        let _ = fs::remove_dir_all(cfg.repo_path);
    }
}
