//! Rich prompt builder — assembles the full prompt sent to AI agents.
//!
//! Replaces the minimal `build_prompt()` in supervisor.rs with a structured
//! builder that injects context graph, test spec, retry info, and signal
//! definitions.

use orch_core::types::{ModelKind, TaskId};
use std::path::Path;

use crate::context_graph::{render_context_for_prompt, ContextGraph};

/// The type of task being performed — drives which template to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptRole {
    Implement,
    TestSpecWrite,
    Review,
    StackCaptain,
    QAValidate,
}

/// Retry context injected when a task is being retried.
#[derive(Debug, Clone)]
pub struct RetryContext {
    pub attempt: u32,
    pub max_retries: u32,
    pub previous_failure: String,
    pub previous_model: ModelKind,
}

/// Configuration for building a rich prompt.
#[derive(Debug, Clone)]
pub struct PromptConfig {
    pub task_id: TaskId,
    pub task_title: String,
    pub role: PromptRole,
    pub context: Option<ContextGraph>,
    pub test_spec: Option<String>,
    pub retry: Option<RetryContext>,
    pub verify_command: Option<String>,
    /// QA failure details injected when retrying after a QA validation failure.
    pub qa_failure_context: Option<String>,
}

/// Build a rich prompt from config and template directory.
///
/// The result is a single string ready to send to the agent CLI.
pub fn build_rich_prompt(config: &PromptConfig, template_dir: &Path) -> String {
    let mut sections: Vec<String> = Vec::new();

    // 1. Role template (from disk).
    let template_file = match config.role {
        PromptRole::Implement => "implementer.md",
        PromptRole::TestSpecWrite => "tests-specialist.md",
        PromptRole::Review => "reviewer.md",
        PromptRole::StackCaptain => "stack-captain.md",
        PromptRole::QAValidate => "qa-validator.md",
    };
    let template_path = template_dir.join(template_file);
    if let Ok(template) = std::fs::read_to_string(&template_path) {
        let content = template.trim();
        if content.lines().count() > 1 {
            // Only include if the template has real content (not just a header).
            sections.push(content.to_string());
        }
    }

    // 2. Task assignment.
    sections.push(format!(
        "# Task Assignment\n\n\
         **Task ID:** {}\n\
         **Title:** {}\n",
        config.task_id.0, config.task_title
    ));

    // 3. Repository context (from context graph).
    if let Some(ctx) = &config.context {
        if !ctx.nodes.is_empty() {
            sections.push(render_context_for_prompt(ctx));
        }
    }

    // 4. Test specification (if available).
    if let Some(spec) = &config.test_spec {
        sections.push(format!(
            "# Test Specification\n\n\
             The following test spec must pass before the task is considered complete:\n\n\
             {spec}\n"
        ));
    }

    // 5. Retry context (if retrying).
    if let Some(retry) = &config.retry {
        sections.push(format!(
            "# Retry Context\n\n\
             This is attempt **{}/{}**.\n\n\
             **Previous model:** {}\n\
             **Previous failure:**\n```\n{}\n```\n\n\
             Fix the issue described above. Do NOT repeat the same mistake.\n",
            retry.attempt,
            retry.max_retries,
            retry.previous_model.as_str(),
            retry.previous_failure,
        ));
    }

    // 5b. QA failure context (when retrying after QA validation failure).
    if let Some(qa_ctx) = &config.qa_failure_context {
        sections.push(qa_ctx.clone());
    }

    // 6. Verify command.
    if let Some(cmd) = &config.verify_command {
        sections.push(format!(
            "# Verification\n\n\
             Run this command to verify your changes before signalling completion:\n\
             ```bash\n{cmd}\n```\n"
        ));
    }

    // 7. Signal definitions (always appended).
    sections.push(signal_definitions());

    sections.join("\n---\n\n")
}

