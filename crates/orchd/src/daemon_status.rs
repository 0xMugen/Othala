use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonHealth {
    pub status: HealthStatus,
    pub uptime_secs: u64,
    pub version: String,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub task_summary: TaskSummary,
    pub model_summary: ModelSummary,
    pub system_info: SystemInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskSummary {
    pub total: usize,
    pub chatting: usize,
    pub ready: usize,
    pub submitting: usize,
    pub awaiting_merge: usize,
    pub merged: usize,
    pub stopped: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelSummary {
    pub enabled_models: Vec<String>,
    pub healthy_models: Vec<String>,
    pub cooldown_models: Vec<String>,
    pub total_invocations: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub os: String,
    pub arch: String,
    pub rust_version: String,
    pub nix_available: bool,
    pub graphite_available: bool,
    pub git_version: Option<String>,
}

impl DaemonHealth {
    pub fn new() -> Self {
        Self {
            status: HealthStatus::Healthy,
            uptime_secs: 0,
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            started_at: Utc::now(),
            task_summary: TaskSummary::default(),
            model_summary: ModelSummary::default(),
            system_info: SystemInfo::detect(),
        }
    }

    pub fn check_health(&self) -> HealthStatus {
        let computed = if !self.model_summary.enabled_models.is_empty()
            && self.model_summary.healthy_models.is_empty()
        {
            HealthStatus::Unhealthy
        } else if !self.model_summary.cooldown_models.is_empty()
            || self.model_summary.healthy_models.len() < self.model_summary.enabled_models.len()
        {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        };

        max_health_status(&self.status, &computed)
    }

    pub fn display_compact(&self) -> String {
        let health = self.check_health();
        format!(
            "status={} uptime={} pid={} version={} tasks={}/{} models={}/{}",
            health_status_label(&health),
            format_uptime(self.uptime_secs),
            self.pid,
            self.version,
            self.task_summary.ready,
            self.task_summary.total,
            self.model_summary.healthy_models.len(),
            self.model_summary.enabled_models.len()
        )
    }

    pub fn display_full(&self) -> String {
        let health = self.check_health();
        format!(
            "Daemon Health\nStatus: {}\nUptime: {}\nVersion: {}\nPID: {}\nStarted At: {}\n\nTask Summary\n  Total: {}\n  Chatting: {}\n  Ready: {}\n  Submitting: {}\n  Awaiting Merge: {}\n  Merged: {}\n  Stopped: {}\n\nModel Summary\n  Enabled: {}\n  Healthy: {}\n  Cooldown: {}\n  Total Invocations: {}\n\nSystem Info\n  OS: {}\n  Arch: {}\n  Rust: {}\n  Nix Available: {}\n  Graphite Available: {}\n  Git Version: {}",
            health_status_label(&health),
            format_uptime(self.uptime_secs),
            self.version,
            self.pid,
            self.started_at.to_rfc3339(),
            self.task_summary.total,
            self.task_summary.chatting,
            self.task_summary.ready,
            self.task_summary.submitting,
            self.task_summary.awaiting_merge,
            self.task_summary.merged,
            self.task_summary.stopped,
            self.model_summary.enabled_models.join(", "),
            self.model_summary.healthy_models.join(", "),
            self.model_summary.cooldown_models.join(", "),
            self.model_summary.total_invocations,
            self.system_info.os,
            self.system_info.arch,
            self.system_info.rust_version,
            self.system_info.nix_available,
            self.system_info.graphite_available,
            self.system_info
                .git_version
                .as_deref()
                .unwrap_or("unknown")
        )
    }
}

impl Default for DaemonHealth {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskSummary {
    pub fn from_counts(states: &[(String, usize)]) -> Self {
        let mut summary = Self::default();

        for (state, count) in states {
            summary.total += count;
            match state.as_str() {
                "chatting" => summary.chatting = *count,
                "ready" => summary.ready = *count,
                "submitting" => summary.submitting = *count,
                "awaiting_merge" => summary.awaiting_merge = *count,
                "merged" => summary.merged = *count,
                "stopped" => summary.stopped = *count,
                _ => {}
            }
        }

        summary
    }
}

impl SystemInfo {
    pub fn detect() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            rust_version: command_stdout("rustc", &["--version"]).unwrap_or_else(|| "unknown".to_string()),
            nix_available: command_succeeds("nix", &["--version"]),
            graphite_available: command_succeeds("gt", &["--version"]),
            git_version: command_stdout("git", &["--version"]),
        }
    }
}

pub fn format_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn command_succeeds(command: &str, args: &[&str]) -> bool {
    Command::new(command)
        .args(args)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn command_stdout(command: &str, args: &[&str]) -> Option<String> {
    Command::new(command)
        .args(args)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|text| text.trim().to_string())
            } else {
                None
            }
        })
}

