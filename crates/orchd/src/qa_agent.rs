//! Live QA agent — spawns an AI agent that actually runs the system and tests it.
//!
//! Unlike the test_spec system (which validates by reading code), the QA agent
//! spins up processes, runs CLI commands, curls endpoints, inspects git/sqlite
//! state, and reports structured pass/fail results.
//!
//! QA runs **before** (baseline) and **after** (validation) each task:
//! - Before: establish that everything that worked before still works
//! - After: verify baseline still passes + new behavior works
//! - The "after" result becomes the baseline for the next task on that branch

use chrono::{DateTime, Utc};
use orch_agents::{default_adapter_for, detect_common_signal, AgentSignalKind, EpochRequest};
use orch_core::types::{ModelKind, RepoId, TaskId};
use serde::{Deserialize, Serialize};

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Whether this is a baseline run or a validation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QAType {
    /// Run before implementation to establish what currently works.
    Baseline,
    /// Run after implementation to check regression + acceptance.
    Validation,
}

impl std::fmt::Display for QAType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QAType::Baseline => write!(f, "baseline"),
            QAType::Validation => write!(f, "validation"),
        }
    }
}

/// A parsed QA specification (baseline or task-specific).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QASpec {
    /// Raw markdown content.
    pub raw: String,
    /// Parsed test cases from the spec.
    pub tests: Vec<QATestCase>,
}

/// A single test case extracted from a QA spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QATestCase {
    /// Short identifier (e.g., "daemon_startup_banner").
    pub name: String,
    /// Suite / section (e.g., "startup", "tui", "cli").
    pub suite: String,
    /// Human-readable steps to execute.
    pub steps: String,
}

/// Structured result of a QA run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QAResult {
    /// Branch that was tested.
    pub branch: String,
    /// Commit hash at time of test.
    pub commit: String,
    /// When the run happened.
    pub timestamp: DateTime<Utc>,
    /// Per-test results.
    pub tests: Vec<QATestResult>,
    /// Summary counts.
    pub summary: QASummary,
}

/// Per-test result from a QA run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QATestResult {
    /// Test name (matches QATestCase::name).
    pub name: String,
    /// Suite (matches QATestCase::suite).
    pub suite: String,
    /// Whether the test passed.
    pub passed: bool,
    /// Details — error message on failure, timing info on success.
    pub detail: String,
    /// How long the test took in milliseconds.
    pub duration_ms: u64,
}

/// Summary of a QA run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QASummary {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
}

/// Status of a QA agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QAStatus {
    Idle,
    RunningBaseline,
    RunningValidation,
    Completed,
    Failed,
}

/// Mutable state for a per-task QA agent.
pub struct QAState {
    pub status: QAStatus,
    pub qa_type: QAType,
    pub child_handle: Option<Child>,
    pub result_rx: Option<mpsc::Receiver<String>>,
    pub qa_complete: bool,
    /// Accumulated output from the agent — used to parse QA results on completion.
    pub output_buffer: Vec<String>,
    /// When the agent signaled qa_complete. Used to enforce a grace period before killing.
    pub signal_at: Option<Instant>,
}

impl QAState {
    pub fn new(qa_type: QAType) -> Self {
        let status = match qa_type {
            QAType::Baseline => QAStatus::RunningBaseline,
            QAType::Validation => QAStatus::RunningValidation,
        };
        Self {
            status,
            qa_type,
            child_handle: None,
            result_rx: None,
            qa_complete: false,
            output_buffer: Vec::new(),
            signal_at: None,
        }
    }
}

// ---------------------------------------------------------------------------
// File I/O
// ---------------------------------------------------------------------------

/// Root directory for QA artifacts.
pub fn qa_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".othala/qa")
}

/// Load the baseline QA spec from `.othala/qa/baseline.md`.
pub fn load_baseline(repo_root: &Path) -> Option<QASpec> {
    let path = qa_dir(repo_root).join("baseline.md");
    let content = std::fs::read_to_string(path).ok()?;
    Some(parse_qa_spec(&content))
}

/// Load a task-specific QA spec from `.othala/qa/specs/{task_id}.md`.
pub fn load_task_spec(repo_root: &Path, task_id: &TaskId) -> Option<String> {
    let path = qa_dir(repo_root)
        .join("specs")
        .join(format!("{}.md", task_id.0));
    std::fs::read_to_string(path).ok()
}

