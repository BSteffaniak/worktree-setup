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
use path_clean::PathClean;

use args::{Args, SetupArgs};
use progress::ProgressManager;
use worktree_setup_config::{
    LoadedConfig, ProfilesFile, ResolvedProfile, discover_configs, discover_profiles_file,
    load_config, load_profiles_file, resolve_profiles,
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

/// Load the profiles file from the repo root (if it exists).
///
/// Returns `None` if no profiles file is found. Prints a warning if
/// the file exists but fails to load.
fn load_profiles(repo_root: &Path) -> Option<ProfilesFile> {
    let path = discover_profiles_file(repo_root)?;
    match load_profiles_file(&path) {
        Ok(profiles) => {
            log::debug!("Loaded {} profiles", profiles.profiles.len());
            Some(profiles)
        }
        Err(e) => {
            output::print_warning(&format!("Failed to load profiles file: {e}"));
            None
        }
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
    profiles_file: Option<&ProfilesFile>,
    all_configs: &[LoadedConfig],
) -> Result<ResolvedProfile, Box<dyn std::error::Error>> {
    let resolved = resolve_profiles(profile_names, profiles_file, all_configs)?;

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

/// Resolve whether post-setup should run based on CLI flag and profile default.
///
/// Priority: CLI `--no-install` flag > profile `skip_post_setup` > default (true).
const fn should_run_post_setup(no_install: bool, profile: Option<&ResolvedProfile>) -> bool {
    if no_install {
        return false;
    }
    if let Some(p) = profile
        && let Some(skip) = p.defaults.skip_post_setup
    {
        return !skip;
    }
    true
}

/// Resolve whether overwrite should be enabled based on CLI flag and profile default.
fn should_overwrite(overwrite_flag: bool, profile: Option<&ResolvedProfile>) -> bool {
    overwrite_flag
        || profile
            .and_then(|p| p.defaults.overwrite_existing)
            .unwrap_or(false)
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
    if args.non_interactive {
        let run_files = is_secondary_worktree && !args.no_files;
        let run_post_setup = should_run_post_setup(args.no_install, resolved_profile);
        let overwrite = should_overwrite(args.overwrite, resolved_profile);
        return Ok((run_files, overwrite, run_post_setup));
    }

    // Interactive: show checklist (profile defaults affect initial checkbox states)
    let default_files = is_secondary_worktree && !args.no_files;
    let default_overwrite = should_overwrite(args.overwrite, resolved_profile);
    let default_post_setup = should_run_post_setup(args.no_install, resolved_profile);

    let choices = interactive::prompt_setup_operations(
        &interactive::SetupOperationDefaults {
            is_secondary_worktree,
            files: default_files,
            overwrite: default_overwrite,
            post_setup: default_post_setup,
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

    // Load profiles and resolve if --profile was provided
    let profiles_file = load_profiles(&repo_root);
    let resolved_profile = if args.profile.is_empty() {
        None
    } else {
        Some(resolve_and_print_profile(
            &args.profile,
            profiles_file.as_ref(),
            &all_configs,
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
        run_post_setup_commands(&unique_commands, &target_path)?;
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

// ─── Default flow (create + setup) ─────────────────────────────────────────

/// Handle worktree creation (both interactive and non-interactive).
fn handle_worktree_creation(
    args: &Args,
    repo: &worktree_setup_git::Repository,
    target_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let options = if args.non_interactive {
        // Handle --remote-branch: fetch first, then set branch to the remote ref
        let branch = if let Some(ref remote_branch) = args.remote_branch {
            let remote = resolve_remote_non_interactive(repo, args.remote.as_deref())?;
            println!("Fetching from {remote}...");
            fetch_remote(repo, &remote)?;
            Some(remote_branch.clone())
        } else {
            args.branch.clone()
        };

        println!("Creating worktree at {}...", target_path.display());
        WorktreeCreateOptions {
            branch,
            new_branch: args.new_branch.clone(),
            force: args.force,
            ..Default::default()
        }
    } else {
        // Interactive creation
        let current_branch = get_current_branch(repo)?;
        let branches = get_local_branches(repo)?;
        let default_branch = get_default_branch(repo);
        let recent_branches = get_recent_branches(repo, 5);
        match interactive::prompt_worktree_create(
            repo,
            target_path,
            current_branch.as_deref(),
            &branches,
            default_branch.as_deref(),
            &recent_branches,
            args.remote.as_deref(),
        )? {
            Some(options) => {
                println!("\nCreating worktree at {}...", target_path.display());
                options
            }
            None => return Ok(()),
        }
    };

    create_worktree_with_recovery(repo, target_path, &options, args.non_interactive)
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

    // Load profiles file (if it exists)
    let profiles_file = load_profiles(&repo_root);

    // If --list, print profiles too and exit
    if args.list {
        if let Some(ref pf) = profiles_file {
            let profile_display: Vec<(String, String, usize)> = pf
                .profiles
                .iter()
                .map(|(name, def)| (name.clone(), def.description.clone(), def.configs.len()))
                .collect();
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
            profiles_file.as_ref(),
            &all_configs,
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
        handle_worktree_creation(args, &repo, &target_path)?;
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

    // Determine if post-setup should run: CLI flag > profile default > prompt
    let should_run_install = should_run_post_setup(args.no_install, resolved_profile);

    // Run post-setup commands
    if !unique_commands.is_empty() && should_run_install {
        let should_run = if args.non_interactive {
            true
        } else {
            interactive::prompt_run_install(true)?
        };

        if should_run {
            run_post_setup_commands(&unique_commands, target_path)?;
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
