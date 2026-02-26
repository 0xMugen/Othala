//! Delta-based operator reporting — only surface meaningful state changes.
//!
//! Instead of dumping the full system state every tick, this module computes
//! diffs between consecutive snapshots and produces compact reports that an
//! operator (human or LLM) can scan in seconds.
//!
//! Key features:
//! - State-change detection across tasks, models, QA, and context gen.
//! - NO_REPLY / noise suppression for idle ticks and repeated states.
//! - Structured `DeltaReport` schema with JSON serialization.
//! - Human-readable and `--json` rendering.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// State snapshot (what we compare between ticks)
// ---------------------------------------------------------------------------

/// A lightweight snapshot of system state at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSnapshot {
    pub at: DateTime<Utc>,
    pub task_states: HashMap<String, String>,
    pub model_health: HashMap<String, ModelHealthState>,
    pub context_gen_status: String,
    pub qa_states: HashMap<String, String>,
    pub active_pipelines: Vec<String>,
    pub generation_count: u64,
    pub merge_count: u64,
    pub stop_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelHealthState {
    Healthy,
    Cooldown,
    Disabled,
}

impl Default for SystemSnapshot {
    fn default() -> Self {
        Self {
            at: Utc::now(),
            task_states: HashMap::new(),
            model_health: HashMap::new(),
            context_gen_status: "idle".to_string(),
            qa_states: HashMap::new(),
            active_pipelines: Vec::new(),
            generation_count: 0,
            merge_count: 0,
            stop_count: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Delta computation
// ---------------------------------------------------------------------------

/// A single change detected between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeltaChange {
    TaskStateChanged {
        task_id: String,
        from: String,
        to: String,
    },
    TaskAdded {
        task_id: String,
        state: String,
    },
    TaskRemoved {
        task_id: String,
        last_state: String,
    },
    ModelHealthChanged {
        model: String,
        from: ModelHealthState,
        to: ModelHealthState,
    },
    ContextGenStatusChanged {
        from: String,
        to: String,
    },
    QAStateChanged {
        task_id: String,
        from: String,
        to: String,
    },
    NewMerges {
        count: u64,
    },
    NewStops {
        count: u64,
    },
    PipelineStarted {
        task_id: String,
    },
    PipelineCompleted {
        task_id: String,
    },
}

/// Compute the set of changes between two snapshots.
pub fn compute_delta(prev: &SystemSnapshot, curr: &SystemSnapshot) -> Vec<DeltaChange> {
    let mut changes = Vec::new();

    // Task state changes.
    for (task_id, new_state) in &curr.task_states {
        match prev.task_states.get(task_id) {
            Some(old_state) if old_state != new_state => {
                changes.push(DeltaChange::TaskStateChanged {
                    task_id: task_id.clone(),
                    from: old_state.clone(),
                    to: new_state.clone(),
                });
            }
            None => {
                changes.push(DeltaChange::TaskAdded {
                    task_id: task_id.clone(),
                    state: new_state.clone(),
                });
            }
            _ => {}
        }
    }

    // Tasks that disappeared.
    for (task_id, old_state) in &prev.task_states {
        if !curr.task_states.contains_key(task_id) {
            changes.push(DeltaChange::TaskRemoved {
                task_id: task_id.clone(),
                last_state: old_state.clone(),
            });
        }
    }

    // Model health changes.
    for (model, new_health) in &curr.model_health {
        if let Some(old_health) = prev.model_health.get(model) {
            if old_health != new_health {
                changes.push(DeltaChange::ModelHealthChanged {
                    model: model.clone(),
                    from: old_health.clone(),
                    to: new_health.clone(),
                });
            }
        }
    }

    // Context gen status.
    if prev.context_gen_status != curr.context_gen_status {
        changes.push(DeltaChange::ContextGenStatusChanged {
            from: prev.context_gen_status.clone(),
            to: curr.context_gen_status.clone(),
        });
    }

    // QA state changes.
    for (task_id, new_state) in &curr.qa_states {
        match prev.qa_states.get(task_id) {
            Some(old_state) if old_state != new_state => {
                changes.push(DeltaChange::QAStateChanged {
                    task_id: task_id.clone(),
                    from: old_state.clone(),
                    to: new_state.clone(),
                });
            }
            _ => {}
        }
    }

    // Pipeline changes.
    let prev_pipelines: std::collections::HashSet<&String> =
        prev.active_pipelines.iter().collect();
    let curr_pipelines: std::collections::HashSet<&String> =
        curr.active_pipelines.iter().collect();

    for p in curr_pipelines.difference(&prev_pipelines) {
        changes.push(DeltaChange::PipelineStarted {
            task_id: (*p).clone(),
        });
    }
    for p in prev_pipelines.difference(&curr_pipelines) {
        changes.push(DeltaChange::PipelineCompleted {
            task_id: (*p).clone(),
        });
    }

    // Merge / stop count deltas.
    if curr.merge_count > prev.merge_count {
        changes.push(DeltaChange::NewMerges {
            count: curr.merge_count - prev.merge_count,
        });
    }
    if curr.stop_count > prev.stop_count {
        changes.push(DeltaChange::NewStops {
            count: curr.stop_count - prev.stop_count,
        });
    }

    changes
}

// ---------------------------------------------------------------------------
// Noise suppression
// ---------------------------------------------------------------------------

/// Policy for suppressing noisy / repeated reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuppressionPolicy {
    /// Suppress reports with zero changes.
    pub suppress_empty: bool,
    /// Suppress context gen idle↔idle transitions.
    pub suppress_context_idle_repeat: bool,
    /// Minimum seconds between non-empty reports (rate limiting).
    pub min_report_interval_secs: u64,
}

impl Default for SuppressionPolicy {
    fn default() -> Self {
        Self {
            suppress_empty: true,
            suppress_context_idle_repeat: true,
            min_report_interval_secs: 30,
        }
    }
}

/// Apply suppression to a set of delta changes.
///
/// Returns the filtered changes. If the result is empty and `suppress_empty`
/// is enabled, the caller should skip emitting a report.
pub fn apply_suppression(changes: &[DeltaChange], policy: &SuppressionPolicy) -> Vec<DeltaChange> {
    let mut filtered: Vec<DeltaChange> = changes.to_vec();

    if policy.suppress_context_idle_repeat {
        filtered.retain(|c| {
            !matches!(
                c,
                DeltaChange::ContextGenStatusChanged { from, to } if from == "idle" && to == "idle"
            )
        });
    }

    filtered
}

/// Check if enough time has passed since the last report.
pub fn should_emit(
    last_report_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    policy: &SuppressionPolicy,
    changes: &[DeltaChange],
) -> bool {
    // Always emit if there are high-priority changes.
    let has_high_priority = changes.iter().any(|c| {
        matches!(
            c,
            DeltaChange::NewStops { .. }
                | DeltaChange::ModelHealthChanged { .. }
                | DeltaChange::TaskRemoved { .. }
        )
    });
    if has_high_priority {
        return true;
    }

    if policy.suppress_empty && changes.is_empty() {
        return false;
    }

    match last_report_at {
        Some(last) => {
            let elapsed = now.signed_duration_since(last).num_seconds();
            elapsed >= policy.min_report_interval_secs as i64
        }
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Delta report (structured output)
// ---------------------------------------------------------------------------

/// A delta-based operator report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaReport {
    pub generated_at: DateTime<Utc>,
    pub tick_number: u64,
    pub changes: Vec<DeltaChange>,
    pub summary: DeltaSummary,
    pub suppressed: bool,
}

/// Summary section of a delta report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaSummary {
    pub total_changes: usize,
    pub task_changes: usize,
    pub model_changes: usize,
    pub qa_changes: usize,
    pub new_merges: u64,
    pub new_stops: u64,
}

/// Build a DeltaReport from two snapshots.
pub fn build_delta_report(
    prev: &SystemSnapshot,
    curr: &SystemSnapshot,
    tick_number: u64,
    policy: &SuppressionPolicy,
) -> DeltaReport {
    let raw_changes = compute_delta(prev, curr);
    let changes = apply_suppression(&raw_changes, policy);

    let suppressed = policy.suppress_empty && changes.is_empty() && !raw_changes.is_empty();

    let task_changes = changes
        .iter()
        .filter(|c| {
            matches!(
                c,
                DeltaChange::TaskStateChanged { .. }
                    | DeltaChange::TaskAdded { .. }
                    | DeltaChange::TaskRemoved { .. }
            )
        })
        .count();

    let model_changes = changes
        .iter()
        .filter(|c| matches!(c, DeltaChange::ModelHealthChanged { .. }))
        .count();

    let qa_changes = changes
        .iter()
        .filter(|c| matches!(c, DeltaChange::QAStateChanged { .. }))
        .count();

    let new_merges = changes
        .iter()
        .filter_map(|c| match c {
            DeltaChange::NewMerges { count } => Some(*count),
            _ => None,
        })
        .sum();

    let new_stops = changes
        .iter()
        .filter_map(|c| match c {
            DeltaChange::NewStops { count } => Some(*count),
            _ => None,
        })
        .sum();

    DeltaReport {
        generated_at: Utc::now(),
        tick_number,
        summary: DeltaSummary {
            total_changes: changes.len(),
            task_changes,
            model_changes,
            qa_changes,
            new_merges,
            new_stops,
        },
        changes,
        suppressed,
    }
}

// ---------------------------------------------------------------------------
// Human-readable rendering
// ---------------------------------------------------------------------------

/// Render a delta report as human-readable text.
pub fn render_delta_report(report: &DeltaReport) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "\x1b[35m── Operator Report (tick #{}) ──\x1b[0m\n",
        report.tick_number
    ));

    if report.changes.is_empty() {
        out.push_str("  \x1b[90m(no changes)\x1b[0m\n");
        return out;
    }

    out.push_str(&format!(
        "  {} change(s): {} task, {} model, {} QA",
        report.summary.total_changes,
        report.summary.task_changes,
        report.summary.model_changes,
        report.summary.qa_changes,
    ));
    if report.summary.new_merges > 0 {
        out.push_str(&format!(", {} merge(s)", report.summary.new_merges));
    }
    if report.summary.new_stops > 0 {
        out.push_str(&format!(", {} stop(s)", report.summary.new_stops));
    }
    out.push('\n');

    for change in &report.changes {
        match change {
            DeltaChange::TaskStateChanged { task_id, from, to } => {
                let color = state_color(to);
                out.push_str(&format!(
                    "  {color}→{}\x1b[0m  {} → {}\n",
                    task_id, from, to
                ));
            }
            DeltaChange::TaskAdded { task_id, state } => {
                out.push_str(&format!("  \x1b[32m+ {}\x1b[0m  ({})\n", task_id, state));
            }
            DeltaChange::TaskRemoved {
                task_id,
                last_state,
            } => {
                out.push_str(&format!(
                    "  \x1b[31m- {}\x1b[0m  (was {})\n",
                    task_id, last_state
                ));
            }
            DeltaChange::ModelHealthChanged { model, from, to } => {
                out.push_str(&format!(
                    "  \x1b[33m⚕ {}\x1b[0m  {:?} → {:?}\n",
                    model, from, to
                ));
            }
            DeltaChange::ContextGenStatusChanged { from, to } => {
                out.push_str(&format!("  \x1b[35m◉ context-gen\x1b[0m  {} → {}\n", from, to));
            }
            DeltaChange::QAStateChanged { task_id, from, to } => {
                out.push_str(&format!(
                    "  \x1b[36m✓ QA/{}\x1b[0m  {} → {}\n",
                    task_id, from, to
                ));
            }
            DeltaChange::NewMerges { count } => {
                out.push_str(&format!("  \x1b[32m✓ {} task(s) merged\x1b[0m\n", count));
            }
            DeltaChange::NewStops { count } => {
                out.push_str(&format!("  \x1b[31m■ {} task(s) stopped\x1b[0m\n", count));
            }
            DeltaChange::PipelineStarted { task_id } => {
                out.push_str(&format!(
                    "  \x1b[33m▶ pipeline started\x1b[0m  {}\n",
                    task_id
                ));
            }
            DeltaChange::PipelineCompleted { task_id } => {
                out.push_str(&format!(
                    "  \x1b[32m■ pipeline done\x1b[0m  {}\n",
                    task_id
                ));
            }
        }
    }

    out
}

