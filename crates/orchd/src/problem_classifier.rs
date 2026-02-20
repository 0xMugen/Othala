//! Problem Classifier — classifies errors to route to optimal recovery agent.
//!
//! Error classes:
//! - Compile: syntax errors, type errors, missing imports → Hephaestus
//! - Config: environment, config files, missing deps → Explorer
//! - Permission: auth, access, credentials → Human escalation
//! - Logic: test failures, wrong behavior → Sisyphus
//! - Network: timeout, connection issues → Retry
//! - Resource: disk, memory, rate limits → Wait & retry

use crate::agent_dispatch::AgentRole;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────────────────────
// Error Class — The type of problem
// ─────────────────────────────────────────────────────────────────────────────

/// Classification of an error for routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Compilation errors: syntax, types, imports, lifetimes
    Compile,
    /// Configuration errors: env vars, config files, dependencies
    Config,
    /// Environment errors: missing tools, wrong versions
    Environment,
    /// Permission errors: auth failures, access denied
    Permission,
    /// Logic errors: test failures, wrong behavior, assertions
    Logic,
    /// Network errors: timeouts, connection refused
    Network,
    /// Resource errors: disk full, OOM, rate limits
    Resource,
    /// Git/VCS errors: conflicts, diverged branches
    Git,
    /// Agent errors: tool failures, format errors
    Agent,
    /// Unknown/unclassified
    Unknown,
}

impl ErrorClass {
    /// Whether this error class requires human intervention.
    pub fn requires_human(&self) -> bool {
        matches!(self, ErrorClass::Permission)
    }

    /// Whether this error class is transient (retry may succeed).
    pub fn is_transient(&self) -> bool {
        matches!(self, ErrorClass::Network | ErrorClass::Resource)
    }

    /// Whether this error class can be fixed by an agent.
    pub fn is_agent_fixable(&self) -> bool {
        matches!(
            self,
            ErrorClass::Compile
                | ErrorClass::Config
                | ErrorClass::Environment
                | ErrorClass::Logic
                | ErrorClass::Git
                | ErrorClass::Agent
        )
    }

    /// Get the recommended agent for fixing this error class.
    pub fn recommended_agent(&self) -> Option<AgentRole> {
        match self {
            ErrorClass::Compile => Some(AgentRole::Hephaestus),
            ErrorClass::Config => Some(AgentRole::Explorer),
            ErrorClass::Environment => Some(AgentRole::Explorer),
            ErrorClass::Logic => Some(AgentRole::Sisyphus),
            ErrorClass::Git => Some(AgentRole::Explorer),
            ErrorClass::Agent => Some(AgentRole::Sisyphus),
            ErrorClass::Permission => None, // Human required
            ErrorClass::Network => None,    // Retry
            ErrorClass::Resource => None,   // Wait
            ErrorClass::Unknown => Some(AgentRole::Sisyphus),
        }
    }

    /// Get the recommended retry delay for transient errors.
    pub fn retry_delay_secs(&self) -> Option<u64> {
        match self {
            ErrorClass::Network => Some(30),
            ErrorClass::Resource => Some(120),
            _ => None,
        }
    }
}

impl std::fmt::Display for ErrorClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            ErrorClass::Compile => "compile",
            ErrorClass::Config => "config",
            ErrorClass::Environment => "environment",
            ErrorClass::Permission => "permission",
            ErrorClass::Logic => "logic",
            ErrorClass::Network => "network",
            ErrorClass::Resource => "resource",
            ErrorClass::Git => "git",
            ErrorClass::Agent => "agent",
            ErrorClass::Unknown => "unknown",
        };
        f.write_str(name)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Error Pattern — Regex-like pattern matching
// ─────────────────────────────────────────────────────────────────────────────

/// A pattern for matching error messages.
#[derive(Debug, Clone)]
struct ErrorPattern {
    keywords: Vec<&'static str>,
    class: ErrorClass,
    priority: u8, // Higher = more specific
}

