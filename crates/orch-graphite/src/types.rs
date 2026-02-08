use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphiteStatusSnapshot {
    pub captured_at: DateTime<Utc>,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackNode {
    pub raw_line: String,
    pub branch: Option<String>,
    pub depth_hint: usize,
    pub is_current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphiteStackSnapshot {
    pub captured_at: DateTime<Utc>,
    pub nodes: Vec<StackNode>,
}

pub fn parse_gt_log_short(raw: &str) -> GraphiteStackSnapshot {
    let nodes = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_stack_line)
        .collect::<Vec<_>>();

    GraphiteStackSnapshot {
        captured_at: Utc::now(),
        nodes,
    }
}

fn parse_stack_line(line: &str) -> StackNode {
    let depth_hint = line
        .chars()
        .take_while(|c| c.is_whitespace())
        .count();
    let is_current = line.contains('*');
    let branch = extract_branch_token(line);

    StackNode {
        raw_line: line.to_string(),
        branch,
        depth_hint,
        is_current,
    }
}

fn extract_branch_token(line: &str) -> Option<String> {
    line.split_whitespace()
        .find(|token| looks_like_branch_token(token))
        .map(|token| token.trim_matches(|c: char| c == ',' || c == ':' || c == ';'))
        .map(ToString::to_string)
}

fn looks_like_branch_token(token: &str) -> bool {
    if token.starts_with('#') {
        return false;
    }
    if token.eq_ignore_ascii_case("gt") || token.eq_ignore_ascii_case("graphite") {
        return false;
    }
    token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
}
