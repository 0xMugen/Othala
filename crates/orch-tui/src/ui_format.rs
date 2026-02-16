use chrono::{DateTime, Local, Utc};
use orch_core::state::TaskState;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::TuiApp;
use crate::model::{
    AgentPane, AgentPaneStatus, PaneCategory, QATestDisplay, TaskOverviewRow, TuiTheme,
};

pub fn state_color(state: TaskState, theme: &TuiTheme) -> Color {
    match state {
        TaskState::Chatting => theme.state_chatting,
        TaskState::Ready => theme.state_ready,
        TaskState::Submitting => theme.state_submitting,
        TaskState::Restacking => theme.state_restacking,
        TaskState::AwaitingMerge => theme.state_awaiting,
        TaskState::Merged => theme.state_merged,
        TaskState::Stopped => theme.state_stopped,
    }
}

/// Pick a color for the composite display state label. Falls back to `state_color`
/// for states that are not overridden by verify status.
pub(crate) fn display_state_color(state: TaskState, display_state: &str, theme: &TuiTheme) -> Color {
    match display_state {
        "VerifyFail" | "SubmitFail" | "QAFail" => Color::Red,
        "Verified" | "QAPassed" => Color::Cyan,
        "Verifying" | "QARunning" => Color::Yellow,
        _ => state_color(state, theme),
    }
}

pub(crate) fn pane_status_color(status: AgentPaneStatus) -> Color {
    match status {
        AgentPaneStatus::Starting => Color::Yellow,
        AgentPaneStatus::Running => Color::Green,
        AgentPaneStatus::Waiting => Color::Magenta,
        AgentPaneStatus::Exited => Color::DarkGray,
        AgentPaneStatus::Failed => Color::Red,
        AgentPaneStatus::Stopped => Color::Cyan,
    }
}

pub(crate) fn status_line_color(message: &str) -> Color {
    let lower = message.to_ascii_lowercase();
    if lower.contains("[needs_human]") || lower.contains("needs_human") {
        Color::Yellow
    } else if lower.contains("[patch_ready]") || lower.contains("patch ready") {
        Color::Green
    } else if lower.contains("[qa_complete]") || lower.contains("qa_complete") {
        Color::Cyan
    } else if lower.contains("error") || lower.contains("failed") || lower.contains("not found") {
        Color::Red
    } else if lower.contains("ready") || lower.contains("updated") || lower.contains("queued") {
        Color::Cyan
    } else {
        Color::Gray
    }
}

pub(crate) fn format_task_row<'a>(
    is_selected: bool,
    task: &'a TaskOverviewRow,
    cost_display: String,
    state_style: Style,
    theme: &TuiTheme,
) -> Line<'a> {
    let ts = to_local_time(task.last_activity);

    let base_style = if is_selected {
        Style::default().bg(theme.selected_bg).fg(Color::White)
    } else {
        Style::default().fg(theme.muted)
    };

    let prefix = if is_selected { "\u{25B6} " } else { "  " };
    let mut state_cell_style = state_style.add_modifier(Modifier::BOLD);
    if is_selected {
        state_cell_style = state_cell_style.bg(theme.selected_bg);
    }
    let state_label = format!("{:?}", task.state);

    Line::from(vec![
        Span::styled(
            prefix,
            if is_selected {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.dim)
            },
        ),
        Span::styled(&task.repo_id.0, base_style),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(
            &task.task_id.0,
            if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
                    .bg(theme.selected_bg)
            } else {
                Style::default().fg(Color::White)
            },
        ),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(&task.title, base_style),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(state_label, state_cell_style),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(&task.verify_summary, base_style),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(cost_display, base_style),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(ts, Style::default().fg(theme.dim)),
    ])
}

