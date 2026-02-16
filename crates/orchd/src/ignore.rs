//! .othalaignore file support — excludes files from AI agent context.
//!
//! Follows .gitignore-like syntax: glob patterns, # comments, ! negation.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnorePattern {
    Include(String),
    Exclude(String),
}

#[derive(Debug, Clone, Default)]
pub struct IgnoreRules {
    patterns: Vec<IgnorePattern>,
    source_file: Option<PathBuf>,
}

impl IgnoreRules {
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(content) => {
                let mut rules = Self::parse(&content);
                rules.source_file = Some(path.to_path_buf());
                rules
            }
            Err(_) => Self::default(),
        }
    }

    pub fn parse(content: &str) -> Self {
        let mut rules = Self::default();
        for line in content.lines() {
            rules.add_pattern(line);
        }
        rules
    }

    pub fn is_ignored(&self, path: &str) -> bool {
        let normalized_path = normalize_path(path);
        if normalized_path.is_empty() {
            return false;
        }

        let mut ignored = false;
        for pattern in &self.patterns {
            match pattern {
                IgnorePattern::Exclude(rule) => {
                    if pattern_matches(rule, &normalized_path) {
                        ignored = true;
                    }
                }
                IgnorePattern::Include(rule) => {
                    if pattern_matches(rule, &normalized_path) {
                        ignored = false;
                    }
                }
            }
        }

        ignored
    }

    pub fn add_pattern(&mut self, pattern: &str) {
        let trimmed = pattern.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return;
        }

        if let Some(include) = trimmed.strip_prefix('!') {
            let include = include.trim();
            if !include.is_empty() {
                self.patterns.push(IgnorePattern::Include(include.to_string()));
            }
            return;
        }

        self.patterns
            .push(IgnorePattern::Exclude(trimmed.to_string()));
    }

    pub fn merge(&mut self, other: &IgnoreRules) {
        self.patterns.extend(other.patterns.clone());
        if self.source_file.is_none() {
            self.source_file = other.source_file.clone();
        }
    }

    pub fn patterns(&self) -> &[IgnorePattern] {
        &self.patterns
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextPathsConfig {
    pub paths: Vec<String>,
    pub auto_detect: bool,
}

impl ContextPathsConfig {
    pub fn default_config() -> Self {
        Self {
            paths: vec![
                "AGENTS.md".to_string(),
                ".othala/context/MAIN.md".to_string(),
            ],
            auto_detect: true,
        }
    }

    pub fn resolve_paths(&self, root: &Path) -> Vec<PathBuf> {
        self.paths
            .iter()
            .map(|path| root.join(path))
            .filter(|path| path.exists())
            .collect()
    }
}

pub fn load_ignore_rules(repo_root: &Path) -> IgnoreRules {
    let mut rules = IgnoreRules::default();

    let ignore_path = repo_root.join(".othalaignore");
    if ignore_path.exists() {
        rules = IgnoreRules::load(&ignore_path);
    }

    let alt_path = repo_root.join(".othala/ignore");
    if alt_path.exists() {
        let alt_rules = IgnoreRules::load(&alt_path);
        rules.merge(&alt_rules);
    }

    rules
}

pub fn display_ignore_rules(rules: &IgnoreRules) -> String {
    let source = rules
        .source_file
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "(none)".to_string());

    let mut out = vec![
        format!("Source: {source}"),
        "Idx | Type    | Pattern".to_string(),
        "----+---------+----------------".to_string(),
    ];

    for (idx, pattern) in rules.patterns.iter().enumerate() {
        match pattern {
            IgnorePattern::Include(value) => out.push(format!("{idx:>3} | include | {value}")),
            IgnorePattern::Exclude(value) => out.push(format!("{idx:>3} | exclude | {value}")),
        }
    }

    if rules.patterns.is_empty() {
        out.push("(no patterns)".to_string());
    }

    out.join("\n")
}

fn normalize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.trim_start_matches("./");
    normalized.trim_start_matches('/').to_string()
}

