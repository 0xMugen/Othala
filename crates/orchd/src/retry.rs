//! Retry logic — decides whether to retry a failed task and which model to use.

use chrono::{DateTime, Duration, Utc};
use orch_core::types::{ModelKind, Task};
use std::collections::HashMap;

use crate::supervisor::AgentOutcome;

/// Decision returned by the retry evaluator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryDecision {
    /// Whether to retry this task.
    pub should_retry: bool,
    /// The model to use for the next attempt (if retrying).
    pub next_model: Option<ModelKind>,
    /// Human-readable explanation.
    pub reason: String,
}

const DEFAULT_FAILURE_THRESHOLD: u32 = 3;
const DEFAULT_COOLDOWN_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Cooldown,
}

#[derive(Debug, Clone)]
pub struct ModelHealthTracker {
    state: HashMap<ModelKind, ModelHealth>,
    failure_threshold: u32,
    cooldown_duration: Duration,
}

#[derive(Debug, Clone)]
struct ModelHealth {
    consecutive_failures: u32,
    last_failure: Option<DateTime<Utc>>,
    cooldown_until: Option<DateTime<Utc>>,
}

impl ModelHealth {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            last_failure: None,
            cooldown_until: None,
        }
    }
}

impl Default for ModelHealthTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelHealthTracker {
    pub fn new() -> Self {
        Self {
            state: HashMap::new(),
            failure_threshold: DEFAULT_FAILURE_THRESHOLD,
            cooldown_duration: Duration::seconds(DEFAULT_COOLDOWN_SECS),
        }
    }

    pub fn with_config(failure_threshold: u32, cooldown_secs: i64) -> Self {
        Self {
            state: HashMap::new(),
            failure_threshold: failure_threshold.max(1),
            cooldown_duration: Duration::seconds(cooldown_secs.max(1)),
        }
    }

    pub fn record_success(&mut self, model: ModelKind) {
        let health = self.state.entry(model).or_insert_with(ModelHealth::new);
        health.consecutive_failures = 0;
        health.cooldown_until = None;
    }

    pub fn record_failure(&mut self, model: ModelKind) {
        self.record_failure_at(model, Utc::now());
    }

    pub fn record_failure_at(&mut self, model: ModelKind, now: DateTime<Utc>) {
        let health = self.state.entry(model).or_insert_with(ModelHealth::new);
        health.consecutive_failures = health.consecutive_failures.saturating_add(1);
        health.last_failure = Some(now);
        if health.consecutive_failures >= self.failure_threshold {
            health.cooldown_until = Some(now + self.cooldown_duration);
        }
    }

    pub fn is_available(&self, model: ModelKind, now: DateTime<Utc>) -> bool {
        self.state
            .get(&model)
            .and_then(|h| h.cooldown_until)
            .is_none_or(|until| now >= until)
    }

    pub fn health_state(&self, model: ModelKind, now: DateTime<Utc>) -> HealthState {
        let Some(health) = self.state.get(&model) else {
            return HealthState::Healthy;
        };

        if health.cooldown_until.is_some_and(|until| now < until) {
            return HealthState::Cooldown;
        }
        if health.consecutive_failures > 0 {
            HealthState::Degraded
        } else {
            HealthState::Healthy
        }
    }

    fn last_failure(&self, model: ModelKind) -> Option<DateTime<Utc>> {
        self.state.get(&model).and_then(|h| h.last_failure)
    }
}

pub fn pick_next_model_with_health(
    task: &Task,
    just_failed: ModelKind,
    enabled_models: &[ModelKind],
    tracker: &ModelHealthTracker,
    now: DateTime<Utc>,
) -> Option<ModelKind> {
    let mut ordered_candidates = Vec::new();

    if let Some(preferred) = task.preferred_model {
        if preferred != just_failed
            && !task.failed_models.contains(&preferred)
            && enabled_models.contains(&preferred)
        {
            ordered_candidates.push(preferred);
        }
    }

    for model in enabled_models {
        if *model == just_failed || task.failed_models.contains(model) {
            continue;
        }
        if !ordered_candidates.contains(model) {
            ordered_candidates.push(*model);
        }
    }

    if ordered_candidates.is_empty() {
        return None;
    }

    let mut healthy = Vec::new();
    let mut degraded = Vec::new();
    let mut cooldown = Vec::new();

    for model in ordered_candidates {
        match tracker.health_state(model, now) {
            HealthState::Healthy => healthy.push(model),
            HealthState::Degraded => degraded.push(model),
            HealthState::Cooldown => cooldown.push(model),
        }
    }

    if let Some(model) = healthy.first() {
        return Some(*model);
    }
    if let Some(model) = degraded.first() {
        return Some(*model);
    }

    cooldown.into_iter().min_by_key(|model| tracker.last_failure(*model))
}

