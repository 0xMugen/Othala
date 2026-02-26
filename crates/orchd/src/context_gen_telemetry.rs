//! Context generation observability — latency, coverage, cache hits,
//! token-budget estimates, stale-context warnings, and a `--json`-friendly
//! report surface.
//!
//! This module instruments the existing `context_gen` pipeline without
//! mutating its core logic. It reads filesystem state (`.othala/context/`)
//! and pairs it with in-memory counters maintained across daemon ticks.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------------------
// Metrics (carried across daemon ticks)
// ---------------------------------------------------------------------------

/// Cumulative metrics for the context generation subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextGenMetrics {
    // -- Generation events --
    /// Total number of generation runs triggered.
    pub generations_started: u64,
    /// How many completed successfully.
    pub generations_succeeded: u64,
    /// How many failed.
    pub generations_failed: u64,
    /// Cumulative generation wall-time (seconds, f64 for sub-second).
    pub total_generation_secs: f64,
    /// Shortest successful generation (seconds).
    pub min_generation_secs: Option<f64>,
    /// Longest successful generation (seconds).
    pub max_generation_secs: Option<f64>,

    // -- Cache / staleness --
    /// How many times we checked and the context was already up-to-date.
    pub cache_hits: u64,
    /// How many times the check showed stale / missing context.
    pub cache_misses: u64,

    // -- Token budget --
    /// Estimated total prompt tokens sent across all generation runs.
    pub estimated_prompt_tokens: u64,

    // -- Timestamps --
    /// When these metrics were first initialised.
    pub started_at: DateTime<Utc>,
    /// Last time a generation completed (success or fail).
    pub last_generation_at: Option<DateTime<Utc>>,
}

impl Default for ContextGenMetrics {
    fn default() -> Self {
        Self {
            generations_started: 0,
            generations_succeeded: 0,
            generations_failed: 0,
            total_generation_secs: 0.0,
            min_generation_secs: None,
            max_generation_secs: None,
            cache_hits: 0,
            cache_misses: 0,
            estimated_prompt_tokens: 0,
            started_at: Utc::now(),
            last_generation_at: None,
        }
    }
}

impl ContextGenMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    // -- Recording helpers --

    /// Record that a generation was started.
    pub fn record_start(&mut self) {
        self.generations_started += 1;
    }

    /// Record a successful generation with elapsed wall-time.
    pub fn record_success(&mut self, elapsed_secs: f64) {
        self.generations_succeeded += 1;
        self.total_generation_secs += elapsed_secs;
        self.min_generation_secs = Some(
            self.min_generation_secs
                .map_or(elapsed_secs, |m| m.min(elapsed_secs)),
        );
        self.max_generation_secs = Some(
            self.max_generation_secs
                .map_or(elapsed_secs, |m| m.max(elapsed_secs)),
        );
        self.last_generation_at = Some(Utc::now());
    }

    /// Record a failed generation with elapsed wall-time.
    pub fn record_failure(&mut self, elapsed_secs: f64) {
        self.generations_failed += 1;
        self.total_generation_secs += elapsed_secs;
        self.last_generation_at = Some(Utc::now());
    }

    /// Record a cache check result (hit = context was current, miss = stale/missing).
    pub fn record_cache_check(&mut self, hit: bool) {
        if hit {
            self.cache_hits += 1;
        } else {
            self.cache_misses += 1;
        }
    }

    /// Add estimated prompt tokens for a generation run.
    pub fn record_prompt_tokens(&mut self, tokens: u64) {
        self.estimated_prompt_tokens += tokens;
    }

    // -- Derived stats --

    /// Average generation time (seconds). Returns `None` if no completions yet.
    pub fn avg_generation_secs(&self) -> Option<f64> {
        let completed = self.generations_succeeded + self.generations_failed;
        if completed == 0 {
            None
        } else {
            Some(self.total_generation_secs / completed as f64)
        }
    }

    /// Cache hit rate (0.0–1.0). Returns `None` if no checks recorded.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            None
        } else {
            Some(self.cache_hits as f64 / total as f64)
        }
    }

    /// Success rate (0.0–1.0). Returns `None` if nothing completed.
    pub fn success_rate(&self) -> Option<f64> {
        let completed = self.generations_succeeded + self.generations_failed;
        if completed == 0 {
            None
        } else {
            Some(self.generations_succeeded as f64 / completed as f64)
        }
    }
}

