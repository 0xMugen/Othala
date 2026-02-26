//! QA self-heal pipeline — failure classification, retry policy, and auto-fix spawning.
//!
//! When QA validation fails, this module:
//! 1. Classifies failures into actionable categories
//! 2. Applies a configurable retry policy (with backoff)
//! 3. Generates targeted fix prompts for auto-spawned fix tasks
//! 4. Tracks red→green transitions for merge-path unblocking

use crate::qa_agent::{QAResult, QATestResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Failure Classification
// ---------------------------------------------------------------------------

/// Classification of a QA test failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QAFailureClass {
    /// A regression in existing functionality — the test passed before.
    Regression,
    /// A flaky test — intermittent, non-deterministic failure.
    Flaky,
    /// An environment or infrastructure issue (not code-related).
    EnvironmentIssue,
    /// A new test that was expected to fail (acceptance test for new feature).
    AcceptanceGap,
    /// Unclassified failure.
    Unknown,
}

impl QAFailureClass {
    /// Whether this class warrants an automatic fix task.
    pub fn is_auto_fixable(&self) -> bool {
        matches!(self, QAFailureClass::Regression | QAFailureClass::AcceptanceGap)
    }

    /// Whether this class warrants a retry (may self-resolve).
    pub fn is_retryable(&self) -> bool {
        matches!(self, QAFailureClass::Flaky | QAFailureClass::EnvironmentIssue)
    }
}

/// Classify a QA test failure based on the test result detail and history.
pub fn classify_failure(
    test: &QATestResult,
    baseline_passed: bool,
) -> QAFailureClass {
    let detail_lower = test.detail.to_ascii_lowercase();

    // Environment indicators
    if detail_lower.contains("connection refused")
        || detail_lower.contains("port already in use")
        || detail_lower.contains("no such file or directory")
        || detail_lower.contains("permission denied")
        || detail_lower.contains("timed out")
        || detail_lower.contains("econnreset")
        || detail_lower.contains("eaddrinuse")
    {
        return QAFailureClass::EnvironmentIssue;
    }

    // Flaky indicators
    if detail_lower.contains("intermittent")
        || detail_lower.contains("flaky")
        || detail_lower.contains("race condition")
        || detail_lower.contains("non-deterministic")
    {
        return QAFailureClass::Flaky;
    }

    // If this test passed in baseline, it's a regression
    if baseline_passed {
        return QAFailureClass::Regression;
    }

    // New test that never passed — acceptance gap
    QAFailureClass::AcceptanceGap
}

/// Classify all failures in a QA result against a baseline.
pub fn classify_all_failures(
    result: &QAResult,
    baseline: Option<&QAResult>,
) -> Vec<(String, QAFailureClass)> {
    let baseline_map: HashMap<String, bool> = baseline
        .map(|b| {
            b.tests
                .iter()
                .map(|t| (format!("{}.{}", t.suite, t.name), t.passed))
                .collect()
        })
        .unwrap_or_default();

    result
        .tests
        .iter()
        .filter(|t| !t.passed)
        .map(|t| {
            let key = format!("{}.{}", t.suite, t.name);
            let baseline_passed = baseline_map.get(&key).copied().unwrap_or(false);
            let class = classify_failure(t, baseline_passed);
            (key, class)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Retry Policy
// ---------------------------------------------------------------------------

/// Configuration for QA retry policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QARetryConfig {
    /// Maximum number of retry attempts for QA validation.
    pub max_retries: u32,
    /// Base delay between retries in seconds.
    pub base_delay_secs: u64,
    /// Backoff multiplier.
    pub backoff_multiplier: f64,
    /// Maximum delay in seconds.
    pub max_delay_secs: u64,
}

impl Default for QARetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_secs: 30,
            backoff_multiplier: 2.0,
            max_delay_secs: 300,
        }
    }
}

/// Tracks retry state for a specific task's QA validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QARetryState {
    pub task_id: String,
    pub attempt: u32,
    pub max_attempts: u32,
    pub last_attempt_at: DateTime<Utc>,
    pub next_retry_at: DateTime<Utc>,
    pub failures_by_class: HashMap<QAFailureClass, u32>,
    pub should_auto_fix: bool,
    pub should_retry: bool,
}

