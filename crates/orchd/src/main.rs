use chrono::Utc;
use orch_agents::{
    probe_models, summarize_setup, validate_setup_selection, ModelSetupSelection, SetupError,
    SetupProbeConfig, SetupSummary,
};
use orch_core::config::{
    apply_setup_selection_to_org_config, load_org_config, load_repo_config, save_org_config,
    ConfigError, RepoConfig, SetupApplyError,
};
use orch_core::events::{Event, EventKind};
use orch_core::state::{ReviewCapacityState, ReviewStatus, TaskState, VerifyStatus};
use orch_core::types::ModelKind;
use orch_core::types::{EventId, Task, TaskSpec};
use orch_core::validation::{Validate, ValidationIssue, ValidationLevel};
use orchd::{OrchdService, Scheduler, SchedulerConfig, ServiceError};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const DEFAULT_ORG_CONFIG: &str = "config/org.toml";
const DEFAULT_REPOS_CONFIG_DIR: &str = "config/repos";
const DEFAULT_SQLITE_PATH: &str = ".orch/state.sqlite";
const DEFAULT_EVENT_LOG_ROOT: &str = ".orch/events";

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunCliArgs {
    org_config_path: PathBuf,
    repos_config_dir: PathBuf,
    sqlite_path: PathBuf,
    event_log_root: PathBuf,
    once: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SetupCliArgs {
    org_config_path: PathBuf,
    enabled_models: Option<Vec<ModelKind>>,
    per_model_concurrency: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CreateTaskCliArgs {
    org_config_path: PathBuf,
    repos_config_dir: PathBuf,
    sqlite_path: PathBuf,
    event_log_root: PathBuf,
    spec_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListTasksCliArgs {
    org_config_path: PathBuf,
    sqlite_path: PathBuf,
    event_log_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliCommand {
    Run(RunCliArgs),
    Setup(SetupCliArgs),
    CreateTask(CreateTaskCliArgs),
    ListTasks(ListTasksCliArgs),
    Help(String),
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("{0}")]
    Args(String),
    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to load org config at {path}: {source}")]
    LoadConfig {
        path: PathBuf,
        #[source]
        source: ConfigError,
    },
    #[error("failed to save org config at {path}: {source}")]
    SaveConfig {
        path: PathBuf,
        #[source]
        source: ConfigError,
    },
    #[error("failed to read repo config directory {path}: {source}")]
    ReadRepoConfigDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read task spec file at {path}: {source}")]
    ReadTaskSpecFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse task spec json at {path}: {source}")]
    ParseTaskSpecJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to serialize task list as json: {source}")]
    SerializeTaskList {
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Setup(#[from] SetupError),
    #[error(transparent)]
    SetupApply(#[from] SetupApplyError),
    #[error("{0}")]
    InvalidConfig(String),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Web(#[from] std::io::Error),
}

fn main() {
    if let Err(err) = run() {
        eprintln!("orchd startup failed: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), MainError> {
    let mut argv = env::args();
    let program = argv.next().unwrap_or_else(|| "orchd".to_string());
    let command = parse_cli_args(argv.collect::<Vec<_>>(), &program)?;

    match command {
        CliCommand::Help(text) => {
            println!("{text}");
            Ok(())
        }
        CliCommand::Run(args) => run_daemon(args),
        CliCommand::Setup(args) => run_setup(args),
        CliCommand::CreateTask(args) => run_create_task(args),
        CliCommand::ListTasks(args) => run_list_tasks(args),
    }
}

fn run_daemon(args: RunCliArgs) -> Result<(), MainError> {
    ensure_parent_dir(&args.sqlite_path)?;
    ensure_dir(&args.event_log_root)?;

    let org = load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
        path: args.org_config_path.clone(),
        source,
    })?;
    validate_org_config(&org.validate())?;

    let repo_configs = load_repo_configs(&args.repos_config_dir)?;
    validate_repo_configs(&repo_configs)?;

    let scheduler = Scheduler::new(SchedulerConfig::from_org_config(&org));
    let service = OrchdService::open(&args.sqlite_path, &args.event_log_root, scheduler)?;

    let task_count = service.list_tasks()?.len();
    println!(
        "orchd bootstrapped sqlite={} event_log_root={} tasks={}",
        args.sqlite_path.display(),
        args.event_log_root.display(),
        task_count
    );
    if repo_configs.is_empty() {
        println!(
            "orchd loaded 0 repo configs from {}",
            args.repos_config_dir.display()
        );
    } else {
        let repo_ids = repo_configs
            .iter()
            .map(|(_, cfg)| cfg.repo_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "orchd loaded {} repo configs from {} [{}]",
            repo_configs.len(),
            args.repos_config_dir.display(),
            repo_ids
        );
    }

    if args.once {
        println!("orchd exiting after bootstrap (--once)");
        return Ok(());
    }

    println!("orchd running; press Ctrl+C to stop");
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

fn run_setup(args: SetupCliArgs) -> Result<(), MainError> {
    let mut org =
        load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
            path: args.org_config_path.clone(),
            source,
        })?;
    validate_org_config(&org.validate())?;

    let probe_config = SetupProbeConfig::default();
    let report = probe_models(&probe_config);

    let selected_models = if let Some(enabled) = args.enabled_models.clone() {
        enabled
    } else {
        report
            .models
            .iter()
            .filter(|probe| probe.healthy)
            .map(|probe| probe.model)
            .collect::<Vec<_>>()
    };

    if selected_models.is_empty() {
        return Err(MainError::Args(
            "no healthy models detected; pass --enable with explicit models".to_string(),
        ));
    }

    let validated = validate_setup_selection(
        &report,
        &ModelSetupSelection {
            enabled_models: selected_models,
        },
    )?;

    let per_model_concurrency = args
        .per_model_concurrency
        .unwrap_or_else(|| org.concurrency.codex.max(1));

    apply_setup_selection_to_org_config(
        &mut org,
        &validated.enabled_models,
        per_model_concurrency,
    )?;
    validate_org_config(&org.validate())?;

    save_org_config(&args.org_config_path, &org).map_err(|source| MainError::SaveConfig {
        path: args.org_config_path.clone(),
        source,
    })?;

    let summary = summarize_setup(&report, &validated);
    print_setup_summary(&summary, per_model_concurrency, &args.org_config_path);

    Ok(())
}

fn run_create_task(args: CreateTaskCliArgs) -> Result<(), MainError> {
    ensure_parent_dir(&args.sqlite_path)?;
    ensure_dir(&args.event_log_root)?;

    let org = load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
        path: args.org_config_path.clone(),
        source,
    })?;
    validate_org_config(&org.validate())?;

    let repo_configs = load_repo_configs(&args.repos_config_dir)?;
    validate_repo_configs(&repo_configs)?;

    let spec_raw =
        fs::read_to_string(&args.spec_path).map_err(|source| MainError::ReadTaskSpecFile {
            path: args.spec_path.clone(),
            source,
        })?;
    let spec: TaskSpec =
        serde_json::from_str(&spec_raw).map_err(|source| MainError::ParseTaskSpecJson {
            path: args.spec_path.clone(),
            source,
        })?;

    let spec_errors = spec
        .validate()
        .into_iter()
        .filter(|issue| issue.level == ValidationLevel::Error)
        .collect::<Vec<_>>();
    if !spec_errors.is_empty() {
        let rendered = spec_errors
            .iter()
            .map(|issue| format!("{}: {}", issue.code, issue.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(MainError::InvalidConfig(format!(
            "task spec validation failed ({})",
            rendered
        )));
    }

    if !repo_configs
        .iter()
        .any(|(_, cfg)| cfg.repo_id == spec.repo_id.0)
    {
        return Err(MainError::InvalidConfig(format!(
            "task repo_id '{}' is missing in {}",
            spec.repo_id.0,
            args.repos_config_dir.display()
        )));
    }

    let now = Utc::now();
    let task = Task {
        id: spec.task_id.clone(),
        repo_id: spec.repo_id.clone(),
        title: spec.title.clone(),
        state: TaskState::Queued,
        role: spec.role,
        task_type: spec.task_type.clone(),
        preferred_model: spec.preferred_model,
        depends_on: spec.depends_on.clone(),
        submit_mode: spec.submit_mode.unwrap_or(org.graphite.submit_mode_default),
        branch_name: None,
        worktree_path: PathBuf::from(format!(".orch/wt/{}", spec.task_id.0)),
        pr: None,
        verify_status: VerifyStatus::NotRun,
        review_status: ReviewStatus {
            required_models: Vec::new(),
            approvals_received: 0,
            approvals_required: 0,
            unanimous: false,
            capacity_state: ReviewCapacityState::Sufficient,
        },
        created_at: now,
        updated_at: now,
    };
    let created_event = Event {
        id: EventId(format!(
            "E-CREATE-{}-{}",
            task.id.0,
            now.timestamp_nanos_opt().unwrap_or_default()
        )),
        task_id: Some(task.id.clone()),
        repo_id: Some(task.repo_id.clone()),
        at: now,
        kind: EventKind::TaskCreated,
    };

    let scheduler = Scheduler::new(SchedulerConfig::from_org_config(&org));
    let service = OrchdService::open(&args.sqlite_path, &args.event_log_root, scheduler)?;
    service.create_task(&task, &created_event)?;

    println!(
        "created task id={} repo={} state={} spec={}",
        task.id.0,
        task.repo_id.0,
        "QUEUED",
        args.spec_path.display()
    );

    Ok(())
}

fn run_list_tasks(args: ListTasksCliArgs) -> Result<(), MainError> {
    ensure_parent_dir(&args.sqlite_path)?;
    ensure_dir(&args.event_log_root)?;

    let org = load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
        path: args.org_config_path.clone(),
        source,
    })?;
    validate_org_config(&org.validate())?;

    let scheduler = Scheduler::new(SchedulerConfig::from_org_config(&org));
    let service = OrchdService::open(&args.sqlite_path, &args.event_log_root, scheduler)?;
    let tasks = service.list_tasks()?;
    let rendered = serde_json::to_string_pretty(&tasks)
        .map_err(|source| MainError::SerializeTaskList { source })?;
    println!("{rendered}");

    Ok(())
}

fn print_setup_summary(summary: &SetupSummary, per_model_concurrency: usize, path: &Path) {
    println!(
        "setup saved org config to {} (per-model concurrency={})",
        path.display(),
        per_model_concurrency
    );
    println!(
        "selected models: {}",
        summary
            .selected_models
            .iter()
            .map(model_kind_tag)
            .collect::<Vec<_>>()
            .join(", ")
    );

    for item in &summary.items {
        let healthy = if item.healthy { "healthy" } else { "unhealthy" };
        let detected = if item.detected {
            "detected"
        } else {
            "not_detected"
        };
        println!(
            "- {}: {} {} selected={}",
            model_kind_tag(&item.model),
            detected,
            healthy,
            item.selected
        );
    }
}

fn model_kind_tag(model: &ModelKind) -> &'static str {
    match model {
        ModelKind::Claude => "claude",
        ModelKind::Codex => "codex",
        ModelKind::Gemini => "gemini",
    }
}

fn ensure_dir(path: &Path) -> Result<(), MainError> {
    fs::create_dir_all(path).map_err(|source| MainError::CreateDir {
        path: path.to_path_buf(),
        source,
    })
}

fn ensure_parent_dir(path: &Path) -> Result<(), MainError> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        ensure_dir(parent)?;
    }
    Ok(())
}

fn validate_org_config(issues: &[ValidationIssue]) -> Result<(), MainError> {
    let errors = issues
        .iter()
        .filter(|issue| issue.level == ValidationLevel::Error)
        .collect::<Vec<_>>();

    if errors.is_empty() {
        return Ok(());
    }

    let rendered = errors
        .iter()
        .map(|issue| format!("{}: {}", issue.code, issue.message))
        .collect::<Vec<_>>()
        .join("; ");
    Err(MainError::InvalidConfig(format!(
        "org config validation failed ({})",
        rendered
    )))
}

fn load_repo_configs(repo_dir: &Path) -> Result<Vec<(PathBuf, RepoConfig)>, MainError> {
    if !repo_dir.exists() {
        return Ok(Vec::new());
    }

    let entries = fs::read_dir(repo_dir).map_err(|source| MainError::ReadRepoConfigDir {
        path: repo_dir.to_path_buf(),
        source,
    })?;

    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| MainError::ReadRepoConfigDir {
            path: repo_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) == Some("toml") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut repo_configs = Vec::new();
    for path in paths {
        let cfg = load_repo_config(&path).map_err(|source| MainError::LoadConfig {
            path: path.clone(),
            source,
        })?;
        repo_configs.push((path, cfg));
    }
    Ok(repo_configs)
}

fn validate_repo_configs(repo_configs: &[(PathBuf, RepoConfig)]) -> Result<(), MainError> {
    let mut errors = Vec::new();

    for (path, cfg) in repo_configs {
        for issue in cfg.validate() {
            if issue.level == ValidationLevel::Error {
                errors.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    issue.code,
                    issue.message
                ));
            }
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    Err(MainError::InvalidConfig(format!(
        "repo config validation failed ({})",
        errors.join("; ")
    )))
}

fn parse_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    if args.is_empty() {
        return Ok(CliCommand::Run(default_run_args()));
    }

    match args[0].as_str() {
        "setup" => parse_setup_cli_args(args[1..].to_vec(), program),
        "create-task" => parse_create_task_cli_args(args[1..].to_vec(), program),
        "list-tasks" => parse_list_tasks_cli_args(args[1..].to_vec(), program),
        "help" | "--help" | "-h" => Ok(CliCommand::Help(usage(program))),
        _ => parse_run_cli_args(args, program),
    }
}

