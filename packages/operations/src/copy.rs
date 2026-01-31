//! File and directory copy operations.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::fs;
use std::path::Path;

use worktree_setup_copy::CopyProgress;

use crate::OperationResult;
use crate::error::OperationError;

/// Copy a file from source to target.
///
/// Only copies if the target doesn't exist.
///
/// # Arguments
///
/// * `source` - Source file path
/// * `target` - Target file path
///
/// # Errors
///
/// * If the copy operation fails
pub fn copy_file(source: &Path, target: &Path) -> Result<OperationResult, OperationError> {
    copy_file_with_progress(source, target, |_| {})
}

/// Copy a file from source to target with progress callback.
///
/// Only copies if the target doesn't exist.
///
/// # Arguments
///
/// * `source` - Source file path
/// * `target` - Target file path
/// * `on_progress` - Progress callback
///
/// # Errors
///
/// * If the copy operation fails
pub fn copy_file_with_progress<F>(
    source: &Path,
    target: &Path,
    on_progress: F,
) -> Result<OperationResult, OperationError>
where
    F: Fn(&CopyProgress),
{
    log::debug!("Copying file: {} -> {}", source.display(), target.display());

    let result = worktree_setup_copy::copy_file(source, target, on_progress)
        .map_err(|e| OperationError::CopyModuleError(e.to_string()))?;

    Ok(match result {
        worktree_setup_copy::CopyResult::Created { .. } => OperationResult::Created,
        worktree_setup_copy::CopyResult::Exists => OperationResult::Exists,
        worktree_setup_copy::CopyResult::SourceNotFound => OperationResult::Skipped,
    })
}

/// Copy a file from source to target, overwriting if it exists.
///
/// # Arguments
///
/// * `source` - Source file path
/// * `target` - Target file path
///
/// # Errors
///
/// * If the copy operation fails
pub fn overwrite_file(source: &Path, target: &Path) -> Result<OperationResult, OperationError> {
    overwrite_file_with_progress(source, target, |_| {})
}

/// Copy a file from source to target, overwriting if it exists, with progress callback.
///
/// # Arguments
///
/// * `source` - Source file path
/// * `target` - Target file path
/// * `on_progress` - Progress callback
///
/// # Errors
///
/// * If the copy operation fails
pub fn overwrite_file_with_progress<F>(
    source: &Path,
    target: &Path,
    on_progress: F,
) -> Result<OperationResult, OperationError>
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
        return Ok(OperationResult::Skipped);
    }

    let existed = target.exists();

    let result = worktree_setup_copy::overwrite_file(source, target, on_progress)
        .map_err(|e| OperationError::CopyModuleError(e.to_string()))?;

    Ok(match result {
        worktree_setup_copy::CopyResult::Created { .. } => {
            if existed {
                OperationResult::Overwritten
            } else {
                OperationResult::Created
            }
        }
        worktree_setup_copy::CopyResult::Exists => OperationResult::Exists,
        worktree_setup_copy::CopyResult::SourceNotFound => OperationResult::Skipped,
    })
}

/// Copy a directory recursively from source to target.
///
/// Only copies if the target doesn't exist.
///
/// # Arguments
///
/// * `source` - Source directory path
/// * `target` - Target directory path
///
/// # Errors
///
/// * If the copy operation fails
pub fn copy_directory(source: &Path, target: &Path) -> Result<OperationResult, OperationError> {
    copy_directory_with_progress(source, target, |_| {})
}

/// Copy a directory recursively from source to target with progress callback.
///
/// Only copies if the target doesn't exist. Uses parallel copying for performance.
///
/// # Arguments
///
/// * `source` - Source directory path
/// * `target` - Target directory path
/// * `on_progress` - Progress callback (called periodically, not for every file)
///
/// # Errors
///
/// * If the copy operation fails
pub fn copy_directory_with_progress<F>(
    source: &Path,
    target: &Path,
    on_progress: F,
) -> Result<OperationResult, OperationError>
where
    F: Fn(&CopyProgress) + Sync,
{
    log::debug!(
        "Copying directory: {} -> {}",
        source.display(),
        target.display()
    );

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| OperationError::IoError {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let result = worktree_setup_copy::copy_directory(source, target, on_progress)
        .map_err(|e| OperationError::CopyModuleError(e.to_string()))?;

    Ok(match result {
        worktree_setup_copy::CopyResult::Created { .. } => OperationResult::Created,
        worktree_setup_copy::CopyResult::Exists => OperationResult::Exists,
        worktree_setup_copy::CopyResult::SourceNotFound => OperationResult::Skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_copy_file() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.txt");
        let target = dir.path().join("target.txt");

        fs::write(&source, "content").unwrap();

        let result = copy_file(&source, &target).unwrap();
        assert_eq!(result, OperationResult::Created);
        assert!(target.exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "content");
    }

    #[test]
    fn test_copy_file_exists() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.txt");
        let target = dir.path().join("target.txt");

        fs::write(&source, "source content").unwrap();
        fs::write(&target, "target content").unwrap();

        let result = copy_file(&source, &target).unwrap();
        assert_eq!(result, OperationResult::Exists);
        // Target should not be overwritten
        assert_eq!(fs::read_to_string(&target).unwrap(), "target content");
    }

    #[test]
    fn test_overwrite_file() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.txt");
        let target = dir.path().join("target.txt");

        fs::write(&source, "new content").unwrap();
        fs::write(&target, "old content").unwrap();

        let result = overwrite_file(&source, &target).unwrap();
        assert_eq!(result, OperationResult::Overwritten);
        assert_eq!(fs::read_to_string(&target).unwrap(), "new content");
    }

    #[test]
    fn test_copy_directory() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source_dir");
        let target = dir.path().join("target_dir");

        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        fs::create_dir_all(source.join("subdir")).unwrap();
        fs::write(source.join("subdir/nested.txt"), "nested").unwrap();

        let result = copy_directory(&source, &target).unwrap();
        assert_eq!(result, OperationResult::Created);
        assert!(target.join("file.txt").exists());
        assert!(target.join("subdir/nested.txt").exists());
    }
}
