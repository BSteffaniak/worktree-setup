//! Parallel file copying implementation.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::fs;
use std::path::Path;

use rayon::prelude::*;

use crate::error::CopyError;
use crate::progress::{CopyProgress, ProgressTracker};

/// Result of a copy operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyResult {
    /// Files were copied successfully.
    Created {
        /// Number of files copied.
        files_copied: u64,
    },
    /// Target already exists, operation skipped.
    Exists,
    /// Source does not exist, operation skipped.
    SourceNotFound,
}

/// Entry collected during directory enumeration.
#[derive(Debug, Clone)]
struct FileEntry {
    /// Source path.
    source: std::path::PathBuf,
    /// Target path.
    target: std::path::PathBuf,
    /// Whether this is a symlink.
    is_symlink: bool,
}

/// Copy a single file with progress callback.
///
/// Only copies if target doesn't exist.
///
/// # Arguments
///
/// * `source` - Source file path
/// * `target` - Target file path
/// * `on_progress` - Callback for progress updates
///
/// # Errors
///
/// * If the copy operation fails
pub fn copy_file<F>(source: &Path, target: &Path, on_progress: F) -> Result<CopyResult, CopyError>
where
    F: Fn(&CopyProgress),
{
    log::debug!("Copying file: {} -> {}", source.display(), target.display());

    if !source.exists() {
        log::debug!("Source does not exist");
        return Ok(CopyResult::SourceNotFound);
    }

    if target.exists() {
        log::debug!("Target already exists");
        return Ok(CopyResult::Exists);
    }

    // Report starting
    on_progress(&CopyProgress::new(
        1,
        0,
        Some(source.to_string_lossy().to_string()),
    ));

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| CopyError::CreateDirError {
            path: parent.to_path_buf(),
            io_error: e,
        })?;
    }

    // Try reflink first, fall back to regular copy
    copy_file_with_reflink(source, target)?;

    // Report complete
    on_progress(&CopyProgress::new(
        1,
        1,
        Some(source.to_string_lossy().to_string()),
    ));

    Ok(CopyResult::Created { files_copied: 1 })
}

/// Copy a single file, overwriting if it exists.
///
/// # Arguments
///
/// * `source` - Source file path
/// * `target` - Target file path
/// * `on_progress` - Callback for progress updates
///
/// # Errors
///
/// * If the copy operation fails
pub fn overwrite_file<F>(
    source: &Path,
    target: &Path,
    on_progress: F,
) -> Result<CopyResult, CopyError>
where
    F: Fn(&CopyProgress),
{
    log::debug!(
        "Overwriting file: {} -> {}",
        source.display(),
        target.display()
    );

    if !source.exists() {
        log::debug!("Source does not exist");
        return Ok(CopyResult::SourceNotFound);
    }

    // Report starting
    on_progress(&CopyProgress::new(
        1,
        0,
        Some(source.to_string_lossy().to_string()),
    ));

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| CopyError::CreateDirError {
            path: parent.to_path_buf(),
            io_error: e,
        })?;
    }

    // Try reflink first, fall back to regular copy
    copy_file_with_reflink(source, target)?;

    // Report complete
    on_progress(&CopyProgress::new(
        1,
        1,
        Some(source.to_string_lossy().to_string()),
    ));

    Ok(CopyResult::Created { files_copied: 1 })
}

