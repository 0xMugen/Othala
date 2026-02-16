use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use orch_core::state::TaskState;
use orch_core::types::{ModelKind, Session, Task, TaskId};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::action::{action_label, map_key_to_command, UiAction, UiCommand};
use crate::event::TuiEvent;
use crate::model::{pane_category_of, AgentPane, AgentPaneStatus, DashboardState, SessionDisplay};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueuedAction {
    Dispatch {
        action: UiAction,
        task_id: Option<TaskId>,
        prompt: Option<String>,
        model: Option<ModelKind>,
    },
    CreateTask {
        repo: String,
        title: String,
        model: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    NewTaskDialog {
        /// Which field is active: 0=repo, 1=title, 2=model
        active_field: usize,
        repo: String,
        title: String,
        model: String,
    },
    NewChatPrompt {
        buffer: String,
    },
    ModelSelect {
        prompt: String,
        models: Vec<ModelKind>,
        selected: usize,
    },
    DeleteTaskConfirm {
        task_id: TaskId,
        branch: Option<String>,
    },
    HelpOverlay,
    FilterInput {
        buffer: String,
    },
    ChatInput {
        buffer: String,
        task_id: TaskId,
        /// The in-progress text the user was typing before navigating history.
        draft: String,
        /// `None` means showing the draft; `Some(i)` means showing `history[len - 1 - i]`.
        history_index: Option<usize>,
    },
    LogView {
        task_id: String,
        log_lines: Vec<String>,
        scroll_offset: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TuiApp {
    pub state: DashboardState,
    pub action_queue: VecDeque<QueuedAction>,
    pub input_mode: InputMode,
    pub should_quit: bool,
    /// Per-task history of submitted chat messages (most recent last).
    pub chat_history: HashMap<TaskId, Vec<String>>,
    all_tasks: Vec<crate::model::TaskOverviewRow>,
    session_task_ids: HashMap<String, Vec<TaskId>>,
    active_session_id: Option<String>,
}

impl Default for TuiApp {
    fn default() -> Self {
        Self {
            state: DashboardState::default(),
            action_queue: VecDeque::new(),
            input_mode: InputMode::Normal,
            should_quit: false,
            chat_history: HashMap::new(),
            all_tasks: Vec::new(),
            session_task_ids: HashMap::new(),
            active_session_id: None,
        }
    }
}

impl TuiApp {
    pub fn from_tasks(tasks: &[Task]) -> Self {
        let mut app = Self::default();
        app.set_tasks(tasks);
        app
    }

    pub fn set_tasks(&mut self, tasks: &[Task]) {
        // Build a lookup of existing QA data so we can carry it across refreshes.
        // (QA status is set by QAUpdate events and has no backing in the Task struct.)
        let prev_qa: std::collections::HashMap<_, _> = self
            .all_tasks
            .iter()
            .chain(self.state.tasks.iter())
            .filter(|t| {
                t.qa_status.is_some()
                    || !t.qa_tests.is_empty()
                    || !t.qa_targets.is_empty()
                    || t.estimated_tokens.is_some()
                    || t.estimated_cost_usd.is_some()
            })
            .map(|t| {
                (
                    t.task_id.clone(),
                    (
                        t.qa_status.clone(),
                        t.qa_tests.clone(),
                        t.qa_targets.clone(),
                        t.estimated_tokens,
                        t.estimated_cost_usd,
                    ),
                )
            })
            .collect();

        let rows: Vec<_> = tasks
            .iter()
            .map(|task| {
                let mut row = crate::model::TaskOverviewRow::from_task(task);
                if let Some((status, tests, targets, estimated_tokens, estimated_cost_usd)) =
                    prev_qa.get(&row.task_id)
                {
                    row.qa_status = status.clone();
                    row.qa_tests = tests.clone();
                    row.qa_targets = targets.clone();
                    row.estimated_tokens = *estimated_tokens;
                    row.estimated_cost_usd = *estimated_cost_usd;
                }
                row
            })
            .collect();
        self.all_tasks = rows;
        self.apply_active_session_filter();
    }

    pub fn set_sessions(&mut self, sessions: &[Session]) {
        self.state.sessions = sessions.iter().map(SessionDisplay::from_session).collect();
        self.session_task_ids = sessions
            .iter()
            .map(|session| (session.id.clone(), session.task_ids.clone()))
            .collect();
        self.state.ensure_selected_session_visible();

        if let Some(active) = self.active_session_id.as_ref() {
            if !self.session_task_ids.contains_key(active) {
                self.active_session_id = None;
            }
        }

        self.apply_active_session_filter();
    }

    fn apply_active_session_filter(&mut self) {
        if let Some(session_id) = self.active_session_id.as_ref() {
            if let Some(task_ids) = self.session_task_ids.get(session_id) {
                let allowed_ids: HashSet<&str> = task_ids.iter().map(|id| id.0.as_str()).collect();
                self.state.tasks = self
                    .all_tasks
                    .iter()
                    .filter(|task| allowed_ids.contains(task.task_id.0.as_str()))
                    .cloned()
                    .collect();
            } else {
                self.active_session_id = None;
                self.state.tasks = self.all_tasks.clone();
            }
        } else {
            self.state.tasks = self.all_tasks.clone();
        }
        self.state.ensure_selected_task_visible();
    }

    fn load_selected_session_tasks(&mut self) {
        let Some(session) = self.state.selected_session().cloned() else {
            self.state.status_line = "no sessions available".to_string();
            return;
        };

        self.active_session_id = Some(session.id.clone());
        self.apply_active_session_filter();
        self.state.show_sessions = false;
        self.state.status_line = format!(
            "loaded session {} ({} tasks)",
            session.id,
            self.state.tasks.len()
        );
    }

    pub fn set_panes(&mut self, panes: Vec<AgentPane>) {
        self.state.panes = panes;
        if self.state.selected_pane_idx >= self.state.panes.len() {
            self.state.selected_pane_idx = self.state.panes.len().saturating_sub(1);
        }
    }

    pub fn push_action(&mut self, action: UiAction) {
        let task_id = self.state.selected_task().map(|task| task.task_id.clone());
        self.state.status_line = match &task_id {
            Some(task_id) => format!("queued action={} task={}", action_label(action), task_id.0),
            None => format!("queued action={}", action_label(action)),
        };
        self.action_queue.push_back(QueuedAction::Dispatch {
            action,
            task_id,
            prompt: None,
            model: None,
        });
    }

    pub fn drain_actions(&mut self) -> Vec<QueuedAction> {
        self.action_queue.drain(..).collect()
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) {
        if key.kind != crossterm::event::KeyEventKind::Press {
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        if self.handle_input_mode_key(key) {
            return;
        }

        if matches!(self.input_mode, InputMode::Normal) && key.code == KeyCode::Char('N') {
            self.begin_new_task_dialog();
            return;
        }

        if matches!(self.input_mode, InputMode::Normal) && key.code == KeyCode::Char('t') {
            self.state.show_timeline = !self.state.show_timeline;
            self.state.status_line = if self.state.show_timeline {
                "timeline shown".to_string()
            } else {
                "timeline hidden".to_string()
            };
            return;
        }

        if matches!(self.input_mode, InputMode::Normal)
            && self.state.show_sessions
            && !self.state.focused_task
            && self.state.focused_pane_idx.is_none()
        {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.state.move_session_selection_previous();
                    let selected = self
                        .state
                        .selected_session()
                        .map(|session| session.id.as_str())
                        .unwrap_or("-");
                    self.state.status_line = format!("session selected: {selected}");
                    return;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.state.move_session_selection_next();
                    let selected = self
                        .state
                        .selected_session()
                        .map(|session| session.id.as_str())
                        .unwrap_or("-");
                    self.state.status_line = format!("session selected: {selected}");
                    return;
                }
                KeyCode::Enter => {
                    self.load_selected_session_tasks();
                    return;
                }
                _ => {}
            }
        }

        if matches!(self.input_mode, InputMode::Normal)
            && self.state.focused_task
            && key.code == KeyCode::Char('l')
        {
            let Some(task_id) = self.state.selected_task().map(|task| task.task_id.0.clone()) else {
                self.state.status_line = "no task selected for log view".to_string();
                return;
            };

            let log_lines = self
                .state
                .log_root
                .as_deref()
                .map(|root| load_agent_log(root, &task_id))
                .unwrap_or_else(|| vec!["No log output available.".to_string()]);

            self.input_mode = InputMode::LogView {
                task_id: task_id.clone(),
                log_lines,
                scroll_offset: 0,
            };
            self.state.status_line = format!("agent log view: {task_id}");
            return;
        }

        if key.code == KeyCode::Esc {
            if self.state.focused_task {
                self.state.focused_task = false;
                self.state.scroll_back = 0;
                self.state.status_line = "task detail closed".to_string();
                return;
            }
            if self.state.focused_pane_idx.is_some() {
                self.state.focused_pane_idx = None;
                self.state.scroll_back = 0;
                self.state.status_line = "pane focus cleared".to_string();
                return;
            }
            self.should_quit = true;
            return;
        }

        // In focused views, arrow keys and page keys scroll content.
        if self.state.focused_task || self.state.focused_pane_idx.is_some() {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.state.scroll_up(1);
                    return;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.state.scroll_down(1);
                    return;
                }
                KeyCode::Left | KeyCode::Right => {
                    self.state.toggle_pane_category();
                    if self.state.focused_pane_idx.is_some() {
                        self.state.focused_pane_idx = Some(self.state.selected_pane_idx);
                    }
                    self.state.scroll_back = 0;
                    return;
                }
                KeyCode::PageUp => {
                    self.state.scroll_up(20);
                    return;
                }
                KeyCode::PageDown => {
                    self.state.scroll_down(20);
                    return;
                }
                KeyCode::Home => {
                    self.state.scroll_to_top();
                    return;
                }
                KeyCode::End => {
                    self.state.scroll_to_bottom();
                    return;
                }
                _ => {} // fall through to normal command handling
            }
        }

        let Some(command) = map_key_to_command(key) else {
            return;
        };

        match command {
            UiCommand::Dispatch(UiAction::CreateTask) => self.begin_new_chat_prompt(),
            UiCommand::Dispatch(UiAction::DeleteTask) => self.begin_delete_task_confirmation(),
            UiCommand::Dispatch(UiAction::SendChatMessage) => self.begin_chat_input(),
            UiCommand::Dispatch(action) => self.push_action(action),
            UiCommand::SelectNextTask => self.state.move_task_selection_next(),
            UiCommand::SelectPreviousTask => self.state.move_task_selection_previous(),
            UiCommand::SelectNextPane => self.state.move_pane_selection_next(),
            UiCommand::SelectPreviousPane => self.state.move_pane_selection_previous(),
            UiCommand::ScrollUp => self.state.scroll_up(5),
            UiCommand::ScrollDown => self.state.scroll_down(5),
            UiCommand::ScrollToTop => self.state.scroll_to_top(),
            UiCommand::ScrollToBottom => self.state.scroll_to_bottom(),
            UiCommand::GoToFirstTask => {
                self.state.selected_task_idx = 0;
                self.state.ensure_selected_task_visible();
            }
            UiCommand::GoToLastTask => {
                self.state.selected_task_idx = self.state.tasks.len().saturating_sub(1);
                self.state.ensure_selected_task_visible();
            }
            UiCommand::CycleTheme => {
                self.state.cycle_theme();
            }
            UiCommand::CycleSort => {
                self.state.sort_mode = self.state.sort_mode.next();
            }
            UiCommand::ToggleSortReverse => {
                self.state.sort_reversed = !self.state.sort_reversed;
            }
            UiCommand::StartFilter => self.begin_filter_input(),
            UiCommand::CycleStateFilter => self.cycle_state_filter(),
            UiCommand::ToggleFocusedPane => {
                if self.state.focused_pane_idx.is_some() {
                    self.state.focused_pane_idx = None;
                    self.state.scroll_back = 0;
                    self.state.status_line = "pane focus cleared".to_string();
                } else if !self.state.panes.is_empty() {
                    self.state.focused_pane_idx = Some(self.state.selected_pane_idx);
                    self.state.focused_task = false;
                    self.state.scroll_back = 0;
                    self.state.status_line = format!(
                        "focused pane {}",
                        self.state.selected_pane_idx.saturating_add(1)
                    );
                }
            }
            UiCommand::ToggleFocusedTask => {
                if self.state.focused_task {
                    self.state.focused_task = false;
                    self.state.scroll_back = 0;
                    self.state.status_line = "task detail closed".to_string();
                } else if !self.state.tasks.is_empty() {
                    self.state.focused_task = true;
                    self.state.focused_pane_idx = None;
                    self.state.scroll_back = 0;
                    let task_id = self
                        .state
                        .selected_task()
                        .map(|t| t.task_id.0.clone())
                        .unwrap_or_default();
                    self.state.status_line = format!("task detail: {task_id}");
                }
            }
            UiCommand::ShowHelp => self.input_mode = InputMode::HelpOverlay,
            UiCommand::Quit => self.should_quit = true,
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        match &mut self.input_mode {
            InputMode::NewChatPrompt { buffer }
            | InputMode::FilterInput { buffer }
            | InputMode::ChatInput { buffer, .. } => {
                buffer.push_str(&normalize_paste_text(text));
            }
            InputMode::NewTaskDialog {
                active_field,
                repo,
                title,
                model,
            } => {
                let normalized = normalize_paste_text(text);
                match *active_field {
                    0 => repo.push_str(&normalized),
                    1 => title.push_str(&normalized),
                    _ => model.push_str(&normalized),
                }
            }
            _ => {}
        }
    }

    fn begin_new_chat_prompt(&mut self) {
        self.input_mode = InputMode::NewChatPrompt {
            buffer: String::new(),
        };
        self.state.status_line =
            "new chat prompt: type feature request, Enter=submit Esc=cancel".to_string();
    }

    fn begin_new_task_dialog(&mut self) {
        self.input_mode = InputMode::NewTaskDialog {
            active_field: 0,
            repo: "default".to_string(),
            title: String::new(),
            model: "claude".to_string(),
        };
        self.state.status_line =
            "new task dialog: Tab=next field Enter=create Esc=cancel".to_string();
    }

    fn begin_chat_input(&mut self) {
        let Some(task) = self.state.selected_task() else {
            self.state.status_line = "no task selected for chat".to_string();
            return;
        };
        let task_id = task.task_id.clone();
        self.input_mode = InputMode::ChatInput {
            buffer: String::new(),
            task_id,
            draft: String::new(),
            history_index: None,
        };
        self.state.status_line =
            "chat input: type message, Enter=send Up/Down=history Esc=cancel".to_string();
    }

    fn begin_filter_input(&mut self) {
        self.input_mode = InputMode::FilterInput {
            buffer: self.state.filter_text.clone().unwrap_or_default(),
        };
        self.state.status_line =
            "filter input: type query, Enter=apply Esc=cancel".to_string();
    }

    fn cycle_state_filter(&mut self) {
        self.state.filter_state = match self.state.filter_state {
            None => Some(TaskState::Chatting),
            Some(TaskState::Chatting) => Some(TaskState::Ready),
            Some(TaskState::Ready) => Some(TaskState::Submitting),
            Some(TaskState::Submitting) => Some(TaskState::AwaitingMerge),
            Some(TaskState::AwaitingMerge) => Some(TaskState::Stopped),
            Some(TaskState::Stopped) => Some(TaskState::Merged),
            Some(TaskState::Merged) | Some(TaskState::Restacking) => None,
        };
        self.state.ensure_selected_task_visible();
        self.state.status_line = match self.state.active_filter_label() {
            Some(label) => format!("filter updated: {label}"),
            None => "filter cleared".to_string(),
        };
    }

    fn handle_chat_input_key(&mut self, key: KeyEvent) {
        let task_id = match &self.input_mode {
            InputMode::ChatInput { task_id, .. } => task_id.clone(),
            _ => return,
        };

        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.state.status_line = "chat input canceled".to_string();
            }
            KeyCode::Enter => {
                let message = match &self.input_mode {
                    InputMode::ChatInput { buffer, .. } => buffer.trim().to_string(),
                    _ => return,
                };
                if message.is_empty() {
                    self.state.status_line = "chat message cannot be empty".to_string();
                    return;
                }
                self.chat_history
                    .entry(task_id.clone())
                    .or_default()
                    .push(message.clone());
                self.action_queue.push_back(QueuedAction::Dispatch {
                    action: UiAction::SendChatMessage,
                    task_id: Some(task_id.clone()),
                    prompt: Some(message),
                    model: None,
                });
                self.input_mode = InputMode::Normal;
                self.state.status_line =
                    format!("queued action=send_chat_message task={}", task_id.0);
            }
            KeyCode::Up => {
                let history_len = self
                    .chat_history
                    .get(&task_id)
                    .map_or(0, |h| h.len());
                if history_len == 0 {
                    return;
                }
                if let InputMode::ChatInput {
                    buffer,
                    draft,
                    history_index,
                    ..
                } = &mut self.input_mode
                {
                    let new_idx = match *history_index {
                        None => {
                            *draft = buffer.clone();
                            0
                        }
                        Some(i) if i + 1 < history_len => i + 1,
                        Some(_) => return,
                    };
                    *history_index = Some(new_idx);
                    *buffer = self.chat_history[&task_id][history_len - 1 - new_idx].clone();
                }
            }
            KeyCode::Down => {
                if let InputMode::ChatInput {
                    buffer,
                    draft,
                    history_index,
                    ..
                } = &mut self.input_mode
                {
                    match *history_index {
                        None => {}
                        Some(0) => {
                            *history_index = None;
                            *buffer = draft.clone();
                        }
                        Some(i) => {
                            let new_idx = i - 1;
                            *history_index = Some(new_idx);
                            let history = &self.chat_history[&task_id];
                            *buffer = history[history.len() - 1 - new_idx].clone();
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if let InputMode::ChatInput {
                    buffer,
                    history_index,
                    ..
                } = &mut self.input_mode
                {
                    buffer.pop();
                    *history_index = None;
                }
            }
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    if let InputMode::ChatInput {
                        buffer,
                        history_index,
                        ..
                    } = &mut self.input_mode
                    {
                        buffer.push(ch);
                        *history_index = None;
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_new_task_dialog_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.state.status_line = "new task dialog canceled".to_string();
            }
            KeyCode::Tab => {
                if let InputMode::NewTaskDialog { active_field, .. } = &mut self.input_mode {
                    *active_field = (*active_field + 1) % 3;
                }
            }
            KeyCode::Backspace => {
                if let InputMode::NewTaskDialog {
                    active_field,
                    repo,
                    title,
                    model,
                } = &mut self.input_mode
                {
                    match *active_field {
                        0 => {
                            repo.pop();
                        }
                        1 => {
                            title.pop();
                        }
                        _ => {
                            model.pop();
                        }
                    }
                }
            }
            KeyCode::Char(ch) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return;
                }
                if let InputMode::NewTaskDialog {
                    active_field,
                    repo,
                    title,
                    model,
                } = &mut self.input_mode
                {
                    match *active_field {
                        0 => repo.push(ch),
                        1 => title.push(ch),
                        _ => model.push(ch),
                    }
                }
            }
            KeyCode::Enter => {
                let (repo_raw, title, model_raw) = match &self.input_mode {
                    InputMode::NewTaskDialog {
                        repo, title, model, ..
                    } => (
                        repo.trim().to_string(),
                        title.trim().to_string(),
                        model.trim().to_string(),
                    ),
                    _ => return,
                };
                let repo = if repo_raw.is_empty() {
                    "default".to_string()
                } else {
                    repo_raw
                };
                let model = if model_raw.is_empty() {
                    "claude".to_string()
                } else {
                    model_raw
                };
                self.action_queue.push_back(QueuedAction::CreateTask {
                    repo: repo.clone(),
                    title,
                    model: model.clone(),
                });
                self.input_mode = InputMode::Normal;
                self.state.status_line = format!("queued create_task repo={repo} model={model}");
            }
            _ => {}
        }
    }

    fn handle_log_view_key(&mut self, key: KeyEvent) {
        let visible_height = log_view_visible_height();
        let mut close_requested = false;

        if let InputMode::LogView {
            log_lines,
            scroll_offset,
            ..
        } = &mut self.input_mode
        {
            match key.code {
                KeyCode::Esc => {
                    close_requested = true;
                }
                KeyCode::Char('j') | KeyCode::Down | KeyCode::PageDown => {
                    *scroll_offset = (*scroll_offset + 1).min(log_lines.len());
                }
                KeyCode::Char('k') | KeyCode::Up | KeyCode::PageUp => {
                    *scroll_offset = scroll_offset.saturating_sub(1);
                }
                KeyCode::Char('G') => {
                    *scroll_offset = log_lines.len().saturating_sub(visible_height);
                }
                KeyCode::Char('g') => {
                    *scroll_offset = 0;
                }
                _ => {}
            }
        }

        if close_requested {
            self.input_mode = InputMode::Normal;
            self.state.status_line = "agent log view closed".to_string();
        }
    }

    fn begin_delete_task_confirmation(&mut self) {
        let Some(task) = self.state.selected_task() else {
            self.state.status_line = "no task selected to delete".to_string();
            return;
        };
        let branch = if task.branch.trim().is_empty() || task.branch == "-" {
            None
        } else {
            Some(task.branch.clone())
        };
        self.input_mode = InputMode::DeleteTaskConfirm {
            task_id: task.task_id.clone(),
            branch,
        };
        self.state.status_line = format!(
            "confirm delete task {}: Enter=delete Esc=cancel",
            task.task_id.0
        );
    }

    pub fn input_prompt(&self) -> Option<&str> {
        match &self.input_mode {
            InputMode::Normal => None,
            InputMode::NewTaskDialog { .. } => None,
            InputMode::NewChatPrompt { buffer } => Some(buffer.as_str()),
            InputMode::FilterInput { buffer } => Some(buffer.as_str()),
            InputMode::ChatInput { buffer, .. } => Some(buffer.as_str()),
            InputMode::ModelSelect { prompt, .. } => Some(prompt.as_str()),
            InputMode::DeleteTaskConfirm { .. } => None,
            InputMode::HelpOverlay => None,
            InputMode::LogView { .. } => None,
        }
    }

    pub fn log_view_display(&self) -> Option<(&str, &[String], usize)> {
        match &self.input_mode {
            InputMode::LogView {
                task_id,
                log_lines,
                scroll_offset,
            } => Some((task_id.as_str(), log_lines.as_slice(), *scroll_offset)),
            _ => None,
        }
    }

    pub fn model_select_display(&self) -> Option<(&[ModelKind], usize)> {
        match &self.input_mode {
            InputMode::ModelSelect {
                models, selected, ..
            } => Some((models, *selected)),
            _ => None,
        }
    }

    pub fn chat_input_display(&self) -> Option<(&str, &TaskId)> {
        match &self.input_mode {
            InputMode::ChatInput { buffer, task_id, .. } => Some((buffer.as_str(), task_id)),
            _ => None,
        }
    }

    pub fn delete_confirm_display(&self) -> Option<(&TaskId, Option<&str>)> {
        match &self.input_mode {
            InputMode::DeleteTaskConfirm { task_id, branch } => Some((task_id, branch.as_deref())),
            _ => None,
        }
    }

    pub fn new_task_dialog_display(&self) -> Option<(usize, &str, &str, &str)> {
        match &self.input_mode {
            InputMode::NewTaskDialog {
                active_field,
                repo,
                title,
                model,
            } => Some((*active_field, repo.as_str(), title.as_str(), model.as_str())),
            _ => None,
        }
    }

    fn handle_input_mode_key(&mut self, key: KeyEvent) -> bool {
        // Handle ChatInput before the match to avoid double &mut self borrow.
        if matches!(self.input_mode, InputMode::ChatInput { .. }) {
            self.handle_chat_input_key(key);
            return true;
        }
        if matches!(self.input_mode, InputMode::NewTaskDialog { .. }) {
            self.handle_new_task_dialog_key(key);
            return true;
        }
        if matches!(self.input_mode, InputMode::LogView { .. }) {
            self.handle_log_view_key(key);
            return true;
        }
        match &mut self.input_mode {
            InputMode::Normal => return false,
            InputMode::NewTaskDialog { .. } => unreachable!(),
            InputMode::LogView { .. } => unreachable!(),
            InputMode::HelpOverlay => match key.code {
                KeyCode::Esc | KeyCode::Char('?') => {
                    self.input_mode = InputMode::Normal;
                }
                _ => {}
            },
            InputMode::NewChatPrompt { buffer } => match key.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                    self.state.status_line = "new chat prompt canceled".to_string();
                }
                KeyCode::Enter => {
                    let prompt = buffer.trim().to_string();
                    if prompt.is_empty() {
                        self.state.status_line = "new chat prompt cannot be empty".to_string();
                        return true;
                    }
                    self.input_mode = InputMode::ModelSelect {
                        prompt,
                        models: vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini],
                        selected: 0,
                    };
                    self.state.status_line =
                        "select model: Up/Down=cycle Enter=confirm Esc=cancel".to_string();
                }
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Char(ch) => {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        buffer.push(ch);
                    }
                }
                _ => {}
            },
            InputMode::FilterInput { buffer } => match key.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                    self.state.status_line = "filter canceled".to_string();
                }
                KeyCode::Enter => {
                    let query = buffer.trim().to_string();
                    self.state.filter_text = if query.is_empty() { None } else { Some(query) };
                    self.state.ensure_selected_task_visible();
                    self.input_mode = InputMode::Normal;
                    self.state.status_line = match self.state.active_filter_label() {
                        Some(label) => format!("filter applied: {label}"),
                        None => "filter cleared".to_string(),
                    };
                }
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Char(ch) => {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        buffer.push(ch);
                    }
                }
                _ => {}
            },
            InputMode::ModelSelect {
                prompt,
                models,
                selected,
            } => match key.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                    self.state.status_line = "model selection canceled".to_string();
                }
                KeyCode::Up => {
                    if *selected == 0 {
                        *selected = models.len().saturating_sub(1);
                    } else {
                        *selected -= 1;
                    }
                }
                KeyCode::Down => {
                    *selected = (*selected + 1) % models.len();
                }
                KeyCode::Enter => {
                    let chosen_model = models[*selected];
                    let prompt_value = prompt.clone();
                    let task_id = self.state.selected_task().map(|task| task.task_id.clone());
                    self.action_queue.push_back(QueuedAction::Dispatch {
                        action: UiAction::CreateTask,
                        task_id,
                        prompt: Some(prompt_value),
                        model: Some(chosen_model),
                    });
                    self.input_mode = InputMode::Normal;
                    self.state.status_line =
                        format!("queued action=create_task (chat) model={:?}", chosen_model);
                }
                _ => {}
            },
            InputMode::ChatInput { .. } => unreachable!(),
            InputMode::DeleteTaskConfirm { task_id, .. } => match key.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                    self.state.status_line = "delete task canceled".to_string();
                }
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let confirmed_task_id = task_id.clone();
                    self.action_queue.push_back(QueuedAction::Dispatch {
                        action: UiAction::DeleteTask,
                        task_id: Some(confirmed_task_id.clone()),
                        prompt: None,
                        model: None,
                    });
                    self.input_mode = InputMode::Normal;
                    self.state.status_line =
                        format!("queued action=delete_task task={}", confirmed_task_id.0);
                }
                _ => {}
            },
        }
        true
    }

    pub fn apply_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::TasksReplaced { tasks } => self.set_tasks(&tasks),
            TuiEvent::AgentPaneOutput {
                instance_id,
                task_id,
                model,
                lines,
            } => {
                let idx = self.ensure_pane_index(&instance_id, task_id, model);
                let pane = &mut self.state.panes[idx];
                if pane.status != AgentPaneStatus::Failed && pane.status != AgentPaneStatus::Exited
                {
                    pane.status = AgentPaneStatus::Running;
                }
                for line in lines {
                    pane.append_line(line);
                }
            }
            TuiEvent::AgentPaneStatusChanged {
                instance_id,
                status,
            } => {
                if let Some(idx) = self.pane_index_by_instance(&instance_id) {
                    let pane = &mut self.state.panes[idx];
                    pane.status = status;
                    pane.updated_at = Utc::now();
                    match status {
                        AgentPaneStatus::Failed => {
                            self.state.status_line = format!("pane failed: {instance_id}");
                        }
                        AgentPaneStatus::Exited => {
                            self.state.status_line = format!("pane exited: {instance_id}");
                        }
                        AgentPaneStatus::Stopped => {
                            self.state.status_line =
                                format!("pane stopped: {instance_id}");
                        }
                        AgentPaneStatus::Starting
                        | AgentPaneStatus::Running
                        | AgentPaneStatus::Waiting => {}
                    }
                } else {
                    self.state.status_line = format!("pane not found: {instance_id}");
                }
            }
            TuiEvent::StatusLine { message } => {
                self.state.status_line = message;
            }
            TuiEvent::QAUpdate {
                task_id,
                status,
                tests,
                targets,
            } => {
                if let Some(task) = self
                    .state
                    .tasks
                    .iter_mut()
                    .find(|t| t.task_id == task_id)
                {
                    task.qa_status = Some(status);
                    task.qa_tests = tests;
                    task.qa_targets = targets;
                }
            }
        }
    }

    fn pane_index_by_instance(&self, instance_id: &str) -> Option<usize> {
        self.state
            .panes
            .iter()
            .position(|pane| pane.instance_id == instance_id)
    }

    fn ensure_pane_index(&mut self, instance_id: &str, task_id: TaskId, model: ModelKind) -> usize {
        if let Some(idx) = self.pane_index_by_instance(instance_id) {
            return idx;
        }

        // Only reuse a non-running pane for the same task AND same category
        // (agent vs QA). This prevents QA from overwriting agent pane slots
        // and vice versa.
        let new_category = pane_category_of(instance_id);
        if let Some(idx) = self
            .state
            .panes
            .iter()
            .position(|pane| {
                pane.task_id == task_id
                    && pane.status != AgentPaneStatus::Running
                    && pane_category_of(&pane.instance_id) == new_category
            })
        {
            let pane = &mut self.state.panes[idx];
            pane.instance_id = instance_id.to_string();
            pane.model = model;
            pane.status = AgentPaneStatus::Running;
            return idx;
        }

        let mut pane = AgentPane::new(instance_id.to_string(), task_id, model);
        pane.status = AgentPaneStatus::Running;
        self.state.panes.push(pane);
        if self.state.panes.len() == 1 {
            self.state.selected_pane_idx = 0;
        }
        self.state.panes.len() - 1
    }
}

