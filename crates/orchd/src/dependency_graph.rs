//! Dependency graph for task relationships.

use std::collections::{HashMap, HashSet, VecDeque};

use orch_core::types::{Task, TaskId};

/// Dependency graph structure.
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

/// Build a dependency graph from tasks.
pub fn build_dependency_graph(tasks: &[Task]) -> DependencyGraph {
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

    graph
}

/// Get tasks that need restacking when a parent task is updated.
pub fn restack_descendants_for_parent(
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
    use super::*;
    use orch_core::types::RepoId;
    use std::path::PathBuf;

    fn mk_task(id: &str, depends_on: &[&str]) -> Task {
        let mut task = Task::new(
            TaskId(id.to_string()),
            RepoId("example".to_string()),
            format!("Task {id}"),
            PathBuf::from(format!(".orch/wt/{id}")),
        );
        task.depends_on = depends_on
            .iter()
            .map(|x| TaskId((*x).to_string()))
            .collect();
        task
    }

    #[test]
    fn builds_graph_from_dependencies() {
        let t1 = mk_task("T1", &[]);
        let t2 = mk_task("T2", &["T1"]);
        let t3 = mk_task("T3", &["T1"]);

        let graph = build_dependency_graph(&[t1, t2, t3]);

        let t2_parents = graph
            .parents_by_child
            .get(&TaskId("T2".to_string()))
            .cloned()
            .unwrap_or_default();
        assert!(t2_parents.contains(&TaskId("T1".to_string())));

        let t1_children = graph
            .children_by_parent
            .get(&TaskId("T1".to_string()))
            .cloned()
            .unwrap_or_default();
        assert!(t1_children.contains(&TaskId("T2".to_string())));
        assert!(t1_children.contains(&TaskId("T3".to_string())));
    }

    #[test]
    fn restack_targets_include_descendants_in_bfs_order() {
        let graph = build_dependency_graph(&[
            mk_task("T1", &[]),
            mk_task("T2", &["T1"]),
            mk_task("T3", &["T1"]),
            mk_task("T4", &["T2"]),
            mk_task("T5", &["T3"]),
        ]);

        let targets = restack_descendants_for_parent(&graph, &TaskId("T1".to_string()));
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
        let graph = build_dependency_graph(&[
            mk_task("T1", &["T1", "T9"]),
            mk_task("T2", &[]),
        ]);

        let t1_parents = graph
            .parents_by_child
            .get(&TaskId("T1".to_string()))
            .cloned()
            .unwrap_or_default();
        assert!(t1_parents.is_empty());
    }

    #[test]
    fn restack_targets_deduplicate_diamond_descendants() {
        let graph = build_dependency_graph(&[
            mk_task("T1", &[]),
            mk_task("T2", &["T1"]),
            mk_task("T3", &["T1"]),
            mk_task("T4", &["T2", "T3"]),
        ]);

        let targets = restack_descendants_for_parent(&graph, &TaskId("T1".to_string()));
        let as_ids = targets.iter().map(|x| x.0.clone()).collect::<Vec<_>>();
        assert_eq!(
            as_ids,
            vec!["T2".to_string(), "T3".to_string(), "T4".to_string()]
        );
    }

    #[test]
    fn restack_targets_empty_for_leaf_or_unknown_parent() {
        let graph = build_dependency_graph(&[
            mk_task("T1", &[]),
            mk_task("T2", &["T1"]),
        ]);

        let leaf_targets = restack_descendants_for_parent(&graph, &TaskId("T2".to_string()));
        assert!(leaf_targets.is_empty());

        let unknown_targets = restack_descendants_for_parent(&graph, &TaskId("T9".to_string()));
        assert!(unknown_targets.is_empty());
    }
}