/// Copy a directory with parallel enumeration and copying.
///
/// Only copies if target directory doesn't exist.
///
/// # Arguments
///
/// * `source` - Source directory path
/// * `target` - Target directory path
/// * `on_progress` - Callback for progress updates (called periodically, not for every file)
///
/// # Errors
///
/// * If enumeration fails
/// * If any file copy fails (fail-fast behavior)
pub fn copy_directory<F>(
    source: &Path,
    target: &Path,
    on_progress: F,
) -> Result<CopyResult, CopyError>
where
    F: Fn(&CopyProgress) + Sync,
{
    log::debug!(
        "Copying directory: {} -> {}",
        source.display(),
        target.display()
    );

    if !source.exists() {
        log::debug!("Source does not exist");
        return Ok(CopyResult::SourceNotFound);
    }

    if target.exists() {
        log::debug!("Target already exists");
        return Ok(CopyResult::Exists);
    }

    // Phase 1: Enumerate all files using jwalk (parallel)
    let entries = enumerate_directory(source, target)?;
    let total_files = entries.len() as u64;

    log::debug!("Found {} files to copy", total_files);

    if total_files == 0 {
        // Empty directory - just create the target
        fs::create_dir_all(target).map_err(|e| CopyError::CreateDirError {
            path: target.to_path_buf(),
            io_error: e,
        })?;
        return Ok(CopyResult::Created { files_copied: 0 });
    }

    // Create progress tracker
    let tracker = ProgressTracker::new();
    tracker.set_total(total_files);

    // Initial progress report
    on_progress(&tracker.snapshot(None));

    // Phase 2: Copy files in parallel using rayon
    // First, collect all unique parent directories and create them
    let mut dirs: std::collections::BTreeSet<std::path::PathBuf> =
        std::collections::BTreeSet::new();
    for entry in &entries {
        if let Some(parent) = entry.target.parent() {
            dirs.insert(parent.to_path_buf());
        }
    }

    for dir in &dirs {
        fs::create_dir_all(dir).map_err(|e| CopyError::CreateDirError {
            path: dir.clone(),
            io_error: e,
        })?;
    }

    // Copy files in parallel
    let tracker_ref = &tracker;
    let on_progress_ref = &on_progress;

    entries
        .par_iter()
        .try_for_each(|entry| -> Result<(), CopyError> {
            if entry.is_symlink {
                copy_symlink(&entry.source, &entry.target)?;
            } else {
                copy_file_with_reflink(&entry.source, &entry.target)?;
            }

            tracker_ref.increment_copied();

            // Report progress (not every file to avoid overhead)
            let copied = tracker_ref.copied();
            if copied % 100 == 0 || copied == total_files {
                on_progress_ref(
                    &tracker_ref.snapshot(Some(entry.source.to_string_lossy().to_string())),
                );
            }

            Ok(())
        })?;

    // Final progress report
    on_progress(&tracker.snapshot(None));

    Ok(CopyResult::Created {
        files_copied: total_files,
    })
}

/// Enumerate all files in a directory using jwalk for parallel traversal.
fn enumerate_directory(source: &Path, target: &Path) -> Result<Vec<FileEntry>, CopyError> {
    let mut entries = Vec::new();

    for entry in jwalk::WalkDir::new(source)
        .skip_hidden(false)
        .follow_links(false)
    {
        let entry = entry.map_err(|e| CopyError::EnumerationError {
            path: source.to_path_buf(),
            message: e.to_string(),
        })?;

        let source_path = entry.path();

        // Skip the root directory itself
        if source_path == source {
            continue;
        }

        // Skip directories (we only copy files and symlinks)
        let file_type = entry.file_type();
        if file_type.is_dir() {
            continue;
        }

        // Calculate relative path and target
        let rel_path =
            source_path
                .strip_prefix(source)
                .map_err(|_| CopyError::EnumerationError {
                    path: source_path.to_path_buf(),
                    message: "Failed to strip prefix".to_string(),
                })?;
        let target_path = target.join(rel_path);

        entries.push(FileEntry {
            source: source_path.to_path_buf(),
            target: target_path,
            is_symlink: file_type.is_symlink(),
        });
    }

    Ok(entries)
}

/// Copy a single file, trying reflink first then falling back to regular copy.
fn copy_file_with_reflink(source: &Path, target: &Path) -> Result<(), CopyError> {
    // Try reflink first (copy-on-write, instant on APFS/Btrfs/ReFS)
    match reflink_copy::reflink(source, target) {
        Ok(()) => {
            log::trace!("Reflinked {} -> {}", source.display(), target.display());
            Ok(())
        }
        Err(_) => {
            // Fall back to regular copy
            fs::copy(source, target).map_err(|e| CopyError::FileCopyError {
                source_path: source.to_path_buf(),
                target_path: target.to_path_buf(),
                io_error: e,
            })?;
            log::trace!("Copied {} -> {}", source.display(), target.display());
            Ok(())
        }
    }
}

