use ratatui::style::Color;

use crate::app::TuiApp;
use crate::model::{AgentPane, AgentPaneStatus};

const THINKING_FRAMES: [&str; 4] = ["o..", ".o.", "..o", ".o."];

fn pane_status_active(status: AgentPaneStatus) -> bool {
    matches!(
        status,
        AgentPaneStatus::Starting | AgentPaneStatus::Running | AgentPaneStatus::Waiting
    )
}

fn animation_frame_now() -> usize {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    (millis / 250) as usize
}

pub(crate) fn status_activity(status: AgentPaneStatus, frame: usize) -> Option<(String, Color)> {
    let pulse = THINKING_FRAMES[frame % THINKING_FRAMES.len()];
    match status {
        AgentPaneStatus::Starting => Some((format!("starting {pulse}"), Color::Yellow)),
        AgentPaneStatus::Running => Some((format!("thinking {pulse}"), Color::Cyan)),
        AgentPaneStatus::Waiting => Some((format!("percolating {pulse}"), Color::Magenta)),
        AgentPaneStatus::Exited | AgentPaneStatus::Failed | AgentPaneStatus::Stopped => None,
    }
}

fn active_activity_pane(app: &TuiApp) -> Option<&AgentPane> {
    if app.state.focused_task {
        if let Some(task) = app.state.selected_task() {
            if let Some(pane) = app
                .state
                .panes
                .iter()
                .find(|pane| pane.task_id == task.task_id && pane_status_active(pane.status))
            {
                return Some(pane);
            }
        }
    }

    if let Some(idx) = app.state.focused_pane_idx {
        if let Some(pane) = app
            .state
            .panes
            .get(idx)
            .filter(|pane| pane_status_active(pane.status))
        {
            return Some(pane);
        }
    }

    if let Some(pane) = app
        .state
        .selected_pane()
        .filter(|pane| pane_status_active(pane.status))
    {
        return Some(pane);
    }

    app.state
        .panes
        .iter()
        .find(|pane| pane_status_active(pane.status))
}

pub(crate) fn pane_activity_indicator(pane: &AgentPane) -> Option<(String, Color)> {
    let frame = animation_frame_now();
    status_activity(pane.status, frame)
}

pub(crate) fn footer_activity_indicator(app: &TuiApp) -> Option<(String, Color)> {
    let pane = active_activity_pane(app)?;
    let frame = animation_frame_now();
    let (activity, color) = status_activity(pane.status, frame)?;
    Some((format!("{} {activity}", pane.instance_id), color))
}
