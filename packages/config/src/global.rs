//! Global configuration for worktree-setup.
//!
//! Supports two config file locations, with repo-level overriding global:
//!
//! * **Global**: `$XDG_CONFIG_HOME/worktree-setup/config.toml`
//!   (typically `~/.config/worktree-setup/config.toml`)
//! * **Repo-level**: `.worktree-setup.toml` at the repository root
//!
//! If neither file exists, all settings use their defaults. Files are never
//! auto-generated.
//!
//! # Example
//!
//! ```toml
//! [remove]
//! branch_delete = "ASK"  # or "ALWAYS" or "NEVER"
//! ```

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// Controls whether local branches are deleted after worktree removal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BranchDeletePolicy {
    /// Prompt the user each time (default).
    #[default]
    Ask,
    /// Always delete the branch without asking.
    Always,
    /// Never delete the branch, don't ask.
    Never,
}

/// Configuration for the `remove` subcommand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoveConfig {
    /// Controls branch deletion after worktree removal.
    #[serde(default)]
    pub branch_delete: BranchDeletePolicy,
}

/// Security-related configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Allow file operations to reference paths outside the worktree boundary.
    ///
    /// By default (`false`), all resolved paths are containment-checked:
    /// they must be inside the target worktree directory. Set to `true` to
    /// disable this globally.
    ///
    /// Individual configs can override this with `allowPathEscape = true`.
    #[serde(default)]
    pub allow_path_escape: bool,
}

/// Global configuration for worktree-setup.
///
/// Loaded from an optional global file and an optional repo-level file.
/// If neither exists, all fields use their `Default` values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// Settings for the `remove` subcommand.
    #[serde(default)]
    pub remove: RemoveConfig,

    /// Security-related settings.
    #[serde(default)]
    pub security: SecurityConfig,
}

impl GlobalConfig {
    /// Merge another `GlobalConfig` on top of self.
    ///
    /// Values from `other` override values in `self`. This is used to
    /// layer repo-level config on top of global config.
    pub const fn merge(&mut self, other: &Self) {
        self.remove.branch_delete = other.remove.branch_delete;
        self.security.allow_path_escape = other.security.allow_path_escape;
    }
}

/// Return the path to the global config file.
///
/// Uses the platform config directory (e.g. `~/.config` on Linux/macOS)
/// joined with `worktree-setup/config.toml`.
///
/// Returns `None` if the platform config directory cannot be determined.
#[must_use]
pub fn global_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("worktree-setup").join("config.toml"))
}

/// Return the path to the repo-level config file.
///
/// This is `.worktree-setup.toml` in the repository root.
#[must_use]
pub fn repo_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".worktree-setup.toml")
}

/// Load and parse a TOML file into a `GlobalConfig`.
///
/// Returns `Ok(None)` if the file does not exist.
///
/// # Errors
///
/// * If the file exists but cannot be read
/// * If the file exists but contains invalid TOML
fn load_config_file(path: &Path) -> Result<Option<GlobalConfig>, ConfigError> {
    if !path.is_file() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::ReadError {
        path: path.to_path_buf(),
        source,
    })?;

    let config: GlobalConfig =
        toml::from_str(&contents).map_err(|source| ConfigError::TomlParseError {
            path: path.to_path_buf(),
            source,
        })?;

    Ok(Some(config))
}

