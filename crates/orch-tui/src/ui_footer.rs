use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::TuiApp;
use crate::ui_activity::footer_activity_indicator;
use crate::ui_format::status_line_color;

const ACCENT: Color = Color::Cyan;
const HEADER_FG: Color = Color::White;
const DIM: Color = Color::DarkGray;
const KEY_FG: Color = Color::Yellow;
const MUTED: Color = Color::Gray;
const FOOTER_DEFAULT_HEIGHT: u16 = 3;
const FOOTER_PROMPT_MIN_HEIGHT: u16 = 6;
const FOOTER_PROMPT_MAX_HEIGHT: u16 = 12;

pub(crate) struct FooterContent {
    pub title: &'static str,
    pub lines: Vec<Line<'static>>,
    pub wrap_trim: bool,
}

pub(crate) fn footer_height(app: &TuiApp, width: u16) -> u16 {
    // When in focused view with ChatInput, the inline box handles display
    let in_focused = app.state.focused_task || app.state.focused_pane_idx.is_some();
    if in_focused && app.chat_input_display().is_some() {
        return FOOTER_DEFAULT_HEIGHT;
    }
    let Some(prompt) = app.input_prompt() else {
        return FOOTER_DEFAULT_HEIGHT;
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
    u16::try_from(total_height)
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
                title: if app.chat_input_display().is_some() {
                    "Chat Input"
                } else {
                    "New Chat"
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
        ("i", "interact"),
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

    FooterContent {
        title: "Actions",
        lines: vec![Line::from(spans)],
        wrap_trim: true,
    }
}
