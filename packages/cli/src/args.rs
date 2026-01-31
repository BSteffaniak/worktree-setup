//! CLI argument definitions.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::PathBuf;

use clap::Parser;

/// CLI arguments for worktree-setup.
#[derive(Debug, Parser)]
#[command(
    name = "worktree-setup",
    about = "Set up git worktrees with project-specific configurations",
    version
)]
pub struct Args {
    /// Path to the target worktree.
    #[arg(index = 1)]
    pub target_path: Option<PathBuf>,

    /// Create worktree from this branch.
    #[arg(long)]
    pub branch: Option<String>,

    /// Create a new branch for the worktree.
    #[arg(long)]
    pub new_branch: Option<String>,

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

impl Args {
    /// Determine if we should copy unstaged files based on flags.
    ///
    /// Returns `Some(true)` if `--unstaged`, `Some(false)` if `--no-unstaged`,
    /// or `None` to use the config default.
    #[must_use]
    pub fn copy_unstaged_override(&self) -> Option<bool> {
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
    pub fn should_run_install(&self) -> bool {
        !self.no_install
    }

    /// Determine if we should show progress bars.
    #[must_use]
    pub fn should_show_progress(&self) -> bool {
        !self.no_progress
    }
}