fn normalize_paste_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

const LOG_VIEW_DEFAULT_VISIBLE_HEIGHT: usize = 20;

fn log_view_visible_height() -> usize {
    crossterm::terminal::size()
        .map(|(_, height)| height.saturating_sub(4) as usize)
        .ok()
        .filter(|height| *height > 0)
        .unwrap_or(LOG_VIEW_DEFAULT_VISIBLE_HEIGHT)
}

fn load_agent_log(log_root: &str, task_id: &str) -> Vec<String> {
    let path = format!("{log_root}/{task_id}/latest.log");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| "No log output available.".to_string())
        .lines()
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use orch_core::state::TaskState;
    use orch_core::types::{ModelKind, RepoId, Session, SessionStatus, Task, TaskId};
    use std::path::PathBuf;
    use crate::{
        AgentPane, AgentPaneStatus, QueuedAction, SessionDisplay, SortMode, TaskOverviewRow,
        TuiApp, TuiEvent, UiAction,
    };

    fn assert_dispatch_action(
        queued: &QueuedAction,
        expected_action: UiAction,
        expected_task_id: Option<TaskId>,
        expected_prompt: Option<&str>,
        expected_model: Option<ModelKind>,
    ) {
        match queued {
            QueuedAction::Dispatch {
                action,
                task_id,
                prompt,
                model,
            } => {
                assert_eq!(*action, expected_action);
                assert_eq!(task_id, &expected_task_id);
                assert_eq!(prompt.as_deref(), expected_prompt);
                assert_eq!(*model, expected_model);
            }
            QueuedAction::CreateTask { .. } => panic!("expected dispatch action"),
        }
    }

    #[test]
    fn apply_event_agent_output_creates_and_updates_pane() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Codex,
            lines: vec!["line one".to_string(), "line two".to_string()],
        });

        assert_eq!(app.state.panes.len(), 1);
        let pane = &app.state.panes[0];
        assert_eq!(pane.instance_id, "A1");
        assert_eq!(pane.task_id, TaskId("T1".to_string()));
        assert_eq!(pane.model, ModelKind::Codex);
        assert_eq!(pane.status, AgentPaneStatus::Running);
        assert_eq!(
            pane.tail(10),
            vec!["line one".to_string(), "line two".to_string()]
        );
    }

    #[test]
    fn apply_event_status_change_updates_existing_pane() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["boot".to_string()],
        });
        app.apply_event(TuiEvent::AgentPaneStatusChanged {
            instance_id: "A1".to_string(),
            status: AgentPaneStatus::Waiting,
        });

        assert_eq!(app.state.panes[0].status, AgentPaneStatus::Waiting);
        assert_eq!(app.state.status_line, "ready");
    }

    #[test]
    fn apply_event_terminal_status_updates_status_line() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["boot".to_string()],
        });
        app.apply_event(TuiEvent::AgentPaneStatusChanged {
            instance_id: "A1".to_string(),
            status: AgentPaneStatus::Exited,
        });
        assert_eq!(app.state.status_line, "pane exited: A1");

        app.apply_event(TuiEvent::AgentPaneStatusChanged {
            instance_id: "A1".to_string(),
            status: AgentPaneStatus::Failed,
        });
        assert_eq!(app.state.status_line, "pane failed: A1");
    }

    #[test]
    fn apply_event_agent_output_reuses_existing_non_running_task_pane() {
        let mut app = TuiApp::default();
        app.state.panes.push(AgentPane {
            instance_id: "H-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            status: AgentPaneStatus::Exited,
            updated_at: Utc::now(),
            lines: std::collections::VecDeque::from(vec!["history".to_string()]),
        });

        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "A-T1-0".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Codex,
            lines: vec!["new line".to_string()],
        });

        assert_eq!(app.state.panes.len(), 1);
        let pane = &app.state.panes[0];
        assert_eq!(pane.instance_id, "A-T1-0");
        assert_eq!(pane.model, ModelKind::Codex);
        assert_eq!(pane.status, AgentPaneStatus::Running);
        assert_eq!(
            pane.tail(10),
            vec!["history".to_string(), "new line".to_string()]
        );
    }

    #[test]
    fn apply_event_unknown_pane_status_change_sets_error_status_line() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneStatusChanged {
            instance_id: "missing".to_string(),
            status: AgentPaneStatus::Failed,
        });
        assert_eq!(app.state.status_line, "pane not found: missing");
    }

    #[test]
    fn push_action_attaches_selected_task_id() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![
            TaskOverviewRow {
                task_id: TaskId("T1".to_string()),
                repo_id: RepoId("example".to_string()),
                title: "Task T1".to_string(),
                branch: "task/T1".to_string(),
                stack_position: None,
                state: TaskState::Chatting,
                display_state: "Chatting".to_string(),
                verify_summary: "not_run".to_string(),
                last_activity: Utc::now(),
                qa_status: None,
                qa_tests: Vec::new(),
                qa_targets: Vec::new(),
                estimated_tokens: None,
                estimated_cost_usd: None,
                retry_count: 0,
                retry_history: Vec::new(),
                depends_on_display: Vec::new(),
                pr_url: None,
                model_display: None,
            },
            TaskOverviewRow {
                task_id: TaskId("T2".to_string()),
                repo_id: RepoId("example".to_string()),
                title: "Task T2".to_string(),
                branch: "task/T2".to_string(),
                stack_position: None,
                state: TaskState::Chatting,
                display_state: "Chatting".to_string(),
                verify_summary: "not_run".to_string(),
                last_activity: Utc::now(),
                qa_status: None,
                qa_tests: Vec::new(),
                qa_targets: Vec::new(),
                estimated_tokens: None,
                estimated_cost_usd: None,
                retry_count: 0,
                retry_history: Vec::new(),
                depends_on_display: Vec::new(),
                pr_url: None,
                model_display: None,
            },
        ];
        app.state.selected_task_idx = 1;

        app.push_action(UiAction::RunVerifyQuick);
        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(
            &drained[0],
            UiAction::RunVerifyQuick,
            Some(TaskId("T2".to_string())),
            None,
            None,
        );
        assert_eq!(
            app.state.status_line,
            "queued action=run_verify_quick task=T2"
        );
    }

    #[test]
    fn push_action_uses_none_task_id_when_no_tasks_exist() {
        let mut app = TuiApp::default();
        app.push_action(UiAction::TriggerRestack);

        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(&drained[0], UiAction::TriggerRestack, None, None, None);
        assert_eq!(app.state.status_line, "queued action=trigger_restack");
    }

    #[test]
    fn set_panes_clamps_selected_index_when_list_shrinks() {
        let mut app = TuiApp::default();
        app.state.selected_pane_idx = 2;

        app.set_panes(vec![
            AgentPane::new("A1", TaskId("T1".to_string()), ModelKind::Codex),
            AgentPane::new("A2", TaskId("T2".to_string()), ModelKind::Claude),
        ]);
        assert_eq!(app.state.selected_pane_idx, 1);

        app.set_panes(vec![]);
        assert_eq!(app.state.selected_pane_idx, 0);
    }

    #[test]
    fn handle_key_event_dispatches_action_from_keymap() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            display_state: "Chatting".to_string(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(
            &drained[0],
            UiAction::RunVerifyQuick,
            Some(TaskId("T1".to_string())),
            None,
            None,
        );
    }

    #[test]
    fn question_mark_opens_help_overlay() {
        let mut app = TuiApp::default();

        app.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::HelpOverlay));
    }

    #[test]
    fn help_overlay_renders() {
        let mut app = TuiApp::default();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::HelpOverlay));
        assert!(!app.should_quit);
    }

    #[test]
    fn esc_closes_help_overlay() {
        let mut app = TuiApp {
            input_mode: super::InputMode::HelpOverlay,
            ..Default::default()
        };

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::Normal));
        assert!(!app.should_quit);
    }

    #[test]
    fn question_mark_closes_help_overlay() {
        let mut app = TuiApp {
            input_mode: super::InputMode::HelpOverlay,
            ..Default::default()
        };

        app.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::Normal));
    }

    #[test]
    fn slash_key_enters_filter_mode() {
        let mut app = TuiApp::default();

        app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));

        assert!(matches!(
            app.input_mode,
            super::InputMode::FilterInput { .. }
        ));
    }

    #[test]
    fn clear_filter_on_esc() {
        let mut app = TuiApp::default();

        app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for ch in "oauth".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::Normal));
        assert_eq!(app.state.filter_text, None);
    }

    #[test]
    fn enter_applies_filter_text() {
        let mut app = TuiApp::default();

        app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for ch in "oauth".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::Normal));
        assert_eq!(app.state.filter_text.as_deref(), Some("oauth"));
    }

    #[test]
    fn f_key_cycles_state_filter() {
        let mut app = TuiApp::default();

        app.handle_key_event(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT));
        assert_eq!(app.state.filter_state, Some(TaskState::Chatting));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT));
        assert_eq!(app.state.filter_state, Some(TaskState::Ready));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT));
        assert_eq!(app.state.filter_state, Some(TaskState::Submitting));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT));
        assert_eq!(app.state.filter_state, Some(TaskState::AwaitingMerge));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT));
        assert_eq!(app.state.filter_state, Some(TaskState::Stopped));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT));
        assert_eq!(app.state.filter_state, Some(TaskState::Merged));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT));
        assert_eq!(app.state.filter_state, None);
    }

    #[test]
    fn s_key_changes_sort_mode() {
        let mut app = TuiApp::default();
        assert_eq!(app.state.sort_mode, SortMode::ByState);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        assert_eq!(app.state.sort_mode, SortMode::ByPriority);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        assert_eq!(app.state.sort_mode, SortMode::ByLastActivity);
    }

    #[test]
    fn set_sessions_builds_session_display_rows() {
        let mut app = TuiApp::default();
        let now = Utc::now();
        let sessions = vec![Session {
            id: "S1".to_string(),
            title: "Session One".to_string(),
            created_at: now,
            updated_at: now,
            task_ids: vec![TaskId("T1".to_string()), TaskId("T2".to_string())],
            parent_session_id: None,
            status: SessionStatus::Active,
        }];

        app.set_sessions(&sessions);
        assert_eq!(app.state.sessions.len(), 1);
        let display = &app.state.sessions[0];
        assert_eq!(display.id, "S1");
        assert_eq!(display.title, "Session One");
        assert_eq!(display.status, SessionStatus::Active);
        assert_eq!(display.task_count, 2);
        assert_eq!(display.updated_at, now);
    }

    #[test]
    fn up_down_navigates_session_list_when_visible() {
        let mut app = TuiApp::default();
        let now = Utc::now();
        app.state.sessions = vec![
            SessionDisplay {
                id: "S1".to_string(),
                title: "Session One".to_string(),
                status: SessionStatus::Active,
                task_count: 1,
                updated_at: now,
            },
            SessionDisplay {
                id: "S2".to_string(),
                title: "Session Two".to_string(),
                status: SessionStatus::Completed,
                task_count: 2,
                updated_at: now,
            },
        ];
        app.state.show_sessions = true;

        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.state.session_list_index, 1);

        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.state.session_list_index, 0);
    }

    #[test]
    fn enter_loads_selected_session_tasks_and_hides_sessions() {
        let mut app = TuiApp::default();
        let mut task_one = Task::new(
            TaskId("T1".to_string()),
            RepoId("example".to_string()),
            "Task T1".to_string(),
            PathBuf::from(".orch/wt/T1"),
        );
        task_one.updated_at = Utc::now();
        let mut task_two = Task::new(
            TaskId("T2".to_string()),
            RepoId("example".to_string()),
            "Task T2".to_string(),
            PathBuf::from(".orch/wt/T2"),
        );
        task_two.updated_at = Utc::now();
        app.set_tasks(&[task_one, task_two]);

        let now = Utc::now();
        app.set_sessions(&[
            Session {
                id: "S1".to_string(),
                title: "Session One".to_string(),
                created_at: now,
                updated_at: now,
                task_ids: vec![TaskId("T1".to_string())],
                parent_session_id: None,
                status: SessionStatus::Active,
            },
            Session {
                id: "S2".to_string(),
                title: "Session Two".to_string(),
                created_at: now,
                updated_at: now,
                task_ids: vec![TaskId("T2".to_string())],
                parent_session_id: None,
                status: SessionStatus::Completed,
            },
        ]);

        app.state.show_sessions = true;
        app.state.session_list_index = 1;
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(!app.state.show_sessions);
        assert_eq!(app.state.tasks.len(), 1);
        assert_eq!(app.state.tasks[0].task_id.0, "T2");
    }

    #[test]
    fn r_key_reverses_sort() {
        let mut app = TuiApp::default();
        assert!(!app.state.sort_reversed);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
        assert!(app.state.sort_reversed);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
        assert!(!app.state.sort_reversed);
    }

    #[test]
    fn t_key_toggles_timeline() {
        let mut app = TuiApp::default();
        assert!(!app.state.show_timeline);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert!(app.state.show_timeline);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert!(!app.state.show_timeline);
    }

    #[test]
    fn handle_key_event_tab_toggles_focused_pane_and_escape_quits() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Codex,
            lines: vec!["boot".to_string()],
        });

        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.state.focused_pane_idx, Some(0));
        assert_eq!(app.state.status_line, "focused pane 1");

        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.state.focused_pane_idx, None);
        assert_eq!(app.state.status_line, "pane focus cleared");

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn handle_key_event_esc_closes_focused_task_before_quitting() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            display_state: "Chatting".to_string(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];
        app.state.focused_task = true;

        // First Esc closes the focused task
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.state.focused_task);
        assert!(!app.should_quit);
        assert_eq!(app.state.status_line, "task detail closed");

        // Second Esc quits
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn handle_key_event_esc_closes_focused_pane_before_quitting() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Codex,
            lines: vec!["boot".to_string()],
        });
        app.state.focused_pane_idx = Some(0);

        // First Esc closes the focused pane
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.state.focused_pane_idx, None);
        assert!(!app.should_quit);
        assert_eq!(app.state.status_line, "pane focus cleared");

        // Second Esc quits
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn apply_event_agent_output_does_not_override_failed_or_exited_status() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Codex,
            lines: vec!["start".to_string()],
        });

        app.apply_event(TuiEvent::AgentPaneStatusChanged {
            instance_id: "A1".to_string(),
            status: AgentPaneStatus::Failed,
        });
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Codex,
            lines: vec!["after-failure".to_string()],
        });
        assert_eq!(app.state.panes[0].status, AgentPaneStatus::Failed);

        app.apply_event(TuiEvent::AgentPaneStatusChanged {
            instance_id: "A1".to_string(),
            status: AgentPaneStatus::Exited,
        });
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "A1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Codex,
            lines: vec!["after-exit".to_string()],
        });
        assert_eq!(app.state.panes[0].status, AgentPaneStatus::Exited);
    }

    #[test]
    fn create_task_key_enters_prompt_mode_and_enter_queues_prompted_action() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            display_state: "Chatting".to_string(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(matches!(
            app.input_mode,
            super::InputMode::NewChatPrompt { .. }
        ));
        assert_eq!(
            app.state.status_line,
            "new chat prompt: type feature request, Enter=submit Esc=cancel"
        );

        for ch in "Build OAuth login".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // After pressing Enter on the prompt, we should be in ModelSelect mode
        assert!(matches!(
            app.input_mode,
            super::InputMode::ModelSelect { .. }
        ));
        assert_eq!(
            app.state.status_line,
            "select model: Up/Down=cycle Enter=confirm Esc=cancel"
        );

        // Confirm model selection (default is Claude at index 0)
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::Normal));
        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(
            &drained[0],
            UiAction::CreateTask,
            Some(TaskId("T1".to_string())),
            Some("Build OAuth login"),
            Some(ModelKind::Claude),
        );
    }

    #[test]
    fn n_key_opens_new_task_dialog() {
        let mut app = TuiApp::default();

        app.handle_key_event(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));

        assert!(matches!(app.input_mode, super::InputMode::NewTaskDialog { .. }));
        let display = app
            .new_task_dialog_display()
            .expect("new task dialog display");
        assert_eq!(display.0, 0);
        assert_eq!(display.1, "default");
        assert_eq!(display.2, "");
        assert_eq!(display.3, "claude");
    }

    #[test]
    fn tab_cycles_dialog_fields() {
        let mut app = TuiApp::default();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));

        assert_eq!(app.new_task_dialog_display().expect("dialog").0, 0);
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.new_task_dialog_display().expect("dialog").0, 1);
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.new_task_dialog_display().expect("dialog").0, 2);
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.new_task_dialog_display().expect("dialog").0, 0);
    }

    #[test]
    fn esc_cancels_dialog() {
        let mut app = TuiApp::default();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::Normal));
        assert!(app.drain_actions().is_empty());
    }

    #[test]
    fn enter_creates_task() {
        let mut app = TuiApp::default();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));

        for _ in 0..7 {
            app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        for ch in "repo-x".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for ch in "Implement OAuth".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for _ in 0..6 {
            app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        for ch in "codex".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::Normal));
        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert!(matches!(
            &drained[0],
            QueuedAction::CreateTask { repo, title, model }
                if repo == "repo-x" && title == "Implement OAuth" && model == "codex"
        ));
    }

    #[test]
    fn backspace_deletes_char_in_dialog() {
        let mut app = TuiApp::default();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));

        app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        let display = app.new_task_dialog_display().expect("dialog");
        assert_eq!(display.1, "defaul");
    }

    #[test]
    fn handle_paste_appends_multiline_text_while_in_prompt_mode() {
        let mut app = TuiApp::default();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        app.handle_paste("line 1\r\nline 2\nline 3\rline 4");
        assert_eq!(app.input_prompt(), Some("line 1\nline 2\nline 3\nline 4"));
    }

    #[test]
    fn handle_paste_is_ignored_outside_prompt_mode() {
        let mut app = TuiApp::default();
        app.handle_paste("should not be captured");
        assert!(app.input_prompt().is_none());
    }

    #[test]
    fn delete_task_key_enters_confirm_mode_and_enter_queues_delete() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(matches!(
            app.input_mode,
            super::InputMode::DeleteTaskConfirm { .. }
        ));
        assert_eq!(
            app.state.status_line,
            "confirm delete task T1: Enter=delete Esc=cancel"
        );

        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.input_mode, super::InputMode::Normal));
        assert_eq!(app.state.status_line, "queued action=delete_task task=T1");

        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(
            &drained[0],
            UiAction::DeleteTask,
            Some(TaskId("T1".to_string())),
            None,
            None,
        );
    }

    #[test]
    fn delete_task_confirmation_escape_cancels_without_queueing_action() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::Normal));
        assert_eq!(app.state.status_line, "delete task canceled");
        assert!(app.drain_actions().is_empty());
    }

    #[test]
    fn model_select_arrow_keys_cycle_through_models() {
        let mut app = TuiApp::default();

        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        for ch in "test prompt".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Default selected is 0 (Claude)
        assert_eq!(app.model_select_display().unwrap().1, 0);

        // Down -> Codex (index 1)
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.model_select_display().unwrap().1, 1);

        // Down -> Gemini (index 2)
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.model_select_display().unwrap().1, 2);

        // Down wraps -> Claude (index 0)
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.model_select_display().unwrap().1, 0);

        // Up wraps -> Gemini (index 2)
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.model_select_display().unwrap().1, 2);

        // Confirm Gemini
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(
            &drained[0],
            UiAction::CreateTask,
            None,
            Some("test prompt"),
            Some(ModelKind::Gemini),
        );
    }

    #[test]
    fn handle_key_event_enter_toggles_focused_task_and_clears_focused_pane() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            display_state: "Chatting".to_string(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        // Set a focused pane to verify it gets cleared
        app.state.focused_pane_idx = Some(0);

        // Enter opens task detail
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.state.focused_task);
        assert_eq!(app.state.focused_pane_idx, None);
        assert_eq!(app.state.status_line, "task detail: T1");

        // Enter again closes task detail
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.state.focused_task);
        assert_eq!(app.state.status_line, "task detail closed");
    }

    #[test]
    fn handle_key_event_enter_does_nothing_when_no_tasks() {
        let mut app = TuiApp::default();
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.state.focused_task);
    }

    #[test]
    fn model_select_esc_cancels_and_returns_to_normal() {
        let mut app = TuiApp::default();

        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        for ch in "test".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            app.input_mode,
            super::InputMode::ModelSelect { .. }
        ));

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::Normal));
        assert_eq!(app.state.status_line, "model selection canceled");
        let drained = app.drain_actions();
        assert!(drained.is_empty());
    }

    #[test]
    fn apply_event_qa_update_sets_task_qa_fields() {
        use crate::model::QATestDisplay;

        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.apply_event(TuiEvent::QAUpdate {
            task_id: TaskId("T1".to_string()),
            status: "passed 2/2".to_string(),
            tests: vec![
                QATestDisplay {
                    name: "banner".to_string(),
                    suite: "startup".to_string(),
                    passed: true,
                    detail: String::new(),
                },
                QATestDisplay {
                    name: "chat".to_string(),
                    suite: "tui".to_string(),
                    passed: true,
                    detail: String::new(),
                },
            ],
            targets: vec!["verify OAuth".to_string()],
        });

        let task = &app.state.tasks[0];
        assert_eq!(task.qa_status.as_deref(), Some("passed 2/2"));
        assert_eq!(task.qa_tests.len(), 2);
        assert!(task.qa_tests[0].passed);
        assert_eq!(task.qa_targets, vec!["verify OAuth"]);
    }

    #[test]
    fn apply_event_qa_update_ignores_unknown_task() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::QAUpdate {
            task_id: TaskId("missing".to_string()),
            status: "failed".to_string(),
            tests: vec![],
            targets: vec![],
        });
        // No panic, no tasks added
        assert!(app.state.tasks.is_empty());
    }

    #[test]
    fn chat_input_key_enters_chat_mode_and_enter_queues_send() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        // Press 'i' to enter chat input mode
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert!(matches!(
            app.input_mode,
            super::InputMode::ChatInput { .. }
        ));
        assert_eq!(
            app.state.status_line,
            "chat input: type message, Enter=send Up/Down=history Esc=cancel"
        );

        // Type a message
        for ch in "fix the bug".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(app.input_prompt(), Some("fix the bug"));
        assert_eq!(
            app.chat_input_display(),
            Some(("fix the bug", &TaskId("T1".to_string())))
        );

        // Press Enter to send
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.input_mode, super::InputMode::Normal));
        assert_eq!(
            app.state.status_line,
            "queued action=send_chat_message task=T1"
        );

        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(
            &drained[0],
            UiAction::SendChatMessage,
            Some(TaskId("T1".to_string())),
            Some("fix the bug"),
            None,
        );
    }

    #[test]
    fn chat_input_escape_cancels_without_queueing_action() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "partial msg".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.input_mode, super::InputMode::Normal));
        assert_eq!(app.state.status_line, "chat input canceled");
        assert!(app.drain_actions().is_empty());
    }

    #[test]
    fn chat_input_empty_message_is_rejected() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        // Press Enter immediately with empty buffer
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Should stay in ChatInput mode, not queue anything
        assert!(matches!(
            app.input_mode,
            super::InputMode::ChatInput { .. }
        ));
        assert_eq!(app.state.status_line, "chat message cannot be empty");
        assert!(app.drain_actions().is_empty());
    }

    #[test]
    fn chat_input_whitespace_only_message_is_rejected() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        // Type only spaces
        for _ in 0..3 {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            app.input_mode,
            super::InputMode::ChatInput { .. }
        ));
        assert_eq!(app.state.status_line, "chat message cannot be empty");
        assert!(app.drain_actions().is_empty());
    }

    #[test]
    fn chat_input_backspace_removes_characters() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "hello".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(app.input_prompt(), Some("hello"));

        app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("hel"));

        // Backspace on empty buffer does nothing
        for _ in 0..10 {
            app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        assert_eq!(app.input_prompt(), Some(""));
    }

    #[test]
    fn chat_input_control_chars_are_ignored() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        // Ctrl+C should not add to buffer
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));

        assert_eq!(app.input_prompt(), Some("ab"));
    }

    #[test]
    fn chat_input_no_task_selected_shows_error() {
        let mut app = TuiApp::default();
        // No tasks in list
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));

        // Should stay in Normal mode
        assert!(matches!(app.input_mode, super::InputMode::Normal));
        assert_eq!(app.state.status_line, "no task selected for chat");
    }

    #[test]
    fn chat_input_paste_appends_multiline_text() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        app.handle_paste("line 1\r\nline 2\nline 3");
        assert_eq!(app.input_prompt(), Some("line 1\nline 2\nline 3"));
    }

    #[test]
    fn chat_input_display_returns_none_in_normal_mode() {
        let app = TuiApp::default();
        assert!(app.chat_input_display().is_none());
    }

    #[test]
    fn tasks_replaced_preserves_qa_data() {
        use crate::model::QATestDisplay;
        use orch_core::types::Task;
        use std::path::PathBuf;

        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: Some("baseline running".to_string()),
            qa_tests: vec![QATestDisplay {
                name: "build".to_string(),
                suite: "build".to_string(),
                passed: true,
                detail: String::new(),
            }],
            qa_targets: vec!["check endpoint".to_string()],
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];

        // Simulate a TasksReplaced event (same task, fresh data from DB).
        let task = Task::new(
            TaskId("T1".to_string()),
            RepoId("example".to_string()),
            "Task T1".to_string(),
            PathBuf::from(".orch/wt/T1"),
        );
        app.apply_event(TuiEvent::TasksReplaced {
            tasks: vec![task],
        });

        // QA data should be preserved.
        let row = &app.state.tasks[0];
        assert_eq!(row.qa_status.as_deref(), Some("baseline running"));
        assert_eq!(row.qa_tests.len(), 1);
        assert_eq!(row.qa_targets, vec!["check endpoint"]);
    }

    #[test]
    fn ensure_pane_index_does_not_reuse_qa_pane_for_agent_output() {
        let mut app = TuiApp::default();

        // Create an exited QA pane for T1.
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "qa-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["qa baseline".to_string()],
        });
        app.apply_event(TuiEvent::AgentPaneStatusChanged {
            instance_id: "qa-T1".to_string(),
            status: AgentPaneStatus::Exited,
        });

        // Now send agent output for T1 — should NOT reuse the QA pane.
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["agent work".to_string()],
        });

        // Should have 2 separate panes.
        assert_eq!(app.state.panes.len(), 2);
        assert_eq!(app.state.panes[0].instance_id, "qa-T1");
        assert_eq!(app.state.panes[1].instance_id, "agent-T1");
    }

    #[test]
    fn ensure_pane_index_reuses_same_category_pane() {
        let mut app = TuiApp::default();

        // Create an exited agent pane for T1.
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["first run".to_string()],
        });
        app.apply_event(TuiEvent::AgentPaneStatusChanged {
            instance_id: "agent-T1".to_string(),
            status: AgentPaneStatus::Exited,
        });

        // New agent output for same task — should reuse the exited agent pane.
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1-retry".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Codex,
            lines: vec!["retry run".to_string()],
        });

        // Should still be 1 pane (reused).
        assert_eq!(app.state.panes.len(), 1);
        assert_eq!(app.state.panes[0].instance_id, "agent-T1-retry");
        assert_eq!(app.state.panes[0].model, ModelKind::Codex);
        assert_eq!(app.state.panes[0].status, AgentPaneStatus::Running);
    }

    #[test]
    fn left_right_arrow_keys_toggle_pane_category_in_focused_task_view() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];
        app.state.focused_task = true;
        assert_eq!(
            app.state.selected_pane_category,
            crate::model::PaneCategory::Agent
        );

        // Right arrow should toggle to QA
        app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            app.state.selected_pane_category,
            crate::model::PaneCategory::QA
        );
        assert_eq!(app.state.scroll_back, 0);

        // Left arrow should toggle back to Agent
        app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(
            app.state.selected_pane_category,
            crate::model::PaneCategory::Agent
        );

        // Ensure no actions were queued (arrows don't dispatch actions)
        assert!(app.drain_actions().is_empty());
    }

    #[test]
    fn left_right_arrow_keys_toggle_category_and_update_focused_pane_idx() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["agent output".to_string()],
        });
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "qa-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["qa output".to_string()],
        });

        // Focus the agent pane
        app.state.focused_pane_idx = Some(0);
        assert_eq!(app.state.selected_pane_idx, 0);

        // Right arrow toggles to QA and updates focused_pane_idx
        app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            app.state.selected_pane_category,
            crate::model::PaneCategory::QA
        );
        assert_eq!(app.state.selected_pane_idx, 1);
        assert_eq!(app.state.focused_pane_idx, Some(1));
        assert_eq!(app.state.scroll_back, 0);

        // Left arrow toggles back and updates focused_pane_idx
        app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(
            app.state.selected_pane_category,
            crate::model::PaneCategory::Agent
        );
        assert_eq!(app.state.selected_pane_idx, 0);
        assert_eq!(app.state.focused_pane_idx, Some(0));
    }

    #[test]
    fn scroll_resets_when_toggling_category_in_focused_pane() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: (0..50).map(|i| format!("line {i}")).collect(),
        });
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "qa-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["qa output".to_string()],
        });
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];
        app.state.focused_pane_idx = Some(0);

        // Scroll up to accumulate scroll_back
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(app.state.scroll_back > 0);

        // Toggle category — scroll_back should reset
        app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.state.scroll_back, 0);
    }

    #[test]
    fn scroll_up_down_in_focused_task() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: (0..30).map(|i| format!("line {i}")).collect(),
        });
        app.state.focused_task = true;

        // Scroll up several times
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.state.scroll_back, 3);

        // Scroll down
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.state.scroll_back, 2);

        // Page up
        app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(app.state.scroll_back >= 20);

        // End resets to bottom
        app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.state.scroll_back, 0);

        // Home scrolls to top
        app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert!(app.state.scroll_back > 0);
    }

    #[test]
    fn vim_keys_scroll_in_focused_task() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }];
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: (0..10).map(|i| format!("line {i}")).collect(),
        });
        app.state.focused_task = true;

        // 'k' scrolls up
        app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.state.scroll_back, 1);

        // 'j' scrolls down
        app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.state.scroll_back, 0);
    }

    #[test]
    fn arrow_keys_navigate_tasks_and_panes_in_normal_view() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![
            TaskOverviewRow {
                task_id: TaskId("T1".to_string()),
                repo_id: RepoId("example".to_string()),
                title: "Task T1".to_string(),
                branch: "task/T1".to_string(),
                stack_position: None,
                state: TaskState::Chatting,
                display_state: "Chatting".to_string(),
                verify_summary: "not_run".to_string(),
                last_activity: Utc::now(),
                qa_status: None,
                qa_tests: Vec::new(),
                qa_targets: Vec::new(),
                estimated_tokens: None,
                estimated_cost_usd: None,
                retry_count: 0,
                retry_history: Vec::new(),
                depends_on_display: Vec::new(),
                pr_url: None,
                model_display: None,
            },
            TaskOverviewRow {
                task_id: TaskId("T2".to_string()),
                repo_id: RepoId("example".to_string()),
                title: "Task T2".to_string(),
                branch: "task/T2".to_string(),
                stack_position: None,
                state: TaskState::Chatting,
                display_state: "Chatting".to_string(),
                verify_summary: "not_run".to_string(),
                last_activity: Utc::now(),
                qa_status: None,
                qa_tests: Vec::new(),
                qa_targets: Vec::new(),
                estimated_tokens: None,
                estimated_cost_usd: None,
                retry_count: 0,
                retry_history: Vec::new(),
                depends_on_display: Vec::new(),
                pr_url: None,
                model_display: None,
            },
        ];
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["T1 output".to_string()],
        });
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T2".to_string(),
            task_id: TaskId("T2".to_string()),
            model: ModelKind::Codex,
            lines: vec!["T2 output".to_string()],
        });

        assert_eq!(app.state.selected_task_idx, 0);

        // Down selects next task
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.state.selected_task_idx, 1);

        // Down wraps back to first
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.state.selected_task_idx, 0);

        // Up wraps to last
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.state.selected_task_idx, 1);

        // Right/Left toggles pane category
        app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            app.state.selected_pane_category,
            crate::model::PaneCategory::QA
        );

        app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(
            app.state.selected_pane_category,
            crate::model::PaneCategory::Agent
        );
    }

    #[test]
    fn task_selection_snaps_pane_when_no_panes_exist() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![
            TaskOverviewRow {
                task_id: TaskId("T1".to_string()),
                repo_id: RepoId("example".to_string()),
                title: "Task T1".to_string(),
                branch: "task/T1".to_string(),
                stack_position: None,
                state: TaskState::Chatting,
                display_state: "Chatting".to_string(),
                verify_summary: "not_run".to_string(),
                last_activity: Utc::now(),
                qa_status: None,
                qa_tests: Vec::new(),
                qa_targets: Vec::new(),
                estimated_tokens: None,
                estimated_cost_usd: None,
                retry_count: 0,
                retry_history: Vec::new(),
                depends_on_display: Vec::new(),
                pr_url: None,
                model_display: None,
            },
            TaskOverviewRow {
                task_id: TaskId("T2".to_string()),
                repo_id: RepoId("example".to_string()),
                title: "Task T2".to_string(),
                branch: "task/T2".to_string(),
                stack_position: None,
                state: TaskState::Chatting,
                display_state: "Chatting".to_string(),
                verify_summary: "not_run".to_string(),
                last_activity: Utc::now(),
                qa_status: None,
                qa_tests: Vec::new(),
                qa_targets: Vec::new(),
                estimated_tokens: None,
                estimated_cost_usd: None,
                retry_count: 0,
                retry_history: Vec::new(),
                depends_on_display: Vec::new(),
                pr_url: None,
                model_display: None,
            },
        ];
        // No panes at all

        // Moving through tasks should not panic
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.state.selected_task_idx, 1);
        assert_eq!(app.state.selected_pane_idx, 0);

        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.state.selected_task_idx, 0);
    }

    #[test]
    fn toggle_focused_task_clears_scroll_back() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
                retry_history: Vec::new(),
                depends_on_display: Vec::new(),
                pr_url: None,
                model_display: None,
        }];
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: (0..20).map(|i| format!("line {i}")).collect(),
        });

        // Open task detail
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.state.focused_task);

        // Scroll up
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(app.state.scroll_back > 0);

        // Close task detail — scroll_back should reset
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.state.focused_task);
        assert_eq!(app.state.scroll_back, 0);
    }

    #[test]
    fn toggle_focused_pane_clears_scroll_back() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: (0..20).map(|i| format!("line {i}")).collect(),
        });

        // Focus pane via Tab
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.state.focused_pane_idx, Some(0));

        // Scroll up
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(app.state.scroll_back > 0);

        // Unfocus pane via Tab — scroll_back should reset
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.state.focused_pane_idx, None);
        assert_eq!(app.state.scroll_back, 0);
    }

    #[test]
    fn esc_from_focused_pane_clears_scroll_back() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: (0..20).map(|i| format!("line {i}")).collect(),
        });

        app.state.focused_pane_idx = Some(0);
        app.state.scroll_back = 5;

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.state.focused_pane_idx, None);
        assert_eq!(app.state.scroll_back, 0);
        assert!(!app.should_quit);
    }

    #[test]
    fn entering_focused_task_clears_focused_pane_and_scroll() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
                retry_history: Vec::new(),
                depends_on_display: Vec::new(),
                pr_url: None,
                model_display: None,
        }];
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["output".to_string()],
        });

        // Focus a pane and set scroll
        app.state.focused_pane_idx = Some(0);
        app.state.scroll_back = 3;

        // Enter task detail — should clear focused pane and scroll
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.state.focused_task);
        assert_eq!(app.state.focused_pane_idx, None);
        assert_eq!(app.state.scroll_back, 0);
    }

    #[test]
    fn entering_focused_pane_clears_focused_task() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
                retry_history: Vec::new(),
                depends_on_display: Vec::new(),
                pr_url: None,
                model_display: None,
        }];
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["output".to_string()],
        });

        // Enter task detail first
        app.state.focused_task = true;

        // Tab to focus pane — should clear focused task
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!app.state.focused_task);
        assert_eq!(app.state.focused_pane_idx, Some(0));
    }

    #[test]
    fn scroll_down_never_goes_negative() {
        let mut app = TuiApp::default();
        app.apply_event(TuiEvent::AgentPaneOutput {
            instance_id: "agent-T1".to_string(),
            task_id: TaskId("T1".to_string()),
            model: ModelKind::Claude,
            lines: vec!["line".to_string()],
        });
        app.state.focused_pane_idx = Some(0);

        // Already at bottom (scroll_back = 0), scroll down should remain 0
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.state.scroll_back, 0);

        // Page down at bottom should remain 0
        app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.state.scroll_back, 0);
    }

    #[test]
    fn normal_view_keys_pass_through_when_focused() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![TaskOverviewRow {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "Task T1".to_string(),
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
                retry_history: Vec::new(),
                depends_on_display: Vec::new(),
                pr_url: None,
                model_display: None,
        }];
        app.state.focused_task = true;

        // Action keys should still work in focused views (they fall through)
        app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(
            &drained[0],
            UiAction::StartAgent,
            Some(TaskId("T1".to_string())),
            None,
            None,
        );
    }

    #[test]
    fn ctrl_c_quits_from_any_view() {
        // From normal view
        let mut app = TuiApp::default();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);

        // From focused task view
        let mut app = TuiApp::default();
        app.state.focused_task = true;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);

        // From focused pane view
        let mut app = TuiApp::default();
        app.state.focused_pane_idx = Some(0);
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);

        // From input mode
        let mut app = TuiApp {
            input_mode: super::InputMode::NewChatPrompt {
                buffer: "test".to_string(),
            },
            ..Default::default()
        };
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    fn make_task_row(id: &str) -> TaskOverviewRow {
        TaskOverviewRow {
            task_id: TaskId(id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Task {id}"),
            branch: format!("task/{id}"),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }
    }

    fn make_log_root(task_id: &str, content: &str) -> String {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be monotonic enough for temp path")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("othala-log-view-{unique}"));
        let task_dir = root.join(task_id);
        std::fs::create_dir_all(&task_dir).expect("create task log directory");
        std::fs::write(task_dir.join("latest.log"), content).expect("write latest.log");
        root.to_string_lossy().to_string()
    }

    fn log_view_scroll_offset(app: &TuiApp) -> usize {
        match &app.input_mode {
            super::InputMode::LogView { scroll_offset, .. } => *scroll_offset,
            _ => panic!("expected log view mode"),
        }
    }

    #[test]
    fn l_key_enters_log_view_mode() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];
        app.state.focused_task = true;
        let log_root = make_log_root("T1", "line one\nline two\nline three\n");
        app.state.log_root = Some(log_root.clone());

        app.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));

        match &app.input_mode {
            super::InputMode::LogView {
                task_id,
                log_lines,
                scroll_offset,
            } => {
                assert_eq!(task_id, "T1");
                assert_eq!(
                    log_lines,
                    &vec![
                        "line one".to_string(),
                        "line two".to_string(),
                        "line three".to_string(),
                    ]
                );
                assert_eq!(*scroll_offset, 0);
            }
            _ => panic!("expected log view mode"),
        }

        std::fs::remove_dir_all(log_root).expect("cleanup temp log root");
    }

    #[test]
    fn log_view_scroll_down() {
        let mut app = TuiApp {
            input_mode: super::InputMode::LogView {
                task_id: "T1".to_string(),
                log_lines: vec!["a".to_string(), "b".to_string(), "c".to_string()],
                scroll_offset: 0,
            },
            ..Default::default()
        };

        app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(log_view_scroll_offset(&app), 1);
    }

    #[test]
    fn log_view_scroll_up() {
        let mut app = TuiApp {
            input_mode: super::InputMode::LogView {
                task_id: "T1".to_string(),
                log_lines: vec!["a".to_string(), "b".to_string(), "c".to_string()],
                scroll_offset: 2,
            },
            ..Default::default()
        };

        app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(log_view_scroll_offset(&app), 1);
    }

    #[test]
    fn log_view_esc_returns_to_normal() {
        let mut app = TuiApp {
            input_mode: super::InputMode::LogView {
                task_id: "T1".to_string(),
                log_lines: vec!["a".to_string()],
                scroll_offset: 0,
            },
            ..Default::default()
        };

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(matches!(app.input_mode, super::InputMode::Normal));
    }

    #[test]
    fn log_view_jump_to_end() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let expected = lines.len().saturating_sub(super::log_view_visible_height());
        let mut app = TuiApp {
            input_mode: super::InputMode::LogView {
                task_id: "T1".to_string(),
                log_lines: lines,
                scroll_offset: 0,
            },
            ..Default::default()
        };

        app.handle_key_event(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));

        assert_eq!(log_view_scroll_offset(&app), expected);
    }

    #[test]
    fn chat_history_up_arrow_recalls_previous_message() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        // Send two messages to build history
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "first message".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "second message".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Enter chat input mode again
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some(""));

        // Up arrow shows most recent message
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("second message"));

        // Up arrow again shows older message
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("first message"));

        // Up arrow at oldest entry stays put
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("first message"));
    }

    #[test]
    fn chat_history_down_arrow_returns_to_draft() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        // Build history
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "msg one".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "msg two".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Start typing a draft, then navigate history
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "my draft".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        // Up to history
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("msg two"));

        // Down back to draft
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("my draft"));

        // Down at draft does nothing
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("my draft"));
    }

    #[test]
    fn chat_history_up_with_no_history_does_nothing() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "hello".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        // No history yet, Up arrow should leave buffer unchanged
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("hello"));
    }

    #[test]
    fn chat_history_typing_resets_history_index() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        // Build history
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "old msg".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Start new input, navigate to history, then type
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("old msg"));

        // Typing a character resets history navigation
        app.handle_key_event(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("old msg!"));

        // Down should do nothing now (history_index is None)
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("old msg!"));
    }

    #[test]
    fn chat_history_is_per_task() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1"), make_task_row("T2")];

        // Send a message for T1
        app.state.selected_task_idx = 0;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "T1 msg".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Switch to T2 and send a message
        app.state.selected_task_idx = 1;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "T2 msg".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Check T2 history
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("T2 msg"));
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // Check T1 history
        app.state.selected_task_idx = 0;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("T1 msg"));
    }

    #[test]
    fn chat_history_submitted_message_is_stored() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "hello world".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            app.chat_history.get(&TaskId("T1".to_string())),
            Some(&vec!["hello world".to_string()])
        );
    }

    #[test]
    fn chat_input_backspace_resets_history_index() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        // Build history
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "old msg".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Start new input, navigate to history, then backspace
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("old msg"));

        app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("old ms"));

        // Down should do nothing now (history_index was reset to None)
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("old ms"));
    }

    #[test]
    fn chat_input_send_history_entry_directly() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        // Build history
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "reusable command".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.drain_actions(); // clear first send

        // Re-enter chat, navigate to history, and send it
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("reusable command"));

        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.input_mode, super::InputMode::Normal));

        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(
            &drained[0],
            UiAction::SendChatMessage,
            Some(TaskId("T1".to_string())),
            Some("reusable command"),
            None,
        );

        // History should now have the same message twice
        assert_eq!(
            app.chat_history.get(&TaskId("T1".to_string())),
            Some(&vec![
                "reusable command".to_string(),
                "reusable command".to_string(),
            ])
        );
    }

    #[test]
    fn chat_input_full_history_navigation_round_trip() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        // Build three messages of history
        for msg in &["first", "second", "third"] {
            app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
            for ch in msg.chars() {
                app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }

        // Enter chat input mode with a draft
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "draft".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        // Navigate all the way up through history
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("third"));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("second"));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("first"));

        // At oldest, Up stays put
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("first"));

        // Navigate all the way back down
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("second"));
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("third"));
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("draft"));

        // At draft, Down stays put
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("draft"));
    }

    #[test]
    fn chat_input_message_is_trimmed_on_send() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "  hello  ".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(
            &drained[0],
            UiAction::SendChatMessage,
            Some(TaskId("T1".to_string())),
            Some("hello"),
            None,
        );

        // Stored in history trimmed
        assert_eq!(
            app.chat_history.get(&TaskId("T1".to_string())),
            Some(&vec!["hello".to_string()])
        );
    }

    #[test]
    fn chat_input_ctrl_c_quits_from_chat_mode() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "typing".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        // Ctrl+C should quit even from chat input mode
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
        // No action should be queued
        assert!(app.drain_actions().is_empty());
    }

    #[test]
    fn chat_input_paste_resets_history_index() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        // Build history
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "old".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Enter chat input, navigate to history
        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input_prompt(), Some("old"));

        // Paste appends to the buffer (but does not reset history_index itself,
        // only char input and backspace do). The buffer content changes though.
        app.handle_paste(" pasted");
        assert_eq!(app.input_prompt(), Some("old pasted"));
    }

    #[test]
    fn chat_input_delete_confirm_y_key_confirms() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(matches!(
            app.input_mode,
            super::InputMode::DeleteTaskConfirm { .. }
        ));

        // 'y' key should also confirm deletion (not just Enter)
        app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(matches!(app.input_mode, super::InputMode::Normal));
        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_dispatch_action(
            &drained[0],
            UiAction::DeleteTask,
            Some(TaskId("T1".to_string())),
            None,
            None,
        );
    }

    #[test]
    fn chat_input_non_press_key_events_are_ignored() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![make_task_row("T1")];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        for ch in "test".chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        // Release event should be ignored (key.kind != Press)
        let mut release_event = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release_event.kind = crossterm::event::KeyEventKind::Release;
        app.handle_key_event(release_event);

        assert_eq!(app.input_prompt(), Some("test")); // 'x' was NOT appended
    }
}
