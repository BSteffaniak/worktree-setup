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

    /// Failed to prune stale worktrees.
    #[error("Failed to prune worktrees: {0}")]
    WorktreePruneError(#[source] git2::Error),

    /// Failed to get repository status.
    #[error("Failed to get repository status: {0}")]
    StatusError(#[source] git2::Error),

    /// Failed to get HEAD reference.
    #[error("Failed to get HEAD reference: {0}")]
    HeadError(#[source] git2::Error),

    /// Failed to list branches.
    #[error("Failed to list branches: {0}")]
    BranchListError(#[source] git2::Error),

    /// Failed to list remotes.
    #[error("Failed to list remotes: {0}")]
    RemoteListError(#[source] git2::Error),

    /// Failed to fetch from remote.
    #[error("Failed to fetch from remote '{remote}': {source}")]
    FetchError {
        /// The remote that was being fetched.
        remote: String,
        /// The underlying git2 error.
        #[source]
        source: git2::Error,
    },

    /// Worktree not found.
    #[error("Worktree not found: {0}")]
    WorktreeNotFound(String),

    /// Failed to remove worktree.
    #[error("Failed to remove worktree at {path}: {message}")]
    WorktreeRemoveError {
        /// Path of the worktree that could not be removed.
        path: String,
        /// Error message from the git CLI.
        message: String,
    },

    /// Cannot remove the main worktree.
    #[error("Cannot remove the main worktree at {0}")]
    CannotRemoveMainWorktree(String),

    /// Failed to delete a branch.
    #[error("Failed to delete branch '{branch}': {message}")]
    BranchDeleteError {
        /// The branch name that could not be deleted.
        branch: String,
        /// Error message from the git CLI.
        message: String,
    },

    /// Path error.
    #[error("Invalid path: {}", .0.display())]
    InvalidPath(PathBuf),
}
