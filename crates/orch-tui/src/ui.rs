use chrono::{DateTime, Local, Utc};
use orch_core::state::TaskState;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::TuiApp;
use crate::model::{AgentPane, AgentPaneStatus, TaskOverviewRow};

// -- Color palette ----------------------------------------------------------

const ACCENT: Color = Color::Cyan;
const HEADER_FG: Color = Color::White;
const HEADER_TITLE: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const SELECTED_BG: Color = Color::Indexed(236); // dark gray background
const BORDER_NORMAL: Color = Color::DarkGray;
const BORDER_FOCUSED: Color = Color::Cyan;
const KEY_FG: Color = Color::Yellow;
const MUTED: Color = Color::Gray;

fn state_color(state: TaskState) -> Color {
    match state {
        TaskState::Running | TaskState::VerifyingQuick | TaskState::VerifyingFull => Color::Green,
        TaskState::Ready | TaskState::Merged => Color::Cyan,
        TaskState::Submitting | TaskState::AwaitingMerge => Color::Blue,
        TaskState::Reviewing | TaskState::DraftPrOpen => Color::Magenta,
        TaskState::NeedsHuman | TaskState::RestackConflict => Color::Yellow,
        TaskState::Failed => Color::Red,
        TaskState::Paused => Color::DarkGray,
        TaskState::Queued | TaskState::Initializing | TaskState::Restacking => Color::White,
    }
}

/// Pick a color for the composite display state label.  Falls back to
/// `state_color` for states that are not overridden by verify status.
fn display_state_color(state: TaskState, display_state: &str) -> Color {
    match display_state {
        "VerifyFail" => Color::Red,
        "Verified" => Color::Cyan,
        "Verifying" => Color::Yellow,
        _ => state_color(state),
    }
}

fn pane_status_color(status: AgentPaneStatus) -> Color {
    match status {
        AgentPaneStatus::Starting => Color::Yellow,
        AgentPaneStatus::Running => Color::Green,
        AgentPaneStatus::Waiting => Color::Magenta,
        AgentPaneStatus::Exited => Color::DarkGray,
        AgentPaneStatus::Failed => Color::Red,
    }
}

fn normal_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER_NORMAL))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
}

fn focused_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(BORDER_FOCUSED)
                .add_modifier(Modifier::BOLD),
        )
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
}

// -- Layout -----------------------------------------------------------------

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

    if app.state.focused_task {
        render_focused_task(frame, root[1], app);
    } else if app.state.focused_pane_idx.is_some() {
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

    if let Some((task_id, branch)) = app.delete_confirm_display() {
        render_delete_confirm_modal(frame, &task_id.0, branch);
    }
}

// -- Header -----------------------------------------------------------------

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

    let line = Line::from(vec![
        Span::styled(
            " Othala ",
            Style::default()
                .fg(HEADER_TITLE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" tasks:", Style::default().fg(DIM)),
        Span::styled(
            format!("{}", app.state.tasks.len()),
            Style::default().fg(HEADER_FG),
        ),
        Span::styled("  panes:", Style::default().fg(DIM)),
        Span::styled(
            format!("{}", app.state.panes.len()),
            Style::default().fg(HEADER_FG),
        ),
        Span::styled("  task:", Style::default().fg(DIM)),
        Span::styled(selected_task, Style::default().fg(ACCENT)),
        Span::styled("  pane:", Style::default().fg(DIM)),
        Span::styled(selected_pane, Style::default().fg(ACCENT)),
    ]);

    let widget = Paragraph::new(line)
        .block(normal_block("Overview"))
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}

// -- Task list --------------------------------------------------------------

fn render_task_list(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let mut lines = Vec::new();

    let header_style = Style::default().fg(DIM).add_modifier(Modifier::BOLD);
    lines.push(Line::from(Span::styled(
        " repo | task | branch | state | verify | review | activity",
        header_style,
    )));
    lines.push(Line::from(Span::styled(
        String::from_utf8(vec![b'\xe2', b'\x94', b'\x80'])
            .unwrap_or_else(|_| "-".to_string())
            .repeat(area.width.saturating_sub(2) as usize),
        Style::default().fg(DIM),
    )));

    for (idx, task) in app.state.tasks.iter().enumerate() {
        let is_selected = idx == app.state.selected_task_idx;
        lines.push(format_task_row(is_selected, task));
    }

    if app.state.tasks.is_empty() {
        lines.push(Line::from(Span::styled(
            " no tasks",
            Style::default().fg(DIM),
        )));
    }

    let widget = Paragraph::new(lines)
        .block(normal_block("Tasks"))
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);
}

