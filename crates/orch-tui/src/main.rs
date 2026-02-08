use orch_tui::{run_tui, TuiApp, TuiError};
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_TICK_MS: u64 = 250;
const DEFAULT_SQLITE_PATH: &str = ".orch/state.sqlite";

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliArgs {
    tick_ms: u64,
    sqlite_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("{0}")]
    Args(String),
    #[error(transparent)]
    Tui(#[from] TuiError),
}

fn main() {
    if let Err(err) = run() {
        eprintln!("orch-tui failed: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), MainError> {
    let mut argv = env::args();
    let program = argv.next().unwrap_or_else(|| "orch-tui".to_string());
    let args = parse_cli_args(argv.collect::<Vec<_>>(), &program)?;

    let mut app = match load_tasks_from_sqlite(&args.sqlite_path) {
        Ok(tasks) => {
            let mut app = TuiApp::from_tasks(&tasks);
            app.state.status_line = format!(
                "orch-tui started tick_ms={} tasks={}",
                args.tick_ms,
                tasks.len()
            );
            app
        }
        Err(err) => {
            let mut app = TuiApp::default();
            app.state.status_line = format!(
                "orch-tui started tick_ms={} task_load_warning={}",
                args.tick_ms, err
            );
            app
        }
    };
    run_tui(&mut app, Duration::from_millis(args.tick_ms))?;
    Ok(())
}

fn load_tasks_from_sqlite(path: &Path) -> Result<Vec<orch_core::types::Task>, String> {
    let store = orchd::SqliteStore::open(path).map_err(|err| err.to_string())?;
    store.migrate().map_err(|err| err.to_string())?;
    store.list_tasks().map_err(|err| err.to_string())
}

fn parse_cli_args(args: Vec<String>, program: &str) -> Result<CliArgs, MainError> {
    let mut tick_ms = DEFAULT_TICK_MS;
    let mut sqlite_path = PathBuf::from(DEFAULT_SQLITE_PATH);
    let mut idx = 0usize;

    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--help" | "-h" => return Err(MainError::Args(usage(program))),
            "--tick-ms" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| MainError::Args("missing value for --tick-ms".to_string()))?;
                tick_ms = value.parse::<u64>().map_err(|_| {
                    MainError::Args(format!("invalid --tick-ms value: {value} (expected u64)"))
                })?;
                if tick_ms == 0 {
                    return Err(MainError::Args(
                        "invalid --tick-ms value: 0 (must be > 0)".to_string(),
                    ));
                }
            }
            "--sqlite-path" => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    MainError::Args("missing value for --sqlite-path".to_string())
                })?;
                sqlite_path = PathBuf::from(value);
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

    Ok(CliArgs {
        tick_ms,
        sqlite_path,
    })
}

fn usage(program: &str) -> String {
    format!(
        "Usage: {program} [--tick-ms <u64>] [--sqlite-path <path>]\n\
Defaults:\n\
  --tick-ms {DEFAULT_TICK_MS}\n\
  --sqlite-path {DEFAULT_SQLITE_PATH}"
    )
}

#[cfg(test)]
mod tests {
    use super::{parse_cli_args, usage, CliArgs};
    use std::path::PathBuf;

    #[test]
    fn parse_cli_args_uses_default_tick_rate() {
        let parsed = parse_cli_args(Vec::new(), "orch-tui").expect("parse");
        assert_eq!(
            parsed,
            CliArgs {
                tick_ms: 250,
                sqlite_path: PathBuf::from(".orch/state.sqlite"),
            }
        );
    }

    #[test]
    fn parse_cli_args_applies_tick_rate_and_sqlite_override() {
        let parsed = parse_cli_args(
            vec![
                "--tick-ms".to_string(),
                "500".to_string(),
                "--sqlite-path".to_string(),
                "/tmp/state.sqlite".to_string(),
            ],
            "orch-tui",
        )
        .expect("parse");
        assert_eq!(
            parsed,
            CliArgs {
                tick_ms: 500,
                sqlite_path: PathBuf::from("/tmp/state.sqlite"),
            }
        );
    }

    #[test]
    fn parse_cli_args_rejects_missing_tick_rate_value() {
        let err =
            parse_cli_args(vec!["--tick-ms".to_string()], "orch-tui").expect_err("should fail");
        assert_eq!(err.to_string(), "missing value for --tick-ms");

        let err =
            parse_cli_args(vec!["--sqlite-path".to_string()], "orch-tui").expect_err("should fail");
        assert_eq!(err.to_string(), "missing value for --sqlite-path");
    }

    #[test]
    fn parse_cli_args_rejects_invalid_tick_rate_values() {
        let err = parse_cli_args(vec!["--tick-ms".to_string(), "abc".to_string()], "orch-tui")
            .expect_err("should fail");
        assert_eq!(
            err.to_string(),
            "invalid --tick-ms value: abc (expected u64)"
        );

        let err = parse_cli_args(vec!["--tick-ms".to_string(), "0".to_string()], "orch-tui")
            .expect_err("should fail");
        assert_eq!(err.to_string(), "invalid --tick-ms value: 0 (must be > 0)");
    }

    #[test]
    fn parse_cli_args_help_returns_usage() {
        let err = parse_cli_args(vec!["--help".to_string()], "orch-tui").expect_err("help path");
        assert_eq!(err.to_string(), usage("orch-tui"));
    }
}
