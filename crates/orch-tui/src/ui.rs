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
const OUTPUT_FG: Color = Color::White;

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

fn status_line_color(message: &str) -> Color {
    let lower = message.to_ascii_lowercase();
    if lower.contains("[needs_human]") || lower.contains("needs_human") {
        Color::Yellow
    } else if lower.contains("[patch_ready]") || lower.contains("patch ready") {
        Color::Green
    } else if lower.contains("error") || lower.contains("failed") || lower.contains("not found") {
        Color::Red
    } else if lower.contains("ready") || lower.contains("updated") || lower.contains("queued") {
        ACCENT
    } else {
        MUTED
    }
}

fn output_line_style(line: &str) -> Style {
    let lower = line.to_ascii_lowercase();
    if line.trim().is_empty() {
        Style::default().fg(DIM)
    } else if lower.contains("[needs_human]") || lower.contains("needs_human") {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if lower.contains("[patch_ready]") || lower.contains("patch ready") {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else if lower.contains("error") || lower.contains("failed") {
        Style::default().fg(Color::Red)
    } else if line.starts_with("## ") {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(OUTPUT_FG)
    }
}

fn stylize_output_line(line: String) -> Line<'static> {
    let style = output_line_style(&line);
    Line::from(Span::styled(line, style))
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
        " repo | task | title | state | verify | review | activity",
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
        let mut lines = pane_meta_lines(pane, None);
        lines.push(divider_line(panes[1].width));
        let tail = pane.tail(20);
        if tail.is_empty() {
            lines.push(Line::from(Span::styled(
                "no output yet",
                Style::default().fg(DIM),
            )));
        } else {
            lines.extend(tail.into_iter().map(stylize_output_line));
        }
        (format!("Chat {}", pane.instance_id), lines)
    } else {
        let selected_task = app
            .state
            .selected_task()
            .map(|task| task.task_id.0.clone())
            .unwrap_or_else(|| "-".to_string());
        let mut lines = vec![
            Line::from(vec![
                Span::styled(" task ", Style::default().fg(DIM)),
                Span::styled(selected_task.clone(), Style::default().fg(ACCENT)),
                Span::styled("  source ", Style::default().fg(DIM)),
                Span::styled("activity log", Style::default().fg(MUTED)),
                Span::styled("  lines ", Style::default().fg(DIM)),
                Span::styled(
                    app.state.selected_task_activity.len().to_string(),
                    Style::default().fg(HEADER_FG),
                ),
            ]),
            divider_line(panes[1].width),
        ];
        if app.state.selected_task_activity.is_empty() {
            lines.push(Line::from(Span::styled(
                "no task activity yet",
                Style::default().fg(DIM),
            )));
        } else {
            lines.extend(
                app.state
                    .selected_task_activity
                    .iter()
                    .cloned()
                    .map(stylize_output_line),
            );
        }
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
        let mut lines = pane_meta_lines(pane, Some(scroll_back));
        lines.push(divider_line(area.width));
        let output_cap = viewport_height.saturating_sub(lines.len());
        let window = pane.window(output_cap, scroll_back);
        if window.is_empty() {
            lines.push(Line::from(Span::styled(
                "no output yet",
                Style::default().fg(DIM),
            )));
        } else {
            lines.extend(window.into_iter().map(stylize_output_line));
        }
        (format!("Focused Chat {}", pane.instance_id), lines)
    } else {
        (
            "Focused Chat".to_string(),
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
        let mut lines = pane_meta_lines(pane, Some(scroll_back));
        lines.push(divider_line(cols[1].width));
        let output_cap = viewport_height.saturating_sub(lines.len());
        let window = pane.window(output_cap, scroll_back);
        if window.is_empty() {
            lines.push(Line::from(Span::styled(
                "no output yet",
                Style::default().fg(DIM),
            )));
        } else {
            lines.extend(window.into_iter().map(stylize_output_line));
        }
        (format!("Agent {}", pane.instance_id), lines)
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
            spans.push(Span::styled(" | status: ", Style::default().fg(DIM)));
            spans.push(Span::styled(
                app.state.status_line.as_str(),
                Style::default()
                    .fg(status_line_color(&app.state.status_line))
                    .add_modifier(Modifier::BOLD),
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
        Span::styled(&task.title, base_style),
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
            spans.push(Span::raw("  "));
        }
        let is_selected = idx == app.state.selected_pane_idx;
        let tag = pane_status_tag(pane);
        let sc = pane_status_color(pane.status);
        let base_style = if is_selected {
            Style::default()
                .fg(Color::White)
                .bg(SELECTED_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED)
        };
        let meta_style = if is_selected {
            Style::default().fg(DIM).bg(SELECTED_BG)
        } else {
            Style::default().fg(DIM)
        };
        let status_style = if is_selected {
            Style::default()
                .fg(sc)
                .bg(SELECTED_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(sc).add_modifier(Modifier::BOLD)
        };

        spans.push(Span::styled(
            format!(" {}:{} ", idx + 1, pane.instance_id),
            base_style,
        ));
        spans.push(Span::styled(format!("{tag} "), status_style));
        spans.push(Span::styled(format!("{}l ", pane.lines.len()), meta_style));
    }
    Line::from(spans)
}

fn pane_meta_lines(pane: &AgentPane, scroll_back: Option<usize>) -> Vec<Line<'static>> {
    let status = pane_status_tag(pane);
    let updated = pane.updated_at.with_timezone(&Local).format("%H:%M:%S");
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" status ", Style::default().fg(DIM)),
            Span::styled(
                status.to_string(),
                Style::default()
                    .fg(pane_status_color(pane.status))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  model ", Style::default().fg(DIM)),
            Span::styled(format!("{:?}", pane.model), Style::default().fg(HEADER_FG)),
        ]),
        Line::from(vec![
            Span::styled(" task ", Style::default().fg(DIM)),
            Span::styled(pane.task_id.0.clone(), Style::default().fg(ACCENT)),
            Span::styled("  lines ", Style::default().fg(DIM)),
            Span::styled(pane.lines.len().to_string(), Style::default().fg(HEADER_FG)),
            Span::styled("  updated ", Style::default().fg(DIM)),
            Span::styled(updated.to_string(), Style::default().fg(MUTED)),
        ]),
    ];
    if let Some(scroll_back) = scroll_back {
        if scroll_back > 0 {
            lines.push(Line::from(vec![
                Span::styled(" scroll ", Style::default().fg(DIM)),
                Span::styled(
                    format!("+{scroll_back} lines from live tail"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }
    lines
}

fn divider_line(width: u16) -> Line<'static> {
    let len = width.saturating_sub(4).max(8) as usize;
    Line::from(Span::styled("-".repeat(len), Style::default().fg(DIM)))
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

    // -- coding: Active during initial work, Done once past Running --
    let coding = match task.state {
        TaskState::Failed => ChecklistState::Blocked,
        TaskState::Queued | TaskState::Paused => ChecklistState::Pending,
        TaskState::Initializing | TaskState::DraftPrOpen => ChecklistState::Active,
        TaskState::Running if task.display_state == "Running" => ChecklistState::Active,
        _ => ChecklistState::Done,
    };

    // -- verifying: tracks verify lifecycle via display_state --
    let verifying = if task.display_state == "VerifyFail" {
        ChecklistState::Blocked
    } else if matches!(
        task.state,
        TaskState::VerifyingQuick | TaskState::VerifyingFull
    ) || task.display_state == "Verifying"
    {
        ChecklistState::Active
    } else if task.display_state == "Verified"
        || matches!(
            task.state,
            TaskState::Reviewing
                | TaskState::NeedsHuman
                | TaskState::Ready
                | TaskState::Submitting
                | TaskState::AwaitingMerge
                | TaskState::Merged
        )
    {
        ChecklistState::Done
    } else {
        ChecklistState::Pending
    };

    // -- reviewing: tracks code-review phase --
    let reviewing = match task.state {
        TaskState::Reviewing => ChecklistState::Active,
        TaskState::NeedsHuman => ChecklistState::Blocked,
        TaskState::Ready | TaskState::Submitting | TaskState::AwaitingMerge | TaskState::Merged => {
            ChecklistState::Done
        }
        _ => ChecklistState::Pending,
    };

    // -- restacking: optional, Skipped unless entered --
    let restacking = match task.state {
        TaskState::Restacking => ChecklistState::Active,
        TaskState::RestackConflict => ChecklistState::Blocked,
        _ => ChecklistState::Skipped,
    };

    // -- pushing: Active during submit, Done once past Submitting --
    let pushing = match task.state {
        TaskState::Submitting => ChecklistState::Active,
        TaskState::AwaitingMerge | TaskState::Merged => ChecklistState::Done,
        TaskState::Failed if matches!(task.display_state.as_str(), "Submitting") => {
            ChecklistState::Blocked
        }
        _ => ChecklistState::Pending,
    };

    // -- merging: Active when awaiting merge, Done when merged --
    let merging = match task.state {
        TaskState::AwaitingMerge => ChecklistState::Active,
        TaskState::Merged => ChecklistState::Done,
        _ => ChecklistState::Pending,
    };

    // -- plan complete (only at Merged) / current phase label --
    let (plan_label, plan_value, plan_color) = if task.state == TaskState::Merged {
        ("plan complete: ", "yes", Color::Green)
    } else {
        let phase = match task.state {
            TaskState::Queued => "queued",
            TaskState::Paused => "paused",
            TaskState::Initializing | TaskState::DraftPrOpen => "coding",
            TaskState::Running => match task.display_state.as_str() {
                "Verifying" => "verifying",
                "VerifyFail" => "verify failed",
                "Verified" => "verified",
                _ => "coding",
            },
            TaskState::VerifyingQuick | TaskState::VerifyingFull => "verifying",
            TaskState::Reviewing => "reviewing",
            TaskState::NeedsHuman => "needs human",
            TaskState::Ready => "ready",
            TaskState::Submitting => "pushing",
            TaskState::AwaitingMerge => "awaiting merge",
            TaskState::Restacking => "restacking",
            TaskState::RestackConflict => "restack conflict",
            TaskState::Failed => "failed",
            TaskState::Merged => unreachable!(),
        };
        ("phase: ", phase, Color::Yellow)
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(plan_label, Style::default().fg(DIM)),
            Span::styled(
                plan_value.to_string(),
                Style::default().fg(plan_color).add_modifier(Modifier::BOLD),
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
        checklist_line("coding", coding),
        checklist_line("verifying", verifying),
        checklist_line("reviewing", reviewing),
        checklist_line("restacking (if needed)", restacking),
        checklist_line("pushing", pushing),
        checklist_line("merging", merging),
    ];

    // -- verify / review / push detail lines --
    if task.verify_summary != "not_run" {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("verify: ", Style::default().fg(DIM)),
            Span::styled(
                task.verify_summary.clone(),
                Style::default().fg(Color::White),
            ),
        ]));
    }
    if task.review_summary != "0/0 unanimous=false cap=ok" {
        if task.verify_summary == "not_run" {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(vec![
            Span::styled("review: ", Style::default().fg(DIM)),
            Span::styled(
                task.review_summary.clone(),
                Style::default().fg(Color::White),
            ),
        ]));
    }

    // -- push detail --
    let push_detail = match task.state {
        TaskState::Submitting => Some("gt submit in progress..."),
        TaskState::AwaitingMerge => Some("pr submitted, awaiting merge"),
        TaskState::Merged => Some("merged"),
        _ => None,
    };
    if let Some(detail) = push_detail {
        lines.push(Line::from(vec![
            Span::styled("push: ", Style::default().fg(DIM)),
            Span::styled(detail.to_string(), Style::default().fg(Color::White)),
        ]));
    }

    lines
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
    use ratatui::style::Color;

    use crate::model::{AgentPane, AgentPaneStatus, DashboardState, TaskOverviewRow};
    use crate::TuiApp;

    use super::{
        format_pane_tabs, format_task_row, output_line_style, pane_status_tag, status_line_color,
        status_sidebar_lines, to_local_time,
    };

    fn mk_row(task_id: &str) -> TaskOverviewRow {
        TaskOverviewRow {
            task_id: TaskId(task_id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Title for {task_id}"),
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
        assert!(text.contains("running"));
        assert!(text.contains("2:A2"));
        assert!(text.contains("waiting"));
        assert!(text.contains("0l"));
    }

    #[test]
    fn format_task_row_includes_expected_columns() {
        let row = mk_row("T9");
        let line = format_task_row(true, &row);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let expected_ts = to_local_time(row.last_activity);

        assert!(text.contains("example"));
        assert!(text.contains("T9"));
        assert!(text.contains("Title for T9"));
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
        assert!(text.contains("starting"));
    }

    #[test]
    fn status_line_color_highlights_attention_levels() {
        assert_eq!(status_line_color("failed to submit"), Color::Red);
        assert_eq!(status_line_color("[needs_human] waiting"), Color::Yellow);
        assert_eq!(status_line_color("[patch_ready]"), Color::Green);
        assert_eq!(status_line_color("pane updated: A1"), Color::Cyan);
    }

    #[test]
    fn output_line_style_marks_special_chat_signals() {
        assert_eq!(
            output_line_style("[needs_human] unblock me").fg,
            Some(Color::Yellow)
        );
        assert_eq!(
            output_line_style("[patch_ready] complete").fg,
            Some(Color::Green)
        );
        assert_eq!(output_line_style("fatal error").fg, Some(Color::Red));
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

        assert!(text.iter().any(|line| line.contains("phase: coding")));
        assert!(text.iter().any(|line| line.contains("status: Running")));
        assert!(text.iter().any(|line| line.contains("[~] coding")));
        assert!(text.iter().any(|line| line.contains("[ ] verifying")));
        assert!(text.iter().any(|line| line.contains("[ ] reviewing")));
        assert!(text
            .iter()
            .any(|line| line.contains("restacking (if needed)")));
        assert!(text.iter().any(|line| line.contains("[ ] pushing")));
        assert!(text.iter().any(|line| line.contains("[ ] merging")));
    }

    #[test]
    fn status_sidebar_lines_marks_ready_plan_complete() {
        let mut row = mk_row("T2");
        row.state = TaskState::Merged;
        row.display_state = "Merged".to_string();

        let rendered = status_sidebar_lines(Some(&row));
        let text: Vec<String> = rendered
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert!(text.iter().any(|line| line.contains("plan complete: yes")));
        assert!(text.iter().any(|line| line.contains("[x] pushing")));
        assert!(text.iter().any(|line| line.contains("[x] merging")));
    }

    #[test]
    fn status_sidebar_lines_shows_push_detail_when_submitting() {
        let mut row = mk_row("T3");
        row.state = TaskState::Submitting;
        row.display_state = "Submitting".to_string();

        let rendered = status_sidebar_lines(Some(&row));
        let text: Vec<String> = rendered
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert!(text
            .iter()
            .any(|line| line.contains("push: gt submit in progress...")));
        assert!(text.iter().any(|line| line.contains("[~] pushing")));
        assert!(text.iter().any(|line| line.contains("[ ] merging")));
    }
}
