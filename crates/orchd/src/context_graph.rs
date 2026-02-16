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
            max_depth: 10,
            max_total_chars: 80_000,
        }
    }
}

/// Load the context graph starting from `.othala/context/MAIN.md`.
///
/// Performs BFS link-following up to `config.max_depth`, stopping when the
/// character budget is exhausted. Returns `None` if the entry point doesn't
/// exist.
pub fn load_context_graph(repo_root: &Path, config: &ContextLoadConfig) -> Option<ContextGraph> {
    let entry = repo_root.join(".othala/context/MAIN.md");
    if !entry.exists() {
        return None;
    }

    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    let mut nodes: Vec<ContextNode> = Vec::new();
    let mut total_chars: usize = 0;
    let mut cycle_count: usize = 0;

    // Normalise entry to a repo-relative path for the visited set.
    let entry_rel = PathBuf::from(".othala/context/MAIN.md");
    queue.push_back((entry_rel.clone(), 0));
    let entry_canonical = repo_root
        .join(&entry_rel)
        .canonicalize()
        .unwrap_or_else(|_| normalise_path(&repo_root.join(&entry_rel)));
    visited.insert(entry_canonical);

    while let Some((rel_path, depth)) = queue.pop_front() {
        if depth > config.max_depth {
            eprintln!(
                "warning: context depth limit reached at {} (depth {}, max_depth {})",
                rel_path.display(),
                depth,
                config.max_depth
            );
            continue;
        }

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
        for link in &links {
            let next_depth = depth + 1;
            if next_depth > config.max_depth {
                eprintln!(
                    "warning: context depth limit reached at {} -> {} (max_depth {})",
                    rel_path.display(),
                    link.display(),
                    config.max_depth
                );
                continue;
            }

            let link_abs = repo_root.join(link);
            let link_canonical = link_abs
                .canonicalize()
                .unwrap_or_else(|_| normalise_path(&link_abs));

            if visited.contains(&link_canonical) {
                cycle_count += 1;
                if cycle_count <= 3 {
                    eprintln!(
                        "warning: cycle detected in context graph: {} -> {}",
                        rel_path.display(),
                        link.display()
                    );
                }
                continue;
            }

            visited.insert(link_canonical);
            queue.push_back((link.clone(), next_depth));
        }

        nodes.push(ContextNode {
            path: rel_path,
            content,
            links,
            source_refs,
        });
    }

    if cycle_count > 3 {
        eprintln!(
            "warning: {} additional cycle(s) suppressed in context graph",
            cycle_count - 3
        );
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

/// Render context graph with inlined source file contents.
///
/// Like `render_context_for_prompt` but also reads files referenced via
/// `@file:` and `[text](../../path)` source_refs, appending their content
/// in fenced code blocks. Respects a character budget for inlined sources.
pub fn render_context_with_sources(
    graph: &ContextGraph,
    repo_root: &Path,
    source_budget: usize,
) -> String {
    let mut out = render_context_for_prompt(graph);

    let mut all_refs: Vec<&PathBuf> = Vec::new();
    let mut seen = HashSet::new();
    for node in &graph.nodes {
        for r in &node.source_refs {
            if seen.insert(r) {
                all_refs.push(r);
            }
        }
    }

    if all_refs.is_empty() {
        return out;
    }

    out.push_str("# Referenced Source Files\n\n");
    let mut used = 0usize;

    for path in all_refs {
        if used >= source_budget {
            break;
        }
        let abs = repo_root.join(path);
        let content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let remaining = source_budget.saturating_sub(used);
        let content = if content.len() > remaining {
            format!("{}...(truncated)", &content[..remaining])
        } else {
            content
        };

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        out.push_str(&format!("## {}\n\n```{}\n", path.display(), ext));
        out.push_str(&content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n\n");
        used += content.len();
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
                if !target.is_empty() && !target.starts_with("http") && !target.starts_with('#') {
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

        let mut wiki_rest = line;
        while let Some(start) = wiki_rest.find("[[") {
            let after = &wiki_rest[start + 2..];
            if let Some(end) = after.find("]]") {
                let target = after[..end].trim();
                if !target.is_empty() {
                    let resolved = parent.join(format!("{target}.md"));
                    let normalised = normalise_path(&resolved);

                    if normalised.starts_with(".othala/")
                        && normalised.extension().map(|e| e == "md").unwrap_or(false)
                    {
                        context_links.push(normalised);
                    } else {
                        source_refs.push(normalised);
                    }
                }
                wiki_rest = &after[end + 2..];
            } else {
                break;
            }
        }

        let mut file_rest = line;
        while let Some(start) = file_rest.find("@file:") {
            let after = &file_rest[start + 6..];
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            let target = after[..end].trim();
            if !target.is_empty() {
                let resolved = parent.join(target);
                source_refs.push(normalise_path(&resolved));
            }
            file_rest = &after[end..];
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

    fn setup_wiki_context_dir(tmp: &Path) {
        let ctx = tmp.join(".othala/context/wiki");
        fs::create_dir_all(&ctx).unwrap();

        fs::write(
            tmp.join(".othala/context/MAIN.md"),
            "# Main Context\n\nSee [[wiki/architecture]].\n",
        )
        .unwrap();

        fs::write(
            ctx.join("architecture.md"),
            "# Architecture\n\nSee [[patterns]] for coding style.\n",
        )
        .unwrap();

        fs::write(
            ctx.join("patterns.md"),
            "# Patterns\n\nKeep modules small.\n",
        )
        .unwrap();
    }

    #[test]
    fn loads_context_graph_bfs() {
        let tmp = std::env::temp_dir().join(format!("othala-ctx-test-{}", std::process::id()));
        setup_context_dir(&tmp);

        let graph =
            load_context_graph(&tmp, &ContextLoadConfig::default()).expect("should load graph");

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

        assert_eq!(
            links,
            vec![PathBuf::from(".othala/context/architecture.md")]
        );
        assert_eq!(refs, vec![PathBuf::from("src/lib.rs")]);
    }

    #[test]
    fn extracts_single_wiki_link() {
        let content = "See [[architecture]].\n";
        let current = Path::new(".othala/context/MAIN.md");
        let (links, refs) = extract_links(content, current);

        assert_eq!(
            links,
            vec![PathBuf::from(".othala/context/architecture.md")]
        );
        assert!(refs.is_empty());
    }

    #[test]
    fn extracts_multiple_wiki_links() {
        let content = "See [[architecture]] and [[patterns]].\n";
        let current = Path::new(".othala/context/MAIN.md");
        let (links, refs) = extract_links(content, current);

        assert_eq!(
            links,
            vec![
                PathBuf::from(".othala/context/architecture.md"),
                PathBuf::from(".othala/context/patterns.md")
            ]
        );
        assert!(refs.is_empty());
    }

    #[test]
    fn extracts_wiki_links_from_nested_context_directory() {
        let content = "See [[patterns]].\n";
        let current = Path::new(".othala/context/wiki/architecture.md");
        let (links, refs) = extract_links(content, current);

        assert_eq!(
            links,
            vec![PathBuf::from(".othala/context/wiki/patterns.md")]
        );
        assert!(refs.is_empty());
    }

    #[test]
    fn extracts_file_references() {
        let content = "Read @file:../../src/lib.rs and @file:../mod.rs\n";
        let current = Path::new(".othala/context/wiki/architecture.md");
        let (links, refs) = extract_links(content, current);

        assert!(links.is_empty());
        assert_eq!(
            refs,
            vec![
                PathBuf::from(".othala/src/lib.rs"),
                PathBuf::from(".othala/context/mod.rs")
            ]
        );
    }

    #[test]
    fn loads_context_graph_bfs_with_wiki_links() {
        let tmp = std::env::temp_dir().join(format!("othala-ctx-wiki-{}", std::process::id()));
        setup_wiki_context_dir(&tmp);

        let graph =
            load_context_graph(&tmp, &ContextLoadConfig::default()).expect("should load graph");

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(
            graph.nodes[0].path,
            PathBuf::from(".othala/context/MAIN.md")
        );
        assert_eq!(
            graph.nodes[1].path,
            PathBuf::from(".othala/context/wiki/architecture.md")
        );
        assert_eq!(
            graph.nodes[2].path,
            PathBuf::from(".othala/context/wiki/patterns.md")
        );

        fs::remove_dir_all(&tmp).ok();
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

    #[test]
    fn test_default_depth_is_10() {
        assert_eq!(ContextLoadConfig::default().max_depth, 10);
    }

    #[test]
    fn test_depth_limit_respected() {
        let tmp =
            std::env::temp_dir().join(format!("othala-ctx-depth-limit-{}", std::process::id()));
        let ctx = tmp.join(".othala/context");
        fs::create_dir_all(&ctx).unwrap();

        fs::write(ctx.join("MAIN.md"), "See [B](B.md)\n").unwrap();
        fs::write(ctx.join("B.md"), "See [C](C.md)\n").unwrap();
        fs::write(ctx.join("C.md"), "See [D](D.md)\n").unwrap();
        fs::write(ctx.join("D.md"), "See [E](E.md)\n").unwrap();
        fs::write(ctx.join("E.md"), "End\n").unwrap();

        let config = ContextLoadConfig {
            max_depth: 2,
            max_total_chars: 80_000,
        };

        let graph = load_context_graph(&tmp, &config).expect("should load graph");
        let loaded: Vec<PathBuf> = graph.nodes.iter().map(|n| n.path.clone()).collect();

        assert_eq!(
            loaded,
            vec![
                PathBuf::from(".othala/context/MAIN.md"),
                PathBuf::from(".othala/context/B.md"),
                PathBuf::from(".othala/context/C.md")
            ]
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_cycle_detection() {
        let tmp = std::env::temp_dir().join(format!("othala-ctx-cycle-{}", std::process::id()));
        let ctx = tmp.join(".othala/context");
        fs::create_dir_all(&ctx).unwrap();

        fs::write(ctx.join("MAIN.md"), "See [A](A.md)\n").unwrap();
        fs::write(ctx.join("A.md"), "See [B](B.md)\n").unwrap();
        fs::write(ctx.join("B.md"), "See [A](A.md)\n").unwrap();

        let graph =
            load_context_graph(&tmp, &ContextLoadConfig::default()).expect("should load graph");

        let loaded: Vec<PathBuf> = graph.nodes.iter().map(|n| n.path.clone()).collect();
        assert_eq!(
            loaded,
            vec![
                PathBuf::from(".othala/context/MAIN.md"),
                PathBuf::from(".othala/context/A.md"),
                PathBuf::from(".othala/context/B.md")
            ]
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_cycle_with_source_refs() {
        let tmp =
            std::env::temp_dir().join(format!("othala-ctx-cycle-source-{}", std::process::id()));
        let ctx = tmp.join(".othala/context");
        fs::create_dir_all(&ctx).unwrap();

        fs::write(
            ctx.join("MAIN.md"),
            "See [A](A.md) and @file:A.md and @file:MAIN.md\n",
        )
        .unwrap();
        fs::write(
            ctx.join("A.md"),
            "See [B](B.md) and @file:MAIN.md and @file:B.md\n",
        )
        .unwrap();
        fs::write(ctx.join("B.md"), "See [A](A.md) and @file:MAIN.md\n").unwrap();

        let graph =
            load_context_graph(&tmp, &ContextLoadConfig::default()).expect("should load graph");

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(
            graph.nodes[0].path,
            PathBuf::from(".othala/context/MAIN.md")
        );
        assert_eq!(graph.nodes[1].path, PathBuf::from(".othala/context/A.md"));
        assert_eq!(graph.nodes[2].path, PathBuf::from(".othala/context/B.md"));

        fs::remove_dir_all(&tmp).ok();
    }
}