fn parse_run_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = default_run_args();

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(usage(program))),
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--repos-config-dir" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --repos-config-dir".to_string())
                })?;
                parsed.repos_config_dir = PathBuf::from(value);
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                parsed.sqlite_path = PathBuf::from(value);
            }
            "--event-log-root" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --event-log-root".to_string())
                })?;
                parsed.event_log_root = PathBuf::from(value);
            }
            "--once" => {
                parsed.once = true;
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown argument: {other}\n\n{}",
                    usage(program)
                )));
            }
        }
        idx += 1;
    }

    Ok(CliCommand::Run(parsed))
}

fn parse_setup_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = SetupCliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        enabled_models: None,
        per_model_concurrency: None,
    };
    let mut enabled_models = Vec::new();

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(setup_usage(program))),
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--enable" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --enable".to_string()))?;
                enabled_models.extend(parse_enabled_models(value)?);
            }
            "--per-model-concurrency" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --per-model-concurrency".to_string())
                })?;
                let parsed_value = value.parse::<usize>().map_err(|_| {
                    MainError::Args(format!(
                        "invalid --per-model-concurrency value: {value} (expected usize > 0)"
                    ))
                })?;
                if parsed_value == 0 {
                    return Err(MainError::Args(
                        "invalid --per-model-concurrency value: 0 (must be > 0)".to_string(),
                    ));
                }
                parsed.per_model_concurrency = Some(parsed_value);
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown setup argument: {other}\n\n{}",
                    setup_usage(program)
                )));
            }
        }
        idx += 1;
    }

    if !enabled_models.is_empty() {
        parsed.enabled_models = Some(enabled_models);
    }

    Ok(CliCommand::Setup(parsed))
}

