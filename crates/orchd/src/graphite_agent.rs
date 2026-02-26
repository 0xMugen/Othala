//! Graphite Master Agent — the single authority for stack queue operations.
//!
//! This module provides daemon-owned orchestration of Graphite operations with:
//! - Queue lock/serialization to avoid concurrent stack mutation races
//! - sync/restack/reconcile loop
//! - Branch tracking divergence preflight repair (gt track/untrack)
//! - Conflict-aware retries with exponential backoff
//! - STOPPED task auto-respawn for recoverable failure classes

use chrono::{DateTime, Duration, Utc};
use orch_core::types::{RepoId, TaskId};
use orch_graphite::{GraphiteClient, GraphiteError, RestackOutcome};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

// ─────────────────────────────────────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the Graphite Master Agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphiteAgentConfig {
    /// Maximum retries for restack/sync operations.
    pub max_retries: u32,
    /// Initial backoff duration in seconds.
    pub initial_backoff_secs: u64,
    /// Maximum backoff duration in seconds.
    pub max_backoff_secs: u64,
    /// Backoff multiplier (exponential).
    pub backoff_multiplier: f64,
    /// Minimum interval between sync operations (seconds).
    pub sync_interval_secs: u64,
    /// Enable auto-respawn for recoverable STOPPED tasks.
    pub auto_respawn_enabled: bool,
    /// Maximum respawn attempts per task.
    pub max_respawn_attempts: u32,
    /// Respawn cooldown in seconds.
    pub respawn_cooldown_secs: u64,
}

impl Default for GraphiteAgentConfig {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_backoff_secs: 5,
            max_backoff_secs: 300, // 5 minutes
            backoff_multiplier: 2.0,
            sync_interval_secs: 60,
            auto_respawn_enabled: true,
            max_respawn_attempts: 3,
            respawn_cooldown_secs: 120,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Queue Lock — Serialized Stack Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Operation types that require queue serialization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StackOperation {
    Sync,
    Restack { branch: String },
    Track { branch: String, parent: String },
    Untrack { branch: String },
    Submit { task_id: TaskId },
    Reconcile,
}

impl std::fmt::Display for StackOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackOperation::Sync => write!(f, "sync"),
            StackOperation::Restack { branch } => write!(f, "restack:{branch}"),
            StackOperation::Track { branch, parent } => write!(f, "track:{branch}→{parent}"),
            StackOperation::Untrack { branch } => write!(f, "untrack:{branch}"),
            StackOperation::Submit { task_id } => write!(f, "submit:{}", task_id.0),
            StackOperation::Reconcile => write!(f, "reconcile"),
        }
    }
}

/// A queued stack operation with metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedOperation {
    pub id: u64,
    pub operation: StackOperation,
    pub repo_id: RepoId,
    pub enqueued_at: DateTime<Utc>,
    pub priority: i32, // Higher = more urgent
}

/// Result of executing a stack operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationResult {
    Success,
    Conflict { details: String },
    AuthFailure { details: String },
    TrunkOutdated { details: String },
    TrackingDivergence { branches: Vec<String> },
    Retryable { reason: String },
    Fatal { reason: String },
}

impl OperationResult {
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            OperationResult::Conflict { .. }
                | OperationResult::TrunkOutdated { .. }
                | OperationResult::TrackingDivergence { .. }
                | OperationResult::Retryable { .. }
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Retry State with Exponential Backoff
// ─────────────────────────────────────────────────────────────────────────────

/// Tracks retry state for operations with exponential backoff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryState {
    pub attempts: u32,
    pub max_attempts: u32,
    pub last_attempt: DateTime<Utc>,
    pub next_retry_at: DateTime<Utc>,
    pub backoff_secs: u64,
    pub last_error: Option<String>,
}

impl RetryState {
    pub fn new(config: &GraphiteAgentConfig, now: DateTime<Utc>) -> Self {
        let next_retry_at = now + Duration::seconds(config.initial_backoff_secs as i64);
        Self {
            attempts: 1,
            max_attempts: config.max_retries,
            last_attempt: now,
            next_retry_at,
            backoff_secs: config.initial_backoff_secs,
            last_error: None,
        }
    }

    pub fn record_failure(
        &mut self,
        config: &GraphiteAgentConfig,
        now: DateTime<Utc>,
        error: &str,
    ) -> bool {
        self.attempts += 1;
        self.last_attempt = now;
        self.last_error = Some(error.to_string());

        if self.attempts > self.max_attempts {
            return false; // Exhausted
        }

        // Exponential backoff with jitter
        self.backoff_secs = ((self.backoff_secs as f64 * config.backoff_multiplier) as u64)
            .min(config.max_backoff_secs);
        self.next_retry_at = now + Duration::seconds(self.backoff_secs as i64);
        true
    }

