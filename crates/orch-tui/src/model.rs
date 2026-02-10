//! MVP TUI model types.

use chrono::{DateTime, Utc};
use orch_core::state::{TaskState, VerifyStatus};
use orch_core::types::{ModelKind, RepoId, Task, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Task overview row for the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskOverviewRow {
    pub task_id: TaskId,
    pub repo_id: RepoId,
    #[serde(default)]
    pub title: String,
    pub branch: String,
    pub stack_position: Option<String>,
    pub state: TaskState,
    /// Composite label that accounts for verify status.
    pub display_state: String,
    pub verify_summary: String,
    pub last_activity: DateTime<Utc>,
}

impl TaskOverviewRow {
    pub fn from_task(task: &Task) -> Self {
        let verify_summary = match &task.verify_status {
            VerifyStatus::NotRun => "not_run".to_string(),
            VerifyStatus::Running => "running".to_string(),
            VerifyStatus::Passed => "passed".to_string(),
            VerifyStatus::Failed { message } => {
                let short = summarize(message, 24);
                format!("failed: {short}")
            }
        };

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
    pub fn window(&self, max_lines: usize, scroll_back: usize) -> Vec<String> {
        let len = self.lines.len();
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
    /// Lines scrolled back from the bottom in focused views.
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

/// Produce a user-facing state label.
pub fn effective_display_state(state: TaskState, verify: &VerifyStatus) -> String {
    match state {
        TaskState::Chatting => match verify {
            VerifyStatus::Failed { .. } => "VerifyFail".to_string(),
            VerifyStatus::Passed => "Verified".to_string(),
            VerifyStatus::Running => "Verifying".to_string(),
            VerifyStatus::NotRun => "Chatting".to_string(),
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
    use super::*;
    use std::path::PathBuf;

    fn mk_task(id: &str) -> Task {
        Task::new(
            TaskId(id.to_string()),
            RepoId("example".to_string()),
            format!("Task {id}"),
            PathBuf::from(format!(".orch/wt/{id}")),
        )
    }

    #[test]
    fn task_overview_row_from_task() {
        let task = mk_task("T1");
        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(row.task_id.0, "T1");
        assert_eq!(row.display_state, "Chatting");
    }

    #[test]
    fn display_state_reflects_verify() {
        let mut task = mk_task("T1");
        task.verify_status = VerifyStatus::Failed {
            message: "test failed".to_string(),
        };
        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(row.display_state, "VerifyFail");
    }

    #[test]
    fn agent_pane_append_and_tail() {
        let mut pane = AgentPane::new("A1", TaskId("T1".to_string()), ModelKind::Claude);
        pane.append_line("line 1");
        pane.append_line("line 2");

        let tail = pane.tail(10);
        assert_eq!(tail, vec!["line 1".to_string(), "line 2".to_string()]);
    }

    #[test]
    fn normalize_pane_line_strips_ansi() {
        let value = normalize_pane_line("[stderr] \u{1b}[31mapply failed\u{1b}[0m")
            .expect("normalized line");
        assert_eq!(value, "apply failed");
    }
}
