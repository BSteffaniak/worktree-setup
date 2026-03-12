//! worktree-setup CLI entry point.
//!
//! A tool for setting up git worktrees with project-specific configurations.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

mod args;
mod interactive;
mod output;
mod progress;

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use colored::Colorize;
use path_clean::PathClean;

use args::{Args, CleanArgs, SetupArgs};
use progress::ProgressManager;
use worktree_setup_config::{
    CreationMethod, LoadedConfig, PostSetupKeyword, PostSetupMode, ResolvedProfile,
    discover_configs, load_config, resolve_profiles,
};
use worktree_setup_git::{
    GitError, WorktreeCreateOptions, create_worktree, discover_repo, fetch_remote,
    get_current_branch, get_default_branch, get_local_branches, get_main_worktree,
    get_recent_branches, get_remotes, get_repo_root, get_unstaged_and_untracked_files,
    prune_worktrees,
};
use worktree_setup_operations::{
    ApplyConfigOptions, OperationType, execute_operation, plan_operations_with_progress,
    plan_unstaged_operations,
};

fn main() {
    let args = Args::parse();

    // Set up logging based on top-level or subcommand verbose flag
    let verbose = match &args.command {
        Some(args::Command::Setup(setup_args)) => setup_args.verbose,
        Some(args::Command::Clean(clean_args)) => clean_args.verbose,
        None => args.verbose,
    };

    if verbose {
        // SAFETY: We're setting this before any other threads are spawned
        unsafe {
            env::set_var("RUST_LOG", "debug");
        }
    }
    pretty_env_logger::init();

    let result = match args.command {
        Some(args::Command::Setup(ref setup_args)) => run_setup(setup_args),
        Some(args::Command::Clean(ref clean_args)) => run_clean(clean_args),
        None => run_create(&args),
    };

    if let Err(e) = result {
        output::print_error(&e.to_string());
        std::process::exit(1);
    }
}

/// Discover and load configs from a repo root, printing status.
///
/// Returns the loaded configs. Prints warnings for configs that fail to load.
fn discover_and_load_configs(
    repo_root: &Path,
) -> Result<Vec<LoadedConfig>, Box<dyn std::error::Error>> {
    let config_paths = discover_configs(repo_root)?;

    let mut all_configs: Vec<LoadedConfig> = Vec::new();
    if config_paths.is_empty() {
        println!("No config files found.\n");
    } else {
        for path in config_paths {
            match load_config(&path, repo_root) {
                Ok(config) => all_configs.push(config),
                Err(e) => {
                    output::print_warning(&format!("Failed to load {}: {}", path.display(), e));
                }
            }
        }

        if all_configs.is_empty() {
            output::print_warning("All config files failed to load.\n");
        } else {
            let config_display: Vec<(String, String)> = all_configs
                .iter()
                .map(|c| (c.relative_path.clone(), c.config.description.clone()))
                .collect();
            output::print_config_list(&config_display);
        }
    }

    Ok(all_configs)
}

/// Select configs from the loaded list, either interactively or by pattern.
///
/// Returns indices into `all_configs` for the selected configs.
fn select_configs(
    all_configs: &[LoadedConfig],
    config_patterns: &[String],
    non_interactive: bool,
) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    if all_configs.is_empty() {
        return Ok(Vec::new());
    }

    if !config_patterns.is_empty() {
        // Filter by provided patterns
        Ok(all_configs
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                config_patterns.iter().any(|p| {
                    c.relative_path.contains(p) || c.config_path.to_string_lossy().contains(p)
                })
            })
            .map(|(i, _)| i)
            .collect())
    } else if non_interactive {
        // Use all configs in non-interactive mode
        Ok((0..all_configs.len()).collect())
    } else {
        // Interactive selection
        Ok(interactive::select_configs(all_configs)?)
    }
}

/// Resolve profiles and select configs, applying profile defaults.
///
/// When `--profile` is used, this resolves the named profiles against the
/// profiles file and loaded configs, returning the resolved profile with
/// config indices and merged defaults. Prints the profile info and selected
/// configs.
///
/// # Errors
///
/// * If any requested profile is not found
fn resolve_and_print_profile(
    profile_names: &[String],
    all_configs: &[LoadedConfig],
    repo_root: &Path,
) -> Result<ResolvedProfile, Box<dyn std::error::Error>> {
    let resolved = resolve_profiles(profile_names, all_configs, repo_root)?;

    output::print_using_profile(&resolved.names);

    let config_display: Vec<(String, String)> = resolved
        .config_indices
        .iter()
        .map(|&i| {
            (
                all_configs[i].relative_path.clone(),
                all_configs[i].config.description.clone(),
            )
        })
        .collect();
    output::print_profile_configs(&config_display);

    Ok(resolved)
}

