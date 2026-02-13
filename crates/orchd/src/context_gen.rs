//! Context generation — auto-generate `.othala/context/` files on startup
//! and after task completion.
//!
//! Spawns an AI agent to analyse the repository and produce MAIN.md plus
//! supporting context files.  Output is parsed from `<!-- FILE: name.md -->`
//! delimited blocks, validated, and written to disk.

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

/// Status of the background context generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextGenStatus {
    Idle,
    Running,
    Completed,
    Failed,
}

/// A single context file to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    pub filename: String,
    pub content: String,
}

/// Parsed agent output — the set of files to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextGenOutput {
    pub files: Vec<ContextFile>,
}

/// Configuration for context regeneration.
#[derive(Debug, Clone)]
pub struct ContextGenConfig {
    /// Minimum seconds between regenerations.
    pub cooldown_secs: u64,
    /// Model to use for generation.
    pub model: ModelKind,
}

impl Default for ContextGenConfig {
    fn default() -> Self {
        Self {
            cooldown_secs: 300,
            model: ModelKind::Claude,
        }
    }
}

/// Mutable state for the background context generation process.
pub struct ContextGenState {
    pub status: ContextGenStatus,
    pub last_generated_at: Option<DateTime<Utc>>,
    pub result_rx: Option<mpsc::Receiver<String>>,
    pub child_handle: Option<Child>,
}

impl ContextGenState {
    pub fn new() -> Self {
        Self {
            status: ContextGenStatus::Idle,
            last_generated_at: None,
            result_rx: None,
            child_handle: None,
        }
    }
}

