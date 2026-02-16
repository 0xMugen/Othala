use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::fmt;

const WARN_THRESHOLD_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TimeoutConfig {
    pub default_timeout_secs: u64,
    pub max_timeout_secs: u64,
    pub grace_period_secs: u64,
    pub check_interval_secs: u64,
    pub per_state_timeouts: HashMap<String, u64>,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            default_timeout_secs: 3_600,
            max_timeout_secs: 86_400,
            grace_period_secs: 60,
            check_interval_secs: 30,
            per_state_timeouts: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeoutEntry {
    pub task_id: String,
    pub started_at: DateTime<Utc>,
    pub deadline: DateTime<Utc>,
    pub state_at_start: String,
    pub grace_expires: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeoutAction {
    Warn {
        task_id: String,
        remaining_secs: u64,
    },
    GracePeriod {
        task_id: String,
    },
    Kill {
        task_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeoutError {
    TaskNotTracked(String),
    ExceedsMaximum(u64),
}

impl fmt::Display for TimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TaskNotTracked(task_id) => write!(f, "task not tracked: {task_id}"),
            Self::ExceedsMaximum(secs) => {
                write!(f, "requested timeout exceeds maximum allowed: {secs}s")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct TimeoutTracker {
    pub entries: HashMap<String, TimeoutEntry>,
    pub config: TimeoutConfig,
}

impl TimeoutTracker {
    pub fn new(config: TimeoutConfig) -> Self {
        Self {
            entries: HashMap::new(),
            config,
        }
    }

    pub fn start_tracking(&mut self, task_id: &str, state: &str) -> TimeoutEntry {
        let now = Utc::now();
        let timeout_secs = self.timeout_for_state(state);
        let deadline = now + Duration::seconds(timeout_secs as i64);

        let entry = TimeoutEntry {
            task_id: task_id.to_string(),
            started_at: now,
            deadline,
            state_at_start: state.to_string(),
            grace_expires: None,
        };

        self.entries.insert(task_id.to_string(), entry.clone());
        entry
    }

    pub fn stop_tracking(&mut self, task_id: &str) -> Option<TimeoutEntry> {
        self.entries.remove(task_id)
    }

    pub fn check_timeouts(&mut self) -> Vec<TimeoutAction> {
        self.check_timeouts_at(Utc::now())
    }

    pub fn extend_deadline(&mut self, task_id: &str, extra_secs: u64) -> Result<(), TimeoutError> {
        let Some(entry) = self.entries.get_mut(task_id) else {
            return Err(TimeoutError::TaskNotTracked(task_id.to_string()));
        };

        let requested_total_secs = (entry.deadline - entry.started_at).num_seconds() + extra_secs as i64;
        if requested_total_secs > self.config.max_timeout_secs as i64 {
            return Err(TimeoutError::ExceedsMaximum(requested_total_secs as u64));
        }

        entry.deadline += Duration::seconds(extra_secs as i64);
        Ok(())
    }

    pub fn is_tracked(&self, task_id: &str) -> bool {
        self.entries.contains_key(task_id)
    }

    pub fn remaining_secs(&self, task_id: &str) -> Option<i64> {
        self.entries.get(task_id).map(|entry| {
            let target = entry.grace_expires.unwrap_or(entry.deadline);
            (target - Utc::now()).num_seconds()
        })
    }

    pub fn active_count(&self) -> usize {
        self.entries.len()
    }

    fn timeout_for_state(&self, state: &str) -> u64 {
        let timeout = self
            .config
            .per_state_timeouts
            .get(state)
            .copied()
            .unwrap_or(self.config.default_timeout_secs);
        timeout.min(self.config.max_timeout_secs)
    }

    fn check_timeouts_at(&mut self, now: DateTime<Utc>) -> Vec<TimeoutAction> {
        let mut actions = Vec::new();
        let mut to_kill = Vec::new();

        for (task_id, entry) in &mut self.entries {
            if let Some(grace_expires) = entry.grace_expires {
                if now >= grace_expires {
                    actions.push(TimeoutAction::Kill {
                        task_id: task_id.clone(),
                    });
                    to_kill.push(task_id.clone());
                }
                continue;
            }

            let remaining = (entry.deadline - now).num_seconds();
            if remaining <= 0 {
                let grace_expires = now + Duration::seconds(self.config.grace_period_secs as i64);
                entry.grace_expires = Some(grace_expires);
                actions.push(TimeoutAction::GracePeriod {
                    task_id: task_id.clone(),
                });
            } else if remaining <= WARN_THRESHOLD_SECS {
                actions.push(TimeoutAction::Warn {
                    task_id: task_id.clone(),
                    remaining_secs: remaining as u64,
                });
            }
        }

        for task_id in to_kill {
            let _ = self.entries.remove(&task_id);
        }

        actions
    }
}

impl Default for TimeoutTracker {
    fn default() -> Self {
        Self::new(TimeoutConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn timeout_config_defaults() {
        let cfg = TimeoutConfig::default();
        assert_eq!(cfg.default_timeout_secs, 3_600);
        assert_eq!(cfg.max_timeout_secs, 86_400);
        assert_eq!(cfg.grace_period_secs, 60);
        assert_eq!(cfg.check_interval_secs, 30);
        assert!(cfg.per_state_timeouts.is_empty());
    }

    #[test]
    fn per_state_timeout_override_is_used() {
        let mut cfg = TimeoutConfig::default();
        cfg.per_state_timeouts.insert("chatting".to_string(), 7_200);
        let mut tracker = TimeoutTracker::new(cfg);

        let entry = tracker.start_tracking("T1", "chatting");
        let timeout_secs = (entry.deadline - entry.started_at).num_seconds();

        assert_eq!(timeout_secs, 7_200);
    }

    #[test]
    fn per_state_timeout_is_capped_by_maximum() {
        let mut cfg = TimeoutConfig {
            max_timeout_secs: 100,
            ..TimeoutConfig::default()
        };
        cfg.per_state_timeouts.insert("chatting".to_string(), 120);
        let mut tracker = TimeoutTracker::new(cfg);

        let entry = tracker.start_tracking("T2", "chatting");
        assert_eq!((entry.deadline - entry.started_at).num_seconds(), 100);
    }

    #[test]
    fn start_tracking_adds_entry() {
        let mut tracker = TimeoutTracker::default();
        tracker.start_tracking("T3", "running");
        assert!(tracker.is_tracked("T3"));
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn stop_tracking_removes_entry() {
        let mut tracker = TimeoutTracker::default();
        tracker.start_tracking("T4", "running");
        let removed = tracker.stop_tracking("T4");
        assert!(removed.is_some());
        assert!(!tracker.is_tracked("T4"));
    }

    #[test]
    fn stop_tracking_unknown_task_returns_none() {
        let mut tracker = TimeoutTracker::default();
        assert!(tracker.stop_tracking("missing").is_none());
    }

    #[test]
    fn active_count_tracks_multiple_entries() {
        let mut tracker = TimeoutTracker::default();
        tracker.start_tracking("A", "running");
        tracker.start_tracking("B", "running");
        tracker.start_tracking("C", "running");
        assert_eq!(tracker.active_count(), 3);
    }

    #[test]
    fn extend_deadline_succeeds_within_maximum() {
        let cfg = TimeoutConfig {
            default_timeout_secs: 100,
            max_timeout_secs: 200,
            ..TimeoutConfig::default()
        };
        let mut tracker = TimeoutTracker::new(cfg);
        tracker.start_tracking("T5", "running");

        let before = tracker.entries.get("T5").expect("entry exists").deadline;
        let result = tracker.extend_deadline("T5", 50);
        let after = tracker.entries.get("T5").expect("entry exists").deadline;

        assert!(result.is_ok());
        assert_eq!((after - before).num_seconds(), 50);
    }

    #[test]
    fn extend_deadline_fails_for_untracked_task() {
        let mut tracker = TimeoutTracker::default();
        let err = tracker
            .extend_deadline("missing", 10)
            .expect_err("expected error");
        assert_eq!(err, TimeoutError::TaskNotTracked("missing".to_string()));
    }

    #[test]
    fn extend_deadline_fails_when_exceeding_maximum() {
        let cfg = TimeoutConfig {
            default_timeout_secs: 100,
            max_timeout_secs: 120,
            ..TimeoutConfig::default()
        };
        let mut tracker = TimeoutTracker::new(cfg);
        tracker.start_tracking("T6", "running");

        let err = tracker
            .extend_deadline("T6", 50)
            .expect_err("expected error");

        assert_eq!(err, TimeoutError::ExceedsMaximum(150));
    }

    #[test]
    fn remaining_secs_for_missing_task_is_none() {
        let tracker = TimeoutTracker::default();
        assert!(tracker.remaining_secs("missing").is_none());
    }

    #[test]
    fn remaining_secs_for_active_task_is_positive() {
        let mut tracker = TimeoutTracker::default();
        tracker.start_tracking("T7", "running");
        let remaining = tracker
            .remaining_secs("T7")
            .expect("tracked task should have remaining seconds");
        assert!(remaining > 0);
    }

    #[test]
    fn check_timeouts_warns_when_under_five_minutes() {
        let mut tracker = TimeoutTracker::default();
        tracker.start_tracking("T8", "running");

        {
            let entry = tracker.entries.get_mut("T8").expect("entry exists");
            entry.deadline = now() + Duration::seconds(240);
        }

        let actions = tracker.check_timeouts();
        assert_eq!(actions.len(), 1);

        match &actions[0] {
            TimeoutAction::Warn {
                task_id,
                remaining_secs,
            } => {
                assert_eq!(task_id, "T8");
                assert!(*remaining_secs <= 240);
            }
            _ => panic!("expected warn action"),
        }
    }

    #[test]
    fn check_timeouts_enters_grace_when_deadline_passed() {
        let cfg = TimeoutConfig {
            grace_period_secs: 30,
            ..TimeoutConfig::default()
        };
        let mut tracker = TimeoutTracker::new(cfg);
        tracker.start_tracking("T9", "running");

        {
            let entry = tracker.entries.get_mut("T9").expect("entry exists");
            entry.deadline = now() - Duration::seconds(1);
        }

        let actions = tracker.check_timeouts();
        assert_eq!(actions, vec![TimeoutAction::GracePeriod {
            task_id: "T9".to_string()
        }]);
        assert!(tracker
            .entries
            .get("T9")
            .expect("entry exists")
            .grace_expires
            .is_some());
    }

    #[test]
    fn check_timeouts_kills_when_grace_expires() {
        let mut tracker = TimeoutTracker::default();
        tracker.start_tracking("T10", "running");

        {
            let entry = tracker.entries.get_mut("T10").expect("entry exists");
            entry.deadline = now() - Duration::seconds(10);
            entry.grace_expires = Some(now() - Duration::seconds(1));
        }

        let actions = tracker.check_timeouts();
        assert_eq!(actions, vec![TimeoutAction::Kill {
            task_id: "T10".to_string()
        }]);
        assert!(!tracker.is_tracked("T10"));
    }

    #[test]
    fn grace_period_prevents_warn_action() {
        let mut tracker = TimeoutTracker::default();
        tracker.start_tracking("T11", "running");

        {
            let entry = tracker.entries.get_mut("T11").expect("entry exists");
            entry.deadline = now() + Duration::seconds(60);
            entry.grace_expires = Some(now() + Duration::seconds(30));
        }

        let actions = tracker.check_timeouts();
        assert!(actions.is_empty());
    }

    #[test]
    fn check_timeouts_handles_multiple_tasks() {
        let mut tracker = TimeoutTracker::default();
        tracker.start_tracking("W", "running");
        tracker.start_tracking("G", "running");
        tracker.start_tracking("K", "running");

        {
            let warn = tracker.entries.get_mut("W").expect("entry exists");
            warn.deadline = now() + Duration::seconds(120);
        }
        {
            let grace = tracker.entries.get_mut("G").expect("entry exists");
            grace.deadline = now() - Duration::seconds(1);
        }
        {
            let kill = tracker.entries.get_mut("K").expect("entry exists");
            kill.deadline = now() - Duration::seconds(5);
            kill.grace_expires = Some(now() - Duration::seconds(1));
        }

        let actions = tracker.check_timeouts();
        assert_eq!(actions.len(), 3);
        assert!(actions.iter().any(|a| {
            matches!(
                a,
                TimeoutAction::Warn {
                    task_id,
                    remaining_secs: _
                } if task_id == "W"
            )
        }));
        assert!(actions.iter().any(|a| {
            matches!(
                a,
                TimeoutAction::GracePeriod { task_id } if task_id == "G"
            )
        }));
        assert!(actions.iter().any(|a| {
            matches!(a, TimeoutAction::Kill { task_id } if task_id == "K")
        }));
    }

    #[test]
    fn timeout_error_display_is_human_readable() {
        let missing = TimeoutError::TaskNotTracked("abc".to_string()).to_string();
        let max = TimeoutError::ExceedsMaximum(999).to_string();
        assert!(missing.contains("abc"));
        assert!(max.contains("999"));
    }
}
