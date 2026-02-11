//! Renders structured `ChatBlock` values into styled ratatui `Line` sequences
//! for the focused chat zone view.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::model::{ChatBlock, ToolStatus};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const OUTPUT_FG: Color = Color::White;

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Truncate a string to at most `max_chars` display characters.
/// Appends `…` if truncated.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let mut truncate_at = None;
    let mut keep_end = 0usize;
    for (i, (idx, _)) in s.char_indices().enumerate() {
        if i == max_chars.saturating_sub(1) {
            keep_end = idx;
        }
        if i == max_chars {
            truncate_at = Some(idx);
            break;
        }
    }

    if truncate_at.is_none() {
        return s.to_string();
    }

    let mut result = String::with_capacity(keep_end + '\u{2026}'.len_utf8());
    result.push_str(&s[..keep_end]);
    result.push('\u{2026}');
    result
}

// ── Dispatch ────────────────────────────────────────────────────────────────

/// Convert parsed chat blocks into styled ratatui `Line` sequences for display.
pub fn render_chat_blocks(blocks: &[ChatBlock], width: u16) -> Vec<Line<'static>> {
    let inner_w = width.saturating_sub(2) as usize;
    let mut output = Vec::with_capacity(blocks.len().saturating_mul(3));
    for (i, block) in blocks.iter().enumerate() {
        // AgentMarker handles its own spacing; others get a blank separator.
        if i > 0 && !matches!(block, ChatBlock::AgentMarker { .. }) {
            output.push(Line::from(""));
        }
        match block {
            ChatBlock::UserMessage { lines } => render_user_message(&mut output, lines, inner_w),
            ChatBlock::Thinking { lines } => render_thinking(&mut output, lines, inner_w),
            ChatBlock::AssistantText { lines } => {
                render_assistant_text(&mut output, lines, inner_w)
            }
            ChatBlock::ToolCall {
                tool,
                lines,
                status,
            } => render_tool_call(&mut output, tool, lines, *status, inner_w),
            ChatBlock::CodeFence { lang, lines } => {
                render_code_fence(&mut output, lang.as_deref(), lines, width)
            }
            ChatBlock::Diff { lines } => render_diff(&mut output, lines, width),
            ChatBlock::AgentMarker { agent } => render_agent_marker(&mut output, agent, width),
            ChatBlock::StatusSignal { line } => render_status_signal(&mut output, line, inner_w),
        }
    }
    output
}

// ── User Message ────────────────────────────────────────────────────────────

