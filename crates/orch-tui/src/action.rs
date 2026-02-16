use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAction {
    CreateTask,
    ApproveTask,
    SubmitTask,
    StartAgent,
    StopAgent,
    RestartAgent,
    DeleteTask,
    RunVerifyQuick,
    RunVerifyFull,
    TriggerRestack,
    MarkNeedsHuman,
    OpenWebUiForTask,
    PauseTask,
    ResumeTask,
    SendChatMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiCommand {
    Dispatch(UiAction),
    SelectNextTask,
    SelectPreviousTask,
    SelectNextPane,
    SelectPreviousPane,
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    GoToFirstTask,
    GoToLastTask,
    CycleTheme,
    CycleSort,
    ToggleSortReverse,
    StartFilter,
    CycleStateFilter,
    ToggleFocusedPane,
    ToggleFocusedTask,
    ShowHelp,
    Quit,
}

pub fn map_key_to_command(key: KeyEvent) -> Option<UiCommand> {
    if key.kind != KeyEventKind::Press {
        return None;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(UiCommand::Quit);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
        return Some(UiCommand::ScrollUp);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
        return Some(UiCommand::ScrollDown);
    }
    if key.code == KeyCode::Esc {
        return Some(UiCommand::Quit);
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::Char('f') {
        return Some(UiCommand::CycleStateFilter);
    }

    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Some(UiCommand::SelectNextTask),
        KeyCode::Up | KeyCode::Char('k') => Some(UiCommand::SelectPreviousTask),
        KeyCode::Right | KeyCode::Char('l') => Some(UiCommand::SelectNextPane),
        KeyCode::Left | KeyCode::Char('h') => Some(UiCommand::SelectPreviousPane),
        KeyCode::PageUp => Some(UiCommand::ScrollUp),
        KeyCode::PageDown => Some(UiCommand::ScrollDown),
        KeyCode::Home => Some(UiCommand::GoToFirstTask),
        KeyCode::End | KeyCode::Char('G') => Some(UiCommand::GoToLastTask),
        KeyCode::Char('T') => Some(UiCommand::CycleTheme),
        KeyCode::Char('S') => Some(UiCommand::CycleSort),
        KeyCode::Char('R') => Some(UiCommand::ToggleSortReverse),
        KeyCode::Tab => Some(UiCommand::ToggleFocusedPane),
        KeyCode::Enter => Some(UiCommand::ToggleFocusedTask),
        KeyCode::Char('/') => Some(UiCommand::StartFilter),
        KeyCode::Char('F') => Some(UiCommand::CycleStateFilter),
        KeyCode::Char('?') => Some(UiCommand::ShowHelp),
        KeyCode::Char('c') => Some(UiCommand::Dispatch(UiAction::CreateTask)),
        KeyCode::Char('a') => Some(UiCommand::Dispatch(UiAction::ApproveTask)),
        KeyCode::Char('g') => Some(UiCommand::Dispatch(UiAction::SubmitTask)),
        KeyCode::Char('s') => Some(UiCommand::Dispatch(UiAction::StartAgent)),
        KeyCode::Char('x') => Some(UiCommand::Dispatch(UiAction::StopAgent)),
        KeyCode::Char('r') => Some(UiCommand::Dispatch(UiAction::RestartAgent)),
        KeyCode::Char('d') => Some(UiCommand::Dispatch(UiAction::DeleteTask)),
        KeyCode::Char('q') => Some(UiCommand::Dispatch(UiAction::RunVerifyQuick)),
        KeyCode::Char('f') => Some(UiCommand::Dispatch(UiAction::RunVerifyFull)),
        KeyCode::Char('t') => Some(UiCommand::Dispatch(UiAction::TriggerRestack)),
        KeyCode::Char('n') => Some(UiCommand::Dispatch(UiAction::MarkNeedsHuman)),
        KeyCode::Char('w') => Some(UiCommand::Dispatch(UiAction::OpenWebUiForTask)),
        KeyCode::Char('p') => Some(UiCommand::Dispatch(UiAction::PauseTask)),
        KeyCode::Char('u') => Some(UiCommand::Dispatch(UiAction::ResumeTask)),
        KeyCode::Char('i') => Some(UiCommand::Dispatch(UiAction::SendChatMessage)),
        _ => None,
    }
}

pub fn action_label(action: UiAction) -> &'static str {
    match action {
        UiAction::CreateTask => "create_task",
        UiAction::ApproveTask => "approve_task",
        UiAction::SubmitTask => "submit_task",
        UiAction::StartAgent => "start_agent",
        UiAction::StopAgent => "stop_agent",
        UiAction::RestartAgent => "restart_agent",
        UiAction::DeleteTask => "delete_task",
        UiAction::RunVerifyQuick => "run_verify_quick",
        UiAction::RunVerifyFull => "run_verify_full",
        UiAction::TriggerRestack => "trigger_restack",
        UiAction::MarkNeedsHuman => "mark_needs_human",
        UiAction::OpenWebUiForTask => "open_web_ui_for_task",
        UiAction::PauseTask => "pause_task",
        UiAction::ResumeTask => "resume_task",
        UiAction::SendChatMessage => "send_chat_message",
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
            map_key_to_command(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(UiCommand::SelectNextTask)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(UiCommand::SelectPreviousTask)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(UiCommand::SelectPreviousTask)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
            Some(UiCommand::SelectNextPane)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
            Some(UiCommand::SelectNextPane)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            Some(UiCommand::SelectPreviousPane)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)),
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
            map_key_to_command(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
            Some(UiCommand::Dispatch(UiAction::SubmitTask))
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(UiCommand::Dispatch(UiAction::RunVerifyQuick))
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some(UiCommand::Dispatch(UiAction::DeleteTask))
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
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(UiCommand::ToggleFocusedTask)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
            Some(UiCommand::StartFilter)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT)),
            Some(UiCommand::CycleStateFilter)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::SHIFT)),
            Some(UiCommand::CycleStateFilter)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE)),
            Some(UiCommand::Dispatch(UiAction::SendChatMessage))
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
    fn map_key_to_command_maps_vim_keybindings() {
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            Some(UiCommand::ScrollUp)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(UiCommand::ScrollDown)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
            Some(UiCommand::ScrollUp)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(UiCommand::ScrollDown)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
            Some(UiCommand::GoToFirstTask)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
            Some(UiCommand::GoToLastTask)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(UiCommand::GoToLastTask)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('T'), KeyModifiers::SHIFT)),
            Some(UiCommand::CycleTheme)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT)),
            Some(UiCommand::CycleSort)
        );
        assert_eq!(
            map_key_to_command(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT)),
            Some(UiCommand::ToggleSortReverse)
        );
    }

    #[test]
    fn action_label_matches_expected_snake_case_values() {
        assert_eq!(action_label(UiAction::CreateTask), "create_task");
        assert_eq!(action_label(UiAction::ApproveTask), "approve_task");
        assert_eq!(action_label(UiAction::SubmitTask), "submit_task");
        assert_eq!(action_label(UiAction::StartAgent), "start_agent");
        assert_eq!(action_label(UiAction::StopAgent), "stop_agent");
        assert_eq!(action_label(UiAction::RestartAgent), "restart_agent");
        assert_eq!(action_label(UiAction::DeleteTask), "delete_task");
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
        assert_eq!(action_label(UiAction::SendChatMessage), "send_chat_message");
    }
}