#[allow(dead_code)] // Used in ui.rs tests
pub(crate) fn format_pane_tabs(app: &TuiApp) -> Line<'static> {
    let theme = &app.state.current_theme;
    if app.state.panes.is_empty() {
        return Line::from(Span::styled(" none", Style::default().fg(theme.dim)));
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
                .bg(theme.selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        let meta_style = if is_selected {
            Style::default().fg(theme.dim).bg(theme.selected_bg)
        } else {
            Style::default().fg(theme.dim)
        };
        let status_style = if is_selected {
            Style::default()
                .fg(sc)
                .bg(theme.selected_bg)
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

pub(crate) fn format_category_tabs(app: &TuiApp) -> Line<'static> {
    let theme = &app.state.current_theme;
    let selected = app.state.selected_pane_category;
    let task_id = app.state.selected_task().map(|t| &t.task_id);

    let has_agent = task_id
        .is_some_and(|tid| app.state.has_pane_in_category(tid, PaneCategory::Agent));
    let has_qa = task_id
        .is_some_and(|tid| app.state.has_pane_in_category(tid, PaneCategory::QA));

    let sel_style = Style::default()
        .fg(Color::White)
        .bg(theme.selected_bg)
        .add_modifier(Modifier::BOLD);
    let active_style = Style::default().fg(theme.muted);
    let dim_style = Style::default().fg(theme.dim);

    let mut spans = Vec::new();
    spans.push(Span::raw(" "));

    // Agent tab
    let agent_sel = selected == PaneCategory::Agent;
    if agent_sel {
        spans.push(Span::styled("\u{25B8} ", Style::default().fg(theme.accent)));
        spans.push(Span::styled(" Agent ", sel_style));
    } else if has_agent {
        spans.push(Span::styled("  Agent ", active_style));
    } else {
        spans.push(Span::styled("  Agent ", dim_style));
    }

    spans.push(Span::styled(" \u{2502} ", Style::default().fg(theme.dim)));

    // QA tab
    let qa_sel = selected == PaneCategory::QA;
    if qa_sel {
        spans.push(Span::styled("\u{25B8} ", Style::default().fg(theme.accent)));
        spans.push(Span::styled(" QA ", sel_style));
    } else if has_qa {
        spans.push(Span::styled("  QA ", active_style));
    } else {
        spans.push(Span::styled("  QA ", dim_style));
    }

    // Hint
    spans.push(Span::styled(
        "  \u{2190}\u{2192} switch",
        Style::default().fg(theme.dim),
    ));

    Line::from(spans)
}

pub(crate) fn pane_meta_lines(
    pane: &AgentPane,
    scroll_back: Option<usize>,
    theme: &TuiTheme,
) -> Vec<Line<'static>> {
    let status = pane_status_tag(pane);
    let updated = pane.updated_at.with_timezone(&Local).format("%H:%M:%S");
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" status ", Style::default().fg(theme.dim)),
            Span::styled(
                status.to_string(),
                Style::default()
                    .fg(pane_status_color(pane.status))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  model ", Style::default().fg(theme.dim)),
            Span::styled(format!("{:?}", pane.model), Style::default().fg(theme.header_fg)),
        ]),
        Line::from(vec![
            Span::styled(" task ", Style::default().fg(theme.dim)),
            Span::styled(pane.task_id.0.clone(), Style::default().fg(theme.accent)),
            Span::styled("  lines ", Style::default().fg(theme.dim)),
            Span::styled(
                pane.lines.len().to_string(),
                Style::default().fg(theme.header_fg),
            ),
            Span::styled("  updated ", Style::default().fg(theme.dim)),
            Span::styled(updated.to_string(), Style::default().fg(theme.muted)),
        ]),
    ];
    if let Some(scroll_back) = scroll_back {
        if scroll_back > 0 {
            lines.push(Line::from(vec![
                Span::styled(" scroll ", Style::default().fg(theme.dim)),
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

pub(crate) fn divider_line(width: u16, theme: &TuiTheme) -> Line<'static> {
    let len = width.saturating_sub(4).max(8) as usize;
    Line::from(Span::styled("-".repeat(len), Style::default().fg(theme.dim)))
}

pub(crate) fn pane_status_tag(pane: &AgentPane) -> &'static str {
    match pane.status {
        AgentPaneStatus::Starting => "starting",
        AgentPaneStatus::Running => "running",
        AgentPaneStatus::Waiting => "waiting",
        AgentPaneStatus::Exited => "exited",
        AgentPaneStatus::Failed => "failed",
        AgentPaneStatus::Stopped => "stopped",
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

fn checklist_line(label: &str, state: ChecklistState, theme: &TuiTheme) -> Line<'static> {
    let (marker, marker_color, label_color) = match state {
        ChecklistState::Done => ("x", Color::Green, Color::White),
        ChecklistState::Pending => (" ", theme.dim, theme.muted),
        ChecklistState::Skipped => ("-", Color::Yellow, theme.dim),
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

pub(crate) fn status_sidebar_lines(
    task: Option<&TaskOverviewRow>,
    activity: &[String],
    theme: &TuiTheme,
) -> Vec<Line<'static>> {
    let Some(task) = task else {
        return vec![Line::from(Span::styled(
            "no task selected",
            Style::default().fg(theme.dim),
        ))];
    };

    let chatting = match task.state {
        TaskState::Chatting => ChecklistState::Active,
        TaskState::Stopped => ChecklistState::Blocked,
        _ => ChecklistState::Done,
    };

    let verifying = if task.display_state == "VerifyFail" {
        ChecklistState::Blocked
    } else if task.display_state == "Verifying" {
        ChecklistState::Active
    } else if matches!(
        task.state,
        TaskState::Ready | TaskState::Submitting | TaskState::AwaitingMerge | TaskState::Merged
    ) {
        ChecklistState::Done
    } else {
        ChecklistState::Pending
    };

    let restacking = match task.state {
        TaskState::Restacking => ChecklistState::Active,
        _ => ChecklistState::Skipped,
    };

    let pushing = match task.state {
        TaskState::Submitting => ChecklistState::Active,
        TaskState::AwaitingMerge | TaskState::Merged => ChecklistState::Done,
        _ => ChecklistState::Pending,
    };

    let merging = match task.state {
        TaskState::AwaitingMerge => ChecklistState::Active,
        TaskState::Merged => ChecklistState::Done,
        _ => ChecklistState::Pending,
    };

    let (plan_label, plan_value, plan_color) = if task.state == TaskState::Merged {
        ("plan complete: ", "yes", Color::Green)
    } else if task.state == TaskState::Stopped {
        ("phase: ", "stopped", Color::Red)
    } else {
        let phase = match task.state {
            TaskState::Chatting => match task.display_state.as_str() {
                "Verifying" => "verifying",
                "VerifyFail" => "verify failed",
                "Verified" => "verified",
                _ => "chatting",
            },
            TaskState::Ready => "ready",
            TaskState::Submitting => "pushing",
            TaskState::Restacking => "restacking",
            TaskState::AwaitingMerge => "awaiting merge",
            TaskState::Merged | TaskState::Stopped => unreachable!(),
        };
        ("phase: ", phase, Color::Yellow)
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(plan_label, Style::default().fg(theme.dim)),
            Span::styled(
                plan_value.to_string(),
                Style::default().fg(plan_color).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("status: ", Style::default().fg(theme.dim)),
            Span::styled(
                task.display_state.clone(),
                Style::default()
                    .fg(display_state_color(task.state, &task.display_state, theme))
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "checklist",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )),
        checklist_line("chatting", chatting, theme),
        checklist_line("verifying", verifying, theme),
        checklist_line("restacking (if needed)", restacking, theme),
        checklist_line("pushing", pushing, theme),
        checklist_line("merging", merging, theme),
    ];

    if task.verify_summary != "not_run" {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("verify: ", Style::default().fg(theme.dim)),
            Span::styled(
                task.verify_summary.clone(),
                Style::default().fg(Color::White),
            ),
        ]));
    }

    let push_detail = match task.state {
        TaskState::Submitting => Some("gt submit in progress..."),
        TaskState::AwaitingMerge => Some("pr submitted, awaiting merge"),
        TaskState::Merged => Some("merged"),
        _ => None,
    };
    if let Some(detail) = push_detail {
        lines.push(Line::from(vec![
            Span::styled("push: ", Style::default().fg(theme.dim)),
            Span::styled(detail.to_string(), Style::default().fg(Color::White)),
        ]));
    }

    // QA section
    if task.qa_status.is_some() || !task.qa_tests.is_empty() || !task.qa_targets.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "qa",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));

        // QA status line
        if let Some(ref status) = task.qa_status {
            let (qa_marker, qa_color) = qa_status_style(status, theme);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("[{qa_marker}] "),
                    Style::default().fg(qa_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(status.clone(), Style::default().fg(Color::White)),
            ]));
        }

        // QA test results grouped by suite
        if !task.qa_tests.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "qa tests",
                Style::default().fg(theme.muted).add_modifier(Modifier::BOLD),
            )));
            lines.extend(qa_test_lines(&task.qa_tests, theme));
        }

        // Task-specific acceptance targets
        if !task.qa_targets.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "qa targets",
                Style::default().fg(theme.muted).add_modifier(Modifier::BOLD),
            )));
            for target in &task.qa_targets {
                lines.push(Line::from(vec![
                    Span::styled("  - ", Style::default().fg(theme.dim)),
                    Span::styled(target.clone(), Style::default().fg(Color::White)),
                ]));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Retry History:",
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
    )));
    if task.retry_history.is_empty() {
        lines.push(Line::from(Span::styled(
            "No retries",
            Style::default().fg(theme.dim),
        )));
    } else {
        for entry in &task.retry_history {
            let reason = entry
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("success");
            let timestamp = format_retry_timestamp(&entry.timestamp);
            lines.push(Line::from(Span::styled(
                format!(
                    "#{} {:<7} — {} ({})",
                    entry.attempt, entry.model, reason, timestamp
                ),
                Style::default().fg(Color::White),
            )));
        }
    }

    if !activity.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "activity",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));
        let tail = if activity.len() > 10 {
            &activity[activity.len() - 10..]
        } else {
            activity
        };
        for entry in tail {
            let lower = entry.to_lowercase();
            let color = if lower.contains("error") {
                Color::Red
            } else if lower.contains("submit") {
                Color::Yellow
            } else if lower.contains("restack") {
                Color::Cyan
            } else {
                theme.dim
            };
            lines.push(Line::from(Span::styled(
                entry.clone(),
                Style::default().fg(color),
            )));
        }
    }

    lines
}