fn render_user_message(output: &mut Vec<Line<'static>>, lines: &[String], inner_w: usize) {
    let budget = inner_w.saturating_sub(2); // prefix "│ " = 2 chars
    for line in lines {
        output.push(Line::from(vec![
            Span::styled(
                "\u{2502} ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate_str(line, budget),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }
}

// ── Thinking (summary-first collapse) ───────────────────────────────────────

fn render_thinking(output: &mut Vec<Line<'static>>, lines: &[String], inner_w: usize) {
    let summary_style = Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD | Modifier::ITALIC);
    let body_style = Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::DIM | Modifier::ITALIC);

    let budget = inner_w.saturating_sub(4); // prefix "  ~ " = 4 chars

    let summary = lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "thinking...".to_string());

    output.push(Line::from(vec![
        Span::styled("  ~ ", summary_style),
        Span::styled(truncate_str(&summary, budget), summary_style),
    ]));

    if lines.len() <= 5 {
        for line in lines.iter().skip(1) {
            output.push(Line::from(Span::styled(
                truncate_str(&format!("    {line}"), inner_w),
                body_style,
            )));
        }
    } else {
        let remaining = lines.len().saturating_sub(1);
        output.push(Line::from(Span::styled(
            format!("    ({remaining} more lines)"),
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        )));
    }
}

// ── Inline Markdown ─────────────────────────────────────────────────────────

fn markdown_line_to_spans(line: &str) -> Vec<Span<'static>> {
    let trimmed = line.trim_start();

    // Headings
    if let Some(rest) = trimmed.strip_prefix("### ") {
        return vec![Span::styled(
            format!("   {rest}"),
            Style::default()
                .fg(OUTPUT_FG)
                .add_modifier(Modifier::ITALIC),
        )];
    }
    if let Some(rest) = trimmed.strip_prefix("## ") {
        return vec![Span::styled(
            rest.to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )];
    }
    if let Some(rest) = trimmed.strip_prefix("# ") {
        return vec![Span::styled(
            rest.to_string(),
            Style::default()
                .fg(OUTPUT_FG)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )];
    }

    // Blockquotes
    if let Some(rest) = trimmed.strip_prefix("> ") {
        let mut spans = vec![Span::styled("\u{2502} ", Style::default().fg(Color::Green))];
        spans.extend(parse_inline_markdown(
            rest,
            Style::default().fg(Color::Green),
        ));
        return spans;
    }

    // Unordered lists
    let list_rest = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "));
    if let Some(rest) = list_rest {
        let indent = line.len() - trimmed.len();
        let prefix = " ".repeat(indent);
        let mut spans = vec![Span::styled(
            format!("{prefix} \u{2022} "),
            Style::default().fg(ACCENT),
        )];
        spans.extend(parse_inline_markdown(rest, Style::default().fg(OUTPUT_FG)));
        return spans;
    }

    // Ordered lists
    if let Some((num_str, rest)) = try_parse_ordered_list(trimmed) {
        let indent = line.len() - trimmed.len();
        let prefix = " ".repeat(indent);
        let mut spans = vec![Span::styled(
            format!("{prefix} {num_str}. "),
            Style::default().fg(Color::Blue),
        )];
        spans.extend(parse_inline_markdown(rest, Style::default().fg(OUTPUT_FG)));
        return spans;
    }

    // Default: inline markdown
    parse_inline_markdown(line, Style::default().fg(OUTPUT_FG))
}

fn try_parse_ordered_list(trimmed: &str) -> Option<(String, &str)> {
    let dot_pos = trimmed.find(". ")?;
    let num_part = &trimmed[..dot_pos];
    if !num_part.is_empty() && num_part.chars().all(|c| c.is_ascii_digit()) {
        Some((num_part.to_string(), &trimmed[dot_pos + 2..]))
    } else {
        None
    }
}

fn parse_inline_markdown(text: &str, base_style: Style) -> Vec<Span<'static>> {
    if !text.contains('`') && !text.contains('*') {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    let mut spans = Vec::with_capacity(4);
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Backtick code span
        if chars[i] == '`' {
            if let Some(end) = find_closing_marker(&chars, i + 1, '`') {
                if !current.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current), base_style));
                }
                let code: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(code, Style::default().fg(ACCENT)));
                i = end + 1;
                continue;
            }
        }

        // Bold+italic (***text***)
        if i + 2 < len && chars[i] == '*' && chars[i + 1] == '*' && chars[i + 2] == '*' {
            if let Some(end) = find_closing_triple_star(&chars, i + 3) {
                if !current.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current), base_style));
                }
                let content: String = chars[i + 3..end].iter().collect();
                spans.push(Span::styled(
                    content,
                    base_style.add_modifier(Modifier::BOLD | Modifier::ITALIC),
                ));
                i = end + 3;
                continue;
            }
            // No closing *** found — emit literal *** and skip all three
            current.push('*');
            current.push('*');
            current.push('*');
            i += 3;
            continue;
        }

        // Bold (**text**)
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_closing_double_star(&chars, i + 2) {
                if !current.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current), base_style));
                }
                let content: String = chars[i + 2..end].iter().collect();
                spans.push(Span::styled(
                    content,
                    base_style.add_modifier(Modifier::BOLD),
                ));
                i = end + 2;
                continue;
            }
            // No closing ** found — emit literal ** and skip both chars
            current.push('*');
            current.push('*');
            i += 2;
            continue;
        }

        // Italic (*text*)
        if chars[i] == '*' {
            if let Some(end) = find_closing_single_star(&chars, i + 1) {
                if !current.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current), base_style));
                }
                let content: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(
                    content,
                    base_style.add_modifier(Modifier::ITALIC),
                ));
                i = end + 1;
                continue;
            }
        }

        current.push(chars[i]);
        i += 1;
    }

    if !current.is_empty() {
        spans.push(Span::styled(current, base_style));
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }

    spans
}

fn find_closing_marker(chars: &[char], start: usize, marker: char) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == marker)
}

