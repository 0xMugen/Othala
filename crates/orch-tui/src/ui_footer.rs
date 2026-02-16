use orch_core::state::TaskState;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{InputMode, TuiApp};
use crate::model::{DashboardState, ModelHealthDisplay};
use crate::ui_activity::footer_activity_indicator;
use crate::ui_format::status_line_color;

const ACCENT: Color = Color::Cyan;
const HEADER_FG: Color = Color::White;
const DIM: Color = Color::DarkGray;
const KEY_FG: Color = Color::Yellow;
const MUTED: Color = Color::Gray;
const FOOTER_DEFAULT_HEIGHT: u16 = 4;
const FOOTER_PROMPT_MIN_HEIGHT: u16 = 6;
const FOOTER_PROMPT_MAX_HEIGHT: u16 = 12;

pub(crate) struct FooterContent {
    pub title: &'static str,
    pub lines: Vec<Line<'static>>,
    pub wrap_trim: bool,
}

pub(crate) fn footer_height(app: &TuiApp, width: u16) -> u16 {
    let model_health_lines = if app.state.model_health.is_empty() { 0 } else { 1 };
    // When in focused view with ChatInput, the inline box handles display
    let in_focused = app.state.focused_task || app.state.focused_pane_idx.is_some();
    if in_focused && app.chat_input_display().is_some() {
        return FOOTER_DEFAULT_HEIGHT + model_health_lines;
    }
    let Some(prompt) = app.input_prompt() else {
        return FOOTER_DEFAULT_HEIGHT + model_health_lines;
    };
    let content_width = width.saturating_sub(2).max(1);
    let prompt_visual_lines = wrapped_visual_line_count(prompt, content_width.saturating_sub(1));
    let controls_visual_lines = wrapped_visual_line_count(
        " Enter=submit Esc=cancel (multiline paste supported)",
        content_width,
    );
    let total_height = prompt_visual_lines
        .saturating_add(controls_visual_lines)
        .saturating_add(3);
    u16::try_from(total_height.saturating_add(model_health_lines as usize))
        .unwrap_or(FOOTER_PROMPT_MAX_HEIGHT)
        .clamp(FOOTER_PROMPT_MIN_HEIGHT, FOOTER_PROMPT_MAX_HEIGHT)
}

pub(crate) fn wrapped_visual_line_count(text: &str, width: u16) -> usize {
    let width = usize::from(width.max(1));
    if text.is_empty() {
        return 1;
    }
    text.split('\n')
        .map(|line| {
            let len = line.chars().count();
            if len == 0 {
                1
            } else {
                (len - 1) / width + 1
            }
        })
        .sum()
}

