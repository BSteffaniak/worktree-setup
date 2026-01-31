//! Configuration file discovery.
//!
//! Discovers worktree configuration files in a repository using git ls-files.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::ConfigError;
use crate::types::LoadedConfig;

/// Discover all worktree configuration files in a repository.
///
/// Searches for files matching `**/worktree.config.{toml,ts}` and
/// `**/worktree.*.config.{toml,ts}` patterns.
///
/// # Arguments
///
/// * `repo_root` - Path to the repository root
///
/// # Errors
///
/// * If git ls-files fails
/// * If the output cannot be parsed
pub fn discover_configs(repo_root: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    log::debug!("Discovering configs in {}", repo_root.display());

    // Use git ls-files to find config files (fast and respects .gitignore)
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "*.config.toml",
            "*.config.ts",
        ])
        .current_dir(repo_root)
        .output()?;

    if !output.status.success() {
        // Fall back to glob if git fails
        return discover_configs_with_glob(repo_root);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut configs: Vec<PathBuf> = stdout
        .lines()
        .filter(|line| {
            let name = Path::new(line)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            // Match worktree.config.{toml,ts} or worktree.*.config.{toml,ts}
            name.starts_with("worktree") && name.contains(".config.")
        })
        .map(|line| repo_root.join(line))
        .collect();

    configs.sort();
    log::debug!("Found {} config files", configs.len());

    Ok(configs)
}

/// Fall back to glob-based discovery if git is not available.
fn discover_configs_with_glob(repo_root: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    log::debug!("Falling back to glob-based discovery");

    let patterns = [
        "**/worktree.config.toml",
        "**/worktree.config.ts",
        "**/worktree.*.config.toml",
        "**/worktree.*.config.ts",
    ];

    let mut configs = Vec::new();

    for pattern in patterns {
        let full_pattern = repo_root.join(pattern).to_string_lossy().to_string();
        for entry in glob::glob(&full_pattern)? {
            if let Ok(path) = entry {
                // Skip node_modules
                if !path.to_string_lossy().contains("node_modules") {
                    configs.push(path);
                }
            }
        }
    }

    configs.sort();
    configs.dedup();

    Ok(configs)
}

/// Get a display name for a loaded configuration.
///
/// Returns a short, human-readable name based on the config's directory.
#[must_use]
pub fn get_config_display_name(config: &LoadedConfig) -> String {
    // Try to get the parent directory name
    if let Some(parent) = config.config_dir.file_name() {
        let parent_name = parent.to_string_lossy();
        // If parent is not a common name, use it
        if parent_name != "." && parent_name != ".." {
            return parent_name.to_string();
        }
    }

    // Fall back to relative path
    config.relative_path.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_config_display_name() {
        let config = LoadedConfig {
            config: crate::types::Config::default(),
            config_path: PathBuf::from("/repo/apps/my-app/worktree.config.toml"),
            config_dir: PathBuf::from("/repo/apps/my-app"),
            relative_path: "apps/my-app/worktree.config.toml".to_string(),
        };

        assert_eq!(get_config_display_name(&config), "my-app");
    }
}
