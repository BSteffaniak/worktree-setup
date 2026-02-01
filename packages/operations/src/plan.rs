//! Operation planning - enumerate operations with file counts without executing.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::{Path, PathBuf};

use worktree_setup_config::LoadedConfig;
use worktree_setup_copy::count_files_with_progress;

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

/// Resolve a path from config, handling repo-root-relative paths.
///
/// Paths starting with `/` are relative to the base (repo root).
/// Other paths are relative to the config file's directory.
///
/// # Arguments
///
/// * `base` - The base path (main_worktree or target_worktree)
/// * `config_relative_dir` - Relative path from repo root to config directory
/// * `path` - The path from the config file
///
/// # Returns
///
/// A tuple of (resolved_path, display_path)
fn resolve_path(base: &Path, config_relative_dir: &Path, path: &str) -> (PathBuf, String) {
    if let Some(stripped) = path.strip_prefix('/') {
        // Repo-root-relative path (e.g., "/.nix" -> ".nix")
        (base.join(stripped), stripped.to_string())
    } else {
        // Config-relative path (e.g., "data" -> "apps/myapp/data")
        let display = config_relative_dir.join(path);
        (base.join(&display), display.to_string_lossy().to_string())
    }
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
/// * If glob pattern matching fails
pub fn plan_operations(
    config: &LoadedConfig,
    main_worktree: &Path,
    target_worktree: &Path,
    options: &ApplyConfigOptions,
) -> Result<Vec<PlannedOperation>, OperationError> {
    plan_operations_with_progress(
        config,
        main_worktree,
        target_worktree,
        options,
        &|_, _, _, _| {},
    )
}

/// Plan all operations for a config with progress reporting.
///
/// This is like `plan_operations` but reports progress during scanning,
/// which is useful for displaying a progress bar to the user.
///
/// The progress callback receives:
/// - `current_op`: Current operation index (1-based)
/// - `total_ops`: Total number of operations to scan
/// - `path`: Path being scanned
/// - `file_count`: Current file count (Some during directory scan, None for quick checks)
///
/// # Arguments
///
/// * `config` - The loaded configuration
/// * `main_worktree` - Path to the main worktree (source)
/// * `target_worktree` - Path to the target worktree (destination)
/// * `options` - Options to override config settings
/// * `on_progress` - Progress callback
///
/// # Errors
///
/// * If glob pattern matching fails
pub fn plan_operations_with_progress<F>(
    config: &LoadedConfig,
    main_worktree: &Path,
    target_worktree: &Path,
    _options: &ApplyConfigOptions,
    on_progress: &F,
) -> Result<Vec<PlannedOperation>, OperationError>
where
    F: Fn(usize, usize, &str, Option<u64>),
{
    let mut operations = Vec::new();

    // Calculate relative path from repo root to config directory
    let config_relative_dir = config
        .config_dir
        .strip_prefix(main_worktree)
        .unwrap_or(&config.config_dir);

    // Calculate total operations (excluding unstaged - those are handled separately)
    let total_ops = config.config.symlinks.len()
        + config.config.copy.len()
        + config.config.overwrite.len()
        + config.config.copy_glob.len()
        + config.config.templates.len();

    let mut current_op = 0usize;

    // Plan symlinks
    for symlink_path in &config.config.symlinks {
        current_op += 1;
        let (source, display_str) = resolve_path(main_worktree, config_relative_dir, symlink_path);
        let (target, _) = resolve_path(target_worktree, config_relative_dir, symlink_path);

        on_progress(current_op, total_ops, &display_str, None);

        let (will_skip, skip_reason) = if !source.exists() {
            (true, Some("not found".to_string()))
        } else if target.exists() || target.is_symlink() {
            (true, Some("exists".to_string()))
        } else {
            (false, None)
        };

        operations.push(PlannedOperation {
            display_path: display_str,
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
        current_op += 1;
        let (source, display_str) = resolve_path(main_worktree, config_relative_dir, copy_path);
        let (target, _) = resolve_path(target_worktree, config_relative_dir, copy_path);

        on_progress(current_op, total_ops, &display_str, None);

        let (will_skip, skip_reason, file_count, is_directory) = if !source.exists() {
            (true, Some("not found".to_string()), 0, false)
        } else if target.exists() {
            (true, Some("exists".to_string()), 0, false)
        } else {
            let is_dir = source.is_dir();
            let count = if is_dir {
                count_files_with_progress(&source, |n| {
                    on_progress(current_op, total_ops, &display_str, Some(n));
                })
            } else {
                1
            };
            (false, None, count, is_dir)
        };

        operations.push(PlannedOperation {
            display_path: display_str,
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
        current_op += 1;
        let (source, display_str) =
            resolve_path(main_worktree, config_relative_dir, overwrite_path);
        let (target, _) = resolve_path(target_worktree, config_relative_dir, overwrite_path);

        on_progress(current_op, total_ops, &display_str, None);

        let (will_skip, skip_reason, file_count, is_directory) = if !source.exists() {
            (true, Some("not found".to_string()), 0, false)
        } else {
            let is_dir = source.is_dir();
            let count = if is_dir {
                count_files_with_progress(&source, |n| {
                    on_progress(current_op, total_ops, &display_str, Some(n));
                })
            } else {
                1
            };
            (false, None, count, is_dir)
        };

        operations.push(PlannedOperation {
            display_path: display_str,
            operation_type: OperationType::Overwrite,
            source,
            target,
            file_count,
            is_directory,
            will_skip,
            skip_reason,
        });
    }

    // Plan glob copies (each pattern counts as 1 operation for progress)
    for pattern in &config.config.copy_glob {
        current_op += 1;

        // Handle repo-root-relative glob patterns
        let (search_dir, display_prefix, glob_pattern) =
            if let Some(stripped) = pattern.strip_prefix('/') {
                (main_worktree.to_path_buf(), PathBuf::new(), stripped)
            } else {
                (
                    main_worktree.join(config_relative_dir),
                    config_relative_dir.to_path_buf(),
                    pattern.as_str(),
                )
            };

        let full_pattern = search_dir.join(glob_pattern).to_string_lossy().to_string();

        on_progress(current_op, total_ops, pattern, None);

        for entry in glob::glob(&full_pattern)? {
            if let Ok(source) = entry {
                if let Ok(rel_path) = source.strip_prefix(&search_dir) {
                    let target = if pattern.starts_with('/') {
                        target_worktree.join(rel_path)
                    } else {
                        target_worktree.join(config_relative_dir).join(rel_path)
                    };
                    let display_path = if display_prefix.as_os_str().is_empty() {
                        rel_path.to_path_buf()
                    } else {
                        display_prefix.join(rel_path)
                    };

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
        current_op += 1;
        let (source, source_display) =
            resolve_path(main_worktree, config_relative_dir, &template.source);
        let (target, target_display) =
            resolve_path(target_worktree, config_relative_dir, &template.target);
        let display_path = format!("{source_display} -> {target_display}");

        on_progress(current_op, total_ops, &display_path, None);

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

    // Note: Unstaged files are NOT planned here - they should be handled separately
    // by the caller who can show a "Checking git status..." spinner first.
    // This avoids the git operation being part of the planning progress bar.

    Ok(operations)
}

/// Plan unstaged file operations.
///
/// This is separate from `plan_operations` so the caller can show a different
/// progress indicator for the git status check.
///
/// # Arguments
///
/// * `unstaged_files` - List of unstaged/untracked file paths from git
/// * `main_worktree` - Path to the main worktree (source)
/// * `target_worktree` - Path to the target worktree (destination)
///
/// # Returns
///
/// Vector of planned operations for unstaged files
pub fn plan_unstaged_operations(
    unstaged_files: &[String],
    main_worktree: &Path,
    target_worktree: &Path,
) -> Vec<PlannedOperation> {
    let mut operations = Vec::new();

    for file in unstaged_files {
        let source = main_worktree.join(file);
        let target = target_worktree.join(file);

        // Only plan if source exists
        if source.exists() {
            operations.push(PlannedOperation {
                display_path: file.clone(),
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

    operations
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

    #[test]
    fn test_plan_operations_with_progress_callback() {
        use std::cell::RefCell;

        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create source files
        fs::create_dir_all(main_dir.path().join("data")).unwrap();
        fs::write(main_dir.path().join("config.json"), "{}").unwrap();

        let config = LoadedConfig {
            config: Config {
                symlinks: vec!["data".to_string()],
                copy: vec!["config.json".to_string()],
                ..Default::default()
            },
            config_path: main_dir.path().join("worktree.config.toml"),
            config_dir: main_dir.path().to_path_buf(),
            relative_path: "worktree.config.toml".to_string(),
        };
        let options = ApplyConfigOptions::default();

        let progress_calls = RefCell::new(Vec::new());
        let ops = plan_operations_with_progress(
            &config,
            main_dir.path(),
            target_dir.path(),
            &options,
            &|current, total, path, _file_count| {
                progress_calls
                    .borrow_mut()
                    .push((current, total, path.to_string()));
            },
        )
        .unwrap();

        let calls = progress_calls.into_inner();
        assert_eq!(ops.len(), 2);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], (1, 2, "data".to_string()));
        assert_eq!(calls[1], (2, 2, "config.json".to_string()));
    }

    #[test]
    fn test_plan_unstaged_operations() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create source files
        fs::write(main_dir.path().join("modified.txt"), "content").unwrap();
        fs::write(main_dir.path().join("untracked.txt"), "content").unwrap();

        let unstaged = vec!["modified.txt".to_string(), "untracked.txt".to_string()];
        let ops = plan_unstaged_operations(&unstaged, main_dir.path(), target_dir.path());

        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].operation_type, OperationType::Unstaged);
        assert_eq!(ops[1].operation_type, OperationType::Unstaged);
    }

    #[test]
    fn test_plan_operations_repo_root_relative_paths() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create repo structure:
        // main_dir/
        //   .nix/
        //     flake.nix
        //   .envrc
        //   apps/
        //     myapp/
        //       worktree.config.toml (config is here)

        // Create root-level files
        fs::create_dir_all(main_dir.path().join(".nix")).unwrap();
        fs::write(main_dir.path().join(".nix/flake.nix"), "{}").unwrap();
        fs::write(main_dir.path().join(".envrc"), "use flake").unwrap();

        // Create app directory structure
        let app_dir = main_dir.path().join("apps/myapp");
        fs::create_dir_all(&app_dir).unwrap();

        // Config in subdirectory referencing root files with /
        let config = LoadedConfig {
            config: Config {
                copy: vec!["/.nix".to_string(), "/.envrc".to_string()],
                ..Default::default()
            },
            config_path: app_dir.join("worktree.config.toml"),
            config_dir: app_dir.clone(),
            relative_path: "apps/myapp/worktree.config.toml".to_string(),
        };
        let options = ApplyConfigOptions::default();

        let ops = plan_operations(&config, main_dir.path(), target_dir.path(), &options).unwrap();

        assert_eq!(ops.len(), 2);

        // Check .nix directory
        assert_eq!(ops[0].display_path, ".nix");
        assert_eq!(ops[0].source, main_dir.path().join(".nix"));
        assert_eq!(ops[0].target, target_dir.path().join(".nix"));
        assert!(ops[0].is_directory);
        assert!(!ops[0].will_skip);

        // Check .envrc file
        assert_eq!(ops[1].display_path, ".envrc");
        assert_eq!(ops[1].source, main_dir.path().join(".envrc"));
        assert_eq!(ops[1].target, target_dir.path().join(".envrc"));
        assert!(!ops[1].is_directory);
        assert!(!ops[1].will_skip);
    }

    #[test]
    fn test_plan_operations_mixed_paths() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create repo structure with both root and app-level files
        fs::write(main_dir.path().join(".envrc"), "use flake").unwrap();

        let app_dir = main_dir.path().join("apps/myapp");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("local.config"), "app config").unwrap();

        // Config with mixed paths: one root-relative, one config-relative
        let config = LoadedConfig {
            config: Config {
                copy: vec![
                    "/.envrc".to_string(),      // root-relative
                    "local.config".to_string(), // config-relative
                ],
                ..Default::default()
            },
            config_path: app_dir.join("worktree.config.toml"),
            config_dir: app_dir.clone(),
            relative_path: "apps/myapp/worktree.config.toml".to_string(),
        };
        let options = ApplyConfigOptions::default();

        let ops = plan_operations(&config, main_dir.path(), target_dir.path(), &options).unwrap();

        assert_eq!(ops.len(), 2);

        // Root-relative path: /.envrc -> .envrc
        assert_eq!(ops[0].display_path, ".envrc");
        assert_eq!(ops[0].source, main_dir.path().join(".envrc"));
        assert_eq!(ops[0].target, target_dir.path().join(".envrc"));

        // Config-relative path: local.config -> apps/myapp/local.config
        assert_eq!(ops[1].display_path, "apps/myapp/local.config");
        assert_eq!(ops[1].source, app_dir.join("local.config"));
        assert_eq!(
            ops[1].target,
            target_dir.path().join("apps/myapp/local.config")
        );
    }

    #[test]
    fn test_plan_operations_template_with_root_paths() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create template at root
        fs::write(main_dir.path().join(".env.template"), "KEY=value").unwrap();

        let app_dir = main_dir.path().join("apps/myapp");
        fs::create_dir_all(&app_dir).unwrap();

        let config = LoadedConfig {
            config: Config {
                templates: vec![worktree_setup_config::TemplateMapping {
                    source: "/.env.template".to_string(), // root-relative source
                    target: ".env.local".to_string(),     // config-relative target
                }],
                ..Default::default()
            },
            config_path: app_dir.join("worktree.config.toml"),
            config_dir: app_dir.clone(),
            relative_path: "apps/myapp/worktree.config.toml".to_string(),
        };
        let options = ApplyConfigOptions::default();

        let ops = plan_operations(&config, main_dir.path(), target_dir.path(), &options).unwrap();

        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation_type, OperationType::Template);
        assert_eq!(
            ops[0].display_path,
            ".env.template -> apps/myapp/.env.local"
        );
        assert_eq!(ops[0].source, main_dir.path().join(".env.template"));
        assert_eq!(
            ops[0].target,
            target_dir.path().join("apps/myapp/.env.local")
        );
    }
}
