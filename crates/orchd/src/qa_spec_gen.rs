//! QA spec generation — auto-generate `.othala/qa/baseline.md` on startup
//! and after significant changes.
//!
//! Spawns an AI agent to analyse the repository and produce comprehensive,
//! executable QA test specifications.  The agent explores binaries, TUI
//! keybindings, database schemas, and state machines to generate tests that
//! use tmux, sqlite3, and other tools to actually exercise the system.
//!
//! Output is parsed from `<!-- QA_SPEC_FILE: name.md -->` delimited blocks,
//! validated, and written to `.othala/qa/`.

use chrono::{DateTime, Utc};
use orch_agents::{default_adapter_for, EpochRequest};
use orch_core::types::{ModelKind, RepoId, TaskId};

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Status of the background QA spec generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QASpecGenStatus {
    Idle,
    Running,
    Completed,
    Failed,
}

/// A single QA spec file to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QASpecFile {
    pub filename: String,
    pub content: String,
}

/// Parsed agent output — the set of spec files to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QASpecGenOutput {
    pub files: Vec<QASpecFile>,
}

/// Configuration for QA spec regeneration.
#[derive(Debug, Clone)]
pub struct QASpecGenConfig {
    /// Minimum seconds between regenerations.
    pub cooldown_secs: u64,
    /// Model to use for generation.
    pub model: ModelKind,
}

impl Default for QASpecGenConfig {
    fn default() -> Self {
        Self {
            cooldown_secs: 600,
            model: ModelKind::Claude,
        }
    }
}

/// Mutable state for the background QA spec generation process.
pub struct QASpecGenState {
    pub status: QASpecGenStatus,
    pub last_generated_at: Option<DateTime<Utc>>,
    pub result_rx: Option<mpsc::Receiver<String>>,
    pub child_handle: Option<Child>,
}

impl QASpecGenState {
    pub fn new() -> Self {
        Self {
            status: QASpecGenStatus::Idle,
            last_generated_at: None,
            result_rx: None,
            child_handle: None,
        }
    }
}

impl Default for QASpecGenState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Read the stored git hash from `.othala/qa/.git-hash`.
pub fn read_stored_hash(repo_root: &Path) -> Option<String> {
    let path = repo_root.join(".othala/qa/.git-hash");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Write the current git hash to `.othala/qa/.git-hash`.
pub fn write_stored_hash(repo_root: &Path, hash: &str) -> std::io::Result<()> {
    let dir = repo_root.join(".othala/qa");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(".git-hash"), hash)
}

/// Check whether QA specs are current: baseline.md exists AND stored hash
/// matches HEAD.
pub fn qa_spec_is_current(repo_root: &Path) -> bool {
    if !repo_root.join(".othala/qa/baseline.md").exists() {
        return false;
    }
    match (
        crate::context_gen::get_head_sha(repo_root),
        read_stored_hash(repo_root),
    ) {
        (Some(head), Some(stored)) => head == stored,
        (None, _) => true,
        (Some(_), None) => false,
    }
}

