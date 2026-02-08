use chrono::{DateTime, Utc};
use orch_core::types::TaskId;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
pub struct InferredStackDependency {
    pub parent_branch: String,
    pub child_branch: String,
    pub parent_task_id: TaskId,
    pub child_task_id: TaskId,
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

pub fn infer_task_dependencies_from_stack(
    snapshot: &GraphiteStackSnapshot,
    branch_to_task: &HashMap<String, TaskId>,
) -> Vec<InferredStackDependency> {
    let branches = snapshot
        .nodes
        .iter()
        .filter_map(|node| {
            node.branch
                .as_ref()
                .map(|branch| (branch.clone(), node.depth_hint))
        })
        .collect::<Vec<_>>();
    if branches.len() < 2 {
        return Vec::new();
    }

    let depth_variants = branches
        .iter()
        .map(|(_, depth)| *depth)
        .collect::<HashSet<_>>();

    let branch_edges = if depth_variants.len() <= 1 {
        infer_edges_linear(&branches)
    } else {
        infer_edges_by_depth(&branches)
    };

    to_task_dependencies(branch_edges, branch_to_task)
}

fn parse_stack_line(line: &str) -> StackNode {
    let depth_hint = stack_depth_hint(line);
    let is_current = line.contains('*');
    let branch = extract_branch_token(line);

    StackNode {
        raw_line: line.to_string(),
        branch,
        depth_hint,
        is_current,
    }
}

fn stack_depth_hint(line: &str) -> usize {
    let leading_space_depth = line.chars().take_while(|c| c.is_whitespace()).count();

    let branch_char_idx = line
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| c == ',' || c == ':' || c == ';'))
        .find(|token| looks_like_branch_token(token))
        .and_then(|token| line.find(token));

    branch_char_idx.unwrap_or(leading_space_depth)
}

fn extract_branch_token(line: &str) -> Option<String> {
    line.split_whitespace()
        .map(|token| token.trim_matches(|c: char| c == ',' || c == ':' || c == ';'))
        .find(|token| looks_like_branch_token(token))
        .map(normalize_branch_name)
}

fn normalize_branch_name(branch: &str) -> String {
    branch.trim().trim_start_matches("refs/heads/").to_string()
}

fn looks_like_branch_token(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if token.starts_with('#') {
        return false;
    }
    if token.eq_ignore_ascii_case("gt") || token.eq_ignore_ascii_case("graphite") {
        return false;
    }
    if token
        .chars()
        .all(|c| matches!(c, '|' | '/' | '\\' | '-' | '*' | 'o' | 'O'))
    {
        return false;
    }
    token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
}

fn infer_edges_linear(branches: &[(String, usize)]) -> Vec<(String, String)> {
    let mut edges = Vec::new();
    for pair in branches.windows(2) {
        let parent = pair[0].0.clone();
        let child = pair[1].0.clone();
        if parent != child {
            edges.push((parent, child));
        }
    }
    edges
}

fn infer_edges_by_depth(branches: &[(String, usize)]) -> Vec<(String, String)> {
    let mut stack: Vec<(String, usize)> = Vec::new();
    let mut edges = Vec::<(String, String)>::new();

    for (branch, depth) in branches {
        while let Some((_, parent_depth)) = stack.last() {
            if *parent_depth < *depth {
                break;
            }
            stack.pop();
        }

        if let Some((parent_branch, _)) = stack.last() {
            if parent_branch != branch {
                edges.push((parent_branch.clone(), branch.clone()));
            }
        }

        stack.push((branch.clone(), *depth));
    }

    edges
}

fn to_task_dependencies(
    edges: Vec<(String, String)>,
    branch_to_task: &HashMap<String, TaskId>,
) -> Vec<InferredStackDependency> {
    let normalized_map = branch_to_task
        .iter()
        .map(|(branch, task)| (normalize_branch_name(branch), task.clone()))
        .collect::<HashMap<_, _>>();

    let mut seen = HashSet::<(TaskId, TaskId)>::new();
    let mut out = Vec::new();

    for (parent_branch, child_branch) in edges {
        let normalized_parent = normalize_branch_name(&parent_branch);
        let normalized_child = normalize_branch_name(&child_branch);

        let Some(parent_task_id) = normalized_map.get(&normalized_parent).cloned() else {
            continue;
        };
        let Some(child_task_id) = normalized_map.get(&normalized_child).cloned() else {
            continue;
        };

        if parent_task_id == child_task_id {
            continue;
        }
        if !seen.insert((parent_task_id.clone(), child_task_id.clone())) {
            continue;
        }

        out.push(InferredStackDependency {
            parent_branch: normalized_parent,
            child_branch: normalized_child,
            parent_task_id,
            child_task_id,
        });
    }

    out.sort_by(|a, b| {
        a.parent_task_id
            .0
            .cmp(&b.parent_task_id.0)
            .then_with(|| a.child_task_id.0.cmp(&b.child_task_id.0))
    });
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use orch_core::types::TaskId;

    use super::{infer_task_dependencies_from_stack, parse_gt_log_short};

    #[test]
    fn infers_linear_dependencies_when_depth_is_uniform() {
        let raw = r#"
stack/root
stack/child-a
stack/child-b
"#;
        let snapshot = parse_gt_log_short(raw);
        let mapping = HashMap::from([
            ("stack/root".to_string(), TaskId("T1".to_string())),
            ("stack/child-a".to_string(), TaskId("T2".to_string())),
            ("stack/child-b".to_string(), TaskId("T3".to_string())),
        ]);
        let inferred = infer_task_dependencies_from_stack(&snapshot, &mapping);
        let ids = inferred
            .into_iter()
            .map(|x| (x.parent_task_id.0, x.child_task_id.0))
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                ("T1".to_string(), "T2".to_string()),
                ("T2".to_string(), "T3".to_string())
            ]
        );
    }

    #[test]
    fn infers_tree_dependencies_when_depth_varies() {
        let raw = r#"
root/main
  feature/api
    feature/web
  feature/docs
"#;
        let snapshot = parse_gt_log_short(raw);
        let mapping = HashMap::from([
            ("root/main".to_string(), TaskId("T1".to_string())),
            ("feature/api".to_string(), TaskId("T2".to_string())),
            ("feature/web".to_string(), TaskId("T3".to_string())),
            ("feature/docs".to_string(), TaskId("T4".to_string())),
        ]);
        let inferred = infer_task_dependencies_from_stack(&snapshot, &mapping);
        let ids = inferred
            .into_iter()
            .map(|x| (x.parent_task_id.0, x.child_task_id.0))
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                ("T1".to_string(), "T2".to_string()),
                ("T1".to_string(), "T4".to_string()),
                ("T2".to_string(), "T3".to_string())
            ]
        );
    }

    #[test]
    fn normalizes_refs_heads_in_mapping_and_stack_lines() {
        let raw = r#"
refs/heads/parent
  refs/heads/child
"#;
        let snapshot = parse_gt_log_short(raw);
        let mapping = HashMap::from([
            ("parent".to_string(), TaskId("T9".to_string())),
            ("refs/heads/child".to_string(), TaskId("T10".to_string())),
        ]);
        let inferred = infer_task_dependencies_from_stack(&snapshot, &mapping);
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].parent_task_id.0, "T9");
        assert_eq!(inferred[0].child_task_id.0, "T10");
    }
}
