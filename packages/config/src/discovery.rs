//! Configuration file discovery.
//!
//! Discovers worktree configuration files in a repository using fast parallel
//! filesystem traversal.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::{Path, PathBuf};

use worktree_setup_glob::DEFAULT_SKIP_DIRS;

use crate::error::ConfigError;
use crate::types::LoadedConfig;

/// Discover all worktree configuration files in a repository.
///
/// Searches for files matching `worktree.config.{toml,ts}` and
/// `worktree.*.config.{toml,ts}` patterns using fast parallel directory traversal.
///
/// Automatically prunes directories listed in [`DEFAULT_SKIP_DIRS`]
/// (`node_modules`, `.git`, `target`) at the directory level so their
/// contents are never traversed.
///
/// # Arguments
///
/// * `repo_root` - Path to the repository root
///
/// # Errors
///
/// * If the directory cannot be read
pub fn discover_configs(repo_root: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    log::debug!("Discovering configs in {}", repo_root.display());

    let mut configs: Vec<PathBuf> = jwalk::WalkDirGeneric::<((), ())>::new(repo_root)
        .skip_hidden(false)
        .sort(false)
        .process_read_dir(|_depth, _path, _state, children| {
            // Prune directories we never want to enter.  Setting
            // `read_children_path = None` prevents jwalk from descending.
            // Removing entries entirely prevents them from appearing in
            // the iterator output at all.
            children.retain(|entry_result| {
                let Ok(entry) = entry_result.as_ref() else {
                    return false;
                };
                if entry.file_type.is_dir() {
                    let name = entry.file_name.to_string_lossy();
                    if DEFAULT_SKIP_DIRS.iter().any(|&skip| name == skip) {
                        return false;
                    }
                }
                true
            });
        })
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            // Only files
            if !entry.file_type().is_file() {
                return false;
            }

            // Match worktree.config.{toml,ts} or worktree.*.config.{toml,ts}
            let name = entry.file_name().to_string_lossy();
            name.starts_with("worktree")
                && name.contains(".config.")
                && (name.ends_with(".toml") || name.ends_with(".ts"))
        })
        .map(|entry| entry.path())
        .collect();

    configs.sort();
    log::debug!("Found {} config files", configs.len());

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
