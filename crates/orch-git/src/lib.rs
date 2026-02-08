pub mod command;
pub mod error;
pub mod repo;
pub mod snapshot;
pub mod worktree;

pub use command::*;
pub use error::*;
pub use repo::*;
pub use snapshot::*;
pub use worktree::*;

#[cfg(test)]
mod tests {
    use super::{
        capture_diff_snapshot, capture_repo_snapshot, capture_status_snapshot, current_branch,
        discover_repo, head_sha, GitCli, GitError, RepoHandle, RepoSnapshot, StatusSnapshot,
    };
    use std::any::TypeId;
    use std::path::Path;

    #[test]
    fn crate_root_reexports_types() {
        let _ = TypeId::of::<GitCli>();
        let _ = TypeId::of::<GitError>();
        let _ = TypeId::of::<RepoHandle>();
        let _ = TypeId::of::<StatusSnapshot>();
        let _ = TypeId::of::<RepoSnapshot>();
    }

    #[test]
    fn crate_root_reexports_snapshot_and_repo_functions() {
        let _discover: fn(&Path, &GitCli) -> Result<RepoHandle, GitError> = discover_repo;
        let _branch: fn(&RepoHandle, &GitCli) -> Result<String, GitError> = current_branch;
        let _head: fn(&RepoHandle, &GitCli) -> Result<String, GitError> = head_sha;
        let _status: fn(&RepoHandle, &GitCli) -> Result<StatusSnapshot, GitError> =
            capture_status_snapshot;
        let _diff: fn(&RepoHandle, &GitCli, Option<&str>) -> Result<super::DiffSnapshot, GitError> =
            capture_diff_snapshot;
        let _repo: fn(&RepoHandle, &GitCli, Option<&str>) -> Result<RepoSnapshot, GitError> =
            capture_repo_snapshot;
    }
}
