use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const OUTPUT_FG: Color = Color::White;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OutputBlockState {
    in_patch_block: bool,
    in_code_fence: bool,
    in_diff_block: bool,
    in_exec_block: bool,
}

impl OutputBlockState {
    fn update(&mut self, line: &str) {
        let trimmed = line.trim();

        if line.starts_with("*** Begin Patch") {
            self.in_patch_block = true;
            self.in_exec_block = false;
        } else if line.starts_with("*** End Patch") {
            self.in_patch_block = false;
        }

        if line.trim_start().starts_with("```") {
            self.in_code_fence = !self.in_code_fence;
            if self.in_code_fence {
                self.in_exec_block = false;
            }
        }

        if line.starts_with("diff --git") || line.starts_with("diff --cc") {
            self.in_diff_block = true;
            self.in_exec_block = false;
        } else if self.in_diff_block && !self.in_patch_block {
            let is_diff_line = line.starts_with('+')
                || line.starts_with('-')
                || line.starts_with(' ')
                || line.starts_with("@@")
                || line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with('\\')
                || line.is_empty();
            if !is_diff_line {
                self.in_diff_block = false;
            }
        }

        if trimmed == "exec" {
            self.in_exec_block = true;
        } else if self.in_exec_block
            && (trimmed == "thinking"
                || trimmed == "codex"
                || trimmed == "claude"
                || trimmed == "gemini"
                || is_patch_marker(line))
        {
            self.in_exec_block = false;
        }
    }
}

