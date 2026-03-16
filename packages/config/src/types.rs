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

    /// Paths and glob patterns to delete when running `clean`.
    ///
    /// Supports exact relative paths (e.g., `"node_modules"`, `".turbo"`) and
    /// glob patterns (e.g., `"**/dist"`, `"*.log"`). Paths are relative to the
    /// config directory. Resolved paths must remain within the target worktree
    /// directory (containment check).
    #[serde(default)]
    pub clean: Vec<String>,

    /// Profile names this config belongs to.
    ///
    /// Keys are profile names. When a user runs `--profile foo`, any config
    /// that has a `"foo"` key here is auto-selected. The `ProfileDefinition`
    /// value carries optional defaults and additional config patterns.
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileDefinition>,

    /// Allow file operations to reference paths outside the worktree boundary.
    ///
    /// By default (`false`), all resolved paths are containment-checked:
    /// they must be inside the target worktree directory. Set to `true` to
    /// disable this check for this config (e.g., for referencing shared
    /// caches or directories outside the worktree).
    ///
    /// Can also be set globally in the global config under `[security]`.
    /// Per-config `true` overrides a global `false`.
    #[serde(default)]
    pub allow_path_escape: bool,
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

/// How the worktree should be created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CreationMethod {
    /// New branch, auto-named after the worktree directory.
    Auto,
    /// Use the current branch.
    Current,
    /// Track a remote branch (infers name from worktree directory).
    Remote,
    /// Detached HEAD at the current commit.
    Detach,
}

/// Keyword for post-setup command behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PostSetupKeyword {
    /// Run all post-setup commands without prompting.
    All,
    /// Skip all post-setup commands without prompting.
    None,
}

/// Controls which post-setup commands to run.
///
/// * `"all"` — run all commands (optionally filtered by `skipPostSetup`)
/// * `"none"` — skip all commands
/// * `["cmd1", "cmd2"]` — run only these specific commands (exact match)
///
/// When not set, the user is prompted interactively.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PostSetupMode {
    /// `"all"` or `"none"` keyword.
    Keyword(PostSetupKeyword),
    /// Specific commands to run (exact match against available commands).
    Commands(Vec<String>),
}

/// Defaults that a profile can provide, overriding interactive prompts.
///
/// All fields are `Option` — only set values are applied. Unset values
/// fall through to CLI flags, interactive prompts, or builtin defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDefaults {
    /// Copy unstaged/untracked files from main worktree.
    pub copy_unstaged: Option<bool>,
    /// Overwrite existing files during file operations.
    pub overwrite_existing: Option<bool>,

    /// Skip the "Create worktree?" confirmation and create automatically.
    pub auto_create: Option<bool>,
    /// How to create the worktree (skips the creation method picker).
    pub creation_method: Option<CreationMethod>,
    /// Base branch for new worktree branches.
    pub base_branch: Option<String>,
    /// Always create a new branch (auto-named after worktree directory).
    pub new_branch: Option<bool>,
    /// Remote name to use for remote branch operations.
    pub remote: Option<String>,

    /// Which post-setup commands to run.
    ///
    /// * `"all"` — run all without prompting
    /// * `"none"` — skip all without prompting
    /// * `["cmd1", "cmd2"]` — run only these (exact match), no prompt
    /// * Not set — prompt the user
    pub post_setup: Option<PostSetupMode>,
    /// Specific post-setup commands to skip (exact match).
    ///
    /// Only meaningful when `post_setup = "all"`. Commands listed here
    /// are excluded from the set of commands that would otherwise run.
    #[serde(default)]
    pub skip_post_setup: Vec<String>,
}

impl ProfileDefaults {
    /// Merge another `ProfileDefaults` on top of self.
    ///
    /// Values from `other` win when they are `Some` (or non-empty for `Vec`).
    pub fn merge(&mut self, other: &Self) {
        if other.copy_unstaged.is_some() {
            self.copy_unstaged = other.copy_unstaged;
        }
        if other.overwrite_existing.is_some() {
            self.overwrite_existing = other.overwrite_existing;
        }
        if other.auto_create.is_some() {
            self.auto_create = other.auto_create;
        }
        if other.creation_method.is_some() {
            self.creation_method.clone_from(&other.creation_method);
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
        if other.post_setup.is_some() {
            self.post_setup.clone_from(&other.post_setup);
        }
        if !other.skip_post_setup.is_empty() {
            self.skip_post_setup.clone_from(&other.skip_post_setup);
        }
    }
}

/// A profile definition as declared in a configuration file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDefinition {
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Additional config file patterns to auto-select.
    ///
    /// Patterns without a leading `/` are matched relative to the declaring
    /// config's directory. Patterns with a leading `/` are matched relative
    /// to the repository root. Uses glob syntax (e.g., `"*.config.toml"`).
    ///
    /// The declaring config is always implicitly included.
    #[serde(default)]
    pub configs: Vec<String>,
    /// Default settings for this profile.
    #[serde(default, flatten)]
    pub defaults: ProfileDefaults,
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
