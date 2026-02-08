use chrono::{DateTime, Local, Utc};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::TuiApp;
use crate::model::{AgentPane, TaskOverviewRow};

pub fn render_dashboard(frame: &mut Frame<'_>, app: &TuiApp) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(frame.area());

    render_header(frame, root[0], app);

    if app.state.focused_pane_idx.is_some() {
        render_focused_pane(frame, root[1], app);
    } else {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(root[1]);
        render_task_list(frame, body[0], app);
        render_pane_summary(frame, body[1], app);
    }

    render_footer(frame, root[2], app);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let selected_task = app
        .state
        .selected_task()
        .map(|task| task.task_id.0.as_str())
        .unwrap_or("-");
    let selected_pane = app
        .state
        .selected_pane()
        .map(|pane| pane.instance_id.as_str())
        .unwrap_or("-");

    let line = format!(
        "Othala Command Center | tasks={} panes={} selected_task={} selected_pane={}",
        app.state.tasks.len(),
        app.state.panes.len(),
        selected_task,
        selected_pane
    );
    let widget = Paragraph::new(line)
        .block(Block::default().borders(Borders::ALL).title("Overview"))
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}

fn render_task_list(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let mut lines = Vec::new();
    lines.push(Line::from(
        "repo | task | branch | state | verify | review | last_activity",
    ));
    lines.push(Line::from(
        "----------------------------------------------------------------",
    ));

    for (idx, task) in app.state.tasks.iter().enumerate() {
        let prefix = if idx == app.state.selected_task_idx {
            ">"
        } else {
            " "
        };
        lines.push(Line::from(format_task_row(prefix, task)));
    }

    if app.state.tasks.is_empty() {
        lines.push(Line::from("no tasks"));
    }

    let widget = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Tasks"))
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);
}

fn render_pane_summary(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let panes = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let pane_tabs = format_pane_tabs(app);
    let tabs = Paragraph::new(pane_tabs)
        .block(Block::default().borders(Borders::ALL).title("Agent Panes"))
        .wrap(Wrap { trim: true });
    frame.render_widget(tabs, panes[0]);

    let lines = if let Some(pane) = app.state.selected_pane() {
        pane.tail(20)
            .into_iter()
            .map(Line::from)
            .collect::<Vec<_>>()
    } else {
        vec![Line::from("no running agent panes")]
    };

    let title = app
        .state
        .selected_pane()
        .map(|pane| {
            format!(
                "PTY {} ({:?}, task={})",
                pane.instance_id, pane.model, pane.task_id.0
            )
        })
        .unwrap_or_else(|| "PTY".to_string());

    let output = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(output, panes[1]);
}

fn render_focused_pane(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let pane = app
        .state
        .focused_pane_idx
        .and_then(|idx| app.state.panes.get(idx));

    let (title, lines) = if let Some(pane) = pane {
        (
            format!(
                "Focused PTY {} ({:?}, task={})",
                pane.instance_id, pane.model, pane.task_id.0
            ),
            pane.tail(200)
                .into_iter()
                .map(Line::from)
                .collect::<Vec<_>>(),
        )
    } else {
        (
            "Focused PTY".to_string(),
            vec![Line::from("focused pane no longer exists")],
        )
    };

    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let help = "keys: c=create s=start x=stop r=restart q=quick f=full t=restack n=needs-human w=web p=pause u=resume | arrows=select tab=focus esc/ctrl-c=quit";
    let line = format!("{} | status: {}", help, app.state.status_line);
    let widget = Paragraph::new(line)
        .block(Block::default().borders(Borders::ALL).title("Actions"))
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}

fn format_task_row(prefix: &str, task: &TaskOverviewRow) -> String {
    let ts = to_local_time(task.last_activity);
    format!(
        "{} {} | {} | {} | {:?} | {} | {} | {}",
        prefix,
        task.repo_id.0,
        task.task_id.0,
        task.branch,
        task.state,
        task.verify_summary,
        task.review_summary,
        ts
    )
}

fn format_pane_tabs(app: &TuiApp) -> String {
    if app.state.panes.is_empty() {
        return "none".to_string();
    }

    let mut out = String::new();
    for (idx, pane) in app.state.panes.iter().enumerate() {
        if idx > 0 {
            out.push_str(" | ");
        }
        if idx == app.state.selected_pane_idx {
            out.push('*');
        } else {
            out.push(' ');
        }
        out.push_str(&format!(
            "{}:{}:{}",
            idx + 1,
            pane.instance_id,
            pane_status_tag(pane)
        ));
    }
    out
}

fn pane_status_tag(pane: &AgentPane) -> &'static str {
    match pane.status {
        crate::model::AgentPaneStatus::Starting => "starting",
        crate::model::AgentPaneStatus::Running => "running",
        crate::model::AgentPaneStatus::Waiting => "waiting",
        crate::model::AgentPaneStatus::Exited => "exited",
        crate::model::AgentPaneStatus::Failed => "failed",
    }
}

fn to_local_time(value: DateTime<Utc>) -> String {
    value
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}
