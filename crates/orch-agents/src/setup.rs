use orch_core::types::ModelKind;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("selected model is not available: {model:?}")]
    SelectedModelUnavailable { model: ModelKind },
    #[error("selected model is unknown in current probe result: {model:?}")]
    SelectedModelUnknown { model: ModelKind },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvRequirementGroup {
    pub any_of: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupProbeConfig {
    pub executable_by_model: HashMap<ModelKind, String>,
    pub env_requirements_by_model: HashMap<ModelKind, Vec<EnvRequirementGroup>>,
}

impl Default for SetupProbeConfig {
    fn default() -> Self {
        let mut executable_by_model = HashMap::new();
        executable_by_model.insert(ModelKind::Claude, "claude".to_string());
        executable_by_model.insert(ModelKind::Codex, "codex".to_string());
        executable_by_model.insert(ModelKind::Gemini, "gemini".to_string());

        let mut env_requirements_by_model = HashMap::new();
        env_requirements_by_model.insert(
            ModelKind::Claude,
            vec![EnvRequirementGroup {
                any_of: vec!["ANTHROPIC_API_KEY".to_string()],
            }],
        );
        env_requirements_by_model.insert(
            ModelKind::Codex,
            vec![EnvRequirementGroup {
                any_of: vec!["OPENAI_API_KEY".to_string()],
            }],
        );
        env_requirements_by_model.insert(
            ModelKind::Gemini,
            vec![EnvRequirementGroup {
                any_of: vec!["GEMINI_API_KEY".to_string(), "GOOGLE_API_KEY".to_string()],
            }],
        );

        Self {
            executable_by_model,
            env_requirements_by_model,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvRequirementStatus {
    pub any_of: Vec<String>,
    pub satisfied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProbeResult {
    pub model: ModelKind,
    pub executable: String,
    pub installed: bool,
    pub version_ok: bool,
    pub version_output: Option<String>,
    pub env_status: Vec<EnvRequirementStatus>,
    pub healthy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupProbeReport {
    pub models: Vec<ModelProbeResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSetupSelection {
    pub enabled_models: Vec<ModelKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedSetupSelection {
    pub enabled_models: Vec<ModelKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSummaryItem {
    pub model: ModelKind,
    pub executable: String,
    pub detected: bool,
    pub healthy: bool,
    pub selected: bool,
    pub missing_env_any_of: Vec<Vec<String>>,
    pub version_output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSummary {
    pub selected_models: Vec<ModelKind>,
    pub all_selected_healthy: bool,
    pub items: Vec<SetupSummaryItem>,
}

pub trait SetupCommandRunner {
    fn command_exists(&self, executable: &str) -> bool;
    fn command_version(&self, executable: &str) -> Result<String, String>;
    fn env_var_present(&self, env_key: &str) -> bool;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessSetupCommandRunner;

impl SetupCommandRunner for ProcessSetupCommandRunner {
    fn command_exists(&self, executable: &str) -> bool {
        Command::new("bash")
            .arg("-lc")
            .arg(format!("command -v -- {}", shell_quote(executable)))
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn command_version(&self, executable: &str) -> Result<String, String> {
        let output = Command::new(executable)
            .arg("--version")
            .output()
            .map_err(|err| err.to_string())?;

        if !output.status.success() {
            return Err(format!(
                "non-zero exit {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let first = text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        Ok(first)
    }

    fn env_var_present(&self, env_key: &str) -> bool {
        std::env::var_os(env_key).is_some()
    }
}

pub fn probe_models(config: &SetupProbeConfig) -> SetupProbeReport {
    let runner = ProcessSetupCommandRunner;
    probe_models_with_runner(config, &runner)
}

pub fn probe_models_with_runner(
    config: &SetupProbeConfig,
    runner: &dyn SetupCommandRunner,
) -> SetupProbeReport {
    let mut models = vec![ModelKind::Claude, ModelKind::Codex, ModelKind::Gemini];
    models.sort_by_key(model_rank);

    let mut out = Vec::new();
    for model in models {
        let executable = config
            .executable_by_model
            .get(&model)
            .cloned()
            .unwrap_or_else(|| default_executable_for_model(model).to_string());

        let installed = runner.command_exists(&executable);
        let (version_ok, version_output) = if installed {
            match runner.command_version(&executable) {
                Ok(text) => (true, Some(text)),
                Err(err) => (false, Some(err)),
            }
        } else {
            (false, None)
        };

        let requirements = config
            .env_requirements_by_model
            .get(&model)
            .cloned()
            .unwrap_or_default();
        let env_status = requirements
            .iter()
            .map(|group| EnvRequirementStatus {
                any_of: group.any_of.clone(),
                satisfied: group.any_of.iter().any(|key| runner.env_var_present(key)),
            })
            .collect::<Vec<_>>();

        let env_ok = env_status.iter().all(|group| group.satisfied);
        let healthy = installed && version_ok && env_ok;

        out.push(ModelProbeResult {
            model,
            executable,
            installed,
            version_ok,
            version_output,
            env_status,
            healthy,
        });
    }

    SetupProbeReport { models: out }
}

pub fn validate_setup_selection(
    report: &SetupProbeReport,
    selection: &ModelSetupSelection,
) -> Result<ValidatedSetupSelection, SetupError> {
    let probe_by_model = report
        .models
        .iter()
        .map(|probe| (probe.model, probe))
        .collect::<HashMap<_, _>>();

    let mut dedup = HashSet::new();
    let mut enabled_models = Vec::new();

    for model in &selection.enabled_models {
        let Some(probe) = probe_by_model.get(model) else {
            return Err(SetupError::SelectedModelUnknown { model: *model });
        };
        if !probe.healthy {
            return Err(SetupError::SelectedModelUnavailable { model: *model });
        }
        if dedup.insert(*model) {
            enabled_models.push(*model);
        }
    }

    Ok(ValidatedSetupSelection { enabled_models })
}

pub fn summarize_setup(
    report: &SetupProbeReport,
    selection: &ValidatedSetupSelection,
) -> SetupSummary {
    let selected = selection.enabled_models.clone();
    let selected_set = selected.iter().copied().collect::<HashSet<_>>();

    let mut items = report
        .models
        .iter()
        .map(|probe| {
            let missing_env_any_of = probe
                .env_status
                .iter()
                .filter(|status| !status.satisfied)
                .map(|status| status.any_of.clone())
                .collect::<Vec<_>>();

            SetupSummaryItem {
                model: probe.model,
                executable: probe.executable.clone(),
                detected: probe.installed,
                healthy: probe.healthy,
                selected: selected_set.contains(&probe.model),
                missing_env_any_of,
                version_output: probe.version_output.clone(),
            }
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|item| model_rank(&item.model));

    let all_selected_healthy = items
        .iter()
        .filter(|item| item.selected)
        .all(|item| item.healthy);

    SetupSummary {
        selected_models: selected,
        all_selected_healthy,
        items,
    }
}

fn model_rank(model: &ModelKind) -> u8 {
    match model {
        ModelKind::Claude => 0,
        ModelKind::Codex => 1,
        ModelKind::Gemini => 2,
    }
}

fn default_executable_for_model(model: ModelKind) -> &'static str {
    match model {
        ModelKind::Claude => "claude",
        ModelKind::Codex => "codex",
        ModelKind::Gemini => "gemini",
    }
}

fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::{
        probe_models_with_runner, summarize_setup, validate_setup_selection, ModelProbeResult,
        ModelSetupSelection, SetupCommandRunner, SetupError, SetupProbeConfig,
        ValidatedSetupSelection,
    };
    use orch_core::types::ModelKind;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MockRunner {
        installed: HashMap<String, bool>,
        versions: HashMap<String, Result<String, String>>,
        env_present: HashMap<String, bool>,
    }

    impl SetupCommandRunner for MockRunner {
        fn command_exists(&self, executable: &str) -> bool {
            self.installed.get(executable).copied().unwrap_or(false)
        }

        fn command_version(&self, executable: &str) -> Result<String, String> {
            self.versions
                .get(executable)
                .cloned()
                .unwrap_or_else(|| Err("not configured".to_string()))
        }

        fn env_var_present(&self, env_key: &str) -> bool {
            self.env_present.get(env_key).copied().unwrap_or(false)
        }
    }

    #[test]
    fn probe_marks_model_unhealthy_when_env_missing() {
        let mut runner = MockRunner::default();
        runner.installed.insert("codex".to_string(), true);
        runner
            .versions
            .insert("codex".to_string(), Ok("codex 1.0.0".to_string()));
        runner
            .env_present
            .insert("OPENAI_API_KEY".to_string(), false);

        let report = probe_models_with_runner(&SetupProbeConfig::default(), &runner);
        let codex = report
            .models
            .iter()
            .find(|m| m.model == ModelKind::Codex)
            .cloned()
            .expect("codex probe missing");

        assert!(codex.installed);
        assert!(codex.version_ok);
        assert!(!codex.healthy);
        assert_eq!(codex.env_status.len(), 1);
        assert!(!codex.env_status[0].satisfied);
    }

    #[test]
    fn probe_marks_model_healthy_when_binary_version_and_env_ok() {
        let mut runner = MockRunner::default();
        runner.installed.insert("claude".to_string(), true);
        runner
            .versions
            .insert("claude".to_string(), Ok("claude 2.0.0".to_string()));
        runner
            .env_present
            .insert("ANTHROPIC_API_KEY".to_string(), true);

        let report = probe_models_with_runner(&SetupProbeConfig::default(), &runner);
        let claude = report
            .models
            .iter()
            .find(|m| m.model == ModelKind::Claude)
            .cloned()
            .expect("claude probe missing");
        assert!(claude.healthy);
    }

    #[test]
    fn validate_selection_rejects_unhealthy_selected_model() {
        let report = super::SetupProbeReport {
            models: vec![ModelProbeResult {
                model: ModelKind::Gemini,
                executable: "gemini".to_string(),
                installed: true,
                version_ok: true,
                version_output: Some("gemini 0.1".to_string()),
                env_status: vec![],
                healthy: false,
            }],
        };

        let selection = ModelSetupSelection {
            enabled_models: vec![ModelKind::Gemini],
        };
        let err = validate_setup_selection(&report, &selection).expect_err("expected error");
        assert!(matches!(
            err,
            SetupError::SelectedModelUnavailable {
                model: ModelKind::Gemini
            }
        ));
    }

    #[test]
    fn validate_selection_rejects_unknown_selected_model() {
        let report = super::SetupProbeReport {
            models: vec![ModelProbeResult {
                model: ModelKind::Codex,
                executable: "codex".to_string(),
                installed: true,
                version_ok: true,
                version_output: Some("codex 1".to_string()),
                env_status: vec![],
                healthy: true,
            }],
        };

        let selection = ModelSetupSelection {
            enabled_models: vec![ModelKind::Gemini],
        };
        let err = validate_setup_selection(&report, &selection).expect_err("expected error");
        assert!(matches!(
            err,
            SetupError::SelectedModelUnknown {
                model: ModelKind::Gemini
            }
        ));
    }

    #[test]
    fn validate_selection_dedupes_models_in_order() {
        let report = super::SetupProbeReport {
            models: vec![
                ModelProbeResult {
                    model: ModelKind::Codex,
                    executable: "codex".to_string(),
                    installed: true,
                    version_ok: true,
                    version_output: Some("codex 1".to_string()),
                    env_status: vec![],
                    healthy: true,
                },
                ModelProbeResult {
                    model: ModelKind::Claude,
                    executable: "claude".to_string(),
                    installed: true,
                    version_ok: true,
                    version_output: Some("claude 1".to_string()),
                    env_status: vec![],
                    healthy: true,
                },
            ],
        };

        let selection = ModelSetupSelection {
            enabled_models: vec![ModelKind::Codex, ModelKind::Claude, ModelKind::Codex],
        };
        let validated = validate_setup_selection(&report, &selection).expect("valid selection");
        assert_eq!(
            validated.enabled_models,
            vec![ModelKind::Codex, ModelKind::Claude]
        );
    }

    #[test]
    fn probe_falls_back_to_default_executables_when_override_missing() {
        let mut config = SetupProbeConfig::default();
        config.executable_by_model.clear();

        let mut runner = MockRunner::default();
        runner.installed.insert("claude".to_string(), true);
        runner.installed.insert("codex".to_string(), true);
        runner.installed.insert("gemini".to_string(), true);
        runner
            .versions
            .insert("claude".to_string(), Ok("claude 1".to_string()));
        runner
            .versions
            .insert("codex".to_string(), Ok("codex 1".to_string()));
        runner
            .versions
            .insert("gemini".to_string(), Ok("gemini 1".to_string()));
        runner
            .env_present
            .insert("ANTHROPIC_API_KEY".to_string(), true);
        runner
            .env_present
            .insert("OPENAI_API_KEY".to_string(), true);
        runner
            .env_present
            .insert("GEMINI_API_KEY".to_string(), true);

        let report = probe_models_with_runner(&config, &runner);

        let claude = report
            .models
            .iter()
            .find(|m| m.model == ModelKind::Claude)
            .expect("claude probe");
        let codex = report
            .models
            .iter()
            .find(|m| m.model == ModelKind::Codex)
            .expect("codex probe");
        let gemini = report
            .models
            .iter()
            .find(|m| m.model == ModelKind::Gemini)
            .expect("gemini probe");

        assert_eq!(claude.executable, "claude");
        assert_eq!(codex.executable, "codex");
        assert_eq!(gemini.executable, "gemini");
        assert!(claude.healthy);
        assert!(codex.healthy);
        assert!(gemini.healthy);
    }

    #[test]
    fn summarize_setup_reports_selected_health_and_missing_env_groups() {
        let report = super::SetupProbeReport {
            models: vec![
                ModelProbeResult {
                    model: ModelKind::Claude,
                    executable: "claude".to_string(),
                    installed: true,
                    version_ok: true,
                    version_output: Some("claude 1".to_string()),
                    env_status: vec![],
                    healthy: true,
                },
                ModelProbeResult {
                    model: ModelKind::Codex,
                    executable: "codex".to_string(),
                    installed: true,
                    version_ok: true,
                    version_output: Some("codex 2".to_string()),
                    env_status: vec![super::EnvRequirementStatus {
                        any_of: vec!["OPENAI_API_KEY".to_string()],
                        satisfied: false,
                    }],
                    healthy: false,
                },
            ],
        };

        let summary = summarize_setup(
            &report,
            &ValidatedSetupSelection {
                enabled_models: vec![ModelKind::Claude, ModelKind::Codex],
            },
        );

        assert_eq!(
            summary.selected_models,
            vec![ModelKind::Claude, ModelKind::Codex]
        );
        assert!(!summary.all_selected_healthy);
        assert_eq!(summary.items.len(), 2);
        assert!(summary.items[0].selected);
        assert_eq!(summary.items[0].model, ModelKind::Claude);
        assert!(summary.items[0].healthy);
        assert!(summary.items[0].missing_env_any_of.is_empty());
        assert_eq!(summary.items[1].model, ModelKind::Codex);
        assert_eq!(
            summary.items[1].missing_env_any_of,
            vec![vec!["OPENAI_API_KEY".to_string()]]
        );
    }
}
