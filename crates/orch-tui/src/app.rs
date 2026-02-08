use crossterm::event::KeyEvent;
use orch_core::types::Task;
use std::collections::VecDeque;

use crate::action::{action_label, map_key_to_command, UiAction, UiCommand};
use crate::model::{AgentPane, DashboardState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiApp {
    pub state: DashboardState,
    pub action_queue: VecDeque<UiAction>,
    pub should_quit: bool,
}

impl Default for TuiApp {
    fn default() -> Self {
        Self {
            state: DashboardState::default(),
            action_queue: VecDeque::new(),
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
        self.state.status_line = format!("queued action={}", action_label(action));
        self.action_queue.push_back(action);
    }

    pub fn drain_actions(&mut self) -> Vec<UiAction> {
        self.action_queue.drain(..).collect()
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) {
        let Some(command) = map_key_to_command(key) else {
            return;
        };

        match command {
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
}