/// Execute file operations for the given configs against a target worktree.
///
/// This handles scanning, planning, progress display, unstaged file copying,
/// and execution of all file operations (symlinks, copies, overwrites,
/// templates, globs).
fn execute_file_operations(
    selected_configs: &[&LoadedConfig],
    main_worktree_path: &Path,
    target_path: &Path,
    copy_unstaged_override: Option<bool>,
    overwrite_existing: bool,
    show_progress: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let progress_mgr = ProgressManager::new(show_progress);
    let options = ApplyConfigOptions {
        copy_unstaged: copy_unstaged_override,
        overwrite_existing,
    };

    // Calculate total operations across all configs for scanning progress
    let config_op_counts: Vec<usize> = selected_configs
        .iter()
        .map(|c| {
            c.config.symlinks.len()
                + c.config.copy.len()
                + c.config.overwrite.len()
                + c.config.copy_glob.len()
                + c.config.templates.len()
        })
        .collect();
    let total_ops: usize = config_op_counts.iter().sum();

    // Create scanning progress bar
    let scanning_bar = progress_mgr.create_scanning_bar(total_ops as u64);

    // Plan all operations across all configs with progress
    let mut all_operations = Vec::new();
    let mut offset = 0usize;
    for (config, &config_count) in selected_configs.iter().zip(&config_op_counts) {
        let current_offset = offset;
        let ops = plan_operations_with_progress(
            config,
            main_worktree_path,
            target_path,
            &options,
            &|current, _total, path, file_count| {
                scanning_bar.set_position((current_offset + current) as u64);
                match file_count {
                    Some(n) => scanning_bar.set_message(format!("{path} ({n} files)")),
                    None => scanning_bar.set_message(path.to_string()),
                }
            },
        )?;
        offset += config_count;
        all_operations.extend(ops);
    }

    // Clear scanning progress bar
    scanning_bar.finish_and_clear();

    // Handle copyUnstaged - check if any selected config enables it
    let should_copy_unstaged = selected_configs
        .iter()
        .any(|c| copy_unstaged_override.unwrap_or(c.config.copy_unstaged));

    if should_copy_unstaged {
        println!("Checking for unstaged files...");
        let repo = worktree_setup_git::open_repo(main_worktree_path)?;
        let unstaged_files = get_unstaged_and_untracked_files(&repo)?;
        if !unstaged_files.is_empty() {
            println!(
                "Found {} unstaged/untracked files to copy",
                unstaged_files.len()
            );
            let unstaged_ops =
                plan_unstaged_operations(&unstaged_files, main_worktree_path, target_path);
            all_operations.extend(unstaged_ops);
        }
    }

    // Execute operations with progress
    for op in &all_operations {
        if op.will_skip {
            let reason = op.skip_reason.as_deref().unwrap_or("skipped");
            progress_mgr.print_result(&op.display_path, reason, false);
            continue;
        }

        let needs_progress_bar = op.is_directory && op.file_count > 1;

        if needs_progress_bar {
            let bar = progress_mgr.create_file_bar(&op.display_path, op.file_count);

            let result = execute_operation(op, |completed, _total| {
                bar.set_position(completed);
            })?;

            bar.finish_and_clear();

            let result_str = format_result_string(result, op.operation_type);
            progress_mgr.print_result_with_count(&op.display_path, &result_str, op.file_count);
        } else {
            let result = execute_operation(op, |_, _| {})?;
            let result_str = format_result_string(result, op.operation_type);
            progress_mgr.print_result(&op.display_path, &result_str, true);
        }
    }

    // Clear any remaining progress bars
    progress_mgr.clear();

    Ok(())
}

/// Run post-setup commands in the target directory.
fn run_post_setup_commands(
    commands: &[&str],
    target_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if commands.is_empty() {
        return Ok(());
    }

    println!("Running post-setup commands:");
    for cmd in commands {
        output::print_command(cmd);

        let mut child = Command::new("sh")
            .args(["-c", cmd])
            .current_dir(target_path)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()?;

        let status = child.wait()?;

        if !status.success() {
            output::print_warning(&format!("Command failed: {cmd}"));
        }
    }
    println!();

    Ok(())
}

/// Collect unique post-setup commands from configs.
fn collect_post_setup_commands<'a>(configs: &[&'a LoadedConfig]) -> Vec<&'a str> {
    let mut unique_commands: Vec<&str> = Vec::new();
    for config in configs {
        for cmd in &config.config.post_setup {
            let cmd_str = cmd.as_str();
            if !unique_commands.contains(&cmd_str) {
                unique_commands.push(cmd_str);
            }
        }
    }
    unique_commands
}

// ─── Subcommand: setup ──────────────────────────────────────────────────────

/// Resolve which post-setup commands to run based on CLI flags and profile defaults.
///
/// Returns:
/// * `Some(cmds)` — run exactly these commands, no prompt needed
/// * `None` — prompt the user to decide
///
/// Priority: `--no-install` CLI flag > profile `post_setup` > prompt.
///
/// When `post_setup = "all"`, `skip_post_setup` filters the command list.
/// When `post_setup = ["cmd1", "cmd2"]`, only those commands run (exact match).
/// When `post_setup = "none"`, returns an empty list.
/// When `post_setup` is not set, returns `None` to trigger a prompt.
fn resolve_post_setup_commands<'a>(
    no_install: bool,
    profile: Option<&ResolvedProfile>,
    available_commands: &[&'a str],
) -> Option<Vec<&'a str>> {
    // CLI --no-install always wins
    if no_install {
        return Some(Vec::new());
    }

    let defaults = profile.map(|p| &p.defaults)?;
    let post_setup = defaults.post_setup.as_ref()?;

    match post_setup {
        PostSetupMode::Keyword(PostSetupKeyword::None) => Some(Vec::new()),
        PostSetupMode::Keyword(PostSetupKeyword::All) => {
            // Run all, minus any in skip_post_setup
            let skip = &defaults.skip_post_setup;
            if skip.is_empty() {
                Some(available_commands.to_vec())
            } else {
                Some(
                    available_commands
                        .iter()
                        .filter(|cmd| !skip.iter().any(|s| s == **cmd))
                        .copied()
                        .collect(),
                )
            }
        }
        PostSetupMode::Commands(cmds) => {
            // Run only commands that exist in the available list (exact match)
            Some(
                available_commands
                    .iter()
                    .filter(|cmd| cmds.iter().any(|c| c == **cmd))
                    .copied()
                    .collect(),
            )
        }
    }
}

/// Resolve whether overwrite should be enabled based on CLI flag and profile default.
///
/// Returns `Some(value)` when determined by CLI or profile, `None` to prompt.
fn resolve_overwrite(overwrite_flag: bool, profile: Option<&ResolvedProfile>) -> Option<bool> {
    if overwrite_flag {
        return Some(true);
    }
    profile.and_then(|p| p.defaults.overwrite_existing)
}

/// Determine what operations to run in `setup` mode.
///
/// Returns `(run_files, overwrite_existing, run_post_setup)`.
fn determine_setup_operations(
    args: &SetupArgs,
    resolved_profile: Option<&ResolvedProfile>,
    is_secondary_worktree: bool,
    unique_commands: &[&str],
) -> Result<(bool, bool, bool), Box<dyn std::error::Error>> {
    // Resolve pre-determined values from CLI flags + profile
    let files_determined: Option<bool> = if args.no_files { Some(false) } else { None };

    let overwrite_determined = resolve_overwrite(args.overwrite, resolved_profile);

    let post_setup_resolved =
        resolve_post_setup_commands(args.no_install, resolved_profile, unique_commands);
    let post_setup_determined: Option<bool> =
        post_setup_resolved.as_ref().map(|cmds| !cmds.is_empty());

    if args.non_interactive {
        let run_files = files_determined.unwrap_or(is_secondary_worktree);
        let overwrite = overwrite_determined.unwrap_or(false);
        let run_post_setup = post_setup_determined.unwrap_or(true);
        return Ok((run_files, overwrite, run_post_setup));
    }

    // Interactive: show checklist (only undetermined items)
    let choices = interactive::prompt_setup_operations(
        &interactive::SetupOperationInputs {
            is_secondary_worktree,
            files: files_determined,
            overwrite: overwrite_determined,
            post_setup: post_setup_determined,
        },
        unique_commands,
    )?;

    Ok((
        choices.run_files,
        choices.overwrite_existing,
        choices.run_post_setup,
    ))
}

