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
    pub selected_task_activity: Vec<String>,
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
            selected_task_activity: Vec::new(),
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::state::{
        ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus, VerifyTier,
    };
    use orch_core::types::{ModelKind, RepoId, SubmitMode, Task, TaskId, TaskRole, TaskType};

    use super::{AgentPane, DashboardState, TaskOverviewRow};

    fn mk_task(id: &str) -> Task {
        Task {
            id: TaskId(id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Task {id}"),
            state: TaskState::Running,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            branch_name: Some(format!("task/{id}")),
            worktree_path: format!(".orch/wt/{id}").into(),
            pr: None,
            verify_status: VerifyStatus::NotRun,
            review_status: ReviewStatus {
                required_models: vec![ModelKind::Claude],
                approvals_received: 0,
                approvals_required: 1,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn task_overview_row_formats_failed_verify_and_review_capacity() {
        let mut task = mk_task("T1");
        task.verify_status = VerifyStatus::Failed {
            tier: VerifyTier::Quick,
            summary: "line one\nline two with a fairly long explanation".to_string(),
        };
        task.review_status.approvals_received = 1;
        task.review_status.approvals_required = 2;
        task.review_status.unanimous = true;
        task.review_status.capacity_state = ReviewCapacityState::WaitingForReviewCapacity;

        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(row.verify_summary, "failed:quick:line one line two wit...");
        assert_eq!(row.review_summary, "1/2 unanimous=true cap=waiting");
    }

    #[test]
    fn task_overview_row_uses_dash_when_branch_missing() {
        let mut task = mk_task("T2");
        task.branch_name = None;

        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(row.branch, "-");
    }

    #[test]
    fn dashboard_selection_wraps_for_tasks_and_panes() {
        let mut state = DashboardState::default();
        state.tasks = vec![
            TaskOverviewRow::from_task(&mk_task("T1")),
            TaskOverviewRow::from_task(&mk_task("T2")),
        ];
        state.panes = vec![
            AgentPane::new("A1", TaskId("T1".to_string()), ModelKind::Codex),
            AgentPane::new("A2", TaskId("T2".to_string()), ModelKind::Claude),
        ];

        state.move_task_selection_previous();
        assert_eq!(state.selected_task_idx, 1);
        state.move_task_selection_next();
        assert_eq!(state.selected_task_idx, 0);

        state.move_pane_selection_previous();
        assert_eq!(state.selected_pane_idx, 1);
        state.move_pane_selection_next();
        assert_eq!(state.selected_pane_idx, 0);
    }

    #[test]
    fn agent_pane_append_line_caps_history_and_tail() {
        let mut pane = AgentPane::new("A1", TaskId("T1".to_string()), ModelKind::Codex);
        for i in 0..405 {
            pane.append_line(format!("line-{i}"));
        }

        assert_eq!(pane.lines.len(), 400);
        assert_eq!(pane.lines.front().cloned(), Some("line-5".to_string()));
        assert_eq!(pane.lines.back().cloned(), Some("line-404".to_string()));

        let tail = pane.tail(3);
        assert_eq!(
            tail,
            vec![
                "line-402".to_string(),
                "line-403".to_string(),
                "line-404".to_string()
            ]
        );
    }
}