fn parse_create_task_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = CreateTaskCliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        repos_config_dir: PathBuf::from(DEFAULT_REPOS_CONFIG_DIR),
        sqlite_path: PathBuf::from(DEFAULT_SQLITE_PATH),
        event_log_root: PathBuf::from(DEFAULT_EVENT_LOG_ROOT),
        spec_path: PathBuf::new(),
    };

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(create_task_usage(program))),
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--repos-config-dir" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --repos-config-dir".to_string())
                })?;
                parsed.repos_config_dir = PathBuf::from(value);
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                parsed.sqlite_path = PathBuf::from(value);
            }
            "--event-log-root" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --event-log-root".to_string())
                })?;
                parsed.event_log_root = PathBuf::from(value);
            }
            "--spec" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --spec".to_string()))?;
                parsed.spec_path = PathBuf::from(value);
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown create-task argument: {other}\n\n{}",
                    create_task_usage(program)
                )));
            }
        }
        idx += 1;
    }

    if parsed.spec_path.as_os_str().is_empty() {
        return Err(MainError::Args(
            "missing required --spec <path> for create-task".to_string(),
        ));
    }

    Ok(CliCommand::CreateTask(parsed))
}

fn parse_list_tasks_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = ListTasksCliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        sqlite_path: PathBuf::from(DEFAULT_SQLITE_PATH),
        event_log_root: PathBuf::from(DEFAULT_EVENT_LOG_ROOT),
    };

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Ok(CliCommand::Help(list_tasks_usage(program))),
            "--org-config" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --org-config".to_string()))?;
                parsed.org_config_path = PathBuf::from(value);
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                parsed.sqlite_path = PathBuf::from(value);
            }
            "--event-log-root" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --event-log-root".to_string())
                })?;
                parsed.event_log_root = PathBuf::from(value);
            }
            other => {
                return Err(MainError::Args(format!(
                    "unknown list-tasks argument: {other}\n\n{}",
                    list_tasks_usage(program)
                )));
            }
        }
        idx += 1;
    }

    Ok(CliCommand::ListTasks(parsed))
}