/// Evaluate whether a task should be retried after a failed agent run.
///
/// Returns a `RetryDecision` indicating whether to retry and which model to
/// use next.
pub fn evaluate_retry(
    task: &Task,
    outcome: &AgentOutcome,
    enabled_models: &[ModelKind],
) -> RetryDecision {
    // Already succeeded — no retry needed.
    if outcome.success || outcome.patch_ready {
        return RetryDecision {
            should_retry: false,
            next_model: None,
            reason: "task succeeded".to_string(),
        };
    }

    // Agent explicitly asked for human help — don't auto-retry.
    if outcome.needs_human {
        return RetryDecision {
            should_retry: false,
            next_model: None,
            reason: "agent requested human help".to_string(),
        };
    }

    // Max retries exhausted.
    if task.retry_count >= task.max_retries {
        return RetryDecision {
            should_retry: false,
            next_model: None,
            reason: format!(
                "max retries ({}) exhausted",
                task.max_retries
            ),
        };
    }

    // Pick the next model: try the current preferred model first, then fall
    // back to any enabled model that hasn't failed on this task yet.
    // Pass the just-failed model so it's excluded even before being recorded
    // in task.failed_models.
    let next_model = pick_next_model(task, outcome.model, enabled_models);

    match next_model {
        Some(model) => RetryDecision {
            should_retry: true,
            next_model: Some(model),
            reason: format!(
                "retrying (attempt {}/{}) with {}",
                task.retry_count + 1,
                task.max_retries,
                model.as_str()
            ),
        },
        None => RetryDecision {
            should_retry: false,
            next_model: None,
            reason: "no available models left (all have failed)".to_string(),
        },
    }
}

