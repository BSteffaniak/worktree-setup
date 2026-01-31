//! Operation planning - enumerate operations with file counts without executing.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::{Path, PathBuf};

use worktree_setup_config::LoadedConfig;
use worktree_setup_copy::count_files;
use worktree_setup_git::{get_unstaged_and_untracked_files, open_repo};

use crate::ApplyConfigOptions;
use crate::error::OperationError;

/// Type of operation to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationType {
    /// Create a symlink.
    Symlink,
    /// Copy file/directory (skip if exists).
    Copy,
    /// Overwrite file/directory.
    Overwrite,
    /// Copy from glob pattern match.
    CopyGlob,
    /// Copy template file.
    Template,
    /// Copy unstaged/untracked file.
    Unstaged,
}

impl std::fmt::Display for OperationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Symlink => write!(f, "symlink"),
            Self::Copy => write!(f, "copy"),
            Self::Overwrite => write!(f, "overwrite"),
            Self::CopyGlob => write!(f, "copy"),
            Self::Template => write!(f, "template"),
            Self::Unstaged => write!(f, "unstaged"),
        }
    }
}

/// A planned operation with metadata for progress display.
#[derive(Debug, Clone)]
pub struct PlannedOperation {
    /// Display path (relative to config).
    pub display_path: String,
    /// Type of operation.
    pub operation_type: OperationType,
    /// Source path (absolute).
    pub source: PathBuf,
    /// Target path (absolute).
    pub target: PathBuf,
    /// Number of files (1 for single files, N for directories).
    pub file_count: u64,
    /// Whether this is a directory operation.
    pub is_directory: bool,
    /// Whether this operation will be skipped.
    pub will_skip: bool,
    /// Reason for skipping (if applicable).
    pub skip_reason: Option<String>,
}