/// Load the latest QA result for a branch.
pub fn load_latest_result(repo_root: &Path, branch: &str) -> Option<QAResult> {
    let sanitized = sanitize_branch_name(branch);
    let results_dir = qa_dir(repo_root).join("results");

    // Find the most recent result file for this branch.
    let entries = std::fs::read_dir(&results_dir).ok()?;
    let mut latest: Option<(String, QAResult)> = None;

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&sanitized) && name.ends_with(".json") {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Ok(result) = serde_json::from_str::<QAResult>(&content) {
                    match &latest {
                        Some((_, prev)) if result.timestamp > prev.timestamp => {
                            latest = Some((name, result));
                        }
                        None => {
                            latest = Some((name, result));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    latest.map(|(_, result)| result)
}

/// Save a QA result to disk. Returns the path written.
pub fn save_qa_result(repo_root: &Path, result: &QAResult) -> std::io::Result<PathBuf> {
    let results_dir = qa_dir(repo_root).join("results");
    std::fs::create_dir_all(&results_dir)?;

    let sanitized = sanitize_branch_name(&result.branch);
    let short_commit = if result.commit.len() >= 7 {
        &result.commit[..7]
    } else {
        &result.commit
    };
    let filename = format!("{}-{}.json", sanitized, short_commit);
    let path = results_dir.join(&filename);

    let json = serde_json::to_string_pretty(result).map_err(std::io::Error::other)?;
    std::fs::write(&path, json)?;

    // Also save to history.
    let history_dir = qa_dir(repo_root).join("history");
    std::fs::create_dir_all(&history_dir)?;
    let ts = result.timestamp.format("%Y%m%dT%H%M%S").to_string();
    let history_path = history_dir.join(format!("{}.json", ts));
    let json = serde_json::to_string_pretty(result).map_err(std::io::Error::other)?;
    std::fs::write(&history_path, json)?;

    Ok(path)
}

/// Sanitize a branch name for use in filenames.
fn sanitize_branch_name(branch: &str) -> String {
    branch
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Spec parsing
// ---------------------------------------------------------------------------

/// Parse a QA spec markdown file into structured test cases.
///
/// Expected format:
/// ```markdown
/// ## Suite Name
/// - test description with steps
/// - another test
/// ```
///
/// Each `## Section` becomes the suite, and each `- item` becomes a test case.
pub fn parse_qa_spec(content: &str) -> QASpec {
    let mut tests = Vec::new();
    let mut current_suite = String::from("general");

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(heading) = trimmed.strip_prefix("## ") {
            current_suite = heading
                .to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect::<String>()
                .trim_matches('_')
                .to_string();
        } else if let Some(item) = trimmed.strip_prefix("- ") {
            let name = item
                .split(',')
                .next()
                .unwrap_or(item)
                .to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect::<String>()
                .trim_matches('_')
                .to_string();

            // Truncate name to something reasonable.
            let name = if name.len() > 60 {
                name[..60].to_string()
            } else {
                name
            };

            tests.push(QATestCase {
                name,
                suite: current_suite.clone(),
                steps: item.to_string(),
            });
        }
    }

    QASpec {
        raw: content.to_string(),
        tests,
    }
}

// ---------------------------------------------------------------------------
// Result parsing
// ---------------------------------------------------------------------------

/// Parse structured QA output from an agent's raw text.
///
/// Looks for lines matching:
/// `<!-- QA_RESULT: test_name | PASS/FAIL | detail -->`
///
/// Also extracts branch/commit from:
/// `<!-- QA_META: branch | commit -->`
pub fn parse_qa_output(raw: &str) -> QAResult {
    let mut tests = Vec::new();
    let mut branch = String::from("unknown");
    let mut commit = String::from("unknown");

    for line in raw.lines() {
        let trimmed = line.trim();

        // Parse meta line.
        if let Some(rest) = trimmed.strip_prefix("<!-- QA_META:") {
            if let Some(content) = rest.strip_suffix("-->") {
                let parts: Vec<&str> = content.split('|').map(|s| s.trim()).collect();
                if parts.len() >= 2 {
                    branch = parts[0].to_string();
                    commit = parts[1].to_string();
                }
            }
        }

        // Parse result line.
        if let Some(rest) = trimmed.strip_prefix("<!-- QA_RESULT:") {
            if let Some(content) = rest.strip_suffix("-->") {
                let parts: Vec<&str> = content.split('|').map(|s| s.trim()).collect();
                if parts.len() >= 2 {
                    let name = parts[0].to_string();
                    let passed = parts[1].eq_ignore_ascii_case("PASS");
                    let detail = if parts.len() >= 3 {
                        parts[2].to_string()
                    } else {
                        String::new()
                    };

                    // Try to extract suite from name (e.g., "startup.daemon_banner" → suite="startup").
                    let (suite, test_name) = if let Some((s, n)) = name.split_once('.') {
                        (s.to_string(), n.to_string())
                    } else {
                        ("general".to_string(), name)
                    };

                    tests.push(QATestResult {
                        name: test_name,
                        suite,
                        passed,
                        detail,
                        duration_ms: 0,
                    });
                }
            }
        }
    }

    let total = tests.len() as u32;
    let passed_count = tests.iter().filter(|t| t.passed).count() as u32;
    let failed_count = total - passed_count;

    QAResult {
        branch,
        commit,
        timestamp: Utc::now(),
        tests,
        summary: QASummary {
            total,
            passed: passed_count,
            failed: failed_count,
        },
    }
}

// ---------------------------------------------------------------------------
// Prompt building
// ---------------------------------------------------------------------------

/// Build the prompt for a QA agent run.
pub fn build_qa_prompt(
    baseline: &QASpec,
    task_spec: Option<&str>,
    previous_result: Option<&QAResult>,
    repo_root: &Path,
    template_dir: &Path,
) -> String {
    let mut sections: Vec<String> = Vec::new();

    // Load the qa-validator template.
    let template_path = template_dir.join("qa-validator.md");
    if let Ok(template) = std::fs::read_to_string(&template_path) {
        let content = template.trim();
        if content.lines().count() > 1 {
            sections.push(content.to_string());
        }
    }

    // Baseline spec.
    sections.push(format!(
        "# QA Baseline Spec\n\n\
         Execute each test scenario below. For each one, report a result line.\n\n\
         {}\n",
        baseline.raw
    ));

    // Task-specific acceptance tests.
    if let Some(spec) = task_spec {
        sections.push(format!(
            "# Task-Specific Acceptance Tests\n\n\
             In addition to the baseline tests above, verify these task-specific scenarios:\n\n\
             {spec}\n"
        ));
    }

    // Previous result for regression comparison.
    if let Some(prev) = previous_result {
        let mut regression_section = String::from(
            "# Previous QA Results (Regression Baseline)\n\n\
             These tests passed in the previous run. They MUST still pass:\n\n",
        );
        for test in &prev.tests {
            let status = if test.passed { "PASS" } else { "FAIL" };
            regression_section.push_str(&format!(
                "- {}.{}: {} {}\n",
                test.suite, test.name, status, test.detail
            ));
        }
        sections.push(regression_section);
    }

    // Repo root for context.
    sections.push(format!(
        "# Environment\n\n\
         Repository root: `{}`\n",
        repo_root.display()
    ));

    sections.join("\n---\n\n")
}

/// Build a QA failure context string to inject into implementation retry prompts.
pub fn build_qa_failure_context(result: &QAResult) -> String {
    let mut ctx = String::from("## QA Failures (from previous attempt)\n\n");

    for test in &result.tests {
        let status = if test.passed { "PASS" } else { "FAIL" };
        let detail = if test.detail.is_empty() {
            String::new()
        } else {
            format!(" — {}", test.detail)
        };
        ctx.push_str(&format!(
            "- {}.{}: {}{}\n",
            test.suite, test.name, status, detail
        ));
    }

    ctx.push_str("\nFix the failing tests before signaling [patch_ready].\n");
    ctx
}

// ---------------------------------------------------------------------------
// Agent management
// ---------------------------------------------------------------------------

/// Spawn a QA agent process.
pub fn spawn_qa_agent(
    cwd: &Path,
    prompt: &str,
    model: ModelKind,
    state: &mut QAState,
) -> anyhow::Result<()> {
    let adapter = default_adapter_for(model)?;

    let request = EpochRequest {
        task_id: TaskId::new("qa-agent"),
        repo_id: RepoId("default".to_string()),
        model,
        repo_path: cwd.to_path_buf(),
        prompt: prompt.to_string(),
        timeout_secs: 900, // QA runs can be longer
        extra_args: vec![],
        env: vec![],
    };

    let cmd = adapter.build_command(&request);

    let mut child = Command::new(&cmd.executable)
        .args(&cmd.args)
        .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .env_remove("CLAUDECODE")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let (tx, rx) = mpsc::channel();

    // Pipe stdout.
    if let Some(stdout) = child.stdout.take() {
        let tx_out = tx.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx_out.send(line);
            }
        });
    }

    // Pipe stderr.
    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }

    state.child_handle = Some(child);
    state.result_rx = Some(rx);

    Ok(())
}

