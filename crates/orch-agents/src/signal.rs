use chrono::Utc;

use crate::types::{AgentSignal, AgentSignalKind};

/// Returns true for lines that are part of diff or structured output (not agent prose).
fn is_structured_output_line(line: &str) -> bool {
    line.starts_with("diff --")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("@@ ")
        || line.starts_with("@@@ ")
        || line.starts_with("*** ")
        || line.starts_with("index ")
}

pub fn detect_common_signal(line: &str) -> Option<AgentSignal> {
    // Skip diff / structured output lines — they may contain signal-like substrings.
    if is_structured_output_line(line) {
        return None;
    }

    let lower = line.to_ascii_lowercase();

    // Skip prompt echo lines — agent startup echoes instructions containing signal markers.
    if lower.contains("print exactly") || lower.contains("print [") {
        return None;
    }

    let kind = if lower.contains("[needs_human]") || lower.contains("[need_human]") {
        Some(AgentSignalKind::NeedHuman)
    } else if lower.contains("[patch_ready]") {
        Some(AgentSignalKind::PatchReady)
    } else if lower.contains("[conflict_resolved]") {
        Some(AgentSignalKind::ConflictResolved)
    } else if lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("too many requests")
    {
        Some(AgentSignalKind::RateLimited)
    } else if lower.contains("error:") || lower.contains("fatal:") || lower.contains("traceback") {
        Some(AgentSignalKind::ErrorHint)
    } else {
        None
    };

    kind.map(|kind| AgentSignal {
        kind,
        at: Utc::now(),
        message: line.trim().to_string(),
        source_line: line.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use crate::types::AgentSignalKind;

    use super::detect_common_signal;

    #[test]
    fn detects_need_human_variants() {
        let signal = detect_common_signal("status: [needs_human] reviewer input required")
            .expect("need human signal");
        assert_eq!(signal.kind, AgentSignalKind::NeedHuman);
        assert_eq!(
            signal.message,
            "status: [needs_human] reviewer input required"
        );
    }

    #[test]
    fn detects_patch_ready_variants() {
        let signal = detect_common_signal("status: [patch_ready] all changes applied")
            .expect("patch ready signal");
        assert_eq!(signal.kind, AgentSignalKind::PatchReady);
    }

    #[test]
    fn detects_conflict_resolved_variants() {
        let signal = detect_common_signal("done: [conflict_resolved] all conflicts fixed")
            .expect("conflict resolved signal");
        assert_eq!(signal.kind, AgentSignalKind::ConflictResolved);
    }

    #[test]
    fn detects_rate_limited_variants() {
        let signal =
            detect_common_signal("429 too many requests from provider").expect("rate limit signal");
        assert_eq!(signal.kind, AgentSignalKind::RateLimited);
    }

    #[test]
    fn detects_error_hint_variants() {
        let signal = detect_common_signal("fatal: failed to apply patch").expect("error signal");
        assert_eq!(signal.kind, AgentSignalKind::ErrorHint);
    }

    #[test]
    fn returns_none_for_non_signal_output() {
        let signal = detect_common_signal("progress: compiling crates");
        assert!(signal.is_none());
    }

    #[test]
    fn ignores_prompt_echo_lines_containing_signal_markers() {
        assert!(detect_common_signal("print exactly [conflict_resolved]").is_none());
        assert!(detect_common_signal("print exactly [needs_human] with a short reason.").is_none());
        assert!(detect_common_signal(
            "When implementation is complete, print exactly [patch_ready]."
        )
        .is_none());
        assert!(detect_common_signal("[stderr] print [conflict_resolved]").is_none());
    }

    #[test]
    fn prioritizes_need_human_over_other_markers_in_same_line() {
        let signal = detect_common_signal(
            "status: [needs_human] and [patch_ready] but fatal: waiting for human",
        )
        .expect("need human signal");
        assert_eq!(signal.kind, AgentSignalKind::NeedHuman);
    }

    #[test]
    fn signal_message_is_trimmed_but_source_line_is_preserved() {
        let raw = "   TRACEBACK: something failed   ";
        let signal = detect_common_signal(raw).expect("error hint signal");
        assert_eq!(signal.kind, AgentSignalKind::ErrorHint);
        assert_eq!(signal.message, "TRACEBACK: something failed");
        assert_eq!(signal.source_line, raw);
    }

    #[test]
    fn prioritizes_patch_ready_over_rate_limit_markers() {
        let signal =
            detect_common_signal("status: [patch_ready] but got rate limit warning afterwards")
                .expect("signal");
        assert_eq!(signal.kind, AgentSignalKind::PatchReady);
    }

    #[test]
    fn skips_diff_header_lines() {
        assert!(detect_common_signal("diff --git a/error.rs b/error.rs").is_none());
    }

    #[test]
    fn skips_diff_context_lines() {
        assert!(detect_common_signal("--- a/file_with_error.rs").is_none());
        assert!(detect_common_signal("+++ b/file_with_error.rs").is_none());
        assert!(detect_common_signal("@@ -1,3 +1,4 @@ fn error_handler").is_none());
        assert!(detect_common_signal("index abc1234..def5678 100644").is_none());
        assert!(detect_common_signal("*** Begin Patch error: something").is_none());
    }

    #[test]
    fn bare_signal_words_no_longer_match() {
        assert!(detect_common_signal("the needs_human flag is set").is_none());
        assert!(detect_common_signal("patch_ready variable was true").is_none());
        assert!(detect_common_signal("conflict_resolved in the merge").is_none());
    }

    #[test]
    fn bracket_signals_still_match() {
        let signal =
            detect_common_signal("status: [needs_human] blocked").expect("need human signal");
        assert_eq!(signal.kind, AgentSignalKind::NeedHuman);

        let signal = detect_common_signal("done: [patch_ready]").expect("patch ready signal");
        assert_eq!(signal.kind, AgentSignalKind::PatchReady);

        let signal =
            detect_common_signal("ok: [conflict_resolved]").expect("conflict resolved signal");
        assert_eq!(signal.kind, AgentSignalKind::ConflictResolved);
    }
}