/// Run the `setup` subcommand.
///
/// Applies worktree configs to an existing directory. On a secondary worktree,
/// this runs file operations and post-setup commands. On the main worktree or
/// a regular clone, only post-setup commands are run.
fn run_setup(args: &SetupArgs) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let target_path = resolve_setup_target(&cwd, args.target_path.as_ref());

    // Discover repository from the target directory
    let repo = discover_repo(&target_path)?;
    let repo_root = get_repo_root(&repo)?;

    output::print_header("Worktree Setup");
    output::print_repo_info(&repo_root.to_string_lossy());
    println!();

    // Determine if the target is a secondary worktree
    let main_worktree = get_main_worktree(&repo)?;
    let is_secondary_worktree = is_secondary(&target_path, &main_worktree.path);

    if !is_secondary_worktree {
        output::print_info("Not a secondary worktree. File operations will be skipped.");
        println!();
    }

    // Discover and load configs
    let all_configs = discover_and_load_configs(&repo_root)?;

    if all_configs.is_empty() {
        output::print_warning("No configs found. Nothing to do.");
        return Ok(());
    }

    // Resolve profiles if --profile was provided
    let resolved_profile = if args.profile.is_empty() {
        None
    } else {
        Some(resolve_and_print_profile(
            &args.profile,
            &all_configs,
            &repo_root,
        )?)
    };

    // Select configs: profile overrides normal selection
    let selected_indices = select_configs_or_profile(
        &all_configs,
        args.non_interactive,
        &args.configs,
        resolved_profile.as_ref(),
    )?;
    let Some(selected_indices) = selected_indices else {
        println!("No configs selected. Exiting.");
        return Ok(());
    };

    let selected_configs: Vec<&LoadedConfig> =
        selected_indices.iter().map(|&i| &all_configs[i]).collect();

    // Collect post-setup commands for display in the checklist
    let unique_commands = collect_post_setup_commands(&selected_configs);

    // Determine what to run
    let (run_files, overwrite_existing, run_post_setup) = determine_setup_operations(
        args,
        resolved_profile.as_ref(),
        is_secondary_worktree,
        &unique_commands,
    )?;

    // Nothing selected
    if !run_files && !run_post_setup {
        println!("Nothing selected. Exiting.");
        return Ok(());
    }

    // Execute file operations
    if run_files {
        let copy_unstaged_override = args.copy_unstaged_override().or_else(|| {
            resolved_profile
                .as_ref()
                .and_then(|p| p.defaults.copy_unstaged)
        });

        println!("\nApplying file operations to: {}", target_path.display());
        println!("Source (main worktree): {}\n", main_worktree.path.display());

        execute_file_operations(
            &selected_configs,
            &main_worktree.path,
            &target_path,
            copy_unstaged_override,
            overwrite_existing,
            args.should_show_progress(),
        )?;

        println!();
    }

    // Run post-setup commands
    if run_post_setup {
        // Apply per-command filtering from profile if present
        let resolved_cmds = resolve_post_setup_commands(
            args.no_install,
            resolved_profile.as_ref(),
            &unique_commands,
        );
        let cmds_to_run = resolved_cmds.as_deref().unwrap_or(&unique_commands);
        run_post_setup_commands(cmds_to_run, &target_path)?;
    }

    output::print_success();
    Ok(())
}

/// Resolve the target path for the `setup` subcommand.
///
/// Defaults to the current working directory if no path is provided.
/// Exits the process if the resolved path does not exist.
#[must_use]
fn resolve_setup_target(cwd: &Path, target: Option<&PathBuf>) -> PathBuf {
    let target_path = target.map_or_else(
        || cwd.to_path_buf(),
        |path| {
            if path.is_absolute() {
                path.clone()
            } else {
                cwd.join(path)
            }
            .clean()
        },
    );

    if !target_path.exists() {
        output::print_error(&format!(
            "Target path does not exist: {}",
            target_path.display()
        ));
        std::process::exit(1);
    }

    target_path
}

// ─── Subcommand: clean ──────────────────────────────────────────────────────

/// Check whether a pattern string contains glob metacharacters.
fn is_glob_pattern(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

/// Calculate the total size of a path (file or directory, recursive).
fn path_size(path: &Path) -> u64 {
    if path.is_file() || path.is_symlink() {
        path.metadata().map_or(0, |m| m.len())
    } else if path.is_dir() {
        walkdir(path)
    } else {
        0
    }
}

/// Recursively sum file sizes in a directory.
fn walkdir(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += walkdir(&path);
            } else {
                total += path.metadata().map_or(0, |m| m.len());
            }
        }
    }
    total
}

/// Resolve clean paths from selected configs into concrete items to delete.
///
/// For each config's `clean` entries:
/// * Exact paths are resolved relative to the config's directory as mapped
///   into the target worktree
/// * Glob patterns are expanded the same way
///
/// All resolved paths must be inside `target_canonical` (containment check).
/// Paths that don't exist on disk are silently skipped.
/// Duplicate paths (by canonical path) are deduplicated.
fn resolve_clean_paths(
    selected_configs: &[&LoadedConfig],
    target_path: &Path,
    target_canonical: &Path,
    repo_root: &Path,
) -> Vec<(PathBuf, String)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut results: Vec<(PathBuf, String)> = Vec::new();

    for config in selected_configs {
        // Map the config directory into the target worktree.
        // Config dir is absolute (e.g. /repo/apps/my-app), we need the same
        // relative offset inside the target worktree.
        let config_rel = config
            .config_dir
            .strip_prefix(repo_root)
            .unwrap_or(&config.config_dir);
        let target_config_dir = target_path.join(config_rel);

        for pattern in &config.config.clean {
            if is_glob_pattern(pattern) {
                resolve_clean_glob(
                    pattern,
                    &target_config_dir,
                    target_canonical,
                    &mut seen,
                    &mut results,
                );
            } else {
                resolve_clean_exact(
                    pattern,
                    &target_config_dir,
                    target_canonical,
                    &mut seen,
                    &mut results,
                );
            }
        }
    }

    results
}

