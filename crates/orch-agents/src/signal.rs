use chrono::Utc;

use crate::types::{AgentSignal, AgentSignalKind};

pub fn detect_common_signal(line: &str) -> Option<AgentSignal> {
    let lower = line.to_ascii_lowercase();
    let kind = if lower.contains("needs_human")
        || lower.contains("need_human")
        || lower.contains("[need_human]")
        || lower.contains("[needs_human]")
    {
        Some(AgentSignalKind::NeedHuman)
    } else if lower.contains("patch_ready")
        || lower.contains("[patch_ready]")
        || lower.contains("ready for review")
    {
        Some(AgentSignalKind::PatchReady)
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
        let signal =
            detect_common_signal("all done, ready for review").expect("patch ready signal");
        assert_eq!(signal.kind, AgentSignalKind::PatchReady);
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
}
