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

mod copy;
mod count;
mod error;
mod progress;

pub use copy::{CopyResult, copy_directory, copy_file, overwrite_file};
pub use count::count_files;
pub use error::CopyError;
pub use progress::{CopyProgress, ProgressTracker};
