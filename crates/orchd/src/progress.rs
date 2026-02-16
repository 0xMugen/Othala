use chrono::{DateTime, Utc};
use orch_core::types::TaskId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const WORKFLOW_STAGES: [TaskStage; 8] = [
    TaskStage::Initializing,
    TaskStage::ContextLoading,
    TaskStage::AgentRunning,
    TaskStage::Verifying,
    TaskStage::QAReview,
    TaskStage::Submitting,
    TaskStage::Restacking,
    TaskStage::AwaitingMerge,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStage {
    Initializing,
    ContextLoading,
    AgentRunning,
    Verifying,
    QAReview,
    Submitting,
    Restacking,
    AwaitingMerge,
    Completed,
    Failed,
}

impl TaskStage {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Initializing => "Initializing",
            Self::ContextLoading => "Context loading",
            Self::AgentRunning => "Agent running",
            Self::Verifying => "Verifying",
            Self::QAReview => "QA review",
            Self::Submitting => "Submitting",
            Self::Restacking => "Restacking",
            Self::AwaitingMerge => "Awaiting merge",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
        }
    }

    pub fn weight(&self) -> f64 {
        match self {
            Self::Initializing => 5.0,
            Self::ContextLoading => 10.0,
            Self::AgentRunning => 40.0,
            Self::Verifying => 15.0,
            Self::QAReview => 10.0,
            Self::Submitting => 10.0,
            Self::Restacking => 5.0,
            Self::AwaitingMerge => 5.0,
            Self::Completed | Self::Failed => 0.0,
        }
    }

    pub fn overall_base_pct(&self) -> f64 {
        match self {
            Self::Initializing => 0.0,
            Self::ContextLoading => 5.0,
            Self::AgentRunning => 15.0,
            Self::Verifying => 55.0,
            Self::QAReview => 70.0,
            Self::Submitting => 80.0,
            Self::Restacking => 90.0,
            Self::AwaitingMerge => 95.0,
            Self::Completed | Self::Failed => 100.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskProgress {
    pub task_id: TaskId,
    pub stage: TaskStage,
    pub stage_progress_pct: f64,
    pub overall_progress_pct: f64,
    pub started_at: DateTime<Utc>,
    pub stage_started_at: DateTime<Utc>,
    pub estimated_remaining_secs: Option<f64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProgressTracker {
    entries: HashMap<TaskId, TaskProgress>,
    stage_durations: HashMap<TaskStage, Vec<f64>>,
}

impl ProgressTracker {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            stage_durations: HashMap::new(),
        }
    }

    pub fn start_task(&mut self, task_id: &TaskId) {
        let now = Utc::now();
        let progress = TaskProgress {
            task_id: task_id.clone(),
            stage: TaskStage::Initializing,
            stage_progress_pct: 0.0,
            overall_progress_pct: 0.0,
            started_at: now,
            stage_started_at: now,
            estimated_remaining_secs: None,
            message: None,
        };
        self.entries.insert(task_id.clone(), progress);
        self.refresh_estimate(task_id);
    }

    pub fn set_stage(&mut self, task_id: &TaskId, stage: TaskStage) {
        let now = Utc::now();

        if let Some(entry) = self.entries.get_mut(task_id) {
            let duration_secs = (now - entry.stage_started_at)
                .to_std()
                .map_or(0.0, |duration| duration.as_secs_f64());
            if duration_secs > 0.0 {
                self.stage_durations
                    .entry(entry.stage)
                    .or_default()
                    .push(duration_secs);
            }

            entry.stage = stage;
            entry.stage_started_at = now;

            match stage {
                TaskStage::Completed => {
                    entry.stage_progress_pct = 100.0;
                    entry.overall_progress_pct = 100.0;
                }
                TaskStage::Failed => {
                    entry.stage_progress_pct = 0.0;
                }
                _ => {
                    entry.stage_progress_pct = 0.0;
                    entry.overall_progress_pct = stage.overall_base_pct();
                }
            }
        }

        self.refresh_estimate(task_id);
    }

    pub fn set_progress(&mut self, task_id: &TaskId, pct: f64) {
        if let Some(entry) = self.entries.get_mut(task_id) {
            let clamped = pct.clamp(0.0, 100.0);
            entry.stage_progress_pct = clamped;

            match entry.stage {
                TaskStage::Completed => {
                    entry.overall_progress_pct = 100.0;
                }
                TaskStage::Failed => {}
                _ => {
                    entry.overall_progress_pct =
                        (entry.stage.overall_base_pct() + entry.stage.weight() * (clamped / 100.0))
                            .clamp(0.0, 100.0);
                }
            }
        }

        self.refresh_estimate(task_id);
    }

    pub fn set_message(&mut self, task_id: &TaskId, msg: impl Into<String>) {
        if let Some(entry) = self.entries.get_mut(task_id) {
            entry.message = Some(msg.into());
        }
    }

    pub fn complete_task(&mut self, task_id: &TaskId) {
        self.set_stage(task_id, TaskStage::Completed);
        if let Some(entry) = self.entries.get_mut(task_id) {
            entry.stage_progress_pct = 100.0;
            entry.overall_progress_pct = 100.0;
            entry.estimated_remaining_secs = Some(0.0);
        }
    }

    pub fn fail_task(&mut self, task_id: &TaskId, message: impl Into<String>) {
        self.set_stage(task_id, TaskStage::Failed);
        if let Some(entry) = self.entries.get_mut(task_id) {
            entry.message = Some(message.into());
            entry.estimated_remaining_secs = None;
        }
    }

    pub fn get(&self, task_id: &TaskId) -> Option<&TaskProgress> {
        self.entries.get(task_id)
    }

    pub fn all(&self) -> Vec<&TaskProgress> {
        let mut all = self.entries.values().collect::<Vec<_>>();
        all.sort_by(|left, right| left.task_id.0.cmp(&right.task_id.0));
        all
    }

    pub fn remove(&mut self, task_id: &TaskId) {
        let _ = self.entries.remove(task_id);
    }

    pub fn average_stage_duration(&self, stage: TaskStage) -> Option<f64> {
        let durations = self.stage_durations.get(&stage)?;
        if durations.is_empty() {
            return None;
        }

        let total: f64 = durations.iter().copied().sum();
        Some(total / durations.len() as f64)
    }

    pub fn estimate_remaining(&self, task_id: &TaskId) -> Option<f64> {
        let entry = self.entries.get(task_id)?;

        match entry.stage {
            TaskStage::Completed => return Some(0.0),
            TaskStage::Failed => return None,
            _ => {}
        }

        let mut remaining_secs = 0.0;
        let mut has_any_estimate = false;

        if let Some(current_avg) = self.average_stage_duration(entry.stage) {
            let stage_remaining_ratio = (1.0 - (entry.stage_progress_pct / 100.0)).clamp(0.0, 1.0);
            remaining_secs += current_avg * stage_remaining_ratio;
            has_any_estimate = true;
        }

        let mut seen_current = false;
        for stage in WORKFLOW_STAGES {
            if stage == entry.stage {
                seen_current = true;
                continue;
            }

            if seen_current {
                if let Some(avg) = self.average_stage_duration(stage) {
                    remaining_secs += avg;
                    has_any_estimate = true;
                }
            }
        }

        if has_any_estimate {
            Some(remaining_secs.max(0.0))
        } else {
            None
        }
    }

    fn refresh_estimate(&mut self, task_id: &TaskId) {
        let estimate = self.estimate_remaining(task_id);
        if let Some(entry) = self.entries.get_mut(task_id) {
            entry.estimated_remaining_secs = estimate;
        }
    }
}

impl Default for ProgressTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn task_id(value: &str) -> TaskId {
        TaskId::new(value)
    }

    #[test]
    fn stage_labels_are_human_readable() {
        assert_eq!(TaskStage::Initializing.label(), "Initializing");
        assert_eq!(TaskStage::ContextLoading.label(), "Context loading");
        assert_eq!(TaskStage::AgentRunning.label(), "Agent running");
        assert_eq!(TaskStage::Verifying.label(), "Verifying");
        assert_eq!(TaskStage::QAReview.label(), "QA review");
        assert_eq!(TaskStage::Submitting.label(), "Submitting");
        assert_eq!(TaskStage::Restacking.label(), "Restacking");
        assert_eq!(TaskStage::AwaitingMerge.label(), "Awaiting merge");
        assert_eq!(TaskStage::Completed.label(), "Completed");
        assert_eq!(TaskStage::Failed.label(), "Failed");
    }

    #[test]
    fn stage_weights_match_expected_values() {
        assert_eq!(TaskStage::Initializing.weight(), 5.0);
        assert_eq!(TaskStage::ContextLoading.weight(), 10.0);
        assert_eq!(TaskStage::AgentRunning.weight(), 40.0);
        assert_eq!(TaskStage::Verifying.weight(), 15.0);
        assert_eq!(TaskStage::QAReview.weight(), 10.0);
        assert_eq!(TaskStage::Submitting.weight(), 10.0);
        assert_eq!(TaskStage::Restacking.weight(), 5.0);
        assert_eq!(TaskStage::AwaitingMerge.weight(), 5.0);
        assert_eq!(TaskStage::Completed.weight(), 0.0);
        assert_eq!(TaskStage::Failed.weight(), 0.0);
    }

    #[test]
    fn stage_base_percentages_are_cumulative() {
        assert_eq!(TaskStage::Initializing.overall_base_pct(), 0.0);
        assert_eq!(TaskStage::ContextLoading.overall_base_pct(), 5.0);
        assert_eq!(TaskStage::AgentRunning.overall_base_pct(), 15.0);
        assert_eq!(TaskStage::Verifying.overall_base_pct(), 55.0);
        assert_eq!(TaskStage::QAReview.overall_base_pct(), 70.0);
        assert_eq!(TaskStage::Submitting.overall_base_pct(), 80.0);
        assert_eq!(TaskStage::Restacking.overall_base_pct(), 90.0);
        assert_eq!(TaskStage::AwaitingMerge.overall_base_pct(), 95.0);
        assert_eq!(TaskStage::Completed.overall_base_pct(), 100.0);
        assert_eq!(TaskStage::Failed.overall_base_pct(), 100.0);
    }

    #[test]
    fn start_task_creates_initializing_entry() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T1");

        tracker.start_task(&id);
        let progress = tracker.get(&id).expect("task should be tracked");

        assert_eq!(progress.task_id, id);
        assert_eq!(progress.stage, TaskStage::Initializing);
        assert_eq!(progress.stage_progress_pct, 0.0);
        assert_eq!(progress.overall_progress_pct, 0.0);
        assert!(progress.message.is_none());
    }

    #[test]
    fn start_task_replaces_existing_progress() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T2");

        tracker.start_task(&id);
        tracker.set_stage(&id, TaskStage::AgentRunning);
        tracker.set_progress(&id, 50.0);
        tracker.start_task(&id);

        let progress = tracker.get(&id).expect("task should be tracked");
        assert_eq!(progress.stage, TaskStage::Initializing);
        assert_eq!(progress.overall_progress_pct, 0.0);
    }

    #[test]
    fn set_stage_updates_stage_and_records_history() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T3");
        tracker.start_task(&id);

        {
            let entry = tracker
                .entries
                .get_mut(&id)
                .expect("entry should exist for test setup");
            entry.stage_started_at = Utc::now() - Duration::seconds(12);
        }

        tracker.set_stage(&id, TaskStage::ContextLoading);

        let progress = tracker.get(&id).expect("task should be tracked");
        assert_eq!(progress.stage, TaskStage::ContextLoading);
        assert_eq!(progress.stage_progress_pct, 0.0);
        assert_eq!(progress.overall_progress_pct, 5.0);
        let initializing_avg = tracker
            .average_stage_duration(TaskStage::Initializing)
            .expect("initializing duration should be recorded");
        assert!(initializing_avg >= 12.0);
    }

    #[test]
    fn set_progress_updates_stage_and_overall_percentages() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T4");
        tracker.start_task(&id);
        tracker.set_stage(&id, TaskStage::AgentRunning);

        tracker.set_progress(&id, 50.0);
        let progress = tracker.get(&id).expect("task should be tracked");

        assert_eq!(progress.stage_progress_pct, 50.0);
        assert_eq!(progress.overall_progress_pct, 35.0);
    }

    #[test]
    fn set_progress_clamps_invalid_values() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T5");
        tracker.start_task(&id);
        tracker.set_stage(&id, TaskStage::Verifying);

        tracker.set_progress(&id, 250.0);
        let progress = tracker.get(&id).expect("task should be tracked");
        assert_eq!(progress.stage_progress_pct, 100.0);
        assert_eq!(progress.overall_progress_pct, 70.0);

        tracker.set_progress(&id, -50.0);
        let progress = tracker.get(&id).expect("task should be tracked");
        assert_eq!(progress.stage_progress_pct, 0.0);
        assert_eq!(progress.overall_progress_pct, 55.0);
    }

    #[test]
    fn set_message_assigns_human_readable_message() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T6");
        tracker.start_task(&id);

        tracker.set_message(&id, "warming up");
        let progress = tracker.get(&id).expect("task should be tracked");
        assert_eq!(progress.message.as_deref(), Some("warming up"));
    }

    #[test]
    fn complete_task_marks_task_fully_done() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T7");
        tracker.start_task(&id);
        tracker.set_stage(&id, TaskStage::Submitting);
        tracker.set_progress(&id, 10.0);

        tracker.complete_task(&id);
        let progress = tracker.get(&id).expect("task should be tracked");

        assert_eq!(progress.stage, TaskStage::Completed);
        assert_eq!(progress.stage_progress_pct, 100.0);
        assert_eq!(progress.overall_progress_pct, 100.0);
        assert_eq!(progress.estimated_remaining_secs, Some(0.0));
    }

    #[test]
    fn fail_task_marks_failed_and_sets_message() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T8");
        tracker.start_task(&id);
        tracker.set_stage(&id, TaskStage::AgentRunning);
        tracker.set_progress(&id, 25.0);

        tracker.fail_task(&id, "lint failed");
        let progress = tracker.get(&id).expect("task should be tracked");

        assert_eq!(progress.stage, TaskStage::Failed);
        assert_eq!(progress.message.as_deref(), Some("lint failed"));
        assert_eq!(progress.estimated_remaining_secs, None);
    }

    #[test]
    fn get_returns_none_for_missing_task() {
        let tracker = ProgressTracker::new();
        assert!(tracker.get(&task_id("missing")).is_none());
    }

    #[test]
    fn all_returns_all_entries_sorted_by_task_id() {
        let mut tracker = ProgressTracker::new();
        let a = task_id("A");
        let c = task_id("C");
        let b = task_id("B");
        tracker.start_task(&a);
        tracker.start_task(&c);
        tracker.start_task(&b);

        let all = tracker.all();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].task_id.0, "A");
        assert_eq!(all[1].task_id.0, "B");
        assert_eq!(all[2].task_id.0, "C");
    }

    #[test]
    fn remove_deletes_tracking_entry() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T9");
        tracker.start_task(&id);
        assert!(tracker.get(&id).is_some());

        tracker.remove(&id);
        assert!(tracker.get(&id).is_none());
    }

    #[test]
    fn average_stage_duration_is_none_without_history() {
        let tracker = ProgressTracker::new();
        assert!(tracker.average_stage_duration(TaskStage::Verifying).is_none());
    }

    #[test]
    fn average_stage_duration_returns_mean_seconds() {
        let mut tracker = ProgressTracker::new();
        tracker
            .stage_durations
            .insert(TaskStage::Verifying, vec![10.0, 20.0, 30.0]);

        let avg = tracker
            .average_stage_duration(TaskStage::Verifying)
            .expect("average should be available");
        assert_eq!(avg, 20.0);
    }

    #[test]
    fn estimate_remaining_is_none_when_no_stage_history_exists() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T10");
        tracker.start_task(&id);

        assert!(tracker.estimate_remaining(&id).is_none());
    }

    #[test]
    fn estimate_remaining_uses_current_stage_and_future_averages() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T11");
        tracker.start_task(&id);
        tracker.set_stage(&id, TaskStage::AgentRunning);
        tracker.set_progress(&id, 25.0);

        tracker
            .stage_durations
            .insert(TaskStage::AgentRunning, vec![100.0]);
        tracker.stage_durations.insert(TaskStage::Verifying, vec![20.0]);
        tracker.stage_durations.insert(TaskStage::QAReview, vec![10.0]);

        let estimate = tracker
            .estimate_remaining(&id)
            .expect("estimate should be available");
        assert_eq!(estimate, 105.0);
    }

    #[test]
    fn estimate_remaining_for_completed_task_is_zero() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T12");
        tracker.start_task(&id);
        tracker.complete_task(&id);

        assert_eq!(tracker.estimate_remaining(&id), Some(0.0));
    }

    #[test]
    fn estimate_remaining_for_failed_task_is_none() {
        let mut tracker = ProgressTracker::new();
        let id = task_id("T13");
        tracker.start_task(&id);
        tracker.fail_task(&id, "boom");

        assert!(tracker.estimate_remaining(&id).is_none());
    }

    #[test]
    fn tracker_handles_multiple_tasks_independently() {
        let mut tracker = ProgressTracker::new();
        let a = task_id("T14-A");
        let b = task_id("T14-B");
        tracker.start_task(&a);
        tracker.start_task(&b);

        tracker.set_stage(&a, TaskStage::ContextLoading);
        tracker.set_progress(&a, 10.0);
        tracker.set_stage(&b, TaskStage::Verifying);
        tracker.set_progress(&b, 50.0);

        let a_progress = tracker.get(&a).expect("a should be tracked");
        let b_progress = tracker.get(&b).expect("b should be tracked");

        assert_eq!(a_progress.stage, TaskStage::ContextLoading);
        assert_eq!(a_progress.overall_progress_pct, 6.0);
        assert_eq!(b_progress.stage, TaskStage::Verifying);
        assert_eq!(b_progress.overall_progress_pct, 62.5);
    }
}
