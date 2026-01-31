//! Progress bar utilities for the CLI.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::io::Write;
use std::sync::Arc;

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

/// Progress bar manager for copy operations.
pub struct ProgressManager {
    multi: Arc<MultiProgress>,
    enabled: bool,
}

impl ProgressManager {
    /// Create a new progress manager.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            multi: Arc::new(MultiProgress::new()),
            enabled,
        }
    }

    /// Create a progress bar for a directory copy operation.
    ///
    /// Returns a `ProgressBar` that shows file count progress.
    /// If progress is disabled, returns a hidden progress bar.
    #[must_use]
    pub fn create_file_bar(&self, label: &str, total: u64) -> ProgressBar {
        if !self.enabled {
            return ProgressBar::hidden();
        }

        let pb = self.multi.add(ProgressBar::new(total));
        pb.set_style(
            ProgressStyle::default_bar()
                .template("  {prefix:<30} [{bar:25.green/dim}] {pos}/{len} files")
                .expect("Invalid progress bar template")
                .progress_chars("━━─"),
        );
        pb.set_prefix(label.to_string());
        pb
    }

    /// Print a completed operation result line.
    ///
    /// Shows a checkmark for success, bullet for skipped.
    pub fn print_result(&self, label: &str, result: &str, is_success: bool) {
        if is_success {
            println!("{} {:<30} {}", "✓".green(), label, result.dimmed());
        } else {
            println!("{} {:<30} {}", "•".dimmed(), label, result.dimmed());
        }
        // Flush to ensure output appears immediately
        let _ = std::io::stdout().flush();
    }

    /// Print a result with file count for directory operations.
    pub fn print_result_with_count(&self, label: &str, result: &str, file_count: u64) {
        println!(
            "{} {:<30} {} ({} files)",
            "✓".green(),
            label,
            result.dimmed(),
            file_count
        );
        let _ = std::io::stdout().flush();
    }

    /// Create a progress bar for scanning operations.
    ///
    /// Shows current operation being scanned with file count during enumeration.
    /// If progress is disabled, returns a hidden progress bar.
    #[must_use]
    pub fn create_scanning_bar(&self, total: u64) -> ProgressBar {
        if !self.enabled {
            return ProgressBar::hidden();
        }

        let pb = self.multi.add(ProgressBar::new(total));
        pb.set_style(
            ProgressStyle::default_bar()
                .template(" Scanning [{bar:20.green/dim}] {pos}/{len} {msg}")
                .expect("Invalid progress bar template")
                .progress_chars("━━─"),
        );
        pb
    }

    /// Clear any active progress bars (for clean output after completion).
    pub fn clear(&self) {
        self.multi.clear().ok();
    }
}