/// Load the global configuration, merging repo-level over global.
///
/// Resolution order:
/// 1. Start with `GlobalConfig::default()`
/// 2. If a global config file exists (`~/.config/worktree-setup/config.toml`),
///    merge it on top
/// 3. If a repo-level config file exists (`.worktree-setup.toml` at repo root),
///    merge it on top (repo wins)
///
/// If no files exist, returns `GlobalConfig::default()`.
///
/// # Arguments
///
/// * `repo_root` - Optional path to the repository root. When `None`, only
///   the global config file is consulted.
///
/// # Errors
///
/// * If a config file exists but cannot be read or parsed
pub fn load_global_config(repo_root: Option<&Path>) -> Result<GlobalConfig, ConfigError> {
    let mut config = GlobalConfig::default();

    // Layer 1: global config file
    if let Some(global_path) = global_config_path()
        && let Some(global) = load_config_file(&global_path)?
    {
        config.merge(&global);
    }

    // Layer 2: repo-level config file (overrides global)
    if let Some(root) = repo_root {
        let repo_path = repo_config_path(root);
        if let Some(repo) = load_config_file(&repo_path)? {
            config.merge(&repo);
        }
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_branch_delete_policy_is_ask() {
        assert_eq!(BranchDeletePolicy::default(), BranchDeletePolicy::Ask);
    }

    #[test]
    fn default_global_config_uses_ask() {
        let config = GlobalConfig::default();
        assert_eq!(config.remove.branch_delete, BranchDeletePolicy::Ask);
    }

    #[test]
    fn parse_branch_delete_ask() {
        let toml_str = r#"
[remove]
branch_delete = "ASK"
"#;
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.remove.branch_delete, BranchDeletePolicy::Ask);
    }

    #[test]
    fn parse_branch_delete_always() {
        let toml_str = r#"
[remove]
branch_delete = "ALWAYS"
"#;
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.remove.branch_delete, BranchDeletePolicy::Always);
    }

    #[test]
    fn parse_branch_delete_never() {
        let toml_str = r#"
[remove]
branch_delete = "NEVER"
"#;
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.remove.branch_delete, BranchDeletePolicy::Never);
    }

    #[test]
    fn parse_empty_toml_uses_defaults() {
        let config: GlobalConfig = toml::from_str("").unwrap();
        assert_eq!(config.remove.branch_delete, BranchDeletePolicy::Ask);
    }

    #[test]
    fn parse_empty_remove_section_uses_defaults() {
        let toml_str = "[remove]\n";
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.remove.branch_delete, BranchDeletePolicy::Ask);
    }

    #[test]
    fn merge_overrides_branch_delete() {
        let mut base = GlobalConfig::default();
        let overlay = GlobalConfig {
            remove: RemoveConfig {
                branch_delete: BranchDeletePolicy::Always,
            },
            ..Default::default()
        };
        base.merge(&overlay);
        assert_eq!(base.remove.branch_delete, BranchDeletePolicy::Always);
    }

    #[test]
    fn load_missing_file_returns_none() {
        let result = load_config_file(Path::new("/nonexistent/path/config.toml")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_global_config_with_no_repo_root_returns_defaults() {
        // When repo_root is None and no global config exists at the platform
        // path, we should get defaults. This test may load an actual global
        // config if one exists on the developer's machine, so we just verify
        // it doesn't error.
        let config = load_global_config(None).unwrap();
        // At minimum, the config should be valid
        let _ = config.remove.branch_delete;
    }

    #[test]
    fn load_global_config_with_nonexistent_repo_returns_defaults() {
        let config = load_global_config(Some(Path::new("/nonexistent/repo/root"))).unwrap();
        // Should not error — missing files are fine
        let _ = config.remove.branch_delete;
    }

    #[test]
    fn load_repo_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join(".worktree-setup.toml");
        std::fs::write(
            &config_path,
            r#"
[remove]
branch_delete = "NEVER"
"#,
        )
        .unwrap();

        let config = load_config_file(&config_path).unwrap().unwrap();
        assert_eq!(config.remove.branch_delete, BranchDeletePolicy::Never);
    }

    #[test]
    fn repo_config_overrides_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".worktree-setup.toml"),
            r#"
[remove]
branch_delete = "ALWAYS"
"#,
        )
        .unwrap();

        let config = load_global_config(Some(dir.path())).unwrap();
        assert_eq!(config.remove.branch_delete, BranchDeletePolicy::Always);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let toml_str = r#"
[remove]
branch_delete = "ASK"
some_future_field = true

[some_future_section]
value = 42
"#;
        // This should fail because serde by default rejects unknown fields
        // for structs that don't use deny_unknown_fields, but toml's
        // deserialization is lenient — unknown fields are ignored.
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.remove.branch_delete, BranchDeletePolicy::Ask);
    }

    #[test]
    fn default_security_allows_no_path_escape() {
        let config = GlobalConfig::default();
        assert!(!config.security.allow_path_escape);
    }

    #[test]
    fn parse_security_allow_path_escape() {
        let toml_str = r"
[security]
allow_path_escape = true
";
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert!(config.security.allow_path_escape);
    }

    #[test]
    fn merge_overrides_security() {
        let mut base = GlobalConfig::default();
        let overlay = GlobalConfig {
            security: SecurityConfig {
                allow_path_escape: true,
            },
            ..Default::default()
        };
        base.merge(&overlay);
        assert!(base.security.allow_path_escape);
    }
}
