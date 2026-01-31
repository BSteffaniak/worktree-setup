//! Progress tracking for copy operations.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Progress information for a copy operation.
#[derive(Debug, Clone)]
pub struct CopyProgress {
    /// Total number of files to copy.
    pub files_total: u64,
    /// Number of files copied so far.
    pub files_copied: u64,
    /// Current file being copied (if any).
    pub current_file: Option<String>,
}

impl CopyProgress {
    /// Create a new progress report.
    #[must_use]
    pub fn new(files_total: u64, files_copied: u64, current_file: Option<String>) -> Self {
        Self {
            files_total,
            files_copied,
            current_file,
        }
    }

    /// Calculate progress as a percentage (0.0 to 100.0).
    #[must_use]
    pub fn percentage(&self) -> f64 {
        if self.files_total == 0 {
            100.0
        } else {
            (self.files_copied as f64 / self.files_total as f64) * 100.0
        }
    }
}

/// Thread-safe progress tracker using atomics.
#[derive(Debug)]
pub struct ProgressTracker {
    files_total: AtomicU64,
    files_copied: AtomicU64,
}

impl ProgressTracker {
    /// Create a new progress tracker.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            files_total: AtomicU64::new(0),
            files_copied: AtomicU64::new(0),
        })
    }

    /// Set the total number of files.
    pub fn set_total(&self, total: u64) {
        self.files_total.store(total, Ordering::SeqCst);
    }

    /// Increment the copied count by 1.
    pub fn increment_copied(&self) {
        self.files_copied.fetch_add(1, Ordering::SeqCst);
    }

    /// Get the current total.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.files_total.load(Ordering::SeqCst)
    }

    /// Get the current copied count.
    #[must_use]
    pub fn copied(&self) -> u64 {
        self.files_copied.load(Ordering::SeqCst)
    }

    /// Get a progress snapshot.
    #[must_use]
    pub fn snapshot(&self, current_file: Option<String>) -> CopyProgress {
        CopyProgress::new(self.total(), self.copied(), current_file)
    }
}

impl Default for ProgressTracker {
    fn default() -> Self {
        Self {
            files_total: AtomicU64::new(0),
            files_copied: AtomicU64::new(0),
        }
    }
}
