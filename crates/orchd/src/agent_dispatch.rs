//! Agent Dispatch Router — routes tasks to optimal agent based on task type and context.
//!
//! This is the key differentiator: Othala uses a *team* of agents, not one.
//! - hephaestus (Codex): Code generation, implementation
//! - sisyphus (Claude Opus): Deep reasoning, complex problems, error recovery
//! - librarian (Claude Sonnet): Documentation, clarity, code review
//! - explore (Claude Haiku): Quick exploration, simple fixes

use orch_core::types::{ModelKind, Task, TaskType};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ─────────────────────────────────────────────────────────────────────────────
// Agent Role — The Specific Agent Persona
// ─────────────────────────────────────────────────────────────────────────────

/// Extended agent roles beyond raw model kinds.
/// Each role maps to a model but carries persona/prompt differences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Hephaestus (Codex) — The Forge. Code generation, implementation.
    Hephaestus,
    /// Sisyphus (Claude Opus) — Deep thinker. Complex problems, error recovery.
    Sisyphus,
    /// Librarian (Claude Sonnet) — Documentation, code review, clarity.
    Librarian,
    /// Explorer (Claude Haiku) — Quick exploration, simple fixes.
    Explorer,
    /// Oracle (GPT-5.2) — High-level reasoning, architecture decisions.
    Oracle,
    /// Multimodal (Gemini) — Visual/multimodal tasks.
    Multimodal,
}

impl AgentRole {
    /// Get the underlying model for this role.
    pub fn model(&self) -> ModelKind {
        match self {
            AgentRole::Hephaestus => ModelKind::Codex,
            AgentRole::Sisyphus => ModelKind::Claude, // Opus
            AgentRole::Librarian => ModelKind::Claude, // Sonnet
            AgentRole::Explorer => ModelKind::Claude,  // Haiku
            AgentRole::Oracle => ModelKind::Codex,     // GPT-5.2 via Codex adapter
            AgentRole::Multimodal => ModelKind::Gemini,
        }
    }

    /// Get the display name for this role.
    pub fn name(&self) -> &'static str {
        match self {
            AgentRole::Hephaestus => "hephaestus",
            AgentRole::Sisyphus => "sisyphus",
            AgentRole::Librarian => "librarian",
            AgentRole::Explorer => "explorer",
            AgentRole::Oracle => "oracle",
            AgentRole::Multimodal => "multimodal",
        }
    }

    /// Get the agent's persona description for prompt injection.
    pub fn persona(&self) -> &'static str {
        match self {
            AgentRole::Hephaestus => "You are Hephaestus, the divine craftsman. Your specialty is forging code with precision and efficiency. Focus on implementation, write clean code, and signal [patch_ready] when done.",
            AgentRole::Sisyphus => "You are Sisyphus, the eternal problem solver. You tackle complex challenges with persistence and deep analysis. When facing errors, analyze root causes before proposing solutions.",
            AgentRole::Librarian => "You are the Librarian, keeper of documentation and clarity. Your role is to review code for correctness, improve documentation, and ensure maintainability.",
            AgentRole::Explorer => "You are the Explorer, quick and nimble. Handle simple fixes, explore codebases rapidly, and identify issues without overthinking.",
            AgentRole::Oracle => "You are the Oracle, providing high-level architectural guidance. Focus on design decisions, system architecture, and strategic planning.",
            AgentRole::Multimodal => "You are capable of visual reasoning. Analyze images, diagrams, and visual content alongside code.",
        }
    }

    /// Get extra CLI args for this role (e.g., model selection).
    pub fn extra_args(&self) -> Vec<String> {
        match self {
            AgentRole::Sisyphus => vec!["--model".to_string(), "opus".to_string()],
            AgentRole::Librarian => vec!["--model".to_string(), "sonnet".to_string()],
            AgentRole::Explorer => vec!["--model".to_string(), "haiku".to_string()],
            _ => vec![],
        }
    }
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Task Intent — What the task is trying to accomplish
// ─────────────────────────────────────────────────────────────────────────────

