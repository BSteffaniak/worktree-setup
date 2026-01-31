//! Error types for copy operations.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::PathBuf;

/// Errors that can occur during copy operations.
#[derive(Debug, thiserror::Error)]
pub enum CopyError {
    /// Failed to read source directory.
    #[error("Failed to read directory {}: {io_error}", path.display())]
    ReadDirError {
        /// The directory path.
        path: PathBuf,
        /// The underlying IO error.
        io_error: std::io::Error,
    },

    /// Failed to create target directory.
    #[error("Failed to create directory {}: {io_error}", path.display())]
    CreateDirError {
        /// The directory path.
        path: PathBuf,
        /// The underlying IO error.
        io_error: std::io::Error,
    },

    /// Failed to copy a file.
    #[error("Failed to copy {} to {}: {io_error}", source_path.display(), target_path.display())]
    FileCopyError {
        /// Source file path.
        source_path: PathBuf,
        /// Target file path.
        target_path: PathBuf,
        /// The underlying IO error.
        io_error: std::io::Error,
    },

    /// Failed to read symlink target.
    #[error("Failed to read symlink {}: {io_error}", path.display())]
    ReadLinkError {
        /// The symlink path.
        path: PathBuf,
        /// The underlying IO error.
        io_error: std::io::Error,
    },

    /// Failed to create symlink.
    #[error("Failed to create symlink {}: {io_error}", path.display())]
    CreateSymlinkError {
        /// The symlink path.
        path: PathBuf,
        /// The underlying IO error.
        io_error: std::io::Error,
    },

    /// Failed to get file metadata.
    #[error("Failed to get metadata for {}: {io_error}", path.display())]
    MetadataError {
        /// The file path.
        path: PathBuf,
        /// The underlying IO error.
        io_error: std::io::Error,
    },

    /// Directory enumeration failed.
    #[error("Failed to enumerate directory {}: {message}", path.display())]
    EnumerationError {
        /// The directory path.
        path: PathBuf,
        /// Error message.
        message: String,
    },
}
