use std::collections::{HashMap, HashSet, VecDeque};

use orch_core::events::EventKind;
use orch_core::types::{Task, TaskId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredDependency {
    pub parent_task_id: TaskId,
    pub child_task_id: TaskId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGraph {
    pub parents_by_child: HashMap<TaskId, HashSet<TaskId>>,
    pub children_by_parent: HashMap<TaskId, HashSet<TaskId>>,
}

impl DependencyGraph {
    pub fn empty() -> Self {
        Self {
            parents_by_child: HashMap::new(),
            children_by_parent: HashMap::new(),
        }
    }
}

pub fn build_effective_dependency_graph(
    tasks: &[Task],
    inferred: &[InferredDependency],
) -> DependencyGraph {
    let mut graph = DependencyGraph::empty();
    let task_ids = tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();

    for task in tasks {
        graph.parents_by_child.entry(task.id.clone()).or_default();
        graph.children_by_parent.entry(task.id.clone()).or_default();
    }

    for task in tasks {
        for parent in &task.depends_on {
            add_edge_if_valid(&mut graph, parent.clone(), task.id.clone(), &task_ids);
        }
    }

    for edge in inferred {
        add_edge_if_valid(
            &mut graph,
            edge.parent_task_id.clone(),
            edge.child_task_id.clone(),
            &task_ids,
        );
    }

    graph
}

pub fn restack_descendants_for_parent_head_update(
    graph: &DependencyGraph,
    parent_task_id: &TaskId,
) -> Vec<TaskId> {
    let mut out = Vec::<TaskId>::new();
    let mut seen = HashSet::<TaskId>::new();
    let mut queue = VecDeque::<TaskId>::new();

    for child in sorted_task_ids(
        graph
            .children_by_parent
            .get(parent_task_id)
            .cloned()
            .unwrap_or_default(),
    ) {
        if seen.insert(child.clone()) {
            queue.push_back(child);
        }
    }

    while let Some(node) = queue.pop_front() {
        out.push(node.clone());
        for child in sorted_task_ids(
            graph
                .children_by_parent
                .get(&node)
                .cloned()
                .unwrap_or_default(),
        ) {
            if seen.insert(child.clone()) {
                queue.push_back(child);
            }
        }
    }

    out
}

pub fn parent_head_update_trigger(event: &EventKind) -> Option<TaskId> {
    match event {
        EventKind::ParentHeadUpdated { parent_task_id } => Some(parent_task_id.clone()),
        _ => None,
    }
}

fn add_edge_if_valid(
    graph: &mut DependencyGraph,
    parent: TaskId,
    child: TaskId,
    known_tasks: &HashSet<TaskId>,
) {
    if parent == child {
        return;
    }
    if !known_tasks.contains(&parent) || !known_tasks.contains(&child) {
        return;
    }

    graph
        .children_by_parent
        .entry(parent.clone())
        .or_default()
        .insert(child.clone());
    graph
        .parents_by_child
        .entry(child)
        .or_default()
        .insert(parent);
}

fn sorted_task_ids(items: HashSet<TaskId>) -> Vec<TaskId> {
    let mut out = items.into_iter().collect::<Vec<_>>();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orch_core::events::EventKind;
    use orch_core::state::{ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus};
    use orch_core::types::{RepoId, SubmitMode, Task, TaskId, TaskRole, TaskType};
    use std::path::PathBuf;

    use super::{
        build_effective_dependency_graph, parent_head_update_trigger,
        restack_descendants_for_parent_head_update, InferredDependency,
    };

    fn mk_task(id: &str, depends_on: &[&str]) -> Task {
        Task {
            id: TaskId(id.to_string()),
            repo_id: RepoId("example".to_string()),
            title: format!("Task {id}"),
            state: TaskState::Running,
            role: TaskRole::General,
            task_type: TaskType::Feature,
            preferred_model: None,
            depends_on: depends_on
                .iter()
                .map(|x| TaskId((*x).to_string()))
                .collect(),
            submit_mode: SubmitMode::Single,
            branch_name: Some(format!("task/{id}")),
            worktree_path: PathBuf::from(format!(".orch/wt/{id}")),
            pr: None,
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
    fn unions_explicit_and_inferred_dependencies() {
        let t1 = mk_task("T1", &[]);
        let t2 = mk_task("T2", &["T1"]);
        let t3 = mk_task("T3", &[]);
        let t4 = mk_task("T4", &[]);

        let graph = build_effective_dependency_graph(
            &[t1, t2, t3, t4],
            &[
                InferredDependency {
                    parent_task_id: TaskId("T2".to_string()),
                    child_task_id: TaskId("T3".to_string()),
                },
                InferredDependency {
                    parent_task_id: TaskId("T3".to_string()),
                    child_task_id: TaskId("T4".to_string()),
                },
            ],
        );

        let t2_parents = graph
            .parents_by_child
            .get(&TaskId("T2".to_string()))
            .cloned()
            .unwrap_or_default();
        assert!(t2_parents.contains(&TaskId("T1".to_string())));

        let t3_parents = graph
            .parents_by_child
            .get(&TaskId("T3".to_string()))
            .cloned()
            .unwrap_or_default();
        assert!(t3_parents.contains(&TaskId("T2".to_string())));
    }

    #[test]
    fn restack_targets_include_descendants_only_in_bfs_order() {
        let graph = build_effective_dependency_graph(
            &[
                mk_task("T1", &[]),
                mk_task("T2", &["T1"]),
                mk_task("T3", &["T1"]),
                mk_task("T4", &["T2"]),
                mk_task("T5", &["T3"]),
            ],
            &[],
        );

        let targets = restack_descendants_for_parent_head_update(&graph, &TaskId("T1".to_string()));
        let as_ids = targets.iter().map(|x| x.0.clone()).collect::<Vec<_>>();
        assert_eq!(
            as_ids,
            vec![
                "T2".to_string(),
                "T3".to_string(),
                "T4".to_string(),
                "T5".to_string()
            ]
        );
    }

    #[test]
    fn ignores_self_and_unknown_dependencies() {
        let graph = build_effective_dependency_graph(
            &[mk_task("T1", &["T1", "T9"]), mk_task("T2", &[])],
            &[
                InferredDependency {
                    parent_task_id: TaskId("T9".to_string()),
                    child_task_id: TaskId("T2".to_string()),
                },
                InferredDependency {
                    parent_task_id: TaskId("T2".to_string()),
                    child_task_id: TaskId("T2".to_string()),
                },
            ],
        );

        let t1_parents = graph
            .parents_by_child
            .get(&TaskId("T1".to_string()))
            .cloned()
            .unwrap_or_default();
        assert!(t1_parents.is_empty());

        let t2_parents = graph
            .parents_by_child
            .get(&TaskId("T2".to_string()))
            .cloned()
            .unwrap_or_default();
        assert!(t2_parents.is_empty());
    }

    #[test]
    fn restack_targets_deduplicate_diamond_descendants() {
        let graph = build_effective_dependency_graph(
            &[
                mk_task("T1", &[]),
                mk_task("T2", &["T1"]),
                mk_task("T3", &["T1"]),
                mk_task("T4", &["T2", "T3"]),
            ],
            &[],
        );

        let targets = restack_descendants_for_parent_head_update(&graph, &TaskId("T1".to_string()));
        let as_ids = targets.iter().map(|x| x.0.clone()).collect::<Vec<_>>();
        assert_eq!(
            as_ids,
            vec!["T2".to_string(), "T3".to_string(), "T4".to_string()]
        );
    }

    #[test]
    fn restack_targets_empty_for_leaf_or_unknown_parent() {
        let graph =
            build_effective_dependency_graph(&[mk_task("T1", &[]), mk_task("T2", &["T1"])], &[]);

        let leaf_targets =
            restack_descendants_for_parent_head_update(&graph, &TaskId("T2".to_string()));
        assert!(leaf_targets.is_empty());

        let unknown_targets =
            restack_descendants_for_parent_head_update(&graph, &TaskId("T9".to_string()));
        assert!(unknown_targets.is_empty());
    }

    #[test]
    fn non_parent_head_events_do_not_trigger_restack() {
        let event = EventKind::RestackStarted;
        assert!(parent_head_update_trigger(&event).is_none());

        let parent = TaskId("T9".to_string());
        let event = EventKind::ParentHeadUpdated {
            parent_task_id: parent.clone(),
        };
        assert_eq!(parent_head_update_trigger(&event), Some(parent));
    }
}
