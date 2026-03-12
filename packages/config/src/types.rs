//! Configuration types for worktree-setup.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::collections::BTreeMap;
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

    /// Profile names this config belongs to.
    ///
    /// When a user runs `--profile foo`, any config that lists `"foo"` here
    /// will be auto-selected (in addition to central profiles file matches).
    #[serde(default)]
    pub profiles: Vec<String>,

    /// Per-profile default overrides declared by this config.
    ///
    /// Keys are profile names, values are `ProfileDefaults`. These override
    /// the central profiles file defaults for the same profile name.
    #[serde(default)]
    pub profile_defaults: BTreeMap<String, ProfileDefaults>,
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

/// Defaults that a profile can provide, overriding interactive prompts.
///
/// All fields are `Option` — only set values are applied. Unset values
/// fall through to CLI flags, interactive prompts, or builtin defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDefaults {
    /// Skip post-setup commands when `true`.
    pub skip_post_setup: Option<bool>,
    /// Copy unstaged/untracked files from main worktree.
    pub copy_unstaged: Option<bool>,
    /// Overwrite existing files during file operations.
    pub overwrite_existing: Option<bool>,
    /// Base branch for new worktree branches.
    pub base_branch: Option<String>,
    /// Always create a new branch (auto-named after worktree directory).
    pub new_branch: Option<bool>,
    /// Remote name to use for remote branch operations.
    pub remote: Option<String>,
}

impl ProfileDefaults {
    /// Merge another `ProfileDefaults` on top of self.
    ///
    /// Values from `other` win when they are `Some`.
    pub fn merge(&mut self, other: &Self) {
        if other.skip_post_setup.is_some() {
            self.skip_post_setup = other.skip_post_setup;
        }
        if other.copy_unstaged.is_some() {
            self.copy_unstaged = other.copy_unstaged;
        }
        if other.overwrite_existing.is_some() {
            self.overwrite_existing = other.overwrite_existing;
        }
        if other.base_branch.is_some() {
            self.base_branch.clone_from(&other.base_branch);
        }
        if other.new_branch.is_some() {
            self.new_branch = other.new_branch;
        }
        if other.remote.is_some() {
            self.remote.clone_from(&other.remote);
        }
    }
}

/// A profile definition as declared in the central profiles file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDefinition {
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Config file patterns to auto-select (substring match on relative paths).
    #[serde(default)]
    pub configs: Vec<String>,
    /// Default settings for this profile.
    #[serde(default, flatten)]
    pub defaults: ProfileDefaults,
}

/// Top-level structure of a `worktree.profiles.toml` / `.ts` file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesFile {
    /// Named profiles.
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileDefinition>,
}

/// A fully resolved profile ready for use by the CLI.
#[derive(Debug, Clone, Default)]
pub struct ResolvedProfile {
    /// Combined profile names.
    pub names: Vec<String>,
    /// Human-readable description (from the last profile with a description).
    pub description: String,
    /// Indices into the loaded config list to auto-select.
    pub config_indices: Vec<usize>,
    /// Merged defaults.
    pub defaults: ProfileDefaults,
}
