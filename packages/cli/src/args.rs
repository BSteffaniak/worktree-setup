//! CLI argument definitions.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// CLI arguments for worktree-setup.
#[derive(Debug, Parser)]
#[allow(clippy::struct_excessive_bools)]
#[command(
    name = "worktree-setup",
    about = "Set up git worktrees with project-specific configurations",
    version
)]
pub struct Args {
    /// Subcommand to run. If omitted, runs the default create+setup flow.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path to the target worktree (used when no subcommand is given).
    #[arg(index = 1)]
    pub target_path: Option<PathBuf>,

    /// Create worktree from this branch.
    #[arg(long)]
    pub branch: Option<String>,

    /// Create a new branch for the worktree.
    #[arg(long)]
    pub new_branch: Option<String>,

    /// Track a remote branch (fetches from origin first).
    #[arg(long)]
    pub remote_branch: Option<String>,

    /// Specific config file to use (can be specified multiple times).
    #[arg(long = "config", short = 'c')]
    pub configs: Vec<String>,

    /// Skip running post-setup commands.
    #[arg(long = "no-install")]
    pub no_install: bool,

    /// Copy unstaged and untracked files from main worktree.
    #[arg(long)]
    pub unstaged: bool,

    /// Skip copying unstaged files (overrides config).
    #[arg(long = "no-unstaged")]
    pub no_unstaged: bool,

    /// List discovered configs and exit.
    #[arg(long)]
    pub list: bool,

    /// Run without prompts (requires target-path).
    #[arg(long)]
    pub non_interactive: bool,

    /// Disable progress bars (useful for CI environments).
    #[arg(long = "no-progress")]
    pub no_progress: bool,

    /// Enable verbose output.
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

/// Subcommands for worktree-setup.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Apply worktree configs to an existing directory.
    ///
    /// Discovers worktree configuration files and applies file operations
    /// (symlinks, copies, templates) and/or runs post-setup commands.
    /// Defaults to the current directory if no path is given.
    Setup(SetupArgs),
}

/// Arguments for the `setup` subcommand.
#[derive(Debug, Parser)]
#[allow(clippy::struct_excessive_bools)]
pub struct SetupArgs {
    /// Path to the target directory (defaults to current directory).
    #[arg(index = 1)]
    pub target_path: Option<PathBuf>,

    /// Specific config file to use (can be specified multiple times).
    #[arg(long = "config", short = 'c')]
    pub configs: Vec<String>,

    /// Default file operations checkbox to unchecked.
    #[arg(long = "no-files")]
    pub no_files: bool,

    /// Default post-setup commands checkbox to unchecked.
    #[arg(long = "no-install")]
    pub no_install: bool,

    /// Default overwrite existing files checkbox to checked.
    #[arg(long)]
    pub overwrite: bool,

    /// Copy unstaged and untracked files from main worktree.
    #[arg(long)]
    pub unstaged: bool,

    /// Skip copying unstaged files (overrides config).
    #[arg(long = "no-unstaged")]
    pub no_unstaged: bool,

    /// Run without prompts, using defaults (respecting flags).
    #[arg(long)]
    pub non_interactive: bool,

    /// Disable progress bars (useful for CI environments).
    #[arg(long = "no-progress")]
    pub no_progress: bool,

    /// Enable verbose output.
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

impl Args {
    /// Determine if we should copy unstaged files based on flags.
    ///
    /// Returns `Some(true)` if `--unstaged`, `Some(false)` if `--no-unstaged`,
    /// or `None` to use the config default.
    #[must_use]
    pub const fn copy_unstaged_override(&self) -> Option<bool> {
        if self.no_unstaged {
            Some(false)
        } else if self.unstaged {
            Some(true)
        } else {
            None
        }
    }

    /// Determine if we should run post-setup commands.
    #[must_use]
    pub const fn should_run_install(&self) -> bool {
        !self.no_install
    }

    /// Determine if we should show progress bars.
    #[must_use]
    pub const fn should_show_progress(&self) -> bool {
        !self.no_progress
    }
}

impl SetupArgs {
    /// Determine if we should copy unstaged files based on flags.
    ///
    /// Returns `Some(true)` if `--unstaged`, `Some(false)` if `--no-unstaged`,
    /// or `None` to use the config default.
    #[must_use]
    pub const fn copy_unstaged_override(&self) -> Option<bool> {
        if self.no_unstaged {
            Some(false)
        } else if self.unstaged {
            Some(true)
        } else {
            None
        }
    }

    /// Determine if we should show progress bars.
    #[must_use]
    pub const fn should_show_progress(&self) -> bool {
        !self.no_progress
    }
}