/// Copy a symlink, preserving it as a symlink.
fn copy_symlink(source: &Path, target: &Path) -> Result<(), CopyError> {
    let link_target = fs::read_link(source).map_err(|e| CopyError::ReadLinkError {
        path: source.to_path_buf(),
        io_error: e,
    })?;

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&link_target, target).map_err(|e| {
            CopyError::CreateSymlinkError {
                path: target.to_path_buf(),
                io_error: e,
            }
        })?;
    }

    #[cfg(windows)]
    {
        // On Windows, we need to determine if it's a file or directory symlink
        if link_target.is_dir() {
            std::os::windows::fs::symlink_dir(&link_target, target).map_err(|e| {
                CopyError::CreateSymlinkError {
                    path: target.to_path_buf(),
                    io_error: e,
                }
            })?;
        } else {
            std::os::windows::fs::symlink_file(&link_target, target).map_err(|e| {
                CopyError::CreateSymlinkError {
                    path: target.to_path_buf(),
                    io_error: e,
                }
            })?;
        }
    }

    log::trace!(
        "Symlinked {} -> {} (target: {})",
        source.display(),
        target.display(),
        link_target.display()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::TempDir;

    #[test]
    fn test_copy_file_creates_new() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.txt");
        let target = dir.path().join("target.txt");

        fs::write(&source, "hello world").unwrap();

        let progress_count = AtomicU64::new(0);
        let result = copy_file(&source, &target, |_| {
            progress_count.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        assert!(matches!(result, CopyResult::Created { files_copied: 1 }));
        assert!(target.exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello world");
        assert!(progress_count.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn test_copy_file_exists() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.txt");
        let target = dir.path().join("target.txt");

        fs::write(&source, "source content").unwrap();
        fs::write(&target, "target content").unwrap();

        let result = copy_file(&source, &target, |_| {}).unwrap();

        assert_eq!(result, CopyResult::Exists);
        assert_eq!(fs::read_to_string(&target).unwrap(), "target content");
    }

    #[test]
    fn test_copy_file_source_not_found() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("nonexistent.txt");
        let target = dir.path().join("target.txt");

        let result = copy_file(&source, &target, |_| {}).unwrap();

        assert_eq!(result, CopyResult::SourceNotFound);
    }

    #[test]
    fn test_copy_directory() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source_dir");
        let target = dir.path().join("target_dir");

        // Create source structure
        fs::create_dir_all(source.join("subdir")).unwrap();
        fs::write(source.join("file1.txt"), "content1").unwrap();
        fs::write(source.join("subdir/file2.txt"), "content2").unwrap();

        let progress_updates = Arc::new(AtomicU64::new(0));
        let progress_updates_clone = Arc::clone(&progress_updates);

        let result = copy_directory(&source, &target, move |_| {
            progress_updates_clone.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        assert!(matches!(result, CopyResult::Created { files_copied: 2 }));
        assert!(target.join("file1.txt").exists());
        assert!(target.join("subdir/file2.txt").exists());
        assert_eq!(
            fs::read_to_string(target.join("file1.txt")).unwrap(),
            "content1"
        );
        assert_eq!(
            fs::read_to_string(target.join("subdir/file2.txt")).unwrap(),
            "content2"
        );
    }

    #[test]
    fn test_copy_directory_exists() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source_dir");
        let target = dir.path().join("target_dir");

        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();

        let result = copy_directory(&source, &target, |_| {}).unwrap();

        assert_eq!(result, CopyResult::Exists);
    }

    #[test]
    fn test_overwrite_file() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.txt");
        let target = dir.path().join("target.txt");

        fs::write(&source, "new content").unwrap();
        fs::write(&target, "old content").unwrap();

        let result = overwrite_file(&source, &target, |_| {}).unwrap();

        assert!(matches!(result, CopyResult::Created { files_copied: 1 }));
        assert_eq!(fs::read_to_string(&target).unwrap(), "new content");
    }
}
