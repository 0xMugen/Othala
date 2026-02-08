pub mod command;
pub mod discover;
pub mod error;
pub mod runner;
pub mod types;

pub use command::*;
pub use discover::*;
pub use error::*;
pub use runner::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::{
        commands_for_tier, discover_verify_commands, prepare_verify_command,
        resolve_verify_commands, run_shell_command, PreparedVerifyCommand, ShellCommandOutput,
        VerifyCommandResult, VerifyError, VerifyFailureClass, VerifyOutcome, VerifyResult,
        VerifyRunner,
    };
    use orch_core::config::RepoConfig;
    use orch_core::state::VerifyTier;
    use std::any::TypeId;
    use std::path::Path;

    #[test]
    fn crate_root_reexports_types() {
        let _ = TypeId::of::<VerifyRunner>();
        let _ = TypeId::of::<VerifyError>();
        let _ = TypeId::of::<VerifyOutcome>();
        let _ = TypeId::of::<VerifyFailureClass>();
        let _ = TypeId::of::<PreparedVerifyCommand>();
        let _ = TypeId::of::<VerifyCommandResult>();
        let _ = TypeId::of::<VerifyResult>();
        let _ = TypeId::of::<ShellCommandOutput>();
    }

    #[test]
    fn crate_root_reexports_helpers() {
        let _run: fn(&Path, &str, &str) -> Result<ShellCommandOutput, VerifyError> =
            run_shell_command;
        let _discover: fn(&Path, VerifyTier) -> Vec<String> = discover_verify_commands;
        let _resolve: fn(&Path, VerifyTier, &[String]) -> Vec<String> = resolve_verify_commands;
        let _commands_for_tier: fn(&RepoConfig, VerifyTier) -> Vec<String> = commands_for_tier;
        let _prepare: fn(&str, &str) -> PreparedVerifyCommand = prepare_verify_command;
    }
}