/// Classified intent of a task, used for routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskIntent {
    /// Pure code implementation
    Implementation,
    /// Bug fix
    BugFix,
    /// Refactoring existing code
    Refactor,
    /// Writing or improving documentation
    Documentation,
    /// Code review
    Review,
    /// Test writing
    Testing,
    /// Error diagnosis and recovery
    ErrorRecovery,
    /// Architecture/design decisions
    Architecture,
    /// Simple/quick changes
    QuickFix,
    /// Visual/multimodal analysis
    Visual,
    /// Unknown/unclassified
    Unknown,
}

impl TaskIntent {
    /// Classify task intent from title and context.
    pub fn classify(title: &str, task_type: TaskType, is_retry: bool) -> Self {
        let lower = title.to_ascii_lowercase();

        // Error recovery takes precedence for retries
        if is_retry {
            return TaskIntent::ErrorRecovery;
        }

        // Check for explicit task types
        match task_type {
            TaskType::TestSpecWrite | TaskType::TestValidate => return TaskIntent::Testing,
            TaskType::Orchestrate => return TaskIntent::Architecture,
            TaskType::Implement => {}
        }

        // Documentation signals
        if lower.contains("document")
            || lower.contains("readme")
            || lower.contains("comment")
            || lower.contains("docs")
        {
            return TaskIntent::Documentation;
        }

        // Review signals
        if lower.contains("review") || lower.contains("audit") || lower.contains("check") {
            return TaskIntent::Review;
        }

        // Quick fix signals (check before bug fix since "fix" is common)
        if lower.contains("typo")
            || lower.contains("simple")
            || lower.contains("minor")
            || lower.contains("small")
        {
            return TaskIntent::QuickFix;
        }

        // Bug fix signals
        if lower.contains("bug")
            || lower.contains("issue")
            || lower.contains("error")
            || lower.contains("broken")
            || lower.contains("fix")
        {
            return TaskIntent::BugFix;
        }

        // Refactor signals
        if lower.contains("refactor")
            || lower.contains("clean")
            || lower.contains("restructure")
            || lower.contains("reorganize")
        {
            return TaskIntent::Refactor;
        }

        // Testing signals
        if lower.contains("test") || lower.contains("spec") || lower.contains("coverage") {
            return TaskIntent::Testing;
        }

        // Architecture signals
        if lower.contains("architect")
            || lower.contains("design")
            || lower.contains("structure")
            || lower.contains("plan")
        {
            return TaskIntent::Architecture;
        }

        // Rename is also a quick fix
        if lower.contains("rename") {
            return TaskIntent::QuickFix;
        }

        // Visual signals
        if lower.contains("image")
            || lower.contains("diagram")
            || lower.contains("visual")
            || lower.contains("screenshot")
        {
            return TaskIntent::Visual;
        }

        // Default to implementation
        TaskIntent::Implementation
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Repo Context — Information about the repository
// ─────────────────────────────────────────────────────────────────────────────

/// Context about the repository for routing decisions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoContext {
    /// Primary language of the repo
    pub primary_language: Option<String>,
    /// Whether the repo uses Rust
    pub is_rust: bool,
    /// Whether the repo uses TypeScript/JavaScript
    pub is_typescript: bool,
    /// Whether the repo uses Python
    pub is_python: bool,
    /// Whether the repo has a flake.nix (Nix-based)
    pub is_nix: bool,
    /// Crate count (for Rust workspaces)
    pub crate_count: usize,
    /// Recent error patterns
    pub recent_errors: Vec<String>,
    /// Success rate of recent tasks
    pub recent_success_rate: f64,
}

impl RepoContext {
    /// Load repo context from filesystem.
    pub fn load(repo_root: &Path) -> Self {
        let mut ctx = RepoContext::default();

        // Check for Rust
        ctx.is_rust = repo_root.join("Cargo.toml").exists();
        if ctx.is_rust {
            ctx.primary_language = Some("rust".to_string());
            // Count crates
            if let Ok(contents) = std::fs::read_to_string(repo_root.join("Cargo.toml")) {
                ctx.crate_count = contents.matches("[workspace]").count().max(1);
            }
        }

        // Check for TypeScript
        ctx.is_typescript = repo_root.join("package.json").exists()
            || repo_root.join("tsconfig.json").exists();
        if ctx.is_typescript && ctx.primary_language.is_none() {
            ctx.primary_language = Some("typescript".to_string());
        }

        // Check for Python
        ctx.is_python = repo_root.join("pyproject.toml").exists()
            || repo_root.join("setup.py").exists()
            || repo_root.join("requirements.txt").exists();
        if ctx.is_python && ctx.primary_language.is_none() {
            ctx.primary_language = Some("python".to_string());
        }

        // Check for Nix
        ctx.is_nix = repo_root.join("flake.nix").exists();

        ctx
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Dispatch Decision — The routing result
// ─────────────────────────────────────────────────────────────────────────────

/// Result of agent dispatch decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchDecision {
    /// Primary agent role to use
    pub role: AgentRole,
    /// Confidence in this decision (0.0 - 1.0)
    pub confidence: f32,
    /// Reasoning for the decision
    pub reasoning: String,
    /// Fallback role if primary fails
    pub fallback: Option<AgentRole>,
    /// Additional context to inject into prompt
    pub context_additions: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Agent Dispatcher — The Routing Engine
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the agent dispatcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchConfig {
    /// Whether to use Sisyphus for error recovery
    pub sisyphus_error_recovery: bool,
    /// Whether to use Hephaestus for code generation
    pub hephaestus_code_gen: bool,
    /// Whether to use Explorer for quick fixes
    pub explorer_quick_fixes: bool,
    /// Minimum complexity threshold for Sisyphus (0.0 - 1.0)
    pub sisyphus_complexity_threshold: f32,
    /// Model overrides per repo
    pub repo_overrides: HashMap<String, AgentRole>,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            sisyphus_error_recovery: true,
            hephaestus_code_gen: true,
            explorer_quick_fixes: true,
            sisyphus_complexity_threshold: 0.7,
            repo_overrides: HashMap::new(),
        }
    }
}

/// The Agent Dispatcher — routes tasks to optimal agents.
#[derive(Debug, Clone)]
pub struct AgentDispatcher {
    pub config: DispatchConfig,
}

impl AgentDispatcher {
    pub fn new(config: DispatchConfig) -> Self {
        Self { config }
    }

