//! Configuration application.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::Path;

use worktree_setup_config::LoadedConfig;
use worktree_setup_copy::CopyProgress;
use worktree_setup_git::{get_unstaged_and_untracked_files, open_repo};

use crate::OperationResult;
use crate::copy::{
    copy_directory_with_progress, copy_file_with_progress, overwrite_file_with_progress,
};
use crate::error::OperationError;
use crate::plan::{OperationType, PlannedOperation, plan_operations, plan_unstaged_operations};
use crate::symlink::create_symlink;

/// Record of a single file operation.
#[derive(Debug, Clone)]
pub struct OperationRecord {
    /// Relative path to the file.
    pub path: String,
    /// Result of the operation.
    pub result: OperationResult,
}

/// Options for applying a configuration.
#[derive(Debug, Clone, Default)]
pub struct ApplyConfigOptions {
    /// Override `copy_unstaged` setting from config.
    pub copy_unstaged: Option<bool>,
}

/// Result of applying a configuration.
#[derive(Debug, Clone, Default)]
pub struct ApplyResult {
    /// Symlink operations performed.
    pub symlinks: Vec<OperationRecord>,
    /// Copy operations performed.
    pub copies: Vec<OperationRecord>,
    /// Overwrite operations performed.
    pub overwrites: Vec<OperationRecord>,
    /// Unstaged file copy operations performed.
    pub unstaged: Vec<OperationRecord>,
    /// Template operations performed.
    pub templates: Vec<OperationRecord>,
}

/// Apply a loaded configuration to a target worktree.
///
/// This is a convenience wrapper around `plan_operations` + `execute_operation`.
///
/// # Arguments
///
/// * `config` - The loaded configuration
/// * `main_worktree` - Path to the main worktree (source)
/// * `target_worktree` - Path to the target worktree (destination)
/// * `options` - Options to override config settings
///
/// # Errors
///
/// * If file operations fail
/// * If git operations fail (when copying unstaged files)
pub fn apply_config(
    config: &LoadedConfig,
    main_worktree: &Path,
    target_worktree: &Path,
    options: &ApplyConfigOptions,
) -> Result<ApplyResult, OperationError> {
    log::info!(
        "Applying config {} to {}",
        config.relative_path,
        target_worktree.display()
    );

    let mut result = ApplyResult::default();

    // Plan and execute regular operations
    let operations = plan_operations(config, main_worktree, target_worktree, options)?;

    for op in &operations {
        let op_result = execute_operation(op, |_, _| {})?;
        let record = OperationRecord {
            path: op.display_path.clone(),
            result: op_result,
        };

        match op.operation_type {
            OperationType::Symlink => result.symlinks.push(record),
            OperationType::Copy | OperationType::CopyGlob => result.copies.push(record),
            OperationType::Overwrite => result.overwrites.push(record),
            OperationType::Template => result.templates.push(record),
            OperationType::Unstaged => result.unstaged.push(record),
        }
    }

    // Handle unstaged files separately (requires git operations)
    let should_copy_unstaged = options.copy_unstaged.unwrap_or(config.config.copy_unstaged);

    if should_copy_unstaged {
        log::info!("Copying unstaged and untracked files");

        let repo = open_repo(main_worktree)?;
        let files = get_unstaged_and_untracked_files(&repo)?;
        let unstaged_ops = plan_unstaged_operations(&files, main_worktree, target_worktree);

        for op in &unstaged_ops {
            let op_result = execute_operation(op, |_, _| {})?;
            result.unstaged.push(OperationRecord {
                path: op.display_path.clone(),
                result: op_result,
            });
        }
    }

    Ok(result)
}

/// Execute a single planned operation with progress callback.
///
/// This function executes one operation that was previously planned by `plan_operations`.
/// For directory operations, the progress callback will be called periodically with
/// (files_completed, files_total).
///
/// # Arguments
///
/// * `op` - The planned operation to execute
/// * `on_progress` - Progress callback for directory operations
///
/// # Returns
///
/// The result of the operation (Created, Exists, Skipped, Overwritten)
///
/// # Errors
///
/// * If the operation fails
pub fn execute_operation<F>(
    op: &PlannedOperation,
    on_progress: F,
) -> Result<OperationResult, OperationError>
where
    F: Fn(u64, u64) + Sync,
{
    // If already marked as skip, return appropriate result
    if op.will_skip {
        return Ok(match op.skip_reason.as_deref() {
            Some("exists") => OperationResult::Exists,
            Some("not found") => OperationResult::Skipped,
            _ => OperationResult::Skipped,
        });
    }

    match op.operation_type {
        OperationType::Symlink => create_symlink(&op.source, &op.target),
        OperationType::Copy | OperationType::CopyGlob | OperationType::Template => {
            if op.is_directory {
                // Directory copy with progress
                let result = copy_directory_with_progress(
                    &op.source,
                    &op.target,
                    |progress: &CopyProgress| {
                        on_progress(progress.files_copied, progress.files_total);
                    },
                )?;
                // Report completion
                on_progress(op.file_count, op.file_count);
                Ok(result)
            } else {
                // Single file copy
                copy_file_with_progress(&op.source, &op.target, |progress: &CopyProgress| {
                    on_progress(progress.files_copied, progress.files_total);
                })
            }
        }
        OperationType::Overwrite | OperationType::Unstaged => {
            if op.is_directory {
                // For overwrite, we'd need to delete first then copy
                // For now, treat as regular copy (directory overwrites are rare)
                let result = copy_directory_with_progress(
                    &op.source,
                    &op.target,
                    |progress: &CopyProgress| {
                        on_progress(progress.files_copied, progress.files_total);
                    },
                )?;
                on_progress(op.file_count, op.file_count);
                Ok(result)
            } else {
                overwrite_file_with_progress(&op.source, &op.target, |progress: &CopyProgress| {
                    on_progress(progress.files_copied, progress.files_total);
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::TempDir;
    use worktree_setup_config::Config;

    fn create_test_config(dir: &Path) -> LoadedConfig {
        LoadedConfig {
            config: Config {
                description: "Test".to_string(),
                symlinks: vec!["data".to_string()],
                copy: vec!["config.json".to_string()],
                overwrite: vec!["settings.json".to_string()],
                ..Default::default()
            },
            config_path: dir.join("worktree.config.toml"),
            config_dir: dir.to_path_buf(),
            relative_path: "worktree.config.toml".to_string(),
        }
    }

    #[test]
    fn test_apply_config_symlinks() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create source directory
        fs::create_dir_all(main_dir.path().join("data")).unwrap();
        fs::write(main_dir.path().join("data/file.txt"), "content").unwrap();

        let config = create_test_config(main_dir.path());
        let options = ApplyConfigOptions::default();

        let result = apply_config(&config, main_dir.path(), target_dir.path(), &options).unwrap();

        assert_eq!(result.symlinks.len(), 1);
        assert_eq!(result.symlinks[0].result, OperationResult::Created);
        assert!(target_dir.path().join("data").is_symlink());
    }

    #[test]
    fn test_apply_config_copies() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create source file
        fs::write(main_dir.path().join("config.json"), "{}").unwrap();

        let config = create_test_config(main_dir.path());
        let options = ApplyConfigOptions::default();

        let result = apply_config(&config, main_dir.path(), target_dir.path(), &options).unwrap();

        assert!(!result.copies.is_empty());
        assert!(target_dir.path().join("config.json").exists());
    }
}
