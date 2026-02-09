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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    NewChatPrompt { buffer: String },
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

        let Some(command) = map_key_to_command(key) else {
            return;
        };

        match command {
            UiCommand::Dispatch(UiAction::CreateTask) => self.begin_new_chat_prompt(),
            UiCommand::Dispatch(action) => self.push_action(action),
            UiCommand::SelectNextTask => self.state.move_task_selection_next(),
            UiCommand::SelectPreviousTask => self.state.move_task_selection_previous(),
            UiCommand::SelectNextPane => self.state.move_pane_selection_next(),
            UiCommand::SelectPreviousPane => self.state.move_pane_selection_previous(),
            UiCommand::ToggleFocusedPane => {
                if self.state.focused_pane_idx.is_some() {
                    self.state.focused_pane_idx = None;
                    self.state.status_line = "pane focus cleared".to_string();
                } else if !self.state.panes.is_empty() {
                    self.state.focused_pane_idx = Some(self.state.selected_pane_idx);
                    self.state.status_line = format!(
                        "focused pane {}",
                        self.state.selected_pane_idx.saturating_add(1)
                    );
                }
            }
            UiCommand::Quit => self.should_quit = true,
        }
    }

    fn begin_new_chat_prompt(&mut self) {
        self.input_mode = InputMode::NewChatPrompt {
            buffer: String::new(),
        };
        self.state.status_line =
            "new chat prompt: type feature request, Enter=submit Esc=cancel".to_string();
    }

    pub fn input_prompt(&self) -> Option<&str> {
        match &self.input_mode {
            InputMode::Normal => None,
            InputMode::NewChatPrompt { buffer } => Some(buffer.as_str()),
        }
    }

    fn handle_input_mode_key(&mut self, key: KeyEvent) -> bool {
        let InputMode::NewChatPrompt { buffer } = &mut self.input_mode else {
            return false;
        };

        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.state.status_line = "new chat prompt canceled".to_string();
                true
            }
            KeyCode::Enter => {
                let prompt = buffer.trim().to_string();
                if prompt.is_empty() {
                    self.state.status_line = "new chat prompt cannot be empty".to_string();
                    return true;
                }
                let task_id = self.state.selected_task().map(|task| task.task_id.clone());
                self.action_queue.push_back(QueuedAction {
                    action: UiAction::CreateTask,
                    task_id,
                    prompt: Some(prompt),
                });
                self.input_mode = InputMode::Normal;
                self.state.status_line = "queued action=create_task (chat)".to_string();
                true
            }
            KeyCode::Backspace => {
                buffer.pop();
                true
            }
            KeyCode::Char(ch) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return true;
                }
                buffer.push(ch);
                true
            }
            _ => true,
        }
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
                self.state.status_line = format!("pane updated: {instance_id}");
            }
            TuiEvent::AgentPaneStatusChanged {
                instance_id,
                status,
            } => {
                if let Some(idx) = self.pane_index_by_instance(&instance_id) {
                    let pane = &mut self.state.panes[idx];
                    pane.status = status;
                    pane.updated_at = Utc::now();
                    self.state.status_line = format!("pane status updated: {instance_id}");
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

        let mut pane = AgentPane::new(instance_id.to_string(), task_id, model);
        pane.status = AgentPaneStatus::Running;
        self.state.panes.push(pane);
        if self.state.panes.len() == 1 {
            self.state.selected_pane_idx = 0;
        }
        self.state.panes.len() - 1
    }
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
        assert_eq!(app.state.status_line, "pane status updated: A1");
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
                branch: "task/T1".to_string(),
                stack_position: None,
                state: TaskState::Running,
                verify_summary: "not_run".to_string(),
                review_summary: "0/0 unanimous=false cap=ok".to_string(),
                last_activity: Utc::now(),
            },
            TaskOverviewRow {
                task_id: TaskId("T2".to_string()),
                repo_id: RepoId("example".to_string()),
                branch: "task/T2".to_string(),
                stack_position: None,
                state: TaskState::Running,
                verify_summary: "not_run".to_string(),
                review_summary: "0/0 unanimous=false cap=ok".to_string(),
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
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Running,
            verify_summary: "not_run".to_string(),
            review_summary: "0/0 unanimous=false cap=ok".to_string(),
            last_activity: Utc::now(),
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
            branch: "task/T1".to_string(),
            stack_position: None,
            state: TaskState::Running,
            verify_summary: "not_run".to_string(),
            review_summary: "0/0 unanimous=false cap=ok".to_string(),
            last_activity: Utc::now(),
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

        assert!(matches!(app.input_mode, super::InputMode::Normal));
        let drained = app.drain_actions();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].action, UiAction::CreateTask);
        assert_eq!(drained[0].task_id, Some(TaskId("T1".to_string())));
        assert_eq!(drained[0].prompt.as_deref(), Some("Build OAuth login"));
    }
}