/// Resolve a single exact clean path.
fn resolve_clean_exact(
    pattern: &str,
    target_config_dir: &Path,
    target_canonical: &Path,
    seen: &mut std::collections::BTreeSet<PathBuf>,
    results: &mut Vec<(PathBuf, String)>,
) {
    let candidate = target_config_dir.join(pattern);

    if !candidate.exists() {
        log::debug!(
            "Clean path does not exist, skipping: {}",
            candidate.display()
        );
        return;
    }

    let Ok(canonical) = candidate.canonicalize() else {
        log::warn!("Could not canonicalize clean path: {}", candidate.display());
        return;
    };

    if !canonical.starts_with(target_canonical) {
        log::warn!("Clean path escapes target directory, skipping: {pattern}");
        return;
    }

    if seen.insert(canonical.clone()) {
        let relative = canonical.strip_prefix(target_canonical).map_or_else(
            |_| candidate.to_string_lossy().to_string(),
            |r| r.to_string_lossy().to_string(),
        );
        results.push((canonical, relative));
    }
}

/// Resolve a glob clean pattern.
fn resolve_clean_glob(
    pattern: &str,
    target_config_dir: &Path,
    target_canonical: &Path,
    seen: &mut std::collections::BTreeSet<PathBuf>,
    results: &mut Vec<(PathBuf, String)>,
) {
    let full_pattern = target_config_dir.join(pattern);
    let full_pattern_str = full_pattern.to_string_lossy();

    let entries = match glob::glob(&full_pattern_str) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("Invalid glob pattern '{pattern}': {e}");
            return;
        }
    };

    for entry in entries {
        let Ok(path) = entry else {
            continue;
        };

        let Ok(canonical) = path.canonicalize() else {
            continue;
        };

        if !canonical.starts_with(target_canonical) {
            log::warn!("Clean path escapes target directory, skipping: {pattern}");
            continue;
        }

        if seen.insert(canonical.clone()) {
            let relative = canonical.strip_prefix(target_canonical).map_or_else(
                |_| path.to_string_lossy().to_string(),
                |r| r.to_string_lossy().to_string(),
            );
            results.push((canonical, relative));
        }
    }
}

/// Run the `clean` subcommand.
///
/// Discovers clean paths from selected configs, shows a preview with sizes,
/// prompts for confirmation (unless `--force` or `--dry-run`), and deletes.
fn run_clean(args: &CleanArgs) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let target_path = resolve_setup_target(&cwd, args.target_path.as_ref());

    // Discover repository from the target directory
    let repo = discover_repo(&target_path)?;
    let repo_root = get_repo_root(&repo)?;

    output::print_header("Worktree Clean");
    output::print_repo_info(&repo_root.to_string_lossy());
    println!();

    // Discover and load configs
    let all_configs = discover_and_load_configs(&repo_root)?;

    if all_configs.is_empty() {
        output::print_warning("No configs found. Nothing to clean.");
        return Ok(());
    }

    // Resolve profiles if --profile was provided
    let resolved_profile = if args.profile.is_empty() {
        None
    } else {
        Some(resolve_and_print_profile(
            &args.profile,
            &all_configs,
            &repo_root,
        )?)
    };

    // Select configs: profile overrides normal selection
    let selected_indices = select_configs_or_profile(
        &all_configs,
        args.non_interactive,
        &args.configs,
        resolved_profile.as_ref(),
    )?;
    let Some(selected_indices) = selected_indices else {
        println!("No configs selected. Exiting.");
        return Ok(());
    };

    let selected_configs: Vec<&LoadedConfig> =
        selected_indices.iter().map(|&i| &all_configs[i]).collect();

    // Check if any selected config has clean paths
    let has_clean_paths = selected_configs.iter().any(|c| !c.config.clean.is_empty());
    if !has_clean_paths {
        output::print_info("No clean paths defined in selected configs.");
        return Ok(());
    }

    // Canonicalize the target for containment checks
    let target_canonical = target_path.canonicalize().map_err(|e| {
        format!(
            "Could not canonicalize target path '{}': {}",
            target_path.display(),
            e
        )
    })?;

    // Resolve clean paths
    let resolved = resolve_clean_paths(
        &selected_configs,
        &target_path,
        &target_canonical,
        &repo_root,
    );

    if resolved.is_empty() {
        output::print_info("All clean paths are already clean (nothing exists to delete).");
        return Ok(());
    }

    // Build preview items with sizes
    let items: Vec<output::CleanItem> = resolved
        .iter()
        .map(|(abs_path, rel_path)| {
            let is_dir = abs_path.is_dir();
            let size = path_size(abs_path);
            output::CleanItem {
                relative_path: rel_path.clone(),
                is_dir,
                size,
            }
        })
        .collect();

    println!();
    output::print_clean_preview(&items);

    // Dry run: stop here
    if args.dry_run {
        println!("\n{}", "Dry run — nothing was deleted.".dimmed());
        return Ok(());
    }

    // Confirm and execute deletion
    confirm_and_delete(args.force, args.non_interactive, &resolved, &items)
}

