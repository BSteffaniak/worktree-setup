//! Operation planning - enumerate operations with file counts without executing.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use worktree_setup_config::LoadedConfig;
use worktree_setup_copy::count_files_with_progress;
use worktree_setup_glob::{GlobResolverOptions, is_glob_pattern, resolve_glob};

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
            Self::Copy | Self::CopyGlob => write!(f, "copy"),
            Self::Overwrite => write!(f, "overwrite"),
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
    /// Whether to force-overwrite existing targets.
    pub force_overwrite: bool,
}

/// Resolve a path from config, handling repo-root-relative paths.
///
/// Paths starting with `/` are relative to the base (repo root).
/// Other paths are relative to the config file's directory.
///
/// # Arguments
///
/// * `base` - The base path (`main_worktree` or `target_worktree`)
/// * `config_relative_dir` - Relative path from repo root to config directory
/// * `path` - The path from the config file
///
/// # Returns
///
/// A tuple of (`resolved_path`, `display_path`)
fn resolve_path(base: &Path, config_relative_dir: &Path, path: &str) -> (PathBuf, String) {
    path.strip_prefix('/').map_or_else(
        || {
            // Config-relative path (e.g., "data" -> "apps/myapp/data")
            let display = config_relative_dir.join(path);
            (base.join(&display), display.to_string_lossy().to_string())
        },
        |stripped| {
            // Repo-root-relative path (e.g., "/.nix" -> ".nix")
            (base.join(stripped), stripped.to_string())
        },
    )
}

/// Check whether a resolved path escapes the containment boundary.
///
/// Returns `true` if containment is enforced and the canonical form of
/// `path` is not a descendant of `containment_root`.
fn escapes_containment(path: &Path, containment_root: Option<&PathBuf>) -> bool {
    let Some(root) = containment_root else {
        return false; // containment not enforced
    };
    let Ok(canonical) = path.canonicalize() else {
        // If we can't canonicalize (e.g., path doesn't exist yet), check
        // the parent directory instead — the target might not exist yet.
        return false;
    };
    !canonical.starts_with(root)
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

/// Shared context for planning operations.
struct PlanContext<'a, F> {
    config_relative_dir: &'a Path,
    main_worktree: &'a Path,
    target_worktree: &'a Path,
    overwrite: bool,
    /// Canonical containment root for path-escape checks.
    ///
    /// When `Some`, resolved paths must be descendants of this root.
    /// When `None`, containment is not enforced (path escape allowed).
    containment_root: Option<PathBuf>,
    on_progress: &'a F,
    total_ops: usize,
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
    options: &ApplyConfigOptions,
    on_progress: &F,
) -> Result<Vec<PlannedOperation>, OperationError>
where
    F: Fn(usize, usize, &str, Option<u64>),
{
    let config_relative_dir = config
        .config_dir
        .strip_prefix(main_worktree)
        .unwrap_or(&config.config_dir);

    let containment_root = if options.allow_path_escape {
        None
    } else {
        main_worktree.canonicalize().ok()
    };

    let total_ops = config.config.symlinks.len()
        + config.config.copy.len()
        + config.config.overwrite.len()
        + config.config.copy_glob.len()
        + config.config.templates.len();

    let ctx = PlanContext {
        config_relative_dir,
        main_worktree,
        target_worktree,
        overwrite: options.overwrite_existing,
        containment_root,
        on_progress,
        total_ops,
    };

    let mut current_op = 0usize;
    let mut operations = Vec::new();

    operations.extend(plan_symlink_ops(
        &ctx,
        &mut current_op,
        &config.config.symlinks,
    ));
    operations.extend(plan_copy_ops(&ctx, &mut current_op, &config.config.copy));
    operations.extend(plan_overwrite_ops(
        &ctx,
        &mut current_op,
        &config.config.overwrite,
    ));
    operations.extend(plan_glob_ops(
        &ctx,
        &mut current_op,
        &config.config.copy_glob,
    ));
    operations.extend(plan_template_ops(
        &ctx,
        &mut current_op,
        &config.config.templates,
    ));

    Ok(operations)
}