fn parse_enabled_models(raw: &str) -> Result<Vec<ModelKind>, MainError> {
    let mut models = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        models.push(parse_model_kind(token)?);
    }

    if models.is_empty() {
        return Err(MainError::Args(
            "--enable requires at least one model (claude,codex,gemini)".to_string(),
        ));
    }

    Ok(models)
}

fn parse_model_kind(value: &str) -> Result<ModelKind, MainError> {
    match value.to_ascii_lowercase().as_str() {
        "claude" => Ok(ModelKind::Claude),
        "codex" => Ok(ModelKind::Codex),
        "gemini" => Ok(ModelKind::Gemini),
        other => Err(MainError::Args(format!(
            "unknown model '{other}' (expected claude|codex|gemini)"
        ))),
    }
}

fn default_run_args() -> RunCliArgs {
    RunCliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        repos_config_dir: PathBuf::from(DEFAULT_REPOS_CONFIG_DIR),
        sqlite_path: PathBuf::from(DEFAULT_SQLITE_PATH),
        event_log_root: PathBuf::from(DEFAULT_EVENT_LOG_ROOT),
        once: false,
    }
}

fn usage(program: &str) -> String {
    format!(
        "Usage:\n  {program} [--org-config <path>] [--repos-config-dir <path>] [--sqlite-path <path>] [--event-log-root <path>] [--once]\n  {program} setup [--org-config <path>] [--enable <models>] [--per-model-concurrency <n>]\n  {program} create-task --spec <path> [--org-config <path>] [--repos-config-dir <path>] [--sqlite-path <path>] [--event-log-root <path>]\n  {program} list-tasks [--org-config <path>] [--sqlite-path <path>] [--event-log-root <path>]\n\
\nDefaults:\n  --org-config config/org.toml\n  --repos-config-dir config/repos\n  --sqlite-path .orch/state.sqlite\n  --event-log-root .orch/events"
    )
}