pub(crate) fn build_footer_content(app: &TuiApp) -> FooterContent {
    if let Some((task_id, branch)) = app.delete_confirm_display() {
        let branch_label = branch.unwrap_or("-").to_string();
        return FooterContent {
            title: "Confirm Delete",
            lines: vec![Line::from(vec![
                Span::styled(" delete: ", Style::default().fg(DIM)),
                Span::styled(task_id.0.clone(), Style::default().fg(HEADER_FG)),
                Span::styled(" branch=", Style::default().fg(DIM)),
                Span::styled(branch_label, Style::default().fg(HEADER_FG)),
                Span::styled("  Enter=confirm Esc=cancel", Style::default().fg(DIM)),
            ])],
            wrap_trim: true,
        };
    }

    if let Some((models, selected)) = app.model_select_display() {
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
        return FooterContent {
            title: "Select Model",
            lines: vec![Line::from(spans)],
            wrap_trim: true,
        };
    }

    // Skip inline prompt when focused view handles ChatInput display
    let in_focused = app.state.focused_task || app.state.focused_pane_idx.is_some();
    let chat_input_handled_inline = in_focused && app.chat_input_display().is_some();
    if !chat_input_handled_inline {
        if let Some(prompt) = app.input_prompt() {
            let mut lines = vec![Line::from(Span::styled(
                " prompt:",
                Style::default().fg(DIM),
            ))];
            let prompt_lines: Vec<String> =
                prompt.split('\n').map(|line| line.to_string()).collect();
            for (idx, prompt_line) in prompt_lines.iter().enumerate() {
                let mut spans = vec![
                    Span::styled(" ", Style::default().fg(DIM)),
                    Span::styled(prompt_line.clone(), Style::default().fg(HEADER_FG)),
                ];
                if idx + 1 == prompt_lines.len() {
                    spans.push(Span::styled("_", Style::default().fg(ACCENT)));
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(Span::styled(
                " Enter=submit Esc=cancel (multiline paste supported)",
                Style::default().fg(DIM),
            )));
            return FooterContent {
                title: match &app.input_mode {
                    InputMode::ChatInput { .. } => "Chat Input",
                    InputMode::FilterInput { .. } => "Filter",
                    _ => "New Chat",
                },
                lines,
                wrap_trim: false,
            };
        }
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" "));
    let keys: &[(&str, &str)] = &[
        ("c", "chat"),
        ("N", "new-task"),
        ("i", "interact"),
        ("a", "approve"),
        ("g", "submit"),
        ("s", "start"),
        ("x", "stop"),
        ("r", "restart"),
        ("d", "delete"),
        ("q", "quick"),
        ("f", "full"),
        ("/", "filter"),
        ("F", "state-filter"),
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
            "| \u{2191}\u{2193}=select \u{2190}\u{2192}=pane \u{21B9}=focus \u{23CE}=detail esc=quit",
            Style::default().fg(DIM),
        ));
    }
    let state_summary = app.state.state_summary();
    if !state_summary.is_empty() {
        spans.push(Span::styled(" | tasks: ", Style::default().fg(DIM)));
        spans.push(Span::styled(
            state_summary,
            Style::default().fg(HEADER_FG).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some((activity, color)) = footer_activity_indicator(app) {
        spans.push(Span::styled(" | thinking: ", Style::default().fg(DIM)));
        spans.push(Span::styled(
            activity,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    if !app.state.status_line.is_empty() {
        spans.push(Span::styled(" | status: ", Style::default().fg(DIM)));
        spans.push(Span::styled(
            app.state.status_line.clone(),
            Style::default()
                .fg(status_line_color(&app.state.status_line))
                .add_modifier(Modifier::BOLD),
        ));
    }

    let mut lines = vec![Line::from(spans)];
    if !app.state.model_health.is_empty() {
        lines.push(model_health_line(&app.state.model_health));
    }
    lines.push(Line::from(Span::styled(
        progress_bar_text(&app.state),
        Style::default().fg(HEADER_FG),
    )));

    FooterContent {
        title: "Actions",
        lines,
        wrap_trim: true,
    }
}

pub(crate) fn progress_bar_text(state: &DashboardState) -> String {
    const BAR_WIDTH: usize = 16;

    let total = state.tasks.len();
    let done = state
        .tasks
        .iter()
        .filter(|task| matches!(task.state, TaskState::Merged | TaskState::Stopped))
        .count();
    let filled = if total == 0 {
        0
    } else {
        (done * BAR_WIDTH + (total / 2)) / total
    }
    .min(BAR_WIDTH);
    let empty = BAR_WIDTH.saturating_sub(filled);
    let percentage = state.completion_percentage().round() as u32;

    format!(
        "Progress: [{}{}] {}% ({done}/{total} tasks)",
        "█".repeat(filled),
        "░".repeat(empty),
        percentage
    )
}

fn model_health_line(model_health: &[ModelHealthDisplay]) -> Line<'static> {
    let mut spans = vec![
        Span::styled(" Models: ", Style::default().fg(DIM)),
    ];

    for (idx, entry) in model_health.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        let symbol = if entry.healthy { "✓" } else { "✗" };
        let color = if entry.healthy {
            Color::Green
        } else {
            Color::Red
        };
        spans.push(Span::styled(
            format!("{}:{symbol}", entry.model),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::state::TaskState;
    use orch_core::types::{RepoId, TaskId};
    use ratatui::style::Color;

    use crate::model::{ModelHealthDisplay, TaskOverviewRow};
    use crate::TuiApp;

    use super::{build_footer_content, progress_bar_text};

    fn mk_row(task_id: &str, state: TaskState) -> TaskOverviewRow {
        TaskOverviewRow {
            task_id: TaskId(task_id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Task {task_id}"),
            branch: format!("task/{task_id}"),
            stack_position: None,
            state,
            display_state: format!("{state:?}"),
            verify_summary: "not_run".to_string(),
            last_activity: Utc::now(),
            qa_status: None,
            qa_tests: Vec::new(),
            qa_targets: Vec::new(),
            estimated_tokens: None,
            estimated_cost_usd: None,
            retry_count: 0,
            retry_history: Vec::new(),
            depends_on_display: Vec::new(),
            pr_url: None,
            model_display: None,
        }
    }

    #[test]
    fn model_health_all_healthy() {
        let mut app = TuiApp::default();
        app.state.model_health = vec![
            ModelHealthDisplay {
                model: "claude".to_string(),
                healthy: true,
                recent_failures: 0,
                cooldown_until: None,
            },
            ModelHealthDisplay {
                model: "codex".to_string(),
                healthy: true,
                recent_failures: 0,
                cooldown_until: None,
            },
            ModelHealthDisplay {
                model: "gemini".to_string(),
                healthy: true,
                recent_failures: 0,
                cooldown_until: None,
            },
        ];

        let content = build_footer_content(&app);
        let text: String = content
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();

        assert!(text.contains("Models: claude:✓ codex:✓ gemini:✓"));
        for line in &content.lines {
            for span in &line.spans {
                let content = span.content.as_ref();
                if content.contains("claude:✓")
                    || content.contains("codex:✓")
                    || content.contains("gemini:✓")
                {
                    assert_eq!(span.style.fg, Some(Color::Green));
                }
            }
        }
    }

    #[test]
    fn model_health_mixed() {
        let mut app = TuiApp::default();
        app.state.model_health = vec![
            ModelHealthDisplay {
                model: "claude".to_string(),
                healthy: true,
                recent_failures: 0,
                cooldown_until: None,
            },
            ModelHealthDisplay {
                model: "codex".to_string(),
                healthy: true,
                recent_failures: 0,
                cooldown_until: None,
            },
            ModelHealthDisplay {
                model: "gemini".to_string(),
                healthy: false,
                recent_failures: 2,
                cooldown_until: Some("2026-02-09T12:00:00Z".to_string()),
            },
        ];

        let content = build_footer_content(&app);
        let text: String = content
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();
        assert!(text.contains("Models: claude:✓ codex:✓ gemini:✗"));

        let gemini_span = content
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref().contains("gemini:✗"))
            .expect("gemini health span");
        assert_eq!(gemini_span.style.fg, Some(Color::Red));
    }

    #[test]
    fn progress_bar_format() {
        let mut app = TuiApp::default();
        app.state.tasks = vec![
            mk_row("T1", TaskState::Merged),
            mk_row("T2", TaskState::Stopped),
            mk_row("T3", TaskState::Merged),
            mk_row("T4", TaskState::Stopped),
            mk_row("T5", TaskState::Merged),
            mk_row("T6", TaskState::Chatting),
            mk_row("T7", TaskState::Chatting),
            mk_row("T8", TaskState::Chatting),
            mk_row("T9", TaskState::Ready),
            mk_row("T10", TaskState::Submitting),
        ];

        let line = progress_bar_text(&app.state);
        assert_eq!(line, "Progress: [████████░░░░░░░░] 50% (5/10 tasks)");
    }
}
