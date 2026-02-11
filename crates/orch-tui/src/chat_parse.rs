//! Line-stream parser that segments raw agent output into structured `ChatBlock` values.

use crate::model::{ChatBlock, ToolStatus};

/// Internal parser state tracking which block type we're currently accumulating.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParserState {
    Default,
    InUserMessage,
    InThinking,
    InExec,
    InCodeFence { lang: Option<String> },
    InDiff,
    InPatch,
}

/// Parse a slice of raw output lines into structured `ChatBlock` values.
///
/// Uses the same markers that `OutputBlockState` recognizes: `thinking`, `exec`,
/// `claude`/`codex`/`gemini`, code fences, `diff --git`, `*** Begin Patch`, and
/// `> ` user message prefixes.
pub fn parse_chat_blocks(lines: &[String]) -> Vec<ChatBlock> {
    let mut blocks: Vec<ChatBlock> = Vec::new();
    let mut accumulator: Vec<String> = Vec::new();
    let mut state = ParserState::Default;

    for line in lines {
        let trimmed = line.trim();

        // Detect structural transitions
        match classify_line(line, trimmed, &state) {
            LineClass::UserMessage => {
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::InUserMessage;
                let content = line.strip_prefix("> ").unwrap_or(line);
                accumulator.push(content.to_string());
            }
            LineClass::AgentMarker(agent) => {
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::Default;
                blocks.push(ChatBlock::AgentMarker { agent });
            }
            LineClass::ThinkingMarker => {
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::InThinking;
            }
            LineClass::ExecMarker => {
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::InExec;
            }
            LineClass::CodeFenceOpen(lang) => {
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::InCodeFence { lang };
            }
            LineClass::CodeFenceClose => {
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::Default;
            }
            LineClass::DiffHeader => {
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::InDiff;
                accumulator.push(line.clone());
            }
            LineClass::PatchBegin => {
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::InPatch;
                accumulator.push(line.clone());
            }
            LineClass::PatchEnd => {
                accumulator.push(line.clone());
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::Default;
            }
            LineClass::StatusSignal => {
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::Default;
                blocks.push(ChatBlock::StatusSignal { line: line.clone() });
            }
            LineClass::DiffContent => {
                accumulator.push(line.clone());
            }
            LineClass::DiffExit => {
                flush_block(&mut blocks, &mut accumulator, &state);
                state = ParserState::Default;
                // Re-classify this line in default context
                accumulator.push(line.clone());
            }
            LineClass::ContinueUserMessage => {
                let content = line.strip_prefix("> ").unwrap_or(line);
                accumulator.push(content.to_string());
            }
            LineClass::Content => {
                // If we were in a user message but got a non-"> " line,
                // flush the user message and start default accumulation.
                if matches!(state, ParserState::InUserMessage) {
                    flush_block(&mut blocks, &mut accumulator, &state);
                    state = ParserState::Default;
                }
                accumulator.push(line.clone());
            }
        }
    }

    flush_block(&mut blocks, &mut accumulator, &state);
    blocks
}

/// Classification result for a single line in context.
#[derive(Debug)]
enum LineClass {
    UserMessage,
    ContinueUserMessage,
    AgentMarker(String),
    ThinkingMarker,
    ExecMarker,
    CodeFenceOpen(Option<String>),
    CodeFenceClose,
    DiffHeader,
    DiffContent,
    DiffExit,
    PatchBegin,
    PatchEnd,
    StatusSignal,
    Content,
}