/// Build the pattern database.
fn build_patterns() -> Vec<ErrorPattern> {
    vec![
        // ─────────────────────────────────────────────────────────────────────
        // Compile errors (Rust-specific)
        // ─────────────────────────────────────────────────────────────────────
        ErrorPattern {
            keywords: vec!["error[E", "cannot find type", "expected", "mismatched types"],
            class: ErrorClass::Compile,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["unresolved import", "use of undeclared", "not found in scope"],
            class: ErrorClass::Compile,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["lifetime", "borrowed", "'static", "does not live long enough"],
            class: ErrorClass::Compile,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["syntax error", "unexpected token", "parse error"],
            class: ErrorClass::Compile,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["cargo build", "cargo check", "rustc"],
            class: ErrorClass::Compile,
            priority: 5,
        },
        // ─────────────────────────────────────────────────────────────────────
        // Compile errors (TypeScript-specific)
        // ─────────────────────────────────────────────────────────────────────
        ErrorPattern {
            keywords: vec!["TS2", "Type '", "is not assignable", "Property '"],
            class: ErrorClass::Compile,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["tsc", "typescript", "Cannot find module"],
            class: ErrorClass::Compile,
            priority: 8,
        },
        // ─────────────────────────────────────────────────────────────────────
        // Config errors
        // ─────────────────────────────────────────────────────────────────────
        ErrorPattern {
            keywords: vec!["config", "configuration", ".toml", ".json", ".yaml", ".env"],
            class: ErrorClass::Config,
            priority: 6,
        },
        ErrorPattern {
            keywords: vec!["missing key", "invalid value", "required field"],
            class: ErrorClass::Config,
            priority: 8,
        },
        ErrorPattern {
            keywords: vec!["environment variable", "env var", "DATABASE_URL"],
            class: ErrorClass::Config,
            priority: 9,
        },
        // ─────────────────────────────────────────────────────────────────────
        // Environment errors
        // ─────────────────────────────────────────────────────────────────────
        ErrorPattern {
            keywords: vec!["command not found", "not installed", "missing tool"],
            class: ErrorClass::Environment,
            priority: 9,
        },
        ErrorPattern {
            keywords: vec!["version mismatch", "incompatible version", "requires"],
            class: ErrorClass::Environment,
            priority: 8,
        },
        ErrorPattern {
            keywords: vec!["nix", "flake", "devshell"],
            class: ErrorClass::Environment,
            priority: 6,
        },
        // ─────────────────────────────────────────────────────────────────────
        // Permission errors
        // ─────────────────────────────────────────────────────────────────────
        ErrorPattern {
            keywords: vec!["permission denied", "access denied", "forbidden"],
            class: ErrorClass::Permission,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["authentication failed", "invalid credentials", "unauthorized"],
            class: ErrorClass::Permission,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["token expired", "token invalid", "not authenticated"],
            class: ErrorClass::Permission,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["gt auth", "gh auth", "API key"],
            class: ErrorClass::Permission,
            priority: 9,
        },
        // ─────────────────────────────────────────────────────────────────────
        // Logic errors (test failures)
        // ─────────────────────────────────────────────────────────────────────
        ErrorPattern {
            keywords: vec!["test failed", "assertion failed", "FAILED"],
            class: ErrorClass::Logic,
            priority: 9,
        },
        ErrorPattern {
            keywords: vec!["expected", "actual", "assert_eq!", "assert!"],
            class: ErrorClass::Logic,
            priority: 8,
        },
        ErrorPattern {
            keywords: vec!["panicked at", "thread 'main' panicked"],
            class: ErrorClass::Logic,
            priority: 9,
        },
        ErrorPattern {
            keywords: vec!["wrong result", "incorrect", "mismatch"],
            class: ErrorClass::Logic,
            priority: 7,
        },
        // ─────────────────────────────────────────────────────────────────────
        // Network errors
        // ─────────────────────────────────────────────────────────────────────
        ErrorPattern {
            keywords: vec!["timeout", "timed out", "connection refused"],
            class: ErrorClass::Network,
            priority: 9,
        },
        ErrorPattern {
            keywords: vec!["network error", "connection reset", "ECONNRESET"],
            class: ErrorClass::Network,
            priority: 9,
        },
        ErrorPattern {
            keywords: vec!["DNS", "could not resolve", "name resolution"],
            class: ErrorClass::Network,
            priority: 9,
        },
        // ─────────────────────────────────────────────────────────────────────
        // Resource errors
        // ─────────────────────────────────────────────────────────────────────
        ErrorPattern {
            keywords: vec!["out of memory", "OOM", "memory allocation failed"],
            class: ErrorClass::Resource,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["disk full", "no space left", "ENOSPC"],
            class: ErrorClass::Resource,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["rate limit", "too many requests", "429"],
            class: ErrorClass::Resource,
            priority: 9,
        },
        ErrorPattern {
            keywords: vec!["quota exceeded", "limit exceeded"],
            class: ErrorClass::Resource,
            priority: 9,
        },
        // ─────────────────────────────────────────────────────────────────────
        // Git errors
        // ─────────────────────────────────────────────────────────────────────
        ErrorPattern {
            keywords: vec!["merge conflict", "CONFLICT", "conflict in"],
            class: ErrorClass::Git,
            priority: 10,
        },
        ErrorPattern {
            keywords: vec!["rebase", "restack", "diverged"],
            class: ErrorClass::Git,
            priority: 8,
        },
        ErrorPattern {
            keywords: vec!["not a git repository", "git checkout", "detached HEAD"],
            class: ErrorClass::Git,
            priority: 7,
        },
        ErrorPattern {
            keywords: vec!["push rejected", "pull failed", "fetch failed"],
            class: ErrorClass::Git,
            priority: 8,
        },
        // ─────────────────────────────────────────────────────────────────────
        // Agent errors
        // ─────────────────────────────────────────────────────────────────────
        ErrorPattern {
            keywords: vec!["[need_human]", "[patch_ready]", "agent"],
            class: ErrorClass::Agent,
            priority: 6,
        },
        ErrorPattern {
            keywords: vec!["tool failed", "command failed", "subprocess"],
            class: ErrorClass::Agent,
            priority: 5,
        },
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// Classification Result
// ─────────────────────────────────────────────────────────────────────────────

/// Result of error classification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassificationResult {
    /// Primary error class
    pub class: ErrorClass,
    /// Confidence (0.0 - 1.0)
    pub confidence: f32,
    /// Matched keywords
    pub matched_keywords: Vec<String>,
    /// Recommended action
    pub action: RecoveryAction,
    /// Recommended agent (if applicable)
    pub recommended_agent: Option<AgentRole>,
    /// Additional context for the agent
    pub context: String,
}

/// Recommended recovery action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Retry with the same agent
    Retry,
    /// Retry with a different agent
    RetryWithAgent,
    /// Wait before retrying (transient error)
    WaitAndRetry,
    /// Escalate to human
    EscalateHuman,
    /// Stop task (unrecoverable)
    Stop,
}

