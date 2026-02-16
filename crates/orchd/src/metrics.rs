use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
}
