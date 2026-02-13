//! Retry logic — decides whether to retry a failed task and which model to use.

use orch_core::types::{ModelKind, Task};

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
}