fn find_closing_triple_star(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 2 < chars.len() {
        if chars[i] == '*' && chars[i + 1] == '*' && chars[i + 2] == '*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_closing_double_star(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < chars.len() {
        if chars[i] == '*' && chars[i + 1] == '*' && (i + 2 >= chars.len() || chars[i + 2] != '*') {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_closing_single_star(chars: &[char], start: usize) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == '*' && (i + 1 >= chars.len() || chars[i + 1] != '*'))
}

// ── Assistant Text (markdown-aware) ─────────────────────────────────────────

fn render_assistant_text(output: &mut Vec<Line<'static>>, lines: &[String], inner_w: usize) {
    for line in lines {
        if line.trim().is_empty() {
            output.push(Line::from(""));
        } else {
            let truncated = truncate_str(line, inner_w);
            output.push(Line::from(markdown_line_to_spans(&truncated)));
        }
    }
}

// ── Tool Call (Codex-style tree) ────────────────────────────────────────────

fn render_tool_call(
    output: &mut Vec<Line<'static>>,
    tool: &str,
    lines: &[String],
    status: ToolStatus,
    inner_w: usize,
) {
    let (indicator, indicator_color) = match status {
        ToolStatus::Succeeded => ("\u{25CF}", Color::Green),
        ToolStatus::Failed => ("\u{25CF}", Color::Red),
        ToolStatus::Running => ("\u{25CF}", Color::Yellow),
    };

    let prefix_len = 3; // " ● " / " │ " / " └ "
    let budget = inner_w.saturating_sub(prefix_len);

    // Extract command label from first non-empty line
    let command_label = lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| tool.to_string());

    output.push(Line::from(vec![
        Span::styled(
            format!(" {indicator} "),
            Style::default().fg(indicator_color),
        ),
        Span::styled(
            truncate_str(&command_label, budget),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    let body_len = lines.len().saturating_sub(1);

    // Detect if body contains edit/diff content → show more lines with red/green
    let has_diff = lines.iter().skip(1).any(|l| is_tool_diff_line(l));
    let (max_visible, head_count, tail_count) = if has_diff { (12, 5, 3) } else { (4, 2, 1) };

    if body_len <= max_visible {
        for (i, line) in lines.iter().skip(1).enumerate() {
            let prefix = if i + 1 == body_len {
                " \u{2514} "
            } else {
                " \u{2502} "
            };
            output.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(DIM)),
                Span::styled(truncate_str(line, budget), tool_line_style(line, has_diff)),
            ]));
        }
    } else {
        // Middle-truncation: first head_count + ellipsis + last tail_count
        for line in lines.iter().skip(1).take(head_count) {
            output.push(Line::from(vec![
                Span::styled(" \u{2502} ", Style::default().fg(DIM)),
                Span::styled(truncate_str(line, budget), tool_line_style(line, has_diff)),
            ]));
        }
        let hidden = body_len - head_count - tail_count;
        output.push(Line::from(vec![
            Span::styled(" \u{2502} ", Style::default().fg(DIM)),
            Span::styled(
                format!("... +{hidden} lines"),
                Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
            ),
        ]));
        let tail_start = lines.len().saturating_sub(tail_count);
        for (i, line) in lines.iter().skip(tail_start).enumerate() {
            let is_last = i + 1 == tail_count;
            let prefix = if is_last { " \u{2514} " } else { " \u{2502} " };
            output.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(DIM)),
                Span::styled(truncate_str(line, budget), tool_line_style(line, has_diff)),
            ]));
        }
    }
}

/// Detect diff-like lines inside tool output (additions/deletions).
fn is_tool_diff_line(line: &str) -> bool {
    (line.starts_with('+') && !line.starts_with("+++"))
        || (line.starts_with('-') && !line.starts_with("---"))
}

/// Style a tool body line: red/green for diff content, gray otherwise.
fn tool_line_style(line: &str, has_diff: bool) -> Style {
    if has_diff {
        if line.starts_with('+') && !line.starts_with("+++") {
            return Style::default().fg(Color::Green);
        }
        if line.starts_with('-') && !line.starts_with("---") {
            return Style::default().fg(Color::Red);
        }
    }
    Style::default().fg(Color::Gray)
}

// ── Code Fence (with line numbers) ──────────────────────────────────────────