/// Drain pending output lines from a running QA agent without checking for
/// completion.  Returns the lines for display in a TUI pane.  Also
/// accumulates lines into `state.output_buffer` so that `poll_qa_agent`
/// can parse the full output on completion.
pub fn drain_qa_output(state: &mut QAState) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(rx) = &state.result_rx {
        while let Ok(line) = rx.try_recv() {
            if let Some(signal) = detect_common_signal(&line) {
                if signal.kind == AgentSignalKind::QAComplete {
                    state.qa_complete = true;
                    if state.signal_at.is_none() {
                        state.signal_at = Some(Instant::now());
                    }
                }
            }
            lines.push(line);
        }
    }
    // Keep a copy in the buffer for parse_qa_output later.
    state.output_buffer.extend(lines.iter().cloned());
    lines
}

/// Non-blocking poll of a running QA agent.
///
/// Returns `Some(QAResult)` when the agent has completed.
///
/// **Important**: call `drain_qa_output` before this so that channel lines
/// are accumulated in `state.output_buffer`.  This function drains any
/// remaining lines (that arrived between the last drain and the child
/// exiting) and uses the full buffer for parsing.
pub fn poll_qa_agent(state: &mut QAState) -> Option<QAResult> {
    if state.status != QAStatus::RunningBaseline && state.status != QAStatus::RunningValidation {
        return None;
    }

    // Drain any lines that arrived since the last drain_qa_output call.
    if let Some(rx) = &state.result_rx {
        while let Ok(line) = rx.try_recv() {
            if let Some(signal) = detect_common_signal(&line) {
                if signal.kind == AgentSignalKind::QAComplete {
                    state.qa_complete = true;
                    if state.signal_at.is_none() {
                        state.signal_at = Some(Instant::now());
                    }
                }
            }
            state.output_buffer.push(line);
        }
    }

    // Kill process if it signaled completion but hasn't exited.
    if let Some(t) = state.signal_at {
        if t.elapsed() > Duration::from_secs(5) {
            if let Some(child) = state.child_handle.as_mut() {
                let _ = child.kill();
            }
        }
    }

    // Check if the child has exited.
    let exited = if let Some(child) = state.child_handle.as_mut() {
        match child.try_wait() {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(_) => true,
        }
    } else {
        state.status = QAStatus::Failed;
        return None;
    };

    if !exited {
        return None;
    }

    // Child exited — drain any final lines from the channel.
    if let Some(rx) = &state.result_rx {
        while let Ok(line) = rx.try_recv() {
            if let Some(signal) = detect_common_signal(&line) {
                if signal.kind == AgentSignalKind::QAComplete {
                    state.qa_complete = true;
                    if state.signal_at.is_none() {
                        state.signal_at = Some(Instant::now());
                    }
                }
            }
            state.output_buffer.push(line);
        }
    }

    // Parse from the full accumulated buffer.
    let raw_output = state.output_buffer.join("\n");
    let result = parse_qa_output(&raw_output);

    if result.tests.is_empty() && !state.qa_complete {
        // Agent produced no structured results — mark as failed.
        state.status = QAStatus::Failed;
        state.child_handle = None;
        state.result_rx = None;
        return None;
    }

    state.status = QAStatus::Completed;
    state.child_handle = None;
    state.result_rx = None;

    Some(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_qa_spec_extracts_suites_and_tests() {
        let content = "\
# QA Baseline

## System Startup
- spin up `othala daemon`, verify purple banner prints to stderr
- verify context status line appears
- kill daemon cleanly with SIGINT

## TUI
- run `orch-tui`, verify it starts without error
- create a chat via TUI, verify task appears in sqlite
";
        let spec = parse_qa_spec(content);
        assert_eq!(spec.tests.len(), 5);
        assert_eq!(spec.tests[0].suite, "system_startup");
        assert_eq!(spec.tests[3].suite, "tui");
        assert!(spec.tests[0].steps.contains("othala daemon"));
    }

    #[test]
    fn parse_qa_spec_empty() {
        let spec = parse_qa_spec("# No tests here\n\nJust prose.\n");
        assert!(spec.tests.is_empty());
    }

    #[test]
    fn parse_qa_output_extracts_results() {
        let raw = "\
Some agent output...
<!-- QA_META: task/T-123 | abc1234 -->
Running tests...
<!-- QA_RESULT: startup.daemon_banner | PASS | banner printed correctly -->
<!-- QA_RESULT: startup.context_status | PASS | status line visible -->
<!-- QA_RESULT: tui.create_chat | FAIL | branch not created within 5s -->
[qa_complete]
";
        let result = parse_qa_output(raw);
        assert_eq!(result.branch, "task/T-123");
        assert_eq!(result.commit, "abc1234");
        assert_eq!(result.tests.len(), 3);
        assert!(result.tests[0].passed);
        assert!(result.tests[1].passed);
        assert!(!result.tests[2].passed);
        assert_eq!(result.tests[2].detail, "branch not created within 5s");
        assert_eq!(result.summary.total, 3);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 1);
    }

    #[test]
    fn parse_qa_output_empty() {
        let result = parse_qa_output("no structured output here");
        assert!(result.tests.is_empty());
        assert_eq!(result.summary.total, 0);
    }

    #[test]
    fn qa_result_roundtrip_json() {
        let result = QAResult {
            branch: "task/T-42".to_string(),
            commit: "abc1234".to_string(),
            timestamp: Utc::now(),
            tests: vec![
                QATestResult {
                    name: "daemon_banner".to_string(),
                    suite: "startup".to_string(),
                    passed: true,
                    detail: "ok".to_string(),
                    duration_ms: 1200,
                },
                QATestResult {
                    name: "tui_create_chat".to_string(),
                    suite: "tui".to_string(),
                    passed: false,
                    detail: "timeout".to_string(),
                    duration_ms: 5000,
                },
            ],
            summary: QASummary {
                total: 2,
                passed: 1,
                failed: 1,
            },
        };

        let json = serde_json::to_string_pretty(&result).unwrap();
        let decoded: QAResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.branch, result.branch);
        assert_eq!(decoded.tests.len(), 2);
        assert_eq!(decoded.summary.failed, 1);
    }

    #[test]
    fn save_and_load_qa_result() {
        let tmp = std::env::temp_dir().join(format!("othala-qa-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        let result = QAResult {
            branch: "task/T-99".to_string(),
            commit: "def5678abc".to_string(),
            timestamp: Utc::now(),
            tests: vec![QATestResult {
                name: "test_one".to_string(),
                suite: "general".to_string(),
                passed: true,
                detail: String::new(),
                duration_ms: 500,
            }],
            summary: QASummary {
                total: 1,
                passed: 1,
                failed: 0,
            },
        };

        let path = save_qa_result(&tmp, &result).unwrap();
        assert!(path.exists());

        // Should be loadable.
        let loaded = load_latest_result(&tmp, "task/T-99").unwrap();
        assert_eq!(loaded.branch, "task/T-99");
        assert_eq!(loaded.tests.len(), 1);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn load_baseline_returns_none_when_missing() {
        let tmp = std::env::temp_dir().join(format!("othala-qa-nobase-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        assert!(load_baseline(&tmp).is_none());

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn load_baseline_parses_spec() {
        let tmp = std::env::temp_dir().join(format!("othala-qa-base-{}", std::process::id()));
        let qa = tmp.join(".othala/qa");
        fs::create_dir_all(&qa).unwrap();
        fs::write(
            qa.join("baseline.md"),
            "# QA Baseline\n\n## CLI\n- run `othala list`\n- check output\n",
        )
        .unwrap();

        let spec = load_baseline(&tmp).unwrap();
        assert_eq!(spec.tests.len(), 2);
        assert_eq!(spec.tests[0].suite, "cli");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn build_qa_prompt_includes_all_sections() {
        let baseline = QASpec {
            raw: "## Startup\n- check daemon\n".to_string(),
            tests: vec![QATestCase {
                name: "check_daemon".to_string(),
                suite: "startup".to_string(),
                steps: "check daemon".to_string(),
            }],
        };

        let prompt = build_qa_prompt(
            &baseline,
            Some("- verify new endpoint returns 200"),
            None,
            Path::new("/repo"),
            Path::new("/nonexistent"),
        );

        assert!(prompt.contains("QA Baseline Spec"));
        assert!(prompt.contains("check daemon"));
        assert!(prompt.contains("Task-Specific Acceptance Tests"));
        assert!(prompt.contains("verify new endpoint returns 200"));
        assert!(prompt.contains("/repo"));
    }

    #[test]
    fn build_qa_prompt_includes_previous_results() {
        let baseline = QASpec {
            raw: "## Test\n- a test\n".to_string(),
            tests: vec![],
        };

        let prev = QAResult {
            branch: "main".to_string(),
            commit: "abc".to_string(),
            timestamp: Utc::now(),
            tests: vec![QATestResult {
                name: "startup_check".to_string(),
                suite: "startup".to_string(),
                passed: true,
                detail: "ok".to_string(),
                duration_ms: 100,
            }],
            summary: QASummary {
                total: 1,
                passed: 1,
                failed: 0,
            },
        };

        let prompt = build_qa_prompt(
            &baseline,
            None,
            Some(&prev),
            Path::new("/repo"),
            Path::new("/nonexistent"),
        );

        assert!(prompt.contains("Previous QA Results"));
        assert!(prompt.contains("startup.startup_check: PASS"));
    }

    #[test]
    fn build_qa_failure_context_formats_correctly() {
        let result = QAResult {
            branch: "task/T-1".to_string(),
            commit: "abc".to_string(),
            timestamp: Utc::now(),
            tests: vec![
                QATestResult {
                    name: "banner".to_string(),
                    suite: "startup".to_string(),
                    passed: true,
                    detail: String::new(),
                    duration_ms: 100,
                },
                QATestResult {
                    name: "create_chat".to_string(),
                    suite: "tui".to_string(),
                    passed: false,
                    detail: "branch not created".to_string(),
                    duration_ms: 5000,
                },
            ],
            summary: QASummary {
                total: 2,
                passed: 1,
                failed: 1,
            },
        };

        let ctx = build_qa_failure_context(&result);
        assert!(ctx.contains("startup.banner: PASS"));
        assert!(ctx.contains("tui.create_chat: FAIL — branch not created"));
        assert!(ctx.contains("[patch_ready]"));
    }

    #[test]
    fn sanitize_branch_name_handles_slashes() {
        assert_eq!(sanitize_branch_name("task/T-123"), "task-T-123");
        assert_eq!(sanitize_branch_name("main"), "main");
        assert_eq!(sanitize_branch_name("feat/my-thing"), "feat-my-thing");
    }

    #[test]
    fn qa_state_initial_status() {
        let state = QAState::new(QAType::Baseline);
        assert_eq!(state.status, QAStatus::RunningBaseline);
        assert!(!state.qa_complete);

        let state = QAState::new(QAType::Validation);
        assert_eq!(state.status, QAStatus::RunningValidation);
    }

    #[test]
    fn qa_type_display() {
        assert_eq!(format!("{}", QAType::Baseline), "baseline");
        assert_eq!(format!("{}", QAType::Validation), "validation");
    }

    #[test]
    fn drain_qa_output_accumulates_lines_and_detects_qa_complete() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut state = QAState::new(QAType::Baseline);
        state.result_rx = Some(rx);

        tx.send("line 1".to_string()).unwrap();
        tx.send("running tests...".to_string()).unwrap();
        tx.send("[qa_complete]".to_string()).unwrap();

        let lines = drain_qa_output(&mut state);

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "line 1");
        assert_eq!(lines[1], "running tests...");
        assert_eq!(lines[2], "[qa_complete]");

        // Lines should also be accumulated in output_buffer.
        assert_eq!(state.output_buffer.len(), 3);

        // qa_complete signal should be detected.
        assert!(state.qa_complete);
    }

    #[test]
    fn drain_qa_output_returns_empty_when_no_lines() {
        let (_tx, rx) = std::sync::mpsc::channel::<String>();
        let mut state = QAState::new(QAType::Validation);
        state.result_rx = Some(rx);

        let lines = drain_qa_output(&mut state);
        assert!(lines.is_empty());
        assert!(state.output_buffer.is_empty());
        assert!(!state.qa_complete);
    }

    #[test]
    fn drain_qa_output_accumulates_across_multiple_calls() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut state = QAState::new(QAType::Baseline);
        state.result_rx = Some(rx);

        tx.send("first batch".to_string()).unwrap();
        let lines1 = drain_qa_output(&mut state);
        assert_eq!(lines1.len(), 1);
        assert_eq!(state.output_buffer.len(), 1);

        tx.send("second batch".to_string()).unwrap();
        let lines2 = drain_qa_output(&mut state);
        assert_eq!(lines2.len(), 1);
        assert_eq!(state.output_buffer.len(), 2);
        assert_eq!(state.output_buffer[0], "first batch");
        assert_eq!(state.output_buffer[1], "second batch");
    }

    #[test]
    fn drain_qa_output_handles_no_receiver() {
        let mut state = QAState::new(QAType::Baseline);
        // result_rx is None
        let lines = drain_qa_output(&mut state);
        assert!(lines.is_empty());
    }

    #[test]
    fn poll_qa_agent_returns_none_when_idle() {
        let mut state = QAState::new(QAType::Baseline);
        state.status = QAStatus::Idle;
        assert!(poll_qa_agent(&mut state).is_none());
    }

    #[test]
    fn poll_qa_agent_returns_none_when_no_child_handle() {
        let mut state = QAState::new(QAType::Baseline);
        // status is RunningBaseline but no child_handle
        assert!(poll_qa_agent(&mut state).is_none());
        assert_eq!(state.status, QAStatus::Failed);
    }

    #[test]
    fn parse_qa_output_handles_multiple_suites() {
        let raw = "\
<!-- QA_META: main | abc1234 -->
<!-- QA_RESULT: build.cargo_check | PASS | exit code 0 -->
<!-- QA_RESULT: build.cargo_test | PASS | 42 tests passed -->
<!-- QA_RESULT: tui.startup | PASS | tui started -->
<!-- QA_RESULT: tui.create_chat | FAIL | branch not created -->
<!-- QA_RESULT: database.integrity | PASS | ok -->
";
        let result = parse_qa_output(raw);
        assert_eq!(result.branch, "main");
        assert_eq!(result.commit, "abc1234");
        assert_eq!(result.tests.len(), 5);
        assert_eq!(result.summary.total, 5);
        assert_eq!(result.summary.passed, 4);
        assert_eq!(result.summary.failed, 1);

        // Verify suite/name extraction.
        assert_eq!(result.tests[0].suite, "build");
        assert_eq!(result.tests[0].name, "cargo_check");
        assert_eq!(result.tests[3].suite, "tui");
        assert_eq!(result.tests[3].name, "create_chat");
        assert!(!result.tests[3].passed);
    }

    #[test]
    fn parse_qa_output_handles_no_suite_prefix() {
        let raw = "<!-- QA_RESULT: standalone_test | PASS | ok -->";
        let result = parse_qa_output(raw);
        assert_eq!(result.tests.len(), 1);
        assert_eq!(result.tests[0].suite, "general");
        assert_eq!(result.tests[0].name, "standalone_test");
    }

    #[test]
    fn parse_qa_output_handles_no_detail() {
        let raw = "<!-- QA_RESULT: build.check | PASS -->";
        let result = parse_qa_output(raw);
        assert_eq!(result.tests.len(), 1);
        assert!(result.tests[0].passed);
        assert!(result.tests[0].detail.is_empty());
    }

    #[test]
    fn qa_spec_parse_long_test_name_truncated() {
        let long_name = "a".repeat(100);
        let content = format!("## Suite\n- {long_name}\n");
        let spec = parse_qa_spec(&content);
        assert_eq!(spec.tests.len(), 1);
        assert!(spec.tests[0].name.len() <= 60);
    }

    #[test]
    fn build_qa_prompt_with_template() {
        let tmp = std::env::temp_dir().join(format!("othala-qa-tmpl-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join("qa-validator.md"),
            "# QA Validator Instructions\n\nRun all tests.\n",
        )
        .unwrap();

        let baseline = QASpec {
            raw: "## Build\n- check cargo\n".to_string(),
            tests: vec![],
        };

        let prompt = build_qa_prompt(&baseline, None, None, std::path::Path::new("/repo"), &tmp);
        assert!(prompt.contains("QA Validator Instructions"));
        assert!(prompt.contains("QA Baseline Spec"));
        assert!(prompt.contains("check cargo"));

        fs::remove_dir_all(&tmp).ok();
    }
}