// ─────────────────────────────────────────────────────────────────────────────
// Problem Classifier
// ─────────────────────────────────────────────────────────────────────────────

/// The Problem Classifier — analyzes errors and recommends recovery.
#[derive(Debug)]
pub struct ProblemClassifier {
    patterns: Vec<ErrorPattern>,
    /// Recent classifications for pattern learning
    recent: Vec<(String, ErrorClass)>,
}

impl ProblemClassifier {
    pub fn new() -> Self {
        Self {
            patterns: build_patterns(),
            recent: Vec::new(),
        }
    }

    /// Classify an error message.
    pub fn classify(&mut self, error: &str) -> ClassificationResult {
        let lower = error.to_ascii_lowercase();
        let mut matches: HashMap<ErrorClass, (u8, Vec<String>)> = HashMap::new();

        // Match against patterns
        for pattern in &self.patterns {
            let matched: Vec<String> = pattern
                .keywords
                .iter()
                .filter(|kw| lower.contains(&kw.to_ascii_lowercase()))
                .map(|s| s.to_string())
                .collect();

            if !matched.is_empty() {
                let entry = matches.entry(pattern.class).or_insert((0, Vec::new()));
                entry.0 = entry.0.max(pattern.priority);
                entry.1.extend(matched);
            }
        }

        // Find best match
        let (class, priority, keywords) = matches
            .into_iter()
            .max_by_key(|(_, (p, kws))| (*p, kws.len()))
            .map(|(c, (p, kws))| (c, p, kws))
            .unwrap_or((ErrorClass::Unknown, 0, vec![]));

        // Calculate confidence
        let confidence = if class == ErrorClass::Unknown {
            0.3
        } else {
            0.5 + (priority as f32 / 20.0) + (keywords.len() as f32 / 10.0).min(0.3)
        };

        // Determine action
        let action = if class.requires_human() {
            RecoveryAction::EscalateHuman
        } else if class.is_transient() {
            RecoveryAction::WaitAndRetry
        } else if class.is_agent_fixable() {
            RecoveryAction::RetryWithAgent
        } else {
            RecoveryAction::Retry
        };

        // Build context for the agent
        let context = self.build_recovery_context(class, &keywords, error);

        // Store for learning
        self.recent.push((error.to_string(), class));
        if self.recent.len() > 100 {
            self.recent.remove(0);
        }

        ClassificationResult {
            class,
            confidence: confidence.min(1.0),
            matched_keywords: keywords,
            action,
            recommended_agent: class.recommended_agent(),
            context,
        }
    }

