pub mod action;
pub mod app;
pub mod error;
pub mod event;
pub mod model;
pub mod runner;
pub mod ui;

pub use action::*;
pub use app::*;
pub use error::*;
pub use event::*;
pub use model::*;
pub use runner::*;
pub use ui::*;

#[cfg(test)]
mod tests {
    use super::{
        action_label, map_key_to_command, render_dashboard, run_tui, AgentPane, DashboardState,
        QueuedAction, TaskOverviewRow, TuiApp, TuiError, TuiEvent, UiAction, UiCommand,
    };
    use std::any::TypeId;
    use std::time::Duration;

    #[test]
    fn crate_root_reexports_types() {
        let _ = TypeId::of::<TuiError>();
        let _ = TypeId::of::<TuiEvent>();
        let _ = TypeId::of::<QueuedAction>();
        let _ = TypeId::of::<TuiApp>();
        let _ = TypeId::of::<TaskOverviewRow>();
        let _ = TypeId::of::<AgentPane>();
        let _ = TypeId::of::<DashboardState>();
        let _ = TypeId::of::<UiAction>();
        let _ = TypeId::of::<UiCommand>();
    }

    #[test]
    fn crate_root_reexports_helper_functions() {
        let _map: fn(crossterm::event::KeyEvent) -> Option<UiCommand> = map_key_to_command;
        let _label: fn(UiAction) -> &'static str = action_label;
        let _run: fn(&mut TuiApp, Duration) -> Result<(), TuiError> = run_tui;
        let _render: fn(&mut ratatui::Frame<'_>, &TuiApp) = render_dashboard;
    }
}
