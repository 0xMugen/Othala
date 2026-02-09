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
    let help = "keys: c=create a=approve s=start x=stop r=restart q=quick f=full t=restack n=needs-human w=web p=pause u=resume | arrows=select tab=focus esc/ctrl-c=quit";
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::state::TaskState;
    use orch_core::types::{ModelKind, RepoId, TaskId};

    use crate::model::{AgentPane, AgentPaneStatus, DashboardState, TaskOverviewRow};
    use crate::TuiApp;

    use super::{format_pane_tabs, format_task_row, pane_status_tag, to_local_time};

    fn mk_row(task_id: &str) -> TaskOverviewRow {
        TaskOverviewRow {
            task_id: TaskId(task_id.to_string()),
            repo_id: RepoId("example".to_string()),
            branch: format!("task/{task_id}"),
            stack_position: None,
            state: TaskState::Running,
            verify_summary: "not_run".to_string(),
            review_summary: "0/0 unanimous=false cap=ok".to_string(),
            last_activity: Utc::now(),
        }
    }

    #[test]
    fn pane_status_tag_maps_all_statuses() {
        let task_id = TaskId("T1".to_string());

        let mut pane = AgentPane::new("A1", task_id.clone(), ModelKind::Codex);
        pane.status = AgentPaneStatus::Starting;
        assert_eq!(pane_status_tag(&pane), "starting");

        pane.status = AgentPaneStatus::Running;
        assert_eq!(pane_status_tag(&pane), "running");

        pane.status = AgentPaneStatus::Waiting;
        assert_eq!(pane_status_tag(&pane), "waiting");

        pane.status = AgentPaneStatus::Exited;
        assert_eq!(pane_status_tag(&pane), "exited");

        pane.status = AgentPaneStatus::Failed;
        assert_eq!(pane_status_tag(&pane), "failed");
    }

    #[test]
    fn format_pane_tabs_handles_empty_and_selected_pane() {
        let mut app = TuiApp::default();
        assert_eq!(format_pane_tabs(&app), "none");

        app.state.panes = vec![
            AgentPane::new("A1", TaskId("T1".to_string()), ModelKind::Codex),
            AgentPane::new("A2", TaskId("T2".to_string()), ModelKind::Claude),
        ];
        app.state.panes[0].status = AgentPaneStatus::Running;
        app.state.panes[1].status = AgentPaneStatus::Waiting;
        app.state.selected_pane_idx = 1;

        let tabs = format_pane_tabs(&app);
        assert_eq!(tabs, " 1:A1:running | *2:A2:waiting");
    }

    #[test]
    fn format_task_row_includes_expected_columns() {
        let row = mk_row("T9");
        let output = format_task_row(">", &row);
        let expected_ts = to_local_time(row.last_activity);

        assert!(output.contains("> example | T9 | task/T9 | Running"));
        assert!(output.contains("| not_run | 0/0 unanimous=false cap=ok |"));
        assert!(output.ends_with(&expected_ts));
    }

    #[test]
    fn to_local_time_uses_fixed_format() {
        let dt = chrono::DateTime::parse_from_rfc3339("2026-02-08T12:34:56Z")
            .expect("parse rfc3339")
            .with_timezone(&Utc);
        let formatted = to_local_time(dt);

        assert_eq!(formatted.len(), 19);
        assert_eq!(formatted.chars().nth(4), Some('-'));
        assert_eq!(formatted.chars().nth(7), Some('-'));
        assert_eq!(formatted.chars().nth(10), Some(' '));
        assert_eq!(formatted.chars().nth(13), Some(':'));
        assert_eq!(formatted.chars().nth(16), Some(':'));
    }

    #[test]
    fn format_pane_tabs_marks_first_selected_by_default() {
        let mut state = DashboardState::default();
        state.panes.push(AgentPane::new(
            "A1",
            TaskId("T1".to_string()),
            ModelKind::Codex,
        ));
        state.panes[0].status = AgentPaneStatus::Starting;

        let app = TuiApp {
            state,
            ..TuiApp::default()
        };
        assert_eq!(format_pane_tabs(&app), "*1:A1:starting");
    }
}