    /// Build context for the recovery agent.
    fn build_recovery_context(
        &self,
        class: ErrorClass,
        keywords: &[String],
        error: &str,
    ) -> String {
        let mut ctx = String::new();

        ctx.push_str(&format!("## Error Analysis\n\n"));
        ctx.push_str(&format!("**Error Class:** {}\n", class));
        ctx.push_str(&format!(
            "**Matched Patterns:** {}\n\n",
            keywords.join(", ")
        ));

        match class {
            ErrorClass::Compile => {
                ctx.push_str("### Recovery Strategy\n");
                ctx.push_str("1. Locate the exact file and line from the error\n");
                ctx.push_str("2. Read the surrounding context\n");
                ctx.push_str("3. Fix the type/syntax/import issue\n");
                ctx.push_str("4. Run verify command to confirm fix\n");
            }
            ErrorClass::Logic => {
                ctx.push_str("### Recovery Strategy\n");
                ctx.push_str("1. Identify the failing test or assertion\n");
                ctx.push_str("2. Understand what behavior is expected vs actual\n");
                ctx.push_str("3. Trace the code path to find the bug\n");
                ctx.push_str("4. Fix the logic issue\n");
                ctx.push_str("5. Re-run tests to verify\n");
            }
            ErrorClass::Config => {
                ctx.push_str("### Recovery Strategy\n");
                ctx.push_str("1. Identify the missing or incorrect config\n");
                ctx.push_str("2. Check environment variables and config files\n");
                ctx.push_str("3. Update configuration as needed\n");
            }
            ErrorClass::Git => {
                ctx.push_str("### Recovery Strategy\n");
                ctx.push_str("1. Check git status and branch state\n");
                ctx.push_str("2. Resolve any conflicts manually\n");
                ctx.push_str("3. Use `gt abort` if restack failed, then retry\n");
            }
            _ => {}
        }

        // Extract relevant error snippet
        let lines: Vec<&str> = error.lines().take(20).collect();
        if !lines.is_empty() {
            ctx.push_str("\n### Error Excerpt\n```\n");
            ctx.push_str(&lines.join("\n"));
            ctx.push_str("\n```\n");
        }

        ctx
    }

