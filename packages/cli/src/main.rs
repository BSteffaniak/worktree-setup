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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use clap::Parser;
use colored::Colorize;
use path_clean::PathClean;

use args::{Args, CleanArgs, RemoveArgs, SetupArgs};
use progress::ProgressManager;
use worktree_setup_config::{
    BranchDeletePolicy, CreationMethod, LoadedConfig, PostSetupKeyword, PostSetupMode,
    ResolvedProfile, discover_configs, load_config, load_global_config, resolve_profiles,
};
use worktree_setup_git::{
    GitError, Repository, WorktreeCreateOptions, WorktreeInfo, create_worktree, delete_branch,
    discover_repo, fetch_remote, get_current_branch, get_default_branch, get_local_branches,
    get_main_worktree, get_recent_branches, get_remotes, get_repo_root,
    get_unstaged_and_untracked_files, get_worktrees, prune_worktrees, remove_worktree,
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
        Some(args::Command::Remove(remove_args)) => remove_args.verbose,
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
        Some(args::Command::Remove(ref remove_args)) => run_remove(remove_args),
        None => run_create(&args),
    };

    if let Err(e) = result {
        output::print_error(&e.to_string());
        std::process::exit(1);
    }
}

/// Discover and load configs from a repo root, printing status.
///
/// Shows a spinner during discovery and loading to provide visual feedback,
/// especially when TypeScript configs require evaluation.
///
/// Returns the loaded configs. Prints warnings for configs that fail to load.
#[allow(clippy::literal_string_with_formatting_args)]
fn discover_and_load_configs(
    repo_root: &Path,
) -> Result<Vec<LoadedConfig>, Box<dyn std::error::Error>> {
    let spinner = indicatif::ProgressBar::new_spinner();
    spinner.set_style(
        indicatif::ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "])
            .template("{spinner:.cyan} {msg}")
            .expect("Invalid spinner template"),
    );
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    spinner.set_message("Discovering configs...");

    let config_paths = discover_configs(repo_root)?;

    let mut all_configs: Vec<LoadedConfig> = Vec::new();
    if config_paths.is_empty() {
        spinner.finish_and_clear();
        println!("No config files found.\n");
    } else {
        let total = config_paths.len();
        spinner.set_message(format!(
            "Loading {total} config{}...",
            if total == 1 { "" } else { "s" }
        ));

        let mut warnings: Vec<String> = Vec::new();
        for path in config_paths {
            match load_config(&path, repo_root) {
                Ok(config) => all_configs.push(config),
                Err(e) => {
                    warnings.push(format!("Failed to load {}: {}", path.display(), e));
                }
            }
        }

        spinner.finish_and_clear();

        // Print any warnings that occurred during loading
        for warning in &warnings {
            output::print_warning(warning);
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
///
/// The `global_config` is used to resolve per-config `allow_path_escape`:
/// * `Some(true)` → allow escape
/// * `Some(false)` → enforce containment
/// * `None` → inherit from `global_config.security.allow_path_escape`
fn execute_file_operations(
    selected_configs: &[&LoadedConfig],
    main_worktree_path: &Path,
    target_path: &Path,
    copy_unstaged_override: Option<bool>,
    overwrite_existing: bool,
    show_progress: bool,
    global_config: &worktree_setup_config::GlobalConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let progress_mgr = ProgressManager::new(show_progress);

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
        // Resolve allow_path_escape per-config: per-config overrides global
        let allow_path_escape = config
            .config
            .allow_path_escape
            .unwrap_or(global_config.security.allow_path_escape);

        let options = ApplyConfigOptions {
            copy_unstaged: copy_unstaged_override,
            overwrite_existing,
            allow_path_escape,
        };

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

        let global_config = load_global_config(Some(&repo_root))?;

        println!("\nApplying file operations to: {}", target_path.display());
        println!("Source (main worktree): {}\n", main_worktree.path.display());

        execute_file_operations(
            &selected_configs,
            &main_worktree.path,
            &target_path,
            copy_unstaged_override,
            overwrite_existing,
            args.should_show_progress(),
            &global_config,
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

/// Resolve the effective max-parallel worktree count from CLI args, env
/// var, and defaults.
///
/// Priority:
/// 1. `--max-parallel N` CLI flag (explicit wins)
/// 2. `WORKTREE_SETUP_MAX_PARALLEL` environment variable
/// 3. Default: `min(num_cpus::get(), num_worktrees).max(1)`
///
/// Returned value is always at least `1` and never exceeds
/// `num_worktrees` (when > 0).
fn resolve_max_parallel(args: &CleanArgs, num_worktrees: usize) -> usize {
    let env_value = std::env::var("WORKTREE_SETUP_MAX_PARALLEL").ok();
    resolve_max_parallel_inner(
        args.max_parallel,
        env_value.as_deref(),
        num_cpus::get(),
        num_worktrees,
    )
}

/// Inner, pure helper for `resolve_max_parallel` — all inputs are
/// passed explicitly, so this function is deterministic and testable
/// without touching process-global state.
///
/// Priority: `explicit_flag` > `env_value` > default.
/// Default: `min(num_cpus, max(num_worktrees, 1))`.
/// Clamping: result is always in `[1, max(num_worktrees, 1)]`.
fn resolve_max_parallel_inner(
    explicit_flag: Option<usize>,
    env_value: Option<&str>,
    num_cpus: usize,
    num_worktrees: usize,
) -> usize {
    let cap = num_worktrees.max(1);
    let default = num_cpus.min(cap).max(1);

    let parsed_env = env_value.and_then(|s| s.parse::<usize>().ok());
    let explicit = explicit_flag.or(parsed_env);

    explicit.map_or(default, |n| n.max(1).min(cap))
}

/// Resolve clean paths from selected configs into concrete items to delete.
///
/// For each config's `clean` entries:
/// * Paths starting with `/` are repo-root-relative (resolved against
///   `target_path`)
/// * Other paths are resolved relative to the config's directory as mapped
///   into the target worktree
/// * Glob patterns (containing `*`, `?`, or `[`) are expanded via a
///   `walkdir` + `globset` walk that skips symlinks and prunes matched dirs
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
    log::debug!(
        "resolve_clean_paths: target={} configs={}",
        target_path.display(),
        selected_configs.len()
    );

    let mut resolver = worktree_setup_glob::GlobResolver::new(
        target_canonical.to_path_buf(),
        worktree_setup_glob::GlobResolverOptions::default(),
    );

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

        // Group contiguous runs of patterns with the same base_dir and
        // resolve each run in a single walk via `resolve_many`. This
        // preserves iteration order (both across and within configs) so
        // first-seen dedup semantics are unchanged.
        let mut run_base: Option<PathBuf> = None;
        let mut run_patterns: Vec<&str> = Vec::new();

        let flush = |resolver: &mut worktree_setup_glob::GlobResolver,
                     results: &mut Vec<(PathBuf, String)>,
                     base: &Path,
                     patterns: &[&str]| {
            if patterns.is_empty() {
                return;
            }
            let matched = resolver.resolve_many(patterns, base);
            log::debug!(
                "  patterns={patterns:?} base={} matched={}",
                base.display(),
                matched.len(),
            );
            for entry in matched {
                results.push((entry.canonical, entry.display));
            }
        };

        for pattern in &config.config.clean {
            // Leading `/` means repo-root-relative (resolved against
            // `target_path`). Otherwise, resolve against the config's
            // directory mapped into the target worktree.
            let (effective_pattern, base_dir) = pattern.strip_prefix('/').map_or_else(
                || (pattern.as_str(), target_config_dir.clone()),
                |stripped| (stripped, target_path.to_path_buf()),
            );

            match &run_base {
                Some(current_base) if current_base == &base_dir => {
                    run_patterns.push(effective_pattern);
                }
                _ => {
                    if let Some(base) = run_base.take() {
                        flush(&mut resolver, &mut results, &base, &run_patterns);
                        run_patterns.clear();
                    }
                    run_base = Some(base_dir);
                    run_patterns.push(effective_pattern);
                }
            }
        }
        if let Some(base) = run_base.take() {
            flush(&mut resolver, &mut results, &base, &run_patterns);
        }
    }

    // Filter out paths that are descendants of other resolved paths.
    let as_resolved: Vec<worktree_setup_glob::ResolvedPath> = results
        .iter()
        .map(|(canonical, display)| worktree_setup_glob::ResolvedPath {
            canonical: canonical.clone(),
            display: display.clone(),
        })
        .collect();
    let filtered = worktree_setup_glob::filter_descendants(&as_resolved);
    filtered
        .into_iter()
        .map(|r| (r.canonical, r.display))
        .collect()
}

// ---------------------------------------------------------------------------
// Remove subcommand
// ---------------------------------------------------------------------------

/// Remove one or more worktrees and optionally delete their local branches.
///
/// Dispatch logic:
/// * Positional path given → remove that specific worktree
/// * No path, CWD is inside a linked worktree → remove that worktree
/// * No path, CWD is main worktree / repo root → interactive multi-select
fn run_remove(args: &RemoveArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Validate conflicting args
    if args.worktrees && args.target_path.is_some() {
        return Err(
            "--worktrees and a positional target path are mutually exclusive. \
             Use --worktrees to select worktrees interactively, or provide a target path."
                .into(),
        );
    }

    let cwd = env::current_dir()?;
    let repo = discover_repo(&cwd)?;
    let repo_root = get_repo_root(&repo)?;
    let global_config = load_global_config(Some(&repo_root))?;
    let worktrees = get_worktrees(&repo)?;

    output::print_header("Worktree Remove");
    output::print_repo_info(&repo_root.to_string_lossy());
    println!();

    // Mode 1: --worktrees flag — always interactive, regardless of CWD
    if args.worktrees {
        return run_remove_interactive(args, &repo, &worktrees, &global_config);
    }

    // Mode 2: explicit positional path
    if let Some(ref target) = args.target_path {
        let target_path = if target.is_absolute() {
            target.clone()
        } else {
            cwd.join(target).clean()
        };
        return run_remove_single(
            args,
            &repo,
            &repo_root,
            &worktrees,
            &target_path,
            &global_config,
        );
    }

    // Mode 3: no args — detect CWD context
    let cwd_canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());

    find_containing_linked_worktree(&cwd_canonical, &worktrees).map_or_else(
        || {
            // CWD is main worktree or repo root — interactive
            run_remove_interactive(args, &repo, &worktrees, &global_config)
        },
        |wt| {
            // CWD is inside a linked worktree — remove it
            run_remove_single(
                args,
                &repo,
                &repo_root,
                &worktrees,
                &wt.path,
                &global_config,
            )
        },
    )
}

/// Find the linked (non-main) worktree that contains the given path, if any.
fn find_containing_linked_worktree<'a>(
    path: &Path,
    worktrees: &'a [WorktreeInfo],
) -> Option<&'a WorktreeInfo> {
    worktrees.iter().find(|wt| {
        if wt.is_main {
            return false;
        }
        wt.path.canonicalize().map_or_else(
            |_| path.starts_with(&wt.path),
            |wt_canonical| path.starts_with(&wt_canonical),
        )
    })
}

