use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub anonymous: bool,
    pub storage_path: Option<PathBuf>,
    pub max_events: usize,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            anonymous: true,
            storage_path: None,
            max_events: 10_000,
        }
    }
}

impl MetricsConfig {
    pub fn is_opted_out() -> bool {
        std::env::var("DO_NOT_TRACK")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
            || std::env::var("OTHALA_NO_TELEMETRY")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: MetricEventType,
    pub properties: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricEventType {
    TaskCreated,
    TaskCompleted,
    TaskFailed,
    AgentInvoked,
    ModelUsed,
    VerifyRun,
    CompactionTriggered,
    SessionCreated,
    CommandExecuted,
    DaemonStarted,
    DaemonStopped,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricsSummary {
    pub total_events: usize,
    pub tasks_created: u64,
    pub tasks_completed: u64,
    pub tasks_failed: u64,
    pub total_agent_invocations: u64,
    pub models_used: HashMap<String, u64>,
    pub avg_task_duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReliabilityKpiSummary {
    pub snapshot_count: usize,
    pub recovery_rate: Option<f64>,
    pub mean_latency_ms: Option<f64>,
    pub p95_latency_ms: Option<f64>,
    pub stuck_sla_compliance_rate: Option<f64>,
    pub false_alert_rate: Option<f64>,
    pub idea_quality_mean: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MetricsSnapshot {
    config: MetricsConfig,
    events: Vec<MetricEvent>,
    counters: HashMap<String, u64>,
    session_id: String,
}

pub struct MetricsCollector {
    config: MetricsConfig,
    events: Vec<MetricEvent>,
    counters: HashMap<String, u64>,
    session_id: String,
}

impl MetricsCollector {
    pub fn new(config: MetricsConfig) -> Self {
        let now = Utc::now();
        let timestamp_nanos = now.timestamp_nanos_opt().unwrap_or(0);
        Self {
            config,
            events: Vec::new(),
            counters: HashMap::new(),
            session_id: format!("session-{timestamp_nanos}"),
        }
    }

    pub fn record(&mut self, event_type: MetricEventType, properties: HashMap<String, String>) {
        if !self.config.enabled || MetricsConfig::is_opted_out() {
            return;
        }

        self.events.push(MetricEvent {
            timestamp: Utc::now(),
            event_type,
            properties,
        });

        if self.events.len() > self.config.max_events {
            let excess = self.events.len() - self.config.max_events;
            self.events.drain(0..excess);
        }
    }

    pub fn increment(&mut self, counter: &str) {
        *self.counters.entry(counter.to_string()).or_insert(0) += 1;
    }

    pub fn get_counter(&self, counter: &str) -> u64 {
        self.counters.get(counter).copied().unwrap_or(0)
    }

    pub fn summary(&self) -> MetricsSummary {
        let mut summary = MetricsSummary {
            total_events: self.events.len(),
            ..MetricsSummary::default()
        };

        let mut duration_sum = 0.0;
        let mut duration_count = 0_u64;

        for event in &self.events {
            match event.event_type {
                MetricEventType::TaskCreated => summary.tasks_created += 1,
                MetricEventType::TaskCompleted => {
                    summary.tasks_completed += 1;
                    if let Some(duration) = event
                        .properties
                        .get("duration_secs")
                        .and_then(|value| value.parse::<f64>().ok())
                    {
                        duration_sum += duration;
                        duration_count += 1;
                    }
                }
                MetricEventType::TaskFailed => summary.tasks_failed += 1,
                MetricEventType::AgentInvoked => summary.total_agent_invocations += 1,
                MetricEventType::ModelUsed => {
                    if let Some(model) = event.properties.get("model") {
                        *summary.models_used.entry(model.clone()).or_insert(0) += 1;
                    }
                }
                MetricEventType::VerifyRun
                | MetricEventType::CompactionTriggered
                | MetricEventType::SessionCreated
                | MetricEventType::CommandExecuted
                | MetricEventType::DaemonStarted
                | MetricEventType::DaemonStopped => {}
            }
        }

        if duration_count > 0 {
            summary.avg_task_duration_secs = Some(duration_sum / duration_count as f64);
        }

        summary
    }

    pub fn save_to_file(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create metrics directory: {err}"))?;
        }

        let snapshot = MetricsSnapshot {
            config: self.config.clone(),
            events: self.events.clone(),
            counters: self.counters.clone(),
            session_id: self.session_id.clone(),
        };

        let content = serde_json::to_string_pretty(&snapshot)
            .map_err(|err| format!("failed to serialize metrics: {err}"))?;
        std::fs::write(path, content).map_err(|err| format!("failed to write metrics file: {err}"))
    }

    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|err| format!("failed to read metrics file: {err}"))?;
        let snapshot: MetricsSnapshot = serde_json::from_str(&content)
            .map_err(|err| format!("failed to parse metrics file: {err}"))?;

        Ok(Self {
            config: snapshot.config,
            events: snapshot.events,
            counters: snapshot.counters,
            session_id: snapshot.session_id,
        })
    }

    pub fn events_since(&self, since: DateTime<Utc>) -> Vec<&MetricEvent> {
        self.events
            .iter()
            .filter(|event| event.timestamp >= since)
            .collect()
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn display_summary(&self) -> String {
        let summary = self.summary();
        let mut lines = vec![
            format!("Session: {}", self.session_id),
            format!("Total events: {}", summary.total_events),
            format!("Tasks created: {}", summary.tasks_created),
            format!("Tasks completed: {}", summary.tasks_completed),
            format!("Tasks failed: {}", summary.tasks_failed),
            format!(
                "Agent invocations: {}",
                summary.total_agent_invocations
            ),
        ];

        if let Some(avg) = summary.avg_task_duration_secs {
            lines.push(format!("Average task duration: {:.2}s", avg));
        }

        if !summary.models_used.is_empty() {
            let mut models: Vec<_> = summary.models_used.into_iter().collect();
            models.sort_by(|left, right| left.0.cmp(&right.0));
            let rendered = models
                .into_iter()
                .map(|(model, count)| format!("{model}={count}"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Models used: {rendered}"));
        }

        lines.join("\n")
    }
}

pub fn summarize_reliability_jsonl(path: &Path) -> Result<ReliabilityKpiSummary, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read reliability snapshots file: {err}"))?;
    summarize_reliability_jsonl_str(&content)
}

pub fn summarize_reliability_jsonl_str(content: &str) -> Result<ReliabilityKpiSummary, String> {
    let mut snapshot_count = 0usize;

    let mut recovery_successes = 0.0f64;
    let mut recovery_attempts = 0.0f64;

    let mut latencies: Vec<f64> = Vec::new();

    let mut sla_met = 0.0f64;
    let mut sla_total = 0.0f64;

    let mut false_alerts = 0.0f64;
    let mut total_alerts = 0.0f64;

    let mut idea_quality_sum = 0.0f64;
    let mut idea_quality_count = 0.0f64;

    for (idx, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let value: Value = serde_json::from_str(line)
            .map_err(|err| format!("invalid JSONL at line {}: {err}", idx + 1))?;
        let Some(obj) = value.as_object() else {
            return Err(format!("invalid JSONL at line {}: expected object", idx + 1));
        };
        snapshot_count += 1;

        // Accept either normalized counters or boolean flags for one-event snapshots.
        if let (Some(successes), Some(attempts)) = (
            get_number(obj, &["recovery_successes", "recoveries", "recovered_count"]),
            get_number(obj, &["recovery_attempts", "recovery_total", "incident_count"]),
        ) {
            recovery_successes += successes.max(0.0);
            recovery_attempts += attempts.max(0.0);
        } else if let Some(rate) = get_number(obj, &["recovery_rate"]) {
            recovery_successes += rate.clamp(0.0, 1.0);
            recovery_attempts += 1.0;
        } else if let Some(ok) =
            get_bool(obj, &["recovery_success", "recovery_succeeded", "recovered"])
        {
            recovery_successes += if ok { 1.0 } else { 0.0 };
            recovery_attempts += 1.0;
        }

        if let Some(latency) = get_number(
            obj,
            &[
                "latency_ms",
                "recovery_latency_ms",
                "detection_latency_ms",
                "latency",
            ],
        ) {
            if latency >= 0.0 {
                latencies.push(latency);
            }
        }

        if let (Some(met), Some(total)) = (
            get_number(obj, &["stuck_sla_met", "stuck_within_sla"]),
            get_number(obj, &["stuck_sla_total", "stuck_incidents", "stuck_total"]),
        ) {
            sla_met += met.max(0.0);
            sla_total += total.max(0.0);
        } else if let Some(met) = get_bool(obj, &["stuck_sla_met", "stuck_within_sla"]) {
            sla_met += if met { 1.0 } else { 0.0 };
            sla_total += 1.0;
        } else if let Some(breached) = get_bool(obj, &["stuck_sla_breached"]) {
            sla_met += if breached { 0.0 } else { 1.0 };
            sla_total += 1.0;
        }

        if let (Some(false_count), Some(total)) = (
            get_number(obj, &["false_alerts", "false_positives"]),
            get_number(obj, &["total_alerts", "alerts_total", "alerts"]),
        ) {
            false_alerts += false_count.max(0.0);
            total_alerts += total.max(0.0);
        } else if let Some(rate) = get_number(obj, &["false_alert_rate"]) {
            false_alerts += rate.clamp(0.0, 1.0);
            total_alerts += 1.0;
        } else if let Some(is_false) = get_bool(obj, &["false_alert", "false_positive"]) {
            false_alerts += if is_false { 1.0 } else { 0.0 };
            total_alerts += 1.0;
        }

        if let Some(score) = get_number(obj, &["idea_quality", "idea_quality_score", "idea_score"])
        {
            idea_quality_sum += score;
            idea_quality_count += 1.0;
        }
    }

    let mean_latency_ms = mean(&latencies);
    let p95_latency_ms = percentile_95(&mut latencies);

    Ok(ReliabilityKpiSummary {
        snapshot_count,
        recovery_rate: ratio(recovery_successes, recovery_attempts),
        mean_latency_ms,
        p95_latency_ms,
        stuck_sla_compliance_rate: ratio(sla_met, sla_total),
        false_alert_rate: ratio(false_alerts, total_alerts),
        idea_quality_mean: ratio(idea_quality_sum, idea_quality_count),
    })
}

pub fn display_reliability_summary(summary: &ReliabilityKpiSummary) -> String {
    let mut lines = vec![format!("Snapshots: {}", summary.snapshot_count)];
    lines.push(format_rate_line("Recovery rate", summary.recovery_rate));
    lines.push(format_number_line("Mean latency", summary.mean_latency_ms, "ms"));
    lines.push(format_number_line("P95 latency", summary.p95_latency_ms, "ms"));
    lines.push(format_rate_line(
        "Stuck SLA compliance",
        summary.stuck_sla_compliance_rate,
    ));
    lines.push(format_rate_line("False alert rate", summary.false_alert_rate));
    lines.push(format_number_line(
        "Idea quality mean",
        summary.idea_quality_mean,
        "",
    ));
    lines.join("\n")
}

fn get_number(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<f64> {
    for key in keys {
        let Some(value) = map.get(*key) else {
            continue;
        };
        if let Some(n) = value.as_f64() {
            return Some(n);
        }
        if let Some(s) = value.as_str() {
            if let Ok(parsed) = s.parse::<f64>() {
                return Some(parsed);
            }
        }
    }
    None
}

fn get_bool(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    for key in keys {
        let Some(value) = map.get(*key) else {
            continue;
        };
        if let Some(b) = value.as_bool() {
            return Some(b);
        }
        if let Some(s) = value.as_str() {
            match s.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => return Some(true),
                "false" | "0" | "no" => return Some(false),
                _ => {}
            }
        }
    }
    None
}

fn ratio(numerator: f64, denominator: f64) -> Option<f64> {
    (denominator > 0.0).then_some(numerator / denominator)
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then_some(values.iter().sum::<f64>() / values.len() as f64)
}

fn percentile_95(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((values.len() as f64) * 0.95).ceil() as usize;
    let idx = idx.saturating_sub(1).min(values.len() - 1);
    Some(values[idx])
}

fn format_rate_line(label: &str, rate: Option<f64>) -> String {
    match rate {
        Some(value) => format!("{label}: {:.2}%", value * 100.0),
        None => format!("{label}: n/a"),
    }
}

fn format_number_line(label: &str, value: Option<f64>, unit: &str) -> String {
    match (value, unit.is_empty()) {
        (Some(v), true) => format!("{label}: {:.2}", v),
        (Some(v), false) => format!("{label}: {:.2}{unit}", v),
        (None, _) => format!("{label}: n/a"),
    }
}

#[cfg(test)]
mod tests {
    use super::{MetricEventType, MetricsCollector, MetricsConfig};
    use chrono::{Duration, Utc};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn clear_opt_out_vars() {
        std::env::remove_var("DO_NOT_TRACK");
        std::env::remove_var("OTHALA_NO_TELEMETRY");
    }

    fn test_path(prefix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}-{}.json",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ))
    }

    #[test]
    fn default_config() {
        let config = MetricsConfig::default();
        assert!(config.enabled);
        assert!(config.anonymous);
        assert!(config.storage_path.is_none());
        assert_eq!(config.max_events, 10_000);
    }

    #[test]
    fn opt_out_via_env_var() {
        let _guard = env_lock().lock().expect("lock env");
        clear_opt_out_vars();
        std::env::set_var("DO_NOT_TRACK", "1");
        assert!(MetricsConfig::is_opted_out());
        clear_opt_out_vars();
    }

    #[test]
    fn record_event() {
        let _guard = env_lock().lock().expect("lock env");
        clear_opt_out_vars();

        let mut collector = MetricsCollector::new(MetricsConfig::default());
        collector.record(MetricEventType::TaskCreated, HashMap::new());

        assert_eq!(collector.event_count(), 1);
    }

    #[test]
    fn increment_counter() {
        let mut collector = MetricsCollector::new(MetricsConfig::default());
        collector.increment("tasks");
        collector.increment("tasks");

        assert_eq!(collector.get_counter("tasks"), 2);
        assert_eq!(collector.get_counter("missing"), 0);
    }

    #[test]
    fn summary_aggregation() {
        let _guard = env_lock().lock().expect("lock env");
        clear_opt_out_vars();

        let mut collector = MetricsCollector::new(MetricsConfig::default());
        collector.record(MetricEventType::TaskCreated, HashMap::new());

        let mut completed = HashMap::new();
        completed.insert("duration_secs".to_string(), "12.5".to_string());
        collector.record(MetricEventType::TaskCompleted, completed);

        collector.record(MetricEventType::TaskFailed, HashMap::new());
        collector.record(MetricEventType::AgentInvoked, HashMap::new());

        let mut model = HashMap::new();
        model.insert("model".to_string(), "codex".to_string());
        collector.record(MetricEventType::ModelUsed, model);

        let summary = collector.summary();
        assert_eq!(summary.total_events, 5);
        assert_eq!(summary.tasks_created, 1);
        assert_eq!(summary.tasks_completed, 1);
        assert_eq!(summary.tasks_failed, 1);
        assert_eq!(summary.total_agent_invocations, 1);
        assert_eq!(summary.models_used.get("codex"), Some(&1));
        assert_eq!(summary.avg_task_duration_secs, Some(12.5));
    }

    #[test]
    fn events_since_filtering() {
        let _guard = env_lock().lock().expect("lock env");
        clear_opt_out_vars();

        let mut collector = MetricsCollector::new(MetricsConfig::default());
        collector.record(MetricEventType::TaskCreated, HashMap::new());

        let since = Utc::now() - Duration::milliseconds(1);
        collector.record(MetricEventType::TaskCompleted, HashMap::new());

        assert!(!collector.events_since(since).is_empty());
    }

    #[test]
    fn display_summary_format() {
        let _guard = env_lock().lock().expect("lock env");
        clear_opt_out_vars();

        let mut collector = MetricsCollector::new(MetricsConfig::default());
        collector.record(MetricEventType::TaskCreated, HashMap::new());
        let display = collector.display_summary();

        assert!(display.contains("Session:"));
        assert!(display.contains("Total events: 1"));
        assert!(display.contains("Tasks created: 1"));
    }

    #[test]
    fn event_count() {
        let _guard = env_lock().lock().expect("lock env");
        clear_opt_out_vars();

        let mut collector = MetricsCollector::new(MetricsConfig::default());
        assert_eq!(collector.event_count(), 0);
        collector.record(MetricEventType::SessionCreated, HashMap::new());
        assert_eq!(collector.event_count(), 1);
    }

    #[test]
    fn record_respects_opt_out() {
        let _guard = env_lock().lock().expect("lock env");
        clear_opt_out_vars();
        std::env::set_var("OTHALA_NO_TELEMETRY", "true");

        let mut collector = MetricsCollector::new(MetricsConfig::default());
        collector.record(MetricEventType::TaskCreated, HashMap::new());
        assert_eq!(collector.event_count(), 0);

        clear_opt_out_vars();
    }

    #[test]
    fn save_and_load_round_trip() {
        let _guard = env_lock().lock().expect("lock env");
        clear_opt_out_vars();

        let path = test_path("othala-metrics");

        let mut collector = MetricsCollector::new(MetricsConfig::default());
        collector.increment("verify_runs");
        collector.record(MetricEventType::VerifyRun, HashMap::new());
        collector.save_to_file(&path).expect("save");

        let loaded = MetricsCollector::load_from_file(&path).expect("load");
        assert_eq!(loaded.event_count(), 1);
        assert_eq!(loaded.get_counter("verify_runs"), 1);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn summarize_reliability_jsonl_computes_expected_kpis() {
        let input = r#"{"recovery_success": true, "latency_ms": 120, "stuck_sla_met": true, "false_alert": false, "idea_quality": 0.8}
{"recovery_success": false, "latency_ms": 300, "stuck_sla_breached": true, "false_alert": true, "idea_quality": 0.4}
{"recovery_rate": 1.0, "latency_ms": 80, "stuck_sla_met": true, "false_alert": false, "idea_quality_score": 1.0}"#;

        let summary = super::summarize_reliability_jsonl_str(input).expect("summary");

        assert_eq!(summary.snapshot_count, 3);
        assert_eq!(summary.recovery_rate, Some(2.0 / 3.0));
        assert_eq!(summary.mean_latency_ms, Some((120.0 + 300.0 + 80.0) / 3.0));
        assert_eq!(summary.p95_latency_ms, Some(300.0));
        assert_eq!(summary.stuck_sla_compliance_rate, Some(2.0 / 3.0));
        assert_eq!(summary.false_alert_rate, Some(1.0 / 3.0));
        assert_eq!(summary.idea_quality_mean, Some((0.8 + 0.4 + 1.0) / 3.0));
    }

    #[test]
    fn summarize_reliability_jsonl_supports_aggregate_counters() {
        let input = r#"{"recovery_successes": 7, "recovery_attempts": 10, "latency_ms": 50, "stuck_sla_met": 8, "stuck_sla_total": 10, "false_alerts": 2, "total_alerts": 10, "idea_score": 3}
{"recovery_successes": 3, "recovery_attempts": 5, "latency_ms": 150, "stuck_sla_met": 4, "stuck_sla_total": 5, "false_alerts": 1, "total_alerts": 5, "idea_score": 5}"#;

        let summary = super::summarize_reliability_jsonl_str(input).expect("summary");

        assert_eq!(summary.snapshot_count, 2);
        assert_eq!(summary.recovery_rate, Some(10.0 / 15.0));
        assert_eq!(summary.mean_latency_ms, Some(100.0));
        assert_eq!(summary.p95_latency_ms, Some(150.0));
        assert_eq!(summary.stuck_sla_compliance_rate, Some(12.0 / 15.0));
        assert_eq!(summary.false_alert_rate, Some(3.0 / 15.0));
        assert_eq!(summary.idea_quality_mean, Some(4.0));
    }

    #[test]
    fn summarize_reliability_jsonl_rejects_invalid_line() {
        let err = super::summarize_reliability_jsonl_str("{not-json}\n").expect_err("invalid json");
        assert!(err.contains("invalid JSONL at line 1"));
    }

    #[test]
    fn display_reliability_summary_renders_rates_and_na() {
        let rendered = super::display_reliability_summary(&super::ReliabilityKpiSummary {
            snapshot_count: 2,
            recovery_rate: Some(0.5),
            mean_latency_ms: Some(42.0),
            p95_latency_ms: None,
            stuck_sla_compliance_rate: Some(1.0),
            false_alert_rate: Some(0.0),
            idea_quality_mean: Some(0.8),
        });

        assert!(rendered.contains("Snapshots: 2"));
        assert!(rendered.contains("Recovery rate: 50.00%"));
        assert!(rendered.contains("Mean latency: 42.00ms"));
        assert!(rendered.contains("P95 latency: n/a"));
        assert!(rendered.contains("Stuck SLA compliance: 100.00%"));
        assert!(rendered.contains("False alert rate: 0.00%"));
        assert!(rendered.contains("Idea quality mean: 0.80"));
    }
}