fn signal_definitions() -> String {
    "# Signals\n\n\
     When you are done and the code is ready, print exactly: `[patch_ready]`\n\
     If you are blocked and need human help, print exactly: `[needs_human]`\n\
     If you have a plan ready for decomposition, print exactly: `[plan_ready]`\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn mk_config() -> PromptConfig {
        PromptConfig {
            task_id: TaskId::new("T-42"),
            task_title: "Add authentication".to_string(),
            role: PromptRole::Implement,
            context: None,
            test_spec: None,
            retry: None,
            verify_command: None,
            qa_failure_context: None,
        }
    }

    #[test]
    fn basic_prompt_includes_task_info() {
        let config = mk_config();
        let prompt = build_rich_prompt(&config, Path::new("/nonexistent"));

        assert!(prompt.contains("T-42"));
        assert!(prompt.contains("Add authentication"));
        assert!(prompt.contains("[patch_ready]"));
        assert!(prompt.contains("[needs_human]"));
    }

    #[test]
    fn prompt_includes_retry_context() {
        let mut config = mk_config();
        config.retry = Some(RetryContext {
            attempt: 2,
            max_retries: 3,
            previous_failure: "cargo test failed: assertion error".to_string(),
            previous_model: ModelKind::Claude,
        });

        let prompt = build_rich_prompt(&config, Path::new("/nonexistent"));
        assert!(prompt.contains("attempt **2/3**"));
        assert!(prompt.contains("assertion error"));
        assert!(prompt.contains("claude"));
    }

    #[test]
    fn prompt_includes_test_spec() {
        let mut config = mk_config();
        config.test_spec = Some("- [ ] Login returns 200\n- [ ] Bad password returns 401\n".to_string());

        let prompt = build_rich_prompt(&config, Path::new("/nonexistent"));
        assert!(prompt.contains("Test Specification"));
        assert!(prompt.contains("Login returns 200"));
    }

    #[test]
    fn prompt_includes_verify_command() {
        let mut config = mk_config();
        config.verify_command = Some("cargo test --workspace".to_string());

        let prompt = build_rich_prompt(&config, Path::new("/nonexistent"));
        assert!(prompt.contains("cargo test --workspace"));
    }

    #[test]
    fn prompt_includes_template_when_present() {
        let tmp = std::env::temp_dir().join(format!("othala-tmpl-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join("implementer.md"),
            "# Implementer\n\nYou are an expert coder.\nFollow repo conventions.\n",
        )
        .unwrap();

        let config = mk_config();
        let prompt = build_rich_prompt(&config, &tmp);
        assert!(prompt.contains("expert coder"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn prompt_includes_context_graph() {
        use crate::context_graph::{ContextGraph, ContextNode};
        use std::path::PathBuf;

        let mut config = mk_config();
        config.context = Some(ContextGraph {
            nodes: vec![ContextNode {
                path: PathBuf::from(".othala/context/MAIN.md"),
                content: "# Main\nUse Rust.\n".to_string(),
                links: vec![],
                source_refs: vec![],
            }],
            total_chars: 16,
        });

        let prompt = build_rich_prompt(&config, Path::new("/nonexistent"));
        assert!(prompt.contains("Repository Context"));
        assert!(prompt.contains("Use Rust."));
    }

    #[test]
    fn all_roles_select_correct_template_file() {
        let tmp = std::env::temp_dir().join(format!("othala-roles-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("implementer.md"), "# Implementer\nImplement things.\n").unwrap();
        fs::write(tmp.join("tests-specialist.md"), "# Tests\nWrite tests.\n").unwrap();
        fs::write(tmp.join("reviewer.md"), "# Reviewer\nReview code.\n").unwrap();
        fs::write(tmp.join("stack-captain.md"), "# Stack Captain\nManage stacks.\n").unwrap();

        for (role, expected) in [
            (PromptRole::Implement, "Implement things"),
            (PromptRole::TestSpecWrite, "Write tests"),
            (PromptRole::Review, "Review code"),
            (PromptRole::StackCaptain, "Manage stacks"),
        ] {
            let mut config = mk_config();
            config.role = role;
            let prompt = build_rich_prompt(&config, &tmp);
            assert!(
                prompt.contains(expected),
                "role {:?} should include {:?}",
                role,
                expected
            );
        }

        fs::remove_dir_all(&tmp).ok();
    }
}
