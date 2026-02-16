//! MVP TUI model types.

use chrono::{DateTime, Utc};
use orch_core::state::{TaskState, VerifyStatus};
use orch_core::types::{ModelKind, RepoId, Task, TaskId};
use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Display-friendly QA test result for the sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QATestDisplay {
    pub name: String,
    pub suite: String,
    pub passed: bool,
    pub detail: String,
}

/// Task overview row for the TUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Current QA status label (e.g. "baseline running", "passed 5/5").
    #[serde(default)]
    pub qa_status: Option<String>,
    /// Per-test QA results.
    #[serde(default)]
    pub qa_tests: Vec<QATestDisplay>,
    /// Task-specific acceptance targets (what the QA hopes for).
    #[serde(default)]
    pub qa_targets: Vec<String>,
    #[serde(default)]
    pub estimated_tokens: Option<u64>,
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default)]
    pub retry_history: Vec<RetryEntry>,
    #[serde(default)]
    pub depends_on_display: Vec<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub model_display: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryEntry {
    pub attempt: u32,
    pub model: String,
    pub reason: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorEntry {
    pub timestamp: String,
    pub task_id: Option<String>,
    pub message: String,
    pub level: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub timestamp: String,
    pub task_id: String,
    pub description: String,
    pub event_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortMode {
    #[default]
    ByState,
    ByPriority,
    ByLastActivity,
    ByName,
}

impl SortMode {
    pub fn label(&self) -> &'static str {
        match self {
            SortMode::ByState => "State",
            SortMode::ByPriority => "Priority",
            SortMode::ByLastActivity => "Activity",
            SortMode::ByName => "Name",
        }
    }

    pub fn next(&self) -> SortMode {
        match self {
            SortMode::ByState => SortMode::ByPriority,
            SortMode::ByPriority => SortMode::ByLastActivity,
            SortMode::ByLastActivity => SortMode::ByName,
            SortMode::ByName => SortMode::ByState,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TuiTheme {
    pub accent: Color,
    pub header_fg: Color,
    pub header_title: Color,
    pub dim: Color,
    pub muted: Color,
    pub border_normal: Color,
    pub border_focused: Color,
    pub selected_bg: Color,
    pub state_chatting: Color,
    pub state_ready: Color,
    pub state_submitting: Color,
    pub state_merged: Color,
    pub state_stopped: Color,
    pub state_restacking: Color,
    pub state_awaiting: Color,
}

impl Default for TuiTheme {
    fn default() -> Self {
        default_theme()
    }
}

pub const THEME_COUNT: usize = 4;

pub fn default_theme() -> TuiTheme {
    TuiTheme {
        accent: Color::Cyan,
        header_fg: Color::White,
        header_title: Color::Cyan,
        dim: Color::DarkGray,
        muted: Color::Gray,
        border_normal: Color::DarkGray,
        border_focused: Color::Cyan,
        selected_bg: Color::Indexed(236),
        state_chatting: Color::Yellow,
        state_ready: Color::Blue,
        state_submitting: Color::Cyan,
        state_merged: Color::Green,
        state_stopped: Color::Red,
        state_restacking: Color::Magenta,
        state_awaiting: Color::LightBlue,
    }
}

pub fn dark_theme() -> TuiTheme {
    TuiTheme {
        accent: Color::LightCyan,
        header_fg: Color::White,
        header_title: Color::LightCyan,
        dim: Color::DarkGray,
        muted: Color::Gray,
        border_normal: Color::Gray,
        border_focused: Color::LightCyan,
        selected_bg: Color::Indexed(238),
        state_chatting: Color::LightYellow,
        state_ready: Color::LightBlue,
        state_submitting: Color::Cyan,
        state_merged: Color::LightGreen,
        state_stopped: Color::LightRed,
        state_restacking: Color::LightMagenta,
        state_awaiting: Color::Blue,
    }
}

pub fn light_theme() -> TuiTheme {
    TuiTheme {
        accent: Color::Blue,
        header_fg: Color::Black,
        header_title: Color::Blue,
        dim: Color::Gray,
        muted: Color::DarkGray,
        border_normal: Color::Gray,
        border_focused: Color::Blue,
        selected_bg: Color::Indexed(252),
        state_chatting: Color::Yellow,
        state_ready: Color::Blue,
        state_submitting: Color::LightBlue,
        state_merged: Color::Green,
        state_stopped: Color::Red,
        state_restacking: Color::Magenta,
        state_awaiting: Color::Cyan,
    }
}

pub fn solarized_theme() -> TuiTheme {
    TuiTheme {
        accent: Color::Rgb(42, 161, 152),
        header_fg: Color::Rgb(238, 232, 213),
        header_title: Color::Rgb(181, 137, 0),
        dim: Color::Rgb(88, 110, 117),
        muted: Color::Rgb(101, 123, 131),
        border_normal: Color::Rgb(88, 110, 117),
        border_focused: Color::Rgb(38, 139, 210),
        selected_bg: Color::Rgb(7, 54, 66),
        state_chatting: Color::Rgb(203, 75, 22),
        state_ready: Color::Rgb(38, 139, 210),
        state_submitting: Color::Rgb(42, 161, 152),
        state_merged: Color::Rgb(133, 153, 0),
        state_stopped: Color::Rgb(220, 50, 47),
        state_restacking: Color::Rgb(211, 54, 130),
        state_awaiting: Color::Rgb(108, 113, 196),
    }
}

pub fn theme_for_index(index: usize) -> TuiTheme {
    match index % THEME_COUNT {
        0 => default_theme(),
        1 => dark_theme(),
        2 => light_theme(),
        _ => solarized_theme(),
    }
}

pub fn theme_name(index: usize) -> &'static str {
    match index % THEME_COUNT {
        0 => "default",
        1 => "dark",
        2 => "light",
        _ => "solarized",
    }
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
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: task.retry_count,
            retry_history: Vec::new(),
            depends_on_display: task.depends_on.iter().map(|d| d.0.clone()).collect(),
            pr_url: task.pr.as_ref().map(|p| p.url.clone()),
            model_display: task.preferred_model.map(|m| m.as_str().to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PaneCategory {
    #[default]
    Agent,
    QA,
}

/// Check whether a pane instance ID belongs to a given category.
pub fn pane_matches_category(instance_id: &str, category: PaneCategory) -> bool {
    match category {
        PaneCategory::Agent => !instance_id.starts_with("qa-"),
        PaneCategory::QA => instance_id.starts_with("qa-"),
    }
}

/// Infer the category of a pane from its instance ID.
pub fn pane_category_of(instance_id: &str) -> PaneCategory {
    if instance_id.starts_with("qa-") {
        PaneCategory::QA
    } else {
        PaneCategory::Agent
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
    /// Agent was killed because the TUI was closed.
    Stopped,
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

fn window_over_lines(lines: &[String], max_lines: usize, scroll_back: usize) -> Vec<String> {
    let len = lines.len();
    let max_back = len.saturating_sub(max_lines.min(len));
    let clamped = scroll_back.min(max_back);
    let end = len - clamped;
    let start = end.saturating_sub(max_lines);
    lines
        .iter()
        .skip(start)
        .take(end - start)
        .cloned()
        .collect()
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelHealthDisplay {
    pub model: String,
    pub healthy: bool,
    pub recent_failures: u32,
    pub cooldown_until: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DashboardState {
    pub tasks: Vec<TaskOverviewRow>,
    pub panes: Vec<AgentPane>,
    #[serde(default)]
    pub recent_errors: Vec<ErrorEntry>,
    #[serde(default)]
    pub log_root: Option<String>,
    #[serde(default)]
    pub timeline_events: Vec<TimelineEvent>,
    #[serde(default)]
    pub show_timeline: bool,
    pub model_health: Vec<ModelHealthDisplay>,
    pub filter_text: Option<String>,
    pub filter_state: Option<TaskState>,
    #[serde(default)]
    pub sort_mode: SortMode,
    #[serde(default)]
    pub sort_reversed: bool,
    pub selected_task_activity: Vec<String>,
    pub selected_task_idx: usize,
    pub selected_pane_idx: usize,
    pub focused_pane_idx: Option<usize>,
    pub focused_task: bool,
    pub status_line: String,
    /// Lines scrolled back from the bottom in focused views.
    pub scroll_back: usize,
    /// Which pane category (Agent / QA) is currently selected.
    pub selected_pane_category: PaneCategory,
    #[serde(skip)]
    pub current_theme: TuiTheme,
    #[serde(default)]
    pub theme_index: usize,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            tasks: Vec::new(),
            panes: Vec::new(),
            recent_errors: Vec::new(),
            log_root: None,
            timeline_events: Vec::new(),
            show_timeline: false,
            model_health: Vec::new(),
            filter_text: None,
            filter_state: None,
            sort_mode: SortMode::ByState,
            sort_reversed: false,
            selected_task_activity: Vec::new(),
            selected_task_idx: 0,
            selected_pane_idx: 0,
            focused_pane_idx: None,
            focused_task: false,
            status_line: "ready".to_string(),
            scroll_back: 0,
            selected_pane_category: PaneCategory::Agent,
            current_theme: default_theme(),
            theme_index: 0,
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

    pub fn cycle_theme(&mut self) -> &'static str {
        self.theme_index = (self.theme_index + 1) % THEME_COUNT;
        self.current_theme = theme_for_index(self.theme_index);
        theme_name(self.theme_index)
    }

    pub fn state_summary(&self) -> String {
        let mut counts: HashMap<TaskState, usize> = HashMap::new();
        for task in &self.tasks {
            *counts.entry(task.state).or_default() += 1;
        }

        let ordered_states = [
            TaskState::Chatting,
            TaskState::Ready,
            TaskState::Submitting,
            TaskState::Restacking,
            TaskState::AwaitingMerge,
            TaskState::Merged,
            TaskState::Stopped,
        ];

        let parts: Vec<String> = ordered_states
            .iter()
            .filter_map(|state| {
                counts
                    .get(state)
                    .map(|count| format!("{count} {}", task_state_label(*state)))
            })
            .collect();

        parts.join(" | ")
    }

    pub fn completion_percentage(&self) -> f64 {
        if self.tasks.is_empty() {
            return 0.0;
        }

        let done = self
            .tasks
            .iter()
            .filter(|t| matches!(t.state, TaskState::Merged | TaskState::Stopped))
            .count();
        (done as f64 / self.tasks.len() as f64) * 100.0
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

    pub fn sorted_tasks(&self) -> Vec<&TaskOverviewRow> {
        let mut tasks: Vec<&TaskOverviewRow> = self.tasks.iter().collect();
        match self.sort_mode {
            SortMode::ByState => tasks.sort_by_key(|task| task_state_sort_key(task.state)),
            SortMode::ByPriority => tasks.sort_by(|a, b| {
                inferred_priority_rank(b)
                    .cmp(&inferred_priority_rank(a))
                    .then_with(|| b.last_activity.cmp(&a.last_activity))
            }),
            SortMode::ByLastActivity => {
                tasks.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
            }
            SortMode::ByName => tasks.sort_by(|a, b| a.title.cmp(&b.title)),
        }
        if self.sort_reversed {
            tasks.reverse();
        }
        tasks
    }

    pub fn filtered_tasks(&self) -> Vec<usize> {
        let text_filter = self
            .filter_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase);

        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| {
                let state_matches = match self.filter_state {
                    Some(state) => task.state == state,
                    None => true,
                };
                if !state_matches {
                    return false;
                }

                match &text_filter {
                    Some(query) => {
                        task.title.to_lowercase().contains(query)
                            || task.task_id.0.to_lowercase().contains(query)
                    }
                    None => true,
                }
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    pub fn active_filter_label(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(state) = self.filter_state {
            parts.push(format!("state={state:?}"));
        }
        if let Some(text) = self
            .filter_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            parts.push(format!("text={text}"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }

    pub fn ensure_selected_task_visible(&mut self) {
        if self.tasks.is_empty() {
            self.selected_task_idx = 0;
            return;
        }

        if self.selected_task_idx >= self.tasks.len() {
            self.selected_task_idx = self.tasks.len().saturating_sub(1);
        }

        let filtered = self.filtered_tasks();
        if filtered.is_empty() {
            self.selected_task_idx = 0;
            return;
        }

        if !filtered.contains(&self.selected_task_idx) {
            self.selected_task_idx = filtered[0];
        }

        self.snap_pane_to_task();
    }

    pub fn move_task_selection_next(&mut self) {
        let filtered = self.filtered_tasks();
        if filtered.is_empty() {
            self.selected_task_idx = 0;
            return;
        }

        if let Some(position) = filtered.iter().position(|idx| *idx == self.selected_task_idx) {
            let next_pos = (position + 1) % filtered.len();
            self.selected_task_idx = filtered[next_pos];
        } else {
            self.selected_task_idx = filtered[0];
        }
        self.snap_pane_to_task();
    }

    pub fn move_task_selection_previous(&mut self) {
        let filtered = self.filtered_tasks();
        if filtered.is_empty() {
            self.selected_task_idx = 0;
            return;
        }

        if let Some(position) = filtered.iter().position(|idx| *idx == self.selected_task_idx) {
            self.selected_task_idx = if position == 0 {
                filtered[filtered.len() - 1]
            } else {
                filtered[position - 1]
            };
        } else {
            self.selected_task_idx = filtered[0];
        }
        self.snap_pane_to_task();
    }

    /// Find the pane index matching a task + category.
    pub fn pane_index_for_task_category(
        &self,
        task_id: &TaskId,
        category: PaneCategory,
    ) -> Option<usize> {
        self.panes
            .iter()
            .position(|p| p.task_id == *task_id && pane_matches_category(&p.instance_id, category))
    }

    /// Check whether a pane exists for the given task + category.
    pub fn has_pane_in_category(&self, task_id: &TaskId, category: PaneCategory) -> bool {
        self.panes
            .iter()
            .any(|p| p.task_id == *task_id && pane_matches_category(&p.instance_id, category))
    }

    /// Toggle between Agent and QA pane categories, updating `selected_pane_idx`.
    pub fn toggle_pane_category(&mut self) {
        self.selected_pane_category = match self.selected_pane_category {
            PaneCategory::Agent => PaneCategory::QA,
            PaneCategory::QA => PaneCategory::Agent,
        };
        if let Some(task) = self.tasks.get(self.selected_task_idx) {
            let tid = task.task_id.clone();
            if let Some(idx) =
                self.pane_index_for_task_category(&tid, self.selected_pane_category)
            {
                self.selected_pane_idx = idx;
            }
        }
    }

    /// Jump `selected_pane_idx` to the first pane belonging to the selected task,
    /// preferring the current category.
    fn snap_pane_to_task(&mut self) {
        if let Some(task) = self.tasks.get(self.selected_task_idx) {
            let tid = &task.task_id;
            // Try current category first.
            if let Some(idx) =
                self.pane_index_for_task_category(tid, self.selected_pane_category)
            {
                self.selected_pane_idx = idx;
            } else if let Some(idx) = self.panes.iter().position(|p| &p.task_id == tid) {
                // Fall back to first pane and update category.
                self.selected_pane_idx = idx;
                self.selected_pane_category = pane_category_of(&self.panes[idx].instance_id);
            }
        }
    }

    pub fn move_pane_selection_next(&mut self) {
        self.toggle_pane_category();
    }

    pub fn move_pane_selection_previous(&mut self) {
        self.toggle_pane_category();
    }

    pub fn pane_window_with_history(
        &self,
        pane_idx: usize,
        max_lines: usize,
        scroll_back: usize,
    ) -> Vec<String> {
        let Some(current_pane) = self.panes.get(pane_idx) else {
            return Vec::new();
        };

        if scroll_back == 0 {
            return current_pane.window(max_lines, 0);
        }

        let mut timeline = self.pane_history_prefix_lines(pane_idx);
        timeline.extend(current_pane.lines.iter().cloned());
        window_over_lines(&timeline, max_lines, scroll_back)
    }

    fn pane_history_prefix_lines(&self, pane_idx: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for pane in self
            .panes
            .iter()
            .take(pane_idx)
            .filter(|pane| !pane.lines.is_empty())
        {
            lines.push(format!(
                "--- previous chat {} ({:?}, task={}) ---",
                pane.instance_id, pane.model, pane.task_id.0
            ));
            lines.extend(pane.lines.iter().cloned());
            lines.push(String::new());
        }
        lines
    }

    fn pane_line_count_with_history(&self, pane_idx: usize) -> usize {
        let Some(current_pane) = self.panes.get(pane_idx) else {
            return 0;
        };
        let history_lines = self
            .panes
            .iter()
            .take(pane_idx)
            .filter(|pane| !pane.lines.is_empty())
            .map(|pane| pane.lines.len() + 2)
            .sum::<usize>();
        history_lines + current_pane.lines.len()
    }

    fn focused_pane_index(&self) -> Option<usize> {
        if self.focused_task {
            self.selected_task()
                .and_then(|task| self.panes.iter().position(|p| p.task_id == task.task_id))
        } else {
            self.focused_pane_idx
        }
    }

    fn focused_pane_line_count(&self) -> usize {
        self.focused_pane_index()
            .map(|idx| self.pane_line_count_with_history(idx))
            .unwrap_or(0)
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

fn task_state_label(state: TaskState) -> &'static str {
    match state {
        TaskState::Chatting => "chatting",
        TaskState::Ready => "ready",
        TaskState::Submitting => "submitting",
        TaskState::Restacking => "restacking",
        TaskState::AwaitingMerge => "awaiting_merge",
        TaskState::Merged => "merged",
        TaskState::Stopped => "stopped",
    }
}

fn task_state_sort_key(state: TaskState) -> u8 {
    match state {
        TaskState::Chatting => 0,
        TaskState::Ready => 1,
        TaskState::Submitting => 2,
        TaskState::Restacking => 3,
        TaskState::AwaitingMerge => 4,
        TaskState::Merged => 5,
        TaskState::Stopped => 6,
    }
}

fn inferred_priority_rank(task: &TaskOverviewRow) -> u8 {
    let display = task.display_state.to_ascii_lowercase();
    let title = task.title.to_ascii_lowercase();
    if display.contains("critical") || title.contains("critical") {
        3
    } else if display.contains("high") || title.contains("high") {
        2
    } else if display.contains("low") || title.contains("low") {
        0
    } else {
        1
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

/// Status of a tool call execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Succeeded,
    Failed,
}

/// Parsed structural block within agent output for the chat zone view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatBlock {
    /// User message sent via interactive chat (lines starting with "> ")
    UserMessage { lines: Vec<String> },
    /// Agent thinking/reasoning block
    Thinking { lines: Vec<String> },
    /// Agent prose (default assistant text)
    AssistantText { lines: Vec<String> },
    /// Tool execution block (exec marker + command output)
    ToolCall {
        tool: String,
        lines: Vec<String>,
        status: ToolStatus,
    },
    /// Code fence (``` ... ```)
    CodeFence {
        lang: Option<String>,
        lines: Vec<String>,
    },
    /// Diff block (diff --git ... or *** Begin Patch ...)
    Diff { lines: Vec<String> },
    /// Agent identity marker (claude/codex/gemini)
    AgentMarker { agent: String },
    /// Status/signal lines ([patch_ready], [needs_human], etc.)
    StatusSignal { line: String },
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
    use ratatui::style::Color;
    use std::path::PathBuf;

    fn mk_task(id: &str) -> Task {
        Task::new(
            TaskId(id.to_string()),
            RepoId("example".to_string()),
            format!("Task {id}"),
            PathBuf::from(format!(".orch/wt/{id}")),
        )
    }

    fn mk_row(id: &str, title: &str, state: TaskState) -> TaskOverviewRow {
        let mut task = mk_task(id);
        task.state = state;
        task.title = title.to_string();
        TaskOverviewRow::from_task(&task)
    }

    #[test]
    fn sort_mode_cycles_correctly() {
        assert_eq!(SortMode::ByState.next(), SortMode::ByPriority);
        assert_eq!(SortMode::ByPriority.next(), SortMode::ByLastActivity);
        assert_eq!(SortMode::ByLastActivity.next(), SortMode::ByName);
        assert_eq!(SortMode::ByName.next(), SortMode::ByState);
    }

    #[test]
    fn default_theme_uses_expected_palette() {
        let theme = default_theme();
        assert_eq!(theme.accent, Color::Cyan);
        assert_eq!(theme.header_fg, Color::White);
        assert_eq!(theme.border_focused, Color::Cyan);
        assert_eq!(theme.selected_bg, Color::Indexed(236));
        assert_eq!(theme.state_restacking, Color::Magenta);
        assert_eq!(theme.state_awaiting, Color::LightBlue);
    }

    #[test]
    fn named_themes_have_distinct_core_colors() {
        let dark = dark_theme();
        let light = light_theme();
        let solarized = solarized_theme();

        assert_eq!(dark.accent, Color::LightCyan);
        assert_eq!(light.header_fg, Color::Black);
        assert_eq!(solarized.selected_bg, Color::Rgb(7, 54, 66));
        assert_ne!(dark.selected_bg, light.selected_bg);
    }

    #[test]
    fn theme_for_index_wraps_across_all_builtins() {
        assert_eq!(theme_for_index(0), default_theme());
        assert_eq!(theme_for_index(1), dark_theme());
        assert_eq!(theme_for_index(2), light_theme());
        assert_eq!(theme_for_index(3), solarized_theme());
        assert_eq!(theme_for_index(4), default_theme());
    }

    #[test]
    fn dashboard_state_cycles_theme_index_and_theme() {
        let mut state = DashboardState::default();
        assert_eq!(state.theme_index, 0);
        assert_eq!(state.current_theme, default_theme());

        let label = state.cycle_theme();
        assert_eq!(label, "dark");
        assert_eq!(state.theme_index, 1);
        assert_eq!(state.current_theme, dark_theme());

        state.cycle_theme();
        state.cycle_theme();
        state.cycle_theme();
        assert_eq!(state.theme_index, 0);
        assert_eq!(state.current_theme, default_theme());
    }

    #[test]
    fn task_overview_row_from_task() {
        let task = mk_task("T1");
        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(row.task_id.0, "T1");
        assert_eq!(row.display_state, "Chatting");
        assert_eq!(row.retry_count, 0);
    }

    #[test]
    fn task_row_includes_pr_url() {
        use orch_core::types::PullRequestRef;

        let mut task = mk_task("T1");
        task.pr = Some(PullRequestRef {
            number: 42,
            url: "https://github.com/example/repo/pull/42".to_string(),
            draft: false,
        });

        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(
            row.pr_url.as_deref(),
            Some("https://github.com/example/repo/pull/42")
        );
    }

    #[test]
    fn task_row_includes_dependencies() {
        let mut task = mk_task("T1");
        task.depends_on = vec![TaskId("task-1".to_string()), TaskId("task-2".to_string())];

        let row = TaskOverviewRow::from_task(&task);
        assert_eq!(row.depends_on_display, vec!["task-1", "task-2"]);
    }

    #[test]
    fn retry_history_default_empty() {
        let value = serde_json::json!({
            "task_id": "T1",
            "repo_id": "example",
            "title": "Task T1",
            "branch": "task/T1",
            "stack_position": null,
            "state": "CHATTING",
            "display_state": "Chatting",
            "verify_summary": "not_run",
            "last_activity": "2026-02-01T00:00:00Z",
            "retry_count": 0
        });

        let row: TaskOverviewRow = serde_json::from_value(value).expect("deserialize task row");
        assert!(row.retry_history.is_empty());
    }

    #[test]
    fn task_row_with_cost() {
        let mut row = TaskOverviewRow::from_task(&mk_task("T9"));
        row.estimated_tokens = Some(40_000);
        row.estimated_cost_usd = Some(0.12);
        assert_eq!(row.estimated_tokens, Some(40_000));
        assert_eq!(row.estimated_cost_usd, Some(0.12));
    }

    #[test]
    fn dashboard_model_health_default() {
        let state = DashboardState::default();
        assert!(state.model_health.is_empty());
    }

    #[test]
    fn timeline_events_default_empty() {
        let state = DashboardState::default();
        assert!(state.timeline_events.is_empty());
    }

    #[test]
    fn timeline_event_serialization() {
        let event = TimelineEvent {
            timestamp: "2026-02-16T10:30:22Z".to_string(),
            task_id: "T123".to_string(),
            description: "Agent spawned (claude)".to_string(),
            event_type: "spawn".to_string(),
        };

        let json = serde_json::to_string(&event).expect("serialize timeline event");
        let parsed: TimelineEvent =
            serde_json::from_str(&json).expect("deserialize timeline event");
        assert_eq!(parsed, event);
    }

    #[test]
    fn state_summary_counts_tasks() {
        let state = DashboardState {
            tasks: vec![
                mk_row("T1", "Task 1", TaskState::Chatting),
                mk_row("T2", "Task 2", TaskState::Chatting),
                mk_row("T3", "Task 3", TaskState::Chatting),
                mk_row("T4", "Task 4", TaskState::Ready),
                mk_row("T5", "Task 5", TaskState::Merged),
                mk_row("T6", "Task 6", TaskState::Merged),
                mk_row("T7", "Task 7", TaskState::Stopped),
            ],
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        assert_eq!(
            state.state_summary(),
            "3 chatting | 1 ready | 2 merged | 1 stopped"
        );
    }

    #[test]
    fn state_summary_empty_tasks() {
        let state = DashboardState::default();
        assert_eq!(state.state_summary(), "");
    }

    #[test]
    fn completion_percentage_with_mixed_states() {
        let state = DashboardState {
            tasks: vec![
                mk_row("T1", "Task 1", TaskState::Merged),
                mk_row("T2", "Task 2", TaskState::Stopped),
                mk_row("T3", "Task 3", TaskState::Chatting),
                mk_row("T4", "Task 4", TaskState::Ready),
            ],
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        assert_eq!(state.completion_percentage(), 50.0);
    }

    #[test]
    fn completion_percentage_empty_tasks() {
        let state = DashboardState::default();
        assert_eq!(state.completion_percentage(), 0.0);
    }

    #[test]
    fn error_entry_serialization() {
        let entry = ErrorEntry {
            timestamp: "2026-02-16T10:30:22Z".to_string(),
            task_id: Some("T123".to_string()),
            message: "verify failed — compile error".to_string(),
            level: "error".to_string(),
        };

        let json = serde_json::to_string(&entry).expect("serialize error entry");
        let parsed: ErrorEntry = serde_json::from_str(&json).expect("deserialize error entry");
        assert_eq!(parsed, entry);
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
    fn filter_by_text_matches_title() {
        let state = DashboardState {
            tasks: vec![
                mk_row("T1", "Implement OAuth login", TaskState::Chatting),
                mk_row("T2", "Fix panic on submit", TaskState::Ready),
            ],
            filter_text: Some("oauth".to_string()),
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        assert_eq!(state.filtered_tasks(), vec![0]);
    }

    #[test]
    fn filter_by_text_matches_id_case_insensitive() {
        let state = DashboardState {
            tasks: vec![
                mk_row("T100", "Implement OAuth login", TaskState::Chatting),
                mk_row("T200", "Fix panic on submit", TaskState::Ready),
            ],
            filter_text: Some("t100".to_string()),
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        assert_eq!(state.filtered_tasks(), vec![0]);
    }

    #[test]
    fn filter_by_state() {
        let state = DashboardState {
            tasks: vec![
                mk_row("T1", "Implement OAuth login", TaskState::Chatting),
                mk_row("T2", "Fix panic on submit", TaskState::Ready),
                mk_row("T3", "Wire merge checks", TaskState::Ready),
            ],
            filter_state: Some(TaskState::Ready),
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        assert_eq!(state.filtered_tasks(), vec![1, 2]);
    }

    #[test]
    fn combined_filter() {
        let state = DashboardState {
            tasks: vec![
                mk_row("T1", "Implement OAuth login", TaskState::Ready),
                mk_row("T2", "OAuth UX polish", TaskState::Chatting),
                mk_row("T3", "Merge queue checks", TaskState::Ready),
            ],
            filter_text: Some("oauth".to_string()),
            filter_state: Some(TaskState::Ready),
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        assert_eq!(state.filtered_tasks(), vec![0]);
    }

    #[test]
    fn sorted_tasks_by_name() {
        let state = DashboardState {
            tasks: vec![
                mk_row("T1", "Zulu", TaskState::Chatting),
                mk_row("T2", "Alpha", TaskState::Ready),
                mk_row("T3", "Bravo", TaskState::Submitting),
            ],
            sort_mode: SortMode::ByName,
            sort_reversed: false,
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        let ordered: Vec<&str> = state
            .sorted_tasks()
            .into_iter()
            .map(|task| task.title.as_str())
            .collect();
        assert_eq!(ordered, vec!["Alpha", "Bravo", "Zulu"]);
    }

    #[test]
    fn sorted_tasks_reversed() {
        let state = DashboardState {
            tasks: vec![
                mk_row("T1", "Alpha", TaskState::Chatting),
                mk_row("T2", "Bravo", TaskState::Ready),
                mk_row("T3", "Zulu", TaskState::Submitting),
            ],
            sort_mode: SortMode::ByName,
            sort_reversed: true,
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        let ordered: Vec<&str> = state
            .sorted_tasks()
            .into_iter()
            .map(|task| task.title.as_str())
            .collect();
        assert_eq!(ordered, vec!["Zulu", "Bravo", "Alpha"]);
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

    fn pane_with_lines(
        instance_id: &str,
        task_id: &str,
        model: ModelKind,
        lines: &[&str],
    ) -> AgentPane {
        let mut pane = AgentPane::new(instance_id, TaskId(task_id.to_string()), model);
        pane.status = AgentPaneStatus::Running;
        for line in lines {
            pane.append_line(*line);
        }
        pane
    }

    #[test]
    fn pane_window_with_history_keeps_live_tail_clean_until_scroll() {
        let state = DashboardState {
            panes: vec![
                pane_with_lines("A1", "T1", ModelKind::Claude, &["old 1", "old 2"]),
                pane_with_lines("A2", "T2", ModelKind::Codex, &["new 1", "new 2"]),
            ],
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        let window = state.pane_window_with_history(1, 20, 0);
        assert_eq!(window, vec!["new 1".to_string(), "new 2".to_string()]);
    }

    #[test]
    fn pane_window_with_history_reveals_previous_chat_on_scroll_up() {
        let state = DashboardState {
            panes: vec![
                pane_with_lines("A1", "T1", ModelKind::Claude, &["old 1", "old 2"]),
                pane_with_lines("A2", "T2", ModelKind::Codex, &["new 1"]),
            ],
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        let window = state.pane_window_with_history(1, 20, 1);
        assert!(window
            .iter()
            .any(|line| line.contains("--- previous chat A1")));
        assert!(window.iter().any(|line| line == "old 1"));
        assert!(window.iter().any(|line| line == "new 1"));
    }

    #[test]
    fn focused_scroll_budget_includes_previous_chat_history() {
        let mut state = DashboardState {
            panes: vec![
                pane_with_lines("A1", "T1", ModelKind::Claude, &["old 1", "old 2"]),
                pane_with_lines("A2", "T2", ModelKind::Codex, &["new 1"]),
            ],
            focused_pane_idx: Some(1),
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        state.scroll_up(50);
        assert!(state.scroll_back > state.panes[1].lines.len());
    }

    #[test]
    fn pane_matches_category_classifies_by_prefix() {
        assert!(pane_matches_category("agent-T1", PaneCategory::Agent));
        assert!(!pane_matches_category("agent-T1", PaneCategory::QA));
        assert!(pane_matches_category("qa-T1", PaneCategory::QA));
        assert!(!pane_matches_category("qa-T1", PaneCategory::Agent));
        assert!(pane_matches_category("pipeline-T1", PaneCategory::Agent));
    }

    #[test]
    fn pane_category_of_infers_from_instance_id() {
        assert_eq!(pane_category_of("agent-T1"), PaneCategory::Agent);
        assert_eq!(pane_category_of("qa-T1"), PaneCategory::QA);
        assert_eq!(pane_category_of("pipeline-T1"), PaneCategory::Agent);
        assert_eq!(pane_category_of("qa-post-T1"), PaneCategory::QA);
    }

    #[test]
    fn toggle_pane_category_switches_between_agent_and_qa() {
        let mut state = DashboardState::default();
        assert_eq!(state.selected_pane_category, PaneCategory::Agent);

        state.toggle_pane_category();
        assert_eq!(state.selected_pane_category, PaneCategory::QA);

        state.toggle_pane_category();
        assert_eq!(state.selected_pane_category, PaneCategory::Agent);
    }

    #[test]
    fn toggle_pane_category_snaps_to_matching_pane() {
        let mut state = DashboardState {
            tasks: vec![TaskOverviewRow::from_task(&mk_task("T1"))],
            panes: vec![
                pane_with_lines("agent-T1", "T1", ModelKind::Claude, &["agent output"]),
                pane_with_lines("qa-T1", "T1", ModelKind::Claude, &["qa output"]),
            ],
            selected_task_idx: 0,
            selected_pane_idx: 0,
            selected_pane_category: PaneCategory::Agent,
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        // Toggle to QA — should snap to the QA pane
        state.toggle_pane_category();
        assert_eq!(state.selected_pane_category, PaneCategory::QA);
        assert_eq!(state.selected_pane_idx, 1);

        // Toggle back to Agent — should snap to the Agent pane
        state.toggle_pane_category();
        assert_eq!(state.selected_pane_category, PaneCategory::Agent);
        assert_eq!(state.selected_pane_idx, 0);
    }

    #[test]
    fn pane_index_for_task_category_finds_correct_pane() {
        let state = DashboardState {
            panes: vec![
                pane_with_lines("agent-T1", "T1", ModelKind::Claude, &["agent"]),
                pane_with_lines("qa-T1", "T1", ModelKind::Claude, &["qa"]),
                pane_with_lines("agent-T2", "T2", ModelKind::Codex, &["agent2"]),
            ],
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        let tid = TaskId("T1".to_string());
        assert_eq!(state.pane_index_for_task_category(&tid, PaneCategory::Agent), Some(0));
        assert_eq!(state.pane_index_for_task_category(&tid, PaneCategory::QA), Some(1));

        let tid2 = TaskId("T2".to_string());
        assert_eq!(state.pane_index_for_task_category(&tid2, PaneCategory::Agent), Some(2));
        assert_eq!(state.pane_index_for_task_category(&tid2, PaneCategory::QA), None);
    }

    #[test]
    fn has_pane_in_category_checks_existence() {
        let state = DashboardState {
            panes: vec![
                pane_with_lines("agent-T1", "T1", ModelKind::Claude, &["agent"]),
                pane_with_lines("qa-T1", "T1", ModelKind::Claude, &["qa"]),
            ],
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        let tid = TaskId("T1".to_string());
        assert!(state.has_pane_in_category(&tid, PaneCategory::Agent));
        assert!(state.has_pane_in_category(&tid, PaneCategory::QA));

        let tid2 = TaskId("T2".to_string());
        assert!(!state.has_pane_in_category(&tid2, PaneCategory::Agent));
        assert!(!state.has_pane_in_category(&tid2, PaneCategory::QA));
    }

    #[test]
    fn snap_pane_to_task_prefers_current_category() {
        let mut state = DashboardState {
            tasks: vec![
                TaskOverviewRow::from_task(&mk_task("T1")),
                TaskOverviewRow::from_task(&mk_task("T2")),
            ],
            panes: vec![
                pane_with_lines("agent-T1", "T1", ModelKind::Claude, &["a1"]),
                pane_with_lines("qa-T1", "T1", ModelKind::Claude, &["q1"]),
                pane_with_lines("agent-T2", "T2", ModelKind::Codex, &["a2"]),
                pane_with_lines("qa-T2", "T2", ModelKind::Codex, &["q2"]),
            ],
            selected_task_idx: 0,
            selected_pane_idx: 0,
            selected_pane_category: PaneCategory::QA,
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        // Move to T2 — should snap to QA pane for T2 (current category)
        state.move_task_selection_next();
        assert_eq!(state.selected_task_idx, 1);
        assert_eq!(state.selected_pane_idx, 3); // qa-T2
        assert_eq!(state.selected_pane_category, PaneCategory::QA);
    }

    #[test]
    fn snap_pane_to_task_falls_back_when_category_missing() {
        let mut state = DashboardState {
            tasks: vec![
                TaskOverviewRow::from_task(&mk_task("T1")),
                TaskOverviewRow::from_task(&mk_task("T2")),
            ],
            panes: vec![
                pane_with_lines("agent-T1", "T1", ModelKind::Claude, &["a1"]),
                pane_with_lines("qa-T1", "T1", ModelKind::Claude, &["q1"]),
                pane_with_lines("agent-T2", "T2", ModelKind::Codex, &["a2"]),
                // No QA pane for T2
            ],
            selected_task_idx: 0,
            selected_pane_idx: 1, // qa-T1
            selected_pane_category: PaneCategory::QA,
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        // Move to T2 — QA not available, should fall back to Agent pane
        state.move_task_selection_next();
        assert_eq!(state.selected_task_idx, 1);
        assert_eq!(state.selected_pane_idx, 2); // agent-T2
        assert_eq!(state.selected_pane_category, PaneCategory::Agent);
    }

    #[test]
    fn move_pane_selection_toggles_category() {
        let mut state = DashboardState {
            tasks: vec![TaskOverviewRow::from_task(&mk_task("T1"))],
            panes: vec![
                pane_with_lines("agent-T1", "T1", ModelKind::Claude, &["agent"]),
                pane_with_lines("qa-T1", "T1", ModelKind::Claude, &["qa"]),
            ],
            selected_task_idx: 0,
            selected_pane_idx: 0,
            selected_pane_category: PaneCategory::Agent,
            recent_errors: vec![],
            log_root: None,
            timeline_events: vec![],
            show_timeline: false,
            ..DashboardState::default()
        };

        state.move_pane_selection_next();
        assert_eq!(state.selected_pane_category, PaneCategory::QA);
        assert_eq!(state.selected_pane_idx, 1);

        state.move_pane_selection_previous();
        assert_eq!(state.selected_pane_category, PaneCategory::Agent);
        assert_eq!(state.selected_pane_idx, 0);
    }
}
