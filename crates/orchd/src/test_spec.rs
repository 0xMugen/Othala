//! Test spec system — generate, parse, and validate test specifications.
//!
//! Test specs are markdown files stored at `.othala/specs/{task_id}.md` that
//! define acceptance criteria before implementation begins.

use orch_core::types::TaskId;
use std::path::{Path, PathBuf};

/// A single criterion in a test specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecCriterion {
    /// The description of the criterion.
    pub description: String,
    /// Whether this criterion has been checked off.
    pub checked: bool,
}

/// A parsed test specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestSpec {
    /// The raw markdown content.
    pub raw: String,
    /// Parsed criteria from the spec.
    pub criteria: Vec<SpecCriterion>,
}

/// Result of validating code against a test spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecValidation {
    /// Whether all criteria passed.
    pub passed: bool,
    /// Per-criterion results.
    pub results: Vec<CriterionResult>,
    /// Human-readable summary.
    pub summary: String,
}

/// Result for a single criterion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionResult {
    pub criterion: String,
    pub passed: bool,
}

/// Get the canonical path for a task's test spec file.
pub fn spec_path(repo_root: &Path, task_id: &TaskId) -> PathBuf {
    repo_root
        .join(".othala/specs")
        .join(format!("{}.md", task_id.0))
}

/// Generate the prompt to send to a test-spec-writing agent.
///
/// Produces **executable** test scenarios — not just checkbox items.
/// Each scenario describes concrete commands to run and expected outcomes.
pub fn generate_test_spec_prompt(task_title: &str, task_description: &str) -> String {
    format!(
        "Write a test specification for the following task.\n\n\
         **Task:** {task_title}\n\n\
         **Description:**\n{task_description}\n\n\
         Output the spec as markdown with two sections:\n\n\
         ### Acceptance Tests\n\
         Concrete, executable test scenarios. Each item should describe:\n\
         - The exact command to run (e.g., `cargo run`, `curl`, CLI invocations)\n\
         - The expected output or behavior to verify\n\
         - Use checkbox items (`- [ ] ...`) for each scenario\n\n\
         ### Regression Tests\n\
         Tests to verify existing functionality is not broken:\n\
         - Build verification (`cargo check`, `cargo test`)\n\
         - Existing endpoints/commands still work\n\
         - Database state integrity\n\n\
         Each criterion must be objectively verifiable by running actual commands.\n\
         Prefer concrete commands over abstract descriptions.\n\
         Example:\n\
         - [ ] Run `cargo build --workspace` — expect exit code 0\n\
         - [ ] Run `othala list` — expect empty list output\n\
         - [ ] POST /api/login with valid creds — expect 200 + session cookie\n"
    )
}

/// Parse a markdown test spec into structured criteria.
///
/// Looks for checkbox items: `- [ ] description` and `- [x] description`.
pub fn parse_test_spec(content: &str) -> TestSpec {
    let mut criteria = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            criteria.push(SpecCriterion {
                description: rest.trim().to_string(),
                checked: false,
            });
        } else if let Some(rest) = trimmed
            .strip_prefix("- [x] ")
            .or_else(|| trimmed.strip_prefix("- [X] "))
        {
            criteria.push(SpecCriterion {
                description: rest.trim().to_string(),
                checked: true,
            });
        }
    }

    TestSpec {
        raw: content.to_string(),
        criteria,
    }
}

/// Load and parse a test spec from disk.
pub fn load_test_spec(repo_root: &Path, task_id: &TaskId) -> Option<TestSpec> {
    let path = spec_path(repo_root, task_id);
    let content = std::fs::read_to_string(path).ok()?;
    Some(parse_test_spec(&content))
}