fn render_code_fence(
    output: &mut Vec<Line<'static>>,
    lang: Option<&str>,
    lines: &[String],
    width: u16,
) {
    let inner_w = width.saturating_sub(2) as usize;
    let border_len = inner_w.saturating_sub(2).max(8);

    // Top border with optional language label
    let top = if let Some(lang) = lang {
        let rest_len = border_len.saturating_sub(lang.len() + 5);
        Line::from(vec![
            Span::styled(
                format!(" \u{2500}\u{2500} {lang} "),
                Style::default().fg(ACCENT),
            ),
            Span::styled("\u{2500}".repeat(rest_len), Style::default().fg(ACCENT)),
        ])
    } else {
        Line::from(Span::styled(
            format!(" {}", "\u{2500}".repeat(border_len)),
            Style::default().fg(ACCENT),
        ))
    };
    output.push(top);

    let gutter_w = line_number_width(lines.len());
    let gutter_prefix_len = gutter_w + 4; // " N │ "
    let content_budget = inner_w.saturating_sub(gutter_prefix_len);
    for (i, line) in lines.iter().enumerate() {
        output.push(Line::from(vec![
            Span::styled(
                format!(" {:>w$} \u{2502} ", i + 1, w = gutter_w),
                Style::default().fg(DIM),
            ),
            Span::styled(
                truncate_str(line, content_budget),
                Style::default().fg(Color::Gray),
            ),
        ]));
    }

    // Bottom border
    output.push(Line::from(Span::styled(
        format!(" {}", "\u{2500}".repeat(border_len)),
        Style::default().fg(ACCENT),
    )));
}

fn line_number_width(count: usize) -> usize {
    if count == 0 {
        1
    } else {
        count.ilog10() as usize + 1
    }
}

// ── Diff (file headers + gutter) ────────────────────────────────────────────

fn render_diff(output: &mut Vec<Line<'static>>, lines: &[String], width: u16) {
    let inner_w = width.saturating_sub(2) as usize;
    let file_path = extract_diff_file_path(lines);
    let (additions, deletions) = count_diff_stats(lines);

    if let Some(path) = &file_path {
        let border_len = inner_w.saturating_sub(2).max(8);
        output.push(Line::from(vec![
            Span::styled(
                format!("  {path}"),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  +{additions} -{deletions}"),
                Style::default().fg(DIM),
            ),
        ]));
        output.push(Line::from(Span::styled(
            format!("  {}", "\u{2500}".repeat(border_len.saturating_sub(2))),
            Style::default().fg(DIM),
        )));
    }

    let gutter_len = 11; // "NNNN NNNN │"
    let content_budget = inner_w.saturating_sub(gutter_len);

    let mut line_num_old: usize = 0;
    let mut line_num_new: usize = 0;
    let mut had_hunk = false;

    for line in lines {
        // Skip raw header lines if we already rendered a nice header
        if file_path.is_some() && is_diff_header_line(line) {
            continue;
        }

        if line.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_hunk_header(line) {
                line_num_old = old_start;
                line_num_new = new_start;
            }
            if had_hunk {
                output.push(Line::from(Span::styled(
                    "            \u{22EE}",
                    Style::default().fg(DIM),
                )));
            }
            had_hunk = true;
            continue;
        }

        // Patch markers pass through with their own styling
        if is_patch_marker(line) {
            output.push(Line::from(Span::styled(
                truncate_str(line, inner_w),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )));
            continue;
        }

        let style = diff_content_style(line);

        if had_hunk {
            let gutter = format_diff_gutter(line, &mut line_num_old, &mut line_num_new);
            output.push(Line::from(vec![
                Span::styled(gutter, Style::default().fg(DIM)),
                Span::styled(truncate_str(line, content_budget), style),
            ]));
        } else {
            output.push(Line::from(Span::styled(truncate_str(line, inner_w), style)));
        }
    }
}

fn extract_diff_file_path(lines: &[String]) -> Option<String> {
    for line in lines {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            if let Some(idx) = rest.find(" b/") {
                return Some(rest[..idx].to_string());
            }
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            return Some(path.trim().to_string());
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            return Some(path.trim().to_string());
        }
    }
    None
}

fn count_diff_stats(lines: &[String]) -> (usize, usize) {
    let mut additions = 0;
    let mut deletions = 0;
    for line in lines {
        if line.starts_with('+') && !line.starts_with("+++") && !line.starts_with("*** ") {
            additions += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            deletions += 1;
        }
    }
    (additions, deletions)
}

fn is_diff_header_line(line: &str) -> bool {
    line.starts_with("diff --git")
        || line.starts_with("diff --cc")
        || line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
}

fn is_patch_marker(line: &str) -> bool {
    line.starts_with("*** Begin Patch")
        || line.starts_with("*** End Patch")
        || line.starts_with("*** Update File:")
        || line.starts_with("*** Add File:")
        || line.starts_with("*** Delete File:")
        || line.starts_with("*** Move to:")
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let mut parts = line.split_whitespace();
    let _marker = parts.next()?;
    let old_start = parse_hunk_range_start(parts.next()?, '-')?;
    let new_start = parse_hunk_range_start(parts.next()?, '+')?;
    Some((old_start, new_start))
}

