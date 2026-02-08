use chrono::Utc;
use crossterm::event::KeyEvent;
use orch_core::types::{ModelKind, Task, TaskId};
use std::collections::VecDeque;

use crate::action::{action_label, map_key_to_command, UiAction, UiCommand};
use crate::event::TuiEvent;
use crate::model::{AgentPane, AgentPaneStatus, DashboardState};

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
    use orch_core::types::{ModelKind, TaskId};

    use crate::{AgentPaneStatus, TuiApp, TuiEvent};

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
}