/// Save a test spec to disk.
pub fn save_test_spec(
    repo_root: &Path,
    task_id: &TaskId,
    content: &str,
) -> std::io::Result<PathBuf> {
    let path = spec_path(repo_root, task_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    Ok(path)
}

/// Build a prompt for validating implementation against a test spec.
///
/// This prompt is sent to a validation agent that checks each criterion.
pub fn generate_validation_prompt(spec: &TestSpec, task_title: &str) -> String {
    let mut prompt = format!(
        "Validate the implementation of \"{}\" against the following test spec.\n\n\
         For each criterion, verify whether it is satisfied. Mark passing criteria \
         with `- [x]` and failing ones with `- [ ]` plus a brief explanation.\n\n",
        task_title
    );

    for criterion in &spec.criteria {
        prompt.push_str(&format!("- [ ] {}\n", criterion.description));
    }

    prompt.push_str("\nAfter checking all criteria, print `[patch_ready]`.\n");
    prompt
}

/// Parse validation output from an agent (updated spec with checked items).
pub fn parse_validation_output(output: &str) -> SpecValidation {
    let spec = parse_test_spec(output);
    let total = spec.criteria.len();
    let passed_count = spec.criteria.iter().filter(|c| c.checked).count();

    let results: Vec<CriterionResult> = spec
        .criteria
        .iter()
        .map(|c| CriterionResult {
            criterion: c.description.clone(),
            passed: c.checked,
        })
        .collect();

    let passed = total > 0 && passed_count == total;
    let summary = format!("{}/{} criteria passed", passed_count, total);

    SpecValidation {
        passed,
        results,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spec_extracts_criteria() {
        let content = "\
## Test Spec: Auth

### Unit Tests
- [ ] Login returns 200 for valid credentials
- [ ] Login returns 401 for bad password
- [x] Session token is set on success

### Build
- [ ] cargo check passes
";
        let spec = parse_test_spec(content);
        assert_eq!(spec.criteria.len(), 4);
        assert!(!spec.criteria[0].checked);
        assert!(!spec.criteria[1].checked);
        assert!(spec.criteria[2].checked);
        assert!(!spec.criteria[3].checked);
        assert_eq!(
            spec.criteria[0].description,
            "Login returns 200 for valid credentials"
        );
    }

    #[test]
    fn parse_spec_handles_empty_content() {
        let spec = parse_test_spec("# No criteria here\n\nJust prose.\n");
        assert!(spec.criteria.is_empty());
    }

    #[test]
    fn spec_path_is_correct() {
        let path = spec_path(Path::new("/repo"), &TaskId::new("T-42"));
        assert_eq!(path, PathBuf::from("/repo/.othala/specs/T-42.md"));
    }

    #[test]
    fn save_and_load_spec() {
        let tmp = std::env::temp_dir().join(format!("othala-spec-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let content = "- [ ] Tests pass\n- [ ] No warnings\n";
        let task_id = TaskId::new("T-99");

        let path = save_test_spec(&tmp, &task_id, content).unwrap();
        assert!(path.exists());

        let spec = load_test_spec(&tmp, &task_id).expect("should load");
        assert_eq!(spec.criteria.len(), 2);
        assert_eq!(spec.criteria[0].description, "Tests pass");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn validation_all_passed() {
        let output = "- [x] Login works\n- [x] Tests pass\n";
        let result = parse_validation_output(output);

        assert!(result.passed);
        assert_eq!(result.results.len(), 2);
        assert!(result.results.iter().all(|r| r.passed));
        assert!(result.summary.contains("2/2"));
    }

    #[test]
    fn validation_partial_failure() {
        let output = "- [x] Login works\n- [ ] Tests pass (3 failures)\n";
        let result = parse_validation_output(output);

        assert!(!result.passed);
        assert!(result.results[0].passed);
        assert!(!result.results[1].passed);
        assert!(result.summary.contains("1/2"));
    }

    #[test]
    fn validation_empty_output() {
        let result = parse_validation_output("No criteria found.\n");

        assert!(!result.passed);
        assert!(result.results.is_empty());
    }

    #[test]
    fn generate_test_spec_prompt_includes_task_info() {
        let prompt = generate_test_spec_prompt("Add auth", "Implement login flow");
        assert!(prompt.contains("Add auth"));
        assert!(prompt.contains("Implement login flow"));
        assert!(prompt.contains("Acceptance Tests"));
        assert!(prompt.contains("Regression Tests"));
        assert!(prompt.contains("executable"));
    }

    #[test]
    fn generate_validation_prompt_includes_criteria() {
        let spec = TestSpec {
            raw: String::new(),
            criteria: vec![
                SpecCriterion {
                    description: "Login works".to_string(),
                    checked: false,
                },
                SpecCriterion {
                    description: "Tests pass".to_string(),
                    checked: false,
                },
            ],
        };
        let prompt = generate_validation_prompt(&spec, "Auth task");
        assert!(prompt.contains("Login works"));
        assert!(prompt.contains("Tests pass"));
        assert!(prompt.contains("[patch_ready]"));
    }
}
