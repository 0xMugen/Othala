//! Orchestration Metrics — tracks per-task and per-agent effectiveness.
//!
//! Provides:
//! - Agent routing decisions and outcomes
//! - Time-to-merge tracking
//! - E2E pass rates
//! - Error recovery success rates

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::agent_dispatch::AgentRole;
use crate::e2e_tester::E2EResult;
use crate::problem_classifier::ErrorClass;

// ─────────────────────────────────────────────────────────────────────────────
// Task Metrics
// ─────────────────────────────────────────────────────────────────────────────

/// Metrics for a single task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMetrics {
    pub task_id: String,
    pub repo_id: String,
    /// Agent roles used in order of attempts
    pub agents_used: Vec<String>,
    /// Time when task started
    pub started_at: DateTime<Utc>,
    /// Time when task was merged (if applicable)
    pub merged_at: Option<DateTime<Utc>>,
    /// Total attempts
    pub total_attempts: u32,
    /// Whether task succeeded
    pub succeeded: bool,
    /// E2E status
    pub e2e_passed: Option<bool>,
    /// Error classes encountered
    pub error_classes: Vec<String>,
    /// Time spent in each agent (seconds)
    pub agent_durations: HashMap<String, f64>,
}

impl TaskMetrics {
    pub fn new(task_id: &str, repo_id: &str) -> Self {
        Self {
            task_id: task_id.to_string(),
            repo_id: repo_id.to_string(),
            agents_used: Vec::new(),
            started_at: Utc::now(),
            merged_at: None,
            total_attempts: 0,
            succeeded: false,
            e2e_passed: None,
            error_classes: Vec::new(),
            agent_durations: HashMap::new(),
        }
    }

    /// Time to merge in hours (if merged).
    pub fn time_to_merge_hours(&self) -> Option<f64> {
        self.merged_at.map(|m| {
            let duration = m.signed_duration_since(self.started_at);
            duration.num_minutes() as f64 / 60.0
        })
    }

    /// Record an agent attempt.
    pub fn record_attempt(&mut self, role: AgentRole, duration_secs: f64) {
        let role_name = role.name().to_string();
        self.agents_used.push(role_name.clone());
        self.total_attempts += 1;
        *self.agent_durations.entry(role_name).or_insert(0.0) += duration_secs;
    }

    /// Record an error.
    pub fn record_error(&mut self, class: ErrorClass) {
        self.error_classes.push(class.to_string());
    }

    /// Mark task as merged.
    pub fn mark_merged(&mut self) {
        self.merged_at = Some(Utc::now());
        self.succeeded = true;
    }

    /// Record E2E result.
    pub fn record_e2e(&mut self, passed: bool) {
        self.e2e_passed = Some(passed);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Agent Metrics
// ─────────────────────────────────────────────────────────────────────────────

/// Aggregated metrics for an agent role.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentMetrics {
    pub role: String,
    pub total_tasks: u64,
    pub successful_tasks: u64,
    pub failed_tasks: u64,
    pub total_duration_secs: f64,
    pub avg_duration_secs: f64,
    /// Success rate when used as first agent
    pub first_attempt_success_rate: f64,
    /// Success rate when used as recovery agent
    pub recovery_success_rate: f64,
    /// Tasks where this agent was used
    pub first_attempts: u64,
    pub recovery_attempts: u64,
    pub first_attempt_successes: u64,
    pub recovery_successes: u64,
}

impl AgentMetrics {
    pub fn new(role: &str) -> Self {
        Self {
            role: role.to_string(),
            ..Default::default()
        }
    }