fn pattern_matches(raw_pattern: &str, path: &str) -> bool {
    let pattern = normalize_path(raw_pattern);
    if pattern.is_empty() {
        return false;
    }

    let anchored = raw_pattern.trim().starts_with('/');
    let dir_only = raw_pattern.trim_end().ends_with('/');
    let core = pattern.trim_end_matches('/');

    if core.is_empty() {
        return false;
    }

    if dir_only {
        return directory_pattern_matches(core, path, anchored);
    }

    if anchored {
        return glob_match(core, path);
    }

    pattern_variants(core)
        .iter()
        .any(|variant| matches_any_boundary(variant, path))
}

fn directory_pattern_matches(pattern: &str, path: &str, anchored: bool) -> bool {
    if anchored {
        return path == pattern || path.starts_with(&format!("{pattern}/"));
    }

    for variant in pattern_variants(pattern) {
        let wildcard_dir_pattern = format!("{variant}/**");
        if matches_any_boundary(&wildcard_dir_pattern, path) {
            return true;
        }

        if matches_any_boundary(variant, path) {
            return true;
        }

        if !variant.contains('/') {
            for component in directory_components(path) {
                if glob_match(variant, component) {
                    return true;
                }
            }
        }
    }

    false
}

fn directory_components(path: &str) -> Vec<&str> {
    let components: Vec<&str> = path.split('/').collect();
    components
        .iter()
        .take(components.len().saturating_sub(1))
        .copied()
        .collect()
}

fn matches_any_boundary(pattern: &str, path: &str) -> bool {
    if glob_match(pattern, path) {
        return true;
    }

    for start in boundary_indices(path) {
        if glob_match(pattern, &path[start..]) {
            return true;
        }
    }

    false
}

fn pattern_variants(pattern: &str) -> Vec<&str> {
    let mut variants = vec![pattern];

    if let Some(stripped) = pattern.strip_prefix("**/") {
        variants.push(stripped);
    }
    if let Some(stripped) = pattern.strip_suffix("/**") {
        variants.push(stripped);
    }

    variants
}

fn boundary_indices(path: &str) -> Vec<usize> {
    let mut indices = Vec::new();
    for (idx, ch) in path.char_indices() {
        if ch == '/' {
            let next = idx + ch.len_utf8();
            if next < path.len() {
                indices.push(next);
            }
        }
    }
    indices
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let cols = txt.len() + 1;
    let mut memo: Vec<Option<bool>> = vec![None; (pat.len() + 1) * cols];
    glob_match_inner(&pat, &txt, 0, 0, cols, &mut memo)
}