fn health_status_label(status: &HealthStatus) -> &'static str {
    match status {
        HealthStatus::Healthy => "healthy",
        HealthStatus::Degraded => "degraded",
        HealthStatus::Unhealthy => "unhealthy",
    }
}

fn health_severity(status: &HealthStatus) -> u8 {
    match status {
        HealthStatus::Healthy => 0,
        HealthStatus::Degraded => 1,
        HealthStatus::Unhealthy => 2,
    }
}

fn max_health_status(a: &HealthStatus, b: &HealthStatus) -> HealthStatus {
    if health_severity(a) >= health_severity(b) {
        a.clone()
    } else {
        b.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_health_new_sets_expected_defaults() {
        let health = DaemonHealth::new();
        assert_eq!(health.status, HealthStatus::Healthy);
        assert_eq!(health.uptime_secs, 0);
        assert!(!health.version.is_empty());
        assert!(health.pid > 0);
    }

    #[test]
    fn format_uptime_seconds_only() {
        assert_eq!(format_uptime(45), "45s");
    }

    #[test]
    fn format_uptime_minutes_and_seconds() {
        assert_eq!(format_uptime(125), "2m 5s");
    }

    #[test]
    fn format_uptime_hours_and_minutes() {
        assert_eq!(format_uptime(7_505), "2h 5m");
    }

    #[test]
    fn format_uptime_days_hours_and_minutes() {
        assert_eq!(format_uptime(184_500), "2d 3h 15m");
    }

    #[test]
    fn task_summary_from_counts_maps_known_states() {
        let states = vec![
            ("chatting".to_string(), 1),
            ("ready".to_string(), 2),
            ("submitting".to_string(), 3),
            ("awaiting_merge".to_string(), 4),
            ("merged".to_string(), 5),
            ("stopped".to_string(), 6),
        ];

        let summary = TaskSummary::from_counts(&states);
        assert_eq!(summary.total, 21);
        assert_eq!(summary.chatting, 1);
        assert_eq!(summary.ready, 2);
        assert_eq!(summary.submitting, 3);
        assert_eq!(summary.awaiting_merge, 4);
        assert_eq!(summary.merged, 5);
        assert_eq!(summary.stopped, 6);
    }

    #[test]
    fn task_summary_from_counts_includes_unknown_in_total() {
        let states = vec![("queued".to_string(), 7)];
        let summary = TaskSummary::from_counts(&states);
        assert_eq!(summary.total, 7);
        assert_eq!(summary.chatting, 0);
        assert_eq!(summary.ready, 0);
    }

    #[test]
    fn check_health_is_unhealthy_when_no_healthy_models() {
        let mut health = DaemonHealth::new();
        health.model_summary.enabled_models = vec!["codex".to_string()];
        health.model_summary.healthy_models = Vec::new();
        assert_eq!(health.check_health(), HealthStatus::Unhealthy);
    }

    #[test]
    fn check_health_is_degraded_when_some_models_are_not_healthy() {
        let mut health = DaemonHealth::new();
        health.model_summary.enabled_models = vec!["codex".to_string(), "claude".to_string()];
        health.model_summary.healthy_models = vec!["codex".to_string()];
        assert_eq!(health.check_health(), HealthStatus::Degraded);
    }

    #[test]
    fn check_health_honors_explicit_unhealthy_status() {
        let mut health = DaemonHealth::new();
        health.status = HealthStatus::Unhealthy;
        health.model_summary.enabled_models = vec!["codex".to_string()];
        health.model_summary.healthy_models = vec!["codex".to_string()];
        assert_eq!(health.check_health(), HealthStatus::Unhealthy);
    }

    #[test]
    fn display_compact_includes_core_fields() {
        let mut health = DaemonHealth::new();
        health.task_summary.total = 5;
        health.task_summary.ready = 2;
        health.model_summary.enabled_models = vec!["codex".to_string()];
        health.model_summary.healthy_models = vec!["codex".to_string()];

        let output = health.display_compact();
        assert!(output.contains("status=healthy"));
        assert!(output.contains("tasks=2/5"));
        assert!(output.contains("models=1/1"));
    }

    #[test]
    fn display_full_includes_sections() {
        let health = DaemonHealth::new();
        let output = health.display_full();

        assert!(output.contains("Daemon Health"));
        assert!(output.contains("Task Summary"));
        assert!(output.contains("Model Summary"));
        assert!(output.contains("System Info"));
    }

    #[test]
    fn system_info_detect_sets_core_fields() {
        let info = SystemInfo::detect();
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
        assert!(!info.rust_version.is_empty());
    }
}
