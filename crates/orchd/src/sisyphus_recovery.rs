//! Sisyphus Error Recovery Loop — the key differentiator.
//!
//! When a task hits STOPPED:
//! 1. Spawn Sisyphus with full context + error + history
//! 2. Sisyphus diagnoses the root cause and proposes a fix
//! 3. Auto-retry with the fix
//! 4. Escalate if 2 Sisyphus rounds fail
//!
//! This is what makes Othala *better* than Sisyphus alone:
//! Sisyphus inside the orchestration loop, with context from prior attempts.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::agent_dispatch::{AgentDispatcher, AgentRole, RepoContext};
use crate::context_manager::{ContextManager, RichContext};
use crate::problem_classifier::{ClassificationResult, ErrorClass, ProblemClassifier, RecoveryAction};
use orch_core::types::{ModelKind, Task};

// ─────────────────────────────────────────────────────────────────────────────
// Recovery State
// ─────────────────────────────────────────────────────────────────────────────

/// State of a recovery attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryState {
    pub task_id: String,
    /// Number of Sisyphus recovery attempts
    pub sisyphus_attempts: u32,
    /// Maximum Sisyphus attempts before escalation
    pub max_sisyphus_attempts: u32,
    /// Current error being recovered from
    pub current_error: Option<String>,
    /// Error classification
    pub error_class: Option<ErrorClass>,
    /// History of recovery attempts
    pub history: Vec<RecoveryAttempt>,
    /// When recovery started
    pub started_at: DateTime<Utc>,
    /// When to next retry (if waiting)
    pub next_retry_at: Option<DateTime<Utc>>,
    /// Whether recovery is complete
    pub complete: bool,
    /// Whether recovery was successful
    pub succeeded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryAttempt {
    pub attempt_number: u32,
    pub agent: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub error_class: String,
    pub outcome: String,
    pub changes: Vec<String>,
}

impl RecoveryState {
    pub fn new(task_id: &str) -> Self {
        Self {
            task_id: task_id.to_string(),
            sisyphus_attempts: 0,
            max_sisyphus_attempts: 2,
            current_error: None,
            error_class: None,
            history: Vec::new(),
            started_at: Utc::now(),
            next_retry_at: None,
            complete: false,
            succeeded: false,
        }
    }

    /// Check if we should escalate to human.
    pub fn should_escalate(&self) -> bool {
        self.sisyphus_attempts >= self.max_sisyphus_attempts
    }

    /// Check if ready to retry.
    pub fn ready_to_retry(&self, now: DateTime<Utc>) -> bool {
        match self.next_retry_at {
            Some(time) => now >= time,
            None => true,
        }
    }

    /// Record a Sisyphus attempt.
    pub fn record_sisyphus_attempt(&mut self, error_class: ErrorClass) {
        self.sisyphus_attempts += 1;
        self.history.push(RecoveryAttempt {
            attempt_number: self.sisyphus_attempts,
            agent: "sisyphus".to_string(),
            started_at: Utc::now(),
            ended_at: None,
            error_class: error_class.to_string(),
            outcome: "in_progress".to_string(),
            changes: vec![],
        });
    }

