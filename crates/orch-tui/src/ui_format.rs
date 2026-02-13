use chrono::{DateTime, Local, Utc};
use orch_core::state::TaskState;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::TuiApp;
use crate::model::{AgentPane, AgentPaneStatus, PaneCategory, QATestDisplay, TaskOverviewRow};

const ACCENT: Color = Color::Cyan;
const HEADER_FG: Color = Color::White;
const DIM: Color = Color::DarkGray;
const MUTED: Color = Color::Gray;
const SELECTED_BG: Color = Color::Indexed(236);

fn state_color(state: TaskState) -> Color {
    match state {
        TaskState::Chatting => Color::Green,
        TaskState::Ready | TaskState::Merged => Color::Cyan,
        TaskState::Submitting | TaskState::AwaitingMerge => Color::Blue,
        TaskState::Restacking => Color::Yellow,
        TaskState::Stopped => Color::Red,
    }
}

/// Pick a color for the composite display state label. Falls back to `state_color`
/// for states that are not overridden by verify status.
pub(crate) fn display_state_color(state: TaskState, display_state: &str) -> Color {
    match display_state {
        "VerifyFail" | "SubmitFail" | "QAFail" => Color::Red,
        "Verified" | "QAPassed" => Color::Cyan,
        "Verifying" | "QARunning" => Color::Yellow,
        _ => state_color(state),
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
        ACCENT
    } else if lower.contains("error") || lower.contains("failed") || lower.contains("not found") {
        Color::Red
    } else if lower.contains("ready") || lower.contains("updated") || lower.contains("queued") {
        ACCENT
    } else {
        MUTED
    }
}

pub(crate) fn format_task_row<'a>(is_selected: bool, task: &'a TaskOverviewRow) -> Line<'a> {
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
        Span::styled(ts, Style::default().fg(DIM)),
    ])
}

#[allow(dead_code)] // Used in ui.rs tests
pub(crate) fn format_pane_tabs(app: &TuiApp) -> Line<'static> {
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

pub(crate) fn format_category_tabs(app: &TuiApp) -> Line<'static> {
    let selected = app.state.selected_pane_category;
    let task_id = app.state.selected_task().map(|t| &t.task_id);

    let has_agent = task_id
        .map_or(false, |tid| app.state.has_pane_in_category(tid, PaneCategory::Agent));
    let has_qa = task_id
        .map_or(false, |tid| app.state.has_pane_in_category(tid, PaneCategory::QA));

    let sel_style = Style::default()
        .fg(Color::White)
        .bg(SELECTED_BG)
        .add_modifier(Modifier::BOLD);
    let active_style = Style::default().fg(MUTED);
    let dim_style = Style::default().fg(DIM);

    let mut spans = Vec::new();
    spans.push(Span::raw(" "));

    // Agent tab
    let agent_sel = selected == PaneCategory::Agent;
    if agent_sel {
        spans.push(Span::styled("\u{25B8} ", Style::default().fg(ACCENT)));
        spans.push(Span::styled(" Agent ", sel_style));
    } else if has_agent {
        spans.push(Span::styled("  Agent ", active_style));
    } else {
        spans.push(Span::styled("  Agent ", dim_style));
    }

    spans.push(Span::styled(" \u{2502} ", Style::default().fg(DIM)));

    // QA tab
    let qa_sel = selected == PaneCategory::QA;
    if qa_sel {
        spans.push(Span::styled("\u{25B8} ", Style::default().fg(ACCENT)));
        spans.push(Span::styled(" QA ", sel_style));
    } else if has_qa {
        spans.push(Span::styled("  QA ", active_style));
    } else {
        spans.push(Span::styled("  QA ", dim_style));
    }

    // Hint
    spans.push(Span::styled("  \u{2190}\u{2192} switch", Style::default().fg(DIM)));

    Line::from(spans)
}

pub(crate) fn pane_meta_lines(pane: &AgentPane, scroll_back: Option<usize>) -> Vec<Line<'static>> {
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

pub(crate) fn divider_line(width: u16) -> Line<'static> {
    let len = width.saturating_sub(4).max(8) as usize;
    Line::from(Span::styled("-".repeat(len), Style::default().fg(DIM)))
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

pub(crate) fn status_sidebar_lines(
    task: Option<&TaskOverviewRow>,
    activity: &[String],
) -> Vec<Line<'static>> {
    let Some(task) = task else {
        return vec![Line::from(Span::styled(
            "no task selected",
            Style::default().fg(DIM),
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
        checklist_line("chatting", chatting),
        checklist_line("verifying", verifying),
        checklist_line("restacking (if needed)", restacking),
        checklist_line("pushing", pushing),
        checklist_line("merging", merging),
    ];

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

    // QA section
    if task.qa_status.is_some() || !task.qa_tests.is_empty() || !task.qa_targets.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "qa",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));

        // QA status line
        if let Some(ref status) = task.qa_status {
            let (qa_marker, qa_color) = qa_status_style(status);
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
                Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
            )));
            lines.extend(qa_test_lines(&task.qa_tests));
        }

        // Task-specific acceptance targets
        if !task.qa_targets.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "qa targets",
                Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
            )));
            for target in &task.qa_targets {
                lines.push(Line::from(vec![
                    Span::styled("  - ", Style::default().fg(DIM)),
                    Span::styled(target.clone(), Style::default().fg(Color::White)),
                ]));
            }
        }
    }

    if !activity.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "activity",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
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
                DIM
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
fn qa_status_style(status: &str) -> (&'static str, Color) {
    let lower = status.to_ascii_lowercase();
    if lower.contains("running") {
        ("~", Color::Cyan)
    } else if lower.contains("passed") && !lower.contains("failed") {
        ("x", Color::Green)
    } else if lower.contains("failed") {
        ("!", Color::Red)
    } else {
        (" ", MUTED)
    }
}

/// Render QA test results grouped by suite.
fn qa_test_lines(tests: &[QATestDisplay]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut current_suite = String::new();

    for test in tests {
        if test.suite != current_suite {
            current_suite = test.suite.clone();
            lines.push(Line::from(Span::styled(
                format!("  {}", current_suite),
                Style::default().fg(MUTED),
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

pub(crate) fn to_local_time(value: DateTime<Utc>) -> String {
    value
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}