fn parse_hunk_range_start(range: &str, prefix: char) -> Option<usize> {
    range.strip_prefix(prefix)?.split(',').next()?.parse().ok()
}

fn format_diff_gutter(line: &str, old: &mut usize, new: &mut usize) -> String {
    if line.starts_with('+') && !line.starts_with("+++") {
        let n = *new;
        *new += 1;
        format!("     {n:>4} \u{2502}")
    } else if line.starts_with('-') && !line.starts_with("---") {
        let o = *old;
        *old += 1;
        format!("{o:>4}      \u{2502}")
    } else {
        let o = *old;
        let n = *new;
        *old += 1;
        *new += 1;
        format!("{o:>4} {n:>4} \u{2502}")
    }
}

fn diff_content_style(line: &str) -> Style {
    if line.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(OUTPUT_FG)
    }
}

// ── Agent Marker (double-line separator) ────────────────────────────────────

fn render_agent_marker(output: &mut Vec<Line<'static>>, agent: &str, width: u16) {
    output.push(Line::from(""));

    let label = format!(" {agent} ");
    let total = width.saturating_sub(4) as usize;
    let label_len = label.len();
    let left = total.saturating_sub(label_len) / 2;
    let right = total.saturating_sub(label_len).saturating_sub(left);

    output.push(Line::from(vec![
        Span::styled("\u{2550}".repeat(left), Style::default().fg(DIM)),
        Span::styled(
            label,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2550}".repeat(right), Style::default().fg(DIM)),
    ]));
}

// ── Status Signal ───────────────────────────────────────────────────────────

