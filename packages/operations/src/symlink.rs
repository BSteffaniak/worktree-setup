//! Symlink operations.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::fs;
use std::path::Path;

use crate::OperationResult;
use crate::error::OperationError;

/// Create a symlink from source to target.
///
/// If the target already exists as a symlink, returns `Exists`.
/// If the source doesn't exist, returns `Skipped`.
/// If the target exists as a file/directory, it is removed first.
///
/// # Arguments
///
/// * `source` - Path to the source (what the symlink points to)
/// * `target` - Path where the symlink will be created
///
/// # Errors
///
/// * If the symlink cannot be created
/// * If an existing file/directory cannot be removed
pub fn create_symlink(source: &Path, target: &Path) -> Result<OperationResult, OperationError> {
    log::debug!(
        "Creating symlink: {} -> {}",
        target.display(),
        source.display()
    );

    // Check if target is already a symlink
    if target.is_symlink() {
        log::debug!("Target is already a symlink");
        return Ok(OperationResult::Exists);
    }

    // Check if source exists
    if !source.exists() {
        log::debug!("Source does not exist: {}", source.display());
        return Ok(OperationResult::Skipped);
    }

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| OperationError::IoError {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    // Remove existing file/directory if it exists (but not symlink - handled above)
    if target.exists() {
        log::debug!("Removing existing path: {}", target.display());
        if target.is_dir() {
            fs::remove_dir_all(target).map_err(|e| OperationError::IoError {
                path: target.to_path_buf(),
                source: e,
            })?;
        } else {
            fs::remove_file(target).map_err(|e| OperationError::IoError {
                path: target.to_path_buf(),
                source: e,
            })?;
        }
    }

    // Create the symlink
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target).map_err(|e| OperationError::SymlinkError {
            source: source.to_path_buf(),
            target: target.to_path_buf(),
            error: e,
        })?;
    }

    #[cfg(windows)]
    {
        if source.is_dir() {
            std::os::windows::fs::symlink_dir(source, target).map_err(|e| {
                OperationError::SymlinkError {
                    source: source.to_path_buf(),
                    target: target.to_path_buf(),
                    error: e,
                }
            })?;
        } else {
            std::os::windows::fs::symlink_file(source, target).map_err(|e| {
                OperationError::SymlinkError {
                    source: source.to_path_buf(),
                    target: target.to_path_buf(),
                    error: e,
                }
            })?;
        }
    }

    log::debug!("Created symlink successfully");
    Ok(OperationResult::Created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_create_symlink() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");

        fs::write(&source, "content").unwrap();

        let result = create_symlink(&source, &target).unwrap();
        assert_eq!(result, OperationResult::Created);
        assert!(target.is_symlink());
    }

    #[test]
    fn test_create_symlink_exists() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");

        fs::write(&source, "content").unwrap();

        // Create symlink first time
        create_symlink(&source, &target).unwrap();

        // Try again - should return Exists
        let result = create_symlink(&source, &target).unwrap();
        assert_eq!(result, OperationResult::Exists);
    }

    #[test]
    fn test_create_symlink_source_missing() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("nonexistent");
        let target = dir.path().join("target");

        let result = create_symlink(&source, &target).unwrap();
        assert_eq!(result, OperationResult::Skipped);
    }
}