/// Check if a worktree has uncommitted changes.
///
/// Returns `true` if the worktree has unstaged or untracked files.
/// Returns `false` on any error (conservative — don't block removal).
fn worktree_has_changes(worktree_path: &Path) -> bool {
    Repository::open(worktree_path).is_ok_and(|wt_repo| {
        get_unstaged_and_untracked_files(&wt_repo).is_ok_and(|files| !files.is_empty())
    })
}

/// Handle branch deletion for a removed worktree based on the global config policy.
fn handle_branch_deletion(
    repo: &Repository,
    branch: &str,
    policy: BranchDeletePolicy,
    non_interactive: bool,
    force: bool,
    dry_run: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let should_delete = match policy {
        BranchDeletePolicy::Always => true,
        BranchDeletePolicy::Never => return Ok(None),
        BranchDeletePolicy::Ask => {
            if non_interactive {
                // In non-interactive mode with Ask policy, skip branch deletion
                return Ok(None);
            }
            let prompt = format!("Delete local branch '{branch}'?");
            dialoguer::Confirm::new()
                .with_prompt(prompt)
                .default(false)
                .interact()?
        }
    };

    if !should_delete {
        return Ok(None);
    }

    if dry_run {
        println!("  Would delete branch '{branch}'");
        return Ok(Some(branch.to_string()));
    }

    // Try safe delete first, fall back to force if requested or confirmed
    match delete_branch(repo, branch, false) {
        Ok(()) => Ok(Some(branch.to_string())),
        Err(_) if force => {
            delete_branch(repo, branch, true)?;
            Ok(Some(branch.to_string()))
        }
        Err(_) if !non_interactive => {
            interactive::flush_stdin();
            let prompt = format!("Branch '{branch}' is not fully merged. Force-delete?");
            let confirmed = dialoguer::Confirm::new()
                .with_prompt(prompt)
                .default(false)
                .interact()?;
            if confirmed {
                delete_branch(repo, branch, true)?;
                Ok(Some(branch.to_string()))
            } else {
                Ok(None)
            }
        }
        Err(e) => {
            output::print_warning(&format!(
                "Could not delete branch '{branch}': {e}. Use --force to force-delete."
            ));
            Ok(None)
        }
    }
}

