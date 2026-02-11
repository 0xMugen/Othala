use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::TuiApp;
use crate::chat_parse;
use crate::chat_render;
use crate::output_style::stylize_output_lines;
#[cfg(test)]
use crate::ui_activity::status_activity;
#[cfg(test)]
use crate::ui_footer::wrapped_visual_line_count;
use crate::ui_footer::{build_footer_content, footer_height};
use crate::ui_format::{
    divider_line, format_pane_tabs, format_task_row, pane_meta_lines, status_sidebar_lines,
};
#[cfg(test)]
use crate::ui_format::{pane_status_tag, status_line_color, to_local_time};

// -- Color palette ----------------------------------------------------------

const ACCENT: Color = Color::Cyan;
const HEADER_FG: Color = Color::White;
const HEADER_TITLE: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const MUTED: Color = Color::Gray;
const BORDER_NORMAL: Color = Color::DarkGray;
const BORDER_FOCUSED: Color = Color::Cyan;

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
    let footer_height = footer_height(app, frame.area().width);
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(footer_height),
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
        " repo | task | title | state | verify | activity",
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
            lines.extend(stylize_output_lines(tail));
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
            lines.extend(stylize_output_lines(
                app.state.selected_task_activity.iter().cloned(),
            ));
        }
        (format!("Task Activity ({selected_task})"), lines)
    };

    let output = Paragraph::new(lines)
        .block(normal_block(&title))
        .wrap(Wrap { trim: false });
    frame.render_widget(output, panes[1]);
}

// -- Focused views ----------------------------------------------------------

fn extend_rendered_chat(lines: &mut Vec<Line<'static>>, window: &[String], width: u16) {
    if window.is_empty() {
        lines.push(Line::from(Span::styled(
            "no output yet",
            Style::default().fg(DIM),
        )));
        return;
    }
    let blocks = chat_parse::parse_chat_blocks(window);
    lines.extend(chat_render::render_chat_blocks(&blocks, width));
}

