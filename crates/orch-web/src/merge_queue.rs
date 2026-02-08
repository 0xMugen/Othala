use chrono::Utc;
use orch_core::state::TaskState;
use orch_core::types::Task;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::model::{MergeQueueGroup, MergeQueueResponse};

pub fn build_merge_queue(tasks: &[Task]) -> MergeQueueResponse {
    let awaiting = tasks
        .iter()
        .filter(|task| task.state == TaskState::AwaitingMerge)
        .cloned()
        .collect::<Vec<_>>();

    let mut by_id = HashMap::<String, Task>::new();
    for task in awaiting {
        by_id.insert(task.id.0.clone(), task);
    }

    let mut children_by_parent = HashMap::<String, HashSet<String>>::new();
    let mut undirected = HashMap::<String, HashSet<String>>::new();

    for (task_id, task) in &by_id {
        undirected.entry(task_id.clone()).or_default();
        for parent in &task.depends_on {
            if !by_id.contains_key(&parent.0) {
                continue;
            }
            children_by_parent
                .entry(parent.0.clone())
                .or_default()
                .insert(task_id.clone());
            undirected
                .entry(parent.0.clone())
                .or_default()
                .insert(task_id.clone());
            undirected
                .entry(task_id.clone())
                .or_default()
                .insert(parent.0.clone());
        }
    }

    let mut seen = HashSet::<String>::new();
    let mut groups = Vec::<MergeQueueGroup>::new();

    let mut all_ids = by_id.keys().cloned().collect::<Vec<_>>();
    all_ids.sort();

    for seed in all_ids {
        if seen.contains(&seed) {
            continue;
        }

        let component = bfs_component(&seed, &undirected, &mut seen);
        let (order, contains_cycle) = topo_order_component(&component, &children_by_parent);
        let mut task_ids = component.iter().cloned().collect::<Vec<_>>();
        task_ids.sort();

        let mut pr_urls = BTreeSet::new();
        for task_id in &task_ids {
            if let Some(url) = by_id
                .get(task_id)
                .and_then(|task| task.pr.as_ref().map(|pr| pr.url.clone()))
            {
                pr_urls.insert(url);
            }
        }

        groups.push(MergeQueueGroup {
            group_id: format!("stack-{}", task_ids.join("+")),
            task_ids,
            recommended_merge_order: order,
            pr_urls: pr_urls.into_iter().collect(),
            contains_cycle,
        });
    }

    groups.sort_by(|a, b| a.group_id.cmp(&b.group_id));

    MergeQueueResponse {
        generated_at: Utc::now(),
        groups,
    }
}

fn bfs_component(
    seed: &str,
    graph: &HashMap<String, HashSet<String>>,
    seen: &mut HashSet<String>,
) -> HashSet<String> {
    let mut queue = VecDeque::new();
    let mut out = HashSet::new();
    queue.push_back(seed.to_string());
    seen.insert(seed.to_string());

    while let Some(current) = queue.pop_front() {
        out.insert(current.clone());
        for next in graph.get(&current).cloned().unwrap_or_default() {
            if seen.insert(next.clone()) {
                queue.push_back(next);
            }
        }
    }
    out
}

