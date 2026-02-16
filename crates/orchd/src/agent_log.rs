use orch_core::types::TaskId;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_LOG_SIZE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ROTATED_FILES: usize = 5;
const EDGE_PRESERVE_LINES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Added(String),
    Removed(String),
    Unchanged(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffSummary {
    pub added: usize,
    pub removed: usize,
    pub unchanged: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionType {
    Error,
    CodeBlock,
    Decision,
    Summary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySection {
    pub section_type: SectionType,
    pub content: String,
    pub line_range: (usize, usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    pub original_lines: usize,
    pub compacted_lines: usize,
    pub summary: String,
    pub compression_ratio: f64,
}

pub fn agent_log_dir(repo_root: &Path, task_id: &TaskId) -> PathBuf {
    repo_root.join(".othala/agent-output").join(&task_id.0)
}

pub fn append_agent_output(
    repo_root: &Path,
    task_id: &TaskId,
    lines: &[String],
) -> std::io::Result<()> {
    let dir = agent_log_dir(repo_root, task_id);
    fs::create_dir_all(&dir)?;
    let path = dir.join("latest.log");
    let _ = rotate_log_if_needed(&path)?;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    for line in lines {
        writeln!(file, "{}", line)?;
    }
    Ok(())
}

pub fn rotate_log_if_needed(log_path: &Path) -> std::io::Result<bool> {
    let metadata = std::fs::metadata(log_path);
    match metadata {
        Ok(m) if m.len() > MAX_LOG_SIZE_BYTES => {
            rotate_log(log_path)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn rotate_log(log_path: &Path) -> std::io::Result<()> {
    for i in (1..MAX_ROTATED_FILES).rev() {
        let from = log_path.with_extension(format!("log.{i}"));
        let to = log_path.with_extension(format!("log.{}", i + 1));
        if from.exists() {
            std::fs::rename(&from, &to)?;
        }
    }

    let rotated = log_path.with_extension("log.1");
    std::fs::rename(log_path, &rotated)?;
    Ok(())
}

pub fn list_rotated_logs(log_dir: &Path) -> Vec<PathBuf> {
    let mut logs = Vec::new();
    let latest = log_dir.join("latest.log");
    if latest.exists() {
        logs.push(latest);
    }
    for i in 1..=MAX_ROTATED_FILES {
        let rotated = log_dir.join(format!("latest.log.{i}"));
        if rotated.exists() {
            logs.push(rotated);
        }
    }
    logs
}

pub fn total_log_size(log_dir: &Path) -> u64 {
    list_rotated_logs(log_dir)
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

pub fn read_agent_log(repo_root: &Path, task_id: &TaskId) -> std::io::Result<String> {
    let path = agent_log_dir(repo_root, task_id).join("latest.log");
    fs::read_to_string(&path)
}

pub fn save_compacted_summary(
    repo_root: &Path,
    task_id: &TaskId,
    summary: &str,
) -> std::io::Result<PathBuf> {
    let dir = agent_log_dir(repo_root, task_id);
    fs::create_dir_all(&dir)?;
    let path = dir.join("compacted.log");
    fs::write(&path, summary)?;
    Ok(path)
}

pub fn extract_key_sections(content: &str) -> Vec<KeySection> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut sections = Vec::new();
    let mut i = 0usize;

    while i < lines.len() {
        let trimmed = lines[i].trim();

        if trimmed.starts_with("```") {
            let start = i;
            let mut end = i;
            i += 1;
            while i < lines.len() {
                end = i;
                if lines[i].trim().starts_with("```") {
                    i += 1;
                    break;
                }
                i += 1;
            }
            if end == start && start + 1 == lines.len() {
                end = start;
            }

            sections.push(KeySection {
                section_type: SectionType::CodeBlock,
                content: lines[start..=end].join("\n"),
                line_range: (start + 1, end + 1),
            });
            continue;
        }

        if is_error_line(trimmed) {
            sections.push(KeySection {
                section_type: SectionType::Error,
                content: lines[i].to_string(),
                line_range: (i + 1, i + 1),
            });
        } else if is_decision_line(trimmed) {
            sections.push(KeySection {
                section_type: SectionType::Decision,
                content: lines[i].to_string(),
                line_range: (i + 1, i + 1),
            });
        } else if is_summary_line(trimmed) {
            sections.push(KeySection {
                section_type: SectionType::Summary,
                content: lines[i].to_string(),
                line_range: (i + 1, i + 1),
            });
        }

        i += 1;
    }

    sections
}

pub fn compact_context(lines: &[String], max_lines: usize) -> CompactionResult {
    let original_lines = lines.len();

    if max_lines == 0 || original_lines == 0 {
        return CompactionResult {
            original_lines,
            compacted_lines: 0,
            summary: String::new(),
            compression_ratio: if original_lines == 0 { 1.0 } else { 0.0 },
        };
    }

    if original_lines <= max_lines {
        let summary = lines.join("\n");
        return CompactionResult {
            original_lines,
            compacted_lines: original_lines,
            summary,
            compression_ratio: 1.0,
        };
    }

    let (head_count, tail_count) = edge_line_counts(original_lines, max_lines);
    let middle_start = head_count;
    let middle_end_exclusive = original_lines.saturating_sub(tail_count);

    let mut compacted = Vec::with_capacity(max_lines);
    compacted.extend(lines[..head_count].iter().cloned());

    let mut remaining_middle = max_lines.saturating_sub(head_count + tail_count);
    if remaining_middle > 0 && middle_start < middle_end_exclusive {
        let full_content = lines.join("\n");
        let mut key_sections: Vec<KeySection> = extract_key_sections(&full_content)
            .into_iter()
            .filter(|section| {
                let (start, end) = section.line_range;
                start > middle_start && end <= middle_end_exclusive
            })
            .collect();

        key_sections.sort_by_key(|section| {
            (
                section_priority(section.section_type),
                section.line_range.0,
                section.line_range.1,
            )
        });

        let mut selected_ranges: Vec<(usize, usize)> = Vec::new();
        for section in key_sections {
            let (start, end) = section.line_range;
            if ranges_overlap(&selected_ranges, start, end) {
                continue;
            }

            let len = end.saturating_sub(start) + 1;
            if len <= remaining_middle {
                selected_ranges.push((start, end));
                remaining_middle -= len;
            }

            if remaining_middle == 0 {
                break;
            }
        }

        selected_ranges.sort_by_key(|(start, _)| *start);
        for (start, end) in selected_ranges {
            for line in &lines[(start - 1)..end] {
                compacted.push(line.clone());
            }
        }
    }

    if tail_count > 0 {
        compacted.extend(lines[original_lines - tail_count..].iter().cloned());
    }

    if compacted.len() > max_lines {
        compacted.truncate(max_lines);
    }

    let compacted_lines = compacted.len();
    let summary = compacted.join("\n");
    let compression_ratio = if original_lines == 0 {
        1.0
    } else {
        compacted_lines as f64 / original_lines as f64
    };

    CompactionResult {
        original_lines,
        compacted_lines,
        summary,
        compression_ratio,
    }
}

pub fn tail_agent_log(
    repo_root: &Path,
    task_id: &TaskId,
    n: usize,
) -> std::io::Result<Vec<String>> {
    let content = read_agent_log(repo_root, task_id)?;
    let lines: Vec<String> = content.lines().map(String::from).collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].to_vec())
}

fn edge_line_counts(total_lines: usize, max_lines: usize) -> (usize, usize) {
    if total_lines <= max_lines {
        return (total_lines, 0);
    }

    if max_lines <= EDGE_PRESERVE_LINES * 2 {
        let head = max_lines.div_ceil(2);
        let tail = max_lines.saturating_sub(head);
        return (head, tail);
    }

    let head = EDGE_PRESERVE_LINES.min(total_lines);
    let tail = EDGE_PRESERVE_LINES.min(total_lines.saturating_sub(head));
    (head, tail)
}

fn section_priority(section_type: SectionType) -> usize {
    match section_type {
        SectionType::Error => 0,
        SectionType::Decision => 1,
        SectionType::CodeBlock => 2,
        SectionType::Summary => 3,
    }
}

fn ranges_overlap(ranges: &[(usize, usize)], start: usize, end: usize) -> bool {
    ranges
        .iter()
        .any(|(existing_start, existing_end)| start <= *existing_end && end >= *existing_start)
}

fn is_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("error")
        || lower.contains("panic")
        || lower.contains("failed")
        || lower.contains("exception")
        || lower.contains("traceback")
}

fn is_decision_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("[patch_ready]")
        || lower.contains("[tests_passed]")
        || lower.contains("[blocked]")
        || lower.contains("[done]")
        || lower.contains("[needs_human]")
        || lower.starts_with("decision:")
        || lower.contains("decided")
        || lower.starts_with("next step:")
}

fn is_summary_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("summary:") || lower.starts_with("tl;dr") || lower.starts_with("recap:")
}

pub fn diff_agent_outputs(lines_a: &[String], lines_b: &[String]) -> Vec<DiffLine> {
    let n = lines_a.len();
    let m = lines_b.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];

    for i in (0..n).rev() {
        for j in (0..m).rev() {
            if lines_a[i] == lines_b[j] {
                lcs[i][j] = lcs[i + 1][j + 1] + 1;
            } else {
                lcs[i][j] = lcs[i + 1][j].max(lcs[i][j + 1]);
            }
        }
    }

    let mut diff = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);

    while i < n && j < m {
        if lines_a[i] == lines_b[j] {
            diff.push(DiffLine::Unchanged(lines_a[i].clone()));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            diff.push(DiffLine::Removed(lines_a[i].clone()));
            i += 1;
        } else {
            diff.push(DiffLine::Added(lines_b[j].clone()));
            j += 1;
        }
    }

    while i < n {
        diff.push(DiffLine::Removed(lines_a[i].clone()));
        i += 1;
    }

    while j < m {
        diff.push(DiffLine::Added(lines_b[j].clone()));
        j += 1;
    }

    diff
}

