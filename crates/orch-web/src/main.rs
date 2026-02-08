use orch_core::config::{load_org_config, ConfigError};
use orch_core::validation::{Validate, ValidationIssue, ValidationLevel};
use orch_web::{run_web_server, WebError, WebState};
use std::env;
use std::path::PathBuf;

const DEFAULT_ORG_CONFIG: &str = "config/org.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliArgs {
    org_config_path: PathBuf,
    bind_override: Option<String>,
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

    println!("orch-web binding to {bind}");
    run_web_server(&bind, WebState::default()).await?;
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
        "Usage: {program} [--org-config <path>] [--bind <ip:port>]\n\
Defaults:\n\
  --org-config {DEFAULT_ORG_CONFIG}\n\
  --bind from config.org.ui.web_bind"
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
            ],
            "orch-web",
        )
        .expect("parse");
        assert_eq!(
            parsed,
            CliCommand::Run(CliArgs {
                org_config_path: PathBuf::from("/tmp/org.toml"),
                bind_override: Some("0.0.0.0:9842".to_string()),
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
    fn parse_cli_args_requires_values_for_org_config_and_bind() {
        let err = parse_cli_args(vec!["--org-config".to_string()], "orch-web")
            .expect_err("missing org config");
        assert_eq!(err.to_string(), "missing value for --org-config");

        let err = parse_cli_args(vec!["--bind".to_string()], "orch-web").expect_err("missing bind");
        assert_eq!(err.to_string(), "missing value for --bind");
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
