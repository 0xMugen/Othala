use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAction {
    CreateTask,
    ApproveTask,
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
        KeyCode::Char('a') => Some(UiCommand::Dispatch(UiAction::ApproveTask)),
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
        UiAction::ApproveTask => "approve_task",
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

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{action_label, map_key_to_command, UiAction, UiCommand};

    #[test]
    fn map_key_to_command_maps_navigation_and_actions() {
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            Some(UiCommand::SelectNextTask)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(UiCommand::SelectPreviousTask)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
            Some(UiCommand::SelectNextPane)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            Some(UiCommand::SelectPreviousPane)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            Some(UiCommand::ToggleFocusedPane)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(UiCommand::Dispatch(UiAction::ApproveTask))
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(UiCommand::Dispatch(UiAction::RunVerifyQuick))
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE)),
            Some(UiCommand::Dispatch(UiAction::RunVerifyFull))
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
            Some(UiCommand::Dispatch(UiAction::PauseTask))
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE)),
            Some(UiCommand::Dispatch(UiAction::ResumeTask))
        );
    }

    #[test]
    fn map_key_to_command_maps_quit_shortcuts_and_ignores_unknown() {
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(UiCommand::Quit)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(UiCommand::Quit)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn action_label_matches_expected_snake_case_values() {
        assert_eq!(action_label(UiAction::CreateTask), "create_task");
        assert_eq!(action_label(UiAction::ApproveTask), "approve_task");
        assert_eq!(action_label(UiAction::StartAgent), "start_agent");
        assert_eq!(action_label(UiAction::StopAgent), "stop_agent");
        assert_eq!(action_label(UiAction::RestartAgent), "restart_agent");
        assert_eq!(action_label(UiAction::RunVerifyQuick), "run_verify_quick");
        assert_eq!(action_label(UiAction::RunVerifyFull), "run_verify_full");
        assert_eq!(action_label(UiAction::TriggerRestack), "trigger_restack");
        assert_eq!(action_label(UiAction::MarkNeedsHuman), "mark_needs_human");
        assert_eq!(
            action_label(UiAction::OpenWebUiForTask),
            "open_web_ui_for_task"
        );
        assert_eq!(action_label(UiAction::PauseTask), "pause_task");
        assert_eq!(action_label(UiAction::ResumeTask), "resume_task");
    }
}
