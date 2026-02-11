use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use orch_core::types::{ModelKind, Task, TaskId};
use std::collections::VecDeque;

use crate::action::{action_label, map_key_to_command, UiAction, UiCommand};
use crate::event::TuiEvent;
use crate::model::{AgentPane, AgentPaneStatus, DashboardState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedAction {
    pub action: UiAction,
    pub task_id: Option<TaskId>,
    pub prompt: Option<String>,
    pub model: Option<ModelKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
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
    ChatInput {
        buffer: String,
        task_id: TaskId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiApp {
    pub state: DashboardState,
    pub action_queue: VecDeque<QueuedAction>,
    pub input_mode: InputMode,
    pub should_quit: bool,
}

impl Default for TuiApp {
    fn default() -> Self {
        Self {
            state: DashboardState::default(),
            action_queue: VecDeque::new(),
            input_mode: InputMode::Normal,
            should_quit: false,
        }
    }
}

impl TuiApp {
    pub fn from_tasks(tasks: &[Task]) -> Self {
        Self {
            state: DashboardState::with_tasks(tasks),
            ..Self::default()
        }
    }

    pub fn set_tasks(&mut self, tasks: &[Task]) {
        self.state.tasks = tasks
            .iter()
            .map(crate::model::TaskOverviewRow::from_task)
            .collect();
        if self.state.selected_task_idx >= self.state.tasks.len() {
            self.state.selected_task_idx = self.state.tasks.len().saturating_sub(1);
        }
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
        self.action_queue.push_back(QueuedAction {
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
            UiCommand::Quit => self.should_quit = true,
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        match &mut self.input_mode {
            InputMode::NewChatPrompt { buffer } | InputMode::ChatInput { buffer, .. } => {
                buffer.push_str(&normalize_paste_text(text));
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

    fn begin_chat_input(&mut self) {
        let Some(task) = self.state.selected_task() else {
            self.state.status_line = "no task selected for chat".to_string();
            return;
        };
        let task_id = task.task_id.clone();
        self.input_mode = InputMode::ChatInput {
            buffer: String::new(),
            task_id,
        };
        self.state.status_line =
            "chat input: type message, Enter=send Esc=cancel".to_string();
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
            InputMode::NewChatPrompt { buffer } => Some(buffer.as_str()),
            InputMode::ChatInput { buffer, .. } => Some(buffer.as_str()),
            InputMode::ModelSelect { prompt, .. } => Some(prompt.as_str()),
            InputMode::DeleteTaskConfirm { .. } => None,
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
            InputMode::ChatInput { buffer, task_id } => Some((buffer.as_str(), task_id)),
            _ => None,
        }
    }

    pub fn delete_confirm_display(&self) -> Option<(&TaskId, Option<&str>)> {
        match &self.input_mode {
            InputMode::DeleteTaskConfirm { task_id, branch } => Some((task_id, branch.as_deref())),
            _ => None,
        }
    }

    fn handle_input_mode_key(&mut self, key: KeyEvent) -> bool {
        match &mut self.input_mode {
            InputMode::Normal => return false,
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
                    self.action_queue.push_back(QueuedAction {
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
            InputMode::ChatInput { buffer, task_id } => match key.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                    self.state.status_line = "chat input canceled".to_string();
                }
                KeyCode::Enter => {
                    let message = buffer.trim().to_string();
                    if message.is_empty() {
                        self.state.status_line = "chat message cannot be empty".to_string();
                        return true;
                    }
                    let tid = task_id.clone();
                    self.action_queue.push_back(QueuedAction {
                        action: UiAction::SendChatMessage,
                        task_id: Some(tid.clone()),
                        prompt: Some(message),
                        model: None,
                    });
                    self.input_mode = InputMode::Normal;
                    self.state.status_line =
                        format!("queued action=send_chat_message task={}", tid.0);
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
            InputMode::DeleteTaskConfirm { task_id, .. } => match key.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                    self.state.status_line = "delete task canceled".to_string();
                }
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let confirmed_task_id = task_id.clone();
                    self.action_queue.push_back(QueuedAction {
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

        if let Some(idx) = self
            .state
            .panes
            .iter()
            .position(|pane| pane.task_id == task_id && pane.status != AgentPaneStatus::Running)
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use orch_core::state::TaskState;
    use orch_core::types::RepoId;
    use orch_core::types::{ModelKind, TaskId};

    use crate::{AgentPane, AgentPaneStatus, TaskOverviewRow, TuiApp, TuiEvent, UiAction};

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
            },
        ];
        app.state.selected_task_idx = 1;

        app.push_action(UiAction::RunVerifyQuick);
        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].action, UiAction::RunVerifyQuick);
        assert_eq!(drained[0].task_id, Some(TaskId("T2".to_string())));
        assert_eq!(drained[0].prompt, None);
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
        assert_eq!(drained[0].action, UiAction::TriggerRestack);
        assert_eq!(drained[0].task_id, None);
        assert_eq!(drained[0].prompt, None);
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
        }];

        app.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].action, UiAction::RunVerifyQuick);
        assert_eq!(drained[0].task_id, Some(TaskId("T1".to_string())));
        assert_eq!(drained[0].prompt, None);
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
        assert_eq!(drained[0].action, UiAction::CreateTask);
        assert_eq!(drained[0].task_id, Some(TaskId("T1".to_string())));
        assert_eq!(drained[0].prompt.as_deref(), Some("Build OAuth login"));
        assert_eq!(drained[0].model, Some(ModelKind::Claude));
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
        assert_eq!(drained[0].action, UiAction::DeleteTask);
        assert_eq!(drained[0].task_id, Some(TaskId("T1".to_string())));
        assert_eq!(drained[0].prompt, None);
        assert_eq!(drained[0].model, None);
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
        assert_eq!(drained[0].model, Some(ModelKind::Gemini));
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
}