    /// **GRACEFUL FALLBACK:** Always succeeds with safe defaults.
    /// If dispatch routing fails for any reason, returns a sensible fallback.
    pub fn dispatch_with_fallback(
        &self,
        task: &Task,
        repo_context: &RepoContext,
        is_retry: bool,
        failure_reason: Option<&str>,
    ) -> DispatchDecision {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.dispatch(task, repo_context, is_retry, failure_reason)
        })) {
            Ok(decision) => decision,
            Err(_) => {
                // If dispatch crashes, degrade gracefully to Claude
                eprintln!(
                    "[dispatch] WARNING: dispatch router panicked for task {}. Using fallback agent.",
                    task.id
                );
                DispatchDecision {
                    role: AgentRole::Sisyphus,
                    confidence: 0.5,
                    reasoning: "Dispatch router failed; using safe fallback (Sisyphus/Claude)".to_string(),
                    fallback: Some(AgentRole::Explorer),
                    context_additions: vec![
                        "NOTE: Agent dispatch router encountered an error. Using fallback routing.".to_string(),
                    ],
                }
            }
        }
    }

    /// Dispatch a task to the optimal agent.
    pub fn dispatch(
        &self,
        task: &Task,
        repo_context: &RepoContext,
        is_retry: bool,
        failure_reason: Option<&str>,
    ) -> DispatchDecision {
        // Check for repo overrides
        if let Some(role) = self.config.repo_overrides.get(&task.repo_id.0) {
            return DispatchDecision {
                role: *role,
                confidence: 1.0,
                reasoning: format!("Repo override: {} → {}", task.repo_id.0, role),
                fallback: None,
                context_additions: vec![],
            };
        }

        // Classify task intent
        let intent = TaskIntent::classify(&task.title, task.task_type, is_retry);

        // Route based on intent
        let (role, confidence, reasoning, fallback) = match intent {
            TaskIntent::ErrorRecovery => {
                if self.config.sisyphus_error_recovery {
                    let mut context = vec![
                        "This is a retry attempt. Analyze the previous failure carefully.".to_string(),
                    ];
                    if let Some(reason) = failure_reason {
                        context.push(format!("Previous failure reason: {}", reason));
                    }
                    return DispatchDecision {
                        role: AgentRole::Sisyphus,
                        confidence: 0.95,
                        reasoning: "Error recovery requires deep analysis (Sisyphus)".to_string(),
                        fallback: Some(AgentRole::Hephaestus),
                        context_additions: context,
                    };
                }
                (AgentRole::Hephaestus, 0.6, "Default error recovery", None)
            }

            TaskIntent::Implementation => {
                if self.config.hephaestus_code_gen {
                    (
                        AgentRole::Hephaestus,
                        0.9,
                        "Code implementation → Hephaestus (Codex)",
                        Some(AgentRole::Sisyphus),
                    )
                } else {
                    (AgentRole::Sisyphus, 0.7, "Fallback implementation", None)
                }
            }

            TaskIntent::BugFix => {
                // Bug fixes might need Sisyphus for complex issues
                let complexity = estimate_complexity(&task.title, repo_context);
                if complexity > self.config.sisyphus_complexity_threshold {
                    (
                        AgentRole::Sisyphus,
                        0.85,
                        "Complex bug fix → Sisyphus",
                        Some(AgentRole::Hephaestus),
                    )
                } else {
                    (
                        AgentRole::Hephaestus,
                        0.8,
                        "Standard bug fix → Hephaestus",
                        Some(AgentRole::Explorer),
                    )
                }
            }

            TaskIntent::Refactor => (
                AgentRole::Hephaestus,
                0.85,
                "Refactoring → Hephaestus",
                Some(AgentRole::Librarian),
            ),

            TaskIntent::Documentation => (
                AgentRole::Librarian,
                0.9,
                "Documentation → Librarian",
                Some(AgentRole::Explorer),
            ),

            TaskIntent::Review => (
                AgentRole::Librarian,
                0.9,
                "Code review → Librarian",
                Some(AgentRole::Sisyphus),
            ),

            TaskIntent::Testing => (
                AgentRole::Hephaestus,
                0.8,
                "Test writing → Hephaestus",
                Some(AgentRole::Librarian),
            ),

            TaskIntent::Architecture => (
                AgentRole::Oracle,
                0.85,
                "Architecture decisions → Oracle",
                Some(AgentRole::Sisyphus),
            ),

            TaskIntent::QuickFix => {
                if self.config.explorer_quick_fixes {
                    (
                        AgentRole::Explorer,
                        0.9,
                        "Quick fix → Explorer (fast)",
                        Some(AgentRole::Hephaestus),
                    )
                } else {
                    (AgentRole::Hephaestus, 0.7, "Fallback quick fix", None)
                }
            }

            TaskIntent::Visual => (
                AgentRole::Multimodal,
                0.9,
                "Visual task → Multimodal (Gemini)",
                Some(AgentRole::Sisyphus),
            ),

            TaskIntent::Unknown => (
                AgentRole::Hephaestus,
                0.5,
                "Unknown intent → default to Hephaestus",
                Some(AgentRole::Sisyphus),
            ),
        };

        DispatchDecision {
            role,
            confidence,
            reasoning: reasoning.to_string(),
            fallback,
            context_additions: vec![],
        }
    }

    /// Get the fallback agent for a failed attempt.
    pub fn get_fallback(&self, current: AgentRole, attempt: u32) -> Option<AgentRole> {
        match (current, attempt) {
            // First fallback: try Sisyphus for error analysis
            (AgentRole::Hephaestus, 1) => Some(AgentRole::Sisyphus),
            (AgentRole::Explorer, 1) => Some(AgentRole::Hephaestus),
            (AgentRole::Librarian, 1) => Some(AgentRole::Sisyphus),

            // Second fallback: always try Sisyphus
            (_, 2) => Some(AgentRole::Sisyphus),

            // Third attempt: Sisyphus with extended thinking
            (AgentRole::Sisyphus, 3) => None, // Exhausted

            _ => None,
        }
    }
}

