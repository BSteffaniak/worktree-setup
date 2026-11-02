//! Fast parallel file copying with progress tracking.
//!
//! This crate provides efficient file and directory copying operations with:
//!
//! * Parallel directory enumeration using `jwalk`
//! * Parallel file copying using `rayon`
//! * Copy-on-write support via `reflink-copy` (APFS, Btrfs, `ReFS`)
//! * Progress callbacks for UI integration
//! * Fast file counting
//!
//! # Example
//!
//! ```rust,ignore
//! use worktree_setup_copy::{copy_directory, CopyProgress, count_files};
//!
//! // Count files first for progress bar
//! let total = count_files(source);
//!
//! copy_directory(source, target, |progress: &CopyProgress| {
//!     println!("{}/{} files copied", progress.files_copied, progress.files_total);
//! })?;
//! ```

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

mod backend;
mod copy;
mod count;
mod error;
mod progress;
mod strategy;

pub use copy::{CopyResult, copy_directory, copy_file, overwrite_file};
pub use copy::{
    copy_directory_with_strategy, copy_file_with_strategy, overwrite_file_with_strategy,
};
pub use count::{count_files, count_files_with_progress, disk_usage};
pub use error::CopyError;
pub use progress::{CopyProgress, ProgressTracker};
pub use strategy::CopyStrategy;