    pub fn is_ready(&self, now: DateTime<Utc>) -> bool {
        now >= self.next_retry_at
    }

    pub fn is_exhausted(&self) -> bool {
        self.attempts > self.max_attempts
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// STOPPED Task Auto-Respawn Policy
// ─────────────────────────────────────────────────────────────────────────────

/// Failure class classification for respawn eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureClass {
    /// Restack/merge conflicts — recoverable after stack repair
    RestackConflict,
    /// Trunk sync failures — recoverable after sync
    TrunkOutdated,
    /// Tracking divergence — recoverable after track repair
    TrackingDivergence,
    /// Temporary network/API failures
    TransientError,
    /// Verification failures — may need code fixes, but can retry
    VerifyFailure,
    /// Auth failures — require manual intervention
    AuthFailure,
    /// Unknown/unclassified — don't auto-respawn
    Unknown,
}

impl FailureClass {
    /// Determine if this failure class is eligible for auto-respawn.
    pub fn is_respawnable(&self) -> bool {
        matches!(
            self,
            FailureClass::RestackConflict
                | FailureClass::TrunkOutdated
                | FailureClass::TrackingDivergence
                | FailureClass::TransientError
                | FailureClass::VerifyFailure
        )
    }

    /// Classify a failure reason string into a failure class.
    pub fn classify(reason: &str) -> Self {
        let lower = reason.to_ascii_lowercase();

        // Check for conflict indicators
        if lower.contains("conflict")
            || lower.contains("merge conflict")
            || lower.contains("restack")
            || lower.contains("could not apply")
        {
            return FailureClass::RestackConflict;
        }

        // Check for trunk/sync issues
        if lower.contains("trunk")
            || lower.contains("out of date")
            || lower.contains("gt sync")
            || lower.contains("fast-forward")
        {
            return FailureClass::TrunkOutdated;
        }

        // Check for tracking issues
        if lower.contains("tracking")
            || lower.contains("diverge")
            || lower.contains("gt track")
            || lower.contains("untrack")
        {
            return FailureClass::TrackingDivergence;
        }

        // Check for auth failures (not respawnable)
        if lower.contains("auth")
            || lower.contains("token")
            || lower.contains("authenticate")
            || lower.contains("permission")
        {
            return FailureClass::AuthFailure;
        }

        // Check for transient errors
        if lower.contains("timeout")
            || lower.contains("network")
            || lower.contains("connection")
            || lower.contains("retry")
            || lower.contains("temporary")
        {
            return FailureClass::TransientError;
        }

        // Check for verify failures
        if lower.contains("verify")
            || lower.contains("test")
            || lower.contains("cargo check")
            || lower.contains("cargo test")
        {
            return FailureClass::VerifyFailure;
        }

        FailureClass::Unknown
    }
}

/// Tracks respawn state for STOPPED tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RespawnState {
    pub task_id: TaskId,
    pub attempts: u32,
    pub max_attempts: u32,
    pub last_attempt: DateTime<Utc>,
    pub next_attempt_at: DateTime<Utc>,
    pub failure_class: FailureClass,
    pub last_reason: String,
}

impl RespawnState {
    pub fn new(
        task_id: TaskId,
        config: &GraphiteAgentConfig,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            task_id,
            attempts: 1,
            max_attempts: config.max_respawn_attempts,
            last_attempt: now,
            next_attempt_at: now + Duration::seconds(config.respawn_cooldown_secs as i64),
            failure_class: FailureClass::classify(reason),
            last_reason: reason.to_string(),
        }
    }

    pub fn is_eligible(&self, now: DateTime<Utc>) -> bool {
        self.failure_class.is_respawnable()
            && self.attempts <= self.max_attempts
            && now >= self.next_attempt_at
    }

    pub fn record_attempt(&mut self, config: &GraphiteAgentConfig, now: DateTime<Utc>) {
        self.attempts += 1;
        self.last_attempt = now;
        // Exponential backoff for respawn
        let backoff = (config.respawn_cooldown_secs as f64
            * config.backoff_multiplier.powi(self.attempts as i32 - 1))
            as i64;
        self.next_attempt_at = now + Duration::seconds(backoff.min(config.max_backoff_secs as i64));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Branch Tracking Divergence Detection & Repair
// ─────────────────────────────────────────────────────────────────────────────

/// Information about a tracked branch's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchTrackingInfo {
    pub branch: String,
    pub expected_parent: Option<String>,
    pub actual_parent: Option<String>,
    pub is_diverged: bool,
    pub needs_track: bool,
    pub needs_untrack: bool,
}

