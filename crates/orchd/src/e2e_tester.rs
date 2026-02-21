//! E2E Testing Framework — post-merge test harness.
//!
//! Provides:
//! - Compile → unit → integration test pipeline
//! - Per-repo test specifications via `.othala/e2e-spec.toml`
//! - Pass criteria enforcement
//! - Override support for non-prod repos

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

// ─────────────────────────────────────────────────────────────────────────────
// E2E Spec — Per-repo test configuration
// ─────────────────────────────────────────────────────────────────────────────

/// E2E test specification for a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2ESpec {
    /// Name of the spec
    pub name: String,
    /// Whether E2E is required for merge
    #[serde(default = "default_true")]
    pub required: bool,
    /// Test stages to run
    #[serde(default)]
    pub stages: Vec<TestStage>,
    /// Pass criteria
    #[serde(default)]
    pub pass_criteria: PassCriteria,
    /// Timeout for the entire E2E run (seconds)
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Environment variables to set
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Nix shell command (if using nix)
    pub nix_shell: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> u64 {
    1800 // 30 minutes
}

impl Default for E2ESpec {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            required: true,
            stages: vec![
                TestStage::default_compile(),
                TestStage::default_unit(),
            ],
            pass_criteria: PassCriteria::default(),
            timeout_secs: default_timeout(),
            env: HashMap::new(),
            nix_shell: None,
        }
    }
}

impl E2ESpec {
    /// Load E2E spec from `.othala/e2e-spec.toml`.
    pub fn load(repo_root: &Path) -> Option<Self> {
        let path = repo_root.join(".othala/e2e-spec.toml");
        if !path.exists() {
            return None;
        }
        let contents = fs::read_to_string(path).ok()?;
        toml::from_str(&contents).ok()
    }

    /// Load or create default spec.
    pub fn load_or_default(repo_root: &Path) -> Self {
        Self::load(repo_root).unwrap_or_else(|| Self::default_for_repo(repo_root))
    }

    /// Create default spec based on repo detection.
    fn default_for_repo(repo_root: &Path) -> Self {
        let mut spec = Self::default();

        // Detect Rust
        if repo_root.join("Cargo.toml").exists() {
            spec.stages = vec![
                TestStage {
                    name: "compile".to_string(),
                    command: "cargo check --all-targets".to_string(),
                    timeout_secs: 300,
                    required: true,
                    continue_on_fail: false,
                },
                TestStage {
                    name: "lint".to_string(),
                    command: "cargo clippy --all-targets -- -D warnings".to_string(),
                    timeout_secs: 300,
                    required: false,
                    continue_on_fail: true,
                },
                TestStage {
                    name: "unit".to_string(),
                    command: "cargo test --lib".to_string(),
                    timeout_secs: 600,
                    required: true,
                    continue_on_fail: false,
                },
            ];

            // Check for nix
            if repo_root.join("flake.nix").exists() {
                spec.nix_shell = Some("nix develop".to_string());
            }
        }

        // Detect TypeScript
        if repo_root.join("package.json").exists() {
            spec.stages = vec![
                TestStage {
                    name: "install".to_string(),
                    command: "npm ci".to_string(),
                    timeout_secs: 300,
                    required: true,
                    continue_on_fail: false,
                },
                TestStage {
                    name: "compile".to_string(),
                    command: "npm run build".to_string(),
                    timeout_secs: 300,
                    required: true,
                    continue_on_fail: false,
                },
                TestStage {
                    name: "lint".to_string(),
                    command: "npm run lint".to_string(),
                    timeout_secs: 120,
                    required: false,
                    continue_on_fail: true,
                },
                TestStage {
                    name: "test".to_string(),
                    command: "npm test".to_string(),
                    timeout_secs: 600,
                    required: true,
                    continue_on_fail: false,
                },
            ];
        }

        spec
    }
}

/// A single test stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestStage {
    /// Stage name
    pub name: String,
    /// Command to run
    pub command: String,
    /// Timeout for this stage (seconds)
    #[serde(default = "default_stage_timeout")]
    pub timeout_secs: u64,
    /// Whether this stage is required to pass
    #[serde(default = "default_true")]
    pub required: bool,
    /// Continue running subsequent stages even if this fails
    #[serde(default)]
    pub continue_on_fail: bool,
}

fn default_stage_timeout() -> u64 {
    600
}

impl TestStage {
    fn default_compile() -> Self {
        Self {
            name: "compile".to_string(),
            command: "cargo check --all-targets".to_string(),
            timeout_secs: 300,
            required: true,
            continue_on_fail: false,
        }
    }

