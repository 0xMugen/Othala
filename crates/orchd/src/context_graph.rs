//! Context graph loader — reads `.othala/context/MAIN.md` and follows markdown
//! links (BFS) to build a flattened context blob for prompt injection.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// A single node in the context graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextNode {
    /// Path to the markdown file (relative to repo root).
    pub path: PathBuf,
    /// Raw content of the file.
    pub content: String,
    /// Links to other `.othala/` context files found in this node.
    pub links: Vec<PathBuf>,
    /// References to repo source files found in this node.
    pub source_refs: Vec<PathBuf>,
}

/// The fully-loaded context graph.
#[derive(Debug, Clone)]
pub struct ContextGraph {
    pub nodes: Vec<ContextNode>,
    pub total_chars: usize,
}

/// Configuration for context loading.
#[derive(Debug, Clone)]
pub struct ContextLoadConfig {
    /// Maximum BFS depth from MAIN.md.
    pub max_depth: usize,
    /// Character budget — stop loading once exceeded.
    pub max_total_chars: usize,
}

impl Default for ContextLoadConfig {
    fn default() -> Self {
        Self {
            max_depth: 6,
            max_total_chars: 80_000,
        }
    }
}

/// Load the context graph starting from `.othala/context/MAIN.md`.
///
/// Performs BFS link-following up to `config.max_depth`, stopping when the
/// character budget is exhausted. Returns `None` if the entry point doesn't
/// exist.
pub fn load_context_graph(
    repo_root: &Path,
    config: &ContextLoadConfig,
) -> Option<ContextGraph> {
    let entry = repo_root.join(".othala/context/MAIN.md");
    if !entry.exists() {
        return None;
    }

    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    let mut nodes: Vec<ContextNode> = Vec::new();
    let mut total_chars: usize = 0;

    // Normalise entry to a repo-relative path for the visited set.
    let entry_rel = PathBuf::from(".othala/context/MAIN.md");
    queue.push_back((entry_rel.clone(), 0));
    visited.insert(entry_rel);

    while let Some((rel_path, depth)) = queue.pop_front() {
        if total_chars >= config.max_total_chars {
            break;
        }

        let abs_path = repo_root.join(&rel_path);
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Respect budget: truncate if the remaining budget is smaller than the file.
        let remaining = config.max_total_chars.saturating_sub(total_chars);
        let content = if content.len() > remaining {
            content[..remaining].to_string()
        } else {
            content
        };

        let (links, source_refs) = extract_links(&content, &rel_path);
        total_chars += content.len();

        // Enqueue links for next depth level.
        if depth < config.max_depth {
            for link in &links {
                if !visited.contains(link) {
                    visited.insert(link.clone());
                    queue.push_back((link.clone(), depth + 1));
                }
            }
        }

        nodes.push(ContextNode {
            path: rel_path,
            content,
            links,
            source_refs,
        });
    }

    Some(ContextGraph { nodes, total_chars })
}