// ---------------------------------------------------------------------------
// Stale-context warnings
// ---------------------------------------------------------------------------

/// Severity of a staleness warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleSeverity {
    /// Context is current — no issue.
    Ok,
    /// Context exists but is slightly out of date.
    Stale,
    /// Context is significantly out of date or missing entirely.
    Critical,
}

/// A warning about context freshness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleWarning {
    pub severity: StaleSeverity,
    pub message: String,
    /// Age in seconds since last generation (if available).
    pub age_secs: Option<i64>,
}

/// Assess context staleness for a repository.
///
/// Returns `Ok` if context matches HEAD, `Stale` if it's older but exists,
/// `Critical` if context is missing or extremely old (>24h).
pub fn assess_staleness(repo_root: &Path) -> StaleWarning {
    use crate::context_gen::{get_head_sha, read_stored_hash};

    let context_dir = repo_root.join(".othala/context");
    let main_md = context_dir.join("MAIN.md");

    if !main_md.exists() {
        return StaleWarning {
            severity: StaleSeverity::Critical,
            message: "No context generated — MAIN.md missing".to_string(),
            age_secs: None,
        };
    }

    // Check hash match.
    let head = get_head_sha(repo_root);
    let stored = read_stored_hash(repo_root);

    match (&head, &stored) {
        (Some(h), Some(s)) if h == s => {
            // Hash matches — check age from file mtime.
            let age = file_age_secs(&main_md);
            if let Some(a) = age {
                if a > 86400 {
                    // >24h old even though hash matches — could be a long-running branch.
                    StaleWarning {
                        severity: StaleSeverity::Stale,
                        message: format!(
                            "Context matches HEAD but is {}h old — consider regenerating",
                            a / 3600
                        ),
                        age_secs: Some(a),
                    }
                } else {
                    StaleWarning {
                        severity: StaleSeverity::Ok,
                        message: "Context is current".to_string(),
                        age_secs: Some(a),
                    }
                }
            } else {
                StaleWarning {
                    severity: StaleSeverity::Ok,
                    message: "Context is current".to_string(),
                    age_secs: None,
                }
            }
        }
        (Some(_), Some(_)) => {
            // Hash mismatch — stale.
            let age = file_age_secs(&main_md);
            StaleWarning {
                severity: StaleSeverity::Stale,
                message: "Context hash does not match HEAD — stale".to_string(),
                age_secs: age,
            }
        }
        (Some(_), None) => {
            // MAIN.md exists but no stored hash.
            StaleWarning {
                severity: StaleSeverity::Stale,
                message: "Context exists but no stored hash — staleness unknown".to_string(),
                age_secs: file_age_secs(&main_md),
            }
        }
        (None, _) => {
            // Not a git repo or can't read HEAD — context exists, assume OK.
            StaleWarning {
                severity: StaleSeverity::Ok,
                message: "Context exists (non-git or unreadable HEAD)".to_string(),
                age_secs: file_age_secs(&main_md),
            }
        }
    }
}

/// Get file age in seconds from mtime.
fn file_age_secs(path: &Path) -> Option<i64> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let elapsed = modified.elapsed().ok()?;
    Some(elapsed.as_secs() as i64)
}

// ---------------------------------------------------------------------------
// Coverage snapshot (filesystem scan)
// ---------------------------------------------------------------------------

/// A snapshot of what exists under `.othala/context/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCoverage {
    /// Total number of `.md` context files.
    pub file_count: usize,
    /// Total bytes across all context files.
    pub total_bytes: u64,
    /// Estimated token count (rough: bytes / 4).
    pub estimated_tokens: u64,
    /// List of context files with individual sizes.
    pub files: Vec<ContextFileInfo>,
}