/// Plan symlink operations.
fn plan_symlink_ops<F>(
    ctx: &PlanContext<'_, F>,
    current_op: &mut usize,
    symlinks: &[String],
) -> Vec<PlannedOperation>
where
    F: Fn(usize, usize, &str, Option<u64>),
{
    let mut operations = Vec::new();

    for symlink_path in symlinks {
        *current_op += 1;
        let (source, display_str) =
            resolve_path(ctx.main_worktree, ctx.config_relative_dir, symlink_path);
        let (target, _) = resolve_path(ctx.target_worktree, ctx.config_relative_dir, symlink_path);

        (ctx.on_progress)(*current_op, ctx.total_ops, &display_str, None);

        let (will_skip, skip_reason, force) =
            if escapes_containment(&source, ctx.containment_root.as_ref()) {
                (
                    true,
                    Some("path escapes worktree boundary".to_string()),
                    false,
                )
            } else if !source.exists() {
                (true, Some("not found".to_string()), false)
            } else if target.exists() || target.is_symlink() {
                if ctx.overwrite {
                    (false, None, true)
                } else {
                    (true, Some("exists".to_string()), false)
                }
            } else {
                (false, None, false)
            };

        operations.push(PlannedOperation {
            display_path: display_str,
            operation_type: OperationType::Symlink,
            source,
            target,
            file_count: 0,
            is_directory: false,
            will_skip,
            skip_reason,
            force_overwrite: force,
        });
    }

    operations
}

/// Plan explicit copy operations.
fn plan_copy_ops<F>(
    ctx: &PlanContext<'_, F>,
    current_op: &mut usize,
    copies: &[String],
) -> Vec<PlannedOperation>
where
    F: Fn(usize, usize, &str, Option<u64>),
{
    let mut operations = Vec::new();

    for copy_path in copies {
        *current_op += 1;
        let (source, display_str) =
            resolve_path(ctx.main_worktree, ctx.config_relative_dir, copy_path);
        let (target, _) = resolve_path(ctx.target_worktree, ctx.config_relative_dir, copy_path);

        (ctx.on_progress)(*current_op, ctx.total_ops, &display_str, None);

        let (will_skip, skip_reason, file_count, is_directory, op_type) =
            if escapes_containment(&source, ctx.containment_root.as_ref()) {
                (
                    true,
                    Some("path escapes worktree boundary".to_string()),
                    0,
                    false,
                    OperationType::Copy,
                )
            } else if !source.exists() {
                (
                    true,
                    Some("not found".to_string()),
                    0,
                    false,
                    OperationType::Copy,
                )
            } else if target.exists() {
                if ctx.overwrite {
                    let is_dir = source.is_dir();
                    let count = if is_dir {
                        count_files_with_progress(&source, |n| {
                            (ctx.on_progress)(*current_op, ctx.total_ops, &display_str, Some(n));
                        })
                    } else {
                        1
                    };
                    (false, None, count, is_dir, OperationType::Overwrite)
                } else {
                    (
                        true,
                        Some("exists".to_string()),
                        0,
                        false,
                        OperationType::Copy,
                    )
                }
            } else {
                let is_dir = source.is_dir();
                let count = if is_dir {
                    count_files_with_progress(&source, |n| {
                        (ctx.on_progress)(*current_op, ctx.total_ops, &display_str, Some(n));
                    })
                } else {
                    1
                };
                (false, None, count, is_dir, OperationType::Copy)
            };

        operations.push(PlannedOperation {
            display_path: display_str,
            operation_type: op_type,
            source,
            target,
            file_count,
            is_directory,
            will_skip,
            skip_reason,
            force_overwrite: false,
        });
    }

    operations
}

