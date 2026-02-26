//! Mission Vault — parse requirements, track coverage, detect gaps.
//!
//! The vault reads mission specs (markdown files under `.othala/missions/`)
//! and maps each requirement to tasks.  It produces a coverage matrix,
//! detects semantically duplicate requirements, and surfaces uncovered gaps
//! for continuous task seeding.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Requirement model
// ---------------------------------------------------------------------------

/// A single requirement parsed from a mission spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Requirement {
    pub id: String,
    pub text: String,
    pub source_file: String,
    pub line_number: usize,
    pub priority: RequirementPriority,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementPriority {
    Must,
    Should,
    Nice,
}

impl RequirementPriority {
    pub fn from_text(text: &str) -> Self {
        let lower = text.to_ascii_lowercase();
        if lower.contains("must") || lower.contains("critical") || lower.contains("required") {
            RequirementPriority::Must
        } else if lower.contains("should") || lower.contains("important") {
            RequirementPriority::Should
        } else {
            RequirementPriority::Nice
        }
    }
}

// ---------------------------------------------------------------------------
// Requirement parsing
// ---------------------------------------------------------------------------

/// Parse requirements from a markdown mission spec.
///
/// Recognizes:
/// - Checkbox items: `- [ ] requirement text`
/// - Checked items: `- [x] requirement text` (already satisfied)
/// - Headings as context/tags
pub fn parse_requirements(content: &str, source_file: &str) -> Vec<Requirement> {
    let mut requirements = Vec::new();
    let mut current_tags: Vec<String> = Vec::new();
    let mut req_counter = 0u32;

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Track heading context for tags.
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() {
                current_tags = vec![normalize_tag(heading)];
            }
            continue;
        }

        // Parse checkbox items.
        let (is_checkbox, text) = if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            (true, rest.trim())
        } else if let Some(rest) = trimmed.strip_prefix("- [x] ") {
            (true, rest.trim())
        } else if let Some(rest) = trimmed.strip_prefix("- [X] ") {
            (true, rest.trim())
        } else {
            (false, "")
        };

        if is_checkbox && !text.is_empty() {
            req_counter += 1;
            let id = format!(
                "REQ-{}-{}",
                normalize_tag(source_file).replace('/', "-"),
                req_counter
            );

            requirements.push(Requirement {
                id,
                text: text.to_string(),
                source_file: source_file.to_string(),
                line_number: line_idx + 1,
                priority: RequirementPriority::from_text(text),
                tags: current_tags.clone(),
            });
        }
    }

    requirements
}

fn normalize_tag(s: &str) -> String {
    s.to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '/' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Load all requirements from `.othala/missions/` directory.
pub fn load_all_requirements(repo_root: &Path) -> Vec<Requirement> {
    let missions_dir = repo_root.join(".othala/missions");
    let mut all = Vec::new();

    if !missions_dir.exists() {
        return all;
    }

    if let Ok(entries) = std::fs::read_dir(&missions_dir) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "md")
                    .unwrap_or(false)
            })
            .map(|e| e.path())
            .collect();
        files.sort();

        for path in files {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let rel = path
                    .strip_prefix(&missions_dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                all.extend(parse_requirements(&content, &rel));
            }
        }
    }

    all
}

// ---------------------------------------------------------------------------
// Coverage matrix
// ---------------------------------------------------------------------------

/// Coverage status of a single requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Covered,
    Partial,
    Uncovered,
}

/// A row in the coverage matrix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageEntry {
    pub requirement_id: String,
    pub requirement_text: String,
    pub priority: RequirementPriority,
    pub status: CoverageStatus,
    pub covering_tasks: Vec<String>,
    pub tags: Vec<String>,
}

/// Full coverage matrix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageMatrix {
    pub entries: Vec<CoverageEntry>,
    pub total_requirements: usize,
    pub covered: usize,
    pub partial: usize,
    pub uncovered: usize,
    pub coverage_percent: f64,
    pub generated_at: DateTime<Utc>,
}