pub(crate) fn output_line_style(line: &str, state: &OutputBlockState) -> Style {
    if let Some(style) = patch_line_style(line, state.in_patch_block) {
        return style;
    }

    if line.trim_start().starts_with("```") {
        return Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    }

    if line.starts_with("diff --git") || line.starts_with("diff --cc") {
        return Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    }

    if state.in_code_fence {
        return Style::default().fg(Color::Gray);
    }

    if state.in_diff_block {
        return diff_line_style(line);
    }

    let trimmed = line.trim();
    if trimmed == "thinking" {
        return Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::DIM);
    }
    if trimmed == "exec" {
        return Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
    }
    if trimmed == "codex" || trimmed == "claude" || trimmed == "gemini" {
        return Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    }

    if trimmed.ends_with('s') && (trimmed.contains("succeeded in") || trimmed.contains("failed in"))
    {
        return Style::default().fg(Color::Yellow);
    }

    if line.starts_with("[agent exited") {
        return Style::default().fg(DIM);
    }
    if line.starts_with("tokens used") {
        return Style::default().fg(DIM);
    }

    if state.in_exec_block {
        return Style::default().fg(Color::Gray);
    }

    let lower = line.to_ascii_lowercase();
    if line.trim().is_empty() {
        Style::default().fg(DIM)
    } else if lower.contains("[needs_human]") {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if lower.contains("[patch_ready]") {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else if lower.contains("[qa_complete]") {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if lower.contains("error") || lower.contains("failed") {
        Style::default().fg(Color::Red)
    } else if line.starts_with("## ") {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(OUTPUT_FG)
    }
}

pub(crate) fn stylize_output_lines(lines: impl IntoIterator<Item = String>) -> Vec<Line<'static>> {
    let mut block_state = OutputBlockState::default();
    lines
        .into_iter()
        .map(|line| {
            let style = output_line_style(&line, &block_state);
            block_state.update(&line);
            Line::from(Span::styled(line, style))
        })
        .collect()
}

fn diff_line_style(line: &str) -> Style {
    if line.starts_with("@@") {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if line.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(OUTPUT_FG)
    }
}

fn patch_line_style(line: &str, in_patch_block: bool) -> Option<Style> {
    if is_patch_marker(line) {
        return Some(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));
    }

    if !in_patch_block {
        return None;
    }

    if line.starts_with("@@") {
        Some(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else if line.starts_with('+') {
        Some(Style::default().fg(Color::Green))
    } else if line.starts_with('-') {
        Some(Style::default().fg(Color::Red))
    } else {
        None
    }
}

fn is_patch_marker(line: &str) -> bool {
    line.starts_with("*** Begin Patch")
        || line.starts_with("*** End Patch")
        || line.starts_with("*** Update File:")
        || line.starts_with("*** Add File:")
        || line.starts_with("*** Delete File:")
        || line.starts_with("*** Move to:")
}

#[cfg(test)]
mod tests {
    use super::{output_line_style, OutputBlockState};
    use ratatui::style::{Color, Modifier};

    #[test]
    fn output_line_style_marks_special_chat_signals() {
        let default_state = OutputBlockState::default();
        assert_eq!(
            output_line_style("[needs_human] unblock me", &default_state).fg,
            Some(Color::Yellow)
        );
        assert_eq!(
            output_line_style("[patch_ready] complete", &default_state).fg,
            Some(Color::Green)
        );
        assert_eq!(
            output_line_style("fatal error", &default_state).fg,
            Some(Color::Red)
        );
    }

    #[test]
    fn output_line_style_marks_patch_edits_clearly() {
        let default_state = OutputBlockState::default();
        let patch_state = OutputBlockState {
            in_patch_block: true,
            ..OutputBlockState::default()
        };
        assert_eq!(
            output_line_style("*** Begin Patch", &default_state).fg,
            Some(Color::Cyan)
        );
        assert_eq!(
            output_line_style("@@ fn main @@ ", &patch_state).fg,
            Some(Color::Yellow)
        );
        assert_eq!(
            output_line_style("+let x = 1;", &patch_state).fg,
            Some(Color::Green)
        );
        assert_eq!(
            output_line_style("-let x = 0;", &patch_state).fg,
            Some(Color::Red)
        );
    }

    #[test]
    fn code_fence_lines_use_muted_style() {
        let state = OutputBlockState {
            in_code_fence: true,
            ..OutputBlockState::default()
        };
        assert_eq!(
            output_line_style("let x = 42;", &state).fg,
            Some(Color::Gray)
        );
    }

    #[test]
    fn code_fence_skips_false_error_styling() {
        let state = OutputBlockState {
            in_code_fence: true,
            ..OutputBlockState::default()
        };
        assert_eq!(
            output_line_style("error: something went wrong", &state).fg,
            Some(Color::Gray)
        );
    }

    #[test]
    fn diff_block_lines_get_diff_coloring() {
        let state = OutputBlockState {
            in_diff_block: true,
            ..OutputBlockState::default()
        };
        assert_eq!(
            output_line_style("+added line", &state).fg,
            Some(Color::Green)
        );
        assert_eq!(
            output_line_style("-removed line", &state).fg,
            Some(Color::Red)
        );
        assert_eq!(
            output_line_style("@@ -1,3 +1,4 @@", &state).fg,
            Some(Color::Yellow)
        );
    }

    #[test]
    fn diff_block_header_styled_as_accent() {
        let default_state = OutputBlockState::default();
        let style = output_line_style("diff --git a/f b/f", &default_state);
        assert_eq!(style.fg, Some(Color::Cyan));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn exec_block_lines_use_muted_style() {
        let state = OutputBlockState {
            in_exec_block: true,
            ..OutputBlockState::default()
        };
        assert_eq!(
            output_line_style(
                "    Finished test profile [unoptimized + debuginfo]",
                &state
            )
            .fg,
            Some(Color::Gray)
        );
    }

    #[test]
    fn exec_block_skips_false_error_styling() {
        let state = OutputBlockState {
            in_exec_block: true,
            ..OutputBlockState::default()
        };
        assert_eq!(
            output_line_style("test result: ok. 0 passed; 0 failed;", &state).fg,
            Some(Color::Gray)
        );
    }

    #[test]
    fn thinking_marker_styled_as_dim_magenta() {
        let default_state = OutputBlockState::default();
        let style = output_line_style("thinking", &default_state);
        assert_eq!(style.fg, Some(Color::Magenta));
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn exec_marker_styled_as_yellow_bold() {
        let default_state = OutputBlockState::default();
        let style = output_line_style("exec", &default_state);
        assert_eq!(style.fg, Some(Color::Yellow));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn codex_marker_styled_as_accent() {
        let default_state = OutputBlockState::default();
        let style = output_line_style("codex", &default_state);
        assert_eq!(style.fg, Some(Color::Cyan));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn agent_exit_line_styled_as_dim() {
        let default_state = OutputBlockState::default();
        let style = output_line_style("[agent exited code=0]", &default_state);
        assert_eq!(style.fg, Some(Color::DarkGray));
    }
}