    /// Record a task outcome.
    pub fn record_task(&mut self, success: bool, duration_secs: f64, is_recovery: bool) {
        self.total_tasks += 1;
        self.total_duration_secs += duration_secs;

        if success {
            self.successful_tasks += 1;
        } else {
            self.failed_tasks += 1;
        }

        if is_recovery {
            self.recovery_attempts += 1;
            if success {
                self.recovery_successes += 1;
            }
        } else {
            self.first_attempts += 1;
            if success {
                self.first_attempt_successes += 1;
            }
        }

        // Recalculate averages
        self.avg_duration_secs = self.total_duration_secs / self.total_tasks as f64;
        self.first_attempt_success_rate = if self.first_attempts > 0 {
            self.first_attempt_successes as f64 / self.first_attempts as f64
        } else {
            0.0
        };
        self.recovery_success_rate = if self.recovery_attempts > 0 {
            self.recovery_successes as f64 / self.recovery_attempts as f64
        } else {
            0.0
        };
    }

    /// Overall success rate.
    pub fn success_rate(&self) -> f64 {
        if self.total_tasks == 0 {
            0.0
        } else {
            self.successful_tasks as f64 / self.total_tasks as f64
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Orchestration Snapshot
// ─────────────────────────────────────────────────────────────────────────────

/// A point-in-time snapshot of orchestration metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationSnapshot {
    pub timestamp: DateTime<Utc>,
    /// State counts
    pub state_counts: HashMap<String, u32>,
    /// Verify results
    pub verify_passed: u32,
    pub verify_failed: u32,
    /// E2E results
    pub e2e_passed: u32,
    pub e2e_failed: u32,
    pub e2e_skipped: u32,
    /// Graphite errors
    pub graphite_errors: u32,
    /// Merges completed
    pub merges: u32,
    /// Tasks stopped
    pub stops: u32,
    /// Agent dispatch counts
    pub agent_dispatches: HashMap<String, u32>,
    /// Error class counts
    pub error_classes: HashMap<String, u32>,
}

impl Default for OrchestrationSnapshot {
    fn default() -> Self {
        Self {
            timestamp: Utc::now(),
            state_counts: HashMap::new(),
            verify_passed: 0,
            verify_failed: 0,
            e2e_passed: 0,
            e2e_failed: 0,
            e2e_skipped: 0,
            graphite_errors: 0,
            merges: 0,
            stops: 0,
            agent_dispatches: HashMap::new(),
            error_classes: HashMap::new(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Orchestration Metrics Store
// ─────────────────────────────────────────────────────────────────────────────

/// Persistent metrics store.
pub struct OrchestrationMetricsStore {
    /// Path to metrics directory
    pub metrics_dir: PathBuf,
    /// Per-task metrics (keyed by task_id)
    pub tasks: HashMap<String, TaskMetrics>,
    /// Per-agent metrics
    pub agents: HashMap<String, AgentMetrics>,
    /// Recent snapshots (last 24h)
    pub snapshots: Vec<OrchestrationSnapshot>,
    /// Current snapshot being built
    pub current_snapshot: OrchestrationSnapshot,
}

impl OrchestrationMetricsStore {
    pub fn new(metrics_dir: PathBuf) -> Self {
        Self {
            metrics_dir,
            tasks: HashMap::new(),
            agents: HashMap::new(),
            snapshots: Vec::new(),
            current_snapshot: OrchestrationSnapshot::default(),
        }
    }

    /// Load metrics from disk.
    pub fn load(metrics_dir: &Path) -> Self {
        let mut store = Self::new(metrics_dir.to_path_buf());

        // Load tasks
        let tasks_path = metrics_dir.join("tasks.json");
        if tasks_path.exists() {
            if let Ok(contents) = fs::read_to_string(&tasks_path) {
                if let Ok(tasks) = serde_json::from_str(&contents) {
                    store.tasks = tasks;
                }
            }
        }

        // Load agents
        let agents_path = metrics_dir.join("agents.json");
        if agents_path.exists() {
            if let Ok(contents) = fs::read_to_string(&agents_path) {
                if let Ok(agents) = serde_json::from_str(&contents) {
                    store.agents = agents;
                }
            }
        }

        // Load recent snapshots
        let snapshots_path = metrics_dir.join("snapshots.jsonl");
        if snapshots_path.exists() {
            if let Ok(contents) = fs::read_to_string(&snapshots_path) {
                let cutoff = Utc::now() - Duration::hours(24);
                for line in contents.lines() {
                    if let Ok(snapshot) = serde_json::from_str::<OrchestrationSnapshot>(line) {
                        if snapshot.timestamp > cutoff {
                            store.snapshots.push(snapshot);
                        }
                    }
                }
            }
        }

        store
    }

    /// Save metrics to disk.
    pub fn save(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.metrics_dir)?;

        // Save tasks
        let tasks_json = serde_json::to_string_pretty(&self.tasks)?;
        fs::write(self.metrics_dir.join("tasks.json"), tasks_json)?;

        // Save agents
        let agents_json = serde_json::to_string_pretty(&self.agents)?;
        fs::write(self.metrics_dir.join("agents.json"), agents_json)?;

        // Save snapshots (append only)
        let snapshots_path = self.metrics_dir.join("snapshots.jsonl");
        let mut snapshots_content = String::new();
        for snapshot in &self.snapshots {
            snapshots_content.push_str(&serde_json::to_string(snapshot)?);
            snapshots_content.push('\n');
        }
        fs::write(snapshots_path, snapshots_content)?;

        Ok(())
    }

    /// Get or create task metrics.
    pub fn get_task(&mut self, task_id: &str, repo_id: &str) -> &mut TaskMetrics {
        self.tasks
            .entry(task_id.to_string())
            .or_insert_with(|| TaskMetrics::new(task_id, repo_id))
    }

    /// Get or create agent metrics.
    pub fn get_agent(&mut self, role: AgentRole) -> &mut AgentMetrics {
        let role_name = role.name().to_string();
        self.agents
            .entry(role_name.clone())
            .or_insert_with(|| AgentMetrics::new(&role_name))
    }

    /// Record a dispatch decision.
    pub fn record_dispatch(&mut self, role: AgentRole) {
        let role_name = role.name().to_string();
        *self
            .current_snapshot
            .agent_dispatches
            .entry(role_name)
            .or_insert(0) += 1;
    }

    /// Record an error classification.
    pub fn record_error_class(&mut self, class: ErrorClass) {
        *self
            .current_snapshot
            .error_classes
            .entry(class.to_string())
            .or_insert(0) += 1;
    }

    /// Record a verify result.
    pub fn record_verify(&mut self, passed: bool) {
        if passed {
            self.current_snapshot.verify_passed += 1;
        } else {
            self.current_snapshot.verify_failed += 1;
        }
    }

    /// Record an E2E result.
    pub fn record_e2e(&mut self, result: &E2EResult) {
        if result.passed {
            self.current_snapshot.e2e_passed += 1;
        } else {
            self.current_snapshot.e2e_failed += 1;
        }
    }

    /// Record a merge.
    pub fn record_merge(&mut self) {
        self.current_snapshot.merges += 1;
    }

    /// Record a stop.
    pub fn record_stop(&mut self) {
        self.current_snapshot.stops += 1;
    }

    /// Record a Graphite error.
    pub fn record_graphite_error(&mut self) {
        self.current_snapshot.graphite_errors += 1;
    }

    /// Update state counts.
    pub fn update_state_counts(&mut self, counts: HashMap<String, u32>) {
        self.current_snapshot.state_counts = counts;
    }

    /// Flush current snapshot and start a new one.
    pub fn flush_snapshot(&mut self) {
        let snapshot = std::mem::take(&mut self.current_snapshot);
        self.snapshots.push(snapshot);
        self.current_snapshot = OrchestrationSnapshot::default();

        // Keep only last 24h of snapshots
        let cutoff = Utc::now() - Duration::hours(24);
        self.snapshots.retain(|s| s.timestamp > cutoff);
    }

    /// Generate summary report.
    pub fn generate_summary(&self) -> OrchestrationSummary {
        let mut summary = OrchestrationSummary::default();

        // Task summary
        summary.total_tasks = self.tasks.len() as u64;
        summary.successful_tasks = self.tasks.values().filter(|t| t.succeeded).count() as u64;
        summary.failed_tasks = self.tasks.values().filter(|t| !t.succeeded).count() as u64;

        // Time to merge
        let merge_times: Vec<f64> = self
            .tasks
            .values()
            .filter_map(|t| t.time_to_merge_hours())
            .collect();
        if !merge_times.is_empty() {
            summary.avg_time_to_merge_hours =
                merge_times.iter().sum::<f64>() / merge_times.len() as f64;
            summary.median_time_to_merge_hours = {
                let mut sorted = merge_times.clone();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                sorted[sorted.len() / 2]
            };
        }

        // E2E pass rate
        let e2e_results: Vec<bool> = self.tasks.values().filter_map(|t| t.e2e_passed).collect();
        if !e2e_results.is_empty() {
            let passed = e2e_results.iter().filter(|&&p| p).count();
            summary.e2e_pass_rate = passed as f64 / e2e_results.len() as f64;
        }

        // Agent summary
        summary.agents = self.agents.clone();

        // Error class distribution
        for task in self.tasks.values() {
            for class in &task.error_classes {
                *summary.error_class_counts.entry(class.clone()).or_insert(0) += 1;
            }
        }

        // Recent activity (from snapshots)
        let recent: Vec<_> = self
            .snapshots
            .iter()
            .rev()
            .take(6) // Last ~30 minutes at 5-min intervals
            .collect();
        summary.recent_merges = recent.iter().map(|s| s.merges).sum();
        summary.recent_stops = recent.iter().map(|s| s.stops).sum();

        summary
    }
}

impl Default for OrchestrationMetricsStore {
    fn default() -> Self {
        Self::new(PathBuf::from("logs/metrics"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Summary Report
// ─────────────────────────────────────────────────────────────────────────────

/// Summary of orchestration metrics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrchestrationSummary {
    pub total_tasks: u64,
    pub successful_tasks: u64,
    pub failed_tasks: u64,
    pub avg_time_to_merge_hours: f64,
    pub median_time_to_merge_hours: f64,
    pub e2e_pass_rate: f64,
    pub agents: HashMap<String, AgentMetrics>,
    pub error_class_counts: HashMap<String, u32>,
    pub recent_merges: u32,
    pub recent_stops: u32,
}

impl OrchestrationSummary {
    /// Render as markdown.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str("# Othala Orchestration Summary\n\n");

        md.push_str("## Task Metrics\n\n");
        md.push_str(&format!("- **Total Tasks:** {}\n", self.total_tasks));
        md.push_str(&format!("- **Successful:** {}\n", self.successful_tasks));
        md.push_str(&format!("- **Failed:** {}\n", self.failed_tasks));
        md.push_str(&format!(
            "- **Success Rate:** {:.1}%\n",
            if self.total_tasks > 0 {
                self.successful_tasks as f64 / self.total_tasks as f64 * 100.0
            } else {
                0.0
            }
        ));
        md.push_str(&format!(
            "- **Avg Time to Merge:** {:.1}h\n",
            self.avg_time_to_merge_hours
        ));
        md.push_str(&format!(
            "- **Median Time to Merge:** {:.1}h\n",
            self.median_time_to_merge_hours
        ));
        md.push_str(&format!("- **E2E Pass Rate:** {:.1}%\n\n", self.e2e_pass_rate * 100.0));

        md.push_str("## Agent Performance\n\n");
        md.push_str("| Agent | Tasks | Success Rate | Avg Duration | Recovery Rate |\n");
        md.push_str("|-------|-------|--------------|--------------|---------------|\n");
        for (name, metrics) in &self.agents {
            md.push_str(&format!(
                "| {} | {} | {:.1}% | {:.1}s | {:.1}% |\n",
                name,
                metrics.total_tasks,
                metrics.success_rate() * 100.0,
                metrics.avg_duration_secs,
                metrics.recovery_success_rate * 100.0,
            ));
        }
        md.push_str("\n");

        if !self.error_class_counts.is_empty() {
            md.push_str("## Error Distribution\n\n");
            for (class, count) in &self.error_class_counts {
                md.push_str(&format!("- **{}:** {}\n", class, count));
            }
            md.push_str("\n");
        }

        md.push_str("## Recent Activity (30 min)\n\n");
        md.push_str(&format!("- **Merges:** {}\n", self.recent_merges));
        md.push_str(&format!("- **Stops:** {}\n", self.recent_stops));

        md
    }

    /// Render as JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_metrics_tracks_attempts() {
        let mut metrics = TaskMetrics::new("T1", "test-repo");

        metrics.record_attempt(AgentRole::Hephaestus, 30.0);
        metrics.record_attempt(AgentRole::Sisyphus, 60.0);

        assert_eq!(metrics.total_attempts, 2);
        assert_eq!(metrics.agents_used.len(), 2);
        assert_eq!(metrics.agent_durations.get("hephaestus"), Some(&30.0));
        assert_eq!(metrics.agent_durations.get("sisyphus"), Some(&60.0));
    }

    #[test]
    fn task_metrics_time_to_merge() {
        let mut metrics = TaskMetrics::new("T1", "test-repo");
        metrics.started_at = Utc::now() - Duration::hours(2);
        metrics.mark_merged();

        let ttm = metrics.time_to_merge_hours().unwrap();
        assert!(ttm > 1.9 && ttm < 2.1);
    }

    #[test]
    fn agent_metrics_tracks_success_rate() {
        let mut metrics = AgentMetrics::new("hephaestus");

        metrics.record_task(true, 30.0, false);
        metrics.record_task(true, 45.0, false);
        metrics.record_task(false, 60.0, false);

        assert_eq!(metrics.total_tasks, 3);
        assert_eq!(metrics.successful_tasks, 2);
        assert!((metrics.success_rate() - 0.666).abs() < 0.01);
    }

    #[test]
    fn agent_metrics_tracks_recovery_rate() {
        let mut metrics = AgentMetrics::new("sisyphus");

        // First attempts
        metrics.record_task(true, 30.0, false);
        metrics.record_task(false, 30.0, false);

        // Recovery attempts
        metrics.record_task(true, 60.0, true);
        metrics.record_task(true, 60.0, true);
        metrics.record_task(false, 60.0, true);

        assert_eq!(metrics.first_attempts, 2);
        assert_eq!(metrics.recovery_attempts, 3);
        assert!((metrics.first_attempt_success_rate - 0.5).abs() < 0.01);
        assert!((metrics.recovery_success_rate - 0.666).abs() < 0.01);
    }

    #[test]
    fn summary_renders_markdown() {
        let mut summary = OrchestrationSummary::default();
        summary.total_tasks = 10;
        summary.successful_tasks = 8;
        summary.failed_tasks = 2;
        summary.e2e_pass_rate = 0.9;

        let md = summary.to_markdown();
        assert!(md.contains("Total Tasks:** 10"));
        assert!(md.contains("Success Rate:** 80.0%"));
        assert!(md.contains("E2E Pass Rate:** 90.0%"));
    }

    #[test]
    fn store_records_snapshots() {
        let mut store = OrchestrationMetricsStore::default();

        store.record_dispatch(AgentRole::Hephaestus);
        store.record_dispatch(AgentRole::Sisyphus);
        store.record_verify(true);
        store.record_merge();

        assert_eq!(
            store.current_snapshot.agent_dispatches.get("hephaestus"),
            Some(&1)
        );
        assert_eq!(
            store.current_snapshot.agent_dispatches.get("sisyphus"),
            Some(&1)
        );
        assert_eq!(store.current_snapshot.verify_passed, 1);
        assert_eq!(store.current_snapshot.merges, 1);
    }
}