/// Render the loaded context graph into a single string suitable for prompt
/// injection.
pub fn render_context_for_prompt(graph: &ContextGraph) -> String {
    let mut out = String::with_capacity(graph.total_chars + 512);
    out.push_str("# Repository Context\n\n");

    for node in &graph.nodes {
        out.push_str(&format!("## {}\n\n", node.path.display()));
        out.push_str(&node.content);
        if !node.content.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    out
}

/// Extract markdown links from content.
///
/// Returns `(context_links, source_refs)`:
/// - `context_links`: paths pointing to other `.othala/` files (for BFS).
/// - `source_refs`: paths pointing to repo source files (informational).
fn extract_links(content: &str, current_path: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut context_links = Vec::new();
    let mut source_refs = Vec::new();

    let parent = current_path.parent().unwrap_or(Path::new("."));

    for line in content.lines() {
        // Match markdown link patterns: [text](path) and bare path references.
        let mut rest = line;
        while let Some(start) = rest.find("](") {
            let after = &rest[start + 2..];
            if let Some(end) = after.find(')') {
                let target = after[..end].trim();
                if !target.is_empty()
                    && !target.starts_with("http")
                    && !target.starts_with('#')
                {
                    let resolved = parent.join(target);
                    let normalised = normalise_path(&resolved);

                    if normalised.starts_with(".othala/")
                        && normalised.extension().map(|e| e == "md").unwrap_or(false)
                    {
                        context_links.push(normalised);
                    } else {
                        source_refs.push(normalised);
                    }
                }
                rest = &after[end + 1..];
            } else {
                break;
            }
        }
    }

    (context_links, source_refs)
}

/// Simple path normalisation — resolve `..` and `.` components without hitting
/// the filesystem.
fn normalise_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_context_dir(tmp: &Path) {
        let ctx = tmp.join(".othala/context");
        fs::create_dir_all(&ctx).unwrap();

        fs::write(
            ctx.join("MAIN.md"),
            "# Main Context\n\nSee [architecture](architecture.md) for details.\n\
             Also references [src/lib.rs](../../src/lib.rs).\n",
        )
        .unwrap();

        fs::write(
            ctx.join("architecture.md"),
            "# Architecture\n\nCore crates: orchd, orch-core.\n\
             See [patterns](patterns.md) for coding style.\n",
        )
        .unwrap();

        fs::write(
            ctx.join("patterns.md"),
            "# Patterns\n\nUse `thiserror` for errors.\n",
        )
        .unwrap();
    }

    #[test]
    fn loads_context_graph_bfs() {
        let tmp = std::env::temp_dir().join(format!("othala-ctx-test-{}", std::process::id()));
        setup_context_dir(&tmp);

        let graph = load_context_graph(&tmp, &ContextLoadConfig::default())
            .expect("should load graph");

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(
            graph.nodes[0].path,
            PathBuf::from(".othala/context/MAIN.md")
        );
        assert_eq!(
            graph.nodes[1].path,
            PathBuf::from(".othala/context/architecture.md")
        );
        assert_eq!(
            graph.nodes[2].path,
            PathBuf::from(".othala/context/patterns.md")
        );
        assert!(graph.total_chars > 0);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn respects_char_budget() {
        let tmp = std::env::temp_dir().join(format!("othala-ctx-budget-{}", std::process::id()));
        setup_context_dir(&tmp);

        let config = ContextLoadConfig {
            max_depth: 3,
            max_total_chars: 50, // very small budget
        };
        let graph = load_context_graph(&tmp, &config).expect("should load graph");

        // Should load at most the budget worth of characters.
        assert!(graph.total_chars <= 50);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn respects_max_depth() {
        let tmp = std::env::temp_dir().join(format!("othala-ctx-depth-{}", std::process::id()));
        setup_context_dir(&tmp);

        let config = ContextLoadConfig {
            max_depth: 1, // only MAIN.md + direct links
            max_total_chars: 50_000,
        };
        let graph = load_context_graph(&tmp, &config).expect("should load graph");

        // MAIN.md (depth 0) links to architecture.md (depth 1).
        // architecture.md links to patterns.md (depth 2) but max_depth=1 stops it.
        assert_eq!(graph.nodes.len(), 2);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn returns_none_when_no_entry() {
        let tmp = std::env::temp_dir().join(format!("othala-ctx-none-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        assert!(load_context_graph(&tmp, &ContextLoadConfig::default()).is_none());

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn extracts_links_and_source_refs() {
        let content = "See [arch](architecture.md) and [code](../../src/lib.rs).\n";
        let current = Path::new(".othala/context/MAIN.md");
        let (links, refs) = extract_links(content, current);

        assert_eq!(links, vec![PathBuf::from(".othala/context/architecture.md")]);
        assert_eq!(refs, vec![PathBuf::from("src/lib.rs")]);
    }

    #[test]
    fn render_context_produces_markdown() {
        let graph = ContextGraph {
            nodes: vec![ContextNode {
                path: PathBuf::from(".othala/context/MAIN.md"),
                content: "# Hello\n".to_string(),
                links: vec![],
                source_refs: vec![],
            }],
            total_chars: 9,
        };

        let rendered = render_context_for_prompt(&graph);
        assert!(rendered.contains("# Repository Context"));
        assert!(rendered.contains("## .othala/context/MAIN.md"));
        assert!(rendered.contains("# Hello"));
    }

    #[test]
    fn normalise_path_resolves_parent_refs() {
        assert_eq!(
            normalise_path(Path::new(".othala/context/../context/foo.md")),
            PathBuf::from(".othala/context/foo.md")
        );
        assert_eq!(
            normalise_path(Path::new(".othala/context/../../src/lib.rs")),
            PathBuf::from("src/lib.rs")
        );
    }
}
