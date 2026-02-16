use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiConfig {
    pub trigger_on_push: bool,
    pub trigger_on_pr: bool,
    pub branches: Vec<String>,
    pub verify_command: String,
    pub nix_enabled: bool,
    pub graphite_enabled: bool,
    pub cache_enabled: bool,
}

impl Default for CiConfig {
    fn default() -> Self {
        Self {
            trigger_on_push: true,
            trigger_on_pr: true,
            branches: vec!["main".to_string()],
            verify_command: "cargo test --workspace".to_string(),
            nix_enabled: true,
            graphite_enabled: false,
            cache_enabled: true,
        }
    }
}

pub fn generate_github_actions(config: &CiConfig) -> String {
    if config.nix_enabled {
        generate_nix_ci(config)
    } else {
        generate_basic_ci(config)
    }
}

pub fn generate_verify_workflow(config: &CiConfig) -> String {
    let mut output = String::new();
    output.push_str("name: Othala Verify\n\n");
    output.push_str(&render_triggers(config));
    output.push('\n');
    output.push_str("jobs:\n");
    output.push_str("  verify_only:\n");
    output.push_str("    name: Verify Only\n");
    output.push_str("    runs-on: ubuntu-latest\n");
    output.push_str("    steps:\n");
    output.push_str("      - name: Checkout\n");
    output.push_str("        uses: actions/checkout@v4\n");
    if config.nix_enabled {
        output.push_str("      - name: Install Nix\n");
        output.push_str("        uses: cachix/install-nix-action@v31\n");
        if config.cache_enabled {
            output.push_str("      - name: Cache Rust artifacts\n");
            output.push_str("        uses: Swatinem/rust-cache@v2\n");
        }
        output.push_str("      - name: Run verify command\n");
        output.push_str("        run: |");
        output.push('\n');
        output.push_str("          nix develop --command bash -c \"");
        output.push_str(&escape_for_double_quotes(&config.verify_command));
        output.push_str("\"\n");
    } else {
        output.push_str("      - name: Setup Rust\n");
        output.push_str("        uses: dtolnay/rust-toolchain@stable\n");
        if config.cache_enabled {
            output.push_str("      - name: Cache Rust artifacts\n");
            output.push_str("        uses: Swatinem/rust-cache@v2\n");
        }
        output.push_str("      - name: Run verify command\n");
        output.push_str("        run: ");
        output.push_str(&config.verify_command);
        output.push('\n');
    }
    output
}

pub fn generate_nix_ci(config: &CiConfig) -> String {
    let mut output = String::new();
    output.push_str("name: Othala CI\n\n");
    output.push_str(&render_triggers(config));
    output.push('\n');
    output.push_str("jobs:\n");
    output.push_str("  verify:\n");
    output.push_str("    name: Build, Test, and Lint\n");
    output.push_str("    runs-on: ubuntu-latest\n");
    output.push_str("    steps:\n");
    output.push_str("      - name: Checkout\n");
    output.push_str("        uses: actions/checkout@v4\n");
    output.push_str("      - name: Install Nix\n");
    output.push_str("        uses: cachix/install-nix-action@v31\n");

    if config.cache_enabled {
        output.push_str("      - name: Cache Rust artifacts\n");
        output.push_str("        uses: Swatinem/rust-cache@v2\n");
    }

    if config.graphite_enabled {
        output.push_str("      - name: Graphite enabled\n");
        output.push_str("        run: echo \"graphite integration enabled\"\n");
    }

    output.push_str("      - name: Build\n");
    output.push_str("        run: |");
    output.push('\n');
    output.push_str("          nix develop --command bash -c \"cargo build -p orchd --locked\"\n");

    output.push_str("      - name: Test\n");
    output.push_str("        run: |");
    output.push('\n');
    output.push_str("          nix develop --command bash -c \"");
    output.push_str(&escape_for_double_quotes(&config.verify_command));
    output.push_str("\"\n");

    output.push_str("      - name: Clippy\n");
    output.push_str("        run: |");
    output.push('\n');
    output.push_str("          nix develop --command bash -c \"cargo clippy -p orchd -- -D warnings\"\n");
    output
}

pub fn generate_basic_ci(config: &CiConfig) -> String {
    let mut output = String::new();
    output.push_str("name: Othala CI\n\n");
    output.push_str(&render_triggers(config));
    output.push('\n');
    output.push_str("jobs:\n");
    output.push_str("  verify:\n");
    output.push_str("    name: Build, Test, and Lint\n");
    output.push_str("    runs-on: ubuntu-latest\n");
    output.push_str("    steps:\n");
    output.push_str("      - name: Checkout\n");
    output.push_str("        uses: actions/checkout@v4\n");
    output.push_str("      - name: Setup Rust\n");
    output.push_str("        uses: dtolnay/rust-toolchain@stable\n");

    if config.cache_enabled {
        output.push_str("      - name: Cache Rust artifacts\n");
        output.push_str("        uses: Swatinem/rust-cache@v2\n");
    }

    if config.graphite_enabled {
        output.push_str("      - name: Graphite enabled\n");
        output.push_str("        run: echo \"graphite integration enabled\"\n");
    }

    output.push_str("      - name: Build\n");
    output.push_str("        run: cargo build -p orchd --locked\n");
    output.push_str("      - name: Test\n");
    output.push_str("        run: ");
    output.push_str(&config.verify_command);
    output.push('\n');
    output.push_str("      - name: Clippy\n");
    output.push_str("        run: cargo clippy -p orchd -- -D warnings\n");
    output
}