fn classify_line(line: &str, trimmed: &str, state: &ParserState) -> LineClass {
    // Code fence close takes priority when inside a code fence
    if matches!(state, ParserState::InCodeFence { .. }) {
        if trimmed.starts_with("```") {
            return LineClass::CodeFenceClose;
        }
        return LineClass::Content;
    }

    // Patch end takes priority when inside a patch
    if matches!(state, ParserState::InPatch) {
        if line.starts_with("*** End Patch") {
            return LineClass::PatchEnd;
        }
        return LineClass::Content;
    }

    // Diff continuation/exit when inside a diff block
    if matches!(state, ParserState::InDiff) {
        if is_diff_continuation(line) {
            return LineClass::DiffContent;
        }
        // Not a diff line — exit the diff block
        // But first check if this line is itself a structural marker
        return classify_as_structural_or_exit(line, trimmed);
    }

    // User message continuation
    if matches!(state, ParserState::InUserMessage) && line.starts_with("> ") {
        return LineClass::ContinueUserMessage;
    }

    // Structural markers (checked from most specific to least)

    // Patch begin
    if line.starts_with("*** Begin Patch") {
        return LineClass::PatchBegin;
    }

    // Diff header
    if line.starts_with("diff --git") || line.starts_with("diff --cc") {
        return LineClass::DiffHeader;
    }

    // Code fence open
    if trimmed.starts_with("```") {
        let lang_hint = trimmed.trim_start_matches('`').trim();
        let lang = if lang_hint.is_empty() {
            None
        } else {
            Some(lang_hint.to_string())
        };
        return LineClass::CodeFenceOpen(lang);
    }

    // Agent markers
    if trimmed == "claude" || trimmed == "codex" || trimmed == "gemini" {
        return LineClass::AgentMarker(trimmed.to_string());
    }

    // Thinking marker
    if trimmed == "thinking" {
        return LineClass::ThinkingMarker;
    }

    // Exec marker
    if trimmed == "exec" {
        return LineClass::ExecMarker;
    }

    // Status signals
    if is_status_signal(trimmed) {
        return LineClass::StatusSignal;
    }

    // User message start
    if line.starts_with("> ") {
        return LineClass::UserMessage;
    }

    LineClass::Content
}

/// When exiting a diff block, check if the line is itself a structural marker.
fn classify_as_structural_or_exit(line: &str, trimmed: &str) -> LineClass {
    if line.starts_with("*** Begin Patch") {
        return LineClass::PatchBegin;
    }
    if line.starts_with("diff --git") || line.starts_with("diff --cc") {
        return LineClass::DiffHeader;
    }
    if trimmed.starts_with("```") {
        let lang_hint = trimmed.trim_start_matches('`').trim();
        let lang = if lang_hint.is_empty() {
            None
        } else {
            Some(lang_hint.to_string())
        };
        return LineClass::CodeFenceOpen(lang);
    }
    if trimmed == "claude" || trimmed == "codex" || trimmed == "gemini" {
        return LineClass::AgentMarker(trimmed.to_string());
    }
    if trimmed == "thinking" {
        return LineClass::ThinkingMarker;
    }
    if trimmed == "exec" {
        return LineClass::ExecMarker;
    }
    if is_status_signal(trimmed) {
        return LineClass::StatusSignal;
    }
    if line.starts_with("> ") {
        return LineClass::UserMessage;
    }
    LineClass::DiffExit
}

fn is_diff_continuation(line: &str) -> bool {
    line.starts_with('+')
        || line.starts_with('-')
        || line.starts_with(' ')
        || line.starts_with("@@")
        || line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with('\\')
        || line.is_empty()
}

fn is_status_signal(trimmed: &str) -> bool {
    let lower = trimmed.to_ascii_lowercase();
    lower.contains("[patch_ready]")
        || lower.contains("[needs_human]")
        || lower.contains("[error]")
        || lower.contains("[done]")
}

fn detect_tool_status(lines: &[String]) -> ToolStatus {
    for line in lines.iter().rev().take(3) {
        let lower = line.to_ascii_lowercase();
        if lower.contains("succeeded in") {
            return ToolStatus::Succeeded;
        }
        if lower.contains("failed in") || (lower.contains("error") && lower.contains("exit code")) {
            return ToolStatus::Failed;
        }
    }
    ToolStatus::Running
}