    /// Get recent error class distribution.
    pub fn get_recent_distribution(&self) -> HashMap<ErrorClass, usize> {
        let mut dist = HashMap::new();
        for (_, class) in &self.recent {
            *dist.entry(*class).or_insert(0) += 1;
        }
        dist
    }

    /// Check if there's a pattern of repeated errors.
    pub fn detect_repeated_pattern(&self, window: usize) -> Option<ErrorClass> {
        if self.recent.len() < window {
            return None;
        }

        let recent_window = &self.recent[self.recent.len() - window..];
        let first_class = recent_window.first()?.1;

        if recent_window.iter().all(|(_, c)| *c == first_class) {
            Some(first_class)
        } else {
            None
        }
    }
}

impl Default for ProblemClassifier {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_compile_error() {
        let mut classifier = ProblemClassifier::new();
        let result = classifier.classify(
            "error[E0308]: mismatched types\n  --> src/lib.rs:10:5\nexpected i32, found String",
        );
        assert_eq!(result.class, ErrorClass::Compile);
        assert!(result.confidence > 0.7);
        assert_eq!(result.recommended_agent, Some(AgentRole::Hephaestus));
    }

    #[test]
    fn classify_test_failure() {
        let mut classifier = ProblemClassifier::new();
        let result = classifier.classify(
            "test result: FAILED. 1 passed; 1 failed\n\nthread 'test_foo' panicked at assertion failed",
        );
        assert_eq!(result.class, ErrorClass::Logic);
        assert_eq!(result.recommended_agent, Some(AgentRole::Sisyphus));
    }

    #[test]
    fn classify_permission_error() {
        let mut classifier = ProblemClassifier::new();
        let result = classifier.classify("authentication failed: token expired, please run gt auth");
        assert_eq!(result.class, ErrorClass::Permission);
        assert!(result.class.requires_human());
        assert_eq!(result.action, RecoveryAction::EscalateHuman);
    }

    #[test]
    fn classify_network_error() {
        let mut classifier = ProblemClassifier::new();
        let result = classifier.classify("connection refused: timeout after 30s");
        assert_eq!(result.class, ErrorClass::Network);
        assert!(result.class.is_transient());
        assert_eq!(result.action, RecoveryAction::WaitAndRetry);
    }

    #[test]
    fn classify_git_conflict() {
        let mut classifier = ProblemClassifier::new();
        let result = classifier.classify("CONFLICT (content): Merge conflict in src/main.rs");
        assert_eq!(result.class, ErrorClass::Git);
        assert_eq!(result.recommended_agent, Some(AgentRole::Explorer));
    }

    #[test]
    fn detect_repeated_compile_errors() {
        let mut classifier = ProblemClassifier::new();

        for _ in 0..5 {
            classifier.classify("error[E0308]: mismatched types");
        }

        assert_eq!(
            classifier.detect_repeated_pattern(3),
            Some(ErrorClass::Compile)
        );
    }

    #[test]
    fn error_class_properties() {
        assert!(ErrorClass::Permission.requires_human());
        assert!(!ErrorClass::Compile.requires_human());

        assert!(ErrorClass::Network.is_transient());
        assert!(ErrorClass::Resource.is_transient());
        assert!(!ErrorClass::Compile.is_transient());

        assert!(ErrorClass::Compile.is_agent_fixable());
        assert!(ErrorClass::Logic.is_agent_fixable());
        assert!(!ErrorClass::Permission.is_agent_fixable());
    }

    #[test]
    fn retry_delays() {
        assert_eq!(ErrorClass::Network.retry_delay_secs(), Some(30));
        assert_eq!(ErrorClass::Resource.retry_delay_secs(), Some(120));
        assert_eq!(ErrorClass::Compile.retry_delay_secs(), None);
    }
}
