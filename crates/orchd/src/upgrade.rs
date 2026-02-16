//! Self-upgrade system for Othala.

use serde::{Deserialize, Serialize};
use std::process::Command;

/// Version information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub current: String,
    pub latest: Option<String>,
    pub update_available: bool,
    pub release_url: Option<String>,
}

/// Check current version
pub fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Check for updates by querying GitHub releases API
pub fn check_for_update() -> VersionInfo {
    let current = current_version();

    // Try to get latest release from GitHub
    let latest = fetch_latest_version();
    let update_available = latest
        .as_deref()
        .map(|l| version_is_newer(l, &current))
        .unwrap_or(false);

    VersionInfo {
        current,
        latest: latest.clone(),
        update_available,
        release_url: latest.map(|v| format!("https://github.com/0xMugen/Othala/releases/tag/v{v}")),
    }
}

/// Fetch latest version from GitHub API using gh CLI
fn fetch_latest_version() -> Option<String> {
    let output = Command::new("gh")
        .args(["api", "repos/0xMugen/Othala/releases/latest", "--jq", ".tag_name"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// Compare version strings (simple semver comparison)
pub fn version_is_newer(candidate: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .filter_map(|p| p.parse::<u64>().ok())
            .collect()
    };

    let c = parse(candidate);
    let cur = parse(current);

    for i in 0..std::cmp::max(c.len(), cur.len()) {
        let a = c.get(i).copied().unwrap_or(0);
        let b = cur.get(i).copied().unwrap_or(0);
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }
    false
}

/// Perform the upgrade
pub fn perform_upgrade() -> Result<String, String> {
    // Try cargo install from git
    let output = Command::new("cargo")
        .args([
            "install",
            "--git",
            "https://github.com/0xMugen/Othala.git",
            "--bin",
            "othala",
        ])
        .output()
        .map_err(|e| format!("failed to run cargo install: {e}"))?;

    if output.status.success() {
        Ok("Upgrade successful!".to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("Upgrade failed: {stderr}"))
    }
}

/// Display version comparison
pub fn display_version_check(info: &VersionInfo) -> String {
    let mut out = format!("Current version: {}\n", info.current);
    if let Some(latest) = &info.latest {
        out.push_str(&format!("Latest version:  {}\n", latest));
        if info.update_available {
            out.push_str("Update available!\n");
            if let Some(url) = &info.release_url {
                out.push_str(&format!("Release: {}\n", url));
            }
            out.push_str("\nRun 'othala upgrade --install' to update.");
        } else {
            out.push_str("You're up to date.");
        }
    } else {
        out.push_str("Could not check for updates (no network or gh CLI not available).");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_returns_something() {
        let current = current_version();
        assert!(!current.is_empty());
    }

    #[test]
    fn version_is_newer_major_bump() {
        assert!(version_is_newer("2.0.0", "1.9.9"));
    }

    #[test]
    fn version_is_newer_minor_bump() {
        assert!(version_is_newer("1.3.0", "1.2.9"));
    }

    #[test]
    fn version_is_newer_patch_bump() {
        assert!(version_is_newer("1.2.4", "1.2.3"));
    }

    #[test]
    fn version_is_newer_same_version_false() {
        assert!(!version_is_newer("1.2.3", "1.2.3"));
    }

    #[test]
    fn version_is_newer_older_version_false() {
        assert!(!version_is_newer("1.2.2", "1.2.3"));
    }

    #[test]
    fn display_version_check_format() {
        let info = VersionInfo {
            current: "1.0.0".to_string(),
            latest: Some("1.1.0".to_string()),
            update_available: true,
            release_url: Some("https://github.com/0xMugen/Othala/releases/tag/v1.1.0".to_string()),
        };

        let output = display_version_check(&info);
        assert!(output.contains("Current version: 1.0.0"));
        assert!(output.contains("Latest version:  1.1.0"));
        assert!(output.contains("Update available!"));
        assert!(output.contains("othala upgrade --install"));
    }
}
