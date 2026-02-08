use orch_core::state::VerifyTier;
use std::path::Path;

pub fn discover_verify_commands(repo_path: &Path, tier: VerifyTier) -> Vec<String> {
    let has_justfile = file_exists(repo_path, "Justfile") || file_exists(repo_path, "justfile");
    let has_cargo = file_exists(repo_path, "Cargo.toml");
    let has_node = file_exists(repo_path, "package.json");
    let has_python = file_exists(repo_path, "pyproject.toml")
        || file_exists(repo_path, "requirements.txt")
        || file_exists(repo_path, "requirements-dev.txt");

    if has_justfile {
        return just_commands(tier);
    }
    if has_cargo {
        return rust_commands(tier);
    }
    if has_node {
        return node_commands(tier);
    }
    if has_python {
        return python_commands(tier);
    }

    Vec::new()
}

pub fn resolve_verify_commands(
    repo_path: &Path,
    tier: VerifyTier,
    configured: &[String],
) -> Vec<String> {
    if !configured.is_empty() {
        return configured.to_vec();
    }
    discover_verify_commands(repo_path, tier)
}

fn just_commands(tier: VerifyTier) -> Vec<String> {
    match tier {
        VerifyTier::Quick => vec![
            "just fmt".to_string(),
            "just lint".to_string(),
            "just test".to_string(),
        ],
        VerifyTier::Full => vec!["just test-all".to_string()],
    }
}

fn rust_commands(tier: VerifyTier) -> Vec<String> {
    match tier {
        VerifyTier::Quick => vec![
            "cargo fmt --all -- --check".to_string(),
            "cargo clippy --workspace --all-targets -- -D warnings".to_string(),
            "cargo test --workspace".to_string(),
        ],
        VerifyTier::Full => vec!["cargo test --workspace --all-targets --all-features".to_string()],
    }
}

fn node_commands(tier: VerifyTier) -> Vec<String> {
    match tier {
        VerifyTier::Quick => vec![
            "npm run format:check --if-present".to_string(),
            "npm run lint --if-present".to_string(),
            "npm run test --if-present".to_string(),
        ],
        VerifyTier::Full => vec!["npm run test --if-present".to_string()],
    }
}

fn python_commands(tier: VerifyTier) -> Vec<String> {
    match tier {
        VerifyTier::Quick => vec![
            "python -m ruff check .".to_string(),
            "python -m pytest -q".to_string(),
        ],
        VerifyTier::Full => vec!["python -m pytest".to_string()],
    }
}

fn file_exists(repo_path: &Path, name: &str) -> bool {
    repo_path.join(name).exists()
}

#[cfg(test)]
mod tests {
    use super::{discover_verify_commands, resolve_verify_commands};
    use orch_core::state::VerifyTier;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "othala-verify-discover-{}-{}",
            name,
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn prefers_justfile_when_present() {
        let dir = tmp_dir("just");
        fs::write(dir.join("Justfile"), "fmt:\n\t@echo fmt\n").expect("write justfile");
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .expect("write cargo");

        let cmds = discover_verify_commands(&dir, VerifyTier::Quick);
        assert_eq!(
            cmds,
            vec![
                "just fmt".to_string(),
                "just lint".to_string(),
                "just test".to_string()
            ]
        );
    }

    #[test]
    fn uses_rust_conventions_for_cargo_repo() {
        let dir = tmp_dir("rust");
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .expect("write cargo");

        let quick = discover_verify_commands(&dir, VerifyTier::Quick);
        let full = discover_verify_commands(&dir, VerifyTier::Full);

        assert!(quick.iter().any(|x| x.contains("cargo clippy")));
        assert_eq!(
            full,
            vec!["cargo test --workspace --all-targets --all-features".to_string()]
        );
    }

    #[test]
    fn configured_commands_override_discovery() {
        let dir = tmp_dir("override");
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .expect("write cargo");

        let configured = vec!["nix develop -c just custom-check".to_string()];
        let resolved = resolve_verify_commands(&dir, VerifyTier::Quick, &configured);
        assert_eq!(resolved, configured);
    }
}
