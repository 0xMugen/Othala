use orch_core::types::ModelKind;

use crate::error::AgentError;
use crate::signal::detect_common_signal;
use crate::types::{AgentCommand, AgentSignal, EpochRequest};

pub trait AgentAdapter: Send + Sync {
    fn model(&self) -> ModelKind;
    fn build_command(&self, request: &EpochRequest) -> AgentCommand;
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
        let mut args = request.extra_args.clone();
        args.push(request.prompt.clone());
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
        let mut args = request.extra_args.clone();
        args.push(request.prompt.clone());
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
        let mut args = request.extra_args.clone();
        args.push(request.prompt.clone());
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