fn glob_match_inner(
    pattern: &[char],
    text: &[char],
    pi: usize,
    ti: usize,
    cols: usize,
    memo: &mut [Option<bool>],
) -> bool {
    let key = pi * cols + ti;
    if let Some(cached) = memo[key] {
        return cached;
    }

    let result = if pi == pattern.len() {
        ti == text.len()
    } else {
        let token = pattern[pi];
        if token == '*' {
            if pi + 1 < pattern.len() && pattern[pi + 1] == '*' {
                let mut next = pi + 2;
                while next < pattern.len() && pattern[next] == '*' {
                    next += 1;
                }

                glob_match_inner(pattern, text, next, ti, cols, memo)
                    || (ti < text.len()
                        && glob_match_inner(pattern, text, pi, ti + 1, cols, memo))
            } else {
                glob_match_inner(pattern, text, pi + 1, ti, cols, memo)
                    || (ti < text.len()
                        && text[ti] != '/'
                        && glob_match_inner(pattern, text, pi, ti + 1, cols, memo))
            }
        } else if token == '?' {
            ti < text.len()
                && text[ti] != '/'
                && glob_match_inner(pattern, text, pi + 1, ti + 1, cols, memo)
        } else {
            ti < text.len()
                && token == text[ti]
                && glob_match_inner(pattern, text, pi + 1, ti + 1, cols, memo)
        }
    };

    memo[key] = Some(result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_test_path(name: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{name}-{id}"))
    }

    #[test]
    fn parse_empty_content() {
        let rules = IgnoreRules::parse("");
        assert!(rules.patterns().is_empty());
    }

    #[test]
    fn parse_comment_lines() {
        let rules = IgnoreRules::parse("# comment\n   # another\n");
        assert!(rules.patterns().is_empty());
    }

    #[test]
    fn parse_simple_glob_patterns() {
        let rules = IgnoreRules::parse("*.log\ncache/**\n");
        assert_eq!(
            rules.patterns(),
            &[
                IgnorePattern::Exclude("*.log".to_string()),
                IgnorePattern::Exclude("cache/**".to_string())
            ]
        );
    }

    #[test]
    fn parse_negation_patterns() {
        let rules = IgnoreRules::parse("*.log\n!important.log\n");
        assert_eq!(
            rules.patterns(),
            &[
                IgnorePattern::Exclude("*.log".to_string()),
                IgnorePattern::Include("important.log".to_string())
            ]
        );
    }

    #[test]
    fn is_ignored_matches_glob() {
        let rules = IgnoreRules::parse("*.log\n");
        assert!(rules.is_ignored("errors.log"));
        assert!(!rules.is_ignored("errors.txt"));
    }

    #[test]
    fn is_ignored_with_double_star_pattern() {
        let rules = IgnoreRules::parse("**/target/**\n");
        assert!(rules.is_ignored("target/debug/app"));
        assert!(rules.is_ignored("workspace/target/release/app"));
        assert!(!rules.is_ignored("workspace/src/main.rs"));
    }

    #[test]
    fn is_ignored_with_negation() {
        let rules = IgnoreRules::parse("*.log\n!important.log\n");
        assert!(rules.is_ignored("errors.log"));
        assert!(!rules.is_ignored("important.log"));
    }

    #[test]
    fn is_ignored_directory_pattern_trailing_slash() {
        let rules = IgnoreRules::parse("build/\n");
        assert!(rules.is_ignored("build/output.txt"));
        assert!(rules.is_ignored("src/build/output.txt"));
        assert!(!rules.is_ignored("builder/output.txt"));
    }

    #[test]
    fn load_from_nonexistent_file_returns_empty() {
        let path = temp_test_path("othala-ignore-missing");
        let rules = IgnoreRules::load(&path);
        assert!(rules.patterns().is_empty());
    }

    #[test]
    fn merge_combines_rules() {
        let mut rules = IgnoreRules::parse("*.log\n");
        let other = IgnoreRules::parse("!keep.log\n");
        rules.merge(&other);

        assert_eq!(rules.patterns().len(), 2);
        assert!(rules.is_ignored("errors.log"));
        assert!(!rules.is_ignored("keep.log"));
    }

    #[test]
    fn context_paths_config_resolve_paths() {
        let root = temp_test_path("othala-context-paths");
        fs::create_dir_all(root.join(".othala/context")).expect("create context directory");
        fs::write(root.join("AGENTS.md"), "agents").expect("write AGENTS.md");
        fs::write(root.join(".othala/context/MAIN.md"), "main").expect("write MAIN.md");

        let config = ContextPathsConfig::default_config();
        let resolved = config.resolve_paths(&root);

        assert_eq!(resolved.len(), 2);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn display_ignore_rules_format() {
        let rules = IgnoreRules::parse("*.log\n!important.log\n");
        let rendered = display_ignore_rules(&rules);

        assert!(rendered.contains("Idx | Type"));
        assert!(rendered.contains("exclude | *.log"));
        assert!(rendered.contains("include | important.log"));
    }

    #[test]
    fn anchored_pattern_matches_only_from_root() {
        let rules = IgnoreRules::parse("/build/**\n");
        assert!(rules.is_ignored("build/output.bin"));
        assert!(!rules.is_ignored("src/build/output.bin"));
    }

    #[test]
    fn question_pattern_matches_single_character() {
        let rules = IgnoreRules::parse("file?.txt\n");
        assert!(rules.is_ignored("file1.txt"));
        assert!(!rules.is_ignored("file12.txt"));
    }
}