/// Task info for coverage mapping.
#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub id: String,
    pub title: String,
    pub state: String,
}

/// Build coverage matrix by matching requirements to tasks.
///
/// Uses keyword overlap to determine coverage. A requirement is:
/// - `Covered` if at least one task matches with high similarity
/// - `Partial` if a task matches loosely
/// - `Uncovered` if no task matches
pub fn build_coverage_matrix(
    requirements: &[Requirement],
    tasks: &[TaskInfo],
) -> CoverageMatrix {
    let mut entries = Vec::new();

    for req in requirements {
        let req_words = extract_keywords(&req.text);
        let mut covering = Vec::new();
        let mut best_score = 0.0f64;

        for task in tasks {
            let task_words = extract_keywords(&task.title);
            let score = keyword_overlap(&req_words, &task_words);
            if score > 0.2 {
                covering.push(task.id.clone());
                if score > best_score {
                    best_score = score;
                }
            }
        }

        let status = if best_score >= 0.4 {
            CoverageStatus::Covered
        } else if best_score > 0.0 {
            CoverageStatus::Partial
        } else {
            CoverageStatus::Uncovered
        };

        entries.push(CoverageEntry {
            requirement_id: req.id.clone(),
            requirement_text: req.text.clone(),
            priority: req.priority,
            status,
            covering_tasks: covering,
            tags: req.tags.clone(),
        });
    }

    let total = entries.len();
    let covered = entries
        .iter()
        .filter(|e| e.status == CoverageStatus::Covered)
        .count();
    let partial = entries
        .iter()
        .filter(|e| e.status == CoverageStatus::Partial)
        .count();
    let uncovered = entries
        .iter()
        .filter(|e| e.status == CoverageStatus::Uncovered)
        .count();

    let coverage_percent = if total > 0 {
        (covered as f64 + partial as f64 * 0.5) / total as f64 * 100.0
    } else {
        0.0
    };

    CoverageMatrix {
        entries,
        total_requirements: total,
        covered,
        partial,
        uncovered,
        coverage_percent,
        generated_at: Utc::now(),
    }
}

/// Extract meaningful keywords from text (lowercase, deduplicated).
fn extract_keywords(text: &str) -> HashSet<String> {
    let stop_words: HashSet<&str> = [
        "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
        "from", "is", "are", "was", "were", "be", "been", "has", "have", "had", "do", "does",
        "did", "will", "would", "could", "should", "may", "might", "must", "shall", "can",
        "not", "no", "all", "each", "every", "this", "that", "it", "its", "as", "over",
    ]
    .into_iter()
    .collect();

    text.to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| w.len() > 2 && !stop_words.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Compute Jaccard-like overlap between two keyword sets (0.0–1.0).
fn keyword_overlap(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

// ---------------------------------------------------------------------------
// Semantic dedup
// ---------------------------------------------------------------------------

/// A group of requirements that appear to be duplicates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub canonical_id: String,
    pub duplicate_ids: Vec<String>,
    pub similarity: f64,
}

/// Detect semantically duplicate requirements.
///
/// Uses keyword overlap to find pairs with similarity above `threshold`.
pub fn detect_duplicates(requirements: &[Requirement], threshold: f64) -> Vec<DuplicateGroup> {
    let mut groups: Vec<DuplicateGroup> = Vec::new();
    let mut assigned: HashSet<String> = HashSet::new();

    let keyword_sets: Vec<HashSet<String>> = requirements
        .iter()
        .map(|r| extract_keywords(&r.text))
        .collect();

    for i in 0..requirements.len() {
        if assigned.contains(&requirements[i].id) {
            continue;
        }

        let mut duplicates = Vec::new();
        let mut best_sim = 0.0f64;

        for j in (i + 1)..requirements.len() {
            if assigned.contains(&requirements[j].id) {
                continue;
            }
            let sim = keyword_overlap(&keyword_sets[i], &keyword_sets[j]);
            if sim >= threshold {
                duplicates.push(requirements[j].id.clone());
                assigned.insert(requirements[j].id.clone());
                if sim > best_sim {
                    best_sim = sim;
                }
            }
        }

        if !duplicates.is_empty() {
            assigned.insert(requirements[i].id.clone());
            groups.push(DuplicateGroup {
                canonical_id: requirements[i].id.clone(),
                duplicate_ids: duplicates,
                similarity: best_sim,
            });
        }
    }

    groups
}

// ---------------------------------------------------------------------------
// Gap detection & task seeding
// ---------------------------------------------------------------------------

/// A suggested task to fill a gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapSuggestion {
    pub requirement_id: String,
    pub requirement_text: String,
    pub priority: RequirementPriority,
    pub suggested_title: String,
}

