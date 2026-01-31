//! Git operations for worktree-setup.
//!
//! This crate provides git operations using the `git2` library, including:
//!
//! * Repository discovery and information
//! * Worktree listing, creation, and management
//! * File status detection (unstaged, untracked)
//!
//! # Example
//!
//! ```rust,ignore
//! use worktree_setup_git::{open_repo, get_worktrees, create_worktree};
//!
//! let repo = open_repo(".")?;
//! let worktrees = get_worktrees(&repo)?;
//! println!("Found {} worktrees", worktrees.len());
//! ```

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

mod error;
mod repo;
mod status;
mod worktree;

pub use error::GitError;
pub use repo::{discover_repo, get_current_branch, get_local_branches, get_repo_root, open_repo};
pub use status::get_unstaged_and_untracked_files;
pub use worktree::{
    WorktreeCreateOptions, WorktreeInfo, create_worktree, get_main_worktree, get_worktrees,
};
