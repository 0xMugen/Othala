use chrono::{DateTime, Utc};
use orch_core::state::{ReviewCapacityState, TaskState, VerifyStatus};
use orch_core::types::{ModelKind, RepoId, Task, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskOverviewRow {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    #[serde(default)]
    pub title: String,
    pub branch: String,
    pub stack_position: Option<String>,
    pub state: TaskState,
    /// Composite label that accounts for verify status when state alone is
    /// ambiguous (e.g. "Running" with a failed verify → "VerifyFail").
    pub display_state: String,
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

        let display_state = effective_display_state(task.state, &task.verify_status);

        Self {
            task_id: task.id.clone(),
            repo_id: task.repo_id.clone(),
            title: summarize(&task.title, 64),
            branch: task.branch_name.clone().unwrap_or_else(|| "-".to_string()),
            stack_position: None,
            state: task.state,
            display_state,
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
        let raw = line.into();
        let Some(line) = normalize_pane_line(&raw) else {
            return;
        };
        self.lines.push_back(line);
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

    /// Returns a window of lines ending `scroll_back` lines from the bottom.
    /// When `scroll_back == 0` this is equivalent to `tail(max_lines)`.
    pub fn window(&self, max_lines: usize, scroll_back: usize) -> Vec<String> {
        let len = self.lines.len();
        // Clamp so the window never slides past the first line.
        let max_back = len.saturating_sub(max_lines.min(len));
        let clamped = scroll_back.min(max_back);
        let end = len - clamped;
        let start = end.saturating_sub(max_lines);
        self.lines
            .iter()
            .skip(start)
            .take(end - start)
            .cloned()
            .collect()
    }
}

/// Normalize raw pane output into stable display text.
///
/// The daemon can forward lines from stdout/stderr and model tool output may
/// include terminal escape sequences. We strip terminal control sequences and
/// legacy `[stderr]` prefixes so the chat UI stays clean.
pub fn normalize_pane_line(raw: &str) -> Option<String> {
    let trimmed = raw.trim_end_matches(['\n', '\r']);
    let normalized = strip_terminal_sequences(trimmed).replace('\r', "");

    let without_stderr = if let Some(rest) = normalized.strip_prefix("[stderr]") {
        rest.trim_start_matches(' ')
    } else {
        normalized.as_str()
    };

    if without_stderr.is_empty() {
        if trimmed.is_empty() {
            return Some(String::new());
        }
        return None;
    }

    Some(without_stderr.to_string())
}

fn strip_terminal_sequences(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut idx = 0usize;

    while idx < bytes.len() {
        let byte = bytes[idx];
        if byte == 0x1B {
            idx += 1;
            if idx >= bytes.len() {
                break;
            }
            match bytes[idx] {
                b'[' => {
                    idx += 1;
                    while idx < bytes.len() {
                        let b = bytes[idx];
                        idx += 1;
                        if (0x40..=0x7E).contains(&b) {
                            break;
                        }
                    }
                }
                b']' => {
                    idx += 1;
                    while idx < bytes.len() {
                        if bytes[idx] == 0x07 {
                            idx += 1;
                            break;
                        }
                        if bytes[idx] == 0x1B && idx + 1 < bytes.len() && bytes[idx + 1] == b'\\' {
                            idx += 2;
                            break;
                        }
                        idx += 1;
                    }
                }
                b'P' | b'X' | b'^' | b'_' => {
                    idx += 1;
                    while idx < bytes.len() {
                        if bytes[idx] == 0x1B && idx + 1 < bytes.len() && bytes[idx + 1] == b'\\' {
                            idx += 2;
                            break;
                        }
                        idx += 1;
                    }
                }
                _ => {
                    idx += 1;
                }
            }
            continue;
        }

        if byte == 0x9B {
            idx += 1;
            while idx < bytes.len() {
                let b = bytes[idx];
                idx += 1;
                if (0x40..=0x7E).contains(&b) {
                    break;
                }
            }
            continue;
        }

        if byte < 0x20 && byte != b'\t' {
            idx += 1;
            continue;
        }

        let ch = input[idx..]
            .chars()
            .next()
            .expect("index always points at char boundary");
        out.push(ch);
        idx += ch.len_utf8();
    }

    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardState {
    pub tasks: Vec<TaskOverviewRow>,
    pub panes: Vec<AgentPane>,
    pub selected_task_activity: Vec<String>,
    pub selected_task_idx: usize,
    pub selected_pane_idx: usize,
    pub focused_pane_idx: Option<usize>,
    pub focused_task: bool,
    pub status_line: String,
    /// Lines scrolled back from the bottom in focused views. 0 = latest output.
    pub scroll_back: usize,
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
            focused_task: false,
            status_line: "ready".to_string(),
            scroll_back: 0,
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

    /// Number of buffered lines in the currently focused pane (if any).
    fn focused_pane_line_count(&self) -> usize {
        if self.focused_task {
            self.selected_task()
                .and_then(|task| self.panes.iter().find(|p| p.task_id == task.task_id))
                .map(|p| p.lines.len())
                .unwrap_or(0)
        } else if let Some(idx) = self.focused_pane_idx {
            self.panes.get(idx).map(|p| p.lines.len()).unwrap_or(0)
        } else {
            0
        }
    }

    pub fn scroll_up(&mut self, amount: usize) {
        let max = self.focused_pane_line_count();
        self.scroll_back = (self.scroll_back + amount).min(max);
    }

    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll_back = self.scroll_back.saturating_sub(amount);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_back = self.focused_pane_line_count();
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_back = 0;
    }
}

/// Produce a user-facing state label that incorporates verify status when the
/// raw `TaskState` alone is ambiguous.  For example, a task in `Running` whose
/// verify has failed shows **VerifyFail** so the user doesn't see "Running" for
/// a task that is effectively stuck.
pub fn effective_display_state(state: TaskState, verify: &VerifyStatus) -> String {
    match state {
        TaskState::Running => match verify {
            VerifyStatus::Failed { .. } => "VerifyFail".to_string(),
            VerifyStatus::Passed { .. } => "Verified".to_string(),
            VerifyStatus::Running { .. } => "Verifying".to_string(),
            VerifyStatus::NotRun => "Running".to_string(),
        },
        other => format!("{other:?}"),
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

    use super::{normalize_pane_line, AgentPane, DashboardState, TaskOverviewRow};

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
        assert_eq!(row.display_state, "VerifyFail");
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
    fn display_state_reflects_verify_status_when_running() {
        use super::effective_display_state;

        // Running + NotRun → "Running"
        let mut task = mk_task("T3");
        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(row.display_state, "Running");

        // Running + Failed → "VerifyFail"
        task.verify_status = VerifyStatus::Failed {
            tier: VerifyTier::Quick,
            summary: "fmt".to_string(),
        };
        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(row.display_state, "VerifyFail");

        // Running + Passed → "Verified"
        task.verify_status = VerifyStatus::Passed {
            tier: VerifyTier::Quick,
        };
        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(row.display_state, "Verified");

        // Running + Running → "Verifying"
        task.verify_status = VerifyStatus::Running {
            tier: VerifyTier::Quick,
        };
        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(row.display_state, "Verifying");

        // Non-Running states use the raw state name
        assert_eq!(
            effective_display_state(TaskState::Failed, &VerifyStatus::NotRun),
            "Failed"
        );
        assert_eq!(
            effective_display_state(TaskState::Reviewing, &VerifyStatus::NotRun),
            "Reviewing"
        );
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

    #[test]
    fn agent_pane_window_returns_slice_offset_from_bottom() {
        let mut pane = AgentPane::new("A1", TaskId("T1".to_string()), ModelKind::Codex);
        for i in 0..20 {
            pane.append_line(format!("L{i}"));
        }

        // scroll_back=0 is equivalent to tail
        assert_eq!(pane.window(3, 0), vec!["L17", "L18", "L19"]);

        // scroll_back=5 skips last 5 lines
        assert_eq!(pane.window(3, 5), vec!["L12", "L13", "L14"]);

        // scroll_back larger than buffer clamps to beginning
        assert_eq!(pane.window(3, 100), vec!["L0", "L1", "L2"]);

        // window larger than available lines returns all
        assert_eq!(pane.window(100, 0).len(), 20);
    }

    #[test]
    fn dashboard_scroll_up_down_and_clamps() {
        let mut state = DashboardState::default();
        let mut pane = AgentPane::new("A1", TaskId("T1".to_string()), ModelKind::Codex);
        for i in 0..50 {
            pane.append_line(format!("line-{i}"));
        }
        state.panes.push(pane);
        state.tasks.push(TaskOverviewRow::from_task(&mk_task("T1")));
        state.focused_task = true;

        assert_eq!(state.scroll_back, 0);
        state.scroll_up(10);
        assert_eq!(state.scroll_back, 10);
        state.scroll_down(3);
        assert_eq!(state.scroll_back, 7);
        state.scroll_down(100);
        assert_eq!(state.scroll_back, 0);

        // clamps to line count
        state.scroll_up(999);
        assert_eq!(state.scroll_back, 50);

        state.scroll_to_bottom();
        assert_eq!(state.scroll_back, 0);
        state.scroll_to_top();
        assert_eq!(state.scroll_back, 50);
    }

    #[test]
    fn normalize_pane_line_strips_ansi_and_stderr_prefix() {
        let value = normalize_pane_line("[stderr] \u{1b}[31mapply failed\u{1b}[0m")
            .expect("normalized line");
        assert_eq!(value, "apply failed");
    }

    #[test]
    fn normalize_pane_line_drops_control_only_sequences() {
        let value = normalize_pane_line("\u{1b}[2K\u{1b}[0m");
        assert_eq!(value, None);
    }

    #[test]
    fn agent_pane_append_line_normalizes_before_store() {
        let mut pane = AgentPane::new("A1", TaskId("T1".to_string()), ModelKind::Codex);
        pane.append_line("[stderr] \u{1b}[32mupdated src/lib.rs\u{1b}[0m");
        pane.append_line("\u{1b}[2K\u{1b}[0m");

        assert_eq!(pane.tail(10), vec!["updated src/lib.rs".to_string()]);
    }
}
