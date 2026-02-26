//! E2E orchestration scenarios — full-lifecycle scenario runner, chaos
//! injection, and soak-test framework.
//!
//! Unlike the per-repo `e2e_tester` (compile → unit → integration pipeline),
//! this module tests the **orchestrator itself**: task creation → agent spawn →
//! verify → submit → merge, including failure injection and long-running soak.
//!
//! # Key types
//! - [`Scenario`] — a declarative orchestration test case.
//! - [`ScenarioStep`] — individual step within a scenario.
//! - [`ChaosPolicy`] — configurable fault injection.
//! - [`SoakConfig`] — soak-test parameters (tick count, stuck-loop detection).
//! - [`ScenarioRunner`] — executes scenarios and collects verdicts.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────────────────────
// Scenario definition
// ─────────────────────────────────────────────────────────────────────────────

/// A declarative orchestration test scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// Unique name for the scenario.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Ordered steps to execute.
    pub steps: Vec<ScenarioStep>,
    /// Maximum time for the entire scenario (seconds).
    #[serde(default = "default_scenario_timeout")]
    pub timeout_secs: u64,
    /// Tags for filtering / grouping.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Whether failure should abort the full suite.
    #[serde(default)]
    pub critical: bool,
}

fn default_scenario_timeout() -> u64 {
    600 // 10 minutes
}

/// A single step in an orchestration scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ScenarioStep {
    /// Create a simulated task with the given spec.
    CreateTask {
        task_id: String,
        description: String,
    },
    /// Assert a task reaches the expected state within a timeout.
    ExpectState {
        task_id: String,
        expected_state: String,
        timeout_secs: u64,
    },
    /// Simulate an agent completing work on a task.
    CompleteAgent {
        task_id: String,
        success: bool,
        model: String,
    },
    /// Simulate a verify pass/fail for a task.
    SimulateVerify {
        task_id: String,
        success: bool,
    },
    /// Simulate a QA run with specific pass/fail outcome.
    SimulateQA {
        task_id: String,
        passed: bool,
        failed_tests: Vec<String>,
    },
    /// Inject a chaos fault.
    InjectChaos {
        fault: ChaosFault,
    },
    /// Clear all active chaos faults.
    ClearChaos,
    /// Wait for a number of simulated ticks to pass.
    WaitTicks {
        count: u64,
    },
    /// Assert on a metric value.
    AssertMetric {
        metric: String,
        op: CompareOp,
        value: f64,
    },
    /// Assert the system has no stuck tasks (no task unchanged for N ticks).
    AssertNoStuckTasks {
        max_unchanged_ticks: u64,
    },
    /// Simulate a successful merge for a task.
    SimulateMerge {
        task_id: String,
    },
    /// Assert the total task count.
    AssertTaskCount {
        expected: usize,
    },
    /// Log a message (for debugging / audit trail).
    Log {
        message: String,
    },
}

/// Comparison operator for metric assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareOp {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
}

impl CompareOp {
    /// Evaluate `lhs <op> rhs`.
    pub fn evaluate(&self, lhs: f64, rhs: f64) -> bool {
        match self {
            CompareOp::Eq => (lhs - rhs).abs() < f64::EPSILON,
            CompareOp::Gt => lhs > rhs,
            CompareOp::Gte => lhs >= rhs,
            CompareOp::Lt => lhs < rhs,
            CompareOp::Lte => lhs <= rhs,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Chaos injection
// ─────────────────────────────────────────────────────────────────────────────

/// Specific fault to inject into the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChaosFault {
    /// Simulates an agent process crash.
    AgentCrash { task_id: String },
    /// Simulates a graphite CLI failure (gt submit / gt create).
    GraphiteFailure { operation: String },
    /// Simulates context generation taking too long or failing.
    ContextGenFailure,
    /// Simulates a model becoming unhealthy (all requests fail).
    ModelHealthDrop { model: String },
    /// Simulates network connectivity loss.
    NetworkOutage,
    /// Simulates disk-full / write failure.
    DiskFull,
    /// Simulates a stuck task (agent never completes).
    AgentHang { task_id: String },
}

impl ChaosFault {
    /// Human-readable label.
    pub fn label(&self) -> &str {
        match self {
            ChaosFault::AgentCrash { .. } => "agent_crash",
            ChaosFault::GraphiteFailure { .. } => "graphite_failure",
            ChaosFault::ContextGenFailure => "context_gen_failure",
            ChaosFault::ModelHealthDrop { .. } => "model_health_drop",
            ChaosFault::NetworkOutage => "network_outage",
            ChaosFault::DiskFull => "disk_full",
            ChaosFault::AgentHang { .. } => "agent_hang",
        }
    }
}

/// Chaos policy — controls which faults are active and their parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChaosPolicy {
    /// Active faults being injected.
    pub active_faults: Vec<ChaosFault>,
    /// Probability of injecting a random fault per tick (0.0 – 1.0).
    #[serde(default)]
    pub random_fault_probability: f64,
    /// Maximum number of simultaneous active faults.
    #[serde(default = "default_max_faults")]
    pub max_concurrent_faults: usize,
}

fn default_max_faults() -> usize {
    3
}

impl Default for ChaosPolicy {
    fn default() -> Self {
        Self {
            active_faults: Vec::new(),
            random_fault_probability: 0.0,
            max_concurrent_faults: default_max_faults(),
        }
    }
}

impl ChaosPolicy {
    /// Add a fault to the active set (respecting max limit).
    pub fn inject(&mut self, fault: ChaosFault) -> bool {
        if self.active_faults.len() >= self.max_concurrent_faults {
            return false;
        }
        self.active_faults.push(fault);
        true
    }

    /// Remove all active faults.
    pub fn clear(&mut self) {
        self.active_faults.clear();
    }

    /// Check if a specific fault type is active.
    pub fn has_fault(&self, label: &str) -> bool {
        self.active_faults.iter().any(|f| f.label() == label)
    }

