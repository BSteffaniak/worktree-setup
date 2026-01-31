//! File operations for worktree-setup.
//!
//! This crate provides file system operations for setting up worktrees:
//!
//! * Symlink creation
//! * File and directory copying
//! * Configuration application
//! * Operation planning with file counts
//!
//! # Example
//!
//! ```rust,ignore
//! use worktree_setup_operations::{plan_operations, execute_operation};
//!
//! // Plan operations first (for progress display)
//! let operations = plan_operations(&config, &main_path, &target_path, &options)?;
//!
//! // Execute each operation with progress callback
//! for op in &operations {
//!     execute_operation(op, |completed, total| {
//!         println!("{}/{} files", completed, total);
//!     })?;
//! }
//! ```

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

mod apply;
mod copy;
mod error;
mod plan;
mod symlink;

pub use apply::{
    ApplyConfigOptions, ApplyResult, OperationRecord, apply_config, execute_operation,
};
pub use copy::{
    copy_directory, copy_directory_with_progress, copy_file, copy_file_with_progress,
    overwrite_file, overwrite_file_with_progress,
};
pub use error::OperationError;
pub use plan::{OperationType, PlannedOperation, plan_operations};
pub use symlink::create_symlink;
pub use worktree_setup_copy::CopyProgress;

/// Result of a single file operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationResult {
    /// The operation created a new file/symlink.
    Created,
    /// The target already existed and was skipped.
    Exists,
    /// The source was not found and operation was skipped.
    Skipped,
    /// The target was overwritten.
    Overwritten,
}

impl std::fmt::Display for OperationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Exists => write!(f, "exists"),
            Self::Skipped => write!(f, "skipped"),
            Self::Overwritten => write!(f, "overwritten"),
        }
    }
}