    /// Mark attempt as complete.
    pub fn complete_attempt(&mut self, success: bool, changes: Vec<String>) {
        if let Some(attempt) = self.history.last_mut() {
            attempt.ended_at = Some(Utc::now());
            attempt.outcome = if success { "success" } else { "failed" }.to_string();
            attempt.changes = changes;
        }
        if success {
            self.complete = true;
            self.succeeded = true;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Recovery Decision
// ─────────────────────────────────────────────────────────────────────────────

/// Decision from the recovery loop.
#[derive(Debug, Clone)]
pub enum RecoveryDecision {
    /// Retry with Sisyphus
    RetryWithSisyphus {
        context: RichContext,
        prompt_additions: Vec<String>,
    },
    /// Retry with a different agent
    RetryWithAgent {
        role: AgentRole,
        context: RichContext,
    },
    /// Wait before retrying (transient error)
    WaitAndRetry {
        wait_secs: u64,
        reason: String,
    },
    /// Escalate to human
    EscalateHuman {
        reason: String,
        summary: String,
    },
    /// Stop task (unrecoverable)
    Stop {
        reason: String,
    },
    /// Recovery succeeded
    Success,
}

// ─────────────────────────────────────────────────────────────────────────────
// Sisyphus Recovery Loop
// ─────────────────────────────────────────────────────────────────────────────

/// The Sisyphus Recovery Loop — coordinates error recovery.
pub struct SisyphusRecoveryLoop {
    /// Problem classifier
    pub classifier: ProblemClassifier,
    /// Agent dispatcher
    pub dispatcher: AgentDispatcher,
    /// Context manager
    pub context_manager: ContextManager,
    /// Active recovery states
    pub recoveries: HashMap<String, RecoveryState>,
}

impl SisyphusRecoveryLoop {
    pub fn new(
        classifier: ProblemClassifier,
        dispatcher: AgentDispatcher,
        context_manager: ContextManager,
    ) -> Self {
        Self {
            classifier,
            dispatcher,
            context_manager,
            recoveries: HashMap::new(),
        }
    }

    /// **GRACEFUL FALLBACK:** Evaluate recovery with automatic escalation on failure.
    /// Never panics; always returns a safe decision (escalate to human if recovery loop breaks).
    pub fn evaluate_with_fallback(
        &mut self,
        task: &Task,
        tasks: &[Task],
        failure_reason: &str,
        repo_context: &RepoContext,
    ) -> RecoveryDecision {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.evaluate(task, tasks, failure_reason, repo_context)
        })) {
            Ok(decision) => decision,
            Err(_) => {
                eprintln!(
                    "[sisyphus] WARNING: Recovery loop panicked for task {}. Escalating to human.",
                    task.id
                );
                // Mark as complete and escalate
                if !self.recoveries.contains_key(&task.id.0) {
                    self.recoveries
                        .insert(task.id.0.clone(), RecoveryState::new(&task.id.0));
                }
                if let Some(recovery) = self.recoveries.get_mut(&task.id.0) {
                    recovery.complete = true;
                }
                RecoveryDecision::EscalateHuman {
                    reason: "Recovery loop encountered internal error; escalating for manual triage".to_string(),
                    summary: format!(
                        "Task {} hit a recovery system error. Please investigate manually.",
                        task.id
                    ),
                }
            }
        }
    }

    /// Evaluate a stopped task and decide on recovery action.
    pub fn evaluate(
        &mut self,
        task: &Task,
        tasks: &[Task],
        failure_reason: &str,
        _repo_context: &RepoContext,
    ) -> RecoveryDecision {
        // Classify the error first (before any mutable borrows)
        let classification = self.classifier.classify(failure_reason);

        // Record in context manager
        {
            let ctx_state = self.context_manager.get_state(&task.repo_id.0);
            ctx_state.record_error(&classification.class.to_string());
        }

        // Check for repeated pattern (before recovery state borrow)
        let has_repeated_pattern = self.classifier.detect_repeated_pattern(3).is_some();

        // Build rich context (before recovery state borrow)
        let rich_context = self
            .context_manager
            .build_context(task, tasks, Some(failure_reason));

        // Get or create recovery state
        if !self.recoveries.contains_key(&task.id.0) {
            self.recoveries
                .insert(task.id.0.clone(), RecoveryState::new(&task.id.0));
        }
        let recovery = self.recoveries.get_mut(&task.id.0).unwrap();

        recovery.current_error = Some(failure_reason.to_string());
        recovery.error_class = Some(classification.class);

        // Handle based on action
        match classification.action {
            RecoveryAction::EscalateHuman => {
                recovery.complete = true;
                let summary = build_escalation_summary_standalone(recovery, &classification);
                return RecoveryDecision::EscalateHuman {
                    reason: format!("Error requires human intervention: {}", classification.class),
                    summary,
                };
            }

            RecoveryAction::WaitAndRetry => {
                let wait_secs = classification.class.retry_delay_secs().unwrap_or(60);
                recovery.next_retry_at = Some(Utc::now() + Duration::seconds(wait_secs as i64));
                return RecoveryDecision::WaitAndRetry {
                    wait_secs,
                    reason: format!("Transient error ({}), waiting {}s", classification.class, wait_secs),
                };
            }

            RecoveryAction::Stop => {
                recovery.complete = true;
                return RecoveryDecision::Stop {
                    reason: format!("Unrecoverable error: {}", failure_reason),
                };
            }

            RecoveryAction::RetryWithAgent | RecoveryAction::Retry => {
                // Continue to Sisyphus evaluation
            }
        }

        // Check if we should escalate after multiple Sisyphus attempts
        if recovery.should_escalate() {
            recovery.complete = true;
            let summary = build_escalation_summary_standalone(recovery, &classification);
            return RecoveryDecision::EscalateHuman {
                reason: format!(
                    "Sisyphus recovery exhausted after {} attempts",
                    recovery.sisyphus_attempts
                ),
                summary,
            };
        }

        // Check for repeated pattern (same error 3+ times)
        if has_repeated_pattern {
            recovery.complete = true;
            let summary = build_escalation_summary_standalone(recovery, &classification);
            return RecoveryDecision::EscalateHuman {
                reason: "Repeated error pattern detected".to_string(),
                summary,
            };
        }

        // Record the Sisyphus attempt
        recovery.record_sisyphus_attempt(classification.class);

        // Build Sisyphus prompt additions
        let prompt_additions = build_sisyphus_prompt_standalone(&classification, recovery);

        RecoveryDecision::RetryWithSisyphus {
            context: rich_context,
            prompt_additions,
        }
    }