/// Decide whether we should trigger a regeneration based on cooldown.
pub fn should_regenerate(
    state: &QASpecGenState,
    config: &QASpecGenConfig,
    now: DateTime<Utc>,
) -> bool {
    if state.status == QASpecGenStatus::Running {
        return false;
    }
    match state.last_generated_at {
        Some(last) => {
            let elapsed = now.signed_duration_since(last).num_seconds();
            elapsed >= config.cooldown_secs as i64
        }
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Startup status
// ---------------------------------------------------------------------------

/// Status of QA spec at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QASpecStartupStatus {
    /// QA spec is current, nothing to do.
    UpToDate,
    /// QA spec exists but is stale — will regen in background.
    Stale,
    /// No QA spec at all — need to generate.
    Missing,
}

/// Check what needs to happen at startup.
pub fn check_qa_spec_startup(repo_root: &Path) -> QASpecStartupStatus {
    if qa_spec_is_current(repo_root) {
        return QASpecStartupStatus::UpToDate;
    }
    if repo_root.join(".othala/qa/baseline.md").exists() {
        QASpecStartupStatus::Stale
    } else {
        QASpecStartupStatus::Missing
    }
}

// ---------------------------------------------------------------------------
// Repo scanning
// ---------------------------------------------------------------------------

/// Scan the repository for test-relevant infrastructure and build a snapshot
/// string that helps the QA spec generator understand what to test.
pub fn scan_test_infrastructure(repo_root: &Path) -> String {
    let mut snapshot = String::from("# Test Infrastructure Snapshot\n\n");

    // Cargo.toml — workspace structure
    let cargo_toml = repo_root.join("Cargo.toml");
    if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
        snapshot.push_str("## Cargo.toml (workspace root)\n```toml\n");
        snapshot.push_str(&content);
        snapshot.push_str("\n```\n\n");
    }

    // Binary entrypoints — find all main.rs files
    snapshot.push_str("## Binary Entrypoints\n\n");
    let crates_dir = repo_root.join("crates");
    if crates_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&crates_dir) {
            let mut crate_names: Vec<String> = entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            crate_names.sort();

            for crate_name in &crate_names {
                let crate_dir = crates_dir.join(crate_name);
                // Check for main.rs or bin/ directory
                let main_rs = crate_dir.join("src/main.rs");
                if main_rs.exists() {
                    if let Ok(content) = std::fs::read_to_string(&main_rs) {
                        let first_lines: String =
                            content.lines().take(50).collect::<Vec<_>>().join("\n");
                        snapshot.push_str(&format!(
                            "### crates/{crate_name}/src/main.rs (first 50 lines)\n```rust\n{first_lines}\n```\n\n"
                        ));
                    }
                }
                // Check for bin/ directory
                let bin_dir = crate_dir.join("src/bin");
                if bin_dir.is_dir() {
                    if let Ok(bins) = std::fs::read_dir(&bin_dir) {
                        for bin in bins.flatten() {
                            let bin_name = bin.file_name().to_string_lossy().to_string();
                            if let Ok(content) = std::fs::read_to_string(bin.path()) {
                                let first_lines: String =
                                    content.lines().take(30).collect::<Vec<_>>().join("\n");
                                snapshot.push_str(&format!(
                                    "### crates/{crate_name}/src/bin/{bin_name} (first 30 lines)\n```rust\n{first_lines}\n```\n\n"
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    // TUI keybindings — critical for TUI testing
    let action_rs = repo_root.join("crates/orch-tui/src/action.rs");
    if let Ok(content) = std::fs::read_to_string(&action_rs) {
        snapshot.push_str("## TUI Keybindings (action.rs)\n```rust\n");
        snapshot.push_str(&content);
        snapshot.push_str("\n```\n\n");
    }

    // Database schema — check for SQL or migration files
    snapshot.push_str("## Database Files\n\n");
    for db_path in &[".orch/state.sqlite", ".othala/db.sqlite"] {
        let full = repo_root.join(db_path);
        if full.exists() {
            snapshot.push_str(&format!("- `{db_path}` exists\n"));
        }
    }
    // Look for schema definitions in persistence code
    let persistence_files = [
        "crates/orchd/src/persistence.rs",
        "crates/orchd/src/service.rs",
    ];
    for rel_path in &persistence_files {
        let full = repo_root.join(rel_path);
        if let Ok(content) = std::fs::read_to_string(&full) {
            // Extract CREATE TABLE statements and SQL queries
            let sql_lines: Vec<&str> = content
                .lines()
                .filter(|l| {
                    let t = l.trim().to_uppercase();
                    t.contains("CREATE TABLE")
                        || t.contains("INSERT INTO")
                        || t.contains("SELECT ")
                        || t.contains("PRAGMA")
                })
                .take(40)
                .collect();
            if !sql_lines.is_empty() {
                snapshot.push_str(&format!(
                    "\n### SQL in {rel_path}\n```sql\n{}\n```\n\n",
                    sql_lines.join("\n")
                ));
            }
        }
    }

    // State machine — task states and transitions
    let state_machine = repo_root.join("crates/orchd/src/state_machine.rs");
    if let Ok(content) = std::fs::read_to_string(&state_machine) {
        snapshot.push_str("## State Machine (state_machine.rs)\n```rust\n");
        let truncated: String = content.lines().take(100).collect::<Vec<_>>().join("\n");
        snapshot.push_str(&truncated);
        snapshot.push_str("\n```\n\n");
    }

    // Task state enum
    let task_state_file = repo_root.join("crates/orch-core/src/state.rs");
    if let Ok(content) = std::fs::read_to_string(&task_state_file) {
        snapshot.push_str("## Task States (state.rs)\n```rust\n");
        snapshot.push_str(&content);
        snapshot.push_str("\n```\n\n");
    }

    // Existing test files — so the agent knows what's already covered
    snapshot.push_str("## Existing Test Coverage\n\n");
    if crates_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&crates_dir) {
            let mut crate_names: Vec<String> = entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            crate_names.sort();

            for crate_name in &crate_names {
                let tests_dir = crates_dir.join(crate_name).join("tests");
                if tests_dir.is_dir() {
                    if let Ok(tests) = std::fs::read_dir(&tests_dir) {
                        let test_files: Vec<String> = tests
                            .flatten()
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .collect();
                        if !test_files.is_empty() {
                            snapshot.push_str(&format!(
                                "- `crates/{crate_name}/tests/`: {}\n",
                                test_files.join(", ")
                            ));
                        }
                    }
                }
            }
        }
    }
    snapshot.push('\n');

    // Configuration directories
    snapshot.push_str("## Configuration\n\n");
    for dir in &[".othala", ".orch", "templates"] {
        let full = repo_root.join(dir);
        if full.is_dir() {
            snapshot.push_str(&format!("### {dir}/\n"));
            if let Ok(entries) = std::fs::read_dir(&full) {
                let mut items: Vec<String> = entries
                    .flatten()
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        if e.path().is_dir() {
                            format!("  {name}/")
                        } else {
                            format!("  {name}")
                        }
                    })
                    .collect();
                items.sort();
                for item in &items {
                    snapshot.push_str(item);
                    snapshot.push('\n');
                }
            }
            snapshot.push('\n');
        }
    }

    snapshot
}

// ---------------------------------------------------------------------------
// Prompt building
// ---------------------------------------------------------------------------

/// Build the full prompt for the QA spec generation agent.
pub fn build_qa_spec_gen_prompt(repo_root: &Path, template_dir: &Path) -> String {
    let mut prompt = String::new();

    // Load the qa-spec-generator template.
    let template_path = template_dir.join("qa-spec-generator.md");
    if let Ok(template) = std::fs::read_to_string(&template_path) {
        prompt.push_str(&template);
        prompt.push_str("\n\n---\n\n");
    }

    // Append test infrastructure snapshot.
    prompt.push_str(&scan_test_infrastructure(repo_root));

    prompt
}

// ---------------------------------------------------------------------------
// Output parsing
// ---------------------------------------------------------------------------

/// Parse the agent's raw output into structured QA spec files.
///
/// Expected format:
/// ```text
/// <!-- QA_SPEC_FILE: baseline.md -->
/// content here...
/// <!-- QA_SPEC_FILE: testing-strategy.md -->
/// more content...
/// ```
pub fn parse_qa_spec_gen_output(raw: &str) -> QASpecGenOutput {
    let mut files = Vec::new();
    let mut current_filename: Option<String> = None;
    let mut current_content = String::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("<!-- QA_SPEC_FILE:") {
            if let Some(name) = rest.strip_suffix("-->") {
                // Save previous file if any.
                if let Some(filename) = current_filename.take() {
                    let content = current_content.trim().to_string();
                    if !content.is_empty() {
                        files.push(QASpecFile { filename, content });
                    }
                }
                current_filename = Some(sanitize_path(name.trim()));
                current_content.clear();
                continue;
            }
        }
        if current_filename.is_some() {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }

    // Flush the last file.
    if let Some(filename) = current_filename {
        let content = current_content.trim().to_string();
        if !content.is_empty() {
            files.push(QASpecFile { filename, content });
        }
    }

    QASpecGenOutput { files }
}

/// Sanitize a path — allow `/` for subdirectories but reject `..`, `\`, and
/// leading `/`.
fn sanitize_path(name: &str) -> String {
    let name = name.replace('\\', "").replace("..", "");
    let name = name.strip_prefix('/').unwrap_or(&name);
    let name = name.replace("//", "/");
    let name = name.trim_matches('/');
    if name.is_empty() {
        "unnamed.md".to_string()
    } else {
        name.to_string()
    }
}

// ---------------------------------------------------------------------------
// File I/O
// ---------------------------------------------------------------------------

/// Write QA spec files to `.othala/qa/`, creating subdirectories as needed.
/// Also writes the current HEAD hash to `.git-hash`.
pub fn write_qa_spec_files(
    repo_root: &Path,
    output: &QASpecGenOutput,
) -> std::io::Result<Vec<PathBuf>> {
    let qa_dir = repo_root.join(".othala/qa");
    std::fs::create_dir_all(&qa_dir)?;

    let mut written = Vec::new();
    for file in &output.files {
        let path = qa_dir.join(&file.filename);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &file.content)?;
        written.push(path);
    }

    // Write current HEAD hash for staleness detection.
    if let Some(hash) = crate::context_gen::get_head_sha(repo_root) {
        write_stored_hash(repo_root, &hash)?;
    }

    Ok(written)
}

// ---------------------------------------------------------------------------
// Process management
// ---------------------------------------------------------------------------

/// Spawn a background agent process for QA spec generation.
pub fn spawn_qa_spec_gen(
    repo_root: &Path,
    prompt: &str,
    model: ModelKind,
    state: &mut QASpecGenState,
) -> anyhow::Result<()> {
    let adapter = default_adapter_for(model)?;

    let request = EpochRequest {
        task_id: TaskId::new("qa-spec-gen"),
        repo_id: RepoId("default".to_string()),
        model,
        repo_path: repo_root.to_path_buf(),
        prompt: prompt.to_string(),
        timeout_secs: 600,
        extra_args: vec![],
        env: vec![],
    };

    let cmd = adapter.build_command(&request);

    let mut child = Command::new(&cmd.executable)
        .args(&cmd.args)
        .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .env_remove("CLAUDECODE")
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let (tx, rx) = mpsc::channel();

    if let Some(stdout) = child.stdout.take() {
        let tx_out = tx.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx_out.send(line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }

    state.status = QASpecGenStatus::Running;
    state.result_rx = Some(rx);
    state.child_handle = Some(child);

    Ok(())
}

/// Non-blocking poll of a running QA spec generation process.
///
/// If the process has finished, parses the output and writes files.
/// Returns the list of written paths on success.
pub fn poll_qa_spec_gen(repo_root: &Path, state: &mut QASpecGenState) -> Option<Vec<PathBuf>> {
    if state.status != QASpecGenStatus::Running {
        return None;
    }

    let exited = if let Some(child) = state.child_handle.as_mut() {
        match child.try_wait() {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(_) => true,
        }
    } else {
        state.status = QASpecGenStatus::Failed;
        return None;
    };

    if !exited {
        return None;
    }

    // Child exited — drain all output.
    let mut output_lines = Vec::new();
    if let Some(rx) = &state.result_rx {
        while let Ok(line) = rx.try_recv() {
            output_lines.push(line);
        }
    }

    let raw_output = output_lines.join("\n");
    let parsed = parse_qa_spec_gen_output(&raw_output);

    if !parsed.files.is_empty() {
        match write_qa_spec_files(repo_root, &parsed) {
            Ok(paths) => {
                eprintln!("[qa-spec-gen] Wrote {} QA spec files", paths.len());
                state.status = QASpecGenStatus::Completed;
                state.last_generated_at = Some(Utc::now());
                state.child_handle = None;
                state.result_rx = None;
                return Some(paths);
            }
            Err(e) => {
                eprintln!("[qa-spec-gen] Failed to write QA spec files: {e}");
                state.status = QASpecGenStatus::Failed;
                state.child_handle = None;
                state.result_rx = None;
                return None;
            }
        }
    }

    // Agent may have written files directly using its Write tool.
    if repo_root.join(".othala/qa/baseline.md").exists() {
        if let Some(hash) = crate::context_gen::get_head_sha(repo_root) {
            let _ = write_stored_hash(repo_root, &hash);
        }
        eprintln!("[qa-spec-gen] Agent wrote QA spec files directly");
        state.status = QASpecGenStatus::Completed;
        state.last_generated_at = Some(Utc::now());
        state.child_handle = None;
        state.result_rx = None;
        return Some(Vec::new());
    }

    eprintln!("[qa-spec-gen] Agent produced no QA spec files");
    state.status = QASpecGenStatus::Failed;
    state.child_handle = None;
    state.result_rx = None;
    None
}

/// Blocking startup variant — waits for QA spec generation to complete.
///
/// The `progress` callback receives stderr lines from the agent process so the
/// caller can display activity (spinner, status line, etc.).
pub fn ensure_qa_spec_exists_blocking(
    repo_root: &Path,
    template_dir: &Path,
    model: ModelKind,
    progress: impl Fn(&str) + Send + 'static,
) -> anyhow::Result<()> {
    match check_qa_spec_startup(repo_root) {
        QASpecStartupStatus::UpToDate => return Ok(()),
        QASpecStartupStatus::Stale => return Ok(()),
        QASpecStartupStatus::Missing => {}
    }

    let prompt = build_qa_spec_gen_prompt(repo_root, template_dir);
    let adapter = default_adapter_for(model)?;

    let request = EpochRequest {
        task_id: TaskId::new("qa-spec-gen-startup"),
        repo_id: RepoId("default".to_string()),
        model,
        repo_path: repo_root.to_path_buf(),
        prompt,
        timeout_secs: 600,
        extra_args: vec![],
        env: vec![],
    };

    let cmd = adapter.build_command(&request);

    let mut child = Command::new(&cmd.executable)
        .args(&cmd.args)
        .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .env_remove("CLAUDECODE")
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        let tx_out = tx.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx_out.send(line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                progress(&line);
            }
        });
    }

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => thread::sleep(Duration::from_millis(200)),
            Err(e) => anyhow::bail!("error waiting for QA spec gen process: {e}"),
        }
    }

    let mut output_lines = Vec::new();
    while let Ok(line) = rx.try_recv() {
        output_lines.push(line);
    }

    let raw = output_lines.join("\n");
    let parsed = parse_qa_spec_gen_output(&raw);

    if !parsed.files.is_empty() {
        let paths = write_qa_spec_files(repo_root, &parsed)?;
        eprintln!(
            "[qa-spec-gen] Generated {} QA spec files at startup",
            paths.len()
        );
    } else if repo_root.join(".othala/qa/baseline.md").exists() {
        if let Some(hash) = crate::context_gen::get_head_sha(repo_root) {
            write_stored_hash(repo_root, &hash)?;
        }
        eprintln!("[qa-spec-gen] Agent wrote QA spec files directly");
    } else {
        anyhow::bail!("QA spec generation agent produced no spec files");
    }

    Ok(())
}

