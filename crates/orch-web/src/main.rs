use orch_core::config::{load_org_config, ConfigError};
use orch_core::validation::{Validate, ValidationIssue, ValidationLevel};
use orch_web::{run_web_server, WebError, WebState};
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_ORG_CONFIG: &str = "config/org.toml";
const DEFAULT_SQLITE_PATH: &str = ".orch/state.sqlite";
const DEFAULT_SYNC_INTERVAL_MS: u64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliArgs {
    org_config_path: PathBuf,
    bind_override: Option<String>,
    sqlite_path: PathBuf,
    sync_interval_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliCommand {
    Run(CliArgs),
    Help(String),
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("{0}")]
    Args(String),
    #[error("failed to load org config at {path}: {source}")]
    LoadConfig {
        path: PathBuf,
        #[source]
        source: ConfigError,
    },
    #[error("{0}")]
    InvalidConfig(String),
    #[error(transparent)]
    Web(#[from] WebError),
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("orch-web failed: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), MainError> {
    let mut argv = env::args();
    let program = argv.next().unwrap_or_else(|| "orch-web".to_string());
    let command = parse_cli_args(argv.collect::<Vec<_>>(), &program)?;
    let CliCommand::Run(args) = command else {
        let CliCommand::Help(text) = command else {
            unreachable!();
        };
        println!("{text}");
        return Ok(());
    };

    let org = load_org_config(&args.org_config_path).map_err(|source| MainError::LoadConfig {
        path: args.org_config_path.clone(),
        source,
    })?;
    validate_org_config(&org.validate())?;
    let bind = resolve_bind(args.bind_override, &org.ui.web_bind)?;

    let state = WebState::default();
    if let Err(err) = sync_tasks_from_sqlite(&args.sqlite_path, &state).await {
        eprintln!("orch-web startup sync warning: {err}");
    }

    let sqlite_path = args.sqlite_path.clone();
    let interval = Duration::from_millis(args.sync_interval_ms);
    let state_for_sync = state.clone();
    tokio::spawn(async move {
        loop {
            if let Err(err) = sync_tasks_from_sqlite(&sqlite_path, &state_for_sync).await {
                eprintln!("orch-web task sync warning: {err}");
            }
            tokio::time::sleep(interval).await;
        }
    });

    println!("orch-web binding to {bind}");
    run_web_server(&bind, state).await?;
    Ok(())
}

async fn sync_tasks_from_sqlite(sqlite_path: &Path, state: &WebState) -> Result<(), String> {
    let sqlite_path = sqlite_path.to_path_buf();
    let tasks = tokio::task::spawn_blocking(move || {
        let store = orchd::SqliteStore::open(&sqlite_path).map_err(|err| err.to_string())?;
        store.migrate().map_err(|err| err.to_string())?;
        store.list_tasks().map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| err.to_string())??;

    state.replace_tasks(tasks).await;
    Ok(())
}

fn resolve_bind(bind_override: Option<String>, org_bind: &str) -> Result<String, MainError> {
    let candidate = bind_override.unwrap_or_else(|| org_bind.to_string());
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return Err(MainError::Args(
            "bind address must not be empty".to_string(),
        ));
    }
    Ok(trimmed.to_string())
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

fn parse_cli_args(args: Vec<String>, program: &str) -> Result<CliCommand, MainError> {
    let mut parsed = CliArgs {
        org_config_path: PathBuf::from(DEFAULT_ORG_CONFIG),
        bind_override: None,
        sqlite_path: PathBuf::from(DEFAULT_SQLITE_PATH),
        sync_interval_ms: DEFAULT_SYNC_INTERVAL_MS,
    };

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
            "--bind" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --bind".to_string()))?;
                parsed.bind_override = Some(value.clone());
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                parsed.sqlite_path = PathBuf::from(value);
            }
            "--sync-interval-ms" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sync-interval-ms".to_string())
                })?;
                let parsed_interval = value.parse::<u64>().map_err(|_| {
                    MainError::Args(format!(
                        "invalid --sync-interval-ms value: {value} (expected u64 > 0)"
                    ))
                })?;
                if parsed_interval == 0 {
                    return Err(MainError::Args(
                        "invalid --sync-interval-ms value: 0 (must be > 0)".to_string(),
                    ));
                }
                parsed.sync_interval_ms = parsed_interval;
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

fn usage(program: &str) -> String {
    format!(
        "Usage: {program} [--org-config <path>] [--bind <ip:port>] [--sqlite-path <path>] [--sync-interval-ms <u64>]\n\
Defaults:\n\
  --org-config {DEFAULT_ORG_CONFIG}\n\
  --bind from config.org.ui.web_bind\n\
  --sqlite-path {DEFAULT_SQLITE_PATH}\n\
  --sync-interval-ms {DEFAULT_SYNC_INTERVAL_MS}"
    )
}

#[cfg(test)]
mod tests {
    use super::{parse_cli_args, resolve_bind, usage, CliArgs, CliCommand};
    use std::path::PathBuf;

    #[test]
    fn parse_cli_args_uses_default_org_config_and_no_bind_override() {
        let parsed = parse_cli_args(Vec::new(), "orch-web").expect("parse");
        assert_eq!(
            parsed,
            CliCommand::Run(CliArgs {
                org_config_path: PathBuf::from("config/org.toml"),
                bind_override: None,
                sqlite_path: PathBuf::from(".orch/state.sqlite"),
                sync_interval_ms: 1000,
            })
        );
    }

    #[test]
    fn parse_cli_args_applies_org_config_and_bind_override() {
        let parsed = parse_cli_args(
            vec![
                "--org-config".to_string(),
                "/tmp/org.toml".to_string(),
                "--bind".to_string(),
                "0.0.0.0:9842".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
                "--sync-interval-ms".to_string(),
                "2000".to_string(),
            ],
            "orch-web",
        )
        .expect("parse");
        assert_eq!(
            parsed,
            CliCommand::Run(CliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                bind_override: Some("0.0.0.0:9842".to_string()),
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
                sync_interval_ms: 2000,
            })
        );
    }

    #[test]
    fn parse_cli_args_help_returns_help_command() {
        let parsed = parse_cli_args(vec!["--help".to_string()], "orch-web").expect("parse");
        assert_eq!(parsed, CliCommand::Help(usage("orch-web")));
    }

    #[test]
    fn parse_cli_args_reports_unknown_argument_with_usage() {
        let err = parse_cli_args(vec!["--bad".to_string()], "orch-web").expect_err("should fail");
        let rendered = err.to_string();
        assert!(rendered.contains("unknown argument: --bad"));
        assert!(rendered.contains("Usage: orch-web"));
    }

    #[test]
    fn parse_cli_args_requires_values_for_all_value_flags() {
        let err = parse_cli_args(vec!["--org-config".to_string()], "orch-web")
            .expect_err("missing org config");
        assert_eq!(err.to_string(), "missing value for --org-config");

        let err = parse_cli_args(vec!["--bind".to_string()], "orch-web").expect_err("missing bind");
        assert_eq!(err.to_string(), "missing value for --bind");

        let err = parse_cli_args(vec!["--sqlite-path".to_string()], "orch-web")
            .expect_err("missing sqlite path");
        assert_eq!(err.to_string(), "missing value for --sqlite-path");

        let err = parse_cli_args(vec!["--sync-interval-ms".to_string()], "orch-web")
            .expect_err("missing sync interval");
        assert_eq!(err.to_string(), "missing value for --sync-interval-ms");
    }

    #[test]
    fn parse_cli_args_validates_sync_interval_is_positive_u64() {
        let err = parse_cli_args(
            vec!["--sync-interval-ms".to_string(), "abc".to_string()],
            "orch-web",
        )
        .expect_err("invalid numeric value");
        assert_eq!(
            err.to_string(),
            "invalid --sync-interval-ms value: abc (expected u64 > 0)"
        );

        let err = parse_cli_args(
            vec!["--sync-interval-ms".to_string(), "0".to_string()],
            "orch-web",
        )
        .expect_err("zero interval");
        assert_eq!(
            err.to_string(),
            "invalid --sync-interval-ms value: 0 (must be > 0)"
        );
    }

    #[test]
    fn resolve_bind_prefers_override_and_rejects_blank_values() {
        let resolved = resolve_bind(Some("127.0.0.1:9999".to_string()), "127.0.0.1:9842")
            .expect("resolve bind");
        assert_eq!(resolved, "127.0.0.1:9999");

        let resolved = resolve_bind(None, "127.0.0.1:9842").expect("resolve fallback");
        assert_eq!(resolved, "127.0.0.1:9842");

        let err = resolve_bind(Some("   ".to_string()), "127.0.0.1:9842")
            .expect_err("blank override should fail");
        assert_eq!(err.to_string(), "bind address must not be empty");
    }
}