/// Identify uncovered requirements and suggest tasks.
pub fn detect_gaps(matrix: &CoverageMatrix) -> Vec<GapSuggestion> {
    matrix
        .entries
        .iter()
        .filter(|e| e.status == CoverageStatus::Uncovered)
        .map(|e| GapSuggestion {
            requirement_id: e.requirement_id.clone(),
            requirement_text: e.requirement_text.clone(),
            priority: e.priority,
            suggested_title: format!("Implement: {}", truncate_text(&e.requirement_text, 80)),
        })
        .collect()
}

fn truncate_text(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

// ---------------------------------------------------------------------------
// Composite report
// ---------------------------------------------------------------------------

/// Full mission completeness report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionReport {
    pub coverage: CoverageMatrix,
    pub duplicates: Vec<DuplicateGroup>,
    pub gaps: Vec<GapSuggestion>,
    pub generated_at: DateTime<Utc>,
}

/// Build a complete mission report.
pub fn build_mission_report(
    requirements: &[Requirement],
    tasks: &[TaskInfo],
    dedup_threshold: f64,
) -> MissionReport {
    let coverage = build_coverage_matrix(requirements, tasks);
    let duplicates = detect_duplicates(requirements, dedup_threshold);
    let gaps = detect_gaps(&coverage);

    MissionReport {
        coverage,
        duplicates,
        gaps,
        generated_at: Utc::now(),
    }
}