impl Default for ContextGenState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Get the current HEAD commit hash.
pub fn get_head_sha(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Read the stored git hash from `.othala/context/.git-hash`.
pub fn read_stored_hash(repo_root: &Path) -> Option<String> {
    let path = repo_root.join(".othala/context/.git-hash");
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Write the current git hash to `.othala/context/.git-hash`.
pub fn write_stored_hash(repo_root: &Path, hash: &str) -> std::io::Result<()> {
    let path = repo_root.join(".othala/context/.git-hash");
    std::fs::write(path, hash)
}

/// Check whether context is current: MAIN.md exists AND stored hash matches HEAD.
pub fn context_is_current(repo_root: &Path) -> bool {
    if !repo_root.join(".othala/context/MAIN.md").exists() {
        return false;
    }
    match (get_head_sha(repo_root), read_stored_hash(repo_root)) {
        (Some(head), Some(stored)) => head == stored,
        // If we can't get the HEAD hash (not a git repo?), consider context current
        // as long as MAIN.md exists.
        (None, _) => true,
        // MAIN.md exists but no stored hash — stale.
        (Some(_), None) => false,
    }
}

/// Parse a raw agent stderr line into a human-friendly status message.
///
/// Returns `None` for lines that aren't interesting for display.
pub fn parse_progress_line(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Claude Code verbose output patterns.
    // Tool calls: "Read(file=...)", "Glob(pattern=...)", "Grep(pattern=...)"
    if let Some(rest) = trimmed.strip_prefix("Read(") {
        if let Some(file) = rest.strip_suffix(')') {
            let file = file
                .strip_prefix("file=")
                .unwrap_or(file)
                .trim_matches('"');
            return Some(format!("reading {file}"));
        }
    }
    if let Some(rest) = trimmed.strip_prefix("Glob(") {
        if let Some(pat) = rest.strip_suffix(')') {
            let pat = pat
                .strip_prefix("pattern=")
                .unwrap_or(pat)
                .trim_matches('"');
            return Some(format!("searching {pat}"));
        }
    }
    if let Some(rest) = trimmed.strip_prefix("Grep(") {
        if let Some(pat) = rest.strip_suffix(')') {
            let pat = pat
                .strip_prefix("pattern=")
                .unwrap_or(pat)
                .trim_matches('"');
            return Some(format!("searching for {pat}"));
        }
    }
    if let Some(rest) = trimmed.strip_prefix("Write(") {
        if let Some(file) = rest.strip_suffix(')') {
            let file = file
                .strip_prefix("file=")
                .unwrap_or(file)
                .trim_matches('"');
            return Some(format!("writing {file}"));
        }
    }
    if let Some(rest) = trimmed.strip_prefix("Bash(") {
        if let Some(cmd) = rest.strip_suffix(')') {
            let cmd = cmd
                .strip_prefix("command=")
                .unwrap_or(cmd)
                .trim_matches('"');
            let short = if cmd.len() > 60 { &cmd[..60] } else { cmd };
            return Some(format!("running {short}"));
        }
    }

    // Look for common tool-use patterns in JSON-ish output.
    if trimmed.contains("\"tool\":") || trimmed.contains("tool_use") {
        if trimmed.contains("Read") || trimmed.contains("read") {
            return Some("reading file...".to_string());
        }
        if trimmed.contains("Glob") || trimmed.contains("glob") {
            return Some("searching files...".to_string());
        }
        if trimmed.contains("Grep") || trimmed.contains("grep") {
            return Some("searching code...".to_string());
        }
        if trimmed.contains("Write") || trimmed.contains("write") {
            return Some("writing output...".to_string());
        }
        return Some("agent working...".to_string());
    }

    // "<!-- FILE: path -->" means it's outputting context files.
    if trimmed.starts_with("<!-- FILE:") {
        if let Some(rest) = trimmed.strip_prefix("<!-- FILE:") {
            if let Some(name) = rest.strip_suffix("-->") {
                return Some(format!("writing {}", name.trim()));
            }
        }
    }

    // Generic: if the line is short enough and looks like a status, show it.
    if trimmed.len() < 120 && !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return Some(trimmed.to_string());
    }

    // Fallback for anything else — just show "agent working".
    Some("agent working...".to_string())
}

/// Decide whether we should trigger a regeneration based on cooldown.
pub fn should_regenerate(state: &ContextGenState, config: &ContextGenConfig, now: DateTime<Utc>) -> bool {
    if state.status == ContextGenStatus::Running {
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

/// Scan the repository for key files and build a snapshot string.
pub fn scan_repo_snapshot(repo_root: &Path) -> String {
    let mut snapshot = String::from("# Repository Snapshot\n\n");

    // Cargo.toml (workspace root)
    let cargo_toml = repo_root.join("Cargo.toml");
    if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
        snapshot.push_str("## Cargo.toml (workspace root)\n```toml\n");
        snapshot.push_str(&content);
        snapshot.push_str("\n```\n\n");
    }

    // README.md
    let readme = repo_root.join("README.md");
    if let Ok(content) = std::fs::read_to_string(&readme) {
        snapshot.push_str("## README.md\n");
        snapshot.push_str(&content);
        snapshot.push_str("\n\n");
    }

    // Directory structure (top two levels)
    snapshot.push_str("## Directory Structure\n```\n");
    if let Ok(entries) = std::fs::read_dir(repo_root) {
        let mut dirs: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') && name != ".othala" {
                continue;
            }
            if entry.path().is_dir() {
                dirs.push(name.clone());
                // Second level
                if let Ok(sub_entries) = std::fs::read_dir(entry.path()) {
                    for sub in sub_entries.flatten() {
                        let sub_name = sub.file_name().to_string_lossy().to_string();
                        if !sub_name.starts_with('.') {
                            dirs.push(format!("  {name}/{sub_name}"));
                        }
                    }
                }
            } else {
                dirs.push(name);
            }
        }
        dirs.sort();
        for d in &dirs {
            snapshot.push_str(d);
            snapshot.push('\n');
        }
    }
    snapshot.push_str("```\n\n");

    // Crate lib.rs files
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
                let lib_rs = crates_dir.join(crate_name).join("src/lib.rs");
                if let Ok(content) = std::fs::read_to_string(&lib_rs) {
                    // Only include the first 80 lines to keep the snapshot manageable.
                    let truncated: String = content
                        .lines()
                        .take(80)
                        .collect::<Vec<_>>()
                        .join("\n");
                    snapshot.push_str(&format!("## crates/{crate_name}/src/lib.rs\n```rust\n"));
                    snapshot.push_str(&truncated);
                    snapshot.push_str("\n```\n\n");
                }
            }
        }
    }

    snapshot
}