fn state_color(state: &str) -> &'static str {
    match state {
        "merged" => "\x1b[32m",
        "stopped" => "\x1b[31m",
        "chatting" => "\x1b[34m",
        "ready" => "\x1b[32m",
        "submitting" | "awaiting_merge" => "\x1b[33m",
        _ => "\x1b[0m",
    }
}

// ---------------------------------------------------------------------------
// Reporter state (carried across ticks)
// ---------------------------------------------------------------------------

/// Maintains state between ticks for delta reporting.
pub struct DeltaReporter {
    pub policy: SuppressionPolicy,
    pub previous_snapshot: Option<SystemSnapshot>,
    pub last_report_at: Option<DateTime<Utc>>,
    pub tick_count: u64,
    pub total_reports_emitted: u64,
    pub total_reports_suppressed: u64,
}

impl DeltaReporter {
    pub fn new(policy: SuppressionPolicy) -> Self {
        Self {
            policy,
            previous_snapshot: None,
            last_report_at: None,
            tick_count: 0,
            total_reports_emitted: 0,
            total_reports_suppressed: 0,
        }
    }

    /// Process a new snapshot. Returns a report if one should be emitted.
    pub fn process_tick(&mut self, snapshot: SystemSnapshot) -> Option<DeltaReport> {
        self.tick_count += 1;

        let report = match &self.previous_snapshot {
            Some(prev) => {
                let report =
                    build_delta_report(prev, &snapshot, self.tick_count, &self.policy);

                let now = Utc::now();
                if should_emit(self.last_report_at, now, &self.policy, &report.changes) {
                    self.last_report_at = Some(now);
                    self.total_reports_emitted += 1;
                    Some(report)
                } else {
                    self.total_reports_suppressed += 1;
                    None
                }
            }
            None => {
                // First tick — no previous to compare against. Emit initial state.
                if !snapshot.task_states.is_empty() {
                    let empty = SystemSnapshot::default();
                    let report =
                        build_delta_report(&empty, &snapshot, self.tick_count, &self.policy);
                    self.last_report_at = Some(Utc::now());
                    self.total_reports_emitted += 1;
                    Some(report)
                } else {
                    self.total_reports_suppressed += 1;
                    None
                }
            }
        };

        self.previous_snapshot = Some(snapshot);
        report
    }