/// Prompt for confirmation (unless forced) and delete the resolved paths.
fn confirm_and_delete(
    force: bool,
    non_interactive: bool,
    resolved: &[(PathBuf, String)],
    items: &[output::CleanItem],
) -> Result<(), Box<dyn std::error::Error>> {
    if !force {
        if non_interactive {
            return Err(
                "Clean requires confirmation. Use --force to skip, or --dry-run to preview.".into(),
            );
        }

        let confirm = dialoguer::Confirm::new()
            .with_prompt("Proceed with deletion?")
            .default(false)
            .interact()?;

        if !confirm {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let mut deleted_count = 0usize;
    let mut total_size = 0u64;

    for (idx, (abs_path, _rel_path)) in resolved.iter().enumerate() {
        let item = &items[idx];
        let result = if abs_path.is_dir() {
            std::fs::remove_dir_all(abs_path)
        } else {
            std::fs::remove_file(abs_path)
        };

        match result {
            Ok(()) => {
                deleted_count += 1;
                total_size += item.size;
            }
            Err(e) => {
                output::print_warning(&format!("Failed to delete '{}': {}", item.relative_path, e));
            }
        }
    }

    println!();
    output::print_clean_summary(deleted_count, total_size);

    Ok(())
}

/// Check if the target path is a secondary worktree (not the main one).
fn is_secondary(target: &Path, main_worktree_path: &Path) -> bool {
    let target_canonical = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let main_canonical = main_worktree_path
        .canonicalize()
        .unwrap_or_else(|_| main_worktree_path.to_path_buf());
    target_canonical != main_canonical
}

/// Select configs from either a resolved profile or normal selection.
///
/// Returns `None` if no configs are selected (caller should exit).
/// Returns `Some(indices)` with the selected config indices.
fn select_configs_or_profile(
    all_configs: &[LoadedConfig],
    non_interactive: bool,
    config_patterns: &[String],
    profile: Option<&ResolvedProfile>,
) -> Result<Option<Vec<usize>>, Box<dyn std::error::Error>> {
    if let Some(p) = profile {
        if p.config_indices.is_empty() {
            output::print_warning("Profile matched no configs.");
            return Ok(None);
        }
        return Ok(Some(p.config_indices.clone()));
    }

    let indices = select_configs(all_configs, config_patterns, non_interactive)?;
    if indices.is_empty() {
        return Ok(None);
    }
    Ok(Some(indices))
}

/// Collect profile display info from all loaded configs for `--list`.
///
/// Aggregates profile names across all configs, deduplicates, and
/// uses the last description found for each profile name.
fn collect_profile_display_info(all_configs: &[LoadedConfig]) -> Vec<(String, String, usize)> {
    use std::collections::BTreeMap;

    // Aggregate: name -> (description, declaring_config_count)
    let mut profiles: BTreeMap<String, (String, usize)> = BTreeMap::new();

    for config in all_configs {
        for (name, def) in &config.config.profiles {
            let entry = profiles.entry(name.clone()).or_default();
            if !def.description.is_empty() {
                entry.0.clone_from(&def.description);
            }
            entry.1 += 1;
        }
    }

    profiles
        .into_iter()
        .map(|(name, (desc, count))| (name, desc, count))
        .collect()
}

// ─── Default flow (create + setup) ─────────────────────────────────────────

/// Handle worktree creation (both interactive and non-interactive).
///
/// Profile defaults are applied with the following priority:
/// * CLI flag > profile default > interactive prompt / builtin default
///
/// Profile defaults used:
/// * `remote` — remote name for remote branch fetch
/// * `base_branch` — base branch for new branch creation
/// * `new_branch` — when `true`, auto-create a branch named after the worktree
/// * `auto_create` — skip the "Create worktree?" confirmation
/// * `creation_method` — skip the creation method picker
fn handle_worktree_creation(
    args: &Args,
    repo: &worktree_setup_git::Repository,
    target_path: &Path,
    profile: Option<&ResolvedProfile>,
) -> Result<(), Box<dyn std::error::Error>> {
    let profile_defaults = profile.map(|p| &p.defaults);
    let worktree_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("worktree");

    // Build creation hints from profile defaults + CLI flags
    let effective_remote = args
        .remote
        .as_deref()
        .or_else(|| profile_defaults.and_then(|d| d.remote.as_deref()));
    let creation_method = profile_defaults.and_then(|d| d.creation_method.as_ref());
    let auto_create = profile_defaults
        .and_then(|d| d.auto_create)
        .unwrap_or(false);
    let profile_base_branch = profile_defaults.and_then(|d| d.base_branch.as_deref());
    let profile_new_branch = profile_defaults.and_then(|d| d.new_branch).unwrap_or(false);

    // Infer branch name for remote tracking (unless --no-infer-branch)
    let is_remote =
        creation_method == Some(&CreationMethod::Remote) || args.remote_branch.is_some();
    let inferred_branch = if is_remote && !args.no_infer_branch {
        Some(worktree_name)
    } else {
        None
    };

    let hints = interactive::CreationProfileHints {
        auto_create,
        creation_method,
        base_branch: profile_base_branch,
        new_branch: profile_new_branch,
        remote_override: effective_remote,
        inferred_branch,
    };

    let options = if args.non_interactive {
        handle_creation_non_interactive(args, repo, target_path, &hints, worktree_name)?
    } else {
        let result = handle_creation_interactive(args, repo, target_path, &hints)?;
        let Some(options) = result else {
            return Ok(());
        };
        println!("\nCreating worktree at {}...", target_path.display());
        options
    };

    create_worktree_with_recovery(repo, target_path, &options, args.non_interactive)
}

/// Non-interactive worktree creation.
fn handle_creation_non_interactive(
    args: &Args,
    repo: &worktree_setup_git::Repository,
    target_path: &Path,
    hints: &interactive::CreationProfileHints<'_>,
    worktree_name: &str,
) -> Result<WorktreeCreateOptions, Box<dyn std::error::Error>> {
    let is_remote =
        hints.creation_method == Some(&CreationMethod::Remote) || args.remote_branch.is_some();

    // Handle --remote-branch or creation_method = "remote"
    let branch = if let Some(ref remote_branch) = args.remote_branch {
        // Explicit --remote-branch: always use it
        let remote = resolve_remote_non_interactive(repo, hints.remote_override)?;
        println!("Fetching from {remote}...");
        fetch_remote(repo, &remote)?;
        Some(remote_branch.clone())
    } else if hints.creation_method == Some(&CreationMethod::Remote) && !args.no_infer_branch {
        // Profile says remote — infer branch from directory name
        let remote = resolve_remote_non_interactive(repo, hints.remote_override)?;
        println!("Fetching from {remote}...");
        fetch_remote(repo, &remote)?;
        println!("Inferred remote branch: {remote}/{worktree_name}");
        Some(worktree_name.to_string())
    } else if hints.creation_method == Some(&CreationMethod::Remote) && args.no_infer_branch {
        return Err(
            "Profile sets creationMethod = \"remote\" but --no-infer-branch is set. \
             Use --remote-branch <name> to specify the branch explicitly."
                .into(),
        );
    } else if hints.creation_method == Some(&CreationMethod::Current) {
        // Use current branch
        get_current_branch(repo)?
    } else if hints.creation_method == Some(&CreationMethod::Detach) {
        // Detached HEAD handled below via the detach flag
        None
    } else {
        // Auto or no creation_method: CLI --branch > profile base_branch > None
        args.branch
            .clone()
            .or_else(|| hints.base_branch.map(String::from))
    };

    // CLI --new-branch > profile new_branch (auto-name from worktree dir) > None
    // (not used when tracking a remote branch or detaching)
    let new_branch = if is_remote
        || hints.creation_method == Some(&CreationMethod::Detach)
        || hints.creation_method == Some(&CreationMethod::Current)
    {
        None
    } else {
        args.new_branch.clone().or_else(|| {
            if hints.new_branch {
                Some(worktree_name.to_string())
            } else {
                None
            }
        })
    };

    let detach = hints.creation_method == Some(&CreationMethod::Detach);

    println!("Creating worktree at {}...", target_path.display());
    Ok(WorktreeCreateOptions {
        branch,
        new_branch,
        force: args.force,
        detach,
    })
}

/// Interactive worktree creation.
///
/// Returns `None` if the user declines to create the worktree.
fn handle_creation_interactive(
    _args: &Args,
    repo: &worktree_setup_git::Repository,
    target_path: &Path,
    hints: &interactive::CreationProfileHints<'_>,
) -> Result<Option<WorktreeCreateOptions>, Box<dyn std::error::Error>> {
    let current_branch = get_current_branch(repo)?;
    let branches = get_local_branches(repo)?;
    let default_branch = get_default_branch(repo);
    let recent_branches = get_recent_branches(repo, 5);

    Ok(interactive::prompt_worktree_create(
        repo,
        target_path,
        current_branch.as_deref(),
        &branches,
        default_branch.as_deref(),
        &recent_branches,
        hints,
    )?)
}

/// Attempt to create a worktree, recovering from stale registrations.
///
/// If `git worktree add` fails because the path is already registered
/// but missing from disk:
/// * Interactive: prompts the user to prune, force, or cancel
/// * Non-interactive: returns the error (use `--force` to override)
fn create_worktree_with_recovery(
    repo: &worktree_setup_git::Repository,
    path: &Path,
    options: &WorktreeCreateOptions,
    non_interactive: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match create_worktree(repo, path, options) {
        Ok(()) => Ok(()),
        Err(ref e) if is_stale_worktree_error(e) => {
            if non_interactive {
                return Err(format!(
                    "Path '{}' is registered as a stale worktree. \
                     Use --force to override, or run 'git worktree prune' first.",
                    path.display()
                )
                .into());
            }

            // Interactive recovery
            match interactive::prompt_stale_worktree_recovery()? {
                interactive::StaleWorktreeAction::Prune => {
                    println!("Pruning stale worktrees...");
                    prune_worktrees(repo)?;
                    println!("Retrying worktree creation...");
                    create_worktree(repo, path, options)?;
                    Ok(())
                }
                interactive::StaleWorktreeAction::Force => {
                    println!("Force creating worktree...");
                    let mut forced = options.clone();
                    forced.force = true;
                    create_worktree(repo, path, &forced)?;
                    Ok(())
                }
                interactive::StaleWorktreeAction::Cancel => {
                    Err("Worktree creation cancelled.".into())
                }
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// Check if a `GitError` is a stale worktree registration error.
fn is_stale_worktree_error(err: &GitError) -> bool {
    match err {
        GitError::WorktreeCreateError { source, .. } => source
            .message()
            .contains("is a missing but already registered worktree"),
        _ => false,
    }
}

/// Resolve the remote name in non-interactive mode.
///
/// If `override_name` is provided, uses that. Otherwise auto-detects:
/// * Single remote: uses it
/// * Multiple or zero remotes: returns an error
fn resolve_remote_non_interactive(
    repo: &worktree_setup_git::Repository,
    override_name: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(name) = override_name {
        return Ok(name.to_string());
    }

    let remotes = get_remotes(repo)?;
    match remotes.len() {
        0 => Err("No remotes configured in this repository".into()),
        1 => Ok(remotes.into_iter().next().unwrap_or_default()),
        _ => Err(format!(
            "Multiple remotes found ({}). Use --remote to specify which one.",
            remotes.join(", ")
        )
        .into()),
    }
}

/// Main application logic for the default (no subcommand) flow.
///
/// This is the original create-worktree-and-setup-it flow.
fn run_create(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    // Discover repository
    let cwd = env::current_dir()?;
    let repo = discover_repo(&cwd)?;
    let repo_root = get_repo_root(&repo)?;

    output::print_header("Worktree Setup");
    output::print_repo_info(&repo_root.to_string_lossy());
    println!();

    // Discover and load configs
    let all_configs = discover_and_load_configs(&repo_root)?;

    // If --list, print available profiles and exit
    if args.list {
        let profile_display = collect_profile_display_info(&all_configs);
        if !profile_display.is_empty() {
            output::print_profile_list(&profile_display);
        }
        return Ok(());
    }

    // Resolve profiles (if --profile was provided)
    let resolved_profile = if args.profile.is_empty() {
        None
    } else {
        Some(resolve_and_print_profile(
            &args.profile,
            &all_configs,
            &repo_root,
        )?)
    };

    // Select configs: profile overrides normal selection
    let selected_configs: Vec<&LoadedConfig> = if all_configs.is_empty() {
        Vec::new()
    } else if let Some(indices) = select_configs_or_profile(
        &all_configs,
        args.non_interactive,
        &args.configs,
        resolved_profile.as_ref(),
    )? {
        indices.iter().map(|&i| &all_configs[i]).collect()
    } else {
        println!("No configs selected. Exiting.");
        return Ok(());
    };

    // Get target path
    let target_path = if let Some(ref path) = args.target_path {
        PathBuf::from(path)
    } else if args.non_interactive {
        output::print_error("Target path is required in non-interactive mode.");
        std::process::exit(1);
    } else {
        interactive::prompt_worktree_path()?
    };

    // Make target path absolute and normalize (resolve . and .. components)
    let target_path = if target_path.is_absolute() {
        target_path
    } else {
        cwd.join(&target_path)
    }
    .clean();

    // Get main worktree
    let main_worktree = get_main_worktree(&repo)?;

    // Check if target is the main worktree
    if target_path == main_worktree.path {
        output::print_error(
            "Cannot set up the main worktree. This tool is for secondary worktrees.",
        );
        std::process::exit(1);
    }

    // Handle worktree creation
    if !target_path.exists() {
        handle_worktree_creation(args, &repo, &target_path, resolved_profile.as_ref())?;
    }

    // Verify target exists
    if !target_path.exists() {
        output::print_error(&format!(
            "Target path does not exist: {}",
            target_path.display()
        ));
        std::process::exit(1);
    }

    // Apply config setup operations (only if configs were selected)
    if !selected_configs.is_empty() {
        apply_create_operations(
            args,
            &selected_configs,
            resolved_profile.as_ref(),
            &main_worktree.path,
            &target_path,
        )?;
    }

    output::print_success();
    Ok(())
}

/// Apply file operations and post-setup commands during worktree creation.
fn apply_create_operations(
    args: &Args,
    selected_configs: &[&LoadedConfig],
    resolved_profile: Option<&ResolvedProfile>,
    main_worktree_path: &Path,
    target_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\nSetting up worktree: {}", target_path.display());
    println!("Main worktree: {}\n", main_worktree_path.display());

    // Determine copy_unstaged: CLI flag > profile default > config default
    let copy_unstaged_override = args
        .copy_unstaged_override()
        .or_else(|| resolved_profile.and_then(|p| p.defaults.copy_unstaged));

    execute_file_operations(
        selected_configs,
        main_worktree_path,
        target_path,
        copy_unstaged_override,
        false, // No overwrite in create flow (fresh worktree)
        args.should_show_progress(),
    )?;

    println!();

    // Collect all post-setup commands
    let unique_commands = collect_post_setup_commands(selected_configs);

    if unique_commands.is_empty() {
        return Ok(());
    }

    // Resolve post-setup: CLI flag > profile > prompt
    let resolved_cmds =
        resolve_post_setup_commands(args.no_install, resolved_profile, &unique_commands);

    match resolved_cmds {
        Some(cmds) => {
            // Fully determined — run without prompting
            if !cmds.is_empty() {
                run_post_setup_commands(&cmds, target_path)?;
            }
        }
        None => {
            // Not determined — prompt the user (or run all in non-interactive)
            if args.non_interactive {
                run_post_setup_commands(&unique_commands, target_path)?;
            } else {
                let should_run = interactive::prompt_run_install(true)?;
                if should_run {
                    run_post_setup_commands(&unique_commands, target_path)?;
                }
            }
        }
    }

    Ok(())
}

/// Format result string based on operation type.
fn format_result_string(
    result: worktree_setup_operations::OperationResult,
    op_type: OperationType,
) -> String {
    use worktree_setup_operations::OperationResult;

    match (result, op_type) {
        (OperationResult::Created, OperationType::Symlink) => "symlink".to_string(),
        (OperationResult::Created, OperationType::Template) => "created".to_string(),
        (
            OperationResult::Created,
            OperationType::Copy
            | OperationType::CopyGlob
            | OperationType::Unstaged
            | OperationType::Overwrite,
        ) => "copied".to_string(),
        (OperationResult::Overwritten, _) => "overwritten".to_string(),
        (OperationResult::Exists, _) => "exists".to_string(),
        (OperationResult::Skipped, _) => "skipped".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use worktree_setup_config::{
        PostSetupKeyword, PostSetupMode, ProfileDefaults, ResolvedProfile,
    };

    /// Build a `ResolvedProfile` with the given defaults (helper).
    fn make_profile(defaults: ProfileDefaults) -> ResolvedProfile {
        ResolvedProfile {
            names: vec!["test".to_string()],
            defaults,
            ..Default::default()
        }
    }

    // ─── resolve_post_setup_commands ────────────────────────────────────

    #[test]
    fn test_resolve_post_setup_no_install_flag_wins() {
        // --no-install always returns empty, even if profile says "all"
        let profile = make_profile(ProfileDefaults {
            post_setup: Some(PostSetupMode::Keyword(PostSetupKeyword::All)),
            ..Default::default()
        });
        let cmds = vec!["bun install", "bun generate"];

        let result = resolve_post_setup_commands(true, Some(&profile), &cmds);
        assert_eq!(result, Some(Vec::<&str>::new()));
    }

    #[test]
    fn test_resolve_post_setup_none_keyword() {
        // post_setup = "none" → skip all, no prompt
        let profile = make_profile(ProfileDefaults {
            post_setup: Some(PostSetupMode::Keyword(PostSetupKeyword::None)),
            ..Default::default()
        });
        let cmds = vec!["bun install", "bun generate"];

        let result = resolve_post_setup_commands(false, Some(&profile), &cmds);
        assert_eq!(result, Some(Vec::<&str>::new()));
    }

    #[test]
    fn test_resolve_post_setup_all_keyword() {
        // post_setup = "all" → run all commands
        let profile = make_profile(ProfileDefaults {
            post_setup: Some(PostSetupMode::Keyword(PostSetupKeyword::All)),
            ..Default::default()
        });
        let cmds = vec!["bun install", "bun generate"];

        let result = resolve_post_setup_commands(false, Some(&profile), &cmds);
        assert_eq!(result, Some(vec!["bun install", "bun generate"]));
    }

    #[test]
    fn test_resolve_post_setup_all_with_skip() {
        // post_setup = "all" + skip_post_setup = ["bun generate"] → run all except skipped
        let profile = make_profile(ProfileDefaults {
            post_setup: Some(PostSetupMode::Keyword(PostSetupKeyword::All)),
            skip_post_setup: vec!["bun generate".to_string()],
            ..Default::default()
        });
        let cmds = vec!["bun install", "bun generate", "bun build"];

        let result = resolve_post_setup_commands(false, Some(&profile), &cmds);
        assert_eq!(result, Some(vec!["bun install", "bun build"]));
    }

    #[test]
    fn test_resolve_post_setup_commands_list() {
        // post_setup = ["bun install"] → run only matching commands
        let profile = make_profile(ProfileDefaults {
            post_setup: Some(PostSetupMode::Commands(vec!["bun install".to_string()])),
            ..Default::default()
        });
        let cmds = vec!["bun install", "bun generate", "bun build"];

        let result = resolve_post_setup_commands(false, Some(&profile), &cmds);
        assert_eq!(result, Some(vec!["bun install"]));
    }

    #[test]
    fn test_resolve_post_setup_commands_list_no_match() {
        // post_setup = ["nonexistent"] → empty (no matching available commands)
        let profile = make_profile(ProfileDefaults {
            post_setup: Some(PostSetupMode::Commands(vec!["nonexistent".to_string()])),
            ..Default::default()
        });
        let cmds = vec!["bun install", "bun generate"];

        let result = resolve_post_setup_commands(false, Some(&profile), &cmds);
        assert_eq!(result, Some(Vec::<&str>::new()));
    }

    #[test]
    fn test_resolve_post_setup_not_set_returns_none() {
        // post_setup not set → None (prompt the user)
        let profile = make_profile(ProfileDefaults::default());
        let cmds = vec!["bun install"];

        let result = resolve_post_setup_commands(false, Some(&profile), &cmds);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_post_setup_no_profile_returns_none() {
        // No profile at all → None (prompt the user)
        let cmds = vec!["bun install"];

        let result = resolve_post_setup_commands(false, None, &cmds);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_post_setup_no_install_without_profile() {
        // --no-install with no profile → empty
        let cmds = vec!["bun install"];

        let result = resolve_post_setup_commands(true, None, &cmds);
        assert_eq!(result, Some(Vec::<&str>::new()));
    }

    // ─── resolve_overwrite ─────────────────────────────────────────────

    #[test]
    fn test_resolve_overwrite_flag_wins() {
        // --overwrite flag → Some(true), regardless of profile
        let profile = make_profile(ProfileDefaults {
            overwrite_existing: Some(false),
            ..Default::default()
        });

        let result = resolve_overwrite(true, Some(&profile));
        assert_eq!(result, Some(true));
    }

    #[test]
    fn test_resolve_overwrite_from_profile() {
        // No flag, profile sets overwrite_existing → that value
        let profile = make_profile(ProfileDefaults {
            overwrite_existing: Some(true),
            ..Default::default()
        });

        let result = resolve_overwrite(false, Some(&profile));
        assert_eq!(result, Some(true));

        // Profile says false
        let profile_false = make_profile(ProfileDefaults {
            overwrite_existing: Some(false),
            ..Default::default()
        });
        let result = resolve_overwrite(false, Some(&profile_false));
        assert_eq!(result, Some(false));
    }

    #[test]
    fn test_resolve_overwrite_neither_set() {
        // No flag, no profile → None (prompt)
        let result = resolve_overwrite(false, None);
        assert_eq!(result, None);

        // No flag, profile doesn't set overwrite_existing → None
        let profile = make_profile(ProfileDefaults::default());
        let result = resolve_overwrite(false, Some(&profile));
        assert_eq!(result, None);
    }

    // ─── is_glob_pattern ────────────────────────────────────────────────

    #[test]
    fn test_is_glob_pattern() {
        assert!(!is_glob_pattern("node_modules"));
        assert!(!is_glob_pattern(".turbo"));
        assert!(!is_glob_pattern("path/to/dir"));
        assert!(is_glob_pattern("**/dist"));
        assert!(is_glob_pattern("*.log"));
        assert!(is_glob_pattern("src/[ab]"));
        assert!(is_glob_pattern("dir?name"));
    }

    // ─── format_size ────────────────────────────────────────────────────

    #[test]
    fn test_format_size() {
        assert_eq!(output::format_size(0), "0 B");
        assert_eq!(output::format_size(512), "512 B");
        assert_eq!(output::format_size(1023), "1023 B");
        assert_eq!(output::format_size(1024), "1.0 KB");
        assert_eq!(output::format_size(1536), "1.5 KB");
        assert_eq!(output::format_size(1_048_576), "1.0 MB");
        assert_eq!(output::format_size(1_572_864), "1.5 MB");
        assert_eq!(output::format_size(1_073_741_824), "1.0 GB");
    }

    // ─── resolve_clean_paths ────────────────────────────────────────────

    fn make_loaded_config_with_clean(
        relative_path: &str,
        config_dir: &Path,
        clean: Vec<String>,
    ) -> LoadedConfig {
        LoadedConfig {
            config: worktree_setup_config::Config {
                clean,
                ..Default::default()
            },
            config_path: config_dir.join("worktree.config.toml"),
            config_dir: config_dir.to_path_buf(),
            relative_path: relative_path.to_string(),
        }
    }

    #[test]
    fn test_resolve_clean_exact_paths() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        // Simulate config at repo_root/apps/my-app/
        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        // Simulate target worktree with same structure
        let target = repo_root.join("worktree");
        let target_app_dir = target.join("apps/my-app");
        std::fs::create_dir_all(&target_app_dir).unwrap();

        // Create files to clean in target
        std::fs::create_dir_all(target_app_dir.join("node_modules")).unwrap();
        std::fs::write(target_app_dir.join("node_modules/pkg.js"), "data").unwrap();
        std::fs::create_dir_all(target_app_dir.join(".turbo")).unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["node_modules".to_string(), ".turbo".to_string()],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        assert_eq!(resolved.len(), 2);
        // Check relative paths contain the expected names
        let rel_paths: Vec<&str> = resolved.iter().map(|(_, r)| r.as_str()).collect();
        assert!(rel_paths.iter().any(|p| p.contains("node_modules")));
        assert!(rel_paths.iter().any(|p| p.contains(".turbo")));
    }

    #[test]
    fn test_resolve_clean_glob_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");
        let target_app_dir = target.join("apps/my-app");

        // Create dist directories to match **/dist
        std::fs::create_dir_all(target_app_dir.join("dist")).unwrap();
        std::fs::write(target_app_dir.join("dist/bundle.js"), "code").unwrap();
        std::fs::create_dir_all(target_app_dir.join("src/dist")).unwrap();
        std::fs::write(target_app_dir.join("src/dist/out.js"), "code").unwrap();
        // Create a non-matching directory
        std::fs::create_dir_all(target_app_dir.join("src/lib")).unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["**/dist".to_string()],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        assert_eq!(resolved.len(), 2);
        let rel_paths: Vec<&str> = resolved.iter().map(|(_, r)| r.as_str()).collect();
        assert!(rel_paths.iter().all(|p| p.contains("dist")));
        assert!(!rel_paths.iter().any(|p| p.contains("lib")));
    }

    #[test]
    fn test_resolve_clean_nonexistent_paths_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");
        let target_app_dir = target.join("apps/my-app");
        std::fs::create_dir_all(&target_app_dir).unwrap();
        // Don't create node_modules — it shouldn't appear in results

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["node_modules".to_string()],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        assert!(resolved.is_empty());
    }

    #[test]
    fn test_resolve_clean_containment_rejects_escape() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");
        let target_app_dir = target.join("apps/my-app");
        std::fs::create_dir_all(&target_app_dir).unwrap();

        // Create a file outside the target that .. would reach
        std::fs::write(repo_root.join("secret.txt"), "sensitive").unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["../../../secret.txt".to_string()],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        // The escaping path should be rejected
        assert!(resolved.is_empty());
    }

    #[test]
    fn test_resolve_clean_dedup_across_configs() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");
        let target_app_dir = target.join("apps/my-app");
        std::fs::create_dir_all(target_app_dir.join("node_modules")).unwrap();

        // Two configs both reference the same path
        let config1 = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["node_modules".to_string()],
        );
        let config2 = make_loaded_config_with_clean(
            "apps/my-app/worktree.local.config.toml",
            &config_dir,
            vec!["node_modules".to_string()],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved =
            resolve_clean_paths(&[&config1, &config2], &target, &target_canonical, repo_root);

        // Should only appear once despite two configs referencing it
        assert_eq!(resolved.len(), 1);
    }
}