/// Build the full prompt for the context generation agent.
pub fn build_context_gen_prompt(repo_root: &Path, template_dir: &Path) -> String {
    let mut prompt = String::new();

    // Load the context-generator template.
    let template_path = template_dir.join("context-generator.md");
    if let Ok(template) = std::fs::read_to_string(&template_path) {
        prompt.push_str(&template);
        prompt.push_str("\n\n---\n\n");
    }

    // Append repository snapshot.
    prompt.push_str(&scan_repo_snapshot(repo_root));

    prompt
}

/// Parse the agent's raw output into structured context files.
///
/// Expected format:
/// ```text
/// <!-- FILE: MAIN.md -->
/// content here...
/// <!-- FILE: architecture.md -->
/// more content...
/// ```
pub fn parse_context_gen_output(raw: &str) -> ContextGenOutput {
    let mut files = Vec::new();
    let mut current_filename: Option<String> = None;
    let mut current_content = String::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("<!-- FILE:") {
            if let Some(name) = rest.strip_suffix("-->") {
                // Save previous file if any.
                if let Some(filename) = current_filename.take() {
                    let content = current_content.trim().to_string();
                    if !content.is_empty() {
                        files.push(ContextFile {
                            filename,
                            content,
                        });
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
            files.push(ContextFile { filename, content });
        }
    }

    ContextGenOutput { files }
}

/// Sanitize a path — allow `/` for subdirectories but reject `..`, `\`, and
/// leading `/`.
fn sanitize_path(name: &str) -> String {
    // Strip backslashes and ".." sequences.
    let name = name.replace('\\', "").replace("..", "");
    // Reject absolute paths (leading /).
    let name = name.strip_prefix('/').unwrap_or(&name);
    // Collapse any double slashes left over from stripping.
    let name = name.replace("//", "/");
    let name = name.trim_matches('/');
    if name.is_empty() {
        "unnamed.md".to_string()
    } else {
        name.to_string()
    }
}

/// Write context files to `.othala/context/`, creating subdirectories as needed.
/// Also writes the current HEAD hash to `.git-hash`.
pub fn write_context_files(
    repo_root: &Path,
    output: &ContextGenOutput,
) -> std::io::Result<Vec<PathBuf>> {
    let context_dir = repo_root.join(".othala/context");
    std::fs::create_dir_all(&context_dir)?;

    let mut written = Vec::new();
    for file in &output.files {
        let path = context_dir.join(&file.filename);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &file.content)?;
        written.push(path);
    }

    // Write current HEAD hash for staleness detection.
    if let Some(hash) = get_head_sha(repo_root) {
        write_stored_hash(repo_root, &hash)?;
    }

    Ok(written)
}

// ---------------------------------------------------------------------------
// Process management
// ---------------------------------------------------------------------------

/// Spawn a background agent process for context generation.
///
/// Sets the state to `Running` and stores the child handle + output receiver.
pub fn spawn_context_gen(
    repo_root: &Path,
    prompt: &str,
    model: ModelKind,
    state: &mut ContextGenState,
) -> anyhow::Result<()> {
    let adapter = default_adapter_for(model)?;

    let request = EpochRequest {
        task_id: TaskId::new("context-gen"),
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

    // Pipe stdout into the channel.
    if let Some(stdout) = child.stdout.take() {
        let tx_out = tx.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx_out.send(line);
            }
        });
    }

    // Pipe stderr (discard into channel for draining).
    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }

    state.status = ContextGenStatus::Running;
    state.result_rx = Some(rx);
    state.child_handle = Some(child);

    Ok(())
}

