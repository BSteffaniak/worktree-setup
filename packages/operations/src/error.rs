//! Error types for file operations.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::PathBuf;

use thiserror::Error;

/// Errors that can occur during file operations.
#[derive(Debug, Error)]
pub enum OperationError {
    /// IO error during file operation.
    #[error("IO error at {}: {source}", path.display())]
    IoError {
        /// Path where the error occurred.
        path: PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to create symlink.
    #[error("Failed to create symlink from {} to {}: {error}", source.display(), target.display())]
    SymlinkError {
        /// Source path.
        source: PathBuf,
        /// Target path.
        target: PathBuf,
        /// The underlying IO error.
        #[source]
        error: std::io::Error,
    },

    /// Failed to copy file.
    #[error("Failed to copy from {} to {}: {error}", source.display(), target.display())]
    CopyError {
        /// Source path.
        source: PathBuf,
        /// Target path.
        target: PathBuf,
        /// The underlying IO error.
        #[source]
        error: std::io::Error,
    },

    /// Glob pattern error.
    #[error("Glob pattern error: {0}")]
    GlobError(#[from] glob::PatternError),

    /// Git operation error.
    #[error("Git error: {0}")]
    GitError(#[from] worktree_setup_git::GitError),

    /// Copy module error.
    #[error("Copy error: {0}")]
    CopyModuleError(String),
}
