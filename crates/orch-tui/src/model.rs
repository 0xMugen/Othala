use chrono::{DateTime, Utc};
use orch_core::state::{ReviewCapacityState, TaskState, VerifyStatus};
use orch_core::types::{ModelKind, RepoId, Task, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskOverviewRow {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    pub branch: String,
    pub stack_position: Option<String>,
    pub state: TaskState,
    pub verify_summary: String,
    pub review_summary: String,
    pub last_activity: DateTime<Utc>,
}

impl TaskOverviewRow {
    pub fn from_task(task: &Task) -> Self {
        let verify_summary = match &task.verify_status {
            VerifyStatus::NotRun => "not_run".to_string(),
            VerifyStatus::Running { tier } => format!("running:{tier:?}").to_ascii_lowercase(),
            VerifyStatus::Passed { tier } => format!("passed:{tier:?}").to_ascii_lowercase(),
            VerifyStatus::Failed { tier, summary } => {
                let short = summarize(summary, 24);
                format!("failed:{tier:?}:{short}").to_ascii_lowercase()
            }
        };

        let review = &task.review_status;
        let review_capacity = match review.capacity_state {
            ReviewCapacityState::Sufficient => "ok",
            ReviewCapacityState::WaitingForReviewCapacity => "waiting",
            ReviewCapacityState::NeedsHuman => "needs_human",
        };
        let review_summary = format!(
            "{}/{} unanimous={} cap={}",
            review.approvals_received, review.approvals_required, review.unanimous, review_capacity
        );

        Self {
            task_id: task.id.clone(),
            repo_id: task.repo_id.clone(),
            branch: task.branch_name.clone().unwrap_or_else(|| "-".to_string()),
            stack_position: None,
            state: task.state,
            verify_summary,
            review_summary,
            last_activity: task.updated_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPaneStatus {
    Starting,
    Running,
    Waiting,
    Exited,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPane {
    pub instance_id: String,
    pub task_id: TaskId,
    pub model: ModelKind,
    pub status: AgentPaneStatus,
    pub updated_at: DateTime<Utc>,
    pub lines: VecDeque<String>,
}

impl AgentPane {
    pub fn new(instance_id: impl Into<String>, task_id: TaskId, model: ModelKind) -> Self {
        Self {
            instance_id: instance_id.into(),
            task_id,
            model,
            status: AgentPaneStatus::Starting,
            updated_at: Utc::now(),
            lines: VecDeque::new(),
        }
    }

    pub fn append_line(&mut self, line: impl Into<String>) {
        self.lines.push_back(line.into());
        self.updated_at = Utc::now();
        while self.lines.len() > 400 {
            self.lines.pop_front();
        }
    }

    pub fn tail(&self, max_lines: usize) -> Vec<String> {
        let len = self.lines.len();
        let start = len.saturating_sub(max_lines);
        self.lines.iter().skip(start).cloned().collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardState {
    pub tasks: Vec<TaskOverviewRow>,
    pub panes: Vec<AgentPane>,
    pub selected_task_idx: usize,
    pub selected_pane_idx: usize,
    pub focused_pane_idx: Option<usize>,
    pub status_line: String,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            tasks: Vec::new(),
            panes: Vec::new(),
            selected_task_idx: 0,
            selected_pane_idx: 0,
            focused_pane_idx: None,
            status_line: "ready".to_string(),
        }
    }
}

impl DashboardState {
    pub fn with_tasks(tasks: &[Task]) -> Self {
        let rows = tasks.iter().map(TaskOverviewRow::from_task).collect();
        Self {
            tasks: rows,
            ..Self::default()
        }
    }

    pub fn selected_task(&self) -> Option<&TaskOverviewRow> {
        self.tasks.get(self.selected_task_idx)
    }

    pub fn selected_pane(&self) -> Option<&AgentPane> {
        self.panes.get(self.selected_pane_idx)
    }

    pub fn selected_pane_mut(&mut self) -> Option<&mut AgentPane> {
        self.panes.get_mut(self.selected_pane_idx)
    }

    pub fn move_task_selection_next(&mut self) {
        if self.tasks.is_empty() {
            self.selected_task_idx = 0;
            return;
        }
        self.selected_task_idx = (self.selected_task_idx + 1) % self.tasks.len();
    }

    pub fn move_task_selection_previous(&mut self) {
        if self.tasks.is_empty() {
            self.selected_task_idx = 0;
            return;
        }
        self.selected_task_idx = if self.selected_task_idx == 0 {
            self.tasks.len() - 1
        } else {
            self.selected_task_idx - 1
        };
    }

    pub fn move_pane_selection_next(&mut self) {
        if self.panes.is_empty() {
            self.selected_pane_idx = 0;
            return;
        }
        self.selected_pane_idx = (self.selected_pane_idx + 1) % self.panes.len();
    }

    pub fn move_pane_selection_previous(&mut self) {
        if self.panes.is_empty() {
            self.selected_pane_idx = 0;
            return;
        }
        self.selected_pane_idx = if self.selected_pane_idx == 0 {
            self.panes.len() - 1
        } else {
            self.selected_pane_idx - 1
        };
    }
}

fn summarize(value: &str, max_len: usize) -> String {
    let mut s = value.trim().replace('\n', " ");
    if s.len() <= max_len {
        return s;
    }
    s.truncate(max_len.saturating_sub(3));
    s.push_str("...");
    s
}