/// Render mission report as human-readable text.
pub fn render_mission_report(report: &MissionReport) -> String {
    let mut out = String::new();

    out.push_str("\x1b[35m── Mission Completeness Report ──\x1b[0m\n\n");

    // Coverage summary.
    let cov = &report.coverage;
    out.push_str(&format!(
        "  Coverage: {:.0}%  ({} covered, {} partial, {} uncovered / {} total)\n\n",
        cov.coverage_percent, cov.covered, cov.partial, cov.uncovered, cov.total_requirements
    ));

    // Coverage table.
    if !cov.entries.is_empty() {
        out.push_str("  \x1b[35mRequirements\x1b[0m\n");
        for entry in &cov.entries {
            let (icon, color) = match entry.status {
                CoverageStatus::Covered => ("✓", "\x1b[32m"),
                CoverageStatus::Partial => ("◐", "\x1b[33m"),
                CoverageStatus::Uncovered => ("✗", "\x1b[31m"),
            };
            let tasks_str = if entry.covering_tasks.is_empty() {
                String::new()
            } else {
                format!(" → {}", entry.covering_tasks.join(", "))
            };
            out.push_str(&format!(
                "    {color}{icon}\x1b[0m [{:?}] {}{}\n",
                entry.priority,
                truncate_text(&entry.requirement_text, 60),
                tasks_str,
            ));
        }
        out.push('\n');
    }

    // Duplicates.
    if !report.duplicates.is_empty() {
        out.push_str(&format!(
            "  \x1b[33m⚠ {} duplicate group(s) detected\x1b[0m\n",
            report.duplicates.len()
        ));
        for group in &report.duplicates {
            out.push_str(&format!(
                "    {} ↔ {} (sim: {:.0}%)\n",
                group.canonical_id,
                group.duplicate_ids.join(", "),
                group.similarity * 100.0
            ));
        }
        out.push('\n');
    }

    // Gaps.
    if !report.gaps.is_empty() {
        out.push_str(&format!(
            "  \x1b[31m{} gap(s) — uncovered requirements\x1b[0m\n",
            report.gaps.len()
        ));
        for gap in &report.gaps {
            out.push_str(&format!(
                "    [{:?}] {}\n      → {}\n",
                gap.priority,
                truncate_text(&gap.requirement_text, 60),
                gap.suggested_title,
            ));
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_requirements_from_checkboxes() {
        let content = "\
# Install Wizard
- [ ] Must check all dependencies on startup
- [ ] Should show readiness score
- [x] Nice to have: colored output
# QA Pipeline
- [ ] Must classify failures
- [ ] Should auto-retry flaky tests
";
        let reqs = parse_requirements(content, "mission.md");
        assert_eq!(reqs.len(), 5);
        assert_eq!(reqs[0].text, "Must check all dependencies on startup");
        assert_eq!(reqs[0].priority, RequirementPriority::Must);
        assert_eq!(reqs[0].tags, vec!["install-wizard"]);
        assert_eq!(reqs[3].tags, vec!["qa-pipeline"]);
    }

    #[test]
    fn parse_requirements_empty_content() {
        let reqs = parse_requirements("No checkboxes here.\nJust text.", "empty.md");
        assert!(reqs.is_empty());
    }

    #[test]
    fn parse_requirements_generates_unique_ids() {
        let content = "- [ ] First\n- [ ] Second\n- [ ] Third\n";
        let reqs = parse_requirements(content, "test.md");
        assert_eq!(reqs.len(), 3);
        let ids: HashSet<String> = reqs.iter().map(|r| r.id.clone()).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn priority_detection() {
        assert_eq!(
            RequirementPriority::from_text("Must validate inputs"),
            RequirementPriority::Must
        );
        assert_eq!(
            RequirementPriority::from_text("Critical: handle errors"),
            RequirementPriority::Must
        );
        assert_eq!(
            RequirementPriority::from_text("Should log warnings"),
            RequirementPriority::Should
        );
        assert_eq!(
            RequirementPriority::from_text("Add nice animations"),
            RequirementPriority::Nice
        );
    }

    #[test]
    fn keyword_extraction() {
        let keywords = extract_keywords("The quick brown fox jumps over the lazy dog");
        assert!(keywords.contains("quick"));
        assert!(keywords.contains("brown"));
        assert!(keywords.contains("fox"));
        // Stop words excluded.
        assert!(!keywords.contains("the"));
        assert!(!keywords.contains("over"));
    }

    #[test]
    fn keyword_overlap_identical() {
        let a = extract_keywords("install wizard dependencies");
        let b = extract_keywords("install wizard dependencies");
        let score = keyword_overlap(&a, &b);
        assert!((score - 1.0).abs() < 0.01);
    }

    #[test]
    fn keyword_overlap_disjoint() {
        let a = extract_keywords("install wizard dependencies");
        let b = extract_keywords("graphite branch tracking");
        let score = keyword_overlap(&a, &b);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn keyword_overlap_partial() {
        let a = extract_keywords("install wizard readiness checks");
        let b = extract_keywords("wizard readiness score display");
        let score = keyword_overlap(&a, &b);
        assert!(score > 0.0);
        assert!(score < 1.0);
    }

    #[test]
    fn coverage_matrix_all_covered() {
        let reqs = vec![Requirement {
            id: "REQ-1".to_string(),
            text: "install wizard dependencies".to_string(),
            source_file: "m.md".to_string(),
            line_number: 1,
            priority: RequirementPriority::Must,
            tags: vec![],
        }];

        let tasks = vec![TaskInfo {
            id: "T1".to_string(),
            title: "Install wizard with dependency checks".to_string(),
            state: "merged".to_string(),
        }];

        let matrix = build_coverage_matrix(&reqs, &tasks);
        assert_eq!(matrix.total_requirements, 1);
        assert_eq!(matrix.covered, 1);
        assert_eq!(matrix.uncovered, 0);
        assert!(matrix.coverage_percent > 90.0);
    }

    #[test]
    fn coverage_matrix_uncovered() {
        let reqs = vec![Requirement {
            id: "REQ-1".to_string(),
            text: "install wizard dependencies".to_string(),
            source_file: "m.md".to_string(),
            line_number: 1,
            priority: RequirementPriority::Must,
            tags: vec![],
        }];

        let tasks = vec![TaskInfo {
            id: "T1".to_string(),
            title: "Graphite branch tracking hardening".to_string(),
            state: "chatting".to_string(),
        }];

        let matrix = build_coverage_matrix(&reqs, &tasks);
        assert_eq!(matrix.uncovered, 1);
        assert_eq!(matrix.covered, 0);
    }

    #[test]
    fn coverage_matrix_empty_requirements() {
        let matrix = build_coverage_matrix(&[], &[]);
        assert_eq!(matrix.total_requirements, 0);
        assert_eq!(matrix.coverage_percent, 0.0);
    }

    #[test]
    fn detect_duplicates_finds_similar() {
        let reqs = vec![
            Requirement {
                id: "REQ-1".to_string(),
                text: "Install wizard dependency checking on startup".to_string(),
                source_file: "a.md".to_string(),
                line_number: 1,
                priority: RequirementPriority::Must,
                tags: vec![],
            },
            Requirement {
                id: "REQ-2".to_string(),
                text: "Dependency checking in install wizard during startup".to_string(),
                source_file: "b.md".to_string(),
                line_number: 1,
                priority: RequirementPriority::Must,
                tags: vec![],
            },
            Requirement {
                id: "REQ-3".to_string(),
                text: "Graphite branch repair command".to_string(),
                source_file: "c.md".to_string(),
                line_number: 1,
                priority: RequirementPriority::Should,
                tags: vec![],
            },
        ];

        let groups = detect_duplicates(&reqs, 0.5);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].canonical_id, "REQ-1");
        assert!(groups[0].duplicate_ids.contains(&"REQ-2".to_string()));
    }

    #[test]
    fn detect_duplicates_no_dupes() {
        let reqs = vec![
            Requirement {
                id: "REQ-1".to_string(),
                text: "Install wizard".to_string(),
                source_file: "a.md".to_string(),
                line_number: 1,
                priority: RequirementPriority::Must,
                tags: vec![],
            },
            Requirement {
                id: "REQ-2".to_string(),
                text: "Graphite repair".to_string(),
                source_file: "b.md".to_string(),
                line_number: 1,
                priority: RequirementPriority::Should,
                tags: vec![],
            },
        ];

        let groups = detect_duplicates(&reqs, 0.5);
        assert!(groups.is_empty());
    }

    #[test]
    fn gap_detection_identifies_uncovered() {
        let reqs = vec![
            Requirement {
                id: "REQ-1".to_string(),
                text: "install wizard".to_string(),
                source_file: "m.md".to_string(),
                line_number: 1,
                priority: RequirementPriority::Must,
                tags: vec![],
            },
            Requirement {
                id: "REQ-2".to_string(),
                text: "end-to-end testing framework".to_string(),
                source_file: "m.md".to_string(),
                line_number: 2,
                priority: RequirementPriority::Should,
                tags: vec![],
            },
        ];

        let tasks = vec![TaskInfo {
            id: "T1".to_string(),
            title: "Install wizard with readiness checks".to_string(),
            state: "merged".to_string(),
        }];

        let matrix = build_coverage_matrix(&reqs, &tasks);
        let gaps = detect_gaps(&matrix);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].requirement_id, "REQ-2");
        assert!(gaps[0].suggested_title.contains("end-to-end"));
    }

    #[test]
    fn build_mission_report_combines_all() {
        let reqs = vec![
            Requirement {
                id: "REQ-1".to_string(),
                text: "install wizard dependencies".to_string(),
                source_file: "m.md".to_string(),
                line_number: 1,
                priority: RequirementPriority::Must,
                tags: vec!["wizard".to_string()],
            },
            Requirement {
                id: "REQ-2".to_string(),
                text: "e2e test suite".to_string(),
                source_file: "m.md".to_string(),
                line_number: 2,
                priority: RequirementPriority::Should,
                tags: vec![],
            },
        ];

        let tasks = vec![TaskInfo {
            id: "T1".to_string(),
            title: "Install wizard dependency checks".to_string(),
            state: "merged".to_string(),
        }];

        let report = build_mission_report(&reqs, &tasks, 0.5);
        assert_eq!(report.coverage.total_requirements, 2);
        assert!(report.gaps.len() >= 1);
        assert!(report.generated_at <= Utc::now());
    }

    #[test]
    fn render_mission_report_includes_sections() {
        let reqs = vec![Requirement {
            id: "REQ-1".to_string(),
            text: "Must validate all inputs".to_string(),
            source_file: "m.md".to_string(),
            line_number: 1,
            priority: RequirementPriority::Must,
            tags: vec![],
        }];

        let report = build_mission_report(&reqs, &[], 0.5);
        let rendered = render_mission_report(&report);

        assert!(rendered.contains("Mission Completeness Report"));
        assert!(rendered.contains("Coverage:"));
        assert!(rendered.contains("gap(s)"));
    }

    #[test]
    fn report_serializes_to_json() {
        let reqs = vec![Requirement {
            id: "REQ-1".to_string(),
            text: "Test req".to_string(),
            source_file: "m.md".to_string(),
            line_number: 1,
            priority: RequirementPriority::Must,
            tags: vec![],
        }];

        let report = build_mission_report(&reqs, &[], 0.5);
        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("\"coverage\""));
        assert!(json.contains("\"duplicates\""));
        assert!(json.contains("\"gaps\""));
        assert!(json.contains("REQ-1"));

        // Roundtrip.
        let decoded: MissionReport = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.coverage.total_requirements, 1);
    }

    #[test]
    fn normalize_tag_handles_special_chars() {
        assert_eq!(normalize_tag("Install Wizard"), "install-wizard");
        assert_eq!(normalize_tag("QA/Pipeline"), "qa/pipeline");
        assert_eq!(normalize_tag("  foo  "), "foo");
    }

    #[test]
    fn load_all_requirements_empty_dir() {
        let tmp =
            std::env::temp_dir().join(format!("othala-vault-load-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // No .othala/missions/ dir.
        let reqs = load_all_requirements(&tmp);
        assert!(reqs.is_empty());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn load_all_requirements_with_files() {
        let tmp =
            std::env::temp_dir().join(format!("othala-vault-files-{}", std::process::id()));
        let missions_dir = tmp.join(".othala/missions");
        std::fs::create_dir_all(&missions_dir).unwrap();
        std::fs::write(
            missions_dir.join("phase1.md"),
            "# Phase 1\n- [ ] Build the thing\n- [ ] Test the thing\n",
        )
        .unwrap();
        std::fs::write(
            missions_dir.join("phase2.md"),
            "# Phase 2\n- [ ] Ship the thing\n",
        )
        .unwrap();

        let reqs = load_all_requirements(&tmp);
        assert_eq!(reqs.len(), 3);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn keyword_overlap_empty_sets() {
        let empty: HashSet<String> = HashSet::new();
        let non_empty = extract_keywords("hello world test");
        assert_eq!(keyword_overlap(&empty, &non_empty), 0.0);
        assert_eq!(keyword_overlap(&non_empty, &empty), 0.0);
        assert_eq!(keyword_overlap(&empty, &empty), 0.0);
    }
}
