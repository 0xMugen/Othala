use orch_core::config::{load_org_config, ConfigError};
use orch_core::validation::{Validate, ValidationIssue, ValidationLevel};
use orchd::{OrchdService, Scheduler, SchedulerConfig, ServiceError};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const DEFAULT_ORG_CONFIG: &str = "config/org.toml";
const DEFAULT_SQLITE_PATH: &str = ".orch/state.sqlite";
const DEFAULT_EVENT_LOG_ROOT: &str = ".orch/events";

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliArgs {
    org_config_path: PathBuf,
    sqlite_path: PathBuf,
    event_log_root: PathBuf,
    once: bool,
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
    #[error("{0}")]
    InvalidConfig(String),
    #[error(transparent)]
    Service(#[from] ServiceError),
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
    let args = parse_cli_args(argv.collect::<Vec<_>>(), &program)?;

    ensure_parent_dir(&args.sqlite_path)?;
    ensure_dir(&args.event_log_root)?;

    let org = load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
        path: args.org_config_path.clone(),
        source,
    })?;
    validate_org_config(&org.validate())?;

    let scheduler = Scheduler::new(SchedulerConfig::from_org_config(&org));
    let service = OrchdService::open(&args.sqlite_path, &args.event_log_root, scheduler)?;

    let task_count = service.list_tasks()?.len();
    println!(
        "orchd bootstrapped sqlite={} event_log_root={} tasks={}",
        args.sqlite_path.display(),
        args.event_log_root.display(),
        task_count
    );

    if args.once {
        println!("orchd exiting after bootstrap (--once)");
        return Ok(());
    }

    println!("orchd running; press Ctrl+C to stop");
    loop {
        thread::sleep(Duration::from_secs(60));
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

fn parse_cli_args(args: Vec<String>, program: &str) -> Result<CliArgs, MainError> {
    let mut parsed = CliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        sqlite_path: PathBuf::from(DEFAULT_SQLITE_PATH),
        event_log_root: PathBuf::from(DEFAULT_EVENT_LOG_ROOT),
        once: false,
    };

    let mut idx = 0usize;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => {
                return Err(MainError::Args(usage(program)));
            }
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

    Ok(parsed)
}

fn usage(program: &str) -> String {
    format!(
        "Usage: {program} [--org-config <path>] [--sqlite-path <path>] [--event-log-root <path>] [--once]\n\
Defaults:\n\
  --org-config config/org.toml\n\
  --sqlite-path .orch/state.sqlite\n\
  --event-log-root .orch/events"
    )
}

#[cfg(test)]
mod tests {
    use super::{parse_cli_args, usage, CliArgs};
    use std::path::PathBuf;

    #[test]
    fn parse_cli_args_uses_defaults_when_no_flags_are_passed() {
        let parsed = parse_cli_args(Vec::new(), "orchd").expect("parse");
        assert_eq!(
            parsed,
            CliArgs {
                org_config_path: PathBuf::from("config/org.toml"),
                sqlite_path: PathBuf::from(".orch/state.sqlite"),
                event_log_root: PathBuf::from(".orch/events"),
                once: false,
            }
        );
    }

    #[test]
    fn parse_cli_args_applies_explicit_paths_and_once_mode() {
        let parsed = parse_cli_args(
            vec![
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
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
            CliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                event_log_root: PathBuf::from("/tmp/events"),
                once: true,
            }
        );
    }

    #[test]
    fn parse_cli_args_reports_unknown_arguments_with_usage() {
        let err = parse_cli_args(vec!["--bad-flag".to_string()], "orchd")
            .expect_err("unknown arg should fail");
        let rendered = err.to_string();
        assert!(rendered.contains("unknown argument: --bad-flag"));
        assert!(rendered.contains("Usage: orchd"));
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
    }

    #[test]
    fn parse_cli_args_help_returns_usage_message() {
        let err = parse_cli_args(vec!["--help".to_string()], "orchd")
            .expect_err("help should produce usage output");
        assert_eq!(err.to_string(), usage("orchd"));
    }
}
