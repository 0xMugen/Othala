pub mod adapter;
pub mod error;
pub mod runner;
pub mod setup;
pub mod signal;
pub mod types;

pub use adapter::*;
pub use error::*;
pub use runner::*;
pub use setup::*;
pub use signal::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::{
        default_adapter_for, detect_common_signal, probe_models, probe_models_with_runner,
        summarize_setup, validate_setup_selection, AgentAdapter, AgentCommand, AgentError,
        AgentSignal, AgentSignalKind, ClaudeAdapter, CodexAdapter, EnvRequirementGroup,
        EnvRequirementStatus, EpochRequest, EpochResult, EpochRunner, EpochStopReason,
        GeminiAdapter, ModelProbeResult, ModelSetupSelection, ProcessSetupCommandRunner, PtyChunk,
        RunnerPtySize, SetupCommandRunner, SetupError, SetupProbeConfig, SetupProbeReport,
        SetupSummary, SetupSummaryItem, ValidatedSetupSelection,
    };
    use orch_core::types::ModelKind;
    use std::any::TypeId;

    #[test]
    fn crate_root_reexports_types() {
        let _ = TypeId::of::<AgentError>();
        let _ = TypeId::of::<AgentCommand>();
        let _ = TypeId::of::<EpochRequest>();
        let _ = TypeId::of::<EpochResult>();
        let _ = TypeId::of::<EpochStopReason>();
        let _ = TypeId::of::<AgentSignal>();
        let _ = TypeId::of::<AgentSignalKind>();
        let _ = TypeId::of::<PtyChunk>();
        let _ = TypeId::of::<ClaudeAdapter>();
        let _ = TypeId::of::<CodexAdapter>();
        let _ = TypeId::of::<GeminiAdapter>();
        let _ = TypeId::of::<EpochRunner>();
        let _ = TypeId::of::<RunnerPtySize>();
        let _ = TypeId::of::<SetupError>();
        let _ = TypeId::of::<EnvRequirementGroup>();
        let _ = TypeId::of::<EnvRequirementStatus>();
        let _ = TypeId::of::<SetupProbeConfig>();
        let _ = TypeId::of::<ModelProbeResult>();
        let _ = TypeId::of::<SetupProbeReport>();
        let _ = TypeId::of::<ModelSetupSelection>();
        let _ = TypeId::of::<ValidatedSetupSelection>();
        let _ = TypeId::of::<SetupSummaryItem>();
        let _ = TypeId::of::<SetupSummary>();
        let _ = TypeId::of::<ProcessSetupCommandRunner>();
    }

    #[test]
    fn crate_root_reexports_helpers() {
        let _default_adapter: fn(ModelKind) -> Result<Box<dyn AgentAdapter>, AgentError> =
            default_adapter_for;
        let _detect_signal: fn(&str) -> Option<AgentSignal> = detect_common_signal;
        let _probe: fn(&SetupProbeConfig) -> SetupProbeReport = probe_models;
        let _probe_with_runner: fn(&SetupProbeConfig, &dyn SetupCommandRunner) -> SetupProbeReport =
            probe_models_with_runner;
        let _validate: fn(
            &SetupProbeReport,
            &ModelSetupSelection,
        ) -> Result<ValidatedSetupSelection, SetupError> = validate_setup_selection;
        let _summarize: fn(&SetupProbeReport, &ValidatedSetupSelection) -> SetupSummary =
            summarize_setup;
    }
}