pub fn format_diff(diff: &[DiffLine]) -> String {
    diff.iter()
        .map(|line| match line {
            DiffLine::Added(text) => format!("+ {text}"),
            DiffLine::Removed(text) => format!("- {text}"),
            DiffLine::Unchanged(text) => format!("  {text}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn diff_summary(diff: &[DiffLine]) -> DiffSummary {
    let mut summary = DiffSummary {
        added: 0,
        removed: 0,
        unchanged: 0,
    };

    for line in diff {
        match line {
            DiffLine::Added(_) => summary.added += 1,
            DiffLine::Removed(_) => summary.removed += 1,
            DiffLine::Unchanged(_) => summary.unchanged += 1,
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_repo_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("othala-agent-log-test-{nanos}"))
    }

    #[test]
    fn append_creates_directory_and_file() {
        let repo_root = unique_test_repo_root();
        let task_id = TaskId::new("task-append");
        let lines = vec!["first".to_string(), "second".to_string()];

        append_agent_output(&repo_root, &task_id, &lines).expect("append should succeed");

        let log_path = agent_log_dir(&repo_root, &task_id).join("latest.log");
        assert!(log_path.exists(), "expected log file to exist");

        let content = fs::read_to_string(&log_path).expect("expected to read log file");
        assert_eq!(content, "first\nsecond\n");

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn tail_returns_last_n_lines() {
        let repo_root = unique_test_repo_root();
        let task_id = TaskId::new("task-tail");
        let lines = vec![
            "line1".to_string(),
            "line2".to_string(),
            "line3".to_string(),
            "line4".to_string(),
        ];

        append_agent_output(&repo_root, &task_id, &lines).expect("append should succeed");

        let tail = tail_agent_log(&repo_root, &task_id, 2).expect("tail should succeed");
        assert_eq!(tail, vec!["line3".to_string(), "line4".to_string()]);

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn read_nonexistent_log_returns_error() {
        let repo_root = unique_test_repo_root();
        let task_id = TaskId::new("task-missing");

        let result = read_agent_log(&repo_root, &task_id);
        assert!(result.is_err(), "expected missing log file error");
    }

    #[test]
    fn rotate_log_shifts_files() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-rotate-shift");
        fs::create_dir_all(&log_dir).expect("create log dir");

        fs::write(log_dir.join("latest.log"), "latest").expect("write latest");
        fs::write(log_dir.join("latest.log.1"), "one").expect("write one");
        fs::write(log_dir.join("latest.log.2"), "two").expect("write two");

        let latest_path = log_dir.join("latest.log");
        rotate_log(&latest_path).expect("rotate should succeed");

        assert!(!latest_path.exists());
        assert_eq!(fs::read_to_string(log_dir.join("latest.log.1")).expect("read .1"), "latest");
        assert_eq!(fs::read_to_string(log_dir.join("latest.log.2")).expect("read .2"), "one");
        assert_eq!(fs::read_to_string(log_dir.join("latest.log.3")).expect("read .3"), "two");

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn rotate_log_respects_max_files() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-rotate-max");
        fs::create_dir_all(&log_dir).expect("create log dir");

        fs::write(log_dir.join("latest.log"), "latest").expect("write latest");
        for i in 1..=MAX_ROTATED_FILES {
            fs::write(log_dir.join(format!("latest.log.{i}")), format!("old-{i}"))
                .expect("write rotated file");
        }

        let latest_path = log_dir.join("latest.log");
        rotate_log(&latest_path).expect("rotate should succeed");

        assert!(!log_dir.join(format!("latest.log.{}", MAX_ROTATED_FILES + 1)).exists());
        assert_eq!(
            fs::read_to_string(log_dir.join(format!("latest.log.{MAX_ROTATED_FILES}")))
                .expect("read max file"),
            format!("old-{}", MAX_ROTATED_FILES - 1)
        );

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn rotate_log_if_needed_skips_small_files() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-rotate-small");
        fs::create_dir_all(&log_dir).expect("create log dir");

        let latest_path = log_dir.join("latest.log");
        fs::write(&latest_path, "small").expect("write small log");

        let rotated = rotate_log_if_needed(&latest_path).expect("rotation check should succeed");
        assert!(!rotated);
        assert!(latest_path.exists());
        assert!(!log_dir.join("latest.log.1").exists());

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn list_rotated_logs_finds_all() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-list");
        fs::create_dir_all(&log_dir).expect("create log dir");

        fs::write(log_dir.join("latest.log"), "latest").expect("write latest");
        fs::write(log_dir.join("latest.log.1"), "one").expect("write one");
        fs::write(log_dir.join("latest.log.3"), "three").expect("write three");

        let logs = list_rotated_logs(&log_dir);
        let expected = vec![
            log_dir.join("latest.log"),
            log_dir.join("latest.log.1"),
            log_dir.join("latest.log.3"),
        ];
        assert_eq!(logs, expected);

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn total_log_size_sums_correctly() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-size");
        fs::create_dir_all(&log_dir).expect("create log dir");

        fs::write(log_dir.join("latest.log"), b"abc").expect("write latest");
        fs::write(log_dir.join("latest.log.1"), b"12345").expect("write rotated");
        fs::write(log_dir.join("latest.log.4"), b"xy").expect("write rotated");

        assert_eq!(total_log_size(&log_dir), 10);

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn rotate_log_if_needed_rotates_large_files() {
        let repo_root = unique_test_repo_root();
        let log_dir = repo_root.join(".othala/agent-output/task-rotate-large");
        fs::create_dir_all(&log_dir).expect("create log dir");
        let latest_path = log_dir.join("latest.log");

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&latest_path)
            .expect("open latest");
        file.set_len(MAX_LOG_SIZE_BYTES + 1).expect("inflate file");
        std::io::Write::write_all(&mut file, b"x").expect("touch file");

        let rotated = rotate_log_if_needed(&latest_path).expect("rotation check should succeed");
        assert!(rotated);
        assert!(!latest_path.exists());
        assert!(log_dir.join("latest.log.1").exists());

        let _ = fs::remove_dir_all(&repo_root);
    }

    #[test]
    fn diff_agent_outputs_computes_line_changes() {
        let old_lines = vec![
            "line-a".to_string(),
            "line-b".to_string(),
            "line-c".to_string(),
        ];
        let new_lines = vec![
            "line-a".to_string(),
            "line-c".to_string(),
            "line-d".to_string(),
        ];

        let diff = diff_agent_outputs(&old_lines, &new_lines);
        assert_eq!(
            diff,
            vec![
                DiffLine::Unchanged("line-a".to_string()),
                DiffLine::Removed("line-b".to_string()),
                DiffLine::Unchanged("line-c".to_string()),
                DiffLine::Added("line-d".to_string()),
            ]
        );
    }

    #[test]
    fn format_diff_renders_expected_markers() {
        let diff = vec![
            DiffLine::Unchanged("same".to_string()),
            DiffLine::Removed("old".to_string()),
            DiffLine::Added("new".to_string()),
        ];

        assert_eq!(format_diff(&diff), "  same\n- old\n+ new");
    }

    #[test]
    fn diff_summary_counts_all_variants() {
        let diff = vec![
            DiffLine::Added("a".to_string()),
            DiffLine::Added("b".to_string()),
            DiffLine::Removed("c".to_string()),
            DiffLine::Unchanged("d".to_string()),
            DiffLine::Unchanged("e".to_string()),
        ];

        assert_eq!(
            diff_summary(&diff),
            DiffSummary {
                added: 2,
                removed: 1,
                unchanged: 2,
            }
        );
    }

    #[test]
    fn diff_agent_outputs_handles_empty_inputs() {
        let old_lines = Vec::<String>::new();
        let new_lines = Vec::<String>::new();

        let diff = diff_agent_outputs(&old_lines, &new_lines);
        assert!(diff.is_empty());
        assert_eq!(
            diff_summary(&diff),
            DiffSummary {
                added: 0,
                removed: 0,
                unchanged: 0,
            }
        );
        assert_eq!(format_diff(&diff), "");
    }

    #[test]
    fn diff_agent_outputs_handles_identical_inputs() {
        let old_lines = vec!["same-1".to_string(), "same-2".to_string()];
        let new_lines = vec!["same-1".to_string(), "same-2".to_string()];

        let diff = diff_agent_outputs(&old_lines, &new_lines);
        assert_eq!(
            diff,
            vec![
                DiffLine::Unchanged("same-1".to_string()),
                DiffLine::Unchanged("same-2".to_string()),
            ]
        );
    }

    #[test]
    fn extract_key_sections_detects_error_code_decision_and_summary() {
        let content = [
            "intro",
            "ERROR: failed to apply patch",
            "[patch_ready]",
            "summary: ship it",
            "```rust",
            "fn main() {}",
            "```",
        ]
        .join("\n");

        let sections = extract_key_sections(&content);
        assert!(sections
            .iter()
            .any(|s| s.section_type == SectionType::Error && s.line_range == (2, 2)));
        assert!(sections
            .iter()
            .any(|s| s.section_type == SectionType::Decision && s.line_range == (3, 3)));
        assert!(sections
            .iter()
            .any(|s| s.section_type == SectionType::Summary && s.line_range == (4, 4)));
        assert!(sections
            .iter()
            .any(|s| s.section_type == SectionType::CodeBlock && s.line_range == (5, 7)));
    }

    #[test]
    fn extract_key_sections_handles_unclosed_code_fence() {
        let content = ["before", "```python", "print('hello')"].join("\n");
        let sections = extract_key_sections(&content);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].section_type, SectionType::CodeBlock);
        assert_eq!(sections[0].line_range, (2, 3));
    }

    #[test]
    fn compact_context_keeps_all_lines_when_within_limit() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let compacted = compact_context(&lines, 10);

        assert_eq!(compacted.original_lines, 3);
        assert_eq!(compacted.compacted_lines, 3);
        assert_eq!(compacted.summary, "a\nb\nc");
        assert_eq!(compacted.compression_ratio, 1.0);
    }

    #[test]
    fn compact_context_keeps_edges_and_middle_error_signal() {
        let mut lines = (1..=70)
            .map(|idx| format!("line-{idx}"))
            .collect::<Vec<String>>();
        lines[34] = "ERROR: model timeout".to_string();

        let compacted = compact_context(&lines, 50);
        let compacted_lines: Vec<&str> = compacted.summary.lines().collect();

        assert_eq!(compacted.original_lines, 70);
        assert!(compacted.compacted_lines <= 50);
        assert_eq!(compacted_lines.first().copied(), Some("line-1"));
        assert_eq!(compacted_lines.get(19).copied(), Some("line-20"));
        assert!(compacted_lines.contains(&"ERROR: model timeout"));
        assert_eq!(compacted_lines.last().copied(), Some("line-70"));
    }

    #[test]
    fn compact_context_with_zero_max_lines_returns_empty_summary() {
        let lines = vec!["line-1".to_string(), "line-2".to_string()];
        let compacted = compact_context(&lines, 0);

        assert_eq!(compacted.original_lines, 2);
        assert_eq!(compacted.compacted_lines, 0);
        assert!(compacted.summary.is_empty());
        assert_eq!(compacted.compression_ratio, 0.0);
    }

    #[test]
    fn save_compacted_summary_writes_compacted_log_file() {
        let repo_root = unique_test_repo_root();
        let task_id = TaskId::new("task-compact-save");
        let summary = "line-a\nline-b";

        let path = save_compacted_summary(&repo_root, &task_id, summary).expect("save compacted");
        assert!(path.ends_with("compacted.log"));
        assert_eq!(fs::read_to_string(path).expect("read compacted"), summary);

        let _ = fs::remove_dir_all(&repo_root);
    }
}