/// Plan overwrite operations.
fn plan_overwrite_ops<F>(
    ctx: &PlanContext<'_, F>,
    current_op: &mut usize,
    overwrites: &[String],
) -> Vec<PlannedOperation>
where
    F: Fn(usize, usize, &str, Option<u64>),
{
    let mut operations = Vec::new();

    for overwrite_path in overwrites {
        *current_op += 1;
        let (source, display_str) =
            resolve_path(ctx.main_worktree, ctx.config_relative_dir, overwrite_path);
        let (target, _) =
            resolve_path(ctx.target_worktree, ctx.config_relative_dir, overwrite_path);

        (ctx.on_progress)(*current_op, ctx.total_ops, &display_str, None);

        let (will_skip, skip_reason, file_count, is_directory) =
            if escapes_containment(&source, ctx.containment_root.as_ref()) {
                (
                    true,
                    Some("path escapes worktree boundary".to_string()),
                    0,
                    false,
                )
            } else if source.exists() {
                let is_dir = source.is_dir();
                let count = if is_dir {
                    count_files_with_progress(&source, |n| {
                        (ctx.on_progress)(*current_op, ctx.total_ops, &display_str, Some(n));
                    })
                } else {
                    1
                };
                (false, None, count, is_dir)
            } else {
                (true, Some("not found".to_string()), 0, false)
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
            force_overwrite: false,
        });
    }

    operations
}

/// Determine the skip/overwrite status for a glob target path.
fn glob_target_status(target: &Path, overwrite: bool) -> (bool, Option<String>, OperationType) {
    if target.exists() {
        if overwrite {
            (false, None, OperationType::Overwrite)
        } else {
            (true, Some("exists".to_string()), OperationType::CopyGlob)
        }
    } else {
        (false, None, OperationType::CopyGlob)
    }
}

/// Plan glob copy operations.
///
/// Uses `worktree_setup_glob::resolve_glob` for directory traversal with
/// `walkdir` + `globset`, providing:
///
/// * Consistent symlink handling (`follow_links(false)`)
/// * Directory pruning (matched directories are not descended into)
/// * Containment enforcement (paths outside the worktree boundary are skipped)
/// * Deduplication across patterns via a shared `seen` set
fn plan_glob_ops<F>(
    ctx: &PlanContext<'_, F>,
    current_op: &mut usize,
    patterns: &[String],
) -> Vec<PlannedOperation>
where
    F: Fn(usize, usize, &str, Option<u64>),
{
    let mut operations = Vec::new();
    let mut seen = BTreeSet::new();

    // Build options: skip symlinks always, enforce containment if we have a root
    let options = GlobResolverOptions {
        skip_symlinks: true,
        enforce_containment: ctx.containment_root.is_some(),
    };

    // Fallback containment root (only used when enforce_containment is true)
    let empty_root = PathBuf::new();
    let containment_root = ctx.containment_root.as_ref().unwrap_or(&empty_root);

    for pattern in patterns {
        *current_op += 1;

        // Determine search directory and display prefix based on root-relative
        // vs config-relative path
        let (search_dir, display_prefix, glob_pattern) = pattern.strip_prefix('/').map_or_else(
            || {
                (
                    ctx.main_worktree.join(ctx.config_relative_dir),
                    ctx.config_relative_dir.to_path_buf(),
                    pattern.as_str(),
                )
            },
            |stripped| (ctx.main_worktree.to_path_buf(), PathBuf::new(), stripped),
        );

        (ctx.on_progress)(*current_op, ctx.total_ops, pattern, None);

        if is_glob_pattern(glob_pattern) {
            plan_glob_pattern(
                ctx,
                &mut operations,
                &mut seen,
                &options,
                containment_root,
                pattern,
                &search_dir,
                &display_prefix,
                glob_pattern,
            );
        } else {
            plan_glob_exact(
                ctx,
                &mut operations,
                &mut seen,
                &search_dir,
                &display_prefix,
                pattern,
                glob_pattern,
            );
        }
    }

    operations
}