impl Default for AgentDispatcher {
    fn default() -> Self {
        Self::new(DispatchConfig::default())
    }
}

/// Estimate task complexity from title and context.
fn estimate_complexity(title: &str, _repo_context: &RepoContext) -> f32 {
    let lower = title.to_ascii_lowercase();
    let mut score: f32 = 0.5;

    // Complexity indicators
    if lower.contains("complex")
        || lower.contains("refactor")
        || lower.contains("redesign")
        || lower.contains("architect")
    {
        score += 0.3;
    }

    if lower.contains("performance") || lower.contains("optimize") || lower.contains("scale") {
        score += 0.2;
    }

    if lower.contains("security") || lower.contains("auth") || lower.contains("crypto") {
        score += 0.2;
    }

    // Simplicity indicators
    if lower.contains("simple")
        || lower.contains("typo")
        || lower.contains("rename")
        || lower.contains("minor")
    {
        score -= 0.3;
    }

    score.clamp(0.0, 1.0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_task(title: &str) -> Task {
        use orch_core::types::RepoId;
        Task::new(
            orch_core::types::TaskId::new("T1"),
            RepoId("test-repo".to_string()),
            title.to_string(),
            PathBuf::from(".orch/wt/T1"),
        )
    }

    #[test]
    fn dispatch_routes_implementation_to_hephaestus() {
        let dispatcher = AgentDispatcher::default();
        let task = make_task("Add user authentication endpoint");
        let ctx = RepoContext::default();

        let decision = dispatcher.dispatch(&task, &ctx, false, None);
        assert_eq!(decision.role, AgentRole::Hephaestus);
        assert!(decision.confidence > 0.8);
    }

    #[test]
    fn dispatch_routes_error_recovery_to_sisyphus() {
        let dispatcher = AgentDispatcher::default();
        let task = make_task("Fix the broken endpoint");
        let ctx = RepoContext::default();

        let decision = dispatcher.dispatch(&task, &ctx, true, Some("compile error"));
        assert_eq!(decision.role, AgentRole::Sisyphus);
        assert!(decision.context_additions.iter().any(|c| c.contains("retry")));
    }

    #[test]
    fn dispatch_routes_docs_to_librarian() {
        let dispatcher = AgentDispatcher::default();
        let task = make_task("Document the API endpoints");
        let ctx = RepoContext::default();

        let decision = dispatcher.dispatch(&task, &ctx, false, None);
        assert_eq!(decision.role, AgentRole::Librarian);
    }

    #[test]
    fn dispatch_routes_quick_fix_to_explorer() {
        let dispatcher = AgentDispatcher::default();
        let task = make_task("Fix simple typo in config");
        let ctx = RepoContext::default();

        let decision = dispatcher.dispatch(&task, &ctx, false, None);
        assert_eq!(decision.role, AgentRole::Explorer);
    }

    #[test]
    fn fallback_escalates_to_sisyphus() {
        let dispatcher = AgentDispatcher::default();

        assert_eq!(
            dispatcher.get_fallback(AgentRole::Hephaestus, 1),
            Some(AgentRole::Sisyphus)
        );
        assert_eq!(
            dispatcher.get_fallback(AgentRole::Explorer, 2),
            Some(AgentRole::Sisyphus)
        );
    }

    #[test]
    fn task_intent_classification() {
        assert_eq!(
            TaskIntent::classify("Add feature", TaskType::Implement, false),
            TaskIntent::Implementation
        );
        assert_eq!(
            TaskIntent::classify("Fix bug in login", TaskType::Implement, false),
            TaskIntent::BugFix
        );
        assert_eq!(
            TaskIntent::classify("Document API", TaskType::Implement, false),
            TaskIntent::Documentation
        );
        assert_eq!(
            TaskIntent::classify("Anything", TaskType::Implement, true),
            TaskIntent::ErrorRecovery
        );
    }

    #[test]
    fn agent_role_model_mapping() {
        assert_eq!(AgentRole::Hephaestus.model(), ModelKind::Codex);
        assert_eq!(AgentRole::Sisyphus.model(), ModelKind::Claude);
        assert_eq!(AgentRole::Explorer.model(), ModelKind::Claude);
        assert_eq!(AgentRole::Multimodal.model(), ModelKind::Gemini);
    }
}