// -- Pane summary -----------------------------------------------------------

fn render_pane_summary(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let panes = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let pane_tabs = format_pane_tabs(app);
    let tabs = Paragraph::new(pane_tabs)
        .block(normal_block("Agent Panes"))
        .wrap(Wrap { trim: true });
    frame.render_widget(tabs, panes[0]);

    let (title, lines) = if let Some(pane) = app.state.selected_pane() {
        (
            format!(
                "PTY {} ({:?}, task={})",
                pane.instance_id, pane.model, pane.task_id.0
            ),
            pane.tail(20)
                .into_iter()
                .map(|s| Line::from(Span::styled(s, Style::default().fg(MUTED))))
                .collect::<Vec<_>>(),
        )
    } else {
        let selected_task = app
            .state
            .selected_task()
            .map(|task| task.task_id.0.clone())
            .unwrap_or_else(|| "-".to_string());
        let lines = if app.state.selected_task_activity.is_empty() {
            vec![Line::from(Span::styled(
                "no task activity yet",
                Style::default().fg(DIM),
            ))]
        } else {
            app.state
                .selected_task_activity
                .iter()
                .cloned()
                .map(|s| Line::from(Span::styled(s, Style::default().fg(MUTED))))
                .collect::<Vec<_>>()
        };
        (format!("Task Activity ({selected_task})"), lines)
    };

    let output = Paragraph::new(lines)
        .block(normal_block(&title))
        .wrap(Wrap { trim: false });
    frame.render_widget(output, panes[1]);
}

// -- Focused views ----------------------------------------------------------