/// Plan all operations for a config without executing.
///
/// This enumerates all operations that would be performed, along with file counts
/// for progress display. Operations are returned in execution order.
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
/// * If git operations fail (when planning unstaged files)
pub fn plan_operations(
    config: &LoadedConfig,
    main_worktree: &Path,
    target_worktree: &Path,
    options: &ApplyConfigOptions,
) -> Result<Vec<PlannedOperation>, OperationError> {
    let mut operations = Vec::new();

    // Calculate relative path from repo root to config directory
    let config_relative_dir = config
        .config_dir
        .strip_prefix(main_worktree)
        .unwrap_or(&config.config_dir);

    // Plan symlinks
    for symlink_path in &config.config.symlinks {
        let source = main_worktree.join(config_relative_dir).join(symlink_path);
        let target = target_worktree.join(config_relative_dir).join(symlink_path);
        let display_path = config_relative_dir.join(symlink_path);

        let (will_skip, skip_reason) = if !source.exists() {
            (true, Some("not found".to_string()))
        } else if target.exists() || target.is_symlink() {
            (true, Some("exists".to_string()))
        } else {
            (false, None)
        };

        operations.push(PlannedOperation {
            display_path: display_path.to_string_lossy().to_string(),
            operation_type: OperationType::Symlink,
            source,
            target,
            file_count: 0, // Symlinks don't have file counts
            is_directory: false,
            will_skip,
            skip_reason,
        });
    }

    // Plan explicit copies
    for copy_path in &config.config.copy {
        let source = main_worktree.join(config_relative_dir).join(copy_path);
        let target = target_worktree.join(config_relative_dir).join(copy_path);
        let display_path = config_relative_dir.join(copy_path);

        let (will_skip, skip_reason, file_count, is_directory) = if !source.exists() {
            (true, Some("not found".to_string()), 0, false)
        } else if target.exists() {
            (true, Some("exists".to_string()), 0, false)
        } else {
            let is_dir = source.is_dir();
            let count = count_files(&source);
            (false, None, count, is_dir)
        };

        operations.push(PlannedOperation {
            display_path: display_path.to_string_lossy().to_string(),
            operation_type: OperationType::Copy,
            source,
            target,
            file_count,
            is_directory,
            will_skip,
            skip_reason,
        });
    }

    // Plan overwrites
    for overwrite_path in &config.config.overwrite {
        let source = main_worktree.join(config_relative_dir).join(overwrite_path);
        let target = target_worktree
            .join(config_relative_dir)
            .join(overwrite_path);
        let display_path = config_relative_dir.join(overwrite_path);

        let (will_skip, skip_reason, file_count, is_directory) = if !source.exists() {
            (true, Some("not found".to_string()), 0, false)
        } else {
            let is_dir = source.is_dir();
            let count = count_files(&source);
            (false, None, count, is_dir)
        };

        operations.push(PlannedOperation {
            display_path: display_path.to_string_lossy().to_string(),
            operation_type: OperationType::Overwrite,
            source,
            target,
            file_count,
            is_directory,
            will_skip,
            skip_reason,
        });
    }

    // Plan glob copies
    for pattern in &config.config.copy_glob {
        let search_dir = main_worktree.join(config_relative_dir);
        let full_pattern = search_dir.join(pattern).to_string_lossy().to_string();

        for entry in glob::glob(&full_pattern)? {
            if let Ok(source) = entry {
                if let Ok(rel_path) = source.strip_prefix(&search_dir) {
                    let target = target_worktree.join(config_relative_dir).join(rel_path);
                    let display_path = config_relative_dir.join(rel_path);

                    let (will_skip, skip_reason) = if target.exists() {
                        (true, Some("exists".to_string()))
                    } else {
                        (false, None)
                    };

                    // Glob matches are always files (globs don't match directories well)
                    operations.push(PlannedOperation {
                        display_path: display_path.to_string_lossy().to_string(),
                        operation_type: OperationType::CopyGlob,
                        source,
                        target,
                        file_count: 1,
                        is_directory: false,
                        will_skip,
                        skip_reason,
                    });
                }
            }
        }
    }

    // Plan templates
    for template in &config.config.templates {
        let source = main_worktree
            .join(config_relative_dir)
            .join(&template.source);
        let target = target_worktree
            .join(config_relative_dir)
            .join(&template.target);
        let display_path = format!(
            "{} -> {}",
            config_relative_dir.join(&template.source).display(),
            config_relative_dir.join(&template.target).display()
        );

        let (will_skip, skip_reason) = if !source.exists() {
            (true, Some("not found".to_string()))
        } else if target.exists() {
            (true, Some("exists".to_string()))
        } else {
            (false, None)
        };

        operations.push(PlannedOperation {
            display_path,
            operation_type: OperationType::Template,
            source,
            target,
            file_count: 1,
            is_directory: false,
            will_skip,
            skip_reason,
        });
    }

    // Plan unstaged files
    let should_copy_unstaged = options.copy_unstaged.unwrap_or(config.config.copy_unstaged);

    if should_copy_unstaged {
        let repo = open_repo(main_worktree)?;
        let files = get_unstaged_and_untracked_files(&repo)?;

        for file in files {
            let source = main_worktree.join(&file);
            let target = target_worktree.join(&file);

            // Only plan if source exists
            if source.exists() {
                operations.push(PlannedOperation {
                    display_path: file,
                    operation_type: OperationType::Unstaged,
                    source,
                    target,
                    file_count: 1,
                    is_directory: false,
                    will_skip: false,
                    skip_reason: None,
                });
            }
        }
    }

    Ok(operations)
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
    fn test_plan_operations_basic() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create source files
        fs::create_dir_all(main_dir.path().join("data")).unwrap();
        fs::write(main_dir.path().join("config.json"), "{}").unwrap();
        fs::write(main_dir.path().join("settings.json"), "{}").unwrap();

        let config = create_test_config(main_dir.path());
        let options = ApplyConfigOptions::default();

        let ops = plan_operations(&config, main_dir.path(), target_dir.path(), &options).unwrap();

        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].operation_type, OperationType::Symlink);
        assert_eq!(ops[1].operation_type, OperationType::Copy);
        assert_eq!(ops[2].operation_type, OperationType::Overwrite);
    }

    #[test]
    fn test_plan_operations_skip_existing() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create source and target files
        fs::write(main_dir.path().join("config.json"), "{}").unwrap();
        fs::write(target_dir.path().join("config.json"), "existing").unwrap();

        let config = LoadedConfig {
            config: Config {
                copy: vec!["config.json".to_string()],
                ..Default::default()
            },
            config_path: main_dir.path().join("worktree.config.toml"),
            config_dir: main_dir.path().to_path_buf(),
            relative_path: "worktree.config.toml".to_string(),
        };
        let options = ApplyConfigOptions::default();

        let ops = plan_operations(&config, main_dir.path(), target_dir.path(), &options).unwrap();

        assert_eq!(ops.len(), 1);
        assert!(ops[0].will_skip);
        assert_eq!(ops[0].skip_reason, Some("exists".to_string()));
    }

    #[test]
    fn test_plan_operations_directory_file_count() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create a directory with files
        let data_dir = main_dir.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join("file1.txt"), "1").unwrap();
        fs::write(data_dir.join("file2.txt"), "2").unwrap();
        fs::create_dir(data_dir.join("subdir")).unwrap();
        fs::write(data_dir.join("subdir/file3.txt"), "3").unwrap();

        let config = LoadedConfig {
            config: Config {
                copy: vec!["data".to_string()],
                ..Default::default()
            },
            config_path: main_dir.path().join("worktree.config.toml"),
            config_dir: main_dir.path().to_path_buf(),
            relative_path: "worktree.config.toml".to_string(),
        };
        let options = ApplyConfigOptions::default();

        let ops = plan_operations(&config, main_dir.path(), target_dir.path(), &options).unwrap();

        assert_eq!(ops.len(), 1);
        assert!(ops[0].is_directory);
        assert_eq!(ops[0].file_count, 3);
    }
}
