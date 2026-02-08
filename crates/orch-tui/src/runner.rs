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
            match event::read()? {
                CEvent::Key(key) => app.handle_key_event(key),
                CEvent::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}