pub fn validate_workflow(content: &str) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    if !content.contains("name:") {
        errors.push("missing workflow name".to_string());
    }
    if !content.contains("on:") {
        errors.push("missing trigger section".to_string());
    }
    if !content.contains("jobs:") {
        errors.push("missing jobs section".to_string());
    }
    if !content.contains("actions/checkout@") {
        errors.push("missing checkout step".to_string());
    }
    if !content.contains("runs-on:") {
        errors.push("missing runs-on key".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn render_triggers(config: &CiConfig) -> String {
    let mut lines = Vec::new();
    lines.push("on:".to_string());

    if config.trigger_on_push {
        lines.push("  push:".to_string());
        lines.push("    branches:".to_string());
        for branch in &config.branches {
            lines.push(format!("      - {branch}"));
        }
    }

    if config.trigger_on_pr {
        lines.push("  pull_request:".to_string());
        lines.push("    branches:".to_string());
        for branch in &config.branches {
            lines.push(format!("      - {branch}"));
        }
    }

    if !config.trigger_on_push && !config.trigger_on_pr {
        lines.push("  workflow_dispatch:".to_string());
    }

    lines.join("\n") + "\n"
}

fn escape_for_double_quotes(input: &str) -> String {
    input.replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::{
        generate_basic_ci, generate_github_actions, generate_nix_ci, generate_verify_workflow,
        validate_workflow, CiConfig,
    };

    #[test]
    fn default_config() {
        let config = CiConfig::default();
        assert!(config.trigger_on_push);
        assert!(config.trigger_on_pr);
        assert_eq!(config.branches, vec!["main".to_string()]);
        assert_eq!(config.verify_command, "cargo test --workspace");
        assert!(config.nix_enabled);
        assert!(!config.graphite_enabled);
        assert!(config.cache_enabled);
    }

    #[test]
    fn generate_github_actions_contains_required_keys() {
        let config = CiConfig::default();
        let workflow = generate_github_actions(&config);

        assert!(workflow.contains("name: Othala CI"));
        assert!(workflow.contains("on:"));
        assert!(workflow.contains("jobs:"));
        assert!(workflow.contains("actions/checkout@v4"));
        assert!(workflow.contains("Build"));
        assert!(workflow.contains("Test"));
        assert!(workflow.contains("Clippy"));
    }

    #[test]
    fn generate_verify_workflow_is_valid() {
        let config = CiConfig::default();
        let workflow = generate_verify_workflow(&config);
        assert!(validate_workflow(&workflow).is_ok());
        assert!(workflow.contains("Verify Only"));
    }

    #[test]
    fn generate_nix_ci_includes_nix_setup() {
        let config = CiConfig::default();
        let workflow = generate_nix_ci(&config);

        assert!(workflow.contains("cachix/install-nix-action"));
        assert!(workflow.contains("nix develop --command"));
    }

    #[test]
    fn generate_basic_ci_excludes_nix() {
        let config = CiConfig {
            nix_enabled: false,
            ..CiConfig::default()
        };
        let workflow = generate_basic_ci(&config);

        assert!(!workflow.contains("install-nix-action"));
        assert!(!workflow.contains("nix develop --command"));
        assert!(workflow.contains("dtolnay/rust-toolchain@stable"));
    }

    #[test]
    fn validate_workflow_catches_missing_fields() {
        let invalid = "name: broken";
        let result = validate_workflow(invalid);
        assert!(result.is_err());
        let errors = result.expect_err("must fail");
        assert!(errors.iter().any(|err| err.contains("missing trigger section")));
        assert!(errors.iter().any(|err| err.contains("missing jobs section")));
    }

    #[test]
    fn cache_enabled_affects_output() {
        let with_cache = generate_nix_ci(&CiConfig {
            cache_enabled: true,
            ..CiConfig::default()
        });
        let without_cache = generate_nix_ci(&CiConfig {
            cache_enabled: false,
            ..CiConfig::default()
        });

        assert!(with_cache.contains("Swatinem/rust-cache@v2"));
        assert!(!without_cache.contains("Swatinem/rust-cache@v2"));
    }

    #[test]
    fn custom_branches_in_config() {
        let config = CiConfig {
            branches: vec!["main".to_string(), "develop".to_string()],
            ..CiConfig::default()
        };
        let workflow = generate_github_actions(&config);

        assert!(workflow.contains("- main"));
        assert!(workflow.contains("- develop"));
    }
}