/// Non-blocking poll of a running context generation process.
///
/// If the process has finished, parses the output and writes files.
/// Returns the list of written paths on success.
pub fn poll_context_gen(
    repo_root: &Path,
    state: &mut ContextGenState,
) -> Option<Vec<PathBuf>> {
    if state.status != ContextGenStatus::Running {
        return None;
    }

    // Check if the child has exited.
    let exited = if let Some(child) = state.child_handle.as_mut() {
        match child.try_wait() {
            Ok(Some(_status)) => true,
            Ok(None) => false,
            Err(_) => true,
        }
    } else {
        // No child handle — shouldn't happen, reset state.
        state.status = ContextGenStatus::Failed;
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
    let parsed = parse_context_gen_output(&raw_output);

    if !parsed.files.is_empty() {
        // Agent output via stdout delimiters — write them.
        match write_context_files(repo_root, &parsed) {
            Ok(paths) => {
                eprintln!(
                    "[context-gen] Wrote {} context files",
                    paths.len()
                );
                state.status = ContextGenStatus::Completed;
                state.last_generated_at = Some(Utc::now());
                state.child_handle = None;
                state.result_rx = None;
                return Some(paths);
            }
            Err(e) => {
                eprintln!("[context-gen] Failed to write context files: {e}");
                state.status = ContextGenStatus::Failed;
                state.child_handle = None;
                state.result_rx = None;
                return None;
            }
        }
    }

    // Agent may have written files directly using its Write tool.
    if repo_root.join(".othala/context/MAIN.md").exists() {
        if let Some(hash) = get_head_sha(repo_root) {
            let _ = write_stored_hash(repo_root, &hash);
        }
        let count = count_context_files(repo_root);
        eprintln!(
            "[context-gen] Agent wrote {} context files directly",
            count
        );
        state.status = ContextGenStatus::Completed;
        state.last_generated_at = Some(Utc::now());
        state.child_handle = None;
        state.result_rx = None;
        // Return empty vec since we didn't write them ourselves.
        return Some(Vec::new());
    }

    eprintln!("[context-gen] Agent produced no context files");
    state.status = ContextGenStatus::Failed;
    state.child_handle = None;
    state.result_rx = None;
    None
}

/// Status of the blocking context generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextStartupStatus {
    /// Context is current, nothing to do.
    UpToDate,
    /// Context exists but is stale — will regen in background.
    Stale,
    /// No context at all — need to generate.
    Missing,
}

/// Check what needs to happen at startup.
pub fn check_context_startup(repo_root: &Path) -> ContextStartupStatus {
    if context_is_current(repo_root) {
        return ContextStartupStatus::UpToDate;
    }
    if repo_root.join(".othala/context/MAIN.md").exists() {
        ContextStartupStatus::Stale
    } else {
        ContextStartupStatus::Missing
    }
}

/// Blocking startup variant — waits for context generation to complete.
///
/// The `progress` callback receives stderr lines from the agent process so the
/// caller can display activity (spinner, status line, etc.).
///
/// Only blocks if context is completely missing. Returns early for stale or
/// up-to-date context. Times out after 600s.
pub fn ensure_context_exists_blocking(
    repo_root: &Path,
    template_dir: &Path,
    model: ModelKind,
    progress: impl Fn(&str) + Send + 'static,
) -> anyhow::Result<()> {
    match check_context_startup(repo_root) {
        ContextStartupStatus::UpToDate => return Ok(()),
        ContextStartupStatus::Stale => return Ok(()),
        ContextStartupStatus::Missing => {}
    }

    let prompt = build_context_gen_prompt(repo_root, template_dir);
    let adapter = default_adapter_for(model)?;

    let request = EpochRequest {
        task_id: TaskId::new("context-gen-startup"),
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

    // Collect stdout in a background thread.
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

    // Pipe stderr to the progress callback.
    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                progress(&line);
            }
        });
    }

    // Wait for completion (no timeout — agent explores at its own pace).
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => thread::sleep(Duration::from_millis(200)),
            Err(e) => anyhow::bail!("error waiting for context gen process: {e}"),
        }
    }

    // Drain output.
    let mut output_lines = Vec::new();
    while let Ok(line) = rx.try_recv() {
        output_lines.push(line);
    }

    let raw = output_lines.join("\n");
    let parsed = parse_context_gen_output(&raw);

    if !parsed.files.is_empty() {
        // Agent output files via stdout delimiters — write them.
        let paths = write_context_files(repo_root, &parsed)?;
        eprintln!(
            "[context-gen] Generated {} context files at startup",
            paths.len()
        );
    } else if repo_root.join(".othala/context/MAIN.md").exists() {
        // Agent wrote files directly using its Write tool — just stamp the hash.
        if let Some(hash) = get_head_sha(repo_root) {
            write_stored_hash(repo_root, &hash)?;
        }
        let count = count_context_files(repo_root);
        eprintln!(
            "[context-gen] Agent wrote {} context files directly",
            count
        );
    } else {
        anyhow::bail!("context generation agent produced no context files");
    }

    Ok(())
}