/// Re-export progress line parsing from context_gen (same agent output format).
pub use crate::context_gen::parse_progress_line;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn qa_spec_is_current_requires_baseline_and_hash() {
        let tmp =
            std::env::temp_dir().join(format!("othala-qaspec-current-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        // No baseline.md → not current.
        assert!(!qa_spec_is_current(&tmp));

        let qa_dir = tmp.join(".othala/qa");
        fs::create_dir_all(&qa_dir).unwrap();
        fs::write(qa_dir.join("baseline.md"), "# QA Baseline\n").unwrap();

        // baseline.md exists, no git hash file. get_head_sha returns None
        // for non-git dir → considered current (graceful fallback).
        assert!(qa_spec_is_current(&tmp));

        // With a hash stored, still current (no git HEAD to compare against).
        fs::write(qa_dir.join(".git-hash"), "abc123").unwrap();
        assert!(qa_spec_is_current(&tmp));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn check_qa_spec_startup_returns_missing() {
        let tmp =
            std::env::temp_dir().join(format!("othala-qaspec-startup-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        assert_eq!(check_qa_spec_startup(&tmp), QASpecStartupStatus::Missing);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn check_qa_spec_startup_returns_up_to_date() {
        let tmp =
            std::env::temp_dir().join(format!("othala-qaspec-uptodate-{}", std::process::id()));
        let qa_dir = tmp.join(".othala/qa");
        fs::create_dir_all(&qa_dir).unwrap();
        fs::write(qa_dir.join("baseline.md"), "# QA Baseline\n").unwrap();

        // No git repo → considered up to date.
        assert_eq!(check_qa_spec_startup(&tmp), QASpecStartupStatus::UpToDate);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn should_regenerate_respects_cooldown() {
        let config = QASpecGenConfig {
            cooldown_secs: 300,
            model: ModelKind::Claude,
        };

        let state = QASpecGenState::new();
        assert!(should_regenerate(&state, &config, Utc::now()));

        let mut state = QASpecGenState::new();
        state.last_generated_at = Some(Utc::now());
        assert!(!should_regenerate(&state, &config, Utc::now()));

        let mut state = QASpecGenState::new();
        state.last_generated_at = Some(Utc::now() - chrono::Duration::seconds(600));
        assert!(should_regenerate(&state, &config, Utc::now()));
    }

    #[test]
    fn should_regenerate_blocks_while_running() {
        let config = QASpecGenConfig::default();
        let mut state = QASpecGenState::new();
        state.status = QASpecGenStatus::Running;
        assert!(!should_regenerate(&state, &config, Utc::now()));
    }

    #[test]
    fn parse_qa_spec_gen_output_basic() {
        let raw = "\
Some preamble text ignored.
<!-- QA_SPEC_FILE: baseline.md -->
# QA Baseline

## Build
- run cargo build

## TUI
- start orch-tui in tmux
<!-- QA_SPEC_FILE: testing-strategy.md -->
# Testing Strategy

Use tmux for TUI tests.
";
        let output = parse_qa_spec_gen_output(raw);
        assert_eq!(output.files.len(), 2);
        assert_eq!(output.files[0].filename, "baseline.md");
        assert!(output.files[0].content.contains("QA Baseline"));
        assert!(output.files[0].content.contains("cargo build"));
        assert_eq!(output.files[1].filename, "testing-strategy.md");
        assert!(output.files[1].content.contains("tmux"));
    }

    #[test]
    fn parse_qa_spec_gen_output_empty() {
        let output = parse_qa_spec_gen_output("no file markers here");
        assert!(output.files.is_empty());
    }

    #[test]
    fn parse_qa_spec_gen_output_strips_path_traversal() {
        let raw = "<!-- QA_SPEC_FILE: ../../../etc/passwd -->\nevil content\n";
        let output = parse_qa_spec_gen_output(raw);
        assert_eq!(output.files.len(), 1);
        assert!(!output.files[0].filename.contains(".."));
    }

    #[test]
    fn sanitize_path_allows_subdirectories() {
        assert_eq!(sanitize_path("baseline.md"), "baseline.md");
        assert_eq!(sanitize_path("specs/task-123.md"), "specs/task-123.md");
    }

    #[test]
    fn sanitize_path_strips_dangerous_sequences() {
        assert_eq!(sanitize_path("../evil.md"), "evil.md");
        assert_eq!(sanitize_path(".."), "unnamed.md");
        assert_eq!(sanitize_path("/etc/passwd"), "etc/passwd");
    }

    #[test]
    fn write_qa_spec_files_creates_files() {
        let tmp = std::env::temp_dir().join(format!("othala-qaspec-write-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        let output = QASpecGenOutput {
            files: vec![
                QASpecFile {
                    filename: "baseline.md".to_string(),
                    content: "# QA Baseline\n".to_string(),
                },
                QASpecFile {
                    filename: "testing-strategy.md".to_string(),
                    content: "# Strategy\n".to_string(),
                },
            ],
        };

        let paths = write_qa_spec_files(&tmp, &output).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(tmp.join(".othala/qa/baseline.md").exists());
        assert!(tmp.join(".othala/qa/testing-strategy.md").exists());

        let content = fs::read_to_string(tmp.join(".othala/qa/baseline.md")).unwrap();
        assert_eq!(content, "# QA Baseline\n");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn qa_spec_gen_state_default() {
        let state = QASpecGenState::default();
        assert_eq!(state.status, QASpecGenStatus::Idle);
        assert!(state.last_generated_at.is_none());
    }

    #[test]
    fn scan_test_infrastructure_includes_cargo_toml() {
        let tmp = std::env::temp_dir().join(format!("othala-qaspec-scan-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();

        let snapshot = scan_test_infrastructure(&tmp);
        assert!(snapshot.contains("Cargo.toml"));
        assert!(snapshot.contains("members"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn build_qa_spec_gen_prompt_includes_snapshot() {
        let tmp = std::env::temp_dir().join(format!("othala-qaspec-prompt-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("Cargo.toml"), "[workspace]\n").unwrap();

        let prompt = build_qa_spec_gen_prompt(&tmp, Path::new("/nonexistent-templates"));
        assert!(prompt.contains("Test Infrastructure Snapshot"));
        assert!(prompt.contains("[workspace]"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn hash_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("othala-qaspec-hash-{}", std::process::id()));
        let qa_dir = tmp.join(".othala/qa");
        fs::create_dir_all(&qa_dir).unwrap();

        assert!(read_stored_hash(&tmp).is_none());

        write_stored_hash(&tmp, "abc123def456").unwrap();
        assert_eq!(read_stored_hash(&tmp).unwrap(), "abc123def456");

        fs::remove_dir_all(&tmp).ok();
    }
}