    pub fn suppression_rate(&self) -> f64 {
        let total = self.total_reports_emitted + self.total_reports_suppressed;
        if total == 0 {
            0.0
        } else {
            self.total_reports_suppressed as f64 / total as f64
        }
    }
}

impl Default for DeltaReporter {
    fn default() -> Self {
        Self::new(SuppressionPolicy::default())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot(tasks: &[(&str, &str)]) -> SystemSnapshot {
        let mut s = SystemSnapshot::default();
        for (id, state) in tasks {
            s.task_states.insert(id.to_string(), state.to_string());
        }
        s
    }

    #[test]
    fn no_changes_yields_empty_delta() {
        let a = make_snapshot(&[("T1", "chatting"), ("T2", "ready")]);
        let b = make_snapshot(&[("T1", "chatting"), ("T2", "ready")]);

        let changes = compute_delta(&a, &b);
        assert!(changes.is_empty());
    }

    #[test]
    fn task_state_change_detected() {
        let a = make_snapshot(&[("T1", "chatting")]);
        let b = make_snapshot(&[("T1", "ready")]);

        let changes = compute_delta(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            &changes[0],
            DeltaChange::TaskStateChanged { task_id, from, to }
                if task_id == "T1" && from == "chatting" && to == "ready"
        ));
    }