fn render_focused_pane(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let pane = app
        .state
        .focused_pane_idx
        .and_then(|idx| app.state.panes.get(idx));

    let viewport_height = area.height.saturating_sub(2) as usize;
    let scroll_back = app.state.scroll_back;

    let (title, lines) = if let Some(pane) = pane {
        let scroll_hint = if scroll_back > 0 {
            format!(" [+{}]", scroll_back)
        } else {
            String::new()
        };
        (
            format!(
                "Focused PTY {} ({:?}, task={}){}",
                pane.instance_id, pane.model, pane.task_id.0, scroll_hint
            ),
            pane.window(viewport_height, scroll_back)
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
        .block(focused_block(&title))
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);
}

fn render_focused_task(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let selected_task = app.state.selected_task();
    let task_id_str = selected_task
        .map(|t| t.task_id.0.clone())
        .unwrap_or_else(|| "-".to_string());

    // Find the agent pane for this task
    let task_pane =
        selected_task.and_then(|task| app.state.panes.iter().find(|p| p.task_id == task.task_id));

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    let viewport_height = cols[1].height.saturating_sub(2) as usize;
    let scroll_back = app.state.scroll_back;

    // Left: task status checklist
    let status_title = format!("Status ({task_id_str})");
    let status_widget = Paragraph::new(status_sidebar_lines(selected_task))
        .block(focused_block(&status_title))
        .wrap(Wrap { trim: false });
    frame.render_widget(status_widget, cols[0]);

    // Right: agent PTY output
    let (pty_title, pty_lines) = if let Some(pane) = task_pane {
        let scroll_hint = if scroll_back > 0 {
            format!(" [+{}]", scroll_back)
        } else {
            String::new()
        };
        (
            format!(
                "Agent {} ({:?}, task={}){}",
                pane.instance_id, pane.model, pane.task_id.0, scroll_hint
            ),
            pane.window(viewport_height, scroll_back)
                .into_iter()
                .map(Line::from)
                .collect::<Vec<_>>(),
        )
    } else {
        (
            format!("Agent (task={task_id_str})"),
            vec![Line::from("no agent running for this task")],
        )
    };

    let pty_widget = Paragraph::new(pty_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(pty_title)
                .border_style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(pty_widget, cols[1]);
}

// -- Footer -----------------------------------------------------------------

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let line = if let Some((task_id, branch)) = app.delete_confirm_display() {
        let branch_label = branch.unwrap_or("-");
        Line::from(vec![
            Span::styled(" delete: ", Style::default().fg(DIM)),
            Span::styled(task_id.0.clone(), Style::default().fg(HEADER_FG)),
            Span::styled(" branch=", Style::default().fg(DIM)),
            Span::styled(branch_label, Style::default().fg(HEADER_FG)),
            Span::styled("  Enter=confirm Esc=cancel", Style::default().fg(DIM)),
        ])
    } else if let Some((models, selected)) = app.model_select_display() {
        let mut spans = vec![Span::styled(" model: ", Style::default().fg(DIM))];
        for (i, m) in models.iter().enumerate() {
            if i == selected {
                spans.push(Span::styled(
                    format!(" {m:?} "),
                    Style::default()
                        .fg(Color::Black)
                        .bg(ACCENT)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(format!(" {m:?} "), Style::default().fg(MUTED)));
            }
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            " Up/Down=cycle Enter=confirm Esc=cancel",
            Style::default().fg(DIM),
        ));
        Line::from(spans)
    } else if let Some(prompt) = app.input_prompt() {
        Line::from(vec![
            Span::styled(" prompt: ", Style::default().fg(DIM)),
            Span::styled(prompt, Style::default().fg(HEADER_FG)),
            Span::styled("_", Style::default().fg(ACCENT)),
            Span::styled("  Enter=submit Esc=cancel", Style::default().fg(DIM)),
        ])
    } else {
        let mut spans: Vec<Span<'_>> = Vec::new();
        spans.push(Span::raw(" "));
        let keys: &[(&str, &str)] = &[
            ("c", "chat"),
            ("a", "approve"),
            ("g", "submit"),
            ("s", "start"),
            ("x", "stop"),
            ("r", "restart"),
            ("d", "delete"),
            ("q", "quick"),
            ("f", "full"),
            ("t", "restack/submit"),
            ("n", "human"),
            ("w", "web"),
            ("p", "pause"),
            ("u", "resume"),
        ];
        for (key, label) in keys {
            spans.push(Span::styled(
                *key,
                Style::default().fg(KEY_FG).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!("={label} "),
                Style::default().fg(MUTED),
            ));
        }
        if app.state.focused_task || app.state.focused_pane_idx.is_some() {
            spans.push(Span::styled(
                "| \u{2191}\u{2193}=scroll PgUp/Dn=page Home/End=top/bottom esc=back",
                Style::default().fg(DIM),
            ));
        } else {
            spans.push(Span::styled(
                "| \u{2191}\u{2193}=select \u{21B9}=focus \u{23CE}=detail esc=quit",
                Style::default().fg(DIM),
            ));
        }
        if !app.state.status_line.is_empty() {
            spans.push(Span::styled(
                format!(" | {}", app.state.status_line),
                Style::default().fg(ACCENT),
            ));
        }
        Line::from(spans)
    };

    let title = if app.delete_confirm_display().is_some() {
        "Confirm Delete"
    } else if app.model_select_display().is_some() {
        "Select Model"
    } else if app.input_prompt().is_some() {
        "New Chat"
    } else {
        "Actions"
    };

    let widget = Paragraph::new(line)
        .block(normal_block(title))
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}

fn render_delete_confirm_modal(frame: &mut Frame<'_>, task_id: &str, branch: Option<&str>) {
    let area = centered_rect(64, 36, frame.area());
    let branch_line = match branch {
        Some(value) => format!("Branch to delete: {value}"),
        None => "Branch to delete: (none)".to_string(),
    };
    let lines = vec![
        Line::from(Span::styled(
            format!("Delete task {task_id}?"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("This permanently removes task state from local storage."),
        Line::from("It also removes the task worktree and local branch."),
        Line::from(branch_line),
        Line::from(""),
        Line::from(Span::styled(
            "Enter/Y = delete now    Esc = cancel",
            Style::default().fg(DIM),
        )),
    ];

    let widget = Paragraph::new(lines)
        .block(focused_block("Are You Sure?"))
        .wrap(Wrap { trim: true });
    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

// -- Formatting helpers -----------------------------------------------------

fn format_task_row<'a>(is_selected: bool, task: &'a TaskOverviewRow) -> Line<'a> {
    let ts = to_local_time(task.last_activity);
    let sc = display_state_color(task.state, &task.display_state);

    let base_style = if is_selected {
        Style::default().bg(SELECTED_BG).fg(Color::White)
    } else {
        Style::default().fg(MUTED)
    };

    let prefix = if is_selected { "\u{25B6} " } else { "  " };

    Line::from(vec![
        Span::styled(
            prefix,
            if is_selected {
                Style::default().fg(ACCENT)
            } else {
                Style::default().fg(DIM)
            },
        ),
        Span::styled(&task.repo_id.0, base_style),
        Span::styled(" | ", Style::default().fg(DIM)),
        Span::styled(
            &task.task_id.0,
            if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
                    .bg(SELECTED_BG)
            } else {
                Style::default().fg(Color::White)
            },
        ),
        Span::styled(" | ", Style::default().fg(DIM)),
        Span::styled(&task.branch, base_style),
        Span::styled(" | ", Style::default().fg(DIM)),
        Span::styled(
            task.display_state.as_str(),
            Style::default().fg(sc).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | ", Style::default().fg(DIM)),
        Span::styled(&task.verify_summary, base_style),
        Span::styled(" | ", Style::default().fg(DIM)),
        Span::styled(&task.review_summary, base_style),
        Span::styled(" | ", Style::default().fg(DIM)),
        Span::styled(ts, Style::default().fg(DIM)),
    ])
}

fn format_pane_tabs(app: &TuiApp) -> Line<'static> {
    if app.state.panes.is_empty() {
        return Line::from(Span::styled(" none", Style::default().fg(DIM)));
    }

    let mut spans = Vec::new();
    spans.push(Span::raw(" "));
    for (idx, pane) in app.state.panes.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" | ", Style::default().fg(DIM)));
        }
        let is_selected = idx == app.state.selected_pane_idx;
        let tag = pane_status_tag(pane);
        let sc = pane_status_color(pane.status);

        let label = format!("{}:{}", idx + 1, pane.instance_id);
        if is_selected {
            spans.push(Span::styled(
                label,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(label, Style::default().fg(MUTED)));
        }
        spans.push(Span::styled(format!(":{tag}"), Style::default().fg(sc)));
    }
    Line::from(spans)
}