/// Remove a single worktree with confirmation, dirty check, and branch deletion.
fn run_remove_single(
    args: &RemoveArgs,
    repo: &Repository,
    repo_root: &Path,
    worktrees: &[WorktreeInfo],
    target_path: &Path,
    global_config: &worktree_setup_config::GlobalConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // Find the worktree matching the target path
    let target_canonical = target_path
        .canonicalize()
        .unwrap_or_else(|_| target_path.to_path_buf());

    let wt = worktrees
        .iter()
        .find(|w| w.path.canonicalize().unwrap_or_else(|_| w.path.clone()) == target_canonical)
        .ok_or_else(|| {
            format!(
                "No worktree found at '{}'. Use 'git worktree list' to see registered worktrees.",
                target_path.display()
            )
        })?;

    // Guard: cannot remove the main worktree
    if wt.is_main {
        return Err(format!(
            "Cannot remove the main worktree at '{}'.",
            target_path.display()
        )
        .into());
    }

    // Check for uncommitted changes
    let has_changes = worktree_has_changes(&wt.path);

    // Build preview
    let display_info = vec![output::RemoveDisplayInfo {
        branch: wt.branch.clone(),
        path: wt.path.to_string_lossy().to_string(),
        has_changes,
    }];
    output::print_remove_preview(&display_info);

    // Dry run
    if args.dry_run {
        if let Some(ref branch) = wt.branch {
            handle_branch_deletion(
                repo,
                branch,
                global_config.remove.branch_delete,
                args.non_interactive,
                args.force,
                true,
            )?;
        }
        println!("\n{}", "Dry run — nothing was removed.".dimmed());
        return Ok(());
    }

    // Confirmation
    if !args.force {
        if args.non_interactive {
            return Err(
                "Remove requires confirmation. Use --force to skip, or --dry-run to preview."
                    .into(),
            );
        }

        let confirm = dialoguer::Confirm::new()
            .with_prompt("Proceed with removal?")
            .default(false)
            .interact()?;

        if !confirm {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Detect if CWD is inside the worktree being removed
    let cwd = env::current_dir()?;
    let cwd_canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
    let cwd_inside = cwd_canonical.starts_with(&target_canonical);

    // Remove the worktree
    let force_remove = args.force || has_changes;
    match remove_worktree(repo, &wt.path, force_remove) {
        Ok(()) => {
            output::print_remove_summary(1, 0);
        }
        Err(e) => {
            output::print_error(&format!("Failed to remove worktree: {e}"));
            output::print_remove_summary(0, 1);
            return Err(e.into());
        }
    }

    // Branch deletion
    let mut deleted_branches = Vec::new();
    if let Some(ref branch) = wt.branch {
        // Re-open repo from repo_root since the worktree is now gone
        let repo = discover_repo(repo_root)?;
        if let Ok(Some(deleted)) = handle_branch_deletion(
            &repo,
            branch,
            global_config.remove.branch_delete,
            args.non_interactive,
            args.force,
            false,
        ) {
            deleted_branches.push(deleted);
        }
    }

    output::print_branch_delete_summary(&deleted_branches);

    if cwd_inside {
        output::print_cwd_removed_note();
    }

    Ok(())
}

/// Interactive multi-select removal of worktrees.
fn run_remove_interactive(
    args: &RemoveArgs,
    repo: &Repository,
    worktrees: &[WorktreeInfo],
    global_config: &worktree_setup_config::GlobalConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let linked_count = worktrees.iter().filter(|w| !w.is_main).count();
    if linked_count == 0 {
        output::print_info("No linked worktrees to remove.");
        return Ok(());
    }

    if args.non_interactive {
        return Err("Interactive worktree selection requires a terminal. \
             Provide a target path for non-interactive removal."
            .into());
    }

    // Spawn background threads to check each linked worktree for uncommitted changes.
    let (warning_tx, warning_rx) = mpsc::channel::<interactive::WarningResolution>();
    let checks_done = Arc::new(AtomicBool::new(false));
    let checks_done_clone = checks_done.clone();

    let worktree_paths: Vec<(usize, std::path::PathBuf, bool)> = worktrees
        .iter()
        .enumerate()
        .map(|(i, wt)| (i, wt.path.clone(), wt.is_main))
        .collect();

    std::thread::spawn(move || {
        let handles: Vec<_> = worktree_paths
            .into_iter()
            .filter(|(_, _, is_main)| !is_main)
            .map(|(index, path, _)| {
                let tx = warning_tx.clone();
                std::thread::spawn(move || {
                    let warning = if worktree_has_changes(&path) {
                        Some("has uncommitted changes".to_string())
                    } else {
                        None
                    };
                    let _ = tx.send(interactive::WarningResolution { index, warning });
                })
            })
            .collect();

        for handle in handles {
            let _ = handle.join();
        }
        checks_done_clone.store(true, Ordering::Relaxed);
    });

    // Show the picker (returns immediately, spinners animate while checks resolve)
    let (selection, resolved_warnings) =
        interactive::select_worktrees_for_removal(worktrees, &warning_rx, &checks_done)?;

    let Some(selected_indices) = selection else {
        println!("Cancelled.");
        return Ok(());
    };

    if selected_indices.is_empty() {
        println!("No worktrees selected. Exiting.");
        return Ok(());
    }

    let selected: Vec<&WorktreeInfo> = selected_indices.iter().map(|&i| &worktrees[i]).collect();

    // Build preview from resolved warnings (no re-checking needed)
    let display_infos: Vec<output::RemoveDisplayInfo> = selected_indices
        .iter()
        .map(|&i| {
            let wt = &worktrees[i];
            output::RemoveDisplayInfo {
                branch: wt.branch.clone(),
                path: wt.path.to_string_lossy().to_string(),
                has_changes: resolved_warnings.get(i).and_then(Option::as_ref).is_some(),
            }
        })
        .collect();

    output::print_remove_preview(&display_infos);

    // Dry run
    if args.dry_run {
        for wt in &selected {
            if let Some(ref branch) = wt.branch {
                handle_branch_deletion(
                    repo,
                    branch,
                    global_config.remove.branch_delete,
                    args.non_interactive,
                    args.force,
                    true,
                )?;
            }
        }
        println!("\n{}", "Dry run — nothing was removed.".dimmed());
        return Ok(());
    }

    // Confirmation
    if !args.force {
        let confirm = dialoguer::Confirm::new()
            .with_prompt(format!(
                "Remove {} worktree{}?",
                selected.len(),
                if selected.len() == 1 { "" } else { "s" }
            ))
            .default(false)
            .interact()?;

        if !confirm {
            println!("Cancelled.");
            return Ok(());
        }
    }

    execute_worktree_removals(
        args,
        repo,
        worktrees,
        &selected,
        &display_infos,
        global_config,
    )
}

/// Execute the actual removal of selected worktrees and their branches.
fn execute_worktree_removals(
    args: &RemoveArgs,
    repo: &Repository,
    worktrees: &[WorktreeInfo],
    selected: &[&WorktreeInfo],
    display_infos: &[output::RemoveDisplayInfo],
    global_config: &worktree_setup_config::GlobalConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut removed = 0usize;
    let mut failed = 0usize;
    let mut deleted_branches = Vec::new();

    // Detect if CWD is inside any selected worktree
    let cwd = env::current_dir()?;
    let cwd_canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
    let mut cwd_was_removed = false;

    for (idx, wt) in selected.iter().enumerate() {
        let has_changes = display_infos[idx].has_changes;
        let force_remove = args.force || has_changes;

        let wt_canonical = wt.path.canonicalize().unwrap_or_else(|_| wt.path.clone());
        if cwd_canonical.starts_with(&wt_canonical) {
            cwd_was_removed = true;
        }

        match remove_worktree(repo, &wt.path, force_remove) {
            Ok(()) => {
                removed += 1;
            }
            Err(e) => {
                output::print_warning(&format!(
                    "Failed to remove worktree at '{}': {e}",
                    wt.path.display()
                ));
                failed += 1;
                continue; // skip branch deletion for failed worktrees
            }
        }

        // Branch deletion
        if let Some(ref branch) = wt.branch {
            // Re-open repo from a still-valid path for each branch deletion
            if let Ok(fresh_repo) = discover_repo(&worktrees[0].path)
                && let Ok(Some(deleted)) = handle_branch_deletion(
                    &fresh_repo,
                    branch,
                    global_config.remove.branch_delete,
                    args.non_interactive,
                    args.force,
                    false,
                )
            {
                deleted_branches.push(deleted);
            }
        }
    }

    output::print_remove_summary(removed, failed);
    output::print_branch_delete_summary(&deleted_branches);

    if cwd_was_removed {
        output::print_cwd_removed_note();
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Clean subcommand
// ---------------------------------------------------------------------------

/// Run the `clean` subcommand.
///
/// Discovers clean paths from selected configs, shows a preview with sizes,
/// prompts for confirmation (unless `--force` or `--dry-run`), and deletes.
///
/// When `--worktrees` is set, presents an interactive multi-select of all
/// worktrees in the repository and cleans each selected one.
fn run_clean(args: &CleanArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Validate conflicting args
    if args.worktrees && args.target_path.is_some() {
        return Err(
            "--worktrees and a positional target path are mutually exclusive. \
             Use --worktrees to select worktrees interactively, or provide a target path."
                .into(),
        );
    }

    if args.worktrees {
        run_clean_multi_worktree(args)
    } else {
        run_clean_single(args)
    }
}

/// A group of resolved clean items for a single worktree.
struct WorktreeCleanGroup {
    /// Display label for the worktree (e.g. branch name).
    label: String,
    /// Resolved absolute paths paired with their display strings.
    resolved: Vec<(PathBuf, String)>,
    /// Preview items with type and size info.
    items: Vec<output::CleanItem>,
}

/// Build the shared rayon thread pool used by the multi-worktree
/// clean pipeline. Pool size follows `resolve_max_parallel`.
fn build_clean_pool(
    args: &CleanArgs,
    num_worktrees: usize,
) -> Result<Arc<rayon::ThreadPool>, Box<dyn std::error::Error>> {
    let max_parallel = resolve_max_parallel(args, num_worktrees);
    log::debug!("clean: max_parallel={max_parallel} worktrees={num_worktrees}");
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_parallel)
        .thread_name(|i| format!("wt-clean-{i}"))
        .build()?;
    Ok(Arc::new(pool))
}

/// Handle for the background resolution pipeline: the channel receiver
/// delivering [`WorktreeResolution`]s as they complete, plus the `done`
/// flag the picker consults to stop the spinner.
struct BackgroundResolution {
    rx: mpsc::Receiver<interactive::WorktreeResolution>,
    done: Arc<AtomicBool>,
}

/// Spawn one pool task per worktree; each task resolves clean paths
/// and computes sizes in parallel via the shared pool, sending a
/// `WorktreeResolution` into the shared channel.
///
/// The `done` flag flips to `true` when the last task completes so
/// the picker can stop animating its spinner.
fn start_background_resolution(
    worktrees: &[worktree_setup_git::WorktreeInfo],
    configs_arc: &Arc<Vec<LoadedConfig>>,
    repo_root_arc: &Arc<PathBuf>,
    pool: &Arc<rayon::ThreadPool>,
) -> BackgroundResolution {
    let (tx, rx) = mpsc::channel::<interactive::WorktreeResolution>();
    let done = Arc::new(AtomicBool::new(false));
    let remaining = Arc::new(std::sync::atomic::AtomicUsize::new(worktrees.len()));

    for (idx, wt) in worktrees.iter().enumerate() {
        let tx = tx.clone();
        let configs = configs_arc.clone();
        let root = repo_root_arc.clone();
        let wt = wt.clone();
        let pool_for_task = pool.clone();
        let remaining_for_task = remaining.clone();
        let done_for_task = done.clone();

        pool.spawn(move || {
            let configs_refs: Vec<&LoadedConfig> = configs.iter().collect();
            let resolution =
                resolve_single_worktree_with_pool(idx, &wt, &configs_refs, &root, &pool_for_task);
            let _ = tx.send(resolution);

            // Mark done when the last task finishes so the picker can
            // stop its spinner animation.
            if remaining_for_task.fetch_sub(1, Ordering::Relaxed) == 1 {
                done_for_task.store(true, Ordering::Relaxed);
            }
        });
    }

    // Drop the original sender so the channel closes once all tasks
    // complete.
    drop(tx);

    BackgroundResolution { rx, done }
}

/// Block-drain any resolutions remaining in the channel. Iterating
/// `rx` consumes it and yields until the channel closes (i.e. when
/// the last pool-task sender is dropped), ensuring we wait for all
/// in-flight resolutions to complete.
fn drain_resolutions_blocking(
    rx: mpsc::Receiver<interactive::WorktreeResolution>,
    mut initial: Vec<interactive::WorktreeResolution>,
) -> Vec<interactive::WorktreeResolution> {
    for res in rx {
        initial.push(res);
    }
    initial
}

/// Build the final grouped results for the selected worktrees,
/// preferring cached resolutions and falling back to synchronous
/// re-resolution on the shared pool if any are missing.
fn build_final_groups(
    selected_indices: &[usize],
    cached: &[interactive::WorktreeResolution],
    worktrees: &[worktree_setup_git::WorktreeInfo],
    configs_arc: &Arc<Vec<LoadedConfig>>,
    repo_root: &Path,
    pool: &rayon::ThreadPool,
) -> Vec<WorktreeCleanGroup> {
    let groups = build_groups_from_cache(selected_indices, cached, worktrees);
    if groups.len() == selected_indices.len() {
        return groups;
    }

    // Fall back to re-resolving on the pool for any selected worktrees
    // that weren't in the cache (rare, but possible if the user
    // confirmed before all background tasks completed).
    let configs_refs: Vec<&LoadedConfig> = configs_arc.iter().collect();
    let selected_wts: Vec<_> = selected_indices.iter().map(|&i| &worktrees[i]).collect();
    resolve_multi_worktree_clean(&selected_wts, &configs_refs, repo_root, pool)
}

/// Run multi-worktree clean: discover configs, spawn background resolution
/// threads, show an interactive multi-select with live size updates, then
/// preview and delete using cached results.
fn run_clean_multi_worktree(args: &CleanArgs) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;

    // Discover repository from cwd
    let repo = discover_repo(&cwd)?;
    let repo_root = get_repo_root(&repo)?;

    output::print_header("Worktree Clean (multi)");
    output::print_repo_info(&repo_root.to_string_lossy());
    println!();

    // Get all worktrees
    let worktrees = get_worktrees(&repo)?;

    if worktrees.len() < 2 {
        output::print_info("Only one worktree found. Use clean without --worktrees instead.");
        return Ok(());
    }

    // Prompt for worktree selection (always, even with --force)
    if args.non_interactive {
        return Err(
            "--worktrees requires interactive mode for worktree selection. \
             Remove --non-interactive to use this feature."
                .into(),
        );
    }

    // Discover configs and select which to use BEFORE showing the worktree
    // picker so that background threads can start resolving immediately.
    let selected_configs = discover_and_select_clean_configs(args, &repo_root)?;

    if selected_configs.is_empty() {
        return Ok(());
    }

    // Check if any selected config has clean paths
    let has_clean_paths = selected_configs.iter().any(|c| !c.config.clean.is_empty());
    if !has_clean_paths {
        output::print_info("No clean paths defined in selected configs.");
        return Ok(());
    }

    // Build the shared pool and kick off background resolution.
    let pool = build_clean_pool(args, worktrees.len())?;
    let configs_arc: Arc<Vec<LoadedConfig>> = Arc::new(selected_configs);
    let repo_root_arc: Arc<PathBuf> = Arc::new(repo_root.clone());
    let bg = start_background_resolution(&worktrees, &configs_arc, &repo_root_arc, &pool);

    // Show the interactive multi-select with live-updating sizes.
    let (selected_indices, cached_resolutions) =
        interactive::select_worktrees_with_sizes(&worktrees, &bg.rx, &bg.done)?;

    let Some(selected_indices) = selected_indices else {
        println!("Cancelled.");
        return Ok(());
    };

    if selected_indices.is_empty() {
        println!("No worktrees selected. Exiting.");
        return Ok(());
    }

    // Wait for any remaining in-flight resolutions.
    let cached_resolutions = drain_resolutions_blocking(bg.rx, cached_resolutions);

    // Build final groups — prefer cache; fall back to synchronous
    // re-resolution for any missing entries.
    let groups = build_final_groups(
        &selected_indices,
        &cached_resolutions,
        &worktrees,
        &configs_arc,
        &repo_root,
        &pool,
    );

    // Check if there's anything to clean
    let total_items: usize = groups.iter().map(|g| g.items.len()).sum();
    if total_items == 0 {
        output::print_info(
            "All clean paths are already clean across selected worktrees (nothing to delete).",
        );
        return Ok(());
    }

    // Build display groups for preview
    let display_groups: Vec<(String, Vec<output::CleanItem>)> = groups
        .iter()
        .map(|g| (g.label.clone(), g.items.clone()))
        .collect();

    println!();
    output::print_multi_worktree_clean_preview(&display_groups);

    // Dry run: stop here
    if args.dry_run {
        println!("\n{}", "Dry run — nothing was deleted.".dimmed());
        return Ok(());
    }

    // Confirm and delete
    confirm_and_delete_multi(args.force, args.non_interactive, &groups)
}

/// Build `WorktreeCleanGroup`s from cached `WorktreeResolution`s.
fn build_groups_from_cache(
    selected_indices: &[usize],
    resolutions: &[interactive::WorktreeResolution],
    worktrees: &[worktree_setup_git::WorktreeInfo],
) -> Vec<WorktreeCleanGroup> {
    let mut groups = Vec::new();

    for &idx in selected_indices {
        if let Some(res) = resolutions.iter().find(|r| r.index == idx) {
            groups.push(WorktreeCleanGroup {
                label: worktree_clean_label(&worktrees[idx]),
                resolved: res.resolved.clone(),
                items: res.items.clone(),
            });
        }
    }

    groups
}

/// Size a list of resolved `(abs_path, rel_path)` entries into
/// `output::CleanItem`s, using the given rayon pool to run `disk_usage`
/// calls in parallel.
///
/// The returned `Vec` preserves the input order — this is a HARD
/// guarantee since downstream code (preview, delete loop) iterates in
/// order and users see items in the original resolution order.
///
/// Each `disk_usage` call is internally serial (see
/// `worktree_setup_copy::count` notes on `Parallelism::Serial`); the
/// pool parallelism is purely at the item level.
fn size_items_in_pool(
    resolved: &[(PathBuf, String)],
    pool: &rayon::ThreadPool,
) -> Vec<output::CleanItem> {
    use rayon::prelude::*;

    pool.install(|| {
        resolved
            .par_iter()
            .map(|(abs_path, rel_path)| {
                let is_dir = abs_path.is_dir();
                let is_empty_dir = is_dir && is_dir_empty(abs_path);
                let size = worktree_setup_copy::disk_usage(abs_path);
                output::CleanItem {
                    relative_path: rel_path.clone(),
                    is_dir,
                    is_empty_dir,
                    size,
                }
            })
            .collect()
    })
}

fn is_dir_empty(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

/// Resolve clean paths for a single worktree and compute sizes in
/// parallel via the provided rayon pool.
///
/// Behaviorally identical to `resolve_single_worktree`, but sizes each
/// matched path concurrently via `size_items_in_pool`. The resolved
/// vector and items vector preserve the same order as the non-pool
/// version — downstream preview, confirm, and delete flows are
/// unchanged.
fn resolve_single_worktree_with_pool(
    index: usize,
    wt: &worktree_setup_git::WorktreeInfo,
    configs: &[&LoadedConfig],
    repo_root: &Path,
    pool: &rayon::ThreadPool,
) -> interactive::WorktreeResolution {
    let target_path = &wt.path;
    let Ok(target_canonical) = target_path.canonicalize() else {
        log::warn!(
            "resolve_single_worktree_with_pool[{index}]: cannot canonicalize {}",
            target_path.display()
        );
        return interactive::WorktreeResolution {
            index,
            resolved: Vec::new(),
            items: Vec::new(),
            stats: output::CleanStats::inaccessible(),
        };
    };

    let resolved = resolve_clean_paths(configs, target_path, &target_canonical, repo_root);

    let items = size_items_in_pool(&resolved, pool);

    let stats = output::clean_stats(&items);

    interactive::WorktreeResolution {
        index,
        resolved,
        items,
        stats,
    }
}

/// Discover configs, resolve profiles, and select which configs to use for cleaning.
///
/// Shared logic between single and multi-worktree clean modes.
fn discover_and_select_clean_configs(
    args: &CleanArgs,
    repo_root: &Path,
) -> Result<Vec<LoadedConfig>, Box<dyn std::error::Error>> {
    let all_configs = discover_and_load_configs(repo_root)?;

    if all_configs.is_empty() {
        output::print_warning("No configs found. Nothing to clean.");
        return Ok(Vec::new());
    }

    // Resolve profiles if --profile was provided
    let resolved_profile = if args.profile.is_empty() {
        None
    } else {
        Some(resolve_and_print_profile(
            &args.profile,
            &all_configs,
            repo_root,
        )?)
    };

    // Select configs
    let selected_indices = select_configs_or_profile(
        &all_configs,
        args.non_interactive,
        &args.configs,
        resolved_profile.as_ref(),
    )?;

    let Some(selected_indices) = selected_indices else {
        return Ok(Vec::new());
    };

    Ok(selected_indices
        .iter()
        .map(|&i| all_configs[i].clone())
        .collect())
}

/// Resolve clean paths for each worktree, building grouped results.
///
/// Uses the provided rayon pool for parallel `disk_usage` computation
/// per worktree (via [`resolve_single_worktree_with_pool`]). This is
/// the synchronous fallback path used when the interactive picker
/// returns before all background resolutions complete — rare in
/// practice because we now block on channel closure, but kept for
/// safety.
///
/// Preserves the HARD warning string `"Could not access worktree
/// '<path>': <err>"` on canonicalize failure.
fn resolve_multi_worktree_clean(
    worktrees: &[&worktree_setup_git::WorktreeInfo],
    selected_configs: &[&LoadedConfig],
    repo_root: &Path,
    pool: &rayon::ThreadPool,
) -> Vec<WorktreeCleanGroup> {
    let mut groups = Vec::new();

    for (idx, wt) in worktrees.iter().enumerate() {
        let target_path = &wt.path;

        // Check canonicalization here (not inside
        // `resolve_single_worktree_with_pool`) because we need the
        // original error to preserve the user-visible warning text.
        if let Err(e) = target_path.canonicalize() {
            output::print_warning(&format!(
                "Could not access worktree '{}': {}",
                target_path.display(),
                e
            ));
            continue;
        }

        let resolution =
            resolve_single_worktree_with_pool(idx, wt, selected_configs, repo_root, pool);

        let label = worktree_clean_label(wt);
        groups.push(WorktreeCleanGroup {
            label,
            resolved: resolution.resolved,
            items: resolution.items,
        });
    }

    groups
}

/// Build a display label for a worktree in clean output.
fn worktree_clean_label(wt: &worktree_setup_git::WorktreeInfo) -> String {
    let suffix = if wt.is_main { " [main]" } else { "" };
    wt.branch.as_ref().map_or_else(
        || {
            wt.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
                + suffix
        },
        |branch| format!("{branch}{suffix}"),
    )
}

/// Confirm and delete items across multiple worktrees.
fn confirm_and_delete_multi(
    force: bool,
    non_interactive: bool,
    groups: &[WorktreeCleanGroup],
) -> Result<(), Box<dyn std::error::Error>> {
    if !force {
        if non_interactive {
            return Err(
                "Clean requires confirmation. Use --force to skip, or --dry-run to preview.".into(),
            );
        }

        let confirm = dialoguer::Confirm::new()
            .with_prompt("Proceed with deletion across all selected worktrees?")
            .default(false)
            .interact()?;

        if !confirm {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let mut deleted_count = 0usize;
    let mut total_size = 0u64;
    let mut worktrees_cleaned = 0usize;

    for group in groups {
        if group.items.is_empty() {
            continue;
        }

        let mut worktree_had_deletion = false;

        for (idx, (abs_path, _)) in group.resolved.iter().enumerate() {
            let item = &group.items[idx];
            let result = if abs_path.is_dir() {
                std::fs::remove_dir_all(abs_path)
            } else {
                std::fs::remove_file(abs_path)
            };

            match result {
                Ok(()) => {
                    deleted_count += 1;
                    total_size += item.size;
                    worktree_had_deletion = true;
                }
                Err(e) => {
                    output::print_warning(&format!(
                        "Failed to delete '{}': {}",
                        item.relative_path, e
                    ));
                }
            }
        }

        if worktree_had_deletion {
            worktrees_cleaned += 1;
        }
    }

    println!();
    output::print_multi_worktree_clean_summary(deleted_count, total_size, worktrees_cleaned);

    Ok(())
}

/// Run single-worktree clean (original behavior).
fn run_clean_single(args: &CleanArgs) -> Result<(), Box<dyn std::error::Error>> {
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

    // Build preview items with sizes, in parallel via a small rayon pool.
    // Single-worktree mode only has one worktree's worth of matches, so
    // a modest pool size suffices; default to `num_cpus` (capped by the
    // number of matches).
    let max_parallel = resolve_max_parallel(args, resolved.len());
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(max_parallel)
        .thread_name(|i| format!("wt-clean-single-{i}"))
        .build()?;
    let items = size_items_in_pool(&resolved, &pool);

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
            interactive::flush_stdin();
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
        Err(ref e) if is_branch_exists_error(e).is_some() => {
            let branch = is_branch_exists_error(e).unwrap();

            if non_interactive {
                return Err(format!(
                    "Branch '{branch}' already exists. \
                     Use a different branch name or delete it first."
                )
                .into());
            }

            // Interactive recovery
            interactive::flush_stdin();
            match interactive::prompt_branch_exists_recovery(&branch)? {
                interactive::BranchExistsAction::UseExisting => {
                    println!("Using existing branch '{branch}'...");
                    let reuse_opts = WorktreeCreateOptions {
                        branch: Some(branch),
                        new_branch: None,
                        detach: options.detach,
                        force: options.force,
                    };
                    create_worktree(repo, path, &reuse_opts)?;
                    Ok(())
                }
                interactive::BranchExistsAction::DeleteAndCreate => {
                    println!("Deleting branch '{branch}' and retrying...");
                    delete_branch(repo, &branch, true)?;
                    create_worktree(repo, path, options)?;
                    Ok(())
                }
                interactive::BranchExistsAction::Cancel => {
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

/// Check if a `GitError` is a "branch already exists" error.
///
/// Returns the branch name if the error matches, `None` otherwise.
fn is_branch_exists_error(err: &GitError) -> Option<String> {
    match err {
        GitError::WorktreeCreateError { source, .. } => {
            let msg = source.message();
            if msg.contains("a branch named") && msg.contains("already exists") {
                // Extract branch name from: "a branch named 'foo' already exists"
                msg.split('\'').nth(1).map(String::from)
            } else {
                None
            }
        }
        _ => None,
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
        let global_config = load_global_config(Some(&repo_root))?;
        apply_create_operations(
            args,
            &selected_configs,
            resolved_profile.as_ref(),
            &main_worktree.path,
            &target_path,
            &global_config,
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
    global_config: &worktree_setup_config::GlobalConfig,
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
        global_config,
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

    // ─── resolve_max_parallel_inner ─────────────────────────────────────

    /// With no explicit flag and no env value, the default is
    /// `min(num_cpus, max(num_worktrees, 1))`.
    #[test]
    fn test_resolve_max_parallel_default_uses_min_cpus_and_worktrees() {
        // More CPUs than worktrees → capped at worktrees.
        assert_eq!(resolve_max_parallel_inner(None, None, 16, 4), 4);
        // Fewer CPUs than worktrees → capped at CPUs.
        assert_eq!(resolve_max_parallel_inner(None, None, 4, 16), 4);
        // Equal → either value.
        assert_eq!(resolve_max_parallel_inner(None, None, 8, 8), 8);
    }

    /// An explicit CLI flag overrides everything else.
    #[test]
    fn test_resolve_max_parallel_explicit_flag_wins() {
        // Flag wins over env.
        assert_eq!(
            resolve_max_parallel_inner(Some(3), Some("99"), 16, 16),
            3,
            "explicit flag must take precedence over env"
        );
        // Flag wins over default.
        assert_eq!(resolve_max_parallel_inner(Some(5), None, 16, 50), 5);
    }

    /// The env var is consulted only when the flag is absent.
    #[test]
    fn test_resolve_max_parallel_env_var_used_when_flag_absent() {
        assert_eq!(resolve_max_parallel_inner(None, Some("7"), 16, 50), 7);
        // Malformed env value is ignored, falling through to the default.
        assert_eq!(
            resolve_max_parallel_inner(None, Some("not-a-number"), 4, 10),
            4
        );
        // Empty env value is ignored.
        assert_eq!(resolve_max_parallel_inner(None, Some(""), 4, 10), 4);
    }

    /// The result is always at least 1, even if inputs ask for less.
    #[test]
    fn test_resolve_max_parallel_clamps_to_at_least_one() {
        // Explicit 0 → clamped to 1.
        assert_eq!(resolve_max_parallel_inner(Some(0), None, 16, 16), 1);
        // Env "0" → clamped to 1.
        assert_eq!(resolve_max_parallel_inner(None, Some("0"), 16, 16), 1);
        // num_cpus=0 (pathological) → clamped to 1 via the default branch.
        assert_eq!(resolve_max_parallel_inner(None, None, 0, 10), 1);
        // num_worktrees=0 (no worktrees) → result is still at least 1.
        assert_eq!(resolve_max_parallel_inner(None, None, 16, 0), 1);
    }

    /// Explicit values never exceed `num_worktrees`.
    #[test]
    fn test_resolve_max_parallel_clamps_to_num_worktrees() {
        // Flag asks for 100, only 4 worktrees → capped at 4.
        assert_eq!(resolve_max_parallel_inner(Some(100), None, 16, 4), 4);
        // Env asks for 50, only 3 worktrees → capped at 3.
        assert_eq!(resolve_max_parallel_inner(None, Some("50"), 16, 3), 3);
        // With 0 worktrees the cap floor is 1, so even a flag of 100 → 1.
        assert_eq!(resolve_max_parallel_inner(Some(100), None, 16, 0), 1);
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

    // ─── format_size ────────────────────────────────────────────────────

    #[test]
    fn test_format_size() {
        assert_eq!(output::format_size(0), "0 B");
        assert_eq!(output::format_size(512), "512 B");
        assert_eq!(output::format_size(1023), "1023 B");
        assert_eq!(output::format_size(1024), "1.0 KiB");
        assert_eq!(output::format_size(1536), "1.5 KiB");
        assert_eq!(output::format_size(1_048_576), "1.0 MiB");
        assert_eq!(output::format_size(1_572_864), "1.5 MiB");
        assert_eq!(output::format_size(1_073_741_824), "1.0 GiB");
    }

    #[test]
    fn test_is_dir_empty_distinguishes_almost_empty() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        let zero_file_dir = dir.path().join("zero-file");
        let empty_child_dir = dir.path().join("empty-child");

        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&zero_file_dir).unwrap();
        std::fs::write(zero_file_dir.join("zero.txt"), "").unwrap();
        std::fs::create_dir_all(empty_child_dir.join("child")).unwrap();

        assert!(is_dir_empty(&empty));
        assert!(!is_dir_empty(&zero_file_dir));
        assert!(!is_dir_empty(&empty_child_dir));
    }

    #[test]
    fn test_clean_stats_formats_empty_dirs_separately() {
        let items = vec![
            output::CleanItem {
                relative_path: "empty".to_string(),
                is_dir: true,
                is_empty_dir: true,
                size: 0,
            },
            output::CleanItem {
                relative_path: "zero-file".to_string(),
                is_dir: false,
                is_empty_dir: false,
                size: 0,
            },
        ];

        let stats = output::clean_stats(&items);
        assert_eq!(stats.item_count, 2);
        assert_eq!(stats.empty_dir_count, 1);
        assert_eq!(
            output::format_clean_stats_plain(&stats),
            "2 items, 0 B, 1 empty dir"
        );
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

    #[test]
    fn test_resolve_clean_filters_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");
        let target_app_dir = target.join("apps/my-app");

        // Create node_modules with a nested dist directory
        std::fs::create_dir_all(target_app_dir.join("node_modules/pkg/dist")).unwrap();
        std::fs::write(
            target_app_dir.join("node_modules/pkg/dist/index.js"),
            "code",
        )
        .unwrap();

        // Create a dist directory outside node_modules (should remain)
        std::fs::create_dir_all(target_app_dir.join("packages/utils/dist")).unwrap();
        std::fs::write(target_app_dir.join("packages/utils/dist/index.js"), "code").unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["node_modules".to_string(), "**/dist".to_string()],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        // Should have 2 items: node_modules and packages/utils/dist
        // The node_modules/pkg/dist should be filtered out as a descendant
        assert_eq!(resolved.len(), 2);
        let rel_paths: Vec<&str> = resolved.iter().map(|(_, r)| r.as_str()).collect();
        assert!(rel_paths.iter().any(|p| p.ends_with("node_modules")));
        assert!(rel_paths.iter().any(|p| p.ends_with("packages/utils/dist")));
        // Ensure the nested dist is NOT present
        assert!(!rel_paths.iter().any(|p| p.contains("node_modules/pkg")));
    }

    #[test]
    fn test_resolve_clean_glob_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");
        let target_app_dir = target.join("apps/my-app");

        // Create a real dist directory (should be found)
        std::fs::create_dir_all(target_app_dir.join("src/dist")).unwrap();
        std::fs::write(target_app_dir.join("src/dist/bundle.js"), "code").unwrap();

        // Create a directory that will be the symlink target (outside normal hierarchy)
        let cache_dir = target.join("cache/pkg");
        std::fs::create_dir_all(cache_dir.join("dist")).unwrap();
        std::fs::write(cache_dir.join("dist/cached.js"), "cached").unwrap();

        // Create node_modules/pkg as a symlink -> ../../cache/pkg
        std::fs::create_dir_all(target_app_dir.join("node_modules")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&cache_dir, target_app_dir.join("node_modules/pkg")).unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["**/dist".to_string()],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        // On unix: only src/dist should appear; node_modules/pkg/dist should be skipped
        // because node_modules/pkg is a symlink
        #[cfg(unix)]
        {
            assert_eq!(resolved.len(), 1);
            assert!(resolved[0].1.contains("src/dist"));
        }

        // On non-unix: symlink wasn't created, so only src/dist will match anyway
        #[cfg(not(unix))]
        {
            assert_eq!(resolved.len(), 1);
        }
    }

    #[test]
    fn test_resolve_clean_root_relative_exact() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        // Config lives in apps/my-app but the clean path is root-relative
        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");
        // Create node_modules at the worktree root (NOT inside apps/my-app)
        std::fs::create_dir_all(target.join("node_modules/pkg")).unwrap();
        std::fs::write(target.join("node_modules/pkg/index.js"), "code").unwrap();
        // Also create the target config dir so it exists
        std::fs::create_dir_all(target.join("apps/my-app")).unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["/node_modules".to_string()],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].1, "node_modules");
    }

    #[test]
    fn test_resolve_clean_root_relative_glob() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");

        // Create dist dirs at various levels (all relative to worktree root)
        std::fs::create_dir_all(target.join("apps/my-app/dist")).unwrap();
        std::fs::write(target.join("apps/my-app/dist/bundle.js"), "code").unwrap();
        std::fs::create_dir_all(target.join("packages/utils/dist")).unwrap();
        std::fs::write(target.join("packages/utils/dist/lib.js"), "code").unwrap();
        // Non-matching directory
        std::fs::create_dir_all(target.join("packages/utils/src")).unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["/**/dist".to_string()],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        // Should find both dist dirs from the worktree root
        assert_eq!(resolved.len(), 2);
        let rel_paths: Vec<&str> = resolved.iter().map(|(_, r)| r.as_str()).collect();
        assert!(rel_paths.iter().all(|p| p.contains("dist")));
        assert!(rel_paths.iter().any(|p| p.contains("apps/my-app/dist")));
        assert!(rel_paths.iter().any(|p| p.contains("packages/utils/dist")));
    }

    #[test]
    fn test_resolve_clean_mixed_relative_and_root() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");
        let target_app_dir = target.join("apps/my-app");

        // Config-relative: node_modules inside apps/my-app
        std::fs::create_dir_all(target_app_dir.join("node_modules")).unwrap();
        // Root-relative: .turbo at the worktree root
        std::fs::create_dir_all(target.join(".turbo")).unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec![
                "node_modules".to_string(), // config-relative
                "/.turbo".to_string(),      // root-relative
            ],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        assert_eq!(resolved.len(), 2);
        let rel_paths: Vec<&str> = resolved.iter().map(|(_, r)| r.as_str()).collect();
        assert!(rel_paths.iter().any(|p| p.contains("node_modules")));
        assert!(rel_paths.iter().any(|p| p == &".turbo"));
    }

    /// Patterns are grouped into runs of the same `base_dir` in
    /// `resolve_clean_paths` (to enable a single batched walk per
    /// contiguous run). This test uses a deliberately alternating list
    /// — root-relative, config-relative, root-relative, config-relative
    /// — to exercise the grouping logic across run boundaries.
    ///
    /// Locks in: the batched grouping produces the same end result as
    /// naive per-pattern resolution regardless of how `base_dir`s
    /// alternate in the input.
    #[test]
    fn test_resolve_clean_paths_alternating_base_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");
        let target_app_dir = target.join("apps/my-app");

        // Root-relative targets: at worktree root
        std::fs::create_dir_all(target.join(".turbo")).unwrap();
        std::fs::create_dir_all(target.join("dist")).unwrap();

        // Config-relative targets: under apps/my-app
        std::fs::create_dir_all(target_app_dir.join("node_modules")).unwrap();
        std::fs::create_dir_all(target_app_dir.join("build")).unwrap();

        // Alternating pattern list crosses run boundaries 3 times.
        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec![
                "/.turbo".to_string(),      // run 1: root-relative
                "node_modules".to_string(), // run 2: config-relative
                "/dist".to_string(),        // run 3: root-relative
                "build".to_string(),        // run 4: config-relative
            ],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        // All four paths must resolve, regardless of the alternating order.
        let rel_paths: std::collections::BTreeSet<String> =
            resolved.iter().map(|(_, r)| r.clone()).collect();
        assert_eq!(
            resolved.len(),
            4,
            "expected 4 resolved paths, got: {rel_paths:?}"
        );
        assert!(rel_paths.iter().any(|p| p == ".turbo"));
        assert!(rel_paths.iter().any(|p| p == "dist"));
        assert!(rel_paths.iter().any(|p| p.ends_with("node_modules")));
        assert!(rel_paths.iter().any(|p| p.ends_with("build")));
    }

    #[test]
    fn test_resolve_clean_glob_prunes_matched_dirs() {
        // When **/node_modules matches a node_modules dir, it should NOT
        // recurse into it. Verify that nested matches inside a matched dir
        // don't appear in results (the parent match subsumes them).
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        let target = repo_root.join("worktree");
        let target_app_dir = target.join("apps/my-app");

        // Create a node_modules with deeply nested sub-node_modules
        // (simulating a non-hoisted dependency)
        std::fs::create_dir_all(target_app_dir.join("node_modules/pkg/node_modules/nested-pkg"))
            .unwrap();
        std::fs::write(
            target_app_dir.join("node_modules/pkg/node_modules/nested-pkg/index.js"),
            "code",
        )
        .unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["**/node_modules".to_string()],
        );

        let target_canonical = target.canonicalize().unwrap();
        let resolved = resolve_clean_paths(&[&config], &target, &target_canonical, repo_root);

        // Should only have 1 result: the top-level node_modules.
        // The nested node_modules/pkg/node_modules should NOT appear because
        // walkdir prunes the parent node_modules/ on match.
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].1.ends_with("node_modules"));
        assert!(!resolved[0].1.contains("pkg"));
    }

    // ─── worktree_clean_label ───────────────────────────────────────────

    #[test]
    fn test_worktree_clean_label_with_branch() {
        let wt = worktree_setup_git::WorktreeInfo {
            path: PathBuf::from("/repo/feature"),
            is_main: false,
            branch: Some("feature-auth".to_string()),
            commit: Some("abc12345".to_string()),
        };
        assert_eq!(worktree_clean_label(&wt), "feature-auth");
    }

    #[test]
    fn test_worktree_clean_label_main_worktree() {
        let wt = worktree_setup_git::WorktreeInfo {
            path: PathBuf::from("/repo"),
            is_main: true,
            branch: Some("master".to_string()),
            commit: Some("abc12345".to_string()),
        };
        assert_eq!(worktree_clean_label(&wt), "master [main]");
    }

    #[test]
    fn test_worktree_clean_label_no_branch() {
        let wt = worktree_setup_git::WorktreeInfo {
            path: PathBuf::from("/repo/detached-wt"),
            is_main: false,
            branch: None,
            commit: Some("abc12345".to_string()),
        };
        // Falls back to directory name
        assert_eq!(worktree_clean_label(&wt), "detached-wt");
    }

    #[test]
    fn test_worktree_clean_label_no_branch_main() {
        let wt = worktree_setup_git::WorktreeInfo {
            path: PathBuf::from("/repo"),
            is_main: true,
            branch: None,
            commit: Some("abc12345".to_string()),
        };
        assert_eq!(worktree_clean_label(&wt), "repo [main]");
    }

    // ─── resolve_multi_worktree_clean ───────────────────────────────────

    #[test]
    fn test_resolve_multi_worktree_clean() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        // Config at repo root level
        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        // Two "worktrees" (just directories for testing)
        let wt1_path = repo_root.join("wt1");
        let wt2_path = repo_root.join("wt2");

        // Create clean targets in both worktrees
        std::fs::create_dir_all(wt1_path.join("apps/my-app/node_modules")).unwrap();
        std::fs::write(wt1_path.join("apps/my-app/node_modules/pkg.js"), "data1").unwrap();

        std::fs::create_dir_all(wt2_path.join("apps/my-app/node_modules")).unwrap();
        std::fs::write(wt2_path.join("apps/my-app/node_modules/pkg.js"), "data2").unwrap();
        std::fs::create_dir_all(wt2_path.join("apps/my-app/.turbo")).unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["node_modules".to_string(), ".turbo".to_string()],
        );

        let wt1 = worktree_setup_git::WorktreeInfo {
            path: wt1_path,
            is_main: false,
            branch: Some("feature-a".to_string()),
            commit: None,
        };
        let wt2 = worktree_setup_git::WorktreeInfo {
            path: wt2_path,
            is_main: false,
            branch: Some("feature-b".to_string()),
            commit: None,
        };

        let worktrees = vec![&wt1, &wt2];
        let configs = vec![&config];

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let groups = resolve_multi_worktree_clean(&worktrees, &configs, repo_root, &pool);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].label, "feature-a");
        assert_eq!(groups[0].items.len(), 1); // only node_modules exists in wt1
        assert_eq!(groups[1].label, "feature-b");
        assert_eq!(groups[1].items.len(), 2); // node_modules + .turbo exist in wt2
    }

    #[test]
    fn test_resolve_multi_worktree_clean_empty_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        // Worktree exists but has no clean targets
        let wt_path = repo_root.join("wt-empty");
        std::fs::create_dir_all(wt_path.join("apps/my-app")).unwrap();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["node_modules".to_string()],
        );

        let wt = worktree_setup_git::WorktreeInfo {
            path: wt_path,
            is_main: false,
            branch: Some("empty-branch".to_string()),
            commit: None,
        };

        let worktrees = vec![&wt];
        let configs = vec![&config];

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let groups = resolve_multi_worktree_clean(&worktrees, &configs, repo_root, &pool);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "empty-branch");
        assert!(groups[0].items.is_empty());
    }

    /// An inaccessible worktree (path that fails `canonicalize`) must
    /// be skipped from the output and must produce a warning in the
    /// legacy format `"Could not access worktree '<path>': <err>"`.
    ///
    /// This test covers the error-handling path in
    /// `resolve_multi_worktree_clean` — preserved across the pool
    /// refactor so the user-visible warning text doesn't drift.
    #[test]
    fn test_resolve_multi_worktree_clean_inaccessible_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        // Good worktree
        let good_path = repo_root.join("good-wt");
        std::fs::create_dir_all(good_path.join("apps/my-app/node_modules")).unwrap();

        // Bad worktree — path doesn't exist on disk, so canonicalize fails
        let bad_path = repo_root.join("does-not-exist");

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["node_modules".to_string()],
        );

        let good_wt = worktree_setup_git::WorktreeInfo {
            path: good_path,
            is_main: false,
            branch: Some("good".to_string()),
            commit: None,
        };
        let bad_wt = worktree_setup_git::WorktreeInfo {
            path: bad_path,
            is_main: false,
            branch: Some("bad".to_string()),
            commit: None,
        };

        let worktrees = vec![&good_wt, &bad_wt];
        let configs = vec![&config];

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let groups = resolve_multi_worktree_clean(&worktrees, &configs, repo_root, &pool);

        // Only the good worktree produces a group; the bad one is skipped.
        assert_eq!(
            groups.len(),
            1,
            "inaccessible worktrees must be omitted from groups"
        );
        assert_eq!(groups[0].label, "good");
        assert_eq!(groups[0].items.len(), 1);
    }

    /// End-to-end check that `resolve_single_worktree_with_pool`
    /// produces identical `WorktreeResolution` values to what would be
    /// produced by a synchronous, single-threaded reference
    /// implementation. Uses several worktrees and runs the
    /// pool-based function concurrently against one shared pool.
    ///
    /// This is the integration-level safety net for the pool code path:
    /// if any change to `resolve_single_worktree_with_pool` or
    /// `size_items_in_pool` starts returning different summaries,
    /// orderings, or items, this test catches it.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_resolve_single_worktree_with_pool_matches_sequential_reference() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path();

        let config_dir = repo_root.join("apps/my-app");
        std::fs::create_dir_all(&config_dir).unwrap();

        // Build 5 worktrees with a mix of clean-target content.
        let worktree_paths: Vec<PathBuf> = (0..5)
            .map(|i| {
                let wt_path = repo_root.join(format!("wt-{i}"));
                let app = wt_path.join("apps/my-app");
                // Always some node_modules content (size-varying).
                std::fs::create_dir_all(app.join("node_modules")).unwrap();
                for j in 0..=i {
                    std::fs::write(
                        app.join(format!("node_modules/pkg-{j}.js")),
                        vec![b'x'; 100 * (j + 1)],
                    )
                    .unwrap();
                }
                // Every other worktree also has a .turbo dir.
                if i % 2 == 0 {
                    std::fs::create_dir_all(app.join(".turbo")).unwrap();
                    std::fs::write(app.join(".turbo/log"), "log").unwrap();
                }
                wt_path
            })
            .collect();

        let config = make_loaded_config_with_clean(
            "apps/my-app/worktree.config.toml",
            &config_dir,
            vec!["node_modules".to_string(), ".turbo".to_string()],
        );

        let worktrees: Vec<worktree_setup_git::WorktreeInfo> = worktree_paths
            .iter()
            .enumerate()
            .map(|(i, p)| worktree_setup_git::WorktreeInfo {
                path: p.clone(),
                is_main: false,
                branch: Some(format!("branch-{i}")),
                commit: None,
            })
            .collect();
        let configs: Vec<&LoadedConfig> = vec![&config];

        // ─── Sequential reference ───────────────────────────────────
        let sequential: Vec<interactive::WorktreeResolution> = worktrees
            .iter()
            .enumerate()
            .map(|(idx, wt)| {
                let target_canonical = wt.path.canonicalize().unwrap();
                let resolved =
                    resolve_clean_paths(&configs, &wt.path, &target_canonical, repo_root);
                let items: Vec<output::CleanItem> = resolved
                    .iter()
                    .map(|(abs_path, rel_path)| {
                        let is_dir = abs_path.is_dir();
                        let is_empty_dir = is_dir && is_dir_empty(abs_path);
                        let size = worktree_setup_copy::disk_usage(abs_path);
                        output::CleanItem {
                            relative_path: rel_path.clone(),
                            is_dir,
                            is_empty_dir,
                            size,
                        }
                    })
                    .collect();
                let stats = output::clean_stats(&items);
                interactive::WorktreeResolution {
                    index: idx,
                    resolved,
                    items,
                    stats,
                }
            })
            .collect();

        // ─── Pool-based path ───────────────────────────────────────
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let mut pool_results: Vec<interactive::WorktreeResolution> = worktrees
            .iter()
            .enumerate()
            .map(|(idx, wt)| resolve_single_worktree_with_pool(idx, wt, &configs, repo_root, &pool))
            .collect();
        pool_results.sort_by_key(|r| r.index);

        // ─── Compare ───────────────────────────────────────────────
        assert_eq!(
            sequential.len(),
            pool_results.len(),
            "sequential and pool-based paths must produce the same \
             number of resolutions"
        );
        for (seq, pool_res) in sequential.iter().zip(pool_results.iter()) {
            assert_eq!(
                seq.index, pool_res.index,
                "indices must match: seq={} pool={}",
                seq.index, pool_res.index
            );
            assert_eq!(
                seq.stats, pool_res.stats,
                "summaries must match for idx {}: seq={:?} pool={:?}",
                seq.index, seq.stats, pool_res.stats,
            );
            assert_eq!(
                seq.resolved.len(),
                pool_res.resolved.len(),
                "resolved path count must match for idx {}",
                seq.index
            );
            // The pool path may sort items differently if `par_iter` is
            // reordering internally; compare as sets of item display fields.
            let seq_items: std::collections::BTreeSet<_> = seq
                .items
                .iter()
                .map(|i| (i.relative_path.clone(), i.is_dir, i.is_empty_dir, i.size))
                .collect();
            let pool_items: std::collections::BTreeSet<_> = pool_res
                .items
                .iter()
                .map(|i| (i.relative_path.clone(), i.is_dir, i.is_empty_dir, i.size))
                .collect();
            assert_eq!(
                seq_items, pool_items,
                "items must match for idx {}",
                seq.index
            );
        }
    }

    // ─── find_containing_linked_worktree ────────────────────────────────

    fn make_worktree_info(path: &Path, is_main: bool, branch: Option<&str>) -> WorktreeInfo {
        WorktreeInfo {
            path: path.to_path_buf(),
            is_main,
            branch: branch.map(String::from),
            commit: None,
        }
    }

    #[test]
    fn test_find_containing_linked_worktree_finds_match() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main");
        let linked_path = dir.path().join("linked");
        std::fs::create_dir_all(&main_path).unwrap();
        std::fs::create_dir_all(linked_path.join("subdir")).unwrap();

        let worktrees = vec![
            make_worktree_info(&main_path, true, Some("master")),
            make_worktree_info(&linked_path, false, Some("feature")),
        ];

        // CWD inside linked worktree
        let cwd = linked_path.canonicalize().unwrap().join("subdir");
        let result = find_containing_linked_worktree(&cwd, &worktrees);
        assert!(result.is_some());
        assert_eq!(result.unwrap().branch.as_deref(), Some("feature"));
    }

    #[test]
    fn test_find_containing_linked_worktree_ignores_main() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main");
        std::fs::create_dir_all(&main_path).unwrap();

        let worktrees = vec![make_worktree_info(&main_path, true, Some("master"))];

        // CWD inside main worktree — should return None
        let cwd = main_path.canonicalize().unwrap();
        let result = find_containing_linked_worktree(&cwd, &worktrees);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_containing_linked_worktree_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main");
        let linked_path = dir.path().join("linked");
        let other_path = dir.path().join("other");
        std::fs::create_dir_all(&main_path).unwrap();
        std::fs::create_dir_all(&linked_path).unwrap();
        std::fs::create_dir_all(&other_path).unwrap();

        let worktrees = vec![
            make_worktree_info(&main_path, true, Some("master")),
            make_worktree_info(&linked_path, false, Some("feature")),
        ];

        // CWD in a directory that isn't any worktree
        let cwd = other_path.canonicalize().unwrap();
        let result = find_containing_linked_worktree(&cwd, &worktrees);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_containing_linked_worktree_exact_root() {
        let dir = tempfile::tempdir().unwrap();
        let linked_path = dir.path().join("linked");
        std::fs::create_dir_all(&linked_path).unwrap();

        let worktrees = vec![make_worktree_info(&linked_path, false, Some("feature"))];

        // CWD is the worktree root itself (not a subdirectory)
        let cwd = linked_path.canonicalize().unwrap();
        let result = find_containing_linked_worktree(&cwd, &worktrees);
        assert!(result.is_some());
    }

    // ─── worktree_has_changes ───────────────────────────────────────────

    fn create_test_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::fs::write(dir.join("README.md"), "# Test").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn test_worktree_has_changes_clean_repo() {
        let dir = tempfile::tempdir().unwrap();
        create_test_repo(dir.path());

        assert!(
            !worktree_has_changes(dir.path()),
            "clean repo should have no changes"
        );
    }

    #[test]
    fn test_worktree_has_changes_dirty_repo() {
        let dir = tempfile::tempdir().unwrap();
        create_test_repo(dir.path());

        // Create an untracked file
        std::fs::write(dir.path().join("new-file.txt"), "content").unwrap();

        assert!(
            worktree_has_changes(dir.path()),
            "repo with untracked file should have changes"
        );
    }

    #[test]
    fn test_worktree_has_changes_unstaged_modification() {
        let dir = tempfile::tempdir().unwrap();
        create_test_repo(dir.path());

        // Modify a tracked file without staging
        std::fs::write(dir.path().join("README.md"), "# Modified").unwrap();

        assert!(
            worktree_has_changes(dir.path()),
            "repo with unstaged modification should have changes"
        );
    }

    #[test]
    fn test_worktree_has_changes_nonexistent_path() {
        assert!(
            !worktree_has_changes(Path::new("/nonexistent/path")),
            "nonexistent path should return false"
        );
    }

    // ─── handle_branch_deletion policy ──────────────────────────────────

    #[test]
    fn test_handle_branch_deletion_never_policy() {
        let dir = tempfile::tempdir().unwrap();
        create_test_repo(dir.path());
        let repo = worktree_setup_git::open_repo(dir.path()).unwrap();

        // Create a branch to test with
        Command::new("git")
            .args(["branch", "test-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Never policy should skip deletion entirely
        let result = handle_branch_deletion(
            &repo,
            "test-branch",
            BranchDeletePolicy::Never,
            false,
            false,
            false,
        )
        .unwrap();
        assert!(result.is_none(), "Never policy should return None");

        // Verify branch still exists
        let output = Command::new("git")
            .args(["branch", "--list", "test-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("test-branch"),
            "branch should still exist after Never policy"
        );
    }

    #[test]
    fn test_handle_branch_deletion_always_policy() {
        let dir = tempfile::tempdir().unwrap();
        create_test_repo(dir.path());
        let repo = worktree_setup_git::open_repo(dir.path()).unwrap();

        // Create a branch (merged, so -d works)
        Command::new("git")
            .args(["branch", "auto-delete-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Always policy should delete without asking
        let result = handle_branch_deletion(
            &repo,
            "auto-delete-branch",
            BranchDeletePolicy::Always,
            true, // non_interactive — shouldn't matter for Always
            false,
            false,
        )
        .unwrap();
        assert_eq!(result, Some("auto-delete-branch".to_string()));

        // Verify branch is gone
        let output = Command::new("git")
            .args(["branch", "--list", "auto-delete-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("auto-delete-branch"),
            "branch should be deleted"
        );
    }

    #[test]
    fn test_handle_branch_deletion_ask_non_interactive_skips() {
        let dir = tempfile::tempdir().unwrap();
        create_test_repo(dir.path());
        let repo = worktree_setup_git::open_repo(dir.path()).unwrap();

        Command::new("git")
            .args(["branch", "ask-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Ask policy in non-interactive mode should skip
        let result = handle_branch_deletion(
            &repo,
            "ask-branch",
            BranchDeletePolicy::Ask,
            true, // non_interactive
            false,
            false,
        )
        .unwrap();
        assert!(
            result.is_none(),
            "Ask policy in non-interactive mode should skip"
        );

        // Branch should still exist
        let output = Command::new("git")
            .args(["branch", "--list", "ask-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("ask-branch"), "branch should still exist");
    }

    #[test]
    fn test_handle_branch_deletion_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        create_test_repo(dir.path());
        let repo = worktree_setup_git::open_repo(dir.path()).unwrap();

        Command::new("git")
            .args(["branch", "dry-run-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Always policy with dry_run should report but not delete
        let result = handle_branch_deletion(
            &repo,
            "dry-run-branch",
            BranchDeletePolicy::Always,
            false,
            false,
            true, // dry_run
        )
        .unwrap();
        assert_eq!(result, Some("dry-run-branch".to_string()));

        // Branch should still exist
        let output = Command::new("git")
            .args(["branch", "--list", "dry-run-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("dry-run-branch"),
            "branch should still exist after dry run"
        );
    }

    #[test]
    fn test_handle_branch_deletion_force_deletes_unmerged() {
        let dir = tempfile::tempdir().unwrap();
        create_test_repo(dir.path());

        // Create a worktree with an unmerged branch, add a commit, remove worktree
        let wt_path = dir.path().join("unmerged-wt");
        Command::new("git")
            .args(["worktree", "add", "-b", "unmerged-force-branch"])
            .arg(&wt_path)
            .current_dir(dir.path())
            .output()
            .unwrap();

        std::fs::write(wt_path.join("extra.txt"), "extra").unwrap();
        Command::new("git")
            .args(["add", "extra.txt"])
            .current_dir(&wt_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "unmerged work"])
            .current_dir(&wt_path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&wt_path)
            .current_dir(dir.path())
            .output()
            .unwrap();

        let repo = worktree_setup_git::open_repo(dir.path()).unwrap();

        // Always + force should force-delete the unmerged branch
        let result = handle_branch_deletion(
            &repo,
            "unmerged-force-branch",
            BranchDeletePolicy::Always,
            false,
            true, // force
            false,
        )
        .unwrap();
        assert_eq!(result, Some("unmerged-force-branch".to_string()));

        // Branch should be gone
        let output = Command::new("git")
            .args(["branch", "--list", "unmerged-force-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("unmerged-force-branch"),
            "unmerged branch should be force-deleted"
        );
    }

    #[test]
    fn test_handle_branch_deletion_unmerged_non_interactive_skips() {
        let dir = tempfile::tempdir().unwrap();
        create_test_repo(dir.path());

        // Create a worktree with an unmerged branch, add a commit, remove worktree
        let wt_path = dir.path().join("unmerged-wt2");
        Command::new("git")
            .args(["worktree", "add", "-b", "unmerged-skip-branch"])
            .arg(&wt_path)
            .current_dir(dir.path())
            .output()
            .unwrap();

        std::fs::write(wt_path.join("extra.txt"), "extra").unwrap();
        Command::new("git")
            .args(["add", "extra.txt"])
            .current_dir(&wt_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "unmerged work"])
            .current_dir(&wt_path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&wt_path)
            .current_dir(dir.path())
            .output()
            .unwrap();

        let repo = worktree_setup_git::open_repo(dir.path()).unwrap();

        // Always policy, non-interactive, no force — should warn and skip
        let result = handle_branch_deletion(
            &repo,
            "unmerged-skip-branch",
            BranchDeletePolicy::Always,
            true, // non_interactive
            false,
            false,
        )
        .unwrap();
        assert!(
            result.is_none(),
            "unmerged branch without --force in non-interactive mode should be skipped"
        );

        // Branch should still exist
        let output = Command::new("git")
            .args(["branch", "--list", "unmerged-skip-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("unmerged-skip-branch"),
            "branch should still exist when skipped"
        );
    }

    #[test]
    fn test_is_branch_exists_error_matches() {
        let err = GitError::WorktreeCreateError {
            path: PathBuf::from("/tmp/test"),
            source: git2::Error::from_str(
                "Preparing worktree (new branch 'my-feature')\n\
                 fatal: a branch named 'my-feature' already exists",
            ),
        };
        let result = is_branch_exists_error(&err);
        assert_eq!(result, Some("my-feature".to_string()));
    }

    #[test]
    fn test_is_branch_exists_error_no_match() {
        let err = GitError::WorktreeCreateError {
            path: PathBuf::from("/tmp/test"),
            source: git2::Error::from_str("fatal: is a missing but already registered worktree"),
        };
        assert!(is_branch_exists_error(&err).is_none());

        // Non-WorktreeCreateError variant
        let err2 = GitError::WorktreeListError(git2::Error::from_str("something"));
        assert!(is_branch_exists_error(&err2).is_none());
    }

    #[test]
    fn test_create_worktree_branch_exists_non_interactive_fails() {
        let dir = tempfile::tempdir().unwrap();
        create_test_repo(dir.path());

        // Create a branch that will conflict
        Command::new("git")
            .args(["branch", "existing-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let repo = worktree_setup_git::open_repo(dir.path()).unwrap();
        let wt_path = dir.path().join("new-wt");
        let options = WorktreeCreateOptions {
            branch: Some("master".to_string()),
            new_branch: Some("existing-branch".to_string()),
            detach: false,
            force: false,
        };

        // Non-interactive should return an error, not prompt
        let result = create_worktree_with_recovery(&repo, &wt_path, &options, true);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("already exists"),
            "error should mention branch already exists, got: {err_msg}"
        );
    }
}