/// Pick marker and color for QA status display.
fn qa_status_style(status: &str, theme: &TuiTheme) -> (&'static str, Color) {
    let lower = status.to_ascii_lowercase();
    if lower.contains("running") {
        ("~", Color::Cyan)
    } else if lower.contains("passed") && !lower.contains("failed") {
        ("x", Color::Green)
    } else if lower.contains("failed") {
        ("!", Color::Red)
    } else {
        (" ", theme.muted)
    }
}

/// Render QA test results grouped by suite.
fn qa_test_lines(tests: &[QATestDisplay], theme: &TuiTheme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut current_suite = String::new();

    for test in tests {
        if test.suite != current_suite {
            current_suite = test.suite.clone();
            lines.push(Line::from(Span::styled(
                format!("  {}", current_suite),
                Style::default().fg(theme.muted),
            )));
        }

        let (marker, color) = if test.passed {
            ("x", Color::Green)
        } else {
            ("!", Color::Red)
        };

        let mut spans = vec![
            Span::styled(
                format!("  [{marker}] "),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(test.name.clone(), Style::default().fg(Color::White)),
        ];

        if !test.passed && !test.detail.is_empty() {
            spans.push(Span::styled(
                format!("  {}", test.detail),
                Style::default().fg(Color::Red),
            ));
        }

        lines.push(Line::from(spans));
    }

    lines
}

fn format_retry_timestamp(timestamp: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| dt.with_timezone(&Local).format("%H:%M:%S").to_string())
        .unwrap_or_else(|_| timestamp.to_string())
}

pub(crate) fn to_local_time(value: DateTime<Utc>) -> String {
    value
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::state_color;
    use crate::model::default_theme;
    use orch_core::state::TaskState;
    use ratatui::style::Color;

    #[test]
    fn state_color_returns_correct_colors() {
        let theme = default_theme();
        assert_eq!(state_color(TaskState::Chatting, &theme), Color::Yellow);
        assert_eq!(state_color(TaskState::Ready, &theme), Color::Blue);
        assert_eq!(state_color(TaskState::Submitting, &theme), Color::Cyan);
        assert_eq!(state_color(TaskState::Restacking, &theme), Color::Magenta);
        assert_eq!(state_color(TaskState::AwaitingMerge, &theme), Color::LightBlue);
        assert_eq!(state_color(TaskState::Merged, &theme), Color::Green);
        assert_eq!(state_color(TaskState::Stopped, &theme), Color::Red);
    }
}