    #[test]
    fn task_added_detected() {
        let a = make_snapshot(&[("T1", "chatting")]);
        let b = make_snapshot(&[("T1", "chatting"), ("T2", "ready")]);

        let changes = compute_delta(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            &changes[0],
            DeltaChange::TaskAdded { task_id, state }
                if task_id == "T2" && state == "ready"
        ));
    }

    #[test]
    fn task_removed_detected() {
        let a = make_snapshot(&[("T1", "chatting"), ("T2", "ready")]);
        let b = make_snapshot(&[("T1", "chatting")]);

        let changes = compute_delta(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            &changes[0],
            DeltaChange::TaskRemoved { task_id, last_state }
                if task_id == "T2" && last_state == "ready"
        ));
    }

    #[test]
    fn model_health_change_detected() {
        let mut a = SystemSnapshot::default();
        a.model_health
            .insert("claude".to_string(), ModelHealthState::Healthy);
        let mut b = SystemSnapshot::default();
        b.model_health
            .insert("claude".to_string(), ModelHealthState::Cooldown);

        let changes = compute_delta(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            &changes[0],
            DeltaChange::ModelHealthChanged { model, from, to }
                if model == "claude"
                    && *from == ModelHealthState::Healthy
                    && *to == ModelHealthState::Cooldown
        ));
    }

    #[test]
    fn context_gen_status_change_detected() {
        let mut a = SystemSnapshot::default();
        a.context_gen_status = "idle".to_string();
        let mut b = SystemSnapshot::default();
        b.context_gen_status = "running".to_string();

        let changes = compute_delta(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            &changes[0],
            DeltaChange::ContextGenStatusChanged { from, to }
                if from == "idle" && to == "running"
        ));
    }

    #[test]
    fn merge_count_delta() {
        let mut a = SystemSnapshot::default();
        a.merge_count = 2;
        let mut b = SystemSnapshot::default();
        b.merge_count = 5;

        let changes = compute_delta(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            &changes[0],
            DeltaChange::NewMerges { count } if *count == 3
        ));
    }

    #[test]
    fn stop_count_delta() {
        let mut a = SystemSnapshot::default();
        a.stop_count = 1;
        let mut b = SystemSnapshot::default();
        b.stop_count = 3;

        let changes = compute_delta(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            &changes[0],
            DeltaChange::NewStops { count } if *count == 2
        ));
    }

    #[test]
    fn pipeline_started_and_completed() {
        let mut a = SystemSnapshot::default();
        a.active_pipelines = vec!["T1".to_string()];
        let mut b = SystemSnapshot::default();
        b.active_pipelines = vec!["T2".to_string()];

        let changes = compute_delta(&a, &b);
        assert_eq!(changes.len(), 2);
        let has_started = changes.iter().any(|c| {
            matches!(c, DeltaChange::PipelineStarted { task_id } if task_id == "T2")
        });
        let has_completed = changes.iter().any(|c| {
            matches!(c, DeltaChange::PipelineCompleted { task_id } if task_id == "T1")
        });
        assert!(has_started);
        assert!(has_completed);
    }

    #[test]
    fn suppression_filters_idle_context_repeat() {
        let changes = vec![DeltaChange::ContextGenStatusChanged {
            from: "idle".to_string(),
            to: "idle".to_string(),
        }];

        let policy = SuppressionPolicy::default();
        let filtered = apply_suppression(&changes, &policy);
        assert!(filtered.is_empty());
    }

    #[test]
    fn suppression_keeps_meaningful_context_change() {
        let changes = vec![DeltaChange::ContextGenStatusChanged {
            from: "idle".to_string(),
            to: "running".to_string(),
        }];

        let policy = SuppressionPolicy::default();
        let filtered = apply_suppression(&changes, &policy);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn should_emit_always_for_high_priority() {
        let policy = SuppressionPolicy {
            suppress_empty: true,
            suppress_context_idle_repeat: true,
            min_report_interval_secs: 300,
        };

        let changes = vec![DeltaChange::NewStops { count: 1 }];
        // Even with very recent last report, high-priority forces emission.
        assert!(should_emit(Some(Utc::now()), Utc::now(), &policy, &changes));
    }

    #[test]
    fn should_emit_suppresses_empty() {
        let policy = SuppressionPolicy::default();
        let changes: Vec<DeltaChange> = vec![];
        assert!(!should_emit(None, Utc::now(), &policy, &changes));
    }

    #[test]
    fn should_emit_respects_rate_limit() {
        let policy = SuppressionPolicy {
            min_report_interval_secs: 60,
            ..Default::default()
        };
        let changes = vec![DeltaChange::TaskStateChanged {
            task_id: "T1".to_string(),
            from: "chatting".to_string(),
            to: "ready".to_string(),
        }];

        // Just reported — should suppress.
        assert!(!should_emit(Some(Utc::now()), Utc::now(), &policy, &changes));

        // Long ago — should emit.
        let old = Utc::now() - chrono::Duration::seconds(120);
        assert!(should_emit(Some(old), Utc::now(), &policy, &changes));
    }

    #[test]
    fn build_delta_report_summary() {
        let a = make_snapshot(&[("T1", "chatting")]);
        let mut b = make_snapshot(&[("T1", "ready"), ("T2", "chatting")]);
        b.merge_count = 1;

        let policy = SuppressionPolicy::default();
        let report = build_delta_report(&a, &b, 42, &policy);

        assert_eq!(report.tick_number, 42);
        assert_eq!(report.summary.total_changes, 3);
        assert_eq!(report.summary.task_changes, 2);
        assert_eq!(report.summary.new_merges, 1);
        assert!(!report.suppressed);
    }

    #[test]
    fn delta_reporter_first_tick_with_tasks() {
        let mut reporter = DeltaReporter::default();
        let snapshot = make_snapshot(&[("T1", "chatting")]);
        let report = reporter.process_tick(snapshot);
        assert!(report.is_some());
        assert_eq!(reporter.total_reports_emitted, 1);
    }

    #[test]
    fn delta_reporter_first_tick_empty_suppressed() {
        let mut reporter = DeltaReporter::default();
        let snapshot = SystemSnapshot::default();
        let report = reporter.process_tick(snapshot);
        assert!(report.is_none());
        assert_eq!(reporter.total_reports_suppressed, 1);
    }

    #[test]
    fn delta_reporter_no_change_suppressed() {
        let mut reporter = DeltaReporter::new(SuppressionPolicy {
            suppress_empty: true,
            suppress_context_idle_repeat: true,
            min_report_interval_secs: 0,
        });

        let s1 = make_snapshot(&[("T1", "chatting")]);
        let _ = reporter.process_tick(s1);

        let s2 = make_snapshot(&[("T1", "chatting")]);
        let report = reporter.process_tick(s2);
        // Empty changes → suppressed.
        assert!(report.is_none());
    }

    #[test]
    fn delta_reporter_change_emits() {
        let mut reporter = DeltaReporter::new(SuppressionPolicy {
            min_report_interval_secs: 0,
            ..Default::default()
        });

        let s1 = make_snapshot(&[("T1", "chatting")]);
        let _ = reporter.process_tick(s1);

        let s2 = make_snapshot(&[("T1", "ready")]);
        let report = reporter.process_tick(s2);
        assert!(report.is_some());
        let r = report.unwrap();
        assert_eq!(r.summary.task_changes, 1);
    }

    #[test]
    fn delta_reporter_suppression_rate() {
        let mut reporter = DeltaReporter::new(SuppressionPolicy {
            min_report_interval_secs: 0,
            ..Default::default()
        });

        let s1 = make_snapshot(&[("T1", "chatting")]);
        let _ = reporter.process_tick(s1);
        // Emitted: 1

        let s2 = make_snapshot(&[("T1", "chatting")]);
        let _ = reporter.process_tick(s2);
        // Suppressed: 1

        let rate = reporter.suppression_rate();
        assert!((rate - 0.5).abs() < 0.01);
    }

    #[test]
    fn render_delta_report_empty() {
        let report = DeltaReport {
            generated_at: Utc::now(),
            tick_number: 1,
            changes: vec![],
            summary: DeltaSummary {
                total_changes: 0,
                task_changes: 0,
                model_changes: 0,
                qa_changes: 0,
                new_merges: 0,
                new_stops: 0,
            },
            suppressed: false,
        };

        let rendered = render_delta_report(&report);
        assert!(rendered.contains("no changes"));
    }

    #[test]
    fn render_delta_report_with_changes() {
        let report = DeltaReport {
            generated_at: Utc::now(),
            tick_number: 5,
            changes: vec![
                DeltaChange::TaskStateChanged {
                    task_id: "T1".to_string(),
                    from: "chatting".to_string(),
                    to: "ready".to_string(),
                },
                DeltaChange::NewMerges { count: 2 },
            ],
            summary: DeltaSummary {
                total_changes: 2,
                task_changes: 1,
                model_changes: 0,
                qa_changes: 0,
                new_merges: 2,
                new_stops: 0,
            },
            suppressed: false,
        };

        let rendered = render_delta_report(&report);
        assert!(rendered.contains("tick #5"));
        assert!(rendered.contains("T1"));
        assert!(rendered.contains("chatting"));
        assert!(rendered.contains("ready"));
        assert!(rendered.contains("2 task(s) merged"));
    }

    #[test]
    fn report_serializes_to_json() {
        let report = DeltaReport {
            generated_at: Utc::now(),
            tick_number: 10,
            changes: vec![DeltaChange::TaskStateChanged {
                task_id: "T1".to_string(),
                from: "chatting".to_string(),
                to: "ready".to_string(),
            }],
            summary: DeltaSummary {
                total_changes: 1,
                task_changes: 1,
                model_changes: 0,
                qa_changes: 0,
                new_merges: 0,
                new_stops: 0,
            },
            suppressed: false,
        };

        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("\"tick_number\": 10"));
        assert!(json.contains("task_state_changed"));
        assert!(json.contains("\"task_id\": \"T1\""));

        // Roundtrip.
        let decoded: DeltaReport = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.tick_number, 10);
        assert_eq!(decoded.changes.len(), 1);
    }

    #[test]
    fn qa_state_change_detected() {
        let mut a = SystemSnapshot::default();
        a.qa_states.insert("T1".to_string(), "pending".to_string());
        let mut b = SystemSnapshot::default();
        b.qa_states.insert("T1".to_string(), "passed".to_string());

        let changes = compute_delta(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(matches!(
            &changes[0],
            DeltaChange::QAStateChanged { task_id, from, to }
                if task_id == "T1" && from == "pending" && to == "passed"
        ));
    }

    #[test]
    fn multiple_changes_combined() {
        let mut a = make_snapshot(&[("T1", "chatting"), ("T2", "ready")]);
        a.model_health
            .insert("claude".to_string(), ModelHealthState::Healthy);
        a.merge_count = 0;

        let mut b = make_snapshot(&[("T1", "ready"), ("T3", "chatting")]);
        b.model_health
            .insert("claude".to_string(), ModelHealthState::Cooldown);
        b.merge_count = 1;

        let changes = compute_delta(&a, &b);
        // T1 changed, T2 removed, T3 added, claude health changed, 1 merge
        assert_eq!(changes.len(), 5);
    }
}