/// Count `.md` files under `.othala/context/`.
fn count_context_files(repo_root: &Path) -> usize {
    let context_dir = repo_root.join(".othala/context");
    walkdir_md_count(&context_dir)
}

fn walkdir_md_count(dir: &Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += walkdir_md_count(&path);
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                count += 1;
            }
        }
    }
    count
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn context_is_current_requires_main_md_and_hash() {
        let tmp = std::env::temp_dir().join(format!(
            "othala-ctxgen-current-{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).unwrap();

        // No MAIN.md → not current.
        assert!(!context_is_current(&tmp));

        let ctx_dir = tmp.join(".othala/context");
        fs::create_dir_all(&ctx_dir).unwrap();
        fs::write(ctx_dir.join("MAIN.md"), "# Main").unwrap();

        // MAIN.md exists but no git hash file — not a git repo, so
        // get_head_sha returns None → considered current (graceful fallback).
        // (In a real repo with a HEAD, it would be stale without .git-hash.)

        // Write a hash to simulate.
        fs::write(ctx_dir.join(".git-hash"), "abc123").unwrap();

        // Hash won't match HEAD (not a real git repo) but get_head_sha
        // returns None → considered current.
        assert!(context_is_current(&tmp));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn should_regenerate_respects_cooldown() {
        let config = ContextGenConfig {
            cooldown_secs: 300,
            model: ModelKind::Claude,
        };

        // Never generated → should regenerate.
        let state = ContextGenState::new();
        assert!(should_regenerate(&state, &config, Utc::now()));

        // Recently generated → should not.
        let mut state = ContextGenState::new();
        state.last_generated_at = Some(Utc::now());
        assert!(!should_regenerate(&state, &config, Utc::now()));

        // Old generation → should regenerate.
        let mut state = ContextGenState::new();
        state.last_generated_at =
            Some(Utc::now() - chrono::Duration::seconds(600));
        assert!(should_regenerate(&state, &config, Utc::now()));
    }

    #[test]
    fn should_regenerate_blocks_while_running() {
        let config = ContextGenConfig::default();
        let mut state = ContextGenState::new();
        state.status = ContextGenStatus::Running;
        assert!(!should_regenerate(&state, &config, Utc::now()));
    }

    #[test]
    fn parse_context_gen_output_basic() {
        let raw = "\
Some preamble text ignored.
<!-- FILE: MAIN.md -->
# Main Context

This is the main file.
<!-- FILE: architecture.md -->
# Architecture

Crate layout here.
";
        let output = parse_context_gen_output(raw);
        assert_eq!(output.files.len(), 2);
        assert_eq!(output.files[0].filename, "MAIN.md");
        assert!(output.files[0].content.contains("main file"));
        assert_eq!(output.files[1].filename, "architecture.md");
        assert!(output.files[1].content.contains("Crate layout"));
    }

    #[test]
    fn parse_context_gen_output_empty() {
        let output = parse_context_gen_output("no file markers here");
        assert!(output.files.is_empty());
    }

    #[test]
    fn parse_context_gen_output_strips_path_traversal() {
        let raw = "<!-- FILE: ../../../etc/passwd -->\nevil content\n";
        let output = parse_context_gen_output(raw);
        assert_eq!(output.files.len(), 1);
        // Should have sanitized away ".." sequences.
        assert!(!output.files[0].filename.contains(".."));
    }

    #[test]
    fn sanitize_path_allows_subdirectories() {
        assert_eq!(sanitize_path("MAIN.md"), "MAIN.md");
        assert_eq!(sanitize_path("crates/orchd/overview.md"), "crates/orchd/overview.md");
        assert_eq!(sanitize_path("architecture/data-flow.md"), "architecture/data-flow.md");
    }

    #[test]
    fn sanitize_path_strips_dangerous_sequences() {
        assert_eq!(sanitize_path("../evil.md"), "evil.md");
        assert_eq!(sanitize_path("../../x"), "x");
        assert_eq!(sanitize_path(".."), "unnamed.md");
        assert_eq!(sanitize_path("foo\\bar.md"), "foobar.md");
        assert_eq!(sanitize_path("/etc/passwd"), "etc/passwd");
    }

    #[test]
    fn sanitize_path_empty_becomes_unnamed() {
        assert_eq!(sanitize_path(""), "unnamed.md");
        assert_eq!(sanitize_path(".."), "unnamed.md");
    }

    #[test]
    fn write_context_files_creates_subdirs_and_files() {
        let tmp = std::env::temp_dir().join(format!(
            "othala-ctxgen-write-{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).unwrap();

        let output = ContextGenOutput {
            files: vec![
                ContextFile {
                    filename: "MAIN.md".to_string(),
                    content: "# Main\n".to_string(),
                },
                ContextFile {
                    filename: "architecture/overview.md".to_string(),
                    content: "# Arch Overview\n".to_string(),
                },
                ContextFile {
                    filename: "crates/orchd/daemon-loop.md".to_string(),
                    content: "# Daemon Loop\n".to_string(),
                },
            ],
        };

        let paths = write_context_files(&tmp, &output).unwrap();
        assert_eq!(paths.len(), 3);
        assert!(tmp.join(".othala/context/MAIN.md").exists());
        assert!(tmp.join(".othala/context/architecture/overview.md").exists());
        assert!(tmp.join(".othala/context/crates/orchd/daemon-loop.md").exists());

        let content = fs::read_to_string(tmp.join(".othala/context/MAIN.md")).unwrap();
        assert_eq!(content, "# Main\n");
        let content = fs::read_to_string(tmp.join(".othala/context/crates/orchd/daemon-loop.md")).unwrap();
        assert_eq!(content, "# Daemon Loop\n");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn scan_repo_snapshot_includes_cargo_toml() {
        let tmp = std::env::temp_dir().join(format!(
            "othala-ctxgen-scan-{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();

        let snapshot = scan_repo_snapshot(&tmp);
        assert!(snapshot.contains("Cargo.toml"));
        assert!(snapshot.contains("members"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn build_context_gen_prompt_includes_snapshot() {
        let tmp = std::env::temp_dir().join(format!(
            "othala-ctxgen-prompt-{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("Cargo.toml"), "[workspace]\n").unwrap();

        let prompt = build_context_gen_prompt(&tmp, Path::new("/nonexistent-templates"));
        assert!(prompt.contains("Repository Snapshot"));
        assert!(prompt.contains("[workspace]"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn context_gen_state_default() {
        let state = ContextGenState::default();
        assert_eq!(state.status, ContextGenStatus::Idle);
        assert!(state.last_generated_at.is_none());
        assert!(state.result_rx.is_none());
        assert!(state.child_handle.is_none());
    }

    #[test]
    fn git_hash_roundtrip() {
        let tmp = std::env::temp_dir().join(format!(
            "othala-ctxgen-hash-{}",
            std::process::id()
        ));
        let ctx_dir = tmp.join(".othala/context");
        fs::create_dir_all(&ctx_dir).unwrap();

        // No hash file yet.
        assert!(read_stored_hash(&tmp).is_none());

        // Write and read back.
        write_stored_hash(&tmp, "abc123def456").unwrap();
        assert_eq!(read_stored_hash(&tmp).unwrap(), "abc123def456");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn parse_context_gen_output_nested_paths() {
        let raw = "\
<!-- FILE: MAIN.md -->
# Main
<!-- FILE: architecture/overview.md -->
# Architecture Overview
<!-- FILE: crates/orchd/daemon-loop.md -->
# Daemon Loop
";
        let output = parse_context_gen_output(raw);
        assert_eq!(output.files.len(), 3);
        assert_eq!(output.files[0].filename, "MAIN.md");
        assert_eq!(output.files[1].filename, "architecture/overview.md");
        assert_eq!(output.files[2].filename, "crates/orchd/daemon-loop.md");
    }
}