/// Choose the next model for a retry attempt.
///
/// Strategy:
/// 1. If the task's preferred model hasn't failed yet AND isn't the one that
///    just failed, use it.
/// 2. Otherwise, pick the first enabled model that isn't in `failed_models`
///    and isn't the just-failed model.
///
/// `just_failed` is needed because at evaluation time the failure hasn't been
/// recorded into `task.failed_models` yet, so without it the function would
/// re-select the model that just crashed.
fn pick_next_model(
    task: &Task,
    just_failed: ModelKind,
    enabled_models: &[ModelKind],
) -> Option<ModelKind> {
    // If preferred model hasn't been exhausted and isn't the one that just failed, keep using it.
    if let Some(preferred) = task.preferred_model {
        if preferred != just_failed
            && !task.failed_models.contains(&preferred)
            && enabled_models.contains(&preferred)
        {
            return Some(preferred);
        }
    }

    // Fall back to first enabled model not yet failed (and not just-failed).
    enabled_models
        .iter()
        .find(|m| **m != just_failed && !task.failed_models.contains(m))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::types::{RepoId, TaskId};
    use std::path::PathBuf;

    fn mk_task() -> Task {
        Task::new(
            TaskId::new("T1"),
            RepoId("repo".to_string()),
            "Test task".to_string(),
            PathBuf::from(".orch/wt/T1"),
        )
    }

    fn mk_outcome(success: bool) -> AgentOutcome {
        AgentOutcome {
            task_id: TaskId::new("T1"),
            model: ModelKind::Claude,
            exit_code: if success { Some(0) } else { Some(1) },
            patch_ready: success,
            needs_human: false,
            success,
            duration_secs: 1,
        }
    }

    fn all_models() -> Vec<ModelKind> {
        vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini]
    }

    #[test]
    fn no_retry_on_success() {
        let task = mk_task();
        let outcome = mk_outcome(true);
        let decision = evaluate_retry(&task, &outcome, &all_models());

        assert!(!decision.should_retry);
    }

    #[test]
    fn no_retry_on_needs_human() {
        let task = mk_task();
        let mut outcome = mk_outcome(false);
        outcome.needs_human = true;
        let decision = evaluate_retry(&task, &outcome, &all_models());

        assert!(!decision.should_retry);
        assert!(decision.reason.contains("human"));
    }

    #[test]
    fn retries_with_preferred_model_when_different_model_failed() {
        let mut task = mk_task();
        task.preferred_model = Some(ModelKind::Claude);

        // Codex failed — preferred Claude should still be selected.
        let mut outcome = mk_outcome(false);
        outcome.model = ModelKind::Codex;
        let decision = evaluate_retry(&task, &outcome, &all_models());

        assert!(decision.should_retry);
        assert_eq!(decision.next_model, Some(ModelKind::Claude));
    }

    #[test]
    fn switches_model_when_preferred_just_failed() {
        let mut task = mk_task();
        task.preferred_model = Some(ModelKind::Claude);

        // Claude just failed — should switch to Codex.
        let outcome = mk_outcome(false); // model: Claude
        let decision = evaluate_retry(&task, &outcome, &all_models());

        assert!(decision.should_retry);
        assert_eq!(decision.next_model, Some(ModelKind::Codex));
    }

    #[test]
    fn falls_back_when_preferred_model_failed() {
        let mut task = mk_task();
        task.preferred_model = Some(ModelKind::Claude);
        task.failed_models.push(ModelKind::Claude);

        let outcome = mk_outcome(false);
        let decision = evaluate_retry(&task, &outcome, &all_models());

        assert!(decision.should_retry);
        assert_eq!(decision.next_model, Some(ModelKind::Codex));
    }

    #[test]
    fn no_retry_when_all_models_failed() {
        let mut task = mk_task();
        task.failed_models = vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini];

        let outcome = mk_outcome(false);
        let decision = evaluate_retry(&task, &outcome, &all_models());

        assert!(!decision.should_retry);
        assert!(decision.reason.contains("no available models"));
    }

    #[test]
    fn no_retry_when_max_retries_exhausted() {
        let mut task = mk_task();
        task.retry_count = 3;
        task.max_retries = 3;

        let outcome = mk_outcome(false);
        let decision = evaluate_retry(&task, &outcome, &all_models());

        assert!(!decision.should_retry);
        assert!(decision.reason.contains("exhausted"));
    }

    #[test]
    fn picks_first_available_when_no_preferred() {
        let task = mk_task(); // preferred_model = None

        // Claude just failed → first available that isn't Claude = Codex.
        let outcome = mk_outcome(false);
        let decision = evaluate_retry(&task, &outcome, &all_models());

        assert!(decision.should_retry);
        assert_eq!(decision.next_model, Some(ModelKind::Codex));
    }

    #[test]
    fn skips_disabled_models() {
        let mut task = mk_task();
        task.preferred_model = Some(ModelKind::Gemini);

        // Gemini just failed, only Claude and Codex enabled.
        let mut outcome = mk_outcome(false);
        outcome.model = ModelKind::Gemini;
        let decision = evaluate_retry(&task, &outcome, &[ModelKind::Claude, ModelKind::Codex]);

        assert!(decision.should_retry);
        assert_eq!(decision.next_model, Some(ModelKind::Claude));
    }

    #[test]
    fn no_retry_when_just_failed_is_only_enabled_model() {
        let mut task = mk_task();
        task.preferred_model = Some(ModelKind::Claude);

        // Claude just failed, and it's the only enabled model.
        let outcome = mk_outcome(false); // model: Claude
        let decision = evaluate_retry(&task, &outcome, &[ModelKind::Claude]);

        assert!(!decision.should_retry);
        assert!(decision.reason.contains("no available models"));
    }

    #[test]
    fn cooldown_activates_after_threshold_failures() {
        let mut tracker = ModelHealthTracker::with_config(3, 60);
        let now = Utc::now();

        tracker.record_failure_at(ModelKind::Claude, now);
        tracker.record_failure_at(ModelKind::Claude, now + Duration::seconds(1));
        assert!(tracker.is_available(ModelKind::Claude, now + Duration::seconds(2)));
        assert_eq!(
            tracker.health_state(ModelKind::Claude, now + Duration::seconds(2)),
            HealthState::Degraded
        );

        tracker.record_failure_at(ModelKind::Claude, now + Duration::seconds(3));
        assert!(!tracker.is_available(ModelKind::Claude, now + Duration::seconds(4)));
        assert_eq!(
            tracker.health_state(ModelKind::Claude, now + Duration::seconds(4)),
            HealthState::Cooldown
        );
    }

    #[test]
    fn cooldown_expires_and_model_becomes_available() {
        let mut tracker = ModelHealthTracker::with_config(3, 60);
        let now = Utc::now();

        tracker.record_failure_at(ModelKind::Codex, now);
        tracker.record_failure_at(ModelKind::Codex, now + Duration::seconds(1));
        tracker.record_failure_at(ModelKind::Codex, now + Duration::seconds(2));

        assert!(!tracker.is_available(ModelKind::Codex, now + Duration::seconds(30)));
        assert!(tracker.is_available(ModelKind::Codex, now + Duration::seconds(63)));
        assert_eq!(
            tracker.health_state(ModelKind::Codex, now + Duration::seconds(63)),
            HealthState::Degraded
        );
    }

    #[test]
    fn healthy_models_are_preferred_over_degraded_ones() {
        let mut task = mk_task();
        task.preferred_model = Some(ModelKind::Claude);
        let enabled = vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini];
        let now = Utc::now();

        let mut tracker = ModelHealthTracker::with_config(3, 60);
        tracker.record_failure_at(ModelKind::Codex, now);

        let next = pick_next_model_with_health(
            &task,
            ModelKind::Gemini,
            &enabled,
            &tracker,
            now + Duration::seconds(1),
        );

        assert_eq!(next, Some(ModelKind::Claude));
    }

    #[test]
    fn all_models_in_cooldown_picks_least_recently_failed() {
        let mut task = mk_task();
        task.preferred_model = Some(ModelKind::Claude);
        let enabled = vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini];
        let now = Utc::now();

        let mut tracker = ModelHealthTracker::with_config(1, 60);
        tracker.record_failure_at(ModelKind::Claude, now + Duration::seconds(30));
        tracker.record_failure_at(ModelKind::Codex, now + Duration::seconds(10));
        tracker.record_failure_at(ModelKind::Gemini, now + Duration::seconds(20));

        let next = pick_next_model_with_health(
            &task,
            ModelKind::Claude,
            &enabled,
            &tracker,
            now + Duration::seconds(35),
        );

        assert_eq!(next, Some(ModelKind::Codex));
    }
}
