//! Error types for git operations.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::PathBuf;

use thiserror::Error;

/// Errors that can occur during git operations.
#[derive(Debug, Error)]
pub enum GitError {
    /// Failed to open repository.
    #[error("Failed to open repository at {}: {source}", path.display())]
    OpenError {
        /// Path that was attempted.
        path: PathBuf,
        /// The underlying git2 error.
        #[source]
        source: git2::Error,
    },

    /// Failed to discover repository.
    #[error("Failed to discover repository from {}: {source}", path.display())]
    DiscoverError {
        /// Path that was attempted.
        path: PathBuf,
        /// The underlying git2 error.
        #[source]
        source: git2::Error,
    },

    /// Repository has no working directory.
    #[error("Repository has no working directory (bare repository)")]
    BareRepository,

    /// Failed to get worktree list.
    #[error("Failed to list worktrees: {0}")]
    WorktreeListError(#[source] git2::Error),

    /// Failed to create worktree.
    #[error("Failed to create worktree at {}: {source}", path.display())]
    WorktreeCreateError {
        /// Path where worktree creation was attempted.
        path: PathBuf,
        /// The underlying git2 error.
        #[source]
        source: git2::Error,
    },

    /// Failed to get repository status.
    #[error("Failed to get repository status: {0}")]
    StatusError(#[source] git2::Error),

    /// Failed to get HEAD reference.
    #[error("Failed to get HEAD reference: {0}")]
    HeadError(#[source] git2::Error),

    /// Failed to list branches.
    #[error("Failed to list branches: {0}")]
    BranchListError(#[source] git2::Error),

    /// No main worktree found.
    #[error("No main worktree found")]
    NoMainWorktree,

    /// Worktree not found.
    #[error("Worktree not found: {0}")]
    WorktreeNotFound(String),

    /// Path error.
    #[error("Invalid path: {}", .0.display())]
    InvalidPath(PathBuf),
}