fn render_status_signal(output: &mut Vec<Line<'static>>, line: &str, inner_w: usize) {
    let lower = line.to_ascii_lowercase();
    let color = if lower.contains("[needs_human]") {
        Color::Yellow
    } else if lower.contains("[patch_ready]") || lower.contains("[done]") {
        Color::Green
    } else if lower.contains("[error]") {
        Color::Red
    } else {
        Color::Yellow
    };
    output.push(Line::from(Span::styled(
        truncate_str(line, inner_w),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn spans_text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    // ── User Message ──

    #[test]
    fn user_message_has_cyan_border() {
        let blocks = vec![ChatBlock::UserMessage {
            lines: vec!["hello".to_string()],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].spans[0].content.as_ref(), "\u{2502} ");
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::Cyan));
        assert_eq!(rendered[0].spans[1].content.as_ref(), "hello");
        assert!(rendered[0].spans[1]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
    }

    // ── Thinking ──

    #[test]
    fn thinking_shows_summary_with_prefix() {
        let blocks = vec![ChatBlock::Thinking {
            lines: vec!["reasoning here".to_string()],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        let text = spans_text(&rendered);
        assert!(text[0].contains("~"));
        assert!(text[0].contains("reasoning here"));
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::Magenta));
    }

    #[test]
    fn thinking_collapses_long_blocks() {
        let lines: Vec<String> = (0..10).map(|i| format!("thought {i}")).collect();
        let blocks = vec![ChatBlock::Thinking { lines }];
        let rendered = render_chat_blocks(&blocks, 80);
        // Summary line + collapse count = 2
        assert_eq!(rendered.len(), 2);
        let text = spans_text(&rendered);
        assert!(text[1].contains("9 more lines"));
    }

    #[test]
    fn thinking_shows_all_when_short() {
        let blocks = vec![ChatBlock::Thinking {
            lines: vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string(),
            ],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        // Summary (first line) + 2 body lines = 3
        assert_eq!(rendered.len(), 3);
    }

    // ── Assistant Text (markdown) ──

    #[test]
    fn assistant_text_renders_plain_white() {
        let blocks = vec![ChatBlock::AssistantText {
            lines: vec!["Hello world".to_string()],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::White));
    }

    #[test]
    fn assistant_text_headers_get_accent() {
        let blocks = vec![ChatBlock::AssistantText {
            lines: vec!["## Section".to_string()],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::Cyan));
        assert!(rendered[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn assistant_text_renders_bold() {
        let blocks = vec![ChatBlock::AssistantText {
            lines: vec!["Hello **world**".to_string()],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        let text = spans_text(&rendered);
        assert!(text[0].contains("world"));
        // Find the bold span
        let bold_span = rendered[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "world")
            .expect("bold span");
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn assistant_text_renders_code_span() {
        let blocks = vec![ChatBlock::AssistantText {
            lines: vec!["Use `cargo build`".to_string()],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        let code_span = rendered[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "cargo build")
            .expect("code span");
        assert_eq!(code_span.style.fg, Some(Color::Cyan));
    }

    #[test]
    fn assistant_text_renders_list_items() {
        let blocks = vec![ChatBlock::AssistantText {
            lines: vec!["- first item".to_string()],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        let text = spans_text(&rendered);
        assert!(text[0].contains("\u{2022}"));
        assert!(text[0].contains("first item"));
    }

    #[test]
    fn assistant_text_renders_blockquote() {
        let blocks = vec![ChatBlock::AssistantText {
            lines: vec!["> quoted text".to_string()],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn markdown_unclosed_bold_is_literal() {
        let spans = parse_inline_markdown("Hello **world", Style::default().fg(OUTPUT_FG));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Hello **world");
    }

    // ── Tool Call ──

    #[test]
    fn tool_call_shows_status_indicator() {
        let blocks = vec![ChatBlock::ToolCall {
            tool: "exec".to_string(),
            lines: vec!["cargo test".to_string()],
            status: ToolStatus::Running,
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        let text = spans_text(&rendered);
        assert!(text[0].contains("\u{25CF}"));
        assert!(text[0].contains("cargo test"));
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn tool_call_succeeded_shows_green() {
        let blocks = vec![ChatBlock::ToolCall {
            tool: "exec".to_string(),
            lines: vec!["cargo test".to_string(), "ok".to_string()],
            status: ToolStatus::Succeeded,
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn tool_call_failed_shows_red() {
        let blocks = vec![ChatBlock::ToolCall {
            tool: "exec".to_string(),
            lines: vec!["cargo test".to_string(), "FAILED".to_string()],
            status: ToolStatus::Failed,
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn tool_call_truncates_long_output() {
        let mut lines = vec!["command".to_string()];
        for i in 0..20 {
            lines.push(format!("output line {i}"));
        }
        let blocks = vec![ChatBlock::ToolCall {
            tool: "exec".to_string(),
            lines,
            status: ToolStatus::Running,
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        let text = spans_text(&rendered);
        assert!(text.iter().any(|t| t.contains("... +")));
        // Non-diff: header(1) + head(2) + ellipsis(1) + tail(1) = 5
        assert_eq!(rendered.len(), 5);
    }

    #[test]
    fn tool_call_short_output_shows_all() {
        let blocks = vec![ChatBlock::ToolCall {
            tool: "exec".to_string(),
            lines: vec![
                "command".to_string(),
                "line 1".to_string(),
                "line 2".to_string(),
            ],
            status: ToolStatus::Running,
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        // header(1) + 2 body lines = 3
        assert_eq!(rendered.len(), 3);
        let text = spans_text(&rendered);
        assert!(text.iter().any(|t| t.contains("\u{2514}")));
    }

    // ── Code Fence ──

    #[test]
    fn code_fence_has_borders_and_language_label() {
        let blocks = vec![ChatBlock::CodeFence {
            lang: Some("rust".to_string()),
            lines: vec!["fn main() {}".to_string()],
        }];
        let rendered = render_chat_blocks(&blocks, 40);
        let text = spans_text(&rendered);
        assert!(text[0].contains("rust"));
        assert!(text[1].contains("fn main() {}"));
        assert!(text[2].contains("\u{2500}"));
    }

    #[test]
    fn code_fence_without_language() {
        let blocks = vec![ChatBlock::CodeFence {
            lang: None,
            lines: vec!["code".to_string()],
        }];
        let rendered = render_chat_blocks(&blocks, 40);
        assert_eq!(rendered.len(), 3);
    }

    #[test]
    fn code_fence_has_line_numbers() {
        let blocks = vec![ChatBlock::CodeFence {
            lang: None,
            lines: vec![
                "line one".to_string(),
                "line two".to_string(),
                "line three".to_string(),
            ],
        }];
        let rendered = render_chat_blocks(&blocks, 40);
        let text = spans_text(&rendered);
        assert!(text[1].contains("1"));
        assert!(text[2].contains("2"));
        assert!(text[3].contains("3"));
        assert!(text[1].contains("\u{2502}"));
    }

    // ── Diff ──

    #[test]
    fn diff_block_extracts_file_header_and_stats() {
        let blocks = vec![ChatBlock::Diff {
            lines: vec![
                "diff --git a/src/main.rs b/src/main.rs".to_string(),
                "index abc..def 100644".to_string(),
                "--- a/src/main.rs".to_string(),
                "+++ b/src/main.rs".to_string(),
                "@@ -1,3 +1,4 @@".to_string(),
                " context".to_string(),
                "+added".to_string(),
                "-removed".to_string(),
            ],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        let text = spans_text(&rendered);
        // File header present
        assert!(text[0].contains("src/main.rs"));
        assert!(text[0].contains("+1"));
        assert!(text[0].contains("-1"));
    }

    #[test]
    fn diff_shows_hunk_separator() {
        let blocks = vec![ChatBlock::Diff {
            lines: vec![
                "diff --git a/f b/f".to_string(),
                "@@ -1,2 +1,2 @@".to_string(),
                " a".to_string(),
                "+b".to_string(),
                "@@ -10,2 +10,2 @@".to_string(),
                " c".to_string(),
                "+d".to_string(),
            ],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        let text = spans_text(&rendered);
        assert!(text.iter().any(|t| t.contains("\u{22EE}")));
    }

    #[test]
    fn diff_line_numbers_track() {
        let blocks = vec![ChatBlock::Diff {
            lines: vec![
                "diff --git a/f b/f".to_string(),
                "@@ -1,3 +1,3 @@".to_string(),
                " ctx".to_string(),
                "+add".to_string(),
                "-del".to_string(),
            ],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        let text = spans_text(&rendered);
        // Should have gutter numbers
        assert!(text.iter().any(|t| t.contains("1")));
    }

    #[test]
    fn patch_markers_styled_in_diff_block() {
        let blocks = vec![ChatBlock::Diff {
            lines: vec![
                "*** Begin Patch".to_string(),
                "*** Update File: src/main.rs".to_string(),
                "+added".to_string(),
                "*** End Patch".to_string(),
            ],
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        // File header from patch
        let text = spans_text(&rendered);
        assert!(text[0].contains("src/main.rs"));
        // Patch markers get accent style
        // Begin Patch is a patch marker
        let begin_line = rendered
            .iter()
            .find(|l| {
                l.spans
                    .iter()
                    .any(|s| s.content.as_ref().contains("Begin Patch"))
            })
            .expect("Begin Patch line");
        assert_eq!(begin_line.spans[0].style.fg, Some(Color::Cyan));
    }

    // ── Agent Marker ──

    #[test]
    fn agent_marker_centered_with_double_rules() {
        let blocks = vec![ChatBlock::AgentMarker {
            agent: "claude".to_string(),
        }];
        let rendered = render_chat_blocks(&blocks, 40);
        // Blank line + marker line = 2
        assert_eq!(rendered.len(), 2);
        // Second line has 3 spans (left rule, label, right rule)
        assert_eq!(rendered[1].spans.len(), 3);
        assert!(rendered[1].spans[1].content.contains("claude"));
        assert_eq!(rendered[1].spans[1].style.fg, Some(Color::Cyan));
        // Uses ═ instead of ─
        assert!(rendered[1].spans[0].content.contains('\u{2550}'));
    }

    #[test]
    fn agent_marker_skips_extra_separator() {
        let blocks = vec![
            ChatBlock::AssistantText {
                lines: vec!["hello".to_string()],
            },
            ChatBlock::AgentMarker {
                agent: "claude".to_string(),
            },
            ChatBlock::AssistantText {
                lines: vec!["world".to_string()],
            },
        ];
        let rendered = render_chat_blocks(&blocks, 80);
        // AssistantText(1) + AgentMarker blank(1) + marker(1) + separator(1) + AssistantText(1) = 5
        assert_eq!(rendered.len(), 5);
    }

    // ── Status Signal ──

    #[test]
    fn status_signal_colored_by_type() {
        let needs_human = vec![ChatBlock::StatusSignal {
            line: "[needs_human] waiting".to_string(),
        }];
        let rendered = render_chat_blocks(&needs_human, 80);
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::Yellow));

        let patch_ready = vec![ChatBlock::StatusSignal {
            line: "[patch_ready] done".to_string(),
        }];
        let rendered = render_chat_blocks(&patch_ready, 80);
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::Green));

        let error = vec![ChatBlock::StatusSignal {
            line: "[error] failed".to_string(),
        }];
        let rendered = render_chat_blocks(&error, 80);
        assert_eq!(rendered[0].spans[0].style.fg, Some(Color::Red));
    }

    // ── Truncation ──

    #[test]
    fn truncate_str_preserves_short_strings() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_truncates_long_strings() {
        let result = truncate_str("hello world", 8);
        assert_eq!(result.chars().count(), 8);
        assert!(result.ends_with('\u{2026}'));
    }

    #[test]
    fn truncate_str_respects_unicode_boundaries() {
        let result = truncate_str("🙂🙂🙂", 2);
        assert_eq!(result, "🙂\u{2026}");
        assert_eq!(result.chars().count(), 2);
    }

    #[test]
    fn truncate_str_single_char_budget_uses_ellipsis() {
        assert_eq!(truncate_str("ab", 1), "\u{2026}");
    }

    #[test]
    fn long_line_truncated_in_code_fence() {
        let long_line = "x".repeat(200);
        let blocks = vec![ChatBlock::CodeFence {
            lang: None,
            lines: vec![long_line],
        }];
        let rendered = render_chat_blocks(&blocks, 40);
        // Content line (index 1) should not exceed inner width (38)
        let content_text: String = rendered[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(content_text.chars().count() <= 38);
    }

    // ── Markdown inline parsing ──

    #[test]
    fn markdown_bold_text() {
        let spans = parse_inline_markdown("Hello **world**", Style::default().fg(OUTPUT_FG));
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "Hello ");
        assert_eq!(spans[1].content.as_ref(), "world");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn markdown_italic_text() {
        let spans = parse_inline_markdown("Hello *world*", Style::default().fg(OUTPUT_FG));
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].content.as_ref(), "world");
        assert!(spans[1].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn markdown_code_span() {
        let spans = parse_inline_markdown("Use `cargo build`", Style::default().fg(OUTPUT_FG));
        let code = spans.iter().find(|s| s.content.as_ref() == "cargo build");
        assert!(code.is_some());
        assert_eq!(code.unwrap().style.fg, Some(Color::Cyan));
    }

    #[test]
    fn markdown_bold_italic() {
        let spans = parse_inline_markdown("***important***", Style::default().fg(OUTPUT_FG));
        assert_eq!(spans[0].content.as_ref(), "important");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(spans[0].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn markdown_empty_line() {
        let spans = parse_inline_markdown("", Style::default().fg(OUTPUT_FG));
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn markdown_plain_line_stays_single_span() {
        let spans = parse_inline_markdown("plain line", Style::default().fg(OUTPUT_FG));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "plain line");
    }

    #[test]
    fn line_number_width_uses_decimal_digits() {
        assert_eq!(line_number_width(0), 1);
        assert_eq!(line_number_width(9), 1);
        assert_eq!(line_number_width(10), 2);
        assert_eq!(line_number_width(9_999), 4);
    }

    #[test]
    fn parse_hunk_header_handles_single_line_ranges() {
        assert_eq!(parse_hunk_header("@@ -12 +34 @@"), Some((12, 34)));
    }

    // ── Tool call diff detection ──

    #[test]
    fn tool_call_with_diff_content_uses_red_green() {
        let blocks = vec![ChatBlock::ToolCall {
            tool: "exec".to_string(),
            lines: vec![
                "edit src/main.rs".to_string(),
                " context line".to_string(),
                "+added line".to_string(),
                "-removed line".to_string(),
            ],
            status: ToolStatus::Succeeded,
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        // Find the green span (+added)
        let green_span = rendered
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains("+added"));
        assert_eq!(green_span.unwrap().style.fg, Some(Color::Green));
        // Find the red span (-removed)
        let red_span = rendered
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains("-removed"));
        assert_eq!(red_span.unwrap().style.fg, Some(Color::Red));
    }

    #[test]
    fn tool_call_with_diff_content_allows_more_lines() {
        let mut lines = vec!["edit command".to_string()];
        for i in 0..10 {
            lines.push(format!("+added line {i}"));
        }
        let blocks = vec![ChatBlock::ToolCall {
            tool: "exec".to_string(),
            lines,
            status: ToolStatus::Running,
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        // 10 body lines <= 12 max_visible for diff, so all shown
        // header(1) + 10 body = 11
        assert_eq!(rendered.len(), 11);
    }

    #[test]
    fn tool_call_non_diff_truncates_aggressively() {
        let mut lines = vec!["ls -la".to_string()];
        for i in 0..10 {
            lines.push(format!("file_{i}.txt"));
        }
        let blocks = vec![ChatBlock::ToolCall {
            tool: "exec".to_string(),
            lines,
            status: ToolStatus::Succeeded,
        }];
        let rendered = render_chat_blocks(&blocks, 80);
        // Non-diff: header(1) + head(2) + ellipsis(1) + tail(1) = 5
        assert_eq!(rendered.len(), 5);
    }
}