/// Plan a single exact (non-glob) path within `plan_glob_ops`.
fn plan_glob_exact<F>(
    ctx: &PlanContext<'_, F>,
    operations: &mut Vec<PlannedOperation>,
    seen: &mut BTreeSet<PathBuf>,
    search_dir: &Path,
    display_prefix: &Path,
    pattern: &str,
    glob_pattern: &str,
) where
    F: Fn(usize, usize, &str, Option<u64>),
{
    let source = search_dir.join(glob_pattern);
    if !source.exists() {
        return;
    }

    // Containment check on exact path
    if escapes_containment(&source, ctx.containment_root.as_ref()) {
        log::warn!("copyGlob exact path escapes worktree boundary, skipping: {pattern}");
        return;
    }

    // Dedup via canonical path
    if let Ok(canonical) = source.canonicalize()
        && !seen.insert(canonical)
    {
        return;
    }

    let rel_path = glob_pattern;
    let target = if pattern.starts_with('/') {
        ctx.target_worktree.join(rel_path)
    } else {
        ctx.target_worktree
            .join(ctx.config_relative_dir)
            .join(rel_path)
    };
    let display_path = if display_prefix.as_os_str().is_empty() {
        PathBuf::from(rel_path)
    } else {
        display_prefix.join(rel_path)
    };

    let (will_skip, skip_reason, op_type) = glob_target_status(&target, ctx.overwrite);

    operations.push(PlannedOperation {
        display_path: display_path.to_string_lossy().to_string(),
        operation_type: op_type,
        source,
        target,
        file_count: 1,
        is_directory: false,
        will_skip,
        skip_reason,
        force_overwrite: false,
    });
}

/// Plan a glob pattern within `plan_glob_ops` using `walkdir` + `globset`.
#[allow(clippy::too_many_arguments)]
fn plan_glob_pattern<F>(
    ctx: &PlanContext<'_, F>,
    operations: &mut Vec<PlannedOperation>,
    seen: &mut BTreeSet<PathBuf>,
    options: &GlobResolverOptions,
    containment_root: &Path,
    pattern: &str,
    search_dir: &Path,
    display_prefix: &Path,
    glob_pattern: &str,
) where
    F: Fn(usize, usize, &str, Option<u64>),
{
    let resolved = resolve_glob(glob_pattern, search_dir, containment_root, seen, options);

    let canonical_search = search_dir
        .canonicalize()
        .unwrap_or_else(|_| search_dir.to_path_buf());

    for entry in &resolved {
        let Ok(rel_path) = entry.canonical.strip_prefix(&canonical_search) else {
            continue;
        };

        let target = if pattern.starts_with('/') {
            ctx.target_worktree.join(rel_path)
        } else {
            ctx.target_worktree
                .join(ctx.config_relative_dir)
                .join(rel_path)
        };
        let display_path = if display_prefix.as_os_str().is_empty() {
            rel_path.to_path_buf()
        } else {
            display_prefix.join(rel_path)
        };

        let (will_skip, skip_reason, op_type) = glob_target_status(&target, ctx.overwrite);

        operations.push(PlannedOperation {
            display_path: display_path.to_string_lossy().to_string(),
            operation_type: op_type,
            source: entry.canonical.clone(),
            target,
            file_count: 1,
            is_directory: false,
            will_skip,
            skip_reason,
            force_overwrite: false,
        });
    }
}

