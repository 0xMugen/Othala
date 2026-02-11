use orch_core::types::ModelKind;

use crate::error::AgentError;
use crate::signal::detect_common_signal;
use crate::types::{AgentCommand, AgentSignal, EpochRequest};

pub trait AgentAdapter: Send + Sync {
    fn model(&self) -> ModelKind;
    fn build_command(&self, request: &EpochRequest) -> AgentCommand;
    /// Build a command for interactive (stdin-driven) sessions.
    /// Omits headless flags (`-p`, `exec --full-auto`) so the agent
    /// reads follow-up messages from stdin.
    fn build_interactive_command(&self, request: &EpochRequest) -> AgentCommand {
        self.build_command(request)
    }
    fn detect_signal(&self, line: &str) -> Option<AgentSignal> {
        detect_common_signal(line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeAdapter {
    pub executable: String,
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self {
            executable: "claude".to_string(),
        }
    }
}

impl AgentAdapter for ClaudeAdapter {
    fn model(&self) -> ModelKind {
        ModelKind::Claude
    }

    fn build_command(&self, request: &EpochRequest) -> AgentCommand {
        let mut args = vec![
            "-p".to_string(),
            "--verbose".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        args.extend(request.extra_args.iter().cloned());
        args.push(request.prompt.clone());
        AgentCommand {
            executable: self.executable.clone(),
            args,
            env: request.env.clone(),
        }
    }

    fn build_interactive_command(&self, request: &EpochRequest) -> AgentCommand {
        let mut args = vec![
            "--verbose".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        args.extend(request.extra_args.iter().cloned());
        AgentCommand {
            executable: self.executable.clone(),
            args,
            env: request.env.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAdapter {
    pub executable: String,
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self {
            executable: "codex".to_string(),
        }
    }
}

impl AgentAdapter for CodexAdapter {
    fn model(&self) -> ModelKind {
        ModelKind::Codex
    }

    fn build_command(&self, request: &EpochRequest) -> AgentCommand {
        let mut args = vec!["exec".to_string(), "--full-auto".to_string()];
        args.extend(request.extra_args.iter().cloned());
        args.push(request.prompt.clone());
        AgentCommand {
            executable: self.executable.clone(),
            args,
            env: request.env.clone(),
        }
    }

    fn build_interactive_command(&self, request: &EpochRequest) -> AgentCommand {
        let mut args = Vec::new();
        args.extend(request.extra_args.iter().cloned());
        AgentCommand {
            executable: self.executable.clone(),
            args,
            env: request.env.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiAdapter {
    pub executable: String,
}

impl Default for GeminiAdapter {
    fn default() -> Self {
        Self {
            executable: "gemini".to_string(),
        }
    }
}

impl AgentAdapter for GeminiAdapter {
    fn model(&self) -> ModelKind {
        ModelKind::Gemini
    }

    fn build_command(&self, request: &EpochRequest) -> AgentCommand {
        let mut args = vec![
            "-p".to_string(),
            request.prompt.clone(),
            "--yolo".to_string(),
        ];
        args.extend(request.extra_args.iter().cloned());
        AgentCommand {
            executable: self.executable.clone(),
            args,
            env: request.env.clone(),
        }
    }

    fn build_interactive_command(&self, request: &EpochRequest) -> AgentCommand {
        let mut args = vec!["--yolo".to_string()];
        args.extend(request.extra_args.iter().cloned());
        AgentCommand {
            executable: self.executable.clone(),
            args,
            env: request.env.clone(),
        }
    }
}

pub fn default_adapter_for(model: ModelKind) -> Result<Box<dyn AgentAdapter>, AgentError> {
    match model {
        ModelKind::Claude => Ok(Box::new(ClaudeAdapter::default())),
        ModelKind::Codex => Ok(Box::new(CodexAdapter::default())),
        ModelKind::Gemini => Ok(Box::new(GeminiAdapter::default())),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use orch_core::types::{ModelKind, RepoId, TaskId};

    use crate::types::EpochRequest;

    use super::{default_adapter_for, AgentAdapter, ClaudeAdapter, CodexAdapter, GeminiAdapter};

    fn mk_request(model: ModelKind) -> EpochRequest {
        EpochRequest {
            task_id: TaskId("T1".to_string()),
            repo_id: RepoId("example".to_string()),
            model,
            repo_path: PathBuf::from("/tmp/repo"),
            prompt: "implement feature".to_string(),
            timeout_secs: 30,
            extra_args: vec!["--flag".to_string(), "--json".to_string()],
            env: vec![("FOO".to_string(), "BAR".to_string())],
        }
    }

    #[test]
    fn claude_adapter_builds_command_with_noninteractive_flags_and_prompt() {
        let adapter = ClaudeAdapter::default();
        let request = mk_request(ModelKind::Claude);
        let command = adapter.build_command(&request);

        assert_eq!(command.executable, "claude");
        assert_eq!(
            command.args,
            vec![
                "-p".to_string(),
                "--verbose".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--flag".to_string(),
                "--json".to_string(),
                "implement feature".to_string()
            ]
        );
        assert_eq!(command.env, vec![("FOO".to_string(), "BAR".to_string())]);
    }

    #[test]
    fn codex_adapter_builds_command_with_exec_subcommand_and_prompt() {
        let adapter = CodexAdapter::default();
        let request = mk_request(ModelKind::Codex);
        let command = adapter.build_command(&request);

        assert_eq!(command.executable, "codex");
        assert_eq!(
            command.args,
            vec![
                "exec".to_string(),
                "--full-auto".to_string(),
                "--flag".to_string(),
                "--json".to_string(),
                "implement feature".to_string()
            ]
        );
        assert_eq!(command.env, vec![("FOO".to_string(), "BAR".to_string())]);
    }

    #[test]
    fn gemini_adapter_builds_command_with_noninteractive_flags_and_prompt() {
        let adapter = GeminiAdapter::default();
        let request = mk_request(ModelKind::Gemini);
        let command = adapter.build_command(&request);

        assert_eq!(command.executable, "gemini");
        assert_eq!(
            command.args,
            vec![
                "-p".to_string(),
                "implement feature".to_string(),
                "--yolo".to_string(),
                "--flag".to_string(),
                "--json".to_string(),
            ]
        );
        assert_eq!(command.env, vec![("FOO".to_string(), "BAR".to_string())]);
    }

    #[test]
    fn default_adapter_for_returns_adapter_matching_requested_model() {
        let claude = default_adapter_for(ModelKind::Claude).expect("claude adapter");
        let codex = default_adapter_for(ModelKind::Codex).expect("codex adapter");
        let gemini = default_adapter_for(ModelKind::Gemini).expect("gemini adapter");

        assert_eq!(claude.model(), ModelKind::Claude);
        assert_eq!(codex.model(), ModelKind::Codex);
        assert_eq!(gemini.model(), ModelKind::Gemini);
    }

    #[test]
    fn adapters_preserve_empty_prompt_as_last_argument() {
        let mut request = mk_request(ModelKind::Claude);
        request.prompt = "".to_string();
        request.extra_args = vec!["--json".to_string()];

        let claude = ClaudeAdapter::default().build_command(&request);
        let codex = CodexAdapter::default().build_command(&EpochRequest {
            model: ModelKind::Codex,
            ..request.clone()
        });
        let gemini = GeminiAdapter::default().build_command(&EpochRequest {
            model: ModelKind::Gemini,
            ..request.clone()
        });

        assert!(
            claude.args.last() == Some(&"".to_string()),
            "claude must append prompt slot even when empty"
        );
        assert!(
            codex.args.last() == Some(&"".to_string()),
            "codex must append prompt slot even when empty"
        );
        // Gemini places prompt right after -p (as its value), not at the end.
        assert!(
            gemini.args[1] == "",
            "gemini must place prompt right after -p even when empty"
        );
    }

    #[test]
    fn claude_interactive_command_omits_headless_flags() {
        let adapter = ClaudeAdapter::default();
        let request = mk_request(ModelKind::Claude);
        let command = adapter.build_interactive_command(&request);

        assert_eq!(command.executable, "claude");
        assert_eq!(
            command.args,
            vec![
                "--verbose".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--flag".to_string(),
                "--json".to_string(),
            ]
        );
        // No -p flag and no prompt argument
        assert!(!command.args.contains(&"-p".to_string()));
        assert!(!command.args.contains(&"implement feature".to_string()));
    }

    #[test]
    fn codex_interactive_command_omits_exec_subcommand() {
        let adapter = CodexAdapter::default();
        let request = mk_request(ModelKind::Codex);
        let command = adapter.build_interactive_command(&request);

        assert_eq!(command.executable, "codex");
        assert!(!command.args.contains(&"exec".to_string()));
        assert!(!command.args.contains(&"--full-auto".to_string()));
        assert_eq!(
            command.args,
            vec!["--flag".to_string(), "--json".to_string()]
        );
    }

    #[test]
    fn gemini_interactive_command_omits_prompt_flag() {
        let adapter = GeminiAdapter::default();
        let request = mk_request(ModelKind::Gemini);
        let command = adapter.build_interactive_command(&request);

        assert_eq!(command.executable, "gemini");
        assert!(!command.args.contains(&"-p".to_string()));
        assert_eq!(
            command.args,
            vec![
                "--yolo".to_string(),
                "--flag".to_string(),
                "--json".to_string(),
            ]
        );
    }

    #[test]
    fn adapters_keep_extra_args_order_before_prompt() {
        let request = EpochRequest {
            extra_args: vec![
                "--first".to_string(),
                "--second".to_string(),
                "--third".to_string(),
            ],
            prompt: "final prompt".to_string(),
            ..mk_request(ModelKind::Claude)
        };

        let command = ClaudeAdapter::default().build_command(&request);
        // Default flags come first, then extra_args, then prompt
        assert_eq!(
            command.args,
            vec![
                "-p".to_string(),
                "--verbose".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--first".to_string(),
                "--second".to_string(),
                "--third".to_string(),
                "final prompt".to_string()
            ]
        );
    }
}