    /// Build prompt additions for Sisyphus recovery (deprecated, use standalone).
    #[allow(dead_code)]
    fn build_sisyphus_prompt(
        &self,
        classification: &ClassificationResult,
        recovery: &RecoveryState,
    ) -> Vec<String> {
        build_sisyphus_prompt_standalone(classification, recovery)
    }
}

/// Standalone function to build Sisyphus prompt (avoids borrow issues).
fn build_sisyphus_prompt_standalone(
    classification: &ClassificationResult,
    recovery: &RecoveryState,
) -> Vec<String> {
    let mut additions = Vec::new();

    // Recovery header
    additions.push(format!(
        "## Recovery Attempt {} of {}",
        recovery.sisyphus_attempts, recovery.max_sisyphus_attempts
    ));

    // Error analysis from classifier
    additions.push(classification.context.clone());

    // History of prior attempts
    if !recovery.history.is_empty() {
        additions.push("### Prior Recovery Attempts\n".to_string());
        for attempt in &recovery.history {
            additions.push(format!(
                "- Attempt {} ({}): {} → {}\n",
                attempt.attempt_number,
                attempt.agent,
                attempt.error_class,
                attempt.outcome
            ));
        }
    }

    // Sisyphus-specific instructions
    additions.push(format!(
        r#"
### Recovery Instructions

You are Sisyphus, tasked with recovering from the {} error class.

1. **Analyze** the error carefully — don't just retry the same approach
2. **Identify** the root cause, not just the symptom
3. **Propose** a fix that addresses the root cause
4. **Implement** the fix
5. **Verify** by running the verify command

If you cannot fix the issue after analysis, signal [need_human] with a clear explanation of what's blocking you.
"#,
        classification.class
    ));

    additions
}

/// Standalone function to build escalation summary (avoids borrow issues).
fn build_escalation_summary_standalone(
    recovery: &RecoveryState,
    classification: &ClassificationResult,
) -> String {
    let mut summary = String::new();

    summary.push_str(&format!("## Task {} Recovery Summary\n\n", recovery.task_id));
    summary.push_str(&format!(
        "**Error Class:** {}\n",
        classification.class
    ));
    summary.push_str(&format!(
        "**Recovery Duration:** {:.1} hours\n",
        (Utc::now() - recovery.started_at).num_minutes() as f64 / 60.0
    ));
    summary.push_str(&format!(
        "**Sisyphus Attempts:** {}\n\n",
        recovery.sisyphus_attempts
    ));

    if let Some(error) = &recovery.current_error {
        summary.push_str("### Latest Error\n```\n");
        summary.push_str(&error.chars().take(500).collect::<String>());
        summary.push_str("\n```\n\n");
    }

    if !recovery.history.is_empty() {
        summary.push_str("### Attempt History\n");
        for attempt in &recovery.history {
            summary.push_str(&format!(
                "- **Attempt {}** ({}): {} → {}\n",
                attempt.attempt_number,
                attempt.agent,
                attempt.error_class,
                attempt.outcome
            ));
        }
    }

    summary.push_str("\n### Recommended Actions\n");
    match classification.class {
        ErrorClass::Permission => {
            summary.push_str("- Check credentials and authentication\n");
            summary.push_str("- Verify API tokens haven't expired\n");
            summary.push_str("- Run `gt auth login` if Graphite related\n");
        }
        ErrorClass::Compile => {
            summary.push_str("- Review the compilation errors manually\n");
            summary.push_str("- Check for missing dependencies\n");
            summary.push_str("- Verify Rust/Cargo version compatibility\n");
        }
        ErrorClass::Logic => {
            summary.push_str("- Review failing tests for logical errors\n");
            summary.push_str("- Check assumptions in the implementation\n");
            summary.push_str("- Consider if requirements changed\n");
        }
        _ => {
            summary.push_str("- Review the error logs carefully\n");
            summary.push_str("- Check environment configuration\n");
        }
    }

    summary
}

impl SisyphusRecoveryLoop {
    /// Mark a recovery as successful.
    pub fn mark_success(&mut self, task_id: &str, changes: Vec<String>) {
        if let Some(recovery) = self.recoveries.get_mut(task_id) {
            recovery.complete_attempt(true, changes);
        }
    }

    /// Mark a recovery attempt as failed.
    pub fn mark_failure(&mut self, task_id: &str) {
        if let Some(recovery) = self.recoveries.get_mut(task_id) {
            recovery.complete_attempt(false, vec![]);
        }
    }