/// Plan template operations.
fn plan_template_ops<F>(
    ctx: &PlanContext<'_, F>,
    current_op: &mut usize,
    templates: &[worktree_setup_config::TemplateMapping],
) -> Vec<PlannedOperation>
where
    F: Fn(usize, usize, &str, Option<u64>),
{
    let mut operations = Vec::new();

    for template in templates {
        *current_op += 1;
        let (source, source_display) =
            resolve_path(ctx.main_worktree, ctx.config_relative_dir, &template.source);
        let (target, target_display) = resolve_path(
            ctx.target_worktree,
            ctx.config_relative_dir,
            &template.target,
        );
        let display_path = format!("{source_display} -> {target_display}");

        (ctx.on_progress)(*current_op, ctx.total_ops, &display_path, None);

        let (will_skip, skip_reason, op_type) =
            if escapes_containment(&source, ctx.containment_root.as_ref()) {
                (
                    true,
                    Some("path escapes worktree boundary".to_string()),
                    OperationType::Template,
                )
            } else if !source.exists() {
                (true, Some("not found".to_string()), OperationType::Template)
            } else if target.exists() {
                if ctx.overwrite {
                    (false, None, OperationType::Overwrite)
                } else {
                    (true, Some("exists".to_string()), OperationType::Template)
                }
            } else {
                (false, None, OperationType::Template)
            };

        operations.push(PlannedOperation {
            display_path,
            operation_type: op_type,
            source,
            target,
            file_count: 1,
            is_directory: false,
            will_skip,
            skip_reason,
            force_overwrite: false,
        });
    }

    operations
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
#[must_use]
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
                force_overwrite: false,
            });
        }
    }

    operations
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
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

    #[test]
    fn test_containment_symlink_escapes_boundary() {
        let root = TempDir::new().unwrap();
        let main_dir = root.path().join("main");
        let outer_dir = root.path().join("outer");
        let target_dir = root.path().join("target");
        fs::create_dir_all(&main_dir).unwrap();
        fs::create_dir_all(&outer_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();

        // Create a file outside the main worktree
        fs::write(outer_dir.join("secret.txt"), "secret").unwrap();

        let config = LoadedConfig {
            config: Config {
                symlinks: vec!["../outer/secret.txt".to_string()],
                ..Default::default()
            },
            config_path: main_dir.join("worktree.config.toml"),
            config_dir: main_dir.clone(),
            relative_path: "worktree.config.toml".to_string(),
        };

        // Default: containment enforced (allow_path_escape = false)
        let options = ApplyConfigOptions::default();
        let ops = plan_operations(&config, &main_dir, &target_dir, &options).unwrap();

        // The operation should be planned but skipped due to containment
        assert_eq!(ops.len(), 1);
        assert!(ops[0].will_skip);
        assert_eq!(
            ops[0].skip_reason,
            Some("path escapes worktree boundary".to_string())
        );
    }

    #[test]
    fn test_containment_copy_escapes_boundary() {
        let root = TempDir::new().unwrap();
        let main_dir = root.path().join("main");
        let outer_dir = root.path().join("outer");
        let target_dir = root.path().join("target");
        fs::create_dir_all(&main_dir).unwrap();
        fs::create_dir_all(&outer_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();

        fs::write(outer_dir.join("secret.txt"), "secret").unwrap();

        let config = LoadedConfig {
            config: Config {
                copy: vec!["../outer/secret.txt".to_string()],
                ..Default::default()
            },
            config_path: main_dir.join("worktree.config.toml"),
            config_dir: main_dir.clone(),
            relative_path: "worktree.config.toml".to_string(),
        };

        let options = ApplyConfigOptions::default();
        let ops = plan_operations(&config, &main_dir, &target_dir, &options).unwrap();

        assert_eq!(ops.len(), 1);
        assert!(ops[0].will_skip);
        assert_eq!(
            ops[0].skip_reason,
            Some("path escapes worktree boundary".to_string())
        );
    }

    #[test]
    fn test_containment_overwrite_escapes_boundary() {
        let root = TempDir::new().unwrap();
        let main_dir = root.path().join("main");
        let outer_dir = root.path().join("outer");
        let target_dir = root.path().join("target");
        fs::create_dir_all(&main_dir).unwrap();
        fs::create_dir_all(&outer_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();

        fs::write(outer_dir.join("secret.txt"), "secret").unwrap();

        let config = LoadedConfig {
            config: Config {
                overwrite: vec!["../outer/secret.txt".to_string()],
                ..Default::default()
            },
            config_path: main_dir.join("worktree.config.toml"),
            config_dir: main_dir.clone(),
            relative_path: "worktree.config.toml".to_string(),
        };

        let options = ApplyConfigOptions::default();
        let ops = plan_operations(&config, &main_dir, &target_dir, &options).unwrap();

        assert_eq!(ops.len(), 1);
        assert!(ops[0].will_skip);
        assert_eq!(
            ops[0].skip_reason,
            Some("path escapes worktree boundary".to_string())
        );
    }

    #[test]
    fn test_containment_template_escapes_boundary() {
        let root = TempDir::new().unwrap();
        let main_dir = root.path().join("main");
        let outer_dir = root.path().join("outer");
        let target_dir = root.path().join("target");
        fs::create_dir_all(&main_dir).unwrap();
        fs::create_dir_all(&outer_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();

        fs::write(outer_dir.join("template.txt"), "secret").unwrap();

        let config = LoadedConfig {
            config: Config {
                templates: vec![worktree_setup_config::TemplateMapping {
                    source: "../outer/template.txt".to_string(),
                    target: "output.txt".to_string(),
                }],
                ..Default::default()
            },
            config_path: main_dir.join("worktree.config.toml"),
            config_dir: main_dir.clone(),
            relative_path: "worktree.config.toml".to_string(),
        };

        let options = ApplyConfigOptions::default();
        let ops = plan_operations(&config, &main_dir, &target_dir, &options).unwrap();

        assert_eq!(ops.len(), 1);
        assert!(ops[0].will_skip);
        assert_eq!(
            ops[0].skip_reason,
            Some("path escapes worktree boundary".to_string())
        );
    }

    #[test]
    fn test_containment_allow_path_escape_permits_outside_paths() {
        let root = TempDir::new().unwrap();
        let main_dir = root.path().join("main");
        let outer_dir = root.path().join("outer");
        let target_dir = TempDir::new().unwrap();
        fs::create_dir_all(&main_dir).unwrap();
        fs::create_dir_all(&outer_dir).unwrap();

        fs::write(outer_dir.join("secret.txt"), "secret").unwrap();

        let config = LoadedConfig {
            config: Config {
                copy: vec!["../outer/secret.txt".to_string()],
                ..Default::default()
            },
            config_path: main_dir.join("worktree.config.toml"),
            config_dir: main_dir.clone(),
            relative_path: "worktree.config.toml".to_string(),
        };

        // Allow path escape
        let options = ApplyConfigOptions {
            allow_path_escape: true,
            ..Default::default()
        };
        let ops = plan_operations(&config, &main_dir, target_dir.path(), &options).unwrap();

        assert_eq!(ops.len(), 1);
        // Should NOT be skipped — path escape allowed and target is separate
        assert!(!ops[0].will_skip);
    }

    #[test]
    fn test_plan_glob_ops_with_walkdir() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create files matching a glob pattern
        fs::write(main_dir.path().join("file1.txt"), "a").unwrap();
        fs::write(main_dir.path().join("file2.txt"), "b").unwrap();
        fs::write(main_dir.path().join("file3.log"), "c").unwrap();

        let config = LoadedConfig {
            config: Config {
                copy_glob: vec!["*.txt".to_string()],
                ..Default::default()
            },
            config_path: main_dir.path().join("worktree.config.toml"),
            config_dir: main_dir.path().to_path_buf(),
            relative_path: "worktree.config.toml".to_string(),
        };

        let options = ApplyConfigOptions::default();
        let ops = plan_operations(&config, main_dir.path(), target_dir.path(), &options).unwrap();

        // Should match file1.txt and file2.txt, but not file3.log
        assert_eq!(ops.len(), 2);
        assert!(
            ops.iter()
                .all(|op| op.operation_type == OperationType::CopyGlob)
        );

        let display_paths: BTreeSet<&str> = ops.iter().map(|op| op.display_path.as_str()).collect();
        assert!(display_paths.contains("file1.txt"));
        assert!(display_paths.contains("file2.txt"));
    }

    #[test]
    fn test_plan_glob_ops_nested_pattern() {
        let main_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();

        // Create nested directory structure
        fs::create_dir_all(main_dir.path().join("a/dist")).unwrap();
        fs::write(main_dir.path().join("a/dist/bundle.js"), "code").unwrap();
        fs::create_dir_all(main_dir.path().join("b/dist")).unwrap();
        fs::write(main_dir.path().join("b/dist/bundle.js"), "code").unwrap();
        fs::create_dir_all(main_dir.path().join("c/src")).unwrap();

        let config = LoadedConfig {
            config: Config {
                copy_glob: vec!["**/dist".to_string()],
                ..Default::default()
            },
            config_path: main_dir.path().join("worktree.config.toml"),
            config_dir: main_dir.path().to_path_buf(),
            relative_path: "worktree.config.toml".to_string(),
        };

        let options = ApplyConfigOptions::default();
        let ops = plan_operations(&config, main_dir.path(), target_dir.path(), &options).unwrap();

        // Should match a/dist and b/dist directories
        assert_eq!(ops.len(), 2);

        let display_paths: BTreeSet<&str> = ops.iter().map(|op| op.display_path.as_str()).collect();
        assert!(display_paths.contains("a/dist"));
        assert!(display_paths.contains("b/dist"));
    }
}