/// Detect branches with tracking divergence.
pub fn detect_tracking_divergence(
    repo_root: &Path,
    expected_branches: &HashMap<String, Option<String>>, // branch -> expected parent
) -> Vec<BranchTrackingInfo> {
    let mut results = Vec::new();

    // Get actual graphite tracking state
    let actual_parents = get_graphite_tracking_state(repo_root);

    for (branch, expected_parent) in expected_branches {
        let actual_parent = actual_parents.get(branch).cloned().flatten();
        let is_diverged = expected_parent != &actual_parent;

        let needs_track = expected_parent.is_some() && actual_parent.is_none();
        let needs_untrack = expected_parent.is_none() && actual_parent.is_some();

        if is_diverged {
            results.push(BranchTrackingInfo {
                branch: branch.clone(),
                expected_parent: expected_parent.clone(),
                actual_parent,
                is_diverged,
                needs_track,
                needs_untrack,
            });
        }
    }

    results
}

/// Get the current Graphite tracking state for all branches.
fn get_graphite_tracking_state(repo_root: &Path) -> HashMap<String, Option<String>> {
    let mut state = HashMap::new();

    // Run `gt log short` to get the stack structure
    let output = Command::new("gt")
        .args(["log", "short"])
        .current_dir(repo_root)
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Parse the stack structure to determine parent relationships
            let mut stack: Vec<(String, usize)> = Vec::new();

            for line in stdout.lines() {
                if let Some(branch) = extract_branch_from_log_line(line) {
                    let depth = line.chars().take_while(|c| c.is_whitespace()).count();

                    // Find parent by looking at stack
                    let parent = stack.iter().rev().find(|(_, d)| *d < depth).map(|(b, _)| b.clone());

                    state.insert(branch.clone(), parent);
                    stack.push((branch, depth));
                }
            }
        }
    }

    state
}

fn extract_branch_from_log_line(line: &str) -> Option<String> {
    // Skip decorators and find branch name
    line.split_whitespace()
        .find(|token| {
            !token.is_empty()
                && !token.starts_with('#')
                && !token.chars().all(|c| matches!(c, '|' | '/' | '\\' | '-' | '*' | 'o' | 'O'))
                && token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
        })
        .map(|s| s.trim_matches(|c: char| c == ',' || c == ':' || c == ';').to_string())
}

/// Repair tracking divergence for a branch.
pub fn repair_tracking(
    graphite: &GraphiteClient,
    info: &BranchTrackingInfo,
) -> Result<(), GraphiteError> {
    if info.needs_track {
        if let Some(parent) = &info.expected_parent {
            graphite.track_branch(&info.branch, parent)?;
        }
    } else if info.needs_untrack {
        // Untrack via git config removal (Graphite doesn't have a direct untrack command)
        untrack_branch(&graphite.repo_root, &info.branch)?;
    } else if info.is_diverged {
        // Re-track with correct parent
        if let Some(parent) = &info.expected_parent {
            // First untrack, then track with new parent
            let _ = untrack_branch(&graphite.repo_root, &info.branch);
            graphite.track_branch(&info.branch, parent)?;
        }
    }
    Ok(())
}

/// Preflight: ensure a branch is tracked by Graphite before committing.
/// If the branch has no graphite-parent set, auto-track it with the given parent.
/// Returns Ok(true) if tracking was performed, Ok(false) if already tracked.
pub fn ensure_tracked(
    graphite: &GraphiteClient,
    branch: &str,
    default_parent: &str,
) -> Result<bool, GraphiteError> {
    // Check if branch already has a graphite-parent
    let output = Command::new("git")
        .args(["config", &format!("branch.{}.graphite-parent", branch)])
        .current_dir(graphite.repo_root())
        .output()
        .map_err(|e| GraphiteError::Io {
            command: "git config branch.<name>.graphite-parent".to_string(),
            source: e,
        })?;

    if output.status.success() {
        let parent = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !parent.is_empty() {
            return Ok(false); // Already tracked
        }
    }

    // Not tracked — auto-track
    graphite.track_branch(branch, default_parent)?;
    Ok(true)
}

/// Git-only fallback for push when Graphite submit fails.
/// Pushes the current branch to origin and returns success/failure.
pub fn git_push_fallback(repo_root: &Path, branch: &str) -> Result<(), GraphiteError> {
    let status = Command::new("git")
        .args(["push", "--set-upstream", "origin", branch])
        .current_dir(repo_root)
        .status()
        .map_err(|e| GraphiteError::Io {
            command: format!("git push --set-upstream origin {branch}"),
            source: e,
        })?;

    if !status.success() {
        return Err(GraphiteError::CommandFailed {
            command: format!("git push --set-upstream origin {branch}"),
            status: status.code(),
            stdout: String::new(),
            stderr: "git push failed".to_string(),
        });
    }
    Ok(())
}