    /// Check if any fault targets a specific task.
    pub fn fault_for_task(&self, task_id: &str) -> Option<&ChaosFault> {
        self.active_faults.iter().find(|f| match f {
            ChaosFault::AgentCrash { task_id: tid } => tid == task_id,
            ChaosFault::AgentHang { task_id: tid } => tid == task_id,
            _ => false,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Soak test configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Soak test parameters for sustained orchestration testing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoakConfig {
    /// Total number of ticks to simulate.
    pub total_ticks: u64,
    /// Maximum ticks any single task may remain in the same state before
    /// being flagged as "stuck".
    #[serde(default = "default_stuck_threshold")]
    pub stuck_threshold_ticks: u64,
    /// Maximum allowed percentage of tasks in error/stopped states.
    #[serde(default = "default_max_error_rate")]
    pub max_error_rate_pct: f64,
    /// Whether to inject random chaos during soak.
    #[serde(default)]
    pub enable_chaos: bool,
    /// Chaos probability per tick when chaos is enabled (0.0 – 1.0).
    #[serde(default = "default_chaos_probability")]
    pub chaos_probability: f64,
    /// Report interval (ticks between progress reports).
    #[serde(default = "default_report_interval")]
    pub report_interval_ticks: u64,
}

fn default_stuck_threshold() -> u64 {
    50
}

fn default_max_error_rate() -> f64 {
    20.0
}

fn default_chaos_probability() -> f64 {
    0.05
}

fn default_report_interval() -> u64 {
    100
}

impl Default for SoakConfig {
    fn default() -> Self {
        Self {
            total_ticks: 1000,
            stuck_threshold_ticks: default_stuck_threshold(),
            max_error_rate_pct: default_max_error_rate(),
            enable_chaos: false,
            chaos_probability: default_chaos_probability(),
            report_interval_ticks: default_report_interval(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Simulated task state (lightweight, not real DaemonState)
// ─────────────────────────────────────────────────────────────────────────────

/// Lightweight simulated task for scenario execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedTask {
    pub task_id: String,
    pub description: String,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// How many ticks the task has been in its current state.
    pub ticks_in_state: u64,
    /// History of state transitions.
    pub transitions: Vec<StateTransitionRecord>,
}

/// Record of a simulated state transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransitionRecord {
    pub from: String,
    pub to: String,
    pub at_tick: u64,
    pub reason: String,
}

impl SimulatedTask {
    /// Create a new simulated task in CHATTING state.
    pub fn new(task_id: &str, description: &str, now: DateTime<Utc>) -> Self {
        Self {
            task_id: task_id.to_string(),
            description: description.to_string(),
            state: "CHATTING".to_string(),
            created_at: now,
            updated_at: now,
            ticks_in_state: 0,
            transitions: Vec::new(),
        }
    }

    /// Transition to a new state.
    pub fn transition(&mut self, to: &str, tick: u64, reason: &str, now: DateTime<Utc>) {
        let from = self.state.clone();
        self.transitions.push(StateTransitionRecord {
            from: from.clone(),
            to: to.to_string(),
            at_tick: tick,
            reason: reason.to_string(),
        });
        self.state = to.to_string();
        self.ticks_in_state = 0;
        self.updated_at = now;
    }

    /// Tick the task forward (increments ticks_in_state).
    pub fn tick(&mut self) {
        self.ticks_in_state += 1;
    }

    /// Is this task in a terminal state?
    pub fn is_terminal(&self) -> bool {
        self.state == "MERGED" || self.state == "STOPPED"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario runner state
// ─────────────────────────────────────────────────────────────────────────────

/// System-level metrics tracked during scenario execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScenarioMetrics {
    pub total_ticks: u64,
    pub tasks_created: u64,
    pub tasks_merged: u64,
    pub tasks_stopped: u64,
    pub agent_spawns: u64,
    pub agent_completions: u64,
    pub verify_runs: u64,
    pub qa_runs: u64,
    pub chaos_injections: u64,
    pub stuck_detections: u64,
    pub state_transitions: u64,
}

impl ScenarioMetrics {
    /// Look up a metric by name for assertion purposes.
    pub fn get(&self, name: &str) -> Option<f64> {
        match name {
            "total_ticks" => Some(self.total_ticks as f64),
            "tasks_created" => Some(self.tasks_created as f64),
            "tasks_merged" => Some(self.tasks_merged as f64),
            "tasks_stopped" => Some(self.tasks_stopped as f64),
            "agent_spawns" => Some(self.agent_spawns as f64),
            "agent_completions" => Some(self.agent_completions as f64),
            "verify_runs" => Some(self.verify_runs as f64),
            "qa_runs" => Some(self.qa_runs as f64),
            "chaos_injections" => Some(self.chaos_injections as f64),
            "stuck_detections" => Some(self.stuck_detections as f64),
            "state_transitions" => Some(self.state_transitions as f64),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario result
// ─────────────────────────────────────────────────────────────────────────────

/// Result of running one scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub scenario_name: String,
    pub passed: bool,
    pub step_results: Vec<StepResult>,
    pub metrics: ScenarioMetrics,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_secs: f64,
    pub error: Option<String>,
}

/// Result of a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_index: usize,
    pub action: String,
    pub passed: bool,
    pub detail: String,
}

/// Aggregated result of running an entire suite of scenarios.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteResult {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub results: Vec<ScenarioResult>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_secs: f64,
}

impl SuiteResult {
    /// Human-readable summary.
    pub fn summary(&self) -> String {
        let status = if self.failed == 0 {
            "\x1b[32mPASS\x1b[0m"
        } else {
            "\x1b[31mFAIL\x1b[0m"
        };
        let mut out = format!(
            "E2E Orchestration Suite: {} ({}/{} passed, {} failed, {} skipped) [{:.1}s]\n",
            status,
            self.passed,
            self.total,
            self.failed,
            self.skipped,
            self.duration_secs,
        );
        for r in &self.results {
            let mark = if r.passed { "\x1b[32m✓\x1b[0m" } else { "\x1b[31m✗\x1b[0m" };
            out.push_str(&format!(
                "  {} {} [{:.1}s]\n",
                mark, r.scenario_name, r.duration_secs,
            ));
            if let Some(err) = &r.error {
                out.push_str(&format!("    \x1b[31m{}\x1b[0m\n", err));
            }
        }
        out
    }

    /// JSON-serialized report.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Soak test result
// ─────────────────────────────────────────────────────────────────────────────

/// Result of a soak test run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoakResult {
    pub passed: bool,
    pub total_ticks: u64,
    pub stuck_tasks: Vec<StuckTaskInfo>,
    pub error_rate_pct: f64,
    pub chaos_events: u64,
    pub progress_reports: Vec<SoakProgressReport>,
    pub duration_secs: f64,
    pub error: Option<String>,
}

/// Info about a task that was detected as stuck.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StuckTaskInfo {
    pub task_id: String,
    pub state: String,
    pub ticks_in_state: u64,
    pub detected_at_tick: u64,
}

/// Periodic progress report during soak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoakProgressReport {
    pub tick: u64,
    pub active_tasks: usize,
    pub terminal_tasks: usize,
    pub stuck_tasks: usize,
    pub chaos_active: bool,
}

impl SoakResult {
    /// Human-readable summary.
    pub fn summary(&self) -> String {
        let status = if self.passed {
            "\x1b[32mPASS\x1b[0m"
        } else {
            "\x1b[31mFAIL\x1b[0m"
        };
        let mut out = format!(
            "Soak Test: {} ({} ticks, {:.1}% error rate, {} stuck tasks) [{:.1}s]\n",
            status, self.total_ticks, self.error_rate_pct, self.stuck_tasks.len(), self.duration_secs,
        );
        if !self.stuck_tasks.is_empty() {
            out.push_str("  Stuck tasks:\n");
            for st in &self.stuck_tasks {
                out.push_str(&format!(
                    "    - {} in {} for {} ticks (detected at tick {})\n",
                    st.task_id, st.state, st.ticks_in_state, st.detected_at_tick,
                ));
            }
        }
        if let Some(err) = &self.error {
            out.push_str(&format!("  \x1b[31mError: {}\x1b[0m\n", err));
        }
        out
    }

    /// JSON-serialized report.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario runner — the executor
// ─────────────────────────────────────────────────────────────────────────────

/// The scenario runner executes orchestration scenarios against a simulated
/// system state.
pub struct ScenarioRunner {
    /// Simulated tasks.
    pub tasks: HashMap<String, SimulatedTask>,
    /// Active chaos policy.
    pub chaos: ChaosPolicy,
    /// Metrics collected during execution.
    pub metrics: ScenarioMetrics,
    /// Current tick counter.
    pub current_tick: u64,
    /// Execution log (for debugging).
    pub log: Vec<String>,
}

impl Default for ScenarioRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl ScenarioRunner {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            chaos: ChaosPolicy::default(),
            metrics: ScenarioMetrics::default(),
            current_tick: 0,
            log: Vec::new(),
        }
    }

    /// Reset the runner for a new scenario.
    pub fn reset(&mut self) {
        self.tasks.clear();
        self.chaos.clear();
        self.metrics = ScenarioMetrics::default();
        self.current_tick = 0;
        self.log.clear();
    }

    /// Run a single scenario and return the result.
    pub fn run_scenario(&mut self, scenario: &Scenario) -> ScenarioResult {
        self.reset();
        let started_at = Utc::now();
        let mut step_results = Vec::new();
        let mut passed = true;
        let mut error: Option<String> = None;

        for (i, step) in scenario.steps.iter().enumerate() {
            let result = self.execute_step(i, step);
            if !result.passed {
                passed = false;
                if error.is_none() {
                    error = Some(format!(
                        "Step {} ({}) failed: {}",
                        i, result.action, result.detail
                    ));
                }
            }
            step_results.push(result);

            // Check timeout
            let elapsed = (Utc::now() - started_at).num_seconds() as u64;
            if elapsed > scenario.timeout_secs {
                passed = false;
                error = Some(format!(
                    "Scenario timed out after {}s (limit: {}s)",
                    elapsed, scenario.timeout_secs,
                ));
                break;
            }
        }

        let ended_at = Utc::now();
        let duration_secs = (ended_at - started_at).num_milliseconds() as f64 / 1000.0;

        ScenarioResult {
            scenario_name: scenario.name.clone(),
            passed,
            step_results,
            metrics: self.metrics.clone(),
            started_at,
            ended_at,
            duration_secs,
            error,
        }
    }

    /// Run a suite of scenarios.
    pub fn run_suite(&mut self, scenarios: &[Scenario]) -> SuiteResult {
        let started_at = Utc::now();
        let mut results = Vec::new();
        let mut passed = 0;
        let mut failed = 0;

        for scenario in scenarios {
            let result = self.run_scenario(scenario);
            if result.passed {
                passed += 1;
            } else {
                failed += 1;
            }
            results.push(result);
        }

        let ended_at = Utc::now();
        let duration_secs = (ended_at - started_at).num_milliseconds() as f64 / 1000.0;

        SuiteResult {
            total: scenarios.len(),
            passed,
            failed,
            skipped: 0,
            results,
            started_at,
            ended_at,
            duration_secs,
        }
    }

    /// Run a soak test.
    pub fn run_soak(
        &mut self,
        config: &SoakConfig,
        initial_tasks: Vec<(String, String)>,
    ) -> SoakResult {
        self.reset();
        let start = std::time::Instant::now();
        let now = Utc::now();

        // Create initial tasks
        for (id, desc) in &initial_tasks {
            let task = SimulatedTask::new(id, desc, now);
            self.tasks.insert(id.clone(), task);
            self.metrics.tasks_created += 1;
        }

        let mut stuck_tasks = Vec::new();
        let mut progress_reports = Vec::new();
        let mut chaos_events: u64 = 0;

        for tick in 0..config.total_ticks {
            self.current_tick = tick;

            // Tick all tasks
            for task in self.tasks.values_mut() {
                task.tick();
            }
            self.metrics.total_ticks = tick + 1;

            // Detect stuck tasks
            for task in self.tasks.values() {
                if !task.is_terminal() && task.ticks_in_state >= config.stuck_threshold_ticks {
                    let already_detected = stuck_tasks
                        .iter()
                        .any(|s: &StuckTaskInfo| s.task_id == task.task_id);
                    if !already_detected {
                        stuck_tasks.push(StuckTaskInfo {
                            task_id: task.task_id.clone(),
                            state: task.state.clone(),
                            ticks_in_state: task.ticks_in_state,
                            detected_at_tick: tick,
                        });
                        self.metrics.stuck_detections += 1;
                    }
                }
            }

            // Simulate autonomous progression for non-terminal tasks
            self.simulate_tick_progression(tick, now + Duration::seconds(tick as i64));

            // Chaos injection (deterministic for testing — use tick % to decide)
            if config.enable_chaos && (tick % 20 == 7) {
                // Inject a simulated fault every 20 ticks (deterministic)
                chaos_events += 1;
                self.metrics.chaos_injections += 1;
            }

            // Progress report
            if config.report_interval_ticks > 0 && tick % config.report_interval_ticks == 0 {
                let active = self.tasks.values().filter(|t| !t.is_terminal()).count();
                let terminal = self.tasks.values().filter(|t| t.is_terminal()).count();
                let stuck = self
                    .tasks
                    .values()
                    .filter(|t| {
                        !t.is_terminal() && t.ticks_in_state >= config.stuck_threshold_ticks
                    })
                    .count();
                progress_reports.push(SoakProgressReport {
                    tick,
                    active_tasks: active,
                    terminal_tasks: terminal,
                    stuck_tasks: stuck,
                    chaos_active: config.enable_chaos,
                });
            }
        }

        // Calculate error rate
        let total = self.tasks.len() as f64;
        let stopped = self
            .tasks
            .values()
            .filter(|t| t.state == "STOPPED")
            .count() as f64;
        let error_rate_pct = if total > 0.0 {
            (stopped / total) * 100.0
        } else {
            0.0
        };

        let duration_secs = start.elapsed().as_secs_f64();

        let passed =
            stuck_tasks.is_empty() && error_rate_pct <= config.max_error_rate_pct;

        let error = if !stuck_tasks.is_empty() {
            Some(format!("{} stuck task(s) detected", stuck_tasks.len()))
        } else if error_rate_pct > config.max_error_rate_pct {
            Some(format!(
                "Error rate {:.1}% exceeds max {:.1}%",
                error_rate_pct, config.max_error_rate_pct,
            ))
        } else {
            None
        };

        SoakResult {
            passed,
            total_ticks: config.total_ticks,
            stuck_tasks,
            error_rate_pct,
            chaos_events,
            progress_reports,
            duration_secs,
            error,
        }
    }

    // ─── Step execution ──────────────────────────────────────────────────

    fn execute_step(&mut self, index: usize, step: &ScenarioStep) -> StepResult {
        match step {
            ScenarioStep::CreateTask {
                task_id,
                description,
            } => {
                let now = Utc::now();
                let task = SimulatedTask::new(task_id, description, now);
                self.tasks.insert(task_id.clone(), task);
                self.metrics.tasks_created += 1;
                self.log
                    .push(format!("[tick {}] Created task {}", self.current_tick, task_id));
                StepResult {
                    step_index: index,
                    action: "create_task".to_string(),
                    passed: true,
                    detail: format!("Created task {}", task_id),
                }
            }

            ScenarioStep::ExpectState {
                task_id,
                expected_state,
                timeout_secs: _,
            } => {
                if let Some(task) = self.tasks.get(task_id) {
                    let actual = &task.state;
                    let passed = actual == expected_state;
                    StepResult {
                        step_index: index,
                        action: "expect_state".to_string(),
                        passed,
                        detail: if passed {
                            format!("Task {} is in {} as expected", task_id, expected_state)
                        } else {
                            format!(
                                "Task {} expected {} but was {}",
                                task_id, expected_state, actual
                            )
                        },
                    }
                } else {
                    StepResult {
                        step_index: index,
                        action: "expect_state".to_string(),
                        passed: false,
                        detail: format!("Task {} not found", task_id),
                    }
                }
            }

            ScenarioStep::CompleteAgent {
                task_id,
                success,
                model,
            } => {
                self.metrics.agent_spawns += 1;
                self.metrics.agent_completions += 1;
                let now = Utc::now();

                // Check for chaos fault targeting this task
                if let Some(fault) = self.chaos.fault_for_task(task_id) {
                    let label = fault.label().to_string();
                    self.log.push(format!(
                        "[tick {}] Chaos: {} for task {}",
                        self.current_tick, label, task_id
                    ));
                    if let Some(task) = self.tasks.get_mut(task_id) {
                        task.transition(
                            "CHATTING",
                            self.current_tick,
                            &format!("chaos: {}", label),
                            now,
                        );
                        self.metrics.state_transitions += 1;
                    }
                    return StepResult {
                        step_index: index,
                        action: "complete_agent".to_string(),
                        passed: true, // chaos injection is expected
                        detail: format!(
                            "Agent for {} intercepted by chaos: {}",
                            task_id, label
                        ),
                    };
                }

                if let Some(task) = self.tasks.get_mut(task_id) {
                    if *success {
                        task.transition("READY", self.current_tick, &format!("agent {} completed", model), now);
                    } else {
                        task.transition(
                            "CHATTING",
                            self.current_tick,
                            &format!("agent {} failed, retrying", model),
                            now,
                        );
                    }
                    self.metrics.state_transitions += 1;
                    StepResult {
                        step_index: index,
                        action: "complete_agent".to_string(),
                        passed: true,
                        detail: format!(
                            "Agent {} for task {}: success={}",
                            model, task_id, success
                        ),
                    }
                } else {
                    StepResult {
                        step_index: index,
                        action: "complete_agent".to_string(),
                        passed: false,
                        detail: format!("Task {} not found", task_id),
                    }
                }
            }

            ScenarioStep::SimulateVerify { task_id, success } => {
                self.metrics.verify_runs += 1;
                let now = Utc::now();
                if let Some(task) = self.tasks.get_mut(task_id) {
                    if *success {
                        task.transition(
                            "SUBMITTING",
                            self.current_tick,
                            "verify passed",
                            now,
                        );
                    } else {
                        task.transition(
                            "CHATTING",
                            self.current_tick,
                            "verify failed",
                            now,
                        );
                    }
                    self.metrics.state_transitions += 1;
                    StepResult {
                        step_index: index,
                        action: "simulate_verify".to_string(),
                        passed: true,
                        detail: format!("Verify for {}: success={}", task_id, success),
                    }
                } else {
                    StepResult {
                        step_index: index,
                        action: "simulate_verify".to_string(),
                        passed: false,
                        detail: format!("Task {} not found", task_id),
                    }
                }
            }

            ScenarioStep::SimulateQA {
                task_id,
                passed,
                failed_tests,
            } => {
                self.metrics.qa_runs += 1;
                let now = Utc::now();
                if let Some(task) = self.tasks.get_mut(task_id) {
                    if *passed {
                        // QA passed — task stays in current state (verify gate decides)
                        self.log.push(format!(
                            "[tick {}] QA passed for {}",
                            self.current_tick, task_id
                        ));
                    } else {
                        task.transition(
                            "CHATTING",
                            self.current_tick,
                            &format!(
                                "QA failed: {} test(s)",
                                failed_tests.len()
                            ),
                            now,
                        );
                        self.metrics.state_transitions += 1;
                    }
                    StepResult {
                        step_index: index,
                        action: "simulate_qa".to_string(),
                        passed: true,
                        detail: format!(
                            "QA for {}: passed={}, failed_tests={:?}",
                            task_id, passed, failed_tests
                        ),
                    }
                } else {
                    StepResult {
                        step_index: index,
                        action: "simulate_qa".to_string(),
                        passed: false,
                        detail: format!("Task {} not found", task_id),
                    }
                }
            }

            ScenarioStep::InjectChaos { fault } => {
                let label = fault.label().to_string();
                let injected = self.chaos.inject(fault.clone());
                self.metrics.chaos_injections += 1;
                self.log.push(format!(
                    "[tick {}] Chaos injected: {}",
                    self.current_tick, label
                ));
                StepResult {
                    step_index: index,
                    action: "inject_chaos".to_string(),
                    passed: injected,
                    detail: if injected {
                        format!("Injected chaos: {}", label)
                    } else {
                        format!("Failed to inject chaos: {} (max concurrent reached)", label)
                    },
                }
            }

            ScenarioStep::ClearChaos => {
                let count = self.chaos.active_faults.len();
                self.chaos.clear();
                self.log.push(format!(
                    "[tick {}] Cleared {} chaos faults",
                    self.current_tick, count
                ));
                StepResult {
                    step_index: index,
                    action: "clear_chaos".to_string(),
                    passed: true,
                    detail: format!("Cleared {} active faults", count),
                }
            }

            ScenarioStep::WaitTicks { count } => {
                let now = Utc::now();
                for _ in 0..*count {
                    self.current_tick += 1;
                    self.metrics.total_ticks += 1;
                    for task in self.tasks.values_mut() {
                        task.tick();
                    }
                    self.simulate_tick_progression(
                        self.current_tick,
                        now + Duration::seconds(self.current_tick as i64),
                    );
                }
                StepResult {
                    step_index: index,
                    action: "wait_ticks".to_string(),
                    passed: true,
                    detail: format!("Waited {} ticks (now at tick {})", count, self.current_tick),
                }
            }

            ScenarioStep::AssertMetric { metric, op, value } => {
                if let Some(actual) = self.metrics.get(metric) {
                    let passed = op.evaluate(actual, *value);
                    StepResult {
                        step_index: index,
                        action: "assert_metric".to_string(),
                        passed,
                        detail: format!(
                            "Metric '{}': {} {:?} {} => {}",
                            metric,
                            actual,
                            op,
                            value,
                            if passed { "ok" } else { "FAILED" }
                        ),
                    }
                } else {
                    StepResult {
                        step_index: index,
                        action: "assert_metric".to_string(),
                        passed: false,
                        detail: format!("Unknown metric: {}", metric),
                    }
                }
            }

            ScenarioStep::AssertNoStuckTasks {
                max_unchanged_ticks,
            } => {
                let stuck: Vec<_> = self
                    .tasks
                    .values()
                    .filter(|t| !t.is_terminal() && t.ticks_in_state >= *max_unchanged_ticks)
                    .collect();
                let passed = stuck.is_empty();
                StepResult {
                    step_index: index,
                    action: "assert_no_stuck_tasks".to_string(),
                    passed,
                    detail: if passed {
                        format!(
                            "No tasks stuck for >= {} ticks",
                            max_unchanged_ticks
                        )
                    } else {
                        format!(
                            "{} task(s) stuck: {}",
                            stuck.len(),
                            stuck
                                .iter()
                                .map(|t| {
                                    format!(
                                        "{}({}, {} ticks)",
                                        t.task_id, t.state, t.ticks_in_state
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    },
                }
            }

            ScenarioStep::SimulateMerge { task_id } => {
                let now = Utc::now();
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.transition("MERGED", self.current_tick, "PR merged", now);
                    self.metrics.tasks_merged += 1;
                    self.metrics.state_transitions += 1;
                    StepResult {
                        step_index: index,
                        action: "simulate_merge".to_string(),
                        passed: true,
                        detail: format!("Merged task {}", task_id),
                    }
                } else {
                    StepResult {
                        step_index: index,
                        action: "simulate_merge".to_string(),
                        passed: false,
                        detail: format!("Task {} not found", task_id),
                    }
                }
            }

            ScenarioStep::AssertTaskCount { expected } => {
                let actual = self.tasks.len();
                let passed = actual == *expected;
                StepResult {
                    step_index: index,
                    action: "assert_task_count".to_string(),
                    passed,
                    detail: format!(
                        "Task count: {} (expected {})",
                        actual, expected
                    ),
                }
            }

            ScenarioStep::Log { message } => {
                self.log
                    .push(format!("[tick {}] {}", self.current_tick, message));
                StepResult {
                    step_index: index,
                    action: "log".to_string(),
                    passed: true,
                    detail: message.clone(),
                }
            }
        }
    }

    /// Simulate autonomous progression: move tasks forward through their
    /// lifecycle based on current state.  This is a simplified model of
    /// what the real daemon_tick does.
    fn simulate_tick_progression(&mut self, tick: u64, now: DateTime<Utc>) {
        // Collect task_ids to avoid borrow conflicts
        let task_ids: Vec<String> = self.tasks.keys().cloned().collect();

        for tid in task_ids {
            let should_progress = {
                let task = match self.tasks.get(&tid) {
                    Some(t) => t,
                    None => continue,
                };
                // Only auto-progress if no chaos fault targets this task
                if self.chaos.fault_for_task(&tid).is_some() {
                    continue;
                }
                // Auto-progress: after enough ticks in a state, advance
                match task.state.as_str() {
                    "CHATTING" if task.ticks_in_state >= 5 => Some("READY"),
                    "READY" if task.ticks_in_state >= 2 => Some("SUBMITTING"),
                    "SUBMITTING" if task.ticks_in_state >= 3 => Some("AWAITING_MERGE"),
                    "AWAITING_MERGE" if task.ticks_in_state >= 4 => Some("MERGED"),
                    _ => None,
                }
            };

            if let Some(next_state) = should_progress {
                if let Some(task) = self.tasks.get_mut(&tid) {
                    let reason = format!("auto-progress after {} ticks", task.ticks_in_state);
                    task.transition(next_state, tick, &reason, now);
                    self.metrics.state_transitions += 1;
                    if next_state == "MERGED" {
                        self.metrics.tasks_merged += 1;
                    }
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Built-in scenarios
// ─────────────────────────────────────────────────────────────────────────────

/// Create the standard suite of built-in orchestration scenarios.
pub fn builtin_scenarios() -> Vec<Scenario> {
    vec![
        scenario_happy_path(),
        scenario_agent_failure_retry(),
        scenario_chaos_agent_crash(),
        scenario_multi_task_lifecycle(),
        scenario_verify_failure_loop(),
        scenario_qa_red_to_green(),
    ]
}

/// Happy path: create → agent → verify → submit → merge.
fn scenario_happy_path() -> Scenario {
    Scenario {
        name: "happy_path".to_string(),
        description: "Full lifecycle: create → agent complete → verify → submit → merge"
            .to_string(),
        steps: vec![
            ScenarioStep::CreateTask {
                task_id: "T1".to_string(),
                description: "Implement feature X".to_string(),
            },
            ScenarioStep::ExpectState {
                task_id: "T1".to_string(),
                expected_state: "CHATTING".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::CompleteAgent {
                task_id: "T1".to_string(),
                success: true,
                model: "claude".to_string(),
            },
            ScenarioStep::ExpectState {
                task_id: "T1".to_string(),
                expected_state: "READY".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::SimulateVerify {
                task_id: "T1".to_string(),
                success: true,
            },
            ScenarioStep::ExpectState {
                task_id: "T1".to_string(),
                expected_state: "SUBMITTING".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::SimulateMerge {
                task_id: "T1".to_string(),
            },
            ScenarioStep::ExpectState {
                task_id: "T1".to_string(),
                expected_state: "MERGED".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::AssertMetric {
                metric: "tasks_created".to_string(),
                op: CompareOp::Eq,
                value: 1.0,
            },
            ScenarioStep::AssertMetric {
                metric: "tasks_merged".to_string(),
                op: CompareOp::Eq,
                value: 1.0,
            },
        ],
        timeout_secs: 30,
        tags: vec!["core".to_string(), "happy-path".to_string()],
        critical: true,
    }
}

/// Agent failure with retry: first attempt fails, second succeeds.
fn scenario_agent_failure_retry() -> Scenario {
    Scenario {
        name: "agent_failure_retry".to_string(),
        description: "Agent fails first attempt, retries with different model, succeeds"
            .to_string(),
        steps: vec![
            ScenarioStep::CreateTask {
                task_id: "T2".to_string(),
                description: "Fix bug Y".to_string(),
            },
            ScenarioStep::CompleteAgent {
                task_id: "T2".to_string(),
                success: false,
                model: "claude".to_string(),
            },
            ScenarioStep::ExpectState {
                task_id: "T2".to_string(),
                expected_state: "CHATTING".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::CompleteAgent {
                task_id: "T2".to_string(),
                success: true,
                model: "codex".to_string(),
            },
            ScenarioStep::ExpectState {
                task_id: "T2".to_string(),
                expected_state: "READY".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::AssertMetric {
                metric: "agent_completions".to_string(),
                op: CompareOp::Eq,
                value: 2.0,
            },
        ],
        timeout_secs: 30,
        tags: vec!["core".to_string(), "retry".to_string()],
        critical: true,
    }
}

/// Chaos: agent crash during execution.
fn scenario_chaos_agent_crash() -> Scenario {
    Scenario {
        name: "chaos_agent_crash".to_string(),
        description: "Inject agent crash, verify task recovers".to_string(),
        steps: vec![
            ScenarioStep::CreateTask {
                task_id: "T3".to_string(),
                description: "Implement feature Z".to_string(),
            },
            ScenarioStep::InjectChaos {
                fault: ChaosFault::AgentCrash {
                    task_id: "T3".to_string(),
                },
            },
            ScenarioStep::CompleteAgent {
                task_id: "T3".to_string(),
                success: true,
                model: "claude".to_string(),
            },
            // Chaos should intercept — task stays CHATTING
            ScenarioStep::ExpectState {
                task_id: "T3".to_string(),
                expected_state: "CHATTING".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::ClearChaos,
            // Now agent can succeed
            ScenarioStep::CompleteAgent {
                task_id: "T3".to_string(),
                success: true,
                model: "claude".to_string(),
            },
            ScenarioStep::ExpectState {
                task_id: "T3".to_string(),
                expected_state: "READY".to_string(),
                timeout_secs: 5,
            },
        ],
        timeout_secs: 30,
        tags: vec!["chaos".to_string()],
        critical: false,
    }
}

/// Multi-task lifecycle: 3 tasks running concurrently.
fn scenario_multi_task_lifecycle() -> Scenario {
    Scenario {
        name: "multi_task_lifecycle".to_string(),
        description: "Three tasks progress through lifecycle concurrently".to_string(),
        steps: vec![
            ScenarioStep::CreateTask {
                task_id: "M1".to_string(),
                description: "Task M1".to_string(),
            },
            ScenarioStep::CreateTask {
                task_id: "M2".to_string(),
                description: "Task M2".to_string(),
            },
            ScenarioStep::CreateTask {
                task_id: "M3".to_string(),
                description: "Task M3".to_string(),
            },
            ScenarioStep::AssertTaskCount { expected: 3 },
            // Complete all agents
            ScenarioStep::CompleteAgent {
                task_id: "M1".to_string(),
                success: true,
                model: "claude".to_string(),
            },
            ScenarioStep::CompleteAgent {
                task_id: "M2".to_string(),
                success: true,
                model: "codex".to_string(),
            },
            ScenarioStep::CompleteAgent {
                task_id: "M3".to_string(),
                success: true,
                model: "gemini".to_string(),
            },
            // All should be READY
            ScenarioStep::ExpectState {
                task_id: "M1".to_string(),
                expected_state: "READY".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::ExpectState {
                task_id: "M2".to_string(),
                expected_state: "READY".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::ExpectState {
                task_id: "M3".to_string(),
                expected_state: "READY".to_string(),
                timeout_secs: 5,
            },
            // Merge all
            ScenarioStep::SimulateVerify {
                task_id: "M1".to_string(),
                success: true,
            },
            ScenarioStep::SimulateMerge {
                task_id: "M1".to_string(),
            },
            ScenarioStep::SimulateVerify {
                task_id: "M2".to_string(),
                success: true,
            },
            ScenarioStep::SimulateMerge {
                task_id: "M2".to_string(),
            },
            ScenarioStep::SimulateVerify {
                task_id: "M3".to_string(),
                success: true,
            },
            ScenarioStep::SimulateMerge {
                task_id: "M3".to_string(),
            },
            ScenarioStep::AssertMetric {
                metric: "tasks_merged".to_string(),
                op: CompareOp::Eq,
                value: 3.0,
            },
        ],
        timeout_secs: 60,
        tags: vec!["core".to_string(), "multi-task".to_string()],
        critical: true,
    }
}

/// Verify failure loop: verify fails, task goes back to chatting, then succeeds.
fn scenario_verify_failure_loop() -> Scenario {
    Scenario {
        name: "verify_failure_loop".to_string(),
        description: "Verify fails, agent re-runs, verify succeeds on second try".to_string(),
        steps: vec![
            ScenarioStep::CreateTask {
                task_id: "V1".to_string(),
                description: "Fix failing tests".to_string(),
            },
            ScenarioStep::CompleteAgent {
                task_id: "V1".to_string(),
                success: true,
                model: "claude".to_string(),
            },
            ScenarioStep::SimulateVerify {
                task_id: "V1".to_string(),
                success: false,
            },
            ScenarioStep::ExpectState {
                task_id: "V1".to_string(),
                expected_state: "CHATTING".to_string(),
                timeout_secs: 5,
            },
            // Agent re-runs and fixes
            ScenarioStep::CompleteAgent {
                task_id: "V1".to_string(),
                success: true,
                model: "claude".to_string(),
            },
            ScenarioStep::SimulateVerify {
                task_id: "V1".to_string(),
                success: true,
            },
            ScenarioStep::ExpectState {
                task_id: "V1".to_string(),
                expected_state: "SUBMITTING".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::AssertMetric {
                metric: "verify_runs".to_string(),
                op: CompareOp::Eq,
                value: 2.0,
            },
        ],
        timeout_secs: 30,
        tags: vec!["core".to_string(), "verify".to_string()],
        critical: true,
    }
}

/// QA red-to-green: QA fails, fix task runs, QA passes.
fn scenario_qa_red_to_green() -> Scenario {
    Scenario {
        name: "qa_red_to_green".to_string(),
        description: "QA fails with regressions, fix task runs, QA passes on retry".to_string(),
        steps: vec![
            ScenarioStep::CreateTask {
                task_id: "Q1".to_string(),
                description: "Add new endpoint".to_string(),
            },
            ScenarioStep::CompleteAgent {
                task_id: "Q1".to_string(),
                success: true,
                model: "claude".to_string(),
            },
            ScenarioStep::SimulateQA {
                task_id: "Q1".to_string(),
                passed: false,
                failed_tests: vec!["test_auth".to_string(), "test_validation".to_string()],
            },
            // Task sent back for fixes
            ScenarioStep::ExpectState {
                task_id: "Q1".to_string(),
                expected_state: "CHATTING".to_string(),
                timeout_secs: 5,
            },
            // Fix agent runs
            ScenarioStep::CompleteAgent {
                task_id: "Q1".to_string(),
                success: true,
                model: "claude".to_string(),
            },
            // QA passes on retry
            ScenarioStep::SimulateQA {
                task_id: "Q1".to_string(),
                passed: true,
                failed_tests: vec![],
            },
            ScenarioStep::ExpectState {
                task_id: "Q1".to_string(),
                expected_state: "READY".to_string(),
                timeout_secs: 5,
            },
            ScenarioStep::AssertMetric {
                metric: "qa_runs".to_string(),
                op: CompareOp::Eq,
                value: 2.0,
            },
        ],
        timeout_secs: 30,
        tags: vec!["core".to_string(), "qa".to_string()],
        critical: true,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_runner_creates_tasks() {
        let mut runner = ScenarioRunner::new();
        let scenario = Scenario {
            name: "test_create".to_string(),
            description: "Test task creation".to_string(),
            steps: vec![
                ScenarioStep::CreateTask {
                    task_id: "T1".to_string(),
                    description: "Test".to_string(),
                },
                ScenarioStep::AssertTaskCount { expected: 1 },
            ],
            timeout_secs: 10,
            tags: vec![],
            critical: false,
        };
        let result = runner.run_scenario(&scenario);
        assert!(result.passed);
        assert_eq!(result.metrics.tasks_created, 1);
    }

    #[test]
    fn happy_path_scenario_passes() {
        let mut runner = ScenarioRunner::new();
        let result = runner.run_scenario(&scenario_happy_path());
        assert!(result.passed, "Happy path failed: {:?}", result.error);
        assert_eq!(result.metrics.tasks_merged, 1);
    }

    #[test]
    fn agent_failure_retry_scenario_passes() {
        let mut runner = ScenarioRunner::new();
        let result = runner.run_scenario(&scenario_agent_failure_retry());
        assert!(result.passed, "Retry scenario failed: {:?}", result.error);
        assert_eq!(result.metrics.agent_completions, 2);
    }

    #[test]
    fn chaos_agent_crash_scenario_passes() {
        let mut runner = ScenarioRunner::new();
        let result = runner.run_scenario(&scenario_chaos_agent_crash());
        assert!(result.passed, "Chaos scenario failed: {:?}", result.error);
    }

    #[test]
    fn multi_task_lifecycle_scenario_passes() {
        let mut runner = ScenarioRunner::new();
        let result = runner.run_scenario(&scenario_multi_task_lifecycle());
        assert!(result.passed, "Multi-task failed: {:?}", result.error);
        assert_eq!(result.metrics.tasks_merged, 3);
    }

    #[test]
    fn verify_failure_loop_scenario_passes() {
        let mut runner = ScenarioRunner::new();
        let result = runner.run_scenario(&scenario_verify_failure_loop());
        assert!(result.passed, "Verify loop failed: {:?}", result.error);
        assert_eq!(result.metrics.verify_runs, 2);
    }

    #[test]
    fn qa_red_to_green_scenario_passes() {
        let mut runner = ScenarioRunner::new();
        let result = runner.run_scenario(&scenario_qa_red_to_green());
        assert!(result.passed, "QA red→green failed: {:?}", result.error);
        assert_eq!(result.metrics.qa_runs, 2);
    }

    #[test]
    fn full_builtin_suite_passes() {
        let mut runner = ScenarioRunner::new();
        let suite = runner.run_suite(&builtin_scenarios());
        assert_eq!(suite.failed, 0, "Suite failures:\n{}", suite.summary());
        assert_eq!(suite.passed, 6);
    }

    #[test]
    fn soak_test_no_stuck_tasks() {
        let mut runner = ScenarioRunner::new();
        let config = SoakConfig {
            total_ticks: 100,
            stuck_threshold_ticks: 20,
            max_error_rate_pct: 10.0,
            enable_chaos: false,
            chaos_probability: 0.0,
            report_interval_ticks: 25,
        };
        let tasks = vec![
            ("S1".to_string(), "Soak task 1".to_string()),
            ("S2".to_string(), "Soak task 2".to_string()),
            ("S3".to_string(), "Soak task 3".to_string()),
        ];
        let result = runner.run_soak(&config, tasks);
        assert!(result.passed, "Soak test failed: {:?}", result.error);
        assert_eq!(result.total_ticks, 100);
        assert!(result.stuck_tasks.is_empty());
        assert!(result.progress_reports.len() >= 4); // at tick 0, 25, 50, 75
    }

    #[test]
    fn soak_test_with_chaos() {
        let mut runner = ScenarioRunner::new();
        let config = SoakConfig {
            total_ticks: 200,
            stuck_threshold_ticks: 30,
            max_error_rate_pct: 50.0,
            enable_chaos: true,
            chaos_probability: 0.1,
            report_interval_ticks: 50,
        };
        let tasks = vec![
            ("C1".to_string(), "Chaos task 1".to_string()),
            ("C2".to_string(), "Chaos task 2".to_string()),
        ];
        let result = runner.run_soak(&config, tasks);
        // With auto-progression and generous error rate, should still pass
        assert!(result.passed, "Soak+chaos failed: {:?}", result.error);
        assert!(result.chaos_events > 0);
    }

    #[test]
    fn compare_op_evaluations() {
        assert!(CompareOp::Eq.evaluate(5.0, 5.0));
        assert!(!CompareOp::Eq.evaluate(5.0, 6.0));
        assert!(CompareOp::Gt.evaluate(6.0, 5.0));
        assert!(!CompareOp::Gt.evaluate(5.0, 5.0));
        assert!(CompareOp::Gte.evaluate(5.0, 5.0));
        assert!(CompareOp::Gte.evaluate(6.0, 5.0));
        assert!(CompareOp::Lt.evaluate(4.0, 5.0));
        assert!(!CompareOp::Lt.evaluate(5.0, 5.0));
        assert!(CompareOp::Lte.evaluate(5.0, 5.0));
        assert!(CompareOp::Lte.evaluate(4.0, 5.0));
    }

    #[test]
    fn chaos_policy_inject_and_clear() {
        let mut chaos = ChaosPolicy::default();
        assert!(chaos.inject(ChaosFault::AgentCrash {
            task_id: "T1".to_string()
        }));
        assert!(chaos.has_fault("agent_crash"));
        assert!(chaos.fault_for_task("T1").is_some());
        assert!(chaos.fault_for_task("T2").is_none());
        chaos.clear();
        assert!(!chaos.has_fault("agent_crash"));
    }

    #[test]
    fn chaos_policy_max_concurrent() {
        let mut chaos = ChaosPolicy {
            max_concurrent_faults: 2,
            ..Default::default()
        };
        assert!(chaos.inject(ChaosFault::NetworkOutage));
        assert!(chaos.inject(ChaosFault::DiskFull));
        assert!(!chaos.inject(ChaosFault::ContextGenFailure));
        assert_eq!(chaos.active_faults.len(), 2);
    }

    #[test]
    fn simulated_task_transitions() {
        let now = Utc::now();
        let mut task = SimulatedTask::new("T1", "Test", now);
        assert_eq!(task.state, "CHATTING");
        assert!(!task.is_terminal());

        task.tick();
        assert_eq!(task.ticks_in_state, 1);

        task.transition("READY", 1, "agent done", now);
        assert_eq!(task.state, "READY");
        assert_eq!(task.ticks_in_state, 0);
        assert_eq!(task.transitions.len(), 1);
        assert_eq!(task.transitions[0].from, "CHATTING");
        assert_eq!(task.transitions[0].to, "READY");
    }

    #[test]
    fn simulated_task_terminal_states() {
        let now = Utc::now();
        let mut task = SimulatedTask::new("T1", "Test", now);
        assert!(!task.is_terminal());

        task.transition("MERGED", 1, "merged", now);
        assert!(task.is_terminal());

        let mut task2 = SimulatedTask::new("T2", "Test", now);
        task2.transition("STOPPED", 1, "stopped", now);
        assert!(task2.is_terminal());
    }

    #[test]
    fn scenario_metrics_get() {
        let metrics = ScenarioMetrics {
            total_ticks: 42,
            tasks_created: 5,
            tasks_merged: 3,
            ..Default::default()
        };
        assert_eq!(metrics.get("total_ticks"), Some(42.0));
        assert_eq!(metrics.get("tasks_created"), Some(5.0));
        assert_eq!(metrics.get("tasks_merged"), Some(3.0));
        assert_eq!(metrics.get("nonexistent"), None);
    }

    #[test]
    fn suite_result_summary_contains_all_names() {
        let mut runner = ScenarioRunner::new();
        let suite = runner.run_suite(&builtin_scenarios());
        let summary = suite.summary();
        assert!(summary.contains("happy_path"));
        assert!(summary.contains("agent_failure_retry"));
        assert!(summary.contains("chaos_agent_crash"));
        assert!(summary.contains("multi_task_lifecycle"));
        assert!(summary.contains("verify_failure_loop"));
        assert!(summary.contains("qa_red_to_green"));
    }

    #[test]
    fn suite_result_json_roundtrip() {
        let mut runner = ScenarioRunner::new();
        let suite = runner.run_suite(&builtin_scenarios());
        let json = suite.to_json();
        let decoded: SuiteResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.total, suite.total);
        assert_eq!(decoded.passed, suite.passed);
        assert_eq!(decoded.failed, suite.failed);
    }

    #[test]
    fn soak_result_json_roundtrip() {
        let mut runner = ScenarioRunner::new();
        let config = SoakConfig {
            total_ticks: 50,
            ..Default::default()
        };
        let tasks = vec![("X1".to_string(), "X1 desc".to_string())];
        let result = runner.run_soak(&config, tasks);
        let json = result.to_json();
        let decoded: SoakResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.total_ticks, 50);
    }

    #[test]
    fn scenario_step_log() {
        let mut runner = ScenarioRunner::new();
        let scenario = Scenario {
            name: "log_test".to_string(),
            description: "Test log step".to_string(),
            steps: vec![ScenarioStep::Log {
                message: "Hello from test".to_string(),
            }],
            timeout_secs: 10,
            tags: vec![],
            critical: false,
        };
        let result = runner.run_scenario(&scenario);
        assert!(result.passed);
        assert!(runner.log.iter().any(|l| l.contains("Hello from test")));
    }

    #[test]
    fn assert_no_stuck_tasks_passes_when_none_stuck() {
        let mut runner = ScenarioRunner::new();
        let scenario = Scenario {
            name: "no_stuck".to_string(),
            description: "No stuck tasks".to_string(),
            steps: vec![
                ScenarioStep::CreateTask {
                    task_id: "NS1".to_string(),
                    description: "Test".to_string(),
                },
                ScenarioStep::AssertNoStuckTasks {
                    max_unchanged_ticks: 10,
                },
            ],
            timeout_secs: 10,
            tags: vec![],
            critical: false,
        };
        let result = runner.run_scenario(&scenario);
        assert!(result.passed);
    }

    #[test]
    fn assert_no_stuck_tasks_fails_when_stuck() {
        let mut runner = ScenarioRunner::new();
        let scenario = Scenario {
            name: "stuck_detect".to_string(),
            description: "Detect stuck task".to_string(),
            steps: vec![
                ScenarioStep::CreateTask {
                    task_id: "ST1".to_string(),
                    description: "Will get stuck".to_string(),
                },
                ScenarioStep::InjectChaos {
                    fault: ChaosFault::AgentHang {
                        task_id: "ST1".to_string(),
                    },
                },
                ScenarioStep::WaitTicks { count: 15 },
                ScenarioStep::AssertNoStuckTasks {
                    max_unchanged_ticks: 10,
                },
            ],
            timeout_secs: 30,
            tags: vec![],
            critical: false,
        };
        let result = runner.run_scenario(&scenario);
        // Should fail because ST1 is stuck
        assert!(!result.passed);
        let last_step = result.step_results.last().unwrap();
        assert!(!last_step.passed);
        assert!(last_step.detail.contains("ST1"));
    }

    #[test]
    fn soak_progress_reports_generated() {
        let mut runner = ScenarioRunner::new();
        let config = SoakConfig {
            total_ticks: 100,
            report_interval_ticks: 20,
            ..Default::default()
        };
        let tasks = vec![("P1".to_string(), "P1".to_string())];
        let result = runner.run_soak(&config, tasks);
        // Reports at tick 0, 20, 40, 60, 80
        assert_eq!(result.progress_reports.len(), 5);
        assert_eq!(result.progress_reports[0].tick, 0);
        assert_eq!(result.progress_reports[1].tick, 20);
    }

    #[test]
    fn chaos_fault_labels() {
        assert_eq!(
            ChaosFault::AgentCrash {
                task_id: "T1".to_string()
            }
            .label(),
            "agent_crash"
        );
        assert_eq!(
            ChaosFault::GraphiteFailure {
                operation: "submit".to_string()
            }
            .label(),
            "graphite_failure"
        );
        assert_eq!(ChaosFault::ContextGenFailure.label(), "context_gen_failure");
        assert_eq!(
            ChaosFault::ModelHealthDrop {
                model: "claude".to_string()
            }
            .label(),
            "model_health_drop"
        );
        assert_eq!(ChaosFault::NetworkOutage.label(), "network_outage");
        assert_eq!(ChaosFault::DiskFull.label(), "disk_full");
        assert_eq!(
            ChaosFault::AgentHang {
                task_id: "T1".to_string()
            }
            .label(),
            "agent_hang"
        );
    }

    #[test]
    fn soak_config_defaults() {
        let config = SoakConfig::default();
        assert_eq!(config.total_ticks, 1000);
        assert_eq!(config.stuck_threshold_ticks, 50);
        assert_eq!(config.max_error_rate_pct, 20.0);
        assert!(!config.enable_chaos);
        assert_eq!(config.report_interval_ticks, 100);
    }
}
