use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orch_core::config::RepoConfig;
use orch_core::types::{Task, TaskRole, TaskType};
use orch_git::{GitCli, GitError};

const CONTEXT_ROOT: &str = ".orch/context";
const COMPARTMENTS_DIR: &str = "compartments";
const UPDATES_DIR: &str = "updates";
const GLOBAL_FILE: &str = "global.md";
const CORE_FILE: &str = "core.md";
const LATEST_MAIN_DIFF_FILE: &str = "main-diff-latest.md";

#[derive(Debug, thiserror::Error)]
pub enum GlobalContextError {
    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write file {path}: {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("git operation failed in {repo_path}: {source}")]
    Git {
        repo_path: PathBuf,
        #[source]
        source: GitError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalContextPaths {
    pub context_root: PathBuf,
    pub global_path: PathBuf,
    pub core_path: PathBuf,
    pub compartments_dir: PathBuf,
    pub updates_dir: PathBuf,
    pub latest_main_diff_path: PathBuf,
    pub branch_head_marker: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalContextRefreshOutcome {
    pub refreshed: bool,
    pub previous_head: Option<String>,
    pub current_head: String,
    pub changed_files: usize,
    pub latest_main_diff_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextCompartment {
    RepoMap,
    Architecture,
    Workflows,
    Quality,
    MainDiff,
}

impl ContextCompartment {
    pub fn all() -> &'static [ContextCompartment] {
        const ORDER: &[ContextCompartment] = &[
            ContextCompartment::RepoMap,
            ContextCompartment::Architecture,
            ContextCompartment::Workflows,
            ContextCompartment::Quality,
            ContextCompartment::MainDiff,
        ];
        ORDER
    }

    pub fn file_name(self) -> &'static str {
        match self {
            ContextCompartment::RepoMap => "repo-map.md",
            ContextCompartment::Architecture => "architecture.md",
            ContextCompartment::Workflows => "workflows.md",
            ContextCompartment::Quality => "quality.md",
            ContextCompartment::MainDiff => "main-diff.md",
        }
    }

    pub fn purpose(self) -> &'static str {
        match self {
            ContextCompartment::RepoMap => "where code and services live",
            ContextCompartment::Architecture => "system boundaries and ownership",
            ContextCompartment::Workflows => "task, Graphite, and branch lifecycle practices",
            ContextCompartment::Quality => "verify, testing, and review expectations",
            ContextCompartment::MainDiff => "latest base-branch changes captured from whole diff",
        }
    }
}

#[derive(Debug, Clone)]
struct BranchDiffSummary {
    range_label: String,
    shortstat: Option<String>,
    name_status_lines: Vec<String>,
}

pub fn ensure_global_context_layout(
    repo_root: &Path,
    base_branch: &str,
) -> Result<GlobalContextPaths, GlobalContextError> {
    let context_root = repo_root.join(CONTEXT_ROOT);
    let compartments_dir = context_root.join(COMPARTMENTS_DIR);
    let updates_dir = context_root.join(UPDATES_DIR);
    let global_path = context_root.join(GLOBAL_FILE);
    let core_path = context_root.join(CORE_FILE);
    let latest_main_diff_path = updates_dir.join(LATEST_MAIN_DIFF_FILE);
    let branch_head_marker = context_root.join(format!(
        "last_{}_head.txt",
        sanitize_for_file_name(base_branch)
    ));

    ensure_dir(&context_root)?;
    ensure_dir(&compartments_dir)?;
    ensure_dir(&updates_dir)?;

    let core_content = render_core(base_branch);
    write_if_missing(&global_path, &core_content)?;
    write_if_missing(&core_path, &core_content)?;
    for compartment in ContextCompartment::all() {
        let path = compartments_dir.join(compartment.file_name());
        write_if_missing(&path, &render_compartment(*compartment))?;
    }
    write_if_missing(
        &latest_main_diff_path,
        &render_empty_main_diff(base_branch, Utc::now()),
    )?;

    Ok(GlobalContextPaths {
        context_root,
        global_path,
        core_path,
        compartments_dir,
        updates_dir,
        latest_main_diff_path,
        branch_head_marker,
    })
}

pub fn maybe_refresh_global_context(
    repo_config: &RepoConfig,
    at: DateTime<Utc>,
) -> Result<GlobalContextRefreshOutcome, GlobalContextError> {
    let paths = ensure_global_context_layout(&repo_config.repo_path, &repo_config.base_branch)?;
    let git = GitCli::default();
    let current_head = branch_head_sha(&git, &repo_config.repo_path, &repo_config.base_branch)?;
    let previous_head = read_optional_trimmed(&paths.branch_head_marker)?;

    if previous_head.as_deref() == Some(current_head.as_str()) {
        return Ok(GlobalContextRefreshOutcome {
            refreshed: false,
            previous_head,
            current_head,
            changed_files: 0,
            latest_main_diff_path: paths.latest_main_diff_path,
        });
    }

    let diff = capture_branch_diff(
        &git,
        &repo_config.repo_path,
        previous_head.as_deref(),
        &current_head,
    )?;
    let changed_files = diff.name_status_lines.len();
    let rendered = render_main_diff_snapshot(
        &repo_config.base_branch,
        previous_head.as_deref(),
        &current_head,
        at,
        &diff,
    );

    write_file(&paths.latest_main_diff_path, &rendered)?;
    let snapshot_name = format!("main-diff-{}.md", short_sha(&current_head));
    let snapshot_path = paths.updates_dir.join(snapshot_name);
    write_file(&snapshot_path, &rendered)?;
    write_file(&paths.branch_head_marker, &format!("{current_head}\n"))?;

    Ok(GlobalContextRefreshOutcome {
        refreshed: true,
        previous_head,
        current_head,
        changed_files,
        latest_main_diff_path: paths.latest_main_diff_path,
    })
}

pub fn select_task_compartments(task: &Task, requested_work: &str) -> Vec<ContextCompartment> {
    let mut selected = HashSet::<ContextCompartment>::new();
    selected.insert(ContextCompartment::RepoMap);

    match task.role {
        TaskRole::Architecture => {
            selected.insert(ContextCompartment::Architecture);
        }
        TaskRole::GraphiteStack => {
            selected.insert(ContextCompartment::Workflows);
        }
        TaskRole::Docs => {
            selected.insert(ContextCompartment::Architecture);
        }
        TaskRole::Frontend => {
            selected.insert(ContextCompartment::Architecture);
            selected.insert(ContextCompartment::Quality);
        }
        TaskRole::Tests => {
            selected.insert(ContextCompartment::Quality);
        }
        TaskRole::General => {
            selected.insert(ContextCompartment::Architecture);
            selected.insert(ContextCompartment::Workflows);
        }
    }

    match &task.task_type {
        TaskType::Feature | TaskType::Refactor => {
            selected.insert(ContextCompartment::Architecture);
        }
        TaskType::Bugfix => {
            selected.insert(ContextCompartment::Quality);
        }
        TaskType::Docs => {
            selected.insert(ContextCompartment::Architecture);
        }
        TaskType::Test => {
            selected.insert(ContextCompartment::Quality);
        }
        TaskType::Chore | TaskType::Other(_) => {}
    }

    let lower = requested_work.to_ascii_lowercase();
    if contains_any(
        &lower,
        &["graphite", "restack", "submit", "rebase", "branch", "stack"],
    ) {
        selected.insert(ContextCompartment::Workflows);
    }
    if contains_any(
        &lower,
        &["test", "verify", "lint", "coverage", "regression", "flaky"],
    ) {
        selected.insert(ContextCompartment::Quality);
    }
    if contains_any(
        &lower,
        &[
            "architecture",
            "design",
            "boundary",
            "service",
            "module",
            "core",
        ],
    ) {
        selected.insert(ContextCompartment::Architecture);
    }
    if contains_any(
        &lower,
        &[
            "main",
            "base branch",
            "upstream",
            "recent diff",
            "changelog",
        ],
    ) {
        selected.insert(ContextCompartment::MainDiff);
    }

    ContextCompartment::all()
        .iter()
        .copied()
        .filter(|compartment| selected.contains(compartment))
        .collect()
}

pub fn build_task_context_prompt(
    repo_config: &RepoConfig,
    task: &Task,
    requested_work: &str,
) -> Result<String, GlobalContextError> {
    let paths = ensure_global_context_layout(&repo_config.repo_path, &repo_config.base_branch)?;
    let selected = select_task_compartments(task, requested_work);
    let selected_lines = selected
        .iter()
        .map(|compartment| {
            let path = paths.compartments_dir.join(compartment.file_name());
            format!("- {} ({})", path.display(), compartment.purpose(),)
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(format!(
        "Global context (shared across all tasks in this repo):\n\
Global file: {}\n\
Core file (legacy alias): {}\n\
Use compartment-based loading:\n\
1. Read the global file first.\n\
2. Only read the compartment files below unless you discover a concrete need.\n\
{}\n\
3. If base-branch changes are relevant, read: {}",
        paths.global_path.display(),
        paths.core_path.display(),
        selected_lines,
        paths.latest_main_diff_path.display()
    ))
}

fn ensure_dir(path: &Path) -> Result<(), GlobalContextError> {
    fs::create_dir_all(path).map_err(|source| GlobalContextError::CreateDir {
        path: path.to_path_buf(),
        source,
    })
}

fn write_if_missing(path: &Path, content: &str) -> Result<(), GlobalContextError> {
    if path.exists() {
        return Ok(());
    }
    write_file(path, content)
}

fn write_file(path: &Path, content: &str) -> Result<(), GlobalContextError> {
    fs::write(path, content).map_err(|source| GlobalContextError::WriteFile {
        path: path.to_path_buf(),
        source,
    })
}

fn read_optional_trimmed(path: &Path) -> Result<Option<String>, GlobalContextError> {
    if !path.exists() {
        return Ok(None);
    }
    let value = fs::read_to_string(path).map_err(|source| GlobalContextError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn branch_head_sha(
    git: &GitCli,
    repo_root: &Path,
    base_branch: &str,
) -> Result<String, GlobalContextError> {
    let reference = format!("refs/heads/{base_branch}");
    let output = git
        .run(repo_root, ["rev-parse", reference.as_str()])
        .map_err(|source| GlobalContextError::Git {
            repo_path: repo_root.to_path_buf(),
            source,
        })?;
    Ok(output.stdout.trim().to_string())
}

fn capture_branch_diff(
    git: &GitCli,
    repo_root: &Path,
    previous_head: Option<&str>,
    current_head: &str,
) -> Result<BranchDiffSummary, GlobalContextError> {
    let (range_label, name_status_output, shortstat_output) =
        if let Some(previous) = previous_head.filter(|value| !value.trim().is_empty()) {
            let range = format!("{previous}..{current_head}");
            let names = git
                .run(
                    repo_root,
                    ["diff", "--name-status", "--find-renames", range.as_str()],
                )
                .map_err(|source| GlobalContextError::Git {
                    repo_path: repo_root.to_path_buf(),
                    source,
                })?;
            let shortstat = git
                .run(repo_root, ["diff", "--shortstat", range.as_str()])
                .map_err(|source| GlobalContextError::Git {
                    repo_path: repo_root.to_path_buf(),
                    source,
                })?;
            (range, names.stdout, shortstat.stdout)
        } else {
            let names = git
                .run(
                    repo_root,
                    ["show", "--name-status", "--format=", current_head],
                )
                .map_err(|source| GlobalContextError::Git {
                    repo_path: repo_root.to_path_buf(),
                    source,
                })?;
            let shortstat = git
                .run(
                    repo_root,
                    ["show", "--shortstat", "--format=", current_head],
                )
                .map_err(|source| GlobalContextError::Git {
                    repo_path: repo_root.to_path_buf(),
                    source,
                })?;
            (current_head.to_string(), names.stdout, shortstat.stdout)
        };

    let name_status_lines = name_status_output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    let shortstat = shortstat_output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string);

    Ok(BranchDiffSummary {
        range_label,
        shortstat,
        name_status_lines,
    })
}

fn render_core(base_branch: &str) -> String {
    format!(
        "# Global Context Core\n\n\
This file is the shared entry point for task context.\n\
Read this file first, then open only the compartments requested in the task prompt.\n\n\
## Compartments\n\
- `compartments/repo-map.md`: where code and services live\n\
- `compartments/architecture.md`: system boundaries and ownership\n\
- `compartments/workflows.md`: task, Graphite, and branch lifecycle practices\n\
- `compartments/quality.md`: verify, testing, and review expectations\n\
- `compartments/main-diff.md`: what to check in base-branch update snapshots\n\n\
## Base Branch Update Feed\n\
- tracked branch: `{base_branch}`\n\
- latest snapshot: `updates/main-diff-latest.md`\n\
- history snapshots: `updates/main-diff-<sha>.md`\n"
    )
}

fn render_compartment(compartment: ContextCompartment) -> String {
    match compartment {
        ContextCompartment::RepoMap => "# Repo Map\n\n\
Keep this file focused on where things live.\n\n\
## Primary Crates\n\
- `crates/orch-core`: shared state, events, and config types\n\
- `crates/orchd`: daemon, scheduler, runtime, and orchestration flows\n\
- `crates/orch-agents`: adapters and epoch execution helpers\n\
- `crates/orch-git`, `crates/orch-graphite`, `crates/orch-verify`: external tool wrappers\n\
- `crates/orch-tui`, `crates/orch-web`: operator interfaces\n\n\
## Runtime Storage\n\
- `.orch/state.sqlite`: canonical orchestrator state\n\
- `.orch/events/`: JSONL task/global event logs\n\
- `.orch/wt/<task-id>`: task worktrees\n\
- `.orch/context/`: shared task context (this system)\n"
            .to_string(),
        ContextCompartment::Architecture => "# Architecture\n\n\
Use this compartment for system boundaries and ownership.\n\n\
## Runtime Flow\n\
1. Scheduler selects queued tasks.\n\
2. Runtime initializes branches/worktrees and verify phases.\n\
3. Agent sessions drive implementation and emit lifecycle signals.\n\
4. Service/event log records every state transition.\n\n\
## Ownership Heuristics\n\
- lifecycle and transitions: `crates/orchd/src/service.rs`\n\
- orchestration tick behavior: `crates/orchd/src/runtime.rs`\n\
- task prompt and TUI/daemon wiring: `crates/orchd/src/main.rs`\n"
            .to_string(),
        ContextCompartment::Workflows => "# Workflows\n\n\
Use this compartment for branch lifecycle and operator actions.\n\n\
## Task Lifecycle\n\
- create task -> queued -> initializing -> running\n\
- verify/review gates promote to ready/submitting\n\
- merged tasks are detected and cleaned up\n\n\
## Branch + Stack Practices\n\
- task branches are managed through Graphite wrappers\n\
- restack conflicts can trigger a dedicated conflict-resolution session\n\
- submit mode can be single-branch or stack-based per repo config\n"
            .to_string(),
        ContextCompartment::Quality => "# Quality\n\n\
Use this compartment for verification and review expectations.\n\n\
## Verification\n\
- quick and full verify commands come from repo config\n\
- runtime records verify events and promotes/blocks state transitions\n\n\
## Review Expectations\n\
- review gate policy determines required approvals and capacity\n\
- critical findings should include file + line references when available\n\
- changes should include focused tests for regressions and state transitions\n"
            .to_string(),
        ContextCompartment::MainDiff => "# Main Diff\n\n\
This compartment explains base-branch update snapshots.\n\n\
## Inputs\n\
- `updates/main-diff-latest.md`: current snapshot\n\
- `updates/main-diff-<sha>.md`: historical snapshots\n\n\
## Guidance\n\
- check changed files before broad refactors\n\
- prefer targeted updates aligned with recent base-branch movement\n\
- if your task collides with recently changed files, re-read the latest snapshot\n"
            .to_string(),
    }
}

fn render_empty_main_diff(base_branch: &str, at: DateTime<Utc>) -> String {
    format!(
        "# Main Branch Diff Snapshot\n\n\
Generated at: {}\n\
Branch: {}\n\
From: <none>\n\
To: <unknown>\n\
Range: <none>\n\
Summary: no updates captured yet\n\
Changed files: 0\n\n\
## Name-status\n\
```text\n\
(no updates captured yet)\n\
```\n",
        at.to_rfc3339(),
        base_branch
    )
}

fn render_main_diff_snapshot(
    base_branch: &str,
    previous_head: Option<&str>,
    current_head: &str,
    at: DateTime<Utc>,
    diff: &BranchDiffSummary,
) -> String {
    let from = previous_head.unwrap_or("<none>");
    let shortstat = diff
        .shortstat
        .as_deref()
        .unwrap_or("no shortstat available from git");
    let name_status = if diff.name_status_lines.is_empty() {
        "(no file-level changes)".to_string()
    } else {
        diff.name_status_lines.join("\n")
    };

    format!(
        "# Main Branch Diff Snapshot\n\n\
Generated at: {}\n\
Branch: {}\n\
From: {}\n\
To: {}\n\
Range: {}\n\
Summary: {}\n\
Changed files: {}\n\n\
## Name-status\n\
```text\n\
{}\n\
```\n",
        at.to_rfc3339(),
        base_branch,
        from,
        current_head,
        diff.range_label,
        shortstat,
        diff.name_status_lines.len(),
        name_status
    )
}

fn sanitize_for_file_name(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "branch".to_string()
    } else {
        out
    }
}

fn short_sha(value: &str) -> &str {
    let trimmed = value.trim();
    let max = 12usize.min(trimmed.len());
    &trimmed[..max]
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::Utc;
    use orch_core::config::{
        NixConfig, RepoConfig, RepoGraphiteConfig, VerifyCommands, VerifyConfig,
    };
    use orch_core::state::{ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus};
    use orch_core::types::{ModelKind, RepoId, SubmitMode, Task, TaskId, TaskRole, TaskType};

    use super::{
        build_task_context_prompt, ensure_global_context_layout, maybe_refresh_global_context,
        select_task_compartments, ContextCompartment,
    };

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("othala-global-context-{prefix}-{stamp}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit_all(cwd: &Path, message: &str) {
        run_git(cwd, &["add", "."]);
        run_git(
            cwd,
            &[
                "-c",
                "user.name=Test User",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                message,
            ],
        );
    }

    fn init_main_repo(root: &Path) {
        run_git(root, &["init"]);
        fs::write(root.join("README.md"), "hello\n").expect("write README");
        commit_all(root, "init");
        run_git(root, &["branch", "-M", "main"]);
    }

    fn mk_repo_config(root: &Path) -> RepoConfig {
        RepoConfig {
            repo_id: "example".to_string(),
            repo_path: root.to_path_buf(),
            base_branch: "main".to_string(),
            nix: NixConfig {
                dev_shell: "nix develop".to_string(),
            },
            verify: VerifyConfig {
                quick: VerifyCommands {
                    commands: vec!["nix develop -c true".to_string()],
                },
                full: VerifyCommands {
                    commands: vec!["nix develop -c true".to_string()],
                },
            },
            graphite: RepoGraphiteConfig {
                draft_on_start: true,
                submit_mode: Some(SubmitMode::Single),
            },
        }
    }

    fn mk_task(role: TaskRole, task_type: TaskType) -> Task {
        Task {
            id: TaskId("T100".to_string()),
            repo_id: RepoId("example".to_string()),
            title: "task".to_string(),
            state: TaskState::Running,
            role,
            task_type,
            preferred_model: Some(ModelKind::Codex),
            depends_on: Vec::new(),
            submit_mode: SubmitMode::Single,
            branch_name: Some("task/T100".to_string()),
            worktree_path: PathBuf::from(".orch/wt/T100"),
            pr: None,
            verify_status: VerifyStatus::NotRun,
            review_status: ReviewStatus {
                required_models: Vec::new(),
                approvals_received: 0,
                approvals_required: 0,
                unanimous: false,
                capacity_state: ReviewCapacityState::Sufficient,
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn ensure_layout_creates_core_compartments_and_updates_paths() {
        let root = unique_temp_dir("layout");
        let paths = ensure_global_context_layout(&root, "main").expect("ensure layout");

        assert!(paths.global_path.exists());
        assert!(paths.core_path.exists());
        assert!(paths.compartments_dir.join("repo-map.md").exists());
        assert!(paths.compartments_dir.join("architecture.md").exists());
        assert!(paths.compartments_dir.join("workflows.md").exists());
        assert!(paths.compartments_dir.join("quality.md").exists());
        assert!(paths.compartments_dir.join("main-diff.md").exists());
        assert!(paths.latest_main_diff_path.exists());

        let global = fs::read_to_string(&paths.global_path).expect("read global");
        assert!(global.contains("compartments/repo-map.md"));
        assert!(global.contains("updates/main-diff-latest.md"));

        let core = fs::read_to_string(&paths.core_path).expect("read core");
        assert!(core.contains("compartments/repo-map.md"));
        assert!(core.contains("updates/main-diff-latest.md"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn maybe_refresh_updates_snapshot_when_main_head_changes() {
        let root = unique_temp_dir("refresh");
        init_main_repo(&root);
        let repo_config = mk_repo_config(&root);

        let first =
            maybe_refresh_global_context(&repo_config, Utc::now()).expect("first context refresh");
        assert!(first.refreshed);
        assert!(first.previous_head.is_none());
        assert!(first.current_head.len() >= 12);

        let second =
            maybe_refresh_global_context(&repo_config, Utc::now()).expect("second context refresh");
        assert!(!second.refreshed);
        assert_eq!(second.current_head, first.current_head);

        fs::write(root.join("README.md"), "hello\nworld\n").expect("rewrite README");
        commit_all(&root, "update");
        let third =
            maybe_refresh_global_context(&repo_config, Utc::now()).expect("third context refresh");
        assert!(third.refreshed);
        assert_eq!(
            third.previous_head.as_deref(),
            Some(first.current_head.as_str())
        );
        assert_ne!(third.current_head, first.current_head);
        assert!(third.changed_files >= 1);

        let diff = fs::read_to_string(third.latest_main_diff_path).expect("read diff file");
        assert!(diff.contains("README.md"));
        assert!(diff.contains("Range:"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn select_compartments_uses_role_type_and_requested_work() {
        let task = mk_task(TaskRole::Tests, TaskType::Bugfix);
        let compartments = select_task_compartments(
            &task,
            "fix flaky verify and check recent main diff for regression",
        );
        assert!(compartments.contains(&ContextCompartment::RepoMap));
        assert!(compartments.contains(&ContextCompartment::Quality));
        assert!(compartments.contains(&ContextCompartment::MainDiff));
    }

    #[test]
    fn build_task_prompt_section_includes_core_and_selected_compartments() {
        let root = unique_temp_dir("prompt");
        let repo_config = mk_repo_config(&root);
        let task = mk_task(TaskRole::Architecture, TaskType::Feature);

        let section = build_task_context_prompt(
            &repo_config,
            &task,
            "update service architecture and module boundaries",
        )
        .expect("build task context prompt");

        assert!(section.contains("Core file (legacy alias):"));
        assert!(section.contains("Global file:"));
        assert!(section.contains("global.md"));
        assert!(section.contains("repo-map.md"));
        assert!(section.contains("architecture.md"));
        assert!(section.contains("main-diff-latest.md"));

        let _ = fs::remove_dir_all(root);
    }
}
