//! Configuration types for worktree-setup.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A template file mapping from source to target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateMapping {
    /// Source file path (relative to config directory).
    pub source: String,
    /// Target file path (relative to config directory).
    pub target: String,
}

/// Worktree setup configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Human-readable description of this configuration.
    #[serde(default)]
    pub description: String,

    /// Paths to symlink from the main worktree (relative to config directory).
    #[serde(default)]
    pub symlinks: Vec<String>,

    /// Paths to copy only if they don't exist in target (relative to config directory).
    #[serde(default)]
    pub copy: Vec<String>,

    /// Paths to always overwrite in target (relative to config directory).
    #[serde(default)]
    pub overwrite: Vec<String>,

    /// Glob patterns to copy (relative to config directory).
    #[serde(default)]
    pub copy_glob: Vec<String>,

    /// Whether to copy unstaged/untracked files from main worktree.
    #[serde(default)]
    pub copy_unstaged: bool,

    /// Template file mappings (copy source to target if target doesn't exist).
    #[serde(default)]
    pub templates: Vec<TemplateMapping>,

    /// Commands to run after setup completes.
    #[serde(default)]
    pub post_setup: Vec<String>,
}

/// A loaded configuration with metadata.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// The parsed configuration.
    pub config: Config,
    /// Absolute path to the configuration file.
    pub config_path: PathBuf,
    /// Directory containing the configuration file.
    pub config_dir: PathBuf,
    /// Path relative to repository root.
    pub relative_path: String,
}
