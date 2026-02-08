use crate::app::TuiApp;
use crate::error::TuiError;
use crate::ui::render_dashboard;
use crossterm::event::{self, Event as CEvent};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::time::Duration;

pub fn run_tui(app: &mut TuiApp, tick_rate: Duration) -> Result<(), TuiError> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let run_result = run_loop(&mut terminal, app, tick_rate);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    run_result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
    tick_rate: Duration,
) -> Result<(), TuiError> {
    while !app.should_quit {
        terminal.draw(|frame| render_dashboard(frame, app))?;

        if event::poll(tick_rate)? {
            handle_terminal_event(app, event::read()?);
        }
    }
    Ok(())
}

fn handle_terminal_event(app: &mut TuiApp, event: CEvent) {
    match event {
        CEvent::Key(key) => app.handle_key_event(key),
        CEvent::Resize(_, _) => {}
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};

    use crate::runner::handle_terminal_event;
    use crate::TuiApp;

    #[test]
    fn handle_terminal_event_routes_key_events_to_app() {
        let mut app = TuiApp::default();
        handle_terminal_event(
            &mut app,
            CEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(app.should_quit);
    }

    #[test]
    fn handle_terminal_event_ignores_resize_events() {
        let mut app = TuiApp::default();
        handle_terminal_event(&mut app, CEvent::Resize(120, 40));
        assert!(!app.should_quit);
    }
}