fn pane_status_tag(pane: &AgentPane) -> &'static str {
    match pane.status {
        AgentPaneStatus::Starting => "starting",
        AgentPaneStatus::Running => "running",
        AgentPaneStatus::Waiting => "waiting",
        AgentPaneStatus::Exited => "exited",
        AgentPaneStatus::Failed => "failed",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChecklistState {
    Done,
    Pending,
    Skipped,
    Active,
    Blocked,
}

fn checklist_line(label: &str, state: ChecklistState) -> Line<'static> {
    let (marker, marker_color, label_color) = match state {
        ChecklistState::Done => ("x", Color::Green, Color::White),
        ChecklistState::Pending => (" ", DIM, MUTED),
        ChecklistState::Skipped => ("-", Color::Yellow, DIM),
        ChecklistState::Active => ("~", Color::Cyan, Color::White),
        ChecklistState::Blocked => ("!", Color::Red, Color::White),
    };
    Line::from(vec![
        Span::styled(
            format!("[{marker}] "),
            Style::default()
                .fg(marker_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(label.to_string(), Style::default().fg(label_color)),
    ])
}

fn status_sidebar_lines(task: Option<&TaskOverviewRow>) -> Vec<Line<'static>> {
    let Some(task) = task else {
        return vec![Line::from(Span::styled(
            "no task selected",
            Style::default().fg(DIM),
        ))];
    };

    let thinking = if matches!(task.state, TaskState::Queued | TaskState::Initializing) {
        ChecklistState::Pending
    } else {
        ChecklistState::Done
    };
    let pushing = if matches!(task.state, TaskState::Queued | TaskState::Initializing) {
        ChecklistState::Pending
    } else {
        ChecklistState::Done
    };
    let reviewing = ChecklistState::Skipped;
    let restacking = match task.state {
        TaskState::Restacking => ChecklistState::Active,
        TaskState::RestackConflict => ChecklistState::Blocked,
        _ => ChecklistState::Skipped,
    };
    let ready_to_merge = if matches!(
        task.state,
        TaskState::Ready | TaskState::AwaitingMerge | TaskState::Merged
    ) {
        ChecklistState::Done
    } else {
        ChecklistState::Pending
    };

    let plan_complete = if ready_to_merge == ChecklistState::Done {
        "yes"
    } else {
        "no"
    };

    vec![
        Line::from(vec![
            Span::styled("plan complete: ", Style::default().fg(DIM)),
            Span::styled(
                plan_complete.to_string(),
                Style::default()
                    .fg(if plan_complete == "yes" {
                        Color::Green
                    } else {
                        Color::Yellow
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("status: ", Style::default().fg(DIM)),
            Span::styled(
                task.display_state.clone(),
                Style::default()
                    .fg(display_state_color(task.state, &task.display_state))
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "checklist",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        checklist_line("thinking", thinking),
        checklist_line("pushing", pushing),
        checklist_line("reviewing (skipped for now)", reviewing),
        checklist_line("restacking (if needed)", restacking),
        checklist_line("ready to merge", ready_to_merge),
    ]
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

    use super::{
        format_pane_tabs, format_task_row, pane_status_tag, status_sidebar_lines, to_local_time,
    };

    fn mk_row(task_id: &str) -> TaskOverviewRow {
        TaskOverviewRow {
            task_id: TaskId(task_id.to_string()),
            repo_id: RepoId("example".to_string()),
            branch: format!("task/{task_id}"),
            stack_position: None,
            state: TaskState::Running,
            display_state: "Running".to_string(),
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
        let tabs = format_pane_tabs(&app);
        let text: String = tabs.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.trim(), "none");

        app.state.panes = vec![
            AgentPane::new("A1", TaskId("T1".to_string()), ModelKind::Codex),
            AgentPane::new("A2", TaskId("T2".to_string()), ModelKind::Claude),
        ];
        app.state.panes[0].status = AgentPaneStatus::Running;
        app.state.panes[1].status = AgentPaneStatus::Waiting;
        app.state.selected_pane_idx = 1;

        let tabs = format_pane_tabs(&app);
        let text: String = tabs.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("1:A1"));
        assert!(text.contains(":running"));
        assert!(text.contains("2:A2"));
        assert!(text.contains(":waiting"));
    }

    #[test]
    fn format_task_row_includes_expected_columns() {
        let row = mk_row("T9");
        let line = format_task_row(true, &row);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let expected_ts = to_local_time(row.last_activity);

        assert!(text.contains("example"));
        assert!(text.contains("T9"));
        assert!(text.contains("task/T9"));
        assert!(text.contains("Running"));
        assert!(text.contains("not_run"));
        assert!(text.contains("0/0 unanimous=false cap=ok"));
        assert!(text.contains(&expected_ts));
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
        let tabs = format_pane_tabs(&app);
        let text: String = tabs.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("1:A1"));
        assert!(text.contains(":starting"));
    }

    #[test]
    fn status_sidebar_lines_compactly_reports_plan_and_status() {
        let mut row = mk_row("T1");
        row.state = TaskState::Running;
        row.display_state = "Running".to_string();

        let rendered = status_sidebar_lines(Some(&row));
        let text: Vec<String> = rendered
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert!(text.iter().any(|line| line.contains("plan complete: no")));
        assert!(text.iter().any(|line| line.contains("status: Running")));
        assert!(text.iter().any(|line| line.contains("[x] thinking")));
        assert!(text.iter().any(|line| line.contains("[x] pushing")));
        assert!(text
            .iter()
            .any(|line| line.contains("reviewing (skipped for now)")));
        assert!(text
            .iter()
            .any(|line| line.contains("restacking (if needed)")));
        assert!(text.iter().any(|line| line.contains("[ ] ready to merge")));
    }

    #[test]
    fn status_sidebar_lines_marks_ready_plan_complete() {
        let mut row = mk_row("T2");
        row.state = TaskState::Ready;
        row.display_state = "Ready".to_string();

        let rendered = status_sidebar_lines(Some(&row));
        let text: Vec<String> = rendered
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert!(text.iter().any(|line| line.contains("plan complete: yes")));
        assert!(text.iter().any(|line| line.contains("[x] ready to merge")));
    }
}