fn flush_block(blocks: &mut Vec<ChatBlock>, accumulator: &mut Vec<String>, state: &ParserState) {
    if accumulator.is_empty() {
        return;
    }
    let lines = std::mem::take(accumulator);
    let block = match state {
        ParserState::InUserMessage => ChatBlock::UserMessage { lines },
        ParserState::InThinking => ChatBlock::Thinking { lines },
        ParserState::InExec => {
            let status = detect_tool_status(&lines);
            ChatBlock::ToolCall {
                tool: "exec".to_string(),
                lines,
                status,
            }
        }
        ParserState::InCodeFence { lang } => ChatBlock::CodeFence {
            lang: lang.clone(),
            lines,
        },
        ParserState::InDiff | ParserState::InPatch => ChatBlock::Diff { lines },
        ParserState::Default => ChatBlock::AssistantText { lines },
    };
    blocks.push(block);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_input_produces_no_blocks() {
        assert!(parse_chat_blocks(&[]).is_empty());
    }

    #[test]
    fn plain_text_becomes_assistant_text() {
        let input = lines(&["Hello world", "How are you?"]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0],
            ChatBlock::AssistantText {
                lines: lines(&["Hello world", "How are you?"])
            }
        );
    }

    #[test]
    fn user_messages_grouped() {
        let input = lines(&["> hello", "> how are you", "I'm fine"]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0],
            ChatBlock::UserMessage {
                lines: lines(&["hello", "how are you"])
            }
        );
        assert_eq!(
            blocks[1],
            ChatBlock::AssistantText {
                lines: lines(&["I'm fine"])
            }
        );
    }

    #[test]
    fn agent_marker_detected() {
        let input = lines(&["claude", "Hello from Claude"]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0],
            ChatBlock::AgentMarker {
                agent: "claude".to_string()
            }
        );
        assert_eq!(
            blocks[1],
            ChatBlock::AssistantText {
                lines: lines(&["Hello from Claude"])
            }
        );
    }

    #[test]
    fn thinking_block_captured() {
        let input = lines(&[
            "thinking",
            "reasoning about stuff",
            "more reasoning",
            "claude",
        ]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0],
            ChatBlock::Thinking {
                lines: lines(&["reasoning about stuff", "more reasoning"])
            }
        );
        assert_eq!(
            blocks[1],
            ChatBlock::AgentMarker {
                agent: "claude".to_string()
            }
        );
    }

    #[test]
    fn exec_block_captured() {
        let input = lines(&["exec", "cargo test", "test result: ok", "claude"]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0],
            ChatBlock::ToolCall {
                tool: "exec".to_string(),
                lines: lines(&["cargo test", "test result: ok"]),
                status: ToolStatus::Running,
            }
        );
        assert_eq!(
            blocks[1],
            ChatBlock::AgentMarker {
                agent: "claude".to_string()
            }
        );
    }

    #[test]
    fn code_fence_captured_with_language() {
        let input = lines(&["some text", "```rust", "fn main() {}", "```", "more text"]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[0],
            ChatBlock::AssistantText {
                lines: lines(&["some text"])
            }
        );
        assert_eq!(
            blocks[1],
            ChatBlock::CodeFence {
                lang: Some("rust".to_string()),
                lines: lines(&["fn main() {}"])
            }
        );
        assert_eq!(
            blocks[2],
            ChatBlock::AssistantText {
                lines: lines(&["more text"])
            }
        );
    }

    #[test]
    fn code_fence_without_language() {
        let input = lines(&["```", "some code", "```"]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0],
            ChatBlock::CodeFence {
                lang: None,
                lines: lines(&["some code"])
            }
        );
    }

    #[test]
    fn diff_block_captured() {
        let input = lines(&[
            "diff --git a/foo.rs b/foo.rs",
            "index abc..def 100644",
            "--- a/foo.rs",
            "+++ b/foo.rs",
            "@@ -1,3 +1,4 @@",
            " unchanged",
            "+added",
            "-removed",
            "next prose line",
        ]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            ChatBlock::Diff { lines } => {
                assert_eq!(lines.len(), 8);
                assert!(lines[0].starts_with("diff --git"));
                assert!(lines[6].starts_with('+'));
                assert!(lines[7].starts_with('-'));
            }
            other => panic!("expected Diff, got {other:?}"),
        }
        assert_eq!(
            blocks[1],
            ChatBlock::AssistantText {
                lines: lines(&["next prose line"])
            }
        );
    }

    #[test]
    fn patch_block_captured() {
        let input = lines(&[
            "*** Begin Patch",
            "*** Update File: src/main.rs",
            "@@ fn main @@",
            "+let x = 1;",
            "*** End Patch",
            "done",
        ]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            ChatBlock::Diff { lines } => {
                assert!(lines[0].contains("Begin Patch"));
                assert!(lines.last().unwrap().contains("End Patch"));
            }
            other => panic!("expected Diff, got {other:?}"),
        }
    }

    #[test]
    fn status_signal_detected() {
        let input = lines(&["some text", "[patch_ready] done", "more text"]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[1],
            ChatBlock::StatusSignal {
                line: "[patch_ready] done".to_string()
            }
        );
    }

    #[test]
    fn mixed_sequence() {
        let input = lines(&[
            "claude",
            "thinking",
            "I need to edit the file",
            "exec",
            "cat foo.rs",
            "fn main() {}",
            "claude",
            "Here is my change:",
            "```rust",
            "fn main() { println!(\"hello\"); }",
            "```",
            "[patch_ready] complete",
        ]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 7);
        assert!(matches!(&blocks[0], ChatBlock::AgentMarker { agent } if agent == "claude"));
        assert!(matches!(&blocks[1], ChatBlock::Thinking { .. }));
        assert!(matches!(&blocks[2], ChatBlock::ToolCall { tool, .. } if tool == "exec"));
        assert!(matches!(&blocks[3], ChatBlock::AgentMarker { agent } if agent == "claude"));
        assert!(matches!(&blocks[4], ChatBlock::AssistantText { .. }));
        assert!(matches!(&blocks[5], ChatBlock::CodeFence { lang: Some(l), .. } if l == "rust"));
        assert!(matches!(&blocks[6], ChatBlock::StatusSignal { .. }));
    }

    #[test]
    fn diff_block_transitions_to_new_structural_marker() {
        let input = lines(&["diff --git a/x b/x", "+added", "exec", "cargo build"]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], ChatBlock::Diff { .. }));
        assert!(matches!(&blocks[1], ChatBlock::ToolCall { .. }));
    }

    #[test]
    fn consecutive_agent_markers() {
        let input = lines(&["claude", "codex"]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], ChatBlock::AgentMarker { agent } if agent == "claude"));
        assert!(matches!(&blocks[1], ChatBlock::AgentMarker { agent } if agent == "codex"));
    }

    #[test]
    fn empty_lines_preserved_in_assistant_text() {
        let input = lines(&["hello", "", "world"]);
        let blocks = parse_chat_blocks(&input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0],
            ChatBlock::AssistantText {
                lines: lines(&["hello", "", "world"])
            }
        );
    }

    #[test]
    fn tool_call_detects_succeeded_status() {
        let input = lines(&[
            "exec",
            "cargo test",
            "\u{23bf}  Finished in 2.3s",
            "\u{23bf}  cargo test succeeded in 2.3s",
            "claude",
        ]);
        let blocks = parse_chat_blocks(&input);
        match &blocks[0] {
            ChatBlock::ToolCall { status, .. } => assert_eq!(*status, ToolStatus::Succeeded),
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_detects_failed_status() {
        let input = lines(&[
            "exec",
            "cargo test",
            "\u{23bf}  cargo test failed in 1.2s",
            "claude",
        ]);
        let blocks = parse_chat_blocks(&input);
        match &blocks[0] {
            ChatBlock::ToolCall { status, .. } => assert_eq!(*status, ToolStatus::Failed),
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_defaults_to_running_when_no_status() {
        let input = lines(&["exec", "cargo test", "running...", "claude"]);
        let blocks = parse_chat_blocks(&input);
        match &blocks[0] {
            ChatBlock::ToolCall { status, .. } => assert_eq!(*status, ToolStatus::Running),
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }
}
