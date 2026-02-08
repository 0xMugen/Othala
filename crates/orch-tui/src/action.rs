use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAction {
    CreateTask,
    StartAgent,
    StopAgent,
    RestartAgent,
    RunVerifyQuick,
    RunVerifyFull,
    TriggerRestack,
    MarkNeedsHuman,
    OpenWebUiForTask,
    PauseTask,
    ResumeTask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiCommand {
    Dispatch(UiAction),
    SelectNextTask,
    SelectPreviousTask,
    SelectNextPane,
    SelectPreviousPane,
    ToggleFocusedPane,
    Quit,
}

pub fn map_key_to_command(key: KeyEvent) -> Option<UiCommand> {
    if key.kind != KeyEventKind::Press {
        return None;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(UiCommand::Quit);
    }
    if key.code == KeyCode::Esc {
        return Some(UiCommand::Quit);
    }

    match key.code {
        KeyCode::Down => Some(UiCommand::SelectNextTask),
        KeyCode::Up => Some(UiCommand::SelectPreviousTask),
        KeyCode::Right => Some(UiCommand::SelectNextPane),
        KeyCode::Left => Some(UiCommand::SelectPreviousPane),
        KeyCode::Tab => Some(UiCommand::ToggleFocusedPane),
        KeyCode::Char('c') => Some(UiCommand::Dispatch(UiAction::CreateTask)),
        KeyCode::Char('s') => Some(UiCommand::Dispatch(UiAction::StartAgent)),
        KeyCode::Char('x') => Some(UiCommand::Dispatch(UiAction::StopAgent)),
        KeyCode::Char('r') => Some(UiCommand::Dispatch(UiAction::RestartAgent)),
        KeyCode::Char('q') => Some(UiCommand::Dispatch(UiAction::RunVerifyQuick)),
        KeyCode::Char('f') => Some(UiCommand::Dispatch(UiAction::RunVerifyFull)),
        KeyCode::Char('t') => Some(UiCommand::Dispatch(UiAction::TriggerRestack)),
        KeyCode::Char('n') => Some(UiCommand::Dispatch(UiAction::MarkNeedsHuman)),
        KeyCode::Char('w') => Some(UiCommand::Dispatch(UiAction::OpenWebUiForTask)),
        KeyCode::Char('p') => Some(UiCommand::Dispatch(UiAction::PauseTask)),
        KeyCode::Char('u') => Some(UiCommand::Dispatch(UiAction::ResumeTask)),
        _ => None,
    }
}

pub fn action_label(action: UiAction) -> &'static str {
    match action {
        UiAction::CreateTask => "create_task",
        UiAction::StartAgent => "start_agent",
        UiAction::StopAgent => "stop_agent",
        UiAction::RestartAgent => "restart_agent",
        UiAction::RunVerifyQuick => "run_verify_quick",
        UiAction::RunVerifyFull => "run_verify_full",
        UiAction::TriggerRestack => "trigger_restack",
        UiAction::MarkNeedsHuman => "mark_needs_human",
        UiAction::OpenWebUiForTask => "open_web_ui_for_task",
        UiAction::PauseTask => "pause_task",
        UiAction::ResumeTask => "resume_task",
    }
}