fn render_focused_pane(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let pane_idx = app.state.focused_pane_idx;
    let pane = pane_idx.and_then(|idx| app.state.panes.get(idx));

    let viewport_height = area.height.saturating_sub(2) as usize;
    let scroll_back = app.state.scroll_back;

    let (title, lines) = if let Some(pane) = pane {
        let mut lines = pane_meta_lines(pane, Some(scroll_back));
        lines.push(divider_line(area.width));
        let output_cap = viewport_height.saturating_sub(lines.len());
        let window = pane_idx
            .map(|idx| {
                app.state
                    .pane_window_with_history(idx, output_cap, scroll_back)
            })
            .unwrap_or_default();
        extend_rendered_chat(&mut lines, &window, area.width);
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

    // Find the agent pane for this task.
    let task_pane_idx = selected_task.and_then(|task| {
        app.state
            .panes
            .iter()
            .position(|p| p.task_id == task.task_id)
    });
    let task_pane = task_pane_idx.and_then(|idx| app.state.panes.get(idx));

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    let viewport_height = cols[1].height.saturating_sub(2) as usize;
    let scroll_back = app.state.scroll_back;

    // Left: task status checklist
    let status_title = format!("Status ({task_id_str})");
    let status_widget = Paragraph::new(status_sidebar_lines(
        selected_task,
        &app.state.selected_task_activity,
    ))
    .block(focused_block(&status_title))
    .wrap(Wrap { trim: false });
    frame.render_widget(status_widget, cols[0]);

    // Right: agent PTY output
    let (pty_title, pty_lines) = if let Some(pane) = task_pane {
        let mut lines = pane_meta_lines(pane, Some(scroll_back));
        lines.push(divider_line(cols[1].width));
        let output_cap = viewport_height.saturating_sub(lines.len());
        let window = task_pane_idx
            .map(|idx| {
                app.state
                    .pane_window_with_history(idx, output_cap, scroll_back)
            })
            .unwrap_or_default();
        extend_rendered_chat(&mut lines, &window, cols[1].width);
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
    let content = build_footer_content(app);
    let widget = Paragraph::new(content.lines)
        .block(normal_block(content.title))
        .wrap(Wrap {
            trim: content.wrap_trim,
        });
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::state::TaskState;
    use orch_core::types::{ModelKind, RepoId, TaskId};
    use ratatui::style::Color;

    use crate::model::{AgentPane, AgentPaneStatus, DashboardState, TaskOverviewRow};
    use crate::TuiApp;

    use super::{
        footer_height, format_pane_tabs, format_task_row, pane_status_tag, status_activity,
        status_line_color, status_sidebar_lines, to_local_time, wrapped_visual_line_count,
    };

    fn mk_row(task_id: &str) -> TaskOverviewRow {
        TaskOverviewRow {
            task_id: TaskId(task_id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Title for {task_id}"),
            branch: format!("task/{task_id}"),
            stack_position: None,
            state: TaskState::Chatting,
            display_state: "Chatting".to_string(),
            verify_summary: "not_run".to_string(),
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
        assert!(text.contains("Chatting"));
        assert!(text.contains("not_run"));
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
    fn wrapped_visual_line_count_handles_wrapping_and_newlines() {
        assert_eq!(wrapped_visual_line_count("", 10), 1);
        assert_eq!(wrapped_visual_line_count("abcd", 2), 2);
        assert_eq!(wrapped_visual_line_count("abcd\nef", 2), 3);
        assert_eq!(wrapped_visual_line_count("a\n\nb", 10), 3);
    }

    #[test]
    fn footer_height_expands_for_large_prompt_and_clamps() {
        use crate::app::InputMode;

        let mut app = TuiApp::default();
        assert_eq!(footer_height(&app, 120), 3);

        app.input_mode = InputMode::NewChatPrompt {
            buffer: "line 1\nline 2".to_string(),
        };
        assert!(footer_height(&app, 120) > 3);

        app.input_mode = InputMode::NewChatPrompt {
            buffer: "x".repeat(4000),
        };
        assert_eq!(footer_height(&app, 40), 12);
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
    fn status_activity_only_animates_live_pane_statuses() {
        let (running, running_color) =
            status_activity(AgentPaneStatus::Running, 2).expect("running activity");
        assert_eq!(running, "thinking ..o");
        assert_eq!(running_color, Color::Cyan);

        let (waiting, waiting_color) =
            status_activity(AgentPaneStatus::Waiting, 1).expect("waiting activity");
        assert_eq!(waiting, "percolating .o.");
        assert_eq!(waiting_color, Color::Magenta);

        assert!(status_activity(AgentPaneStatus::Exited, 0).is_none());
    }

    #[test]
    fn status_sidebar_lines_compactly_reports_plan_and_status() {
        let mut row = mk_row("T1");
        row.state = TaskState::Chatting;
        row.display_state = "Chatting".to_string();

        let rendered = status_sidebar_lines(Some(&row), &[]);
        let text: Vec<String> = rendered
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert!(text.iter().any(|line| line.contains("phase: chatting")));
        assert!(text.iter().any(|line| line.contains("status: Chatting")));
        assert!(text.iter().any(|line| line.contains("[~] chatting")));
        assert!(text.iter().any(|line| line.contains("[ ] verifying")));
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

        let rendered = status_sidebar_lines(Some(&row), &[]);
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

        let rendered = status_sidebar_lines(Some(&row), &[]);
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

    #[test]
    fn status_sidebar_shows_verifying_when_verify_in_progress() {
        let mut row = mk_row("T1");
        row.state = TaskState::Chatting;
        row.display_state = "Verifying".to_string();

        let rendered = status_sidebar_lines(Some(&row), &[]);
        let text: Vec<String> = rendered
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert!(text.iter().any(|line| line.contains("[~] verifying")));
        assert!(text.iter().any(|line| line.contains("phase: verifying")));
    }

    #[test]
    fn status_sidebar_shows_verify_failed_when_verify_fails() {
        let mut row = mk_row("T1");
        row.state = TaskState::Chatting;
        row.display_state = "VerifyFail".to_string();

        let rendered = status_sidebar_lines(Some(&row), &[]);
        let text: Vec<String> = rendered
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert!(text.iter().any(|line| line.contains("[!] verifying")));
        assert!(text
            .iter()
            .any(|line| line.contains("phase: verify failed")));
    }

    #[test]
    fn status_sidebar_shows_activity_lines_with_styling() {
        let row = mk_row("T1");
        let activity = vec![
            "gt submit --publish".to_string(),
            "error: push rejected".to_string(),
            "restack onto task/T0".to_string(),
            "some other log line".to_string(),
        ];

        let rendered = status_sidebar_lines(Some(&row), &activity);
        let text: Vec<String> = rendered
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        // Activity header present
        assert!(text.iter().any(|line| line == "activity"));
        // All activity entries present
        assert!(text.iter().any(|line| line.contains("gt submit --publish")));
        assert!(text
            .iter()
            .any(|line| line.contains("error: push rejected")));
        assert!(text
            .iter()
            .any(|line| line.contains("restack onto task/T0")));
        assert!(text.iter().any(|line| line.contains("some other log line")));

        // Verify color styling on activity lines
        for line in &rendered {
            for span in &line.spans {
                let content = span.content.as_ref();
                if content.contains("error") && content.contains("push rejected") {
                    assert_eq!(span.style.fg, Some(Color::Red));
                } else if content.contains("gt submit") {
                    assert_eq!(span.style.fg, Some(Color::Yellow));
                } else if content.contains("restack onto") {
                    assert_eq!(span.style.fg, Some(Color::Cyan));
                } else if content == "some other log line" {
                    assert_eq!(span.style.fg, Some(Color::DarkGray));
                }
            }
        }
    }

    #[test]
    fn status_sidebar_limits_activity_to_last_10() {
        let row = mk_row("T1");
        let activity: Vec<String> = (0..15).map(|i| format!("log line {i}")).collect();

        let rendered = status_sidebar_lines(Some(&row), &activity);
        let text: Vec<String> = rendered
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        // Should NOT contain the first 5 entries
        assert!(!text.iter().any(|line| line.contains("log line 0")));
        assert!(!text.iter().any(|line| line.contains("log line 4")));
        // Should contain the last 10 entries
        assert!(text.iter().any(|line| line.contains("log line 5")));
        assert!(text.iter().any(|line| line.contains("log line 14")));
    }
}