    /// Remove completed recovery state.
    pub fn cleanup(&mut self, task_id: &str) {
        self.recoveries.remove(task_id);
    }

    /// Get recovery state for a task.
    pub fn get_state(&self, task_id: &str) -> Option<&RecoveryState> {
        self.recoveries.get(task_id)
    }

    /// Get all active recoveries.
    pub fn active_recoveries(&self) -> Vec<&RecoveryState> {
        self.recoveries
            .values()
            .filter(|r| !r.complete)
            .collect()
    }
}

impl Default for SisyphusRecoveryLoop {
    fn default() -> Self {
        Self::new(
            ProblemClassifier::default(),
            AgentDispatcher::default(),
            ContextManager::default(),
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Integration with Task State
// ─────────────────────────────────────────────────────────────────────────────

/// Convert recovery decision to daemon action.
pub fn recovery_to_model(decision: &RecoveryDecision) -> Option<ModelKind> {
    match decision {
        RecoveryDecision::RetryWithSisyphus { .. } => Some(ModelKind::Claude), // Opus
        RecoveryDecision::RetryWithAgent { role, .. } => Some(role.model()),
        _ => None,
    }
}

/// Check if a failure reason is recoverable.
pub fn is_recoverable_failure(reason: &str) -> bool {
    let mut classifier = ProblemClassifier::new();
    let classification = classifier.classify(reason);
    classification.class.is_agent_fixable()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use orch_core::types::{RepoId, TaskId};

    fn make_task(id: &str) -> Task {
        Task::new(
            TaskId::new(id),
            RepoId("test-repo".to_string()),
            "Test task".to_string(),
            PathBuf::from(format!(".orch/wt/{}", id)),
        )
    }

    #[test]
    fn recovery_state_tracks_attempts() {
        let mut state = RecoveryState::new("T1");

        assert_eq!(state.sisyphus_attempts, 0);
        assert!(!state.should_escalate());

        state.record_sisyphus_attempt(ErrorClass::Compile);
        assert_eq!(state.sisyphus_attempts, 1);
        assert!(!state.should_escalate());

        state.record_sisyphus_attempt(ErrorClass::Compile);
        assert_eq!(state.sisyphus_attempts, 2);
        assert!(state.should_escalate());
    }

    #[test]
    fn recovery_loop_escalates_permission_errors() {
        let mut loop_ = SisyphusRecoveryLoop::default();
        let task = make_task("T1");
        let repo_ctx = RepoContext::default();

        let decision = loop_.evaluate(
            &task,
            &[],
            "authentication failed: token expired",
            &repo_ctx,
        );

        assert!(matches!(decision, RecoveryDecision::EscalateHuman { .. }));
    }

    #[test]
    fn recovery_loop_waits_for_transient_errors() {
        let mut loop_ = SisyphusRecoveryLoop::default();
        let task = make_task("T1");
        let repo_ctx = RepoContext::default();

        let decision = loop_.evaluate(
            &task,
            &[],
            "connection timeout after 30s",
            &repo_ctx,
        );

        assert!(matches!(decision, RecoveryDecision::WaitAndRetry { .. }));
    }

    #[test]
    fn recovery_loop_uses_sisyphus_for_compile_errors() {
        let mut loop_ = SisyphusRecoveryLoop::default();
        let task = make_task("T1");
        let repo_ctx = RepoContext::default();

        let decision = loop_.evaluate(
            &task,
            &[],
            "error[E0308]: mismatched types",
            &repo_ctx,
        );

        assert!(matches!(decision, RecoveryDecision::RetryWithSisyphus { .. }));
    }

    #[test]
    fn recovery_loop_escalates_after_max_attempts() {
        let mut loop_ = SisyphusRecoveryLoop::default();
        let task = make_task("T1");
        let repo_ctx = RepoContext::default();

        // First attempt
        let _ = loop_.evaluate(&task, &[], "compile error", &repo_ctx);
        loop_.mark_failure(&task.id.0);

        // Second attempt
        let _ = loop_.evaluate(&task, &[], "compile error", &repo_ctx);
        loop_.mark_failure(&task.id.0);

        // Third attempt should escalate
        let decision = loop_.evaluate(&task, &[], "compile error", &repo_ctx);
        assert!(matches!(decision, RecoveryDecision::EscalateHuman { .. }));
    }

    #[test]
    fn is_recoverable_checks_error_class() {
        assert!(is_recoverable_failure("error[E0308]: mismatched types"));
        assert!(is_recoverable_failure("test failed: assertion error"));
        assert!(!is_recoverable_failure("authentication failed: token expired"));
    }
}