fn topo_order_component(
    component: &HashSet<String>,
    children_by_parent: &HashMap<String, HashSet<String>>,
) -> (Vec<String>, bool) {
    let mut indegree = HashMap::<String, usize>::new();
    for node in component {
        indegree.insert(node.clone(), 0);
    }

    for parent in component {
        let children = children_by_parent.get(parent).cloned().unwrap_or_default();
        for child in children {
            if component.contains(&child) {
                *indegree.entry(child).or_insert(0) += 1;
            }
        }
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(id, in_deg)| if *in_deg == 0 { Some(id.clone()) } else { None })
        .collect::<Vec<_>>();
    ready.sort();

    let mut order = Vec::<String>::new();
    while !ready.is_empty() {
        let current = ready.remove(0);
        order.push(current.clone());
        let children = children_by_parent
            .get(&current)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        for child in children {
            if !component.contains(&child) {
                continue;
            }
            let value = indegree.entry(child.clone()).or_insert(0);
            if *value > 0 {
                *value -= 1;
                if *value == 0 {
                    ready.push(child);
                    ready.sort();
                }
            }
        }
    }

    let contains_cycle = order.len() != component.len();
    if contains_cycle {
        let mut fallback = component.iter().cloned().collect::<Vec<_>>();
        fallback.sort();
        return (fallback, true);
    }
    (order, false)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::state::{ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus};
    use orch_core::types::{PullRequestRef, RepoId, SubmitMode, Task, TaskId, TaskRole, TaskType};
    use std::path::PathBuf;

    use super::build_merge_queue;

    fn mk_task(id: &str, state: TaskState, depends_on: &[&str], pr_url: Option<&str>) -> Task {
        Task {
            id: TaskId(id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Task {id}"),
            state,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: depends_on
                .iter()
                .map(|parent| TaskId((*parent).to_string()))
                .collect(),
            submit_mode: SubmitMode::Single,
            branch_name: Some(format!("task/{id}")),
            worktree_path: PathBuf::from(format!(".orch/wt/{id}")),
            pr: pr_url.map(|url| PullRequestRef {
                number: 1,
                url: url.to_string(),
                draft: false,
            }),
            verify_status: VerifyStatus::NotRun,
            review_status: ReviewStatus {
                required_models: Vec::new(),
                approvals_received: 0,
                approvals_required: 0,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn build_merge_queue_filters_non_awaiting_and_keeps_stack_order() {
        let tasks = vec![
            mk_task(
                "T1",
                TaskState::AwaitingMerge,
                &[],
                Some("https://github.com/org/repo/pull/1"),
            ),
            mk_task(
                "T2",
                TaskState::AwaitingMerge,
                &["T1"],
                Some("https://github.com/org/repo/pull/2"),
            ),
            mk_task("T3", TaskState::Running, &["T2"], None),
        ];

        let queue = build_merge_queue(&tasks);
        assert_eq!(queue.groups.len(), 1);
        let group = &queue.groups[0];
        assert_eq!(group.task_ids, vec!["T1".to_string(), "T2".to_string()]);
        assert_eq!(
            group.recommended_merge_order,
            vec!["T1".to_string(), "T2".to_string()]
        );
        assert_eq!(
            group.pr_urls,
            vec![
                "https://github.com/org/repo/pull/1".to_string(),
                "https://github.com/org/repo/pull/2".to_string(),
            ]
        );
        assert!(!group.contains_cycle);
    }

    #[test]
    fn build_merge_queue_marks_cycle_and_uses_sorted_fallback_order() {
        let tasks = vec![
            mk_task("A", TaskState::AwaitingMerge, &["B"], None),
            mk_task("B", TaskState::AwaitingMerge, &["A"], None),
        ];

        let queue = build_merge_queue(&tasks);
        assert_eq!(queue.groups.len(), 1);
        let group = &queue.groups[0];
        assert!(group.contains_cycle);
        assert_eq!(group.task_ids, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(
            group.recommended_merge_order,
            vec!["A".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn build_merge_queue_splits_disconnected_components() {
        let tasks = vec![
            mk_task("T1", TaskState::AwaitingMerge, &[], None),
            mk_task("T2", TaskState::AwaitingMerge, &["T1"], None),
            mk_task("T9", TaskState::AwaitingMerge, &[], None),
        ];

        let queue = build_merge_queue(&tasks);
        assert_eq!(queue.groups.len(), 2);
        assert_eq!(
            queue.groups[0].task_ids,
            vec!["T1".to_string(), "T2".to_string()]
        );
        assert_eq!(queue.groups[1].task_ids, vec!["T9".to_string()]);
    }
}