    fn default_unit() -> Self {
        Self {
            name: "unit".to_string(),
            command: "cargo test".to_string(),
            timeout_secs: 600,
            required: true,
            continue_on_fail: false,
        }
    }
}

/// Pass criteria for E2E.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassCriteria {
    /// Minimum required stages to pass
    #[serde(default)]
    pub min_stages_passed: Option<usize>,
    /// All required stages must pass
    #[serde(default = "default_true")]
    pub all_required_must_pass: bool,
    /// Specific stages that must pass
    #[serde(default)]
    pub required_stages: Vec<String>,
}

impl Default for PassCriteria {
    fn default() -> Self {
        Self {
            min_stages_passed: None,
            all_required_must_pass: true,
            required_stages: vec![],
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// E2E Result — Test run results
// ─────────────────────────────────────────────────────────────────────────────

/// Result of an E2E test run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2EResult {
    /// Task ID this E2E was for
    pub task_id: String,
    /// Branch name
    pub branch: String,
    /// Overall pass/fail
    pub passed: bool,
    /// Individual stage results
    pub stages: Vec<StageResult>,
    /// Started at
    pub started_at: DateTime<Utc>,
    /// Ended at
    pub ended_at: DateTime<Utc>,
    /// Total duration (seconds)
    pub duration_secs: f64,
    /// Error message if overall failure
    pub error: Option<String>,
}

impl E2EResult {
    /// Get summary statistics.
    pub fn summary(&self) -> E2ESummary {
        E2ESummary {
            total_stages: self.stages.len(),
            passed_stages: self.stages.iter().filter(|s| s.passed).count(),
            failed_stages: self.stages.iter().filter(|s| !s.passed).count(),
            skipped_stages: self.stages.iter().filter(|s| s.skipped).count(),
            duration_secs: self.duration_secs,
        }
    }

    /// Get failed stage names.
    pub fn failed_stage_names(&self) -> Vec<&str> {
        self.stages
            .iter()
            .filter(|s| !s.passed && !s.skipped)
            .map(|s| s.name.as_str())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2ESummary {
    pub total_stages: usize,
    pub passed_stages: usize,
    pub failed_stages: usize,
    pub skipped_stages: usize,
    pub duration_secs: f64,
}

/// Result of a single test stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    /// Stage name
    pub name: String,
    /// Whether it passed
    pub passed: bool,
    /// Whether it was skipped
    pub skipped: bool,
    /// Exit code
    pub exit_code: Option<i32>,
    /// Duration (seconds)
    pub duration_secs: f64,
    /// Stdout (truncated)
    pub stdout: String,
    /// Stderr (truncated)
    pub stderr: String,
    /// Error message
    pub error: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// E2E Tester — The test runner
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the E2E tester.
#[derive(Debug, Clone)]
pub struct E2ETesterConfig {
    /// Maximum output to capture per stage (bytes)
    pub max_output_bytes: usize,
    /// Whether to run in verbose mode
    pub verbose: bool,
}

impl Default for E2ETesterConfig {
    fn default() -> Self {
        Self {
            max_output_bytes: 100_000,
            verbose: false,
        }
    }
}

/// The E2E Tester — runs test pipelines.
pub struct E2ETester {
    pub config: E2ETesterConfig,
}

impl E2ETester {
    pub fn new(config: E2ETesterConfig) -> Self {
        Self { config }
    }

    /// **GRACEFUL FALLBACK:** Run E2E with automatic skip if spec is missing.
    /// Never blocks on missing `.othala/e2e-spec.toml`.
    pub fn run_with_fallback(
        &self,
        repo_root: &Path,
        task_id: &str,
        branch: &str,
    ) -> E2EResult {
        let started_at = Utc::now();
        let start_instant = Instant::now();

        // If spec doesn't exist, return "skipped" (not failure)
        let spec = match E2ESpec::load(repo_root) {
            Some(s) => s,
            None => {
                eprintln!(
                    "[e2e] INFO: E2E spec not found for {}. Skipping E2E tests (non-blocking).",
                    task_id
                );
                let duration_secs = start_instant.elapsed().as_secs_f64();
                return E2EResult {
                    task_id: task_id.to_string(),
                    branch: branch.to_string(),
                    passed: true, // Treat missing spec as "pass" (don't block merge)
                    stages: vec![StageResult {
                        name: "skipped (no spec)".to_string(),
                        passed: true,
                        skipped: true,
                        exit_code: None,
                        duration_secs,
                        stdout: "E2E spec not found; skipping E2E validation".to_string(),
                        stderr: String::new(),
                        error: None,
                    }],
                    started_at,
                    ended_at: Utc::now(),
                    duration_secs,
                    error: None,
                };
            }
        };

        // Run E2E normally
        self.run(&spec, repo_root, task_id, branch)
    }

    /// Run E2E tests for a task.
    pub fn run(
        &self,
        spec: &E2ESpec,
        repo_root: &Path,
        task_id: &str,
        branch: &str,
    ) -> E2EResult {
        let started_at = Utc::now();
        let start_instant = Instant::now();
        let mut stages = Vec::new();
        let mut all_passed = true;
        let mut should_continue = true;

        for stage in &spec.stages {
            if !should_continue {
                stages.push(StageResult {
                    name: stage.name.clone(),
                    passed: false,
                    skipped: true,
                    exit_code: None,
                    duration_secs: 0.0,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: Some("Skipped due to prior failure".to_string()),
                });
                continue;
            }

            let result = self.run_stage(spec, stage, repo_root);

            if !result.passed && stage.required {
                all_passed = false;
            }

            if !result.passed && !stage.continue_on_fail {
                should_continue = false;
            }

            stages.push(result);

            // Check overall timeout
            if start_instant.elapsed().as_secs() > spec.timeout_secs {
                all_passed = false;
                break;
            }
        }

        // Evaluate pass criteria
        let passed = self.evaluate_pass_criteria(spec, &stages, all_passed);

        let ended_at = Utc::now();
        let duration_secs = start_instant.elapsed().as_secs_f64();

        E2EResult {
            task_id: task_id.to_string(),
            branch: branch.to_string(),
            passed,
            stages,
            started_at,
            ended_at,
            duration_secs,
            error: if passed {
                None
            } else {
                Some("E2E tests failed".to_string())
            },
        }
    }

    /// Run a single test stage.
    fn run_stage(&self, spec: &E2ESpec, stage: &TestStage, repo_root: &Path) -> StageResult {
        let start = Instant::now();

        // Build command with nix shell if needed
        let command = if let Some(nix) = &spec.nix_shell {
            format!("{} -c '{}'", nix, stage.command)
        } else {
            stage.command.clone()
        };

        // Run the command
        let result = Command::new("bash")
            .arg("-lc")
            .arg(&command)
            .current_dir(repo_root)
            .envs(&spec.env)
            .output();

        let duration_secs = start.elapsed().as_secs_f64();

        match result {
            Ok(output) => {
                let stdout = truncate_output(&output.stdout, self.config.max_output_bytes);
                let stderr = truncate_output(&output.stderr, self.config.max_output_bytes);
                let passed = output.status.success();

                StageResult {
                    name: stage.name.clone(),
                    passed,
                    skipped: false,
                    exit_code: output.status.code(),
                    duration_secs,
                    stdout,
                    stderr,
                    error: if passed {
                        None
                    } else {
                        Some(format!(
                            "Stage '{}' failed with exit code {:?}",
                            stage.name,
                            output.status.code()
                        ))
                    },
                }
            }
            Err(e) => StageResult {
                name: stage.name.clone(),
                passed: false,
                skipped: false,
                exit_code: None,
                duration_secs,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(format!("Failed to run stage '{}': {}", stage.name, e)),
            },
        }
    }

    /// Evaluate pass criteria.
    fn evaluate_pass_criteria(
        &self,
        spec: &E2ESpec,
        stages: &[StageResult],
        all_required_passed: bool,
    ) -> bool {
        let criteria = &spec.pass_criteria;

        // Check all required stages passed
        if criteria.all_required_must_pass && !all_required_passed {
            return false;
        }

        // Check minimum stages passed
        if let Some(min) = criteria.min_stages_passed {
            let passed_count = stages.iter().filter(|s| s.passed).count();
            if passed_count < min {
                return false;
            }
        }

        // Check specific required stages
        for required in &criteria.required_stages {
            let stage_passed = stages
                .iter()
                .find(|s| &s.name == required)
                .map(|s| s.passed)
                .unwrap_or(false);
            if !stage_passed {
                return false;
            }
        }

        true
    }

    /// Check if E2E should be skipped for a repo.
    pub fn should_skip(&self, repo_root: &Path) -> bool {
        // Check for explicit skip file
        if repo_root.join(".othala/skip-e2e").exists() {
            return true;
        }

        // Check repo-mode.toml
        let mode_path = repo_root.join(".othala/repo-mode.toml");
        if let Ok(contents) = fs::read_to_string(mode_path) {
            if contents.contains("e2e = false") || contents.contains("skip_e2e = true") {
                return true;
            }
        }

        false
    }

    /// Check if E2E override is allowed (non-prod repos).
    pub fn override_allowed(&self, repo_root: &Path) -> bool {
        let mode_path = repo_root.join(".othala/repo-mode.toml");
        if let Ok(contents) = fs::read_to_string(mode_path) {
            if contents.contains("mode = \"merge\"") || contents.contains("prod = false") {
                return true;
            }
        }

        // Check if repo name suggests non-prod
        if let Some(name) = repo_root.file_name().and_then(|n| n.to_str()) {
            let lower = name.to_ascii_lowercase();
            if lower.contains("test")
                || lower.contains("dev")
                || lower.contains("sandbox")
                || lower.contains("experiment")
            {
                return true;
            }
        }

        false
    }
}

impl Default for E2ETester {
    fn default() -> Self {
        Self::new(E2ETesterConfig::default())
    }
}

fn truncate_output(bytes: &[u8], max_bytes: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        format!("{}...[truncated]", &s[..max_bytes])
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// E2E Gate — Merge gating logic
// ─────────────────────────────────────────────────────────────────────────────

/// Decision from the E2E gate.
#[derive(Debug, Clone)]
pub enum E2EGateDecision {
    /// Allow merge — E2E passed
    Allow,
    /// Block merge — E2E failed
    Block { reason: String },
    /// Skip E2E — not required for this repo
    Skip { reason: String },
    /// Allow override — E2E failed but override is allowed
    AllowOverride { reason: String, result: E2EResult },
}

/// The E2E Gate — decides whether to allow merge.
pub struct E2EGate {
    pub tester: E2ETester,
}

impl E2EGate {
    pub fn new(tester: E2ETester) -> Self {
        Self { tester }
    }

    /// Check if a task can be merged.
    pub fn check(
        &self,
        spec: &E2ESpec,
        repo_root: &Path,
        task_id: &str,
        branch: &str,
    ) -> E2EGateDecision {
        // Check if E2E should be skipped
        if self.tester.should_skip(repo_root) {
            return E2EGateDecision::Skip {
                reason: "E2E skipped for this repo".to_string(),
            };
        }

        // Check if E2E is not required
        if !spec.required {
            return E2EGateDecision::Skip {
                reason: "E2E not required by spec".to_string(),
            };
        }

        // Run E2E
        let result = self.tester.run(spec, repo_root, task_id, branch);

        if result.passed {
            E2EGateDecision::Allow
        } else if self.tester.override_allowed(repo_root) {
            E2EGateDecision::AllowOverride {
                reason: format!(
                    "E2E failed but override allowed for non-prod repo. Failed: {}",
                    result.failed_stage_names().join(", ")
                ),
                result,
            }
        } else {
            E2EGateDecision::Block {
                reason: format!(
                    "E2E failed. Stages failed: {}",
                    result.failed_stage_names().join(", ")
                ),
            }
        }
    }

    /// Quick check without running tests (just check if E2E would run).
    pub fn would_run(&self, repo_root: &Path) -> bool {
        if self.tester.should_skip(repo_root) {
            return false;
        }
        let spec = E2ESpec::load_or_default(repo_root);
        spec.required
    }
}

impl Default for E2EGate {
    fn default() -> Self {
        Self::new(E2ETester::default())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Metrics
// ─────────────────────────────────────────────────────────────────────────────

/// E2E metrics for tracking.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct E2EMetrics {
    pub total_runs: u64,
    pub passed_runs: u64,
    pub failed_runs: u64,
    pub skipped_runs: u64,
    pub override_runs: u64,
    pub avg_duration_secs: f64,
    pub stage_pass_rates: HashMap<String, f64>,
}

impl E2EMetrics {
    /// Update metrics with a new result.
    pub fn record(&mut self, result: &E2EResult) {
        self.total_runs += 1;
        if result.passed {
            self.passed_runs += 1;
        } else {
            self.failed_runs += 1;
        }

        // Update average duration
        let prev_total = self.avg_duration_secs * (self.total_runs - 1) as f64;
        self.avg_duration_secs = (prev_total + result.duration_secs) / self.total_runs as f64;

        // Update stage pass rates
        for stage in &result.stages {
            let rate = self
                .stage_pass_rates
                .entry(stage.name.clone())
                .or_insert(0.0);
            // Simple moving average
            *rate = (*rate * 0.9) + if stage.passed { 0.1 } else { 0.0 };
        }
    }

    /// Get pass rate as percentage.
    pub fn pass_rate(&self) -> f64 {
        if self.total_runs == 0 {
            0.0
        } else {
            (self.passed_runs as f64 / self.total_runs as f64) * 100.0
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    #[test]
    fn default_spec_detects_rust_repo() {
        let root = temp_dir().join(format!("othala-e2e-test-{}", Utc::now().timestamp()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let spec = E2ESpec::default_for_repo(&root);

        assert!(spec.stages.iter().any(|s| s.name == "compile"));
        assert!(spec.stages.iter().any(|s| s.name == "unit"));
        assert!(spec.stages.iter().any(|s| s.command.contains("cargo")));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stage_result_tracking() {
        let result = E2EResult {
            task_id: "T1".to_string(),
            branch: "task/T1".to_string(),
            passed: false,
            stages: vec![
                StageResult {
                    name: "compile".to_string(),
                    passed: true,
                    skipped: false,
                    exit_code: Some(0),
                    duration_secs: 10.0,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: None,
                },
                StageResult {
                    name: "test".to_string(),
                    passed: false,
                    skipped: false,
                    exit_code: Some(1),
                    duration_secs: 30.0,
                    stdout: String::new(),
                    stderr: "test failed".to_string(),
                    error: Some("Test failed".to_string()),
                },
            ],
            started_at: Utc::now(),
            ended_at: Utc::now(),
            duration_secs: 40.0,
            error: Some("E2E failed".to_string()),
        };

        let summary = result.summary();
        assert_eq!(summary.total_stages, 2);
        assert_eq!(summary.passed_stages, 1);
        assert_eq!(summary.failed_stages, 1);

        let failed = result.failed_stage_names();
        assert_eq!(failed, vec!["test"]);
    }

    #[test]
    fn pass_criteria_evaluation() {
        let tester = E2ETester::default();
        let spec = E2ESpec {
            pass_criteria: PassCriteria {
                min_stages_passed: Some(2),
                all_required_must_pass: true,
                required_stages: vec!["compile".to_string()],
            },
            ..Default::default()
        };

        let stages = vec![
            StageResult {
                name: "compile".to_string(),
                passed: true,
                skipped: false,
                exit_code: Some(0),
                duration_secs: 10.0,
                stdout: String::new(),
                stderr: String::new(),
                error: None,
            },
            StageResult {
                name: "test".to_string(),
                passed: true,
                skipped: false,
                exit_code: Some(0),
                duration_secs: 20.0,
                stdout: String::new(),
                stderr: String::new(),
                error: None,
            },
        ];

        assert!(tester.evaluate_pass_criteria(&spec, &stages, true));
    }

    #[test]
    fn metrics_tracking() {
        let mut metrics = E2EMetrics::default();

        metrics.record(&E2EResult {
            task_id: "T1".to_string(),
            branch: "task/T1".to_string(),
            passed: true,
            stages: vec![StageResult {
                name: "compile".to_string(),
                passed: true,
                skipped: false,
                exit_code: Some(0),
                duration_secs: 10.0,
                stdout: String::new(),
                stderr: String::new(),
                error: None,
            }],
            started_at: Utc::now(),
            ended_at: Utc::now(),
            duration_secs: 10.0,
            error: None,
        });

        metrics.record(&E2EResult {
            task_id: "T2".to_string(),
            branch: "task/T2".to_string(),
            passed: false,
            stages: vec![StageResult {
                name: "compile".to_string(),
                passed: false,
                skipped: false,
                exit_code: Some(1),
                duration_secs: 5.0,
                stdout: String::new(),
                stderr: String::new(),
                error: Some("failed".to_string()),
            }],
            started_at: Utc::now(),
            ended_at: Utc::now(),
            duration_secs: 5.0,
            error: Some("E2E failed".to_string()),
        });

        assert_eq!(metrics.total_runs, 2);
        assert_eq!(metrics.passed_runs, 1);
        assert_eq!(metrics.failed_runs, 1);
        assert_eq!(metrics.pass_rate(), 50.0);
    }

    #[test]
    fn override_detection() {
        let root = temp_dir().join(format!("othala-e2e-override-{}", Utc::now().timestamp()));
        fs::create_dir_all(root.join(".othala")).unwrap();

        // Test with merge mode
        fs::write(
            root.join(".othala/repo-mode.toml"),
            "mode = \"merge\"\nprod = false",
        )
        .unwrap();

        let tester = E2ETester::default();
        assert!(tester.override_allowed(&root));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skip_detection() {
        let root = temp_dir().join(format!("othala-e2e-skip-{}", Utc::now().timestamp()));
        fs::create_dir_all(root.join(".othala")).unwrap();

        // Test with skip file
        fs::write(root.join(".othala/skip-e2e"), "").unwrap();

        let tester = E2ETester::default();
        assert!(tester.should_skip(&root));

        fs::remove_dir_all(&root).ok();
    }
}
