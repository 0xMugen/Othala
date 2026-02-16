use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

type GraphBuild = (Vec<Vec<usize>>, Vec<usize>, usize);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStrategy {
    Sequential,
    Parallel,
    Conditional,
}

#[allow(clippy::derivable_impls)]
impl Default for DelegationStrategy {
    fn default() -> Self {
        DelegationStrategy::Sequential
    }
}

impl std::fmt::Display for DelegationStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DelegationStrategy::Sequential => f.write_str("sequential"),
            DelegationStrategy::Parallel => f.write_str("parallel"),
            DelegationStrategy::Conditional => f.write_str("conditional"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubTaskSpec {
    pub title: String,
    pub description: String,
    pub model: Option<String>,
    pub priority: Option<String>,
    pub depends_on: Vec<String>,
    pub files: Vec<String>,
    pub verify_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationPlan {
    pub parent_task_id: String,
    pub strategy: DelegationStrategy,
    pub subtasks: Vec<SubTaskSpec>,
    pub created_at: DateTime<Utc>,
    pub max_parallel: usize,
    pub fail_fast: bool,
    pub timeout_secs: Option<u64>,
}

impl DelegationPlan {
    pub fn new(parent_id: &str) -> Self {
        Self {
            parent_task_id: parent_id.to_string(),
            strategy: DelegationStrategy::default(),
            subtasks: Vec::new(),
            created_at: Utc::now(),
            max_parallel: 1,
            fail_fast: true,
            timeout_secs: None,
        }
    }

    pub fn add_subtask(&mut self, spec: SubTaskSpec) {
        self.subtasks.push(spec);
    }

    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        let mut title_to_idx = HashMap::<String, usize>::new();

        for (idx, subtask) in self.subtasks.iter().enumerate() {
            let title = subtask.title.trim();
            if title.is_empty() {
                errors.push(format!("subtask {idx} has empty title"));
                continue;
            }

            if title_to_idx.insert(title.to_string(), idx).is_some() {
                errors.push(format!("duplicate subtask title: {title}"));
            }
        }

        for (idx, subtask) in self.subtasks.iter().enumerate() {
            for dep in &subtask.depends_on {
                if !title_to_idx.contains_key(dep.as_str()) {
                    errors.push(format!(
                        "subtask {idx} depends on missing subtask title: {dep}"
                    ));
                }
            }
        }

        if let Ok((_, _, unresolved)) = build_graph(self) {
            if unresolved > 0 {
                errors.push("cyclic dependencies detected".to_string());
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn execution_order(&self) -> Result<Vec<Vec<usize>>, String> {
        if let Err(errors) = self.validate() {
            return Err(errors.join("; "));
        }

        let (children, mut indegree, unresolved) =
            build_graph(self).map_err(|e| format!("failed to build graph: {e}"))?;
        if unresolved > 0 {
            return Err("cyclic dependencies detected".to_string());
        }

        let mut remaining = HashSet::<usize>::from_iter(0..self.subtasks.len());
        let mut waves = Vec::<Vec<usize>>::new();

        while !remaining.is_empty() {
            let mut ready = remaining
                .iter()
                .copied()
                .filter(|idx| indegree[*idx] == 0)
                .collect::<Vec<_>>();
            ready.sort_unstable();

            if ready.is_empty() {
                return Err("cyclic dependencies detected".to_string());
            }

            for idx in &ready {
                remaining.remove(idx);
            }

            for idx in &ready {
                for child in &children[*idx] {
                    indegree[*child] -= 1;
                }
            }

            match self.strategy {
                DelegationStrategy::Sequential => {
                    waves.extend(ready.into_iter().map(|idx| vec![idx]));
                }
                DelegationStrategy::Parallel | DelegationStrategy::Conditional => {
                    let chunk_size = self.max_parallel.max(1);
                    for chunk in ready.chunks(chunk_size) {
                        waves.push(chunk.to_vec());
                    }
                }
            }
        }

        Ok(waves)
    }

    pub fn summary(&self) -> String {
        let mut out = format!(
            "Delegation plan for {}: {} subtask(s), strategy={}, max_parallel={}, fail_fast={}",
            self.parent_task_id,
            self.subtasks.len(),
            self.strategy,
            self.max_parallel,
            self.fail_fast
        );

        if let Some(timeout) = self.timeout_secs {
            out.push_str(&format!(", timeout={}s", timeout));
        }

        for (idx, subtask) in self.subtasks.iter().enumerate() {
            let deps = if subtask.depends_on.is_empty() {
                "none".to_string()
            } else {
                subtask.depends_on.join(", ")
            };
            out.push_str(&format!("\n{}: {} (deps: {})", idx, subtask.title, deps));
        }

        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubTaskStatus {
    Pending,
    Running,
    Completed,
    Failed(String),
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationTracker {
    pub plan: DelegationPlan,
    pub statuses: HashMap<usize, SubTaskStatus>,
    pub task_id_mapping: HashMap<usize, String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl DelegationTracker {
    pub fn new(plan: DelegationPlan) -> Self {
        let mut statuses = HashMap::new();
        for idx in 0..plan.subtasks.len() {
            statuses.insert(idx, SubTaskStatus::Pending);
        }

        Self {
            plan,
            statuses,
            task_id_mapping: HashMap::new(),
            started_at: None,
            completed_at: None,
        }
    }

    pub fn start_subtask(&mut self, idx: usize, task_id: &str) {
        if idx >= self.plan.subtasks.len() {
            return;
        }

        if self.started_at.is_none() {
            self.started_at = Some(Utc::now());
        }

        self.statuses.insert(idx, SubTaskStatus::Running);
        self.task_id_mapping.insert(idx, task_id.to_string());
    }

    pub fn complete_subtask(&mut self, idx: usize) {
        if idx >= self.plan.subtasks.len() {
            return;
        }

        self.statuses.insert(idx, SubTaskStatus::Completed);
        if self.is_complete() {
            self.completed_at = Some(Utc::now());
        }
    }

    pub fn fail_subtask(&mut self, idx: usize, reason: &str) {
        if idx >= self.plan.subtasks.len() {
            return;
        }

        self.statuses
            .insert(idx, SubTaskStatus::Failed(reason.to_string()));
        if self.is_complete() {
            self.completed_at = Some(Utc::now());
        }
    }

    pub fn skip_subtask(&mut self, idx: usize) {
        if idx >= self.plan.subtasks.len() {
            return;
        }

        self.statuses.insert(idx, SubTaskStatus::Skipped);
        if self.is_complete() {
            self.completed_at = Some(Utc::now());
        }
    }

    pub fn next_runnable(&self) -> Vec<usize> {
        if self.plan.fail_fast && self.is_failed() {
            return Vec::new();
        }

        let mut title_to_idx = HashMap::<&str, usize>::new();
        for (idx, subtask) in self.plan.subtasks.iter().enumerate() {
            title_to_idx.insert(subtask.title.as_str(), idx);
        }

        let mut runnable = Vec::new();
        for (idx, subtask) in self.plan.subtasks.iter().enumerate() {
            let status = self.statuses.get(&idx).unwrap_or(&SubTaskStatus::Pending);
            if !matches!(status, SubTaskStatus::Pending) {
                continue;
            }
            if self.task_id_mapping.contains_key(&idx) {
                continue;
            }

            let deps_met = subtask.depends_on.iter().all(|dep| {
                title_to_idx
                    .get(dep.as_str())
                    .and_then(|dep_idx| self.statuses.get(dep_idx))
                    .is_some_and(|dep_status| matches!(dep_status, SubTaskStatus::Completed))
            });

            if deps_met {
                runnable.push(idx);
            }
        }

        runnable
    }

    pub fn is_complete(&self) -> bool {
        (0..self.plan.subtasks.len()).all(|idx| {
            let status = self.statuses.get(&idx).unwrap_or(&SubTaskStatus::Pending);
            !matches!(status, SubTaskStatus::Pending | SubTaskStatus::Running)
        })
    }

    pub fn is_failed(&self) -> bool {
        self.statuses
            .values()
            .any(|status| matches!(status, SubTaskStatus::Failed(_)))
    }

    pub fn progress_summary(&self) -> String {
        let total = self.plan.subtasks.len();
        let mut complete = 0usize;
        let mut running = 0usize;
        let mut pending = 0usize;
        let mut failed = 0usize;
        let mut skipped = 0usize;

        for idx in 0..total {
            match self.statuses.get(&idx).unwrap_or(&SubTaskStatus::Pending) {
                SubTaskStatus::Pending => pending += 1,
                SubTaskStatus::Running => running += 1,
                SubTaskStatus::Completed => complete += 1,
                SubTaskStatus::Failed(_) => failed += 1,
                SubTaskStatus::Skipped => skipped += 1,
            }
        }

        let mut out = format!(
            "{}/{} complete, {} running, {} pending",
            complete, total, running, pending
        );
        if failed > 0 {
            out.push_str(&format!(", {} failed", failed));
        }
        if skipped > 0 {
            out.push_str(&format!(", {} skipped", skipped));
        }
        out
    }

    pub fn all_results(&self) -> Vec<(usize, &SubTaskStatus)> {
        let mut out = self.statuses.iter().map(|(idx, status)| (*idx, status)).collect::<Vec<_>>();
        out.sort_by_key(|(idx, _)| *idx);
        out
    }
}

pub fn parse_delegation_from_agent_output(output: &str) -> Option<DelegationPlan> {
    if let Some((prefix, candidate)) = extract_delegate_json_candidate(output) {
        if let Some(plan) = parse_plan_candidate(candidate, prefix) {
            return Some(plan);
        }
    }

    if let Some(candidate) = extract_first_json_object(output) {
        if let Some(plan) = parse_plan_candidate(candidate, "parent") {
            return Some(plan);
        }
    }

    let mut markdown_titles = Vec::<String>::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(title) = trimmed
            .strip_prefix("- [ ] ")
            .or_else(|| trimmed.strip_prefix("* [ ] "))
        {
            let title = title.trim();
            if !title.is_empty() {
                markdown_titles.push(title.to_string());
            }
        }
    }

    if markdown_titles.is_empty() {
        return None;
    }

    let mut plan = DelegationPlan::new("parent");
    for title in markdown_titles {
        plan.add_subtask(SubTaskSpec {
            title: title.clone(),
            description: title,
            model: None,
            priority: None,
            depends_on: Vec::new(),
            files: Vec::new(),
            verify_command: None,
        });
    }

    if plan.validate().is_ok() {
        Some(plan)
    } else {
        None
    }
}

pub fn format_delegation_prompt(plan: &DelegationPlan) -> String {
    let mut out = String::new();
    out.push_str("Delegation Plan\n");
    out.push_str(&format!("Parent Task: {}\n", plan.parent_task_id));
    out.push_str(&format!("Strategy: {}\n", plan.strategy));
    out.push_str(&format!("Max Parallel: {}\n", plan.max_parallel));
    out.push_str(&format!("Fail Fast: {}\n", plan.fail_fast));
    if let Some(timeout) = plan.timeout_secs {
        out.push_str(&format!("Timeout: {}s\n", timeout));
    }
    out.push_str("Subtasks:\n");

    for (idx, subtask) in plan.subtasks.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", idx + 1, subtask.title));
        if !subtask.description.is_empty() {
            out.push_str(&format!("   - Description: {}\n", subtask.description));
        }
        if !subtask.depends_on.is_empty() {
            out.push_str(&format!("   - Depends On: {}\n", subtask.depends_on.join(", ")));
        }
        if !subtask.files.is_empty() {
            out.push_str(&format!("   - Files: {}\n", subtask.files.join(", ")));
        }
        if let Some(model) = &subtask.model {
            out.push_str(&format!("   - Model: {}\n", model));
        }
        if let Some(priority) = &subtask.priority {
            out.push_str(&format!("   - Priority: {}\n", priority));
        }
        if let Some(cmd) = &subtask.verify_command {
            out.push_str(&format!("   - Verify: {}\n", cmd));
        }
    }

    out.trim_end().to_string()
}

fn build_graph(plan: &DelegationPlan) -> Result<GraphBuild, String> {
    let mut title_to_idx = HashMap::<&str, usize>::new();
    for (idx, subtask) in plan.subtasks.iter().enumerate() {
        let title = subtask.title.trim();
        if title.is_empty() {
            continue;
        }
        if title_to_idx.insert(title, idx).is_some() {
            return Err(format!("duplicate subtask title: {title}"));
        }
    }

    let mut children = vec![Vec::<usize>::new(); plan.subtasks.len()];
    let mut indegree = vec![0usize; plan.subtasks.len()];

    for (idx, subtask) in plan.subtasks.iter().enumerate() {
        for dep in &subtask.depends_on {
            let dep_idx = *title_to_idx
                .get(dep.as_str())
                .ok_or_else(|| format!("missing dependency: {dep}"))?;
            children[dep_idx].push(idx);
            indegree[idx] += 1;
        }
    }

    let mut indegree_copy = indegree.clone();
    let mut resolved = 0usize;
    let mut queue = (0..plan.subtasks.len())
        .filter(|idx| indegree_copy[*idx] == 0)
        .collect::<Vec<_>>();

    while let Some(idx) = queue.pop() {
        resolved += 1;
        for child in &children[idx] {
            indegree_copy[*child] -= 1;
            if indegree_copy[*child] == 0 {
                queue.push(*child);
            }
        }
    }

    Ok((children, indegree, plan.subtasks.len() - resolved))
}

fn extract_delegate_json_candidate(output: &str) -> Option<(&str, &str)> {
    let marker = "DELEGATE:";
    let pos = output.find(marker)?;
    let candidate = output[pos + marker.len()..].trim();
    Some((&output[..pos], candidate))
}

fn parse_plan_candidate(candidate: &str, fallback_parent: &str) -> Option<DelegationPlan> {
    let json_text = if candidate.starts_with('{') {
        extract_first_json_object(candidate).unwrap_or(candidate)
    } else {
        extract_first_json_object(candidate)?
    };
    let value = serde_json::from_str::<serde_json::Value>(json_text).ok()?;

    if let Ok(plan) = serde_json::from_value::<DelegationPlan>(value.clone()) {
        if plan.validate().is_ok() {
            return Some(plan);
        }
    }

    let subtasks_value = value.get("subtasks")?;
    let subtasks = subtasks_value.as_array()?;
    if subtasks.is_empty() {
        return None;
    }

    let parent_id = value
        .get("parent_task_id")
        .and_then(|v| v.as_str())
        .unwrap_or(fallback_parent);
    let mut plan = DelegationPlan::new(parent_id);

    if let Some(strategy) = value.get("strategy").and_then(|v| v.as_str()) {
        plan.strategy = match strategy {
            "sequential" => DelegationStrategy::Sequential,
            "parallel" => DelegationStrategy::Parallel,
            "conditional" => DelegationStrategy::Conditional,
            _ => DelegationStrategy::Sequential,
        };
    }
    if let Some(max_parallel) = value.get("max_parallel").and_then(|v| v.as_u64()) {
        plan.max_parallel = usize::try_from(max_parallel).unwrap_or(usize::MAX).max(1);
    }
    if let Some(fail_fast) = value.get("fail_fast").and_then(|v| v.as_bool()) {
        plan.fail_fast = fail_fast;
    }
    if let Some(timeout_secs) = value.get("timeout_secs").and_then(|v| v.as_u64()) {
        plan.timeout_secs = Some(timeout_secs);
    }

    for subtask in subtasks {
        let title = subtask.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let description = subtask
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let depends_on = subtask
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let files = subtask
            .get("files")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        plan.add_subtask(SubTaskSpec {
            title: title.to_string(),
            description: description.to_string(),
            model: subtask
                .get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            priority: subtask
                .get("priority")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            depends_on,
            files,
            verify_command: subtask
                .get("verify_command")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        });
    }

    if plan.validate().is_ok() {
        Some(plan)
    } else {
        None
    }
}

fn extract_first_json_object(input: &str) -> Option<&str> {
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            continue;
        }

        if ch == '{' {
            if start.is_none() {
                start = Some(idx);
            }
            depth += 1;
            continue;
        }

        if ch == '}' && depth > 0 {
            depth -= 1;
            if depth == 0 {
                if let Some(start_idx) = start {
                    return Some(&input[start_idx..=idx]);
                }
                return None;
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subtask(title: &str, depends_on: &[&str]) -> SubTaskSpec {
        SubTaskSpec {
            title: title.to_string(),
            description: format!("Work on {title}"),
            model: None,
            priority: None,
            depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
            files: Vec::new(),
            verify_command: None,
        }
    }

    #[test]
    fn new_plan_has_defaults() {
        let plan = DelegationPlan::new("T-parent");
        assert_eq!(plan.parent_task_id, "T-parent");
        assert_eq!(plan.strategy, DelegationStrategy::Sequential);
        assert_eq!(plan.max_parallel, 1);
        assert!(plan.fail_fast);
        assert!(plan.timeout_secs.is_none());
        assert!(plan.subtasks.is_empty());
    }

    #[test]
    fn add_subtask_works() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.add_subtask(subtask("one", &[]));
        assert_eq!(plan.subtasks.len(), 1);
        assert_eq!(plan.subtasks[0].title, "one");
    }

    #[test]
    fn validate_catches_empty_title() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.add_subtask(subtask("", &[]));

        let errors = plan.validate().expect_err("expected errors");
        assert!(errors.iter().any(|e| e.contains("empty title")));
    }

    #[test]
    fn validate_catches_cyclic_deps() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.add_subtask(subtask("a", &["b"]));
        plan.add_subtask(subtask("b", &["a"]));

        let errors = plan.validate().expect_err("expected cycle");
        assert!(errors.iter().any(|e| e.contains("cyclic")));
    }

    #[test]
    fn execution_order_sequential() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.add_subtask(subtask("a", &[]));
        plan.add_subtask(subtask("b", &[]));
        plan.add_subtask(subtask("c", &[]));

        let order = plan.execution_order().expect("order");
        assert_eq!(order, vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn execution_order_with_deps() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.strategy = DelegationStrategy::Parallel;
        plan.max_parallel = 10;
        plan.add_subtask(subtask("a", &[]));
        plan.add_subtask(subtask("b", &["a"]));
        plan.add_subtask(subtask("c", &["a"]));

        let order = plan.execution_order().expect("order");
        assert_eq!(order, vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn tracker_next_runnable() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.add_subtask(subtask("a", &[]));
        plan.add_subtask(subtask("b", &["a"]));

        let mut tracker = DelegationTracker::new(plan);
        assert_eq!(tracker.next_runnable(), vec![0]);

        tracker.start_subtask(0, "ST-1");
        tracker.complete_subtask(0);
        assert_eq!(tracker.next_runnable(), vec![1]);
    }

    #[test]
    fn tracker_complete_flow() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.add_subtask(subtask("a", &[]));

        let mut tracker = DelegationTracker::new(plan);
        tracker.start_subtask(0, "ST-1");
        assert!(!tracker.is_complete());
        tracker.complete_subtask(0);
        assert!(tracker.is_complete());
        assert!(tracker.completed_at.is_some());
    }

    #[test]
    fn tracker_fail_fast_behavior() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.fail_fast = true;
        plan.add_subtask(subtask("a", &[]));
        plan.add_subtask(subtask("b", &[]));

        let mut tracker = DelegationTracker::new(plan);
        tracker.fail_subtask(0, "boom");

        assert!(tracker.is_failed());
        assert!(tracker.next_runnable().is_empty());
    }

    #[test]
    fn progress_summary_formatting() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.add_subtask(subtask("a", &[]));
        plan.add_subtask(subtask("b", &[]));
        plan.add_subtask(subtask("c", &[]));

        let mut tracker = DelegationTracker::new(plan);
        tracker.complete_subtask(0);
        tracker.start_subtask(1, "ST-2");

        assert_eq!(tracker.progress_summary(), "1/3 complete, 1 running, 1 pending");
    }

    #[test]
    fn parse_delegation_from_agent_output_with_json() {
        let output = r#"Agent says:
DELEGATE: {
  "parent_task_id": "T-parent",
  "strategy": "parallel",
  "subtasks": [
    {"title": "a", "description": "A", "depends_on": []},
    {"title": "b", "description": "B", "depends_on": ["a"]}
  ]
}
"#;

        let plan = parse_delegation_from_agent_output(output).expect("parsed plan");
        assert_eq!(plan.parent_task_id, "T-parent");
        assert_eq!(plan.strategy, DelegationStrategy::Parallel);
        assert_eq!(plan.subtasks.len(), 2);
    }

    #[test]
    fn parse_delegation_from_agent_output_returns_none_for_no_delegation() {
        assert!(parse_delegation_from_agent_output("No work decomposition provided").is_none());
    }

    #[test]
    fn all_results_returns_sorted_indices() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.add_subtask(subtask("a", &[]));
        plan.add_subtask(subtask("b", &[]));
        let mut tracker = DelegationTracker::new(plan);
        tracker.complete_subtask(1);
        tracker.start_subtask(0, "ST-1");

        let results = tracker.all_results();
        assert_eq!(results[0].0, 0);
        assert_eq!(results[1].0, 1);
    }

    #[test]
    fn format_delegation_prompt_includes_key_fields() {
        let mut plan = DelegationPlan::new("T-parent");
        plan.add_subtask(subtask("a", &[]));
        let prompt = format_delegation_prompt(&plan);
        assert!(prompt.contains("Parent Task: T-parent"));
        assert!(prompt.contains("1. a"));
    }
}
