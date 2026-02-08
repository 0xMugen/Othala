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
