//! Install Wizard v2 — comprehensive dependency checks, readiness scoring,
//! guided remediation, and non-interactive CI mode.

use orch_agents::setup::{probe_models, SetupProbeConfig};
use orch_core::config::load_org_config;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Command, Stdio};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single readiness check with weight, pass/fail, and remediation hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessCheck {
    pub name: String,
    pub passed: bool,
    pub critical: bool,
    pub weight: u32,
    pub detail: String,
    pub remediation: String,
}

/// Aggregated readiness report with a 0-100 score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessReport {
    pub checks: Vec<ReadinessCheck>,
    pub score: u32,
    pub all_critical_passed: bool,
    pub total_checks: usize,
    pub passed_checks: usize,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn command_ok(executable: &str) -> bool {
    Command::new(executable)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn which_ok(executable: &str) -> bool {
    Command::new("which")
        .arg(executable)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn dir_writable(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let probe = path.join(".othala_write_probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Check builders (one per check category)
// ---------------------------------------------------------------------------

fn check_git(repo_root: &Path) -> Vec<ReadinessCheck> {
    let installed = command_ok("git");
    let in_repo = if installed {
        Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(repo_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        false
    };
    vec![ReadinessCheck {
        name: "git".to_string(),
        passed: installed && in_repo,
        critical: true,
        weight: 10,
        detail: if !installed {
            "git not found on PATH".to_string()
        } else if !in_repo {
            "not inside a git repository".to_string()
        } else {
            "git available and inside a repository".to_string()
        },
        remediation: if !installed {
            "Install git: https://git-scm.com/downloads or nix-env -iA nixpkgs.git".to_string()
        } else if !in_repo {
            "Run from inside a git repository, or run: git init".to_string()
        } else {
            String::new()
        },
    }]
}

fn check_graphite() -> Vec<ReadinessCheck> {
    let found = which_ok("gt");
    vec![ReadinessCheck {
        name: "graphite_cli".to_string(),
        passed: found,
        critical: true,
        weight: 10,
        detail: if found {
            "gt found on PATH".to_string()
        } else {
            "gt not found on PATH".to_string()
        },
        remediation: if found {
            String::new()
        } else {
            "Install Graphite CLI: npm i -g @withgraphite/graphite-cli".to_string()
        },
    }]
}

fn check_cargo() -> Vec<ReadinessCheck> {
    let found = command_ok("cargo");
    vec![ReadinessCheck {
        name: "cargo".to_string(),
        passed: found,
        critical: true,
        weight: 10,
        detail: if found {
            "cargo available".to_string()
        } else {
            "cargo not found on PATH".to_string()
        },
        remediation: if found {
            String::new()
        } else {
            "Enter nix dev shell (nix develop) or install Rust: https://rustup.rs".to_string()
        },
    }]
}

fn check_nix() -> Vec<ReadinessCheck> {
    let found = which_ok("nix");
    vec![ReadinessCheck {
        name: "nix".to_string(),
        passed: found,
        critical: false,
        weight: 5,
        detail: if found {
            "nix available".to_string()
        } else {
            "nix not found on PATH".to_string()
        },
        remediation: if found {
            String::new()
        } else {
            "Install Nix: https://nixos.org/download.html (recommended for reproducible builds)"
                .to_string()
        },
    }]
}

fn check_gh() -> Vec<ReadinessCheck> {
    let found = which_ok("gh");
    vec![ReadinessCheck {
        name: "gh_cli".to_string(),
        passed: found,
        critical: false,
        weight: 5,
        detail: if found {
            "gh CLI available".to_string()
        } else {
            "gh CLI not found on PATH".to_string()
        },
        remediation: if found {
            String::new()
        } else {
            "Install GitHub CLI: https://cli.github.com/ or nix-env -iA nixpkgs.gh".to_string()
        },
    }]
}

fn check_othala_dir(repo_root: &Path) -> Vec<ReadinessCheck> {
    let othala_dir = repo_root.join(".othala");
    let exists = othala_dir.is_dir();
    vec![ReadinessCheck {
        name: "othala_directory".to_string(),
        passed: exists,
        critical: true,
        weight: 5,
        detail: if exists {
            format!("{} exists", othala_dir.display())
        } else {
            format!("{} missing", othala_dir.display())
        },
        remediation: if exists {
            String::new()
        } else {
            "Run: othala init".to_string()
        },
    }]
}

fn check_config(repo_root: &Path) -> Vec<ReadinessCheck> {
    let config_path = repo_root.join(".othala/config.toml");
    let (passed, detail, remediation) = if !config_path.exists() {
        (
            false,
            "config file missing".to_string(),
            "Run: othala wizard (or othala init) to generate config".to_string(),
        )
    } else {
        match load_org_config(&config_path) {
            Ok(_) => (
                true,
                "config parsed successfully".to_string(),
                String::new(),
            ),
            Err(err) => (
                false,
                format!("config parse failed: {err}"),
                "Fix syntax errors in .othala/config.toml or re-run: othala wizard".to_string(),
            ),
        }
    };
    vec![ReadinessCheck {
        name: "config_toml".to_string(),
        passed,
        critical: false,
        weight: 5,
        detail,
        remediation,
    }]
}

fn check_sqlite(repo_root: &Path) -> Vec<ReadinessCheck> {
    let db_path = repo_root.join(".othala/db.sqlite");
    let ok = db_path.is_file()
        && rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .is_ok();
    vec![ReadinessCheck {
        name: "sqlite".to_string(),
        passed: ok,
        critical: false,
        weight: 5,
        detail: if ok {
            format!("{} is readable", db_path.display())
        } else {
            format!("{} missing or unreadable", db_path.display())
        },
        remediation: if ok {
            String::new()
        } else {
            "Run: othala init  to create the database".to_string()
        },
    }]
}

fn check_context_paths(repo_root: &Path) -> Vec<ReadinessCheck> {
    let context_dir = repo_root.join(".othala/context");
    let exists = context_dir.is_dir();
    vec![ReadinessCheck {
        name: "context_directory".to_string(),
        passed: exists,
        critical: false,
        weight: 5,
        detail: if exists {
            format!("{} exists", context_dir.display())
        } else {
            format!("{} missing", context_dir.display())
        },
        remediation: if exists {
            String::new()
        } else {
            "Run: othala init  or mkdir -p .othala/context".to_string()
        },
    }]
}

fn check_models() -> Vec<ReadinessCheck> {
    let report = probe_models(&SetupProbeConfig::default());
    let any_installed = report.models.iter().any(|m| m.installed);

    let mut checks = vec![ReadinessCheck {
        name: "any_model_cli".to_string(),
        passed: any_installed,
        critical: true,
        weight: 15,
        detail: if any_installed {
            let names: Vec<_> = report
                .models
                .iter()
                .filter(|m| m.installed)
                .map(|m| m.executable.as_str())
                .collect();
            format!("model CLI(s) found: {}", names.join(", "))
        } else {
            "no model CLI found on PATH (need at least one of: claude, codex, gemini)".to_string()
        },
        remediation: if any_installed {
            String::new()
        } else {
            "Install at least one model CLI: claude, codex, or gemini".to_string()
        },
    }];

    // Check API keys for installed models
    let mut env_ok = true;
    let mut missing_keys = Vec::new();
    for probe in &report.models {
        if !probe.installed {
            continue;
        }
        for env_status in &probe.env_status {
            if !env_status.satisfied {
                env_ok = false;
                missing_keys.push(env_status.any_of.join(" or "));
            }
        }
    }

    checks.push(ReadinessCheck {
        name: "model_api_keys".to_string(),
        passed: env_ok,
        critical: false,
        weight: 10,
        detail: if env_ok {
            "API keys set for installed models".to_string()
        } else {
            format!("missing env vars: {}", missing_keys.join("; "))
        },
        remediation: if env_ok {
            String::new()
        } else {
            format!(
                "Export the required env vars: {}",
                missing_keys
                    .iter()
                    .map(|k| format!("export {k}=..."))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    });

    checks
}

fn check_permissions(repo_root: &Path) -> Vec<ReadinessCheck> {
    let othala_dir = repo_root.join(".othala");
    let writable = dir_writable(&othala_dir);
    let mut checks = vec![ReadinessCheck {
        name: "othala_dir_writable".to_string(),
        passed: writable,
        critical: false,
        weight: 5,
        detail: if writable {
            format!("{} is writable", othala_dir.display())
        } else {
            format!("{} is not writable or missing", othala_dir.display())
        },
        remediation: if writable {
            String::new()
        } else {
            format!(
                "Fix permissions: chmod -R u+w {} or run: othala init",
                othala_dir.display()
            )
        },
    }];

    let events_dir = repo_root.join(".othala/events");
    let events_ok = events_dir.is_dir();
    checks.push(ReadinessCheck {
        name: "events_directory".to_string(),
        passed: events_ok,
        critical: false,
        weight: 5,
        detail: if events_ok {
            format!("{} exists", events_dir.display())
        } else {
            format!("{} missing", events_dir.display())
        },
        remediation: if events_ok {
            String::new()
        } else {
            "Run: othala init  or mkdir -p .othala/events".to_string()
        },
    });

    let gt_auth = Command::new("gt")
        .args(["auth", "--token"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    // gt auth --token exits 0 only when authenticated; if gt is missing the check
    // just fails gracefully (which_ok already covers gt presence separately).
    checks.push(ReadinessCheck {
        name: "graphite_auth".to_string(),
        passed: gt_auth,
        critical: false,
        weight: 5,
        detail: if gt_auth {
            "Graphite CLI authenticated".to_string()
        } else {
            "Graphite CLI not authenticated or gt not available".to_string()
        },
        remediation: if gt_auth {
            String::new()
        } else {
            "Authenticate Graphite: gt auth --token <YOUR_TOKEN>".to_string()
        },
    });

    checks
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run all readiness checks and compute the aggregate score.
pub fn run_readiness_checks(repo_root: &Path) -> ReadinessReport {
    let mut checks = Vec::new();

    // Critical tools (~40)
    checks.extend(check_git(repo_root));
    checks.extend(check_graphite());
    checks.extend(check_cargo());
    checks.extend(check_nix());
    checks.extend(check_gh());

    // Configuration (~20)
    checks.extend(check_othala_dir(repo_root));
    checks.extend(check_config(repo_root));
    checks.extend(check_sqlite(repo_root));
    checks.extend(check_context_paths(repo_root));

    // Models (~25)
    checks.extend(check_models());

    // Permissions & environment (~15)
    checks.extend(check_permissions(repo_root));

    compute_report(checks)
}

/// Build a `ReadinessReport` from a Vec of checks (public so tests and
/// doctor/self-test refactors can construct reports from subsets).
pub fn compute_report(checks: Vec<ReadinessCheck>) -> ReadinessReport {
    let total_weight: u32 = checks.iter().map(|c| c.weight).sum();
    let earned: u32 = checks.iter().filter(|c| c.passed).map(|c| c.weight).sum();
    let score = if total_weight == 0 {
        100
    } else {
        ((earned as f64 / total_weight as f64) * 100.0).round() as u32
    };
    let all_critical_passed = checks.iter().all(|c| !c.critical || c.passed);
    let total_checks = checks.len();
    let passed_checks = checks.iter().filter(|c| c.passed).count();
    ReadinessReport {
        checks,
        score,
        all_critical_passed,
        total_checks,
        passed_checks,
    }
}

/// Print the readiness report to stdout/stderr.
pub fn print_readiness_report(report: &ReadinessReport, json: bool) {
    if json {
        let out =
            serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string());
        println!("{out}");
        return;
    }

    eprintln!("\x1b[35m── Readiness Report ──\x1b[0m\n");
    eprintln!(
        "{:<24} {:<8} {:<8} DETAIL",
        "CHECK", "STATUS", "WEIGHT"
    );
    eprintln!("{}", "-".repeat(80));

    for check in &report.checks {
        let (symbol, color) = if check.passed {
            ("\u{2713}", "\x1b[32m")
        } else {
            ("\u{2717}", "\x1b[31m")
        };
        let crit_tag = if check.critical { " [critical]" } else { "" };
        eprintln!(
            "  {color}{symbol}\x1b[0m {:<22} {:<8} {}{crit_tag}",
            check.name,
            check.weight,
            check.detail,
        );
        if !check.passed && !check.remediation.is_empty() {
            eprintln!(
                "     \x1b[33m↳ {}\x1b[0m",
                check.remediation
            );
        }
    }

    eprintln!();
    let score_color = if report.score >= 80 {
        "\x1b[32m"
    } else if report.score >= 50 {
        "\x1b[33m"
    } else {
        "\x1b[31m"
    };
    eprintln!(
        "Readiness score: {score_color}{}/100\x1b[0m  ({}/{} checks passed)",
        report.score, report.passed_checks, report.total_checks,
    );

    if report.all_critical_passed {
        eprintln!("\x1b[32mAll critical checks passed\x1b[0m");
    } else {
        eprintln!("\x1b[31mOne or more critical checks failed\x1b[0m");
    }
}

/// Returns true if the report is CI-pass-worthy (score >= 80 AND all critical pass).
pub fn is_ci_ready(report: &ReadinessReport) -> bool {
    report.score >= 80 && report.all_critical_passed
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_check(name: &str, passed: bool, critical: bool, weight: u32) -> ReadinessCheck {
        ReadinessCheck {
            name: name.to_string(),
            passed,
            critical,
            weight,
            detail: format!("{name} detail"),
            remediation: if passed {
                String::new()
            } else {
                format!("fix {name}")
            },
        }
    }

    #[test]
    fn test_readiness_score_computation() {
        let checks = vec![
            make_check("a", true, false, 60),
            make_check("b", false, false, 40),
        ];
        let report = compute_report(checks);
        assert_eq!(report.score, 60); // 60/100
        assert_eq!(report.total_checks, 2);
        assert_eq!(report.passed_checks, 1);
    }

    #[test]
    fn test_all_checks_pass_gives_100() {
        let checks = vec![
            make_check("a", true, true, 50),
            make_check("b", true, false, 50),
        ];
        let report = compute_report(checks);
        assert_eq!(report.score, 100);
        assert!(report.all_critical_passed);
    }

    #[test]
    fn test_critical_failure_flag() {
        let checks = vec![
            make_check("crit", false, true, 50),
            make_check("opt", true, false, 50),
        ];
        let report = compute_report(checks);
        assert!(!report.all_critical_passed);
        assert_eq!(report.score, 50);
    }

    #[test]
    fn test_json_output_format() {
        let checks = vec![make_check("git", true, true, 10)];
        let report = compute_report(checks);
        let json = serde_json::to_string_pretty(&report).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["score"], 100);
        assert_eq!(value["all_critical_passed"], true);
        assert!(value["checks"].is_array());
        assert_eq!(value["checks"][0]["name"], "git");
    }

    #[test]
    fn test_remediation_present_for_failures() {
        let checks = vec![
            make_check("fail1", false, true, 30),
            make_check("pass1", true, false, 70),
        ];
        let report = compute_report(checks);
        for check in &report.checks {
            if !check.passed {
                assert!(
                    !check.remediation.is_empty(),
                    "failing check '{}' should have remediation",
                    check.name
                );
            }
        }
    }

    #[test]
    fn test_empty_checks() {
        let report = compute_report(vec![]);
        assert_eq!(report.score, 100);
        assert!(report.all_critical_passed);
        assert_eq!(report.total_checks, 0);
        assert_eq!(report.passed_checks, 0);
    }

    #[test]
    fn test_is_ci_ready() {
        let good = compute_report(vec![
            make_check("a", true, true, 90),
            make_check("b", true, false, 10),
        ]);
        assert!(is_ci_ready(&good));

        let low_score = compute_report(vec![
            make_check("a", true, true, 30),
            make_check("b", false, false, 70),
        ]);
        assert!(!is_ci_ready(&low_score));

        let crit_fail = compute_report(vec![
            make_check("a", false, true, 10),
            make_check("b", true, false, 90),
        ]);
        assert!(!is_ci_ready(&crit_fail));
    }
}
