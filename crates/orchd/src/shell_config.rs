use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellConfig {
    pub path: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub working_dir: Option<String>,
    pub timeout_secs: u64,
    pub inherit_env: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            path: "/bin/bash".to_string(),
            args: vec!["-c".to_string()],
            env: HashMap::new(),
            working_dir: None,
            timeout_secs: 300,
            inherit_env: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellPreset {
    Bash,
    Zsh,
    Fish,
    Sh,
    Nix,
    Custom(ShellConfig),
}

impl ShellPreset {
    pub fn to_config(&self) -> ShellConfig {
        match self {
            ShellPreset::Bash => ShellConfig::default(),
            ShellPreset::Zsh => ShellConfig {
                path: "/bin/zsh".to_string(),
                args: vec!["-c".to_string()],
                ..ShellConfig::default()
            },
            ShellPreset::Fish => ShellConfig {
                path: "fish".to_string(),
                args: vec!["-c".to_string()],
                ..ShellConfig::default()
            },
            ShellPreset::Sh => ShellConfig {
                path: "/bin/sh".to_string(),
                args: vec!["-c".to_string()],
                ..ShellConfig::default()
            },
            ShellPreset::Nix => ShellConfig {
                path: "nix".to_string(),
                args: vec![
                    "develop".to_string(),
                    "--command".to_string(),
                    "bash".to_string(),
                    "-c".to_string(),
                ],
                ..ShellConfig::default()
            },
            ShellPreset::Custom(config) => config.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u64,
}

impl ShellOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("Shell binary not found: {0}")]
    NotFound(String),
    #[error("Shell command timed out after {0} seconds")]
    Timeout(u64),
    #[error("Shell execution failed: {0}")]
    ExecutionFailed(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    #[default]
    Medium,
    High,
}

impl ReasoningEffort {
    pub fn as_str(&self) -> &str {
        match self {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelOptions {
    pub reasoning_effort: Option<ReasoningEffort>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub top_p: Option<f64>,
    pub stop_sequences: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellRunner {
    pub config: ShellConfig,
}

impl ShellRunner {
    pub fn new(config: ShellConfig) -> Self {
        Self { config }
    }

    pub fn from_preset(preset: ShellPreset) -> Self {
        Self {
            config: preset.to_config(),
        }
    }

    pub fn run_command(&self, command: &str) -> Result<ShellOutput, ShellError> {
        self.run_command_internal(command, None, None)
    }

    pub fn run_command_with_env(
        &self,
        command: &str,
        extra_env: &HashMap<String, String>,
    ) -> Result<ShellOutput, ShellError> {
        self.run_command_internal(command, Some(extra_env), None)
    }

    pub fn run_command_with_stdin(
        &self,
        command: &str,
        stdin: &str,
    ) -> Result<ShellOutput, ShellError> {
        self.run_command_internal(command, None, Some(stdin))
    }

    pub fn detect_shell() -> ShellPreset {
        let shell = std::env::var("SHELL").unwrap_or_default().to_lowercase();
        if shell.contains("zsh") {
            ShellPreset::Zsh
        } else if shell.contains("fish") {
            ShellPreset::Fish
        } else if shell.contains("bash") {
            ShellPreset::Bash
        } else if shell.contains("sh") {
            ShellPreset::Sh
        } else {
            ShellPreset::Bash
        }
    }

    pub fn validate_shell(path: &str) -> bool {
        binary_exists(path)
    }

    fn run_command_internal(
        &self,
        command: &str,
        extra_env: Option<&HashMap<String, String>>,
        stdin: Option<&str>,
    ) -> Result<ShellOutput, ShellError> {
        if !Self::validate_shell(&self.config.path) {
            return Err(ShellError::NotFound(self.config.path.clone()));
        }

        let mut process = Command::new(&self.config.path);
        process
            .args(&self.config.args)
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if stdin.is_some() {
            process.stdin(Stdio::piped());
        }

        if !self.config.inherit_env {
            process.env_clear();
        }

        if let Some(working_dir) = &self.config.working_dir {
            process.current_dir(working_dir);
        }

        for (key, value) in &self.config.env {
            process.env(key, value);
        }

        if let Some(extra_env) = extra_env {
            for (key, value) in extra_env {
                process.env(key, value);
            }
        }

        let start = Instant::now();
        let mut child = process.spawn()?;

        if let Some(stdin_content) = stdin {
            if let Some(mut child_stdin) = child.stdin.take() {
                child_stdin.write_all(stdin_content.as_bytes())?;
            }
        }

        let timeout = Duration::from_secs(self.config.timeout_secs);
        loop {
            if let Some(status) = child.try_wait()? {
                let output = child.wait_with_output()?;
                let duration_ms = start.elapsed().as_millis() as u64;
                return Ok(ShellOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    exit_code: status.code().unwrap_or(-1),
                    duration_ms,
                });
            }

            if start.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ShellError::Timeout(self.config.timeout_secs));
            }

            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn binary_exists(path: &str) -> bool {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return candidate.exists();
    }

    if path.contains(std::path::MAIN_SEPARATOR) {
        return candidate.exists();
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join(path).exists() {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_config_defaults_match_expected_values() {
        let config = ShellConfig::default();
        assert_eq!(config.path, "/bin/bash");
        assert_eq!(config.args, vec!["-c"]);
        assert!(config.env.is_empty());
        assert_eq!(config.working_dir, None);
        assert_eq!(config.timeout_secs, 300);
        assert!(config.inherit_env);
    }

    #[test]
    fn shell_preset_bash_to_config() {
        let config = ShellPreset::Bash.to_config();
        assert_eq!(config.path, "/bin/bash");
        assert_eq!(config.args, vec!["-c"]);
    }

    #[test]
    fn shell_preset_zsh_to_config() {
        let config = ShellPreset::Zsh.to_config();
        assert_eq!(config.path, "/bin/zsh");
        assert_eq!(config.args, vec!["-c"]);
    }

    #[test]
    fn shell_preset_fish_to_config() {
        let config = ShellPreset::Fish.to_config();
        assert_eq!(config.path, "fish");
        assert_eq!(config.args, vec!["-c"]);
    }

    #[test]
    fn shell_preset_sh_to_config() {
        let config = ShellPreset::Sh.to_config();
        assert_eq!(config.path, "/bin/sh");
        assert_eq!(config.args, vec!["-c"]);
    }

    #[test]
    fn shell_preset_nix_to_config() {
        let config = ShellPreset::Nix.to_config();
        assert_eq!(config.path, "nix");
        assert_eq!(
            config.args,
            vec!["develop", "--command", "bash", "-c"]
        );
    }

    #[test]
    fn shell_preset_custom_to_config_returns_clone() {
        let custom = ShellConfig {
            path: "/usr/bin/env".to_string(),
            args: vec!["bash".to_string(), "-c".to_string()],
            env: HashMap::from([("A".to_string(), "B".to_string())]),
            working_dir: Some("/tmp".to_string()),
            timeout_secs: 10,
            inherit_env: false,
        };

        let config = ShellPreset::Custom(custom.clone()).to_config();
        assert_eq!(config, custom);
    }

    #[test]
    fn shell_runner_new_stores_config() {
        let config = ShellConfig::default();
        let runner = ShellRunner::new(config.clone());
        assert_eq!(runner.config, config);
    }

    #[test]
    fn shell_runner_from_preset_uses_config() {
        let runner = ShellRunner::from_preset(ShellPreset::Sh);
        assert_eq!(runner.config.path, "/bin/sh");
        assert_eq!(runner.config.args, vec!["-c"]);
    }

    #[test]
    fn detect_shell_maps_bash() {
        let old = std::env::var("SHELL").ok();
        unsafe {
            std::env::set_var("SHELL", "/bin/bash");
        }
        assert_eq!(ShellRunner::detect_shell(), ShellPreset::Bash);
        restore_shell(old);
    }

    #[test]
    fn detect_shell_maps_zsh() {
        let old = std::env::var("SHELL").ok();
        unsafe {
            std::env::set_var("SHELL", "/usr/local/bin/zsh");
        }
        assert_eq!(ShellRunner::detect_shell(), ShellPreset::Zsh);
        restore_shell(old);
    }

    #[test]
    fn detect_shell_maps_fish() {
        let old = std::env::var("SHELL").ok();
        unsafe {
            std::env::set_var("SHELL", "/usr/bin/fish");
        }
        assert_eq!(ShellRunner::detect_shell(), ShellPreset::Fish);
        restore_shell(old);
    }

    #[test]
    fn detect_shell_maps_sh() {
        let old = std::env::var("SHELL").ok();
        unsafe {
            std::env::set_var("SHELL", "/bin/sh");
        }
        assert_eq!(ShellRunner::detect_shell(), ShellPreset::Sh);
        restore_shell(old);
    }

    #[test]
    fn detect_shell_defaults_to_bash() {
        let old = std::env::var("SHELL").ok();
        unsafe {
            std::env::remove_var("SHELL");
        }
        assert_eq!(ShellRunner::detect_shell(), ShellPreset::Bash);
        restore_shell(old);
    }

    #[test]
    fn validate_shell_returns_true_for_existing_absolute_binary() {
        assert!(ShellRunner::validate_shell("/bin/sh"));
    }

    #[test]
    fn validate_shell_returns_false_for_missing_binary() {
        assert!(!ShellRunner::validate_shell("definitely-not-a-real-shell-binary"));
    }

    #[test]
    fn shell_output_success_true_for_zero_exit_code() {
        let output = ShellOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 1,
        };
        assert!(output.success());
    }

    #[test]
    fn shell_output_success_false_for_non_zero_exit_code() {
        let output = ShellOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 1,
            duration_ms: 1,
        };
        assert!(!output.success());
    }

    #[test]
    fn shell_error_display_messages_are_readable() {
        let not_found = ShellError::NotFound("/missing".to_string());
        assert!(not_found.to_string().contains("Shell binary not found"));

        let timeout = ShellError::Timeout(15);
        assert!(timeout.to_string().contains("15"));

        let failed = ShellError::ExecutionFailed("boom".to_string());
        assert!(failed.to_string().contains("boom"));
    }

    #[test]
    fn reasoning_effort_as_str_and_display() {
        assert_eq!(ReasoningEffort::Low.as_str(), "low");
        assert_eq!(ReasoningEffort::Medium.to_string(), "medium");
        assert_eq!(ReasoningEffort::High.to_string(), "high");
    }

    #[test]
    fn reasoning_effort_default_is_medium() {
        assert_eq!(ReasoningEffort::default(), ReasoningEffort::Medium);
    }

    #[test]
    fn reasoning_effort_serialization_round_trip() {
        let effort = ReasoningEffort::High;
        let json = serde_json::to_string(&effort).expect("serialize effort");
        assert_eq!(json, "\"high\"");

        let parsed: ReasoningEffort = serde_json::from_str(&json).expect("deserialize effort");
        assert_eq!(parsed, ReasoningEffort::High);
    }

    #[test]
    fn model_options_default_values() {
        let options = ModelOptions::default();
        assert_eq!(options.reasoning_effort, None);
        assert_eq!(options.temperature, None);
        assert_eq!(options.max_tokens, None);
        assert_eq!(options.top_p, None);
        assert!(options.stop_sequences.is_empty());
    }

    #[test]
    fn model_options_can_hold_values() {
        let options = ModelOptions {
            reasoning_effort: Some(ReasoningEffort::Low),
            temperature: Some(0.2),
            max_tokens: Some(256),
            top_p: Some(0.9),
            stop_sequences: vec!["END".to_string()],
        };

        assert_eq!(options.reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(options.temperature, Some(0.2));
        assert_eq!(options.max_tokens, Some(256));
        assert_eq!(options.top_p, Some(0.9));
        assert_eq!(options.stop_sequences, vec!["END"]);
    }

    #[test]
    fn run_command_returns_not_found_for_invalid_shell() {
        let runner = ShellRunner::new(ShellConfig {
            path: "missing-shell-binary-123".to_string(),
            ..ShellConfig::default()
        });

        let err = runner
            .run_command("echo hi")
            .expect_err("should fail with not found");
        match err {
            ShellError::NotFound(path) => assert_eq!(path, "missing-shell-binary-123"),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    fn restore_shell(old: Option<String>) {
        if let Some(value) = old {
            unsafe {
                std::env::set_var("SHELL", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SHELL");
            }
        }
    }
}