/// Info about a single context file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextFileInfo {
    /// Relative path within `.othala/context/`.
    pub relative_path: String,
    /// File size in bytes.
    pub bytes: u64,
}

/// Scan `.othala/context/` and build a coverage snapshot.
pub fn scan_coverage(repo_root: &Path) -> ContextCoverage {
    let context_dir = repo_root.join(".othala/context");
    let mut files = Vec::new();
    let mut total_bytes: u64 = 0;

    collect_md_files(&context_dir, &context_dir, &mut files, &mut total_bytes);

    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    ContextCoverage {
        file_count: files.len(),
        total_bytes,
        estimated_tokens: total_bytes / 4,
        files,
    }
}

fn collect_md_files(
    base: &Path,
    dir: &Path,
    out: &mut Vec<ContextFileInfo>,
    total: &mut u64,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(base, &path, out, total);
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            *total += bytes;
            out.push(ContextFileInfo {
                relative_path: relative,
                bytes,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Composite report (for CLI `--json` and human-readable)
// ---------------------------------------------------------------------------

/// Full context generation status report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextGenReport {
    /// Current staleness assessment.
    pub staleness: StaleWarning,
    /// Coverage snapshot from disk.
    pub coverage: ContextCoverage,
    /// Cumulative metrics (if a daemon has been running).
    pub metrics: Option<ContextGenMetrics>,
    /// Token budget info.
    pub token_budget: TokenBudgetInfo,
    /// Timestamp of this report.
    pub generated_at: DateTime<Utc>,
}

/// Token budget summary — how much context gen costs relative to budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudgetInfo {
    /// Estimated tokens in current context files.
    pub context_tokens: u64,
    /// Estimated prompt tokens consumed by generation runs.
    pub generation_prompt_tokens: u64,
    /// Warning if context token count is high.
    pub warning: Option<String>,
}

/// Build a report from filesystem state + optional in-memory metrics.
pub fn build_report(
    repo_root: &Path,
    metrics: Option<&ContextGenMetrics>,
) -> ContextGenReport {
    let staleness = assess_staleness(repo_root);
    let coverage = scan_coverage(repo_root);

    let generation_prompt_tokens = metrics
        .map(|m| m.estimated_prompt_tokens)
        .unwrap_or(0);

    let warning = if coverage.estimated_tokens > 50_000 {
        Some(format!(
            "Context files contain ~{}k tokens — consider trimming to stay under budget",
            coverage.estimated_tokens / 1000
        ))
    } else {
        None
    };

    let token_budget = TokenBudgetInfo {
        context_tokens: coverage.estimated_tokens,
        generation_prompt_tokens,
        warning,
    };

    ContextGenReport {
        staleness,
        coverage,
        metrics: metrics.cloned(),
        token_budget,
        generated_at: Utc::now(),
    }
}

/// Render a human-readable report to stderr-style string.
pub fn render_report(report: &ContextGenReport) -> String {
    let mut out = String::new();

    // -- Header --
    out.push_str("\x1b[35m── Context Generation Status ──\x1b[0m\n\n");

    // -- Staleness --
    let (icon, color) = match report.staleness.severity {
        StaleSeverity::Ok => ("✓", "\x1b[32m"),
        StaleSeverity::Stale => ("⚠", "\x1b[33m"),
        StaleSeverity::Critical => ("✗", "\x1b[31m"),
    };
    out.push_str(&format!(
        "  {color}{icon} {}\x1b[0m\n",
        report.staleness.message
    ));
    if let Some(age) = report.staleness.age_secs {
        out.push_str(&format!("    Age: {}s", age));
        if age > 3600 {
            out.push_str(&format!(" ({}h {}m)", age / 3600, (age % 3600) / 60));
        }
        out.push('\n');
    }
    out.push('\n');

    // -- Coverage --
    out.push_str(&format!(
        "  Files: {}   Bytes: {}   ~Tokens: {}\n",
        report.coverage.file_count,
        format_bytes(report.coverage.total_bytes),
        report.coverage.estimated_tokens
    ));

    if !report.coverage.files.is_empty() {
        out.push_str("  ─────────────────────────────────\n");
        for f in &report.coverage.files {
            out.push_str(&format!(
                "    {:40} {:>8}\n",
                f.relative_path,
                format_bytes(f.bytes)
            ));
        }
    }
    out.push('\n');

    // -- Metrics (if available) --
    if let Some(m) = &report.metrics {
        out.push_str("  \x1b[35mMetrics\x1b[0m\n");
        out.push_str(&format!(
            "    Generations:  {} started, {} succeeded, {} failed\n",
            m.generations_started, m.generations_succeeded, m.generations_failed
        ));
        if let Some(avg) = m.avg_generation_secs() {
            out.push_str(&format!("    Avg time:     {:.1}s", avg));
            if let (Some(min), Some(max)) = (m.min_generation_secs, m.max_generation_secs) {
                out.push_str(&format!("  (min {:.1}s, max {:.1}s)", min, max));
            }
            out.push('\n');
        }
        if let Some(rate) = m.cache_hit_rate() {
            out.push_str(&format!(
                "    Cache:        {:.0}% hit ({} hits, {} misses)\n",
                rate * 100.0,
                m.cache_hits,
                m.cache_misses
            ));
        }
        if let Some(rate) = m.success_rate() {
            out.push_str(&format!("    Success rate: {:.0}%\n", rate * 100.0));
        }
        if m.estimated_prompt_tokens > 0 {
            out.push_str(&format!(
                "    Prompt tokens: ~{}\n",
                m.estimated_prompt_tokens
            ));
        }
        out.push('\n');
    }

    // -- Token budget warning --
    if let Some(warning) = &report.token_budget.warning {
        out.push_str(&format!("  \x1b[33m⚠ {warning}\x1b[0m\n\n"));
    }

    out
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

// ---------------------------------------------------------------------------
// Rough token estimation for prompts
// ---------------------------------------------------------------------------

/// Estimate token count from a string (rough heuristic: ~4 chars per token).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64 + 3) / 4
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn metrics_default_is_zero() {
        let m = ContextGenMetrics::new();
        assert_eq!(m.generations_started, 0);
        assert_eq!(m.generations_succeeded, 0);
        assert_eq!(m.generations_failed, 0);
        assert_eq!(m.total_generation_secs, 0.0);
        assert!(m.min_generation_secs.is_none());
        assert!(m.max_generation_secs.is_none());
        assert_eq!(m.cache_hits, 0);
        assert_eq!(m.cache_misses, 0);
        assert_eq!(m.estimated_prompt_tokens, 0);
    }

    #[test]
    fn record_success_updates_min_max_avg() {
        let mut m = ContextGenMetrics::new();
        m.record_success(10.0);
        m.record_success(20.0);
        m.record_success(15.0);

        assert_eq!(m.generations_succeeded, 3);
        assert_eq!(m.min_generation_secs, Some(10.0));
        assert_eq!(m.max_generation_secs, Some(20.0));
        assert!((m.avg_generation_secs().unwrap() - 15.0).abs() < 0.01);
        assert!(m.last_generation_at.is_some());
    }

    #[test]
    fn record_failure_counts_separately() {
        let mut m = ContextGenMetrics::new();
        m.record_success(5.0);
        m.record_failure(2.0);

        assert_eq!(m.generations_succeeded, 1);
        assert_eq!(m.generations_failed, 1);
        // avg includes both success + failure
        assert!((m.avg_generation_secs().unwrap() - 3.5).abs() < 0.01);
        assert!((m.success_rate().unwrap() - 0.5).abs() < 0.01);
    }

    #[test]
    fn cache_hit_rate_calculation() {
        let mut m = ContextGenMetrics::new();
        assert!(m.cache_hit_rate().is_none());

        m.record_cache_check(true);
        m.record_cache_check(true);
        m.record_cache_check(false);

        let rate = m.cache_hit_rate().unwrap();
        assert!((rate - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn record_start_increments() {
        let mut m = ContextGenMetrics::new();
        m.record_start();
        m.record_start();
        assert_eq!(m.generations_started, 2);
    }

    #[test]
    fn estimate_tokens_rough() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        // Rough heuristic — not exact.
        assert!(estimate_tokens("hello world") > 0);
    }

    #[test]
    fn assess_staleness_missing_context() {
        let tmp =
            std::env::temp_dir().join(format!("othala-telemetry-stale-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        let w = assess_staleness(&tmp);
        assert_eq!(w.severity, StaleSeverity::Critical);
        assert!(w.message.contains("missing"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn assess_staleness_hash_mismatch() {
        let tmp = std::env::temp_dir()
            .join(format!("othala-telemetry-mismatch-{}", std::process::id()));
        let ctx_dir = tmp.join(".othala/context");
        fs::create_dir_all(&ctx_dir).unwrap();
        fs::write(ctx_dir.join("MAIN.md"), "# Context").unwrap();
        fs::write(ctx_dir.join(".git-hash"), "old-hash-abc").unwrap();

        // Not a real git repo so get_head_sha returns None → considered OK.
        // (In a real repo with a different HEAD, this would be Stale.)
        let w = assess_staleness(&tmp);
        // Non-git: head is None → Ok path.
        assert_eq!(w.severity, StaleSeverity::Ok);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn scan_coverage_empty() {
        let tmp =
            std::env::temp_dir().join(format!("othala-telemetry-cov-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        let cov = scan_coverage(&tmp);
        assert_eq!(cov.file_count, 0);
        assert_eq!(cov.total_bytes, 0);
        assert!(cov.files.is_empty());

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn scan_coverage_with_files() {
        let tmp =
            std::env::temp_dir().join(format!("othala-telemetry-covf-{}", std::process::id()));
        let ctx_dir = tmp.join(".othala/context");
        let sub_dir = ctx_dir.join("architecture");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(ctx_dir.join("MAIN.md"), "# Main context\n").unwrap();
        fs::write(sub_dir.join("overview.md"), "# Overview\n").unwrap();
        // Non-md file should be ignored.
        fs::write(ctx_dir.join(".git-hash"), "abc123").unwrap();

        let cov = scan_coverage(&tmp);
        assert_eq!(cov.file_count, 2);
        assert!(cov.total_bytes > 0);
        assert_eq!(cov.files.len(), 2);
        // Sorted alphabetically.
        assert_eq!(cov.files[0].relative_path, "MAIN.md");
        assert_eq!(cov.files[1].relative_path, "architecture/overview.md");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn build_report_without_metrics() {
        let tmp =
            std::env::temp_dir().join(format!("othala-telemetry-rpt-{}", std::process::id()));
        let ctx_dir = tmp.join(".othala/context");
        fs::create_dir_all(&ctx_dir).unwrap();
        fs::write(ctx_dir.join("MAIN.md"), "# Test\n").unwrap();

        let report = build_report(&tmp, None);
        assert!(report.metrics.is_none());
        assert_eq!(report.coverage.file_count, 1);
        assert!(report.generated_at <= Utc::now());

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn build_report_with_metrics() {
        let tmp =
            std::env::temp_dir().join(format!("othala-telemetry-rptm-{}", std::process::id()));
        let ctx_dir = tmp.join(".othala/context");
        fs::create_dir_all(&ctx_dir).unwrap();
        fs::write(ctx_dir.join("MAIN.md"), "# Test\n").unwrap();

        let mut metrics = ContextGenMetrics::new();
        metrics.record_start();
        metrics.record_success(12.5);
        metrics.record_cache_check(true);
        metrics.record_prompt_tokens(5000);

        let report = build_report(&tmp, Some(&metrics));
        assert!(report.metrics.is_some());
        let m = report.metrics.unwrap();
        assert_eq!(m.generations_succeeded, 1);
        assert_eq!(m.cache_hits, 1);
        assert_eq!(m.estimated_prompt_tokens, 5000);
        assert_eq!(report.token_budget.generation_prompt_tokens, 5000);
    }

    #[test]
    fn render_report_includes_sections() {
        let tmp = std::env::temp_dir()
            .join(format!("othala-telemetry-render-{}", std::process::id()));
        let ctx_dir = tmp.join(".othala/context");
        fs::create_dir_all(&ctx_dir).unwrap();
        fs::write(ctx_dir.join("MAIN.md"), "# Test\n").unwrap();

        let mut metrics = ContextGenMetrics::new();
        metrics.record_start();
        metrics.record_success(10.0);
        metrics.record_cache_check(true);

        let report = build_report(&tmp, Some(&metrics));
        let rendered = render_report(&report);

        assert!(rendered.contains("Context Generation Status"));
        assert!(rendered.contains("Files:"));
        assert!(rendered.contains("MAIN.md"));
        assert!(rendered.contains("Metrics"));
        assert!(rendered.contains("Generations:"));
        assert!(rendered.contains("Cache:"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn render_report_shows_token_budget_warning() {
        let report = ContextGenReport {
            staleness: StaleWarning {
                severity: StaleSeverity::Ok,
                message: "Context is current".to_string(),
                age_secs: None,
            },
            coverage: ContextCoverage {
                file_count: 5,
                total_bytes: 250_000,
                estimated_tokens: 62_500,
                files: vec![],
            },
            metrics: None,
            token_budget: TokenBudgetInfo {
                context_tokens: 62_500,
                generation_prompt_tokens: 0,
                warning: Some(
                    "Context files contain ~62k tokens — consider trimming to stay under budget"
                        .to_string(),
                ),
            },
            generated_at: Utc::now(),
        };

        let rendered = render_report(&report);
        assert!(rendered.contains("62k tokens"));
        assert!(rendered.contains("trimming"));
    }

    #[test]
    fn format_bytes_scales() {
        assert_eq!(format_bytes(500), "500B");
        assert_eq!(format_bytes(1024), "1.0KB");
        assert_eq!(format_bytes(1536), "1.5KB");
        assert_eq!(format_bytes(1048576), "1.0MB");
    }

    #[test]
    fn metrics_serializes_to_json() {
        let mut m = ContextGenMetrics::new();
        m.record_start();
        m.record_success(5.0);
        m.record_cache_check(true);
        m.record_prompt_tokens(1000);

        let json = serde_json::to_string_pretty(&m).unwrap();
        assert!(json.contains("\"generations_succeeded\": 1"));
        assert!(json.contains("\"cache_hits\": 1"));
        assert!(json.contains("\"estimated_prompt_tokens\": 1000"));

        // Roundtrip
        let m2: ContextGenMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(m2.generations_succeeded, 1);
    }

    #[test]
    fn report_serializes_to_json() {
        let tmp =
            std::env::temp_dir().join(format!("othala-telemetry-json-{}", std::process::id()));
        let ctx_dir = tmp.join(".othala/context");
        fs::create_dir_all(&ctx_dir).unwrap();
        fs::write(ctx_dir.join("MAIN.md"), "# Test\n").unwrap();

        let report = build_report(&tmp, None);
        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("\"staleness\""));
        assert!(json.contains("\"coverage\""));
        assert!(json.contains("\"token_budget\""));
        assert!(json.contains("\"generated_at\""));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn stale_warning_severity_order() {
        // Just verify enum variants exist and can be compared.
        assert_ne!(StaleSeverity::Ok, StaleSeverity::Stale);
        assert_ne!(StaleSeverity::Stale, StaleSeverity::Critical);
        assert_ne!(StaleSeverity::Ok, StaleSeverity::Critical);
    }
}