impl QARetryState {
    /// Create a new retry state from a QA result.
    pub fn from_result(
        task_id: &str,
        result: &QAResult,
        baseline: Option<&QAResult>,
        config: &QARetryConfig,
        now: DateTime<Utc>,
    ) -> Self {
        let classifications = classify_all_failures(result, baseline);
        let mut failures_by_class: HashMap<QAFailureClass, u32> = HashMap::new();
        for (_, class) in &classifications {
            *failures_by_class.entry(*class).or_insert(0) += 1;
        }

        let should_retry = classifications.iter().any(|(_, c)| c.is_retryable());
        let should_auto_fix = classifications.iter().any(|(_, c)| c.is_auto_fixable());
        let delay = chrono::Duration::seconds(config.base_delay_secs as i64);

        Self {
            task_id: task_id.to_string(),
            attempt: 1,
            max_attempts: config.max_retries,
            last_attempt_at: now,
            next_retry_at: now + delay,
            failures_by_class,
            should_auto_fix,
            should_retry,
        }
    }

    /// Record a retry attempt, returns false if exhausted.
    pub fn record_retry(&mut self, config: &QARetryConfig, now: DateTime<Utc>) -> bool {
        self.attempt += 1;
        self.last_attempt_at = now;

        if self.attempt > self.max_attempts {
            return false;
        }

        let delay_secs = (config.base_delay_secs as f64
            * config.backoff_multiplier.powi(self.attempt as i32 - 1))
            as i64;
        let capped = delay_secs.min(config.max_delay_secs as i64);
        self.next_retry_at = now + chrono::Duration::seconds(capped);
        true
    }

    /// Whether a retry is ready to execute.
    pub fn is_ready(&self, now: DateTime<Utc>) -> bool {
        self.should_retry && self.attempt <= self.max_attempts && now >= self.next_retry_at
    }

    /// Whether retries are exhausted and we should fall through to auto-fix.
    pub fn is_exhausted(&self) -> bool {
        self.attempt > self.max_attempts
    }
}

// ---------------------------------------------------------------------------
// Fix Task Prompt Generation
// ---------------------------------------------------------------------------

/// Generate a targeted fix prompt from QA failures.
pub fn build_fix_prompt(
    result: &QAResult,
    baseline: Option<&QAResult>,
) -> String {
    let classifications = classify_all_failures(result, baseline);
    let mut prompt = String::from(
        "## QA Self-Heal: Fix Failing Tests\n\n\
         The following QA tests failed after the latest change. \
         Fix the issues and ensure all tests pass.\n\n",
    );

    // Group by classification
    let mut regressions = Vec::new();
    let mut acceptance_gaps = Vec::new();

    for (test_name, class) in &classifications {
        match class {
            QAFailureClass::Regression => regressions.push(test_name.as_str()),
            QAFailureClass::AcceptanceGap => acceptance_gaps.push(test_name.as_str()),
            _ => {}
        }
    }

    if !regressions.is_empty() {
        prompt.push_str("### Regressions (tests that previously passed)\n");
        for name in &regressions {
            prompt.push_str(&format!("- {name}\n"));
        }
        prompt.push_str("\nThese MUST be fixed — they represent broken existing functionality.\n\n");
    }

    if !acceptance_gaps.is_empty() {
        prompt.push_str("### Acceptance Gaps (new tests that don't pass yet)\n");
        for name in &acceptance_gaps {
            prompt.push_str(&format!("- {name}\n"));
        }
        prompt.push_str("\nThese represent the expected behavior of the new feature.\n\n");
    }

    // Append failure details
    prompt.push_str("### Failure Details\n\n");
    for test in &result.tests {
        if !test.passed {
            prompt.push_str(&format!(
                "- **{}.{}**: {}\n",
                test.suite, test.name, test.detail
            ));
        }
    }

    prompt.push_str("\nFix the failing tests. Signal [patch_ready] when done.\n");
    prompt
}

// ---------------------------------------------------------------------------
// Merge Path Unblock Detection
// ---------------------------------------------------------------------------