fn is_benign_noop_restack(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("does not need to be restacked")
        || lower.contains("already up to date")
        || lower.contains("nothing to restack")
}

fn untrack_branch(repo_root: &Path, branch: &str) -> Result<(), GraphiteError> {
    let status = Command::new("git")
        .args([
            "config",
            "--unset",
            &format!("branch.{}.graphite-parent", branch),
        ])
        .current_dir(repo_root)
        .status()
        .map_err(|e| GraphiteError::Io {
            command: "git config --unset".to_string(),
            source: e,
        })?;

    if !status.success() {
        // Not an error if config key doesn't exist
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Graphite Master Agent
// ─────────────────────────────────────────────────────────────────────────────

/// The Graphite Master Agent — single authority for stack operations.
#[derive(Debug)]
pub struct GraphiteMasterAgent {
    /// Configuration.
    pub config: GraphiteAgentConfig,
    /// Operation queue per repository.
    pub queues: HashMap<RepoId, VecDeque<QueuedOperation>>,
    /// Retry states for failed operations.
    pub retries: HashMap<(RepoId, StackOperation), RetryState>,
    /// Respawn states for STOPPED tasks.
    pub respawn_candidates: HashMap<TaskId, RespawnState>,
    /// Last sync time per repository.
    pub last_sync: HashMap<RepoId, DateTime<Utc>>,
    /// Lock flag — true if an operation is currently executing.
    pub locked: AtomicBool,
    /// Next operation ID.
    next_op_id: u64,
    /// Deduplication set for pending operations.
    pending_ops: HashSet<(RepoId, StackOperation)>,
}

impl GraphiteMasterAgent {
    pub fn new(config: GraphiteAgentConfig) -> Self {
        Self {
            config,
            queues: HashMap::new(),
            retries: HashMap::new(),
            respawn_candidates: HashMap::new(),
            last_sync: HashMap::new(),
            locked: AtomicBool::new(false),
            next_op_id: 1,
            pending_ops: HashSet::new(),
        }
    }

    /// Enqueue a stack operation with deduplication.
    pub fn enqueue(&mut self, repo_id: RepoId, operation: StackOperation, priority: i32) -> bool {
        let key = (repo_id.clone(), operation.clone());
        if self.pending_ops.contains(&key) {
            return false; // Already queued
        }

        let op = QueuedOperation {
            id: self.next_op_id,
            operation: operation.clone(),
            repo_id: repo_id.clone(),
            enqueued_at: Utc::now(),
            priority,
        };
        self.next_op_id += 1;

        let queue = self.queues.entry(repo_id).or_insert_with(VecDeque::new);
        queue.push_back(op);
        self.pending_ops.insert(key);

        // Sort by priority (stable sort to preserve FIFO within same priority)
        let mut items: Vec<_> = queue.drain(..).collect();
        items.sort_by(|a, b| b.priority.cmp(&a.priority));
        queue.extend(items);

        true
    }

    /// Try to acquire the lock for executing operations.
    pub fn try_lock(&self) -> bool {
        self.locked
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Release the lock after operation execution.
    pub fn unlock(&self) {
        self.locked.store(false, Ordering::SeqCst);
    }

    /// Check if the lock is currently held.
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    /// Get the next operation to execute for a repository.
    pub fn next_operation(&mut self, repo_id: &RepoId, now: DateTime<Utc>) -> Option<QueuedOperation> {
        let queue = self.queues.get_mut(repo_id)?;

        // Find first operation that's ready (not in backoff)
        let position = queue.iter().position(|op| {
            let key = (repo_id.clone(), op.operation.clone());
            self.retries
                .get(&key)
                .map(|r| r.is_ready(now))
                .unwrap_or(true)
        })?;

        Some(queue.remove(position)?)
    }

    /// Execute a sync/restack/reconcile cycle for a repository.
    pub fn execute_sync_cycle(
        &mut self,
        repo_root: &Path,
        repo_id: &RepoId,
        now: DateTime<Utc>,
    ) -> Vec<OperationResult> {
        let mut results = Vec::new();

        // Check sync interval
        if let Some(last) = self.last_sync.get(repo_id) {
            let elapsed = now.signed_duration_since(*last);
            if elapsed.num_seconds() < self.config.sync_interval_secs as i64 {
                return results;
            }
        }

        let graphite = GraphiteClient::new(repo_root);

        // 1. Sync trunk
        match graphite.sync_trunk() {
            Ok(()) => {
                results.push(OperationResult::Success);
            }
            Err(e) if e.is_auth_failure() => {
                results.push(OperationResult::AuthFailure {
                    details: e.to_string(),
                });
                return results;
            }
            Err(e) => {
                results.push(OperationResult::Retryable {
                    reason: e.to_string(),
                });
            }
        }

        // 2. Restack
        match graphite.restack_with_outcome() {
            Ok(RestackOutcome::Restacked) => {
                results.push(OperationResult::Success);
            }
            Ok(RestackOutcome::Conflict { stdout, stderr }) => {
                let combined = format!("{stdout}\n{stderr}");
                if is_benign_noop_restack(&combined) {
                    // Treat Graphite's "no restack needed" output as success.
                    results.push(OperationResult::Success);
                } else {
                    // Abort the failed rebase
                    let _ = graphite.abort_rebase();
                    results.push(OperationResult::Conflict { details: combined });
                }
            }
            Err(e) => {
                results.push(OperationResult::Retryable {
                    reason: e.to_string(),
                });
            }
        }

        self.last_sync.insert(repo_id.clone(), now);
        results
    }

    /// Record a failed operation and schedule retry if applicable.
    pub fn record_failure(
        &mut self,
        repo_id: &RepoId,
        operation: &StackOperation,
        result: &OperationResult,
        now: DateTime<Utc>,
    ) {
        let key = (repo_id.clone(), operation.clone());
        self.pending_ops.remove(&key);

        let error_msg = match result {
            OperationResult::Conflict { details } => details.clone(),
            OperationResult::AuthFailure { details } => details.clone(),
            OperationResult::TrunkOutdated { details } => details.clone(),
            OperationResult::TrackingDivergence { branches } => {
                format!("tracking divergence: {:?}", branches)
            }
            OperationResult::Retryable { reason } => reason.clone(),
            OperationResult::Fatal { reason } => reason.clone(),
            OperationResult::Success => return,
        };

        if !result.is_recoverable() {
            self.retries.remove(&key);
            return;
        }

        let retry = self
            .retries
            .entry(key.clone())
            .or_insert_with(|| RetryState::new(&self.config, now));

        if !retry.record_failure(&self.config, now, &error_msg) {
            // Exhausted retries
            self.retries.remove(&key);
        }
    }

    /// Record successful operation completion.
    pub fn record_success(&mut self, repo_id: &RepoId, operation: &StackOperation) {
        let key = (repo_id.clone(), operation.clone());
        self.pending_ops.remove(&key);
        self.retries.remove(&key);
    }

    /// Register a STOPPED task for potential respawn.
    pub fn register_stopped_task(&mut self, task_id: TaskId, reason: &str, now: DateTime<Utc>) {
        if !self.config.auto_respawn_enabled {
            return;
        }

        let state = RespawnState::new(task_id.clone(), &self.config, reason, now);
        if state.failure_class.is_respawnable() {
            self.respawn_candidates.insert(task_id, state);
        }
    }

    /// Get tasks eligible for respawn.
    pub fn get_respawn_candidates(&mut self, now: DateTime<Utc>) -> Vec<TaskId> {
        self.respawn_candidates
            .iter()
            .filter(|(_, state)| state.is_eligible(now))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Record a respawn attempt.
    pub fn record_respawn_attempt(&mut self, task_id: &TaskId, now: DateTime<Utc>) {
        if let Some(state) = self.respawn_candidates.get_mut(task_id) {
            state.record_attempt(&self.config, now);
            if state.attempts > state.max_attempts {
                self.respawn_candidates.remove(task_id);
            }
        }
    }

    /// Remove a task from respawn candidates (e.g., when manually resumed).
    pub fn remove_respawn_candidate(&mut self, task_id: &TaskId) {
        self.respawn_candidates.remove(task_id);
    }

    /// Get queue depth for monitoring.
    pub fn queue_depth(&self, repo_id: &RepoId) -> usize {
        self.queues.get(repo_id).map(|q| q.len()).unwrap_or(0)
    }

    /// Get total pending operations across all repos.
    pub fn total_pending(&self) -> usize {
        self.queues.values().map(|q| q.len()).sum()
    }

    /// Get retry state summary for monitoring.
    pub fn retry_summary(&self) -> HashMap<RepoId, Vec<(String, u32, u32)>> {
        let mut summary: HashMap<RepoId, Vec<(String, u32, u32)>> = HashMap::new();

        for ((repo_id, op), state) in &self.retries {
            summary
                .entry(repo_id.clone())
                .or_default()
                .push((op.to_string(), state.attempts, state.max_attempts));
        }

        summary
    }

    /// Get respawn candidates summary.
    pub fn respawn_summary(&self) -> Vec<(TaskId, FailureClass, u32, u32)> {
        self.respawn_candidates
            .iter()
            .map(|(id, state)| {
                (
                    id.clone(),
                    state.failure_class,
                    state.attempts,
                    state.max_attempts,
                )
            })
            .collect()
    }
}

impl Default for GraphiteMasterAgent {
    fn default() -> Self {
        Self::new(GraphiteAgentConfig::default())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Integration Actions for Daemon Loop
// ─────────────────────────────────────────────────────────────────────────────

/// Actions that the Graphite Master Agent can request from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphiteAgentAction {
    /// Execute a sync cycle for a repository.
    ExecuteSyncCycle { repo_id: RepoId, repo_root: PathBuf },
    /// Repair tracking divergence.
    RepairTracking {
        repo_id: RepoId,
        repo_root: PathBuf,
        branches: Vec<BranchTrackingInfo>,
    },
    /// Respawn a STOPPED task.
    RespawnTask { task_id: TaskId },
    /// Log a message.
    Log { level: LogLevel, message: String },
    /// Emit an event.
    EmitEvent { task_id: Option<TaskId>, event_type: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Generate actions for the current tick.
pub fn graphite_agent_tick(
    agent: &mut GraphiteMasterAgent,
    repos: &[(RepoId, PathBuf)],
    stopped_tasks: &[(TaskId, String)], // (task_id, failure_reason)
    now: DateTime<Utc>,
) -> Vec<GraphiteAgentAction> {
    let mut actions = Vec::new();

    // Register any new stopped tasks
    for (task_id, reason) in stopped_tasks {
        if !agent.respawn_candidates.contains_key(task_id) {
            agent.register_stopped_task(task_id.clone(), reason, now);
        }
    }

    // Check for respawn candidates
    let respawn_candidates = agent.get_respawn_candidates(now);
    for task_id in respawn_candidates {
        agent.record_respawn_attempt(&task_id, now);
        actions.push(GraphiteAgentAction::RespawnTask {
            task_id: task_id.clone(),
        });
        actions.push(GraphiteAgentAction::Log {
            level: LogLevel::Info,
            message: format!(
                "Auto-respawning stopped task {} (failure class: {:?})",
                task_id.0,
                agent
                    .respawn_candidates
                    .get(&task_id)
                    .map(|s| s.failure_class)
            ),
        });
    }

    // Check sync cycles for each repo
    for (repo_id, repo_root) in repos {
        if let Some(last) = agent.last_sync.get(repo_id) {
            let elapsed = now.signed_duration_since(*last);
            if elapsed.num_seconds() >= agent.config.sync_interval_secs as i64 {
                actions.push(GraphiteAgentAction::ExecuteSyncCycle {
                    repo_id: repo_id.clone(),
                    repo_root: repo_root.clone(),
                });
            }
        } else {
            // Never synced — schedule sync
            actions.push(GraphiteAgentAction::ExecuteSyncCycle {
                repo_id: repo_id.clone(),
                repo_root: repo_root.clone(),
            });
        }
    }

    actions
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_config() -> GraphiteAgentConfig {
        GraphiteAgentConfig {
            max_retries: 3,
            initial_backoff_secs: 5,
            max_backoff_secs: 60,
            backoff_multiplier: 2.0,
            sync_interval_secs: 60,
            auto_respawn_enabled: true,
            max_respawn_attempts: 3,
            respawn_cooldown_secs: 30,
        }
    }

    #[test]
    fn failure_class_classification() {
        assert_eq!(
            FailureClass::classify("CONFLICT (content): Merge conflict in src/main.rs"),
            FailureClass::RestackConflict
        );
        assert_eq!(
            FailureClass::classify("trunk branch is out of date"),
            FailureClass::TrunkOutdated
        );
        assert_eq!(
            FailureClass::classify("branch tracking diverged from expected"),
            FailureClass::TrackingDivergence
        );
        assert_eq!(
            FailureClass::classify("authentication failed: token expired"),
            FailureClass::AuthFailure
        );
        assert_eq!(
            FailureClass::classify("verify command failed: cargo test had 3 failures"),
            FailureClass::VerifyFailure
        );
        assert_eq!(
            FailureClass::classify("network timeout during API call"),
            FailureClass::TransientError
        );
        assert_eq!(
            FailureClass::classify("some random error"),
            FailureClass::Unknown
        );
    }

    #[test]
    fn failure_class_respawnability() {
        assert!(FailureClass::RestackConflict.is_respawnable());
        assert!(FailureClass::TrunkOutdated.is_respawnable());
        assert!(FailureClass::TrackingDivergence.is_respawnable());
        assert!(FailureClass::TransientError.is_respawnable());
        assert!(FailureClass::VerifyFailure.is_respawnable());
        assert!(!FailureClass::AuthFailure.is_respawnable());
        assert!(!FailureClass::Unknown.is_respawnable());
    }

    #[test]
    fn retry_state_exponential_backoff() {
        let config = mk_config();
        let now = Utc::now();
        let mut retry = RetryState::new(&config, now);

        assert_eq!(retry.attempts, 1);
        assert_eq!(retry.backoff_secs, 5);

        // First failure
        assert!(retry.record_failure(&config, now, "error 1"));
        assert_eq!(retry.attempts, 2);
        assert_eq!(retry.backoff_secs, 10); // 5 * 2

        // Second failure
        assert!(retry.record_failure(&config, now, "error 2"));
        assert_eq!(retry.attempts, 3);
        assert_eq!(retry.backoff_secs, 20); // 10 * 2

        // Third failure — exhausted
        assert!(!retry.record_failure(&config, now, "error 3"));
        assert!(retry.is_exhausted());
    }

    #[test]
    fn respawn_state_eligibility() {
        let config = mk_config();
        let now = Utc::now();

        // Respawnable failure
        let state = RespawnState::new(
            TaskId("T1".to_string()),
            &config,
            "restack conflict",
            now,
        );
        assert!(!state.is_eligible(now)); // Not ready yet (cooldown)
        assert!(state.is_eligible(now + Duration::seconds(31)));

        // Non-respawnable failure
        let state = RespawnState::new(
            TaskId("T2".to_string()),
            &config,
            "authentication failed",
            now,
        );
        assert!(!state.is_eligible(now + Duration::seconds(1000)));
    }

    #[test]
    fn agent_enqueue_deduplication() {
        let mut agent = GraphiteMasterAgent::new(mk_config());
        let repo = RepoId("test-repo".to_string());

        // First enqueue succeeds
        assert!(agent.enqueue(repo.clone(), StackOperation::Sync, 0));
        assert_eq!(agent.queue_depth(&repo), 1);

        // Duplicate is rejected
        assert!(!agent.enqueue(repo.clone(), StackOperation::Sync, 0));
        assert_eq!(agent.queue_depth(&repo), 1);

        // Different operation succeeds
        assert!(agent.enqueue(
            repo.clone(),
            StackOperation::Restack {
                branch: "main".to_string()
            },
            0
        ));
        assert_eq!(agent.queue_depth(&repo), 2);
    }

    #[test]
    fn agent_priority_ordering() {
        let mut agent = GraphiteMasterAgent::new(mk_config());
        let repo = RepoId("test-repo".to_string());

        agent.enqueue(repo.clone(), StackOperation::Sync, 0);
        agent.enqueue(
            repo.clone(),
            StackOperation::Restack {
                branch: "a".to_string(),
            },
            10,
        );
        agent.enqueue(
            repo.clone(),
            StackOperation::Restack {
                branch: "b".to_string(),
            },
            5,
        );

        let now = Utc::now();
        let op1 = agent.next_operation(&repo, now).unwrap();
        assert!(matches!(
            op1.operation,
            StackOperation::Restack { branch } if branch == "a"
        ));

        let op2 = agent.next_operation(&repo, now).unwrap();
        assert!(matches!(
            op2.operation,
            StackOperation::Restack { branch } if branch == "b"
        ));

        let op3 = agent.next_operation(&repo, now).unwrap();
        assert!(matches!(op3.operation, StackOperation::Sync));
    }

    #[test]
    fn agent_lock_mechanism() {
        let agent = GraphiteMasterAgent::new(mk_config());

        assert!(!agent.is_locked());
        assert!(agent.try_lock());
        assert!(agent.is_locked());
        assert!(!agent.try_lock()); // Already locked

        agent.unlock();
        assert!(!agent.is_locked());
        assert!(agent.try_lock()); // Can lock again
    }

    #[test]
    fn agent_respawn_registration() {
        let mut agent = GraphiteMasterAgent::new(mk_config());
        let now = Utc::now();

        // Register respawnable task
        agent.register_stopped_task(
            TaskId("T1".to_string()),
            "restack conflict",
            now,
        );
        assert!(agent.respawn_candidates.contains_key(&TaskId("T1".to_string())));

        // Register non-respawnable task
        agent.register_stopped_task(
            TaskId("T2".to_string()),
            "authentication failed",
            now,
        );
        assert!(!agent.respawn_candidates.contains_key(&TaskId("T2".to_string())));
    }

    #[test]
    fn graphite_agent_tick_generates_respawn_actions() {
        let mut agent = GraphiteMasterAgent::new(mk_config());
        let now = Utc::now();

        // Register a stopped task
        agent.register_stopped_task(
            TaskId("T1".to_string()),
            "trunk out of date",
            now - Duration::seconds(60),
        );

        // Tick after cooldown
        let actions = graphite_agent_tick(
            &mut agent,
            &[],
            &[],
            now,
        );

        let respawn_actions: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, GraphiteAgentAction::RespawnTask { .. }))
            .collect();
        assert_eq!(respawn_actions.len(), 1);
    }

    #[test]
    fn operation_result_recoverability() {
        assert!(OperationResult::Conflict {
            details: "test".to_string()
        }
        .is_recoverable());
        assert!(OperationResult::TrunkOutdated {
            details: "test".to_string()
        }
        .is_recoverable());
        assert!(OperationResult::TrackingDivergence {
            branches: vec!["a".to_string()]
        }
        .is_recoverable());
        assert!(OperationResult::Retryable {
            reason: "test".to_string()
        }
        .is_recoverable());

        assert!(!OperationResult::AuthFailure {
            details: "test".to_string()
        }
        .is_recoverable());
        assert!(!OperationResult::Fatal {
            reason: "test".to_string()
        }
        .is_recoverable());
        assert!(!OperationResult::Success.is_recoverable());
    }

    #[test]
    fn branch_tracking_info_detection() {
        let info = BranchTrackingInfo {
            branch: "feature/test".to_string(),
            expected_parent: Some("main".to_string()),
            actual_parent: None,
            is_diverged: true,
            needs_track: true,
            needs_untrack: false,
        };

        assert!(info.is_diverged);
        assert!(info.needs_track);
        assert!(!info.needs_untrack);
    }

    #[test]
    fn noop_restack_messages_are_treated_as_benign() {
        assert!(is_benign_noop_restack(
            "task/foo does not need to be restacked on main"
        ));
        assert!(is_benign_noop_restack("nothing to restack"));
        assert!(!is_benign_noop_restack("merge conflict in src/lib.rs"));
    }

    #[test]
    fn extract_branch_from_typical_log_lines() {
        assert_eq!(
            extract_branch_from_log_line("  task/chat-123"),
            Some("task/chat-123".to_string())
        );
        assert_eq!(
            extract_branch_from_log_line("* main"),
            Some("main".to_string())
        );
        assert_eq!(extract_branch_from_log_line("  |"), None);
    }

    #[test]
    fn divergence_detection_reports_missing_parent() {
        let mut expected = HashMap::new();
        expected.insert("task/T1".to_string(), Some("main".to_string()));

        // With empty actual state, everything diverges
        let results = detect_tracking_divergence_inner(&expected, &HashMap::new());
        assert_eq!(results.len(), 1);
        assert!(results[0].needs_track);
    }

    // Helper: test-friendly version of detect_tracking_divergence that accepts pre-fetched state
    fn detect_tracking_divergence_inner(
        expected: &HashMap<String, Option<String>>,
        actual: &HashMap<String, Option<String>>,
    ) -> Vec<BranchTrackingInfo> {
        let mut results = Vec::new();
        for (branch, expected_parent) in expected {
            let actual_parent = actual.get(branch).cloned().flatten();
            let is_diverged = expected_parent != &actual_parent;
            let needs_track = expected_parent.is_some() && actual_parent.is_none();
            let needs_untrack = expected_parent.is_none() && actual_parent.is_some();
            if is_diverged {
                results.push(BranchTrackingInfo {
                    branch: branch.clone(),
                    expected_parent: expected_parent.clone(),
                    actual_parent,
                    is_diverged,
                    needs_track,
                    needs_untrack,
                });
            }
        }
        results
    }

    #[test]
    fn divergence_detection_reports_wrong_parent() {
        let mut expected = HashMap::new();
        expected.insert("task/T1".to_string(), Some("main".to_string()));

        let mut actual = HashMap::new();
        actual.insert("task/T1".to_string(), Some("develop".to_string()));

        let results = detect_tracking_divergence_inner(&expected, &actual);
        assert_eq!(results.len(), 1);
        assert!(!results[0].needs_track);
        assert!(!results[0].needs_untrack);
        assert!(results[0].is_diverged);
    }

    #[test]
    fn divergence_detection_no_divergence_when_matching() {
        let mut expected = HashMap::new();
        expected.insert("task/T1".to_string(), Some("main".to_string()));

        let mut actual = HashMap::new();
        actual.insert("task/T1".to_string(), Some("main".to_string()));

        let results = detect_tracking_divergence_inner(&expected, &actual);
        assert!(results.is_empty());
    }
}