fn setup_usage(program: &str) -> String {
    format!(
        "Usage: {program} setup [--org-config <path>] [--enable <models>] [--per-model-concurrency <n>]\n\
\nExamples:\n  {program} setup\n  {program} setup --enable claude,codex --per-model-concurrency 5\n\
\nNotes:\n  --enable accepts comma-separated values from claude|codex|gemini\n  if --enable is omitted, all healthy detected models are selected"
    )
}

fn create_task_usage(program: &str) -> String {
    format!(
        "Usage: {program} create-task --spec <path> [--org-config <path>] [--repos-config-dir <path>] [--sqlite-path <path>] [--event-log-root <path>]\n\
\nNotes:\n  --spec expects JSON matching orch_core::types::TaskSpec"
    )
}

fn list_tasks_usage(program: &str) -> String {
    format!(
        "Usage: {program} list-tasks [--org-config <path>] [--sqlite-path <path>] [--event-log-root <path>]"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        create_task_usage, list_tasks_usage, load_repo_configs, parse_cli_args,
        parse_enabled_models, setup_usage, usage, CliCommand, CreateTaskCliArgs, ListTasksCliArgs,
        RunCliArgs, SetupCliArgs,
    };
    use orch_core::types::ModelKind;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn parse_cli_args_uses_defaults_when_no_flags_are_passed() {
        let parsed = parse_cli_args(Vec::new(), "orchd").expect("parse");
        assert_eq!(
            parsed,
            CliCommand::Run(RunCliArgs {
                org_config_path: PathBuf::from("config/org.toml"),
                repos_config_dir: PathBuf::from("config/repos"),
                sqlite_path: PathBuf::from(".orch/state.sqlite"),
                event_log_root: PathBuf::from(".orch/events"),
                once: false,
            })
        );
    }

    #[test]
    fn parse_cli_args_applies_explicit_paths_and_once_mode() {
        let parsed = parse_cli_args(
            vec![
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--repos-config-dir".to_string(),
                "/tmp/repos".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
                "--event-log-root".to_string(),
                "/tmp/events".to_string(),
                "--once".to_string(),
            ],
            "orchd",
        )
        .expect("parse");

        assert_eq!(
            parsed,
            CliCommand::Run(RunCliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                repos_config_dir: PathBuf::from("/tmp/repos"),
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                event_log_root: PathBuf::from("/tmp/events"),
                once: true,
            })
        );
    }

    #[test]
    fn parse_cli_args_reports_unknown_arguments_with_usage() {
        let err = parse_cli_args(vec!["--bad-flag".to_string()], "orchd")
            .expect_err("unknown arg should fail");
        let rendered = err.to_string();
        assert!(rendered.contains("unknown argument: --bad-flag"));
        assert!(rendered.contains("Usage:"));
    }

    #[test]
    fn parse_cli_args_requires_values_for_path_flags() {
        let err = parse_cli_args(vec!["--sqlite-path".to_string()], "orchd")
            .expect_err("missing sqlite path should fail");
        assert_eq!(err.to_string(), "missing value for --sqlite-path");

        let err = parse_cli_args(vec!["--org-config".to_string()], "orchd")
            .expect_err("missing org config should fail");
        assert_eq!(err.to_string(), "missing value for --org-config");

        let err = parse_cli_args(vec!["--event-log-root".to_string()], "orchd")
            .expect_err("missing event log root should fail");
        assert_eq!(err.to_string(), "missing value for --event-log-root");

        let err = parse_cli_args(vec!["--repos-config-dir".to_string()], "orchd")
            .expect_err("missing repos config dir should fail");
        assert_eq!(err.to_string(), "missing value for --repos-config-dir");
    }

    #[test]
    fn parse_cli_args_help_returns_usage_message() {
        let parsed = parse_cli_args(vec!["--help".to_string()], "orchd")
            .expect("help should return command");
        assert_eq!(parsed, CliCommand::Help(usage("orchd")));
    }

    #[test]
    fn parse_cli_args_setup_defaults() {
        let parsed = parse_cli_args(vec!["setup".to_string()], "orchd").expect("parse setup");
        assert_eq!(
            parsed,
            CliCommand::Setup(SetupCliArgs {
                org_config_path: PathBuf::from("config/org.toml"),
                enabled_models: None,
                per_model_concurrency: None,
            })
        );
    }

    #[test]
    fn parse_cli_args_setup_with_explicit_models_and_concurrency() {
        let parsed = parse_cli_args(
            vec![
                "setup".to_string(),
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--enable".to_string(),
                "claude,codex".to_string(),
                "--enable".to_string(),
                "gemini".to_string(),
                "--per-model-concurrency".to_string(),
                "7".to_string(),
            ],
            "orchd",
        )
        .expect("parse setup");

        assert_eq!(
            parsed,
            CliCommand::Setup(SetupCliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                enabled_models: Some(vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini]),
                per_model_concurrency: Some(7),
            })
        );
    }

    #[test]
    fn parse_cli_args_setup_help_returns_setup_usage() {
        let parsed = parse_cli_args(vec!["setup".to_string(), "--help".to_string()], "orchd")
            .expect("setup help");
        assert_eq!(parsed, CliCommand::Help(setup_usage("orchd")));
    }

    #[test]
    fn parse_cli_args_create_task_requires_spec() {
        let err = parse_cli_args(vec!["create-task".to_string()], "orchd")
            .expect_err("missing --spec should fail");
        assert_eq!(
            err.to_string(),
            "missing required --spec <path> for create-task"
        );
    }

    #[test]
    fn parse_cli_args_create_task_parses_paths() {
        let parsed = parse_cli_args(
            vec![
                "create-task".to_string(),
                "--spec".to_string(),
                "/tmp/task.json".to_string(),
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--repos-config-dir".to_string(),
                "/tmp/repos".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
                "--event-log-root".to_string(),
                "/tmp/events".to_string(),
            ],
            "orchd",
        )
        .expect("create-task parse");

        assert_eq!(
            parsed,
            CliCommand::CreateTask(CreateTaskCliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                repos_config_dir: PathBuf::from("/tmp/repos"),
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                event_log_root: PathBuf::from("/tmp/events"),
                spec_path: PathBuf::from("/tmp/task.json"),
            })
        );
    }

    #[test]
    fn parse_cli_args_create_task_help_returns_usage() {
        let parsed = parse_cli_args(
            vec!["create-task".to_string(), "--help".to_string()],
            "orchd",
        )
        .expect("create-task help");
        assert_eq!(parsed, CliCommand::Help(create_task_usage("orchd")));
    }

    #[test]
    fn parse_cli_args_list_tasks_defaults() {
        let parsed =
            parse_cli_args(vec!["list-tasks".to_string()], "orchd").expect("list-tasks parse");
        assert_eq!(
            parsed,
            CliCommand::ListTasks(ListTasksCliArgs {
                org_config_path: PathBuf::from("config/org.toml"),
                sqlite_path: PathBuf::from(".orch/state.sqlite"),
                event_log_root: PathBuf::from(".orch/events"),
            })
        );
    }

    #[test]
    fn parse_cli_args_list_tasks_overrides_paths() {
        let parsed = parse_cli_args(
            vec![
                "list-tasks".to_string(),
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
                "--event-log-root".to_string(),
                "/tmp/events".to_string(),
            ],
            "orchd",
        )
        .expect("list-tasks parse");
        assert_eq!(
            parsed,
            CliCommand::ListTasks(ListTasksCliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                event_log_root: PathBuf::from("/tmp/events"),
            })
        );
    }

    #[test]
    fn parse_cli_args_list_tasks_help_returns_usage() {
        let parsed = parse_cli_args(
            vec!["list-tasks".to_string(), "--help".to_string()],
            "orchd",
        )
        .expect("list-tasks help");
        assert_eq!(parsed, CliCommand::Help(list_tasks_usage("orchd")));
    }

    #[test]
    fn parse_enabled_models_rejects_unknown_model() {
        let err = parse_enabled_models("claude,unknown").expect_err("unknown model should fail");
        assert!(err.to_string().contains("unknown model 'unknown'"));
    }

    #[test]
    fn load_repo_configs_reads_only_toml_files_sorted_by_path() {
        let root = std::env::temp_dir().join(format!(
            "othala-orchd-load-repos-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create temp repo dir");
        fs::write(
            root.join("b.toml"),
            r#"
repo_id = "b"
repo_path = "/tmp/b"
base_branch = "main"

[nix]
dev_shell = "nix develop"

[verify.quick]
commands = ["nix develop -c just test"]

[verify.full]
commands = ["nix develop -c just test-all"]

[graphite]
draft_on_start = true
submit_mode = "single"
"#,
        )
        .expect("write b.toml");
        fs::write(
            root.join("a.toml"),
            r#"
repo_id = "a"
repo_path = "/tmp/a"
base_branch = "main"

[nix]
dev_shell = "nix develop"

[verify.quick]
commands = ["nix develop -c just test"]

[verify.full]
commands = ["nix develop -c just test-all"]

[graphite]
draft_on_start = true
submit_mode = "single"
"#,
        )
        .expect("write a.toml");
        fs::write(root.join("notes.txt"), "ignore me").expect("write non-toml");

        let loaded = load_repo_configs(&root).expect("load repo configs");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].1.repo_id, "a");
        assert_eq!(loaded[1].1.repo_id, "b");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_repo_configs_returns_empty_when_directory_is_missing() {
        let root = std::env::temp_dir().join(format!(
            "othala-orchd-missing-repos-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let loaded = load_repo_configs(&root).expect("missing dir should be treated as empty");
        assert!(loaded.is_empty());
    }
}