/// Represents a QA state transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QATransition {
    /// QA went from failing to passing — merge path is unblocked.
    RedToGreen {
        task_id: String,
        previous_failures: u32,
        current_passed: u32,
    },
    /// QA went from passing to failing — regression detected.
    GreenToRed {
        task_id: String,
        new_failures: u32,
    },
    /// QA stayed green — no change.
    StillGreen { task_id: String },
    /// QA stayed red — still failing.
    StillRed {
        task_id: String,
        failures: u32,
    },
}

/// Detect QA state transition between two consecutive results.
pub fn detect_transition(
    task_id: &str,
    previous: Option<&QAResult>,
    current: &QAResult,
) -> QATransition {
    let current_failed = current.summary.failed;
    let current_passed = current.summary.passed;

    match previous {
        None => {
            if current_failed == 0 {
                QATransition::StillGreen {
                    task_id: task_id.to_string(),
                }
            } else {
                QATransition::StillRed {
                    task_id: task_id.to_string(),
                    failures: current_failed,
                }
            }
        }
        Some(prev) => {
            let prev_failed = prev.summary.failed;

            match (prev_failed > 0, current_failed > 0) {
                (true, false) => QATransition::RedToGreen {
                    task_id: task_id.to_string(),
                    previous_failures: prev_failed,
                    current_passed,
                },
                (false, true) => QATransition::GreenToRed {
                    task_id: task_id.to_string(),
                    new_failures: current_failed,
                },
                (false, false) => QATransition::StillGreen {
                    task_id: task_id.to_string(),
                },
                (true, true) => QATransition::StillRed {
                    task_id: task_id.to_string(),
                    failures: current_failed,
                },
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qa_agent::{QASummary, QATestResult};

    fn mk_test(name: &str, suite: &str, passed: bool, detail: &str) -> QATestResult {
        QATestResult {
            name: name.to_string(),
            suite: suite.to_string(),
            passed,
            detail: detail.to_string(),
            duration_ms: 100,
        }
    }

    fn mk_result(tests: Vec<QATestResult>) -> QAResult {
        let total = tests.len() as u32;
        let passed = tests.iter().filter(|t| t.passed).count() as u32;
        let failed = total - passed;
        QAResult {
            branch: "task/T1".to_string(),
            commit: "abc123".to_string(),
            timestamp: Utc::now(),
            tests,
            summary: QASummary {
                total,
                passed,
                failed,
            },
        }
    }

    #[test]
    fn classify_regression_when_baseline_passed() {
        let test = mk_test("login", "auth", false, "assertion failed");
        let class = classify_failure(&test, true);
        assert_eq!(class, QAFailureClass::Regression);
    }

    #[test]
    fn classify_acceptance_gap_when_baseline_never_passed() {
        let test = mk_test("new_feature", "ui", false, "not implemented");
        let class = classify_failure(&test, false);
        assert_eq!(class, QAFailureClass::AcceptanceGap);
    }

    #[test]
    fn classify_environment_issue_from_detail() {
        let test = mk_test("api", "net", false, "connection refused on port 8080");
        let class = classify_failure(&test, true);
        assert_eq!(class, QAFailureClass::EnvironmentIssue);
    }

    #[test]
    fn classify_flaky_from_detail() {
        let test = mk_test("render", "ui", false, "intermittent timeout");
        let class = classify_failure(&test, true);
        assert_eq!(class, QAFailureClass::Flaky);
    }

    #[test]
    fn failure_class_auto_fixability() {
        assert!(QAFailureClass::Regression.is_auto_fixable());
        assert!(QAFailureClass::AcceptanceGap.is_auto_fixable());
        assert!(!QAFailureClass::Flaky.is_auto_fixable());
        assert!(!QAFailureClass::EnvironmentIssue.is_auto_fixable());
        assert!(!QAFailureClass::Unknown.is_auto_fixable());
    }

    #[test]
    fn failure_class_retryability() {
        assert!(QAFailureClass::Flaky.is_retryable());
        assert!(QAFailureClass::EnvironmentIssue.is_retryable());
        assert!(!QAFailureClass::Regression.is_retryable());
        assert!(!QAFailureClass::AcceptanceGap.is_retryable());
    }

    #[test]
    fn retry_state_from_result_identifies_classes() {
        let result = mk_result(vec![
            mk_test("a", "s", true, "ok"),
            mk_test("b", "s", false, "assertion failed"),
        ]);
        let baseline = mk_result(vec![
            mk_test("a", "s", true, "ok"),
            mk_test("b", "s", true, "ok"),
        ]);
        let config = QARetryConfig::default();
        let state = QARetryState::from_result("T1", &result, Some(&baseline), &config, Utc::now());
        assert!(state.should_auto_fix);
        assert!(!state.should_retry); // Regression, not retryable
        assert_eq!(*state.failures_by_class.get(&QAFailureClass::Regression).unwrap_or(&0), 1);
    }

    #[test]
    fn retry_state_backoff() {
        let config = QARetryConfig {
            max_retries: 3,
            base_delay_secs: 10,
            backoff_multiplier: 2.0,
            max_delay_secs: 120,
        };
        let result = mk_result(vec![mk_test("a", "s", false, "flaky timeout")]);
        let now = Utc::now();
        let mut state = QARetryState::from_result("T1", &result, None, &config, now);

        assert!(state.record_retry(&config, now));
        assert_eq!(state.attempt, 2);

        assert!(state.record_retry(&config, now));
        assert_eq!(state.attempt, 3);

        assert!(!state.record_retry(&config, now)); // Exhausted
        assert!(state.is_exhausted());
    }

    #[test]
    fn fix_prompt_includes_regressions_and_gaps() {
        let result = mk_result(vec![
            mk_test("login", "auth", false, "assertion failed: expected 200, got 401"),
            mk_test("new_widget", "ui", false, "element not found"),
            mk_test("startup", "core", true, "ok"),
        ]);
        let baseline = mk_result(vec![
            mk_test("login", "auth", true, "ok"),
            mk_test("startup", "core", true, "ok"),
        ]);

        let prompt = build_fix_prompt(&result, Some(&baseline));
        assert!(prompt.contains("Regressions"));
        assert!(prompt.contains("auth.login"));
        assert!(prompt.contains("Acceptance Gaps"));
        assert!(prompt.contains("ui.new_widget"));
    }

    #[test]
    fn transition_red_to_green() {
        let prev = mk_result(vec![mk_test("a", "s", false, "fail")]);
        let curr = mk_result(vec![mk_test("a", "s", true, "ok")]);
        let transition = detect_transition("T1", Some(&prev), &curr);
        assert!(matches!(transition, QATransition::RedToGreen { .. }));
    }

    #[test]
    fn transition_green_to_red() {
        let prev = mk_result(vec![mk_test("a", "s", true, "ok")]);
        let curr = mk_result(vec![mk_test("a", "s", false, "fail")]);
        let transition = detect_transition("T1", Some(&prev), &curr);
        assert!(matches!(transition, QATransition::GreenToRed { .. }));
    }

    #[test]
    fn transition_still_green() {
        let prev = mk_result(vec![mk_test("a", "s", true, "ok")]);
        let curr = mk_result(vec![mk_test("a", "s", true, "ok")]);
        let transition = detect_transition("T1", Some(&prev), &curr);
        assert!(matches!(transition, QATransition::StillGreen { .. }));
    }

    #[test]
    fn transition_still_red() {
        let prev = mk_result(vec![mk_test("a", "s", false, "fail")]);
        let curr = mk_result(vec![mk_test("a", "s", false, "still fail")]);
        let transition = detect_transition("T1", Some(&prev), &curr);
        assert!(matches!(transition, QATransition::StillRed { .. }));
    }

    #[test]
    fn transition_no_previous_green() {
        let curr = mk_result(vec![mk_test("a", "s", true, "ok")]);
        let transition = detect_transition("T1", None, &curr);
        assert!(matches!(transition, QATransition::StillGreen { .. }));
    }

    #[test]
    fn transition_no_previous_red() {
        let curr = mk_result(vec![mk_test("a", "s", false, "fail")]);
        let transition = detect_transition("T1", None, &curr);
        assert!(matches!(transition, QATransition::StillRed { .. }));
    }
}
