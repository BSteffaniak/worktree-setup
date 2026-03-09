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
use worktree_setup_config::{LoadedConfig, discover_configs, load_config};
use worktree_setup_git::{
    WorktreeCreateOptions, create_worktree, discover_repo, get_current_branch, get_default_branch,
    get_local_branches, get_main_worktree, get_recent_branches, get_repo_root,
    get_unstaged_and_untracked_files,
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

/// Run the `setup` subcommand.
///
/// Applies worktree configs to an existing directory. On a secondary worktree,
/// this runs file operations and post-setup commands. On the main worktree or
/// a regular clone, only post-setup commands are run.
fn run_setup(args: &SetupArgs) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;

    // Resolve target path (default to CWD)
    let target_path = if let Some(ref path) = args.target_path {
        if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(path)
        }
        .clean()
    } else {
        cwd
    };

    // Verify target exists
    if !target_path.exists() {
        output::print_error(&format!(
            "Target path does not exist: {}",
            target_path.display()
        ));
        std::process::exit(1);
    }

    // Discover repository from the target directory
    let repo = discover_repo(&target_path)?;
    let repo_root = get_repo_root(&repo)?;

    output::print_header("Worktree Setup");
    output::print_repo_info(&repo_root.to_string_lossy());
    println!();

    // Determine if the target is a secondary worktree
    let main_worktree = get_main_worktree(&repo)?;
    let is_secondary_worktree = {
        let target_canonical = target_path
            .canonicalize()
            .unwrap_or_else(|_| target_path.clone());
        let main_canonical = main_worktree
            .path
            .canonicalize()
            .unwrap_or_else(|_| main_worktree.path.clone());
        target_canonical != main_canonical
    };

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

    // Select configs
    let selected_indices = select_configs(&all_configs, &args.configs, args.non_interactive)?;

    if selected_indices.is_empty() {
        println!("No configs selected. Exiting.");
        return Ok(());
    }

    let selected_configs: Vec<&LoadedConfig> =
        selected_indices.iter().map(|&i| &all_configs[i]).collect();

    // Collect post-setup commands for display in the checklist
    let unique_commands = collect_post_setup_commands(&selected_configs);

    // Determine what to run
    let (run_files, overwrite_existing, run_post_setup) = if args.non_interactive {
        // Non-interactive: use defaults as modified by flags
        let run_files = is_secondary_worktree && !args.no_files;
        let run_post_setup = !args.no_install;
        let overwrite = args.overwrite;
        (run_files, overwrite, run_post_setup)
    } else {
        // Interactive: show checklist
        let default_files = is_secondary_worktree && !args.no_files;
        let default_overwrite = args.overwrite;
        let default_post_setup = !args.no_install;

        let choices = interactive::prompt_setup_operations(
            &interactive::SetupOperationDefaults {
                is_secondary_worktree,
                files: default_files,
                overwrite: default_overwrite,
                post_setup: default_post_setup,
            },
            &unique_commands,
        )?;

        (
            choices.run_files,
            choices.overwrite_existing,
            choices.run_post_setup,
        )
    };

    // Nothing selected
    if !run_files && !run_post_setup {
        println!("Nothing selected. Exiting.");
        return Ok(());
    }

    // Execute file operations
    if run_files {
        println!("\nApplying file operations to: {}", target_path.display());
        println!("Source (main worktree): {}\n", main_worktree.path.display());

        execute_file_operations(
            &selected_configs,
            &main_worktree.path,
            &target_path,
            args.copy_unstaged_override(),
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

// ─── Default flow (create + setup) ─────────────────────────────────────────

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

    // If --list, exit (after printing any discovered configs above)
    if args.list {
        return Ok(());
    }

    // Select configs (only if we have valid configs to select from)
    let selected_configs: Vec<&LoadedConfig> = if all_configs.is_empty() {
        Vec::new()
    } else {
        let selected_indices = select_configs(&all_configs, &args.configs, args.non_interactive)?;

        if selected_indices.is_empty() {
            println!("No configs selected. Exiting.");
            return Ok(());
        }

        selected_indices.iter().map(|&i| &all_configs[i]).collect()
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
        if args.non_interactive {
            // Create with provided options
            // Default behavior: let git create an auto-named branch (don't set detach: true)
            println!("Creating worktree at {}...", target_path.display());
            let options = WorktreeCreateOptions {
                branch: args.branch.clone(),
                new_branch: args.new_branch.clone(),
                detach: false, // Don't default to detached HEAD - let git create auto-named branch
            };
            create_worktree(&repo, &target_path, &options)?;
        } else {
            // Interactive creation
            let current_branch = get_current_branch(&repo)?;
            let branches = get_local_branches(&repo)?;
            let default_branch = get_default_branch(&repo);
            let recent_branches = get_recent_branches(&repo, 5);
            if let Some(options) = interactive::prompt_worktree_create(
                &target_path,
                current_branch.as_deref(),
                &branches,
                default_branch.as_deref(),
                &recent_branches,
            )? {
                println!("\nCreating worktree at {}...", target_path.display());
                create_worktree(&repo, &target_path, &options)?;
            }
        }
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
        println!("\nSetting up worktree: {}", target_path.display());
        println!("Main worktree: {}\n", main_worktree.path.display());

        execute_file_operations(
            &selected_configs,
            &main_worktree.path,
            &target_path,
            args.copy_unstaged_override(),
            false, // No overwrite in create flow (fresh worktree)
            args.should_show_progress(),
        )?;

        println!();

        // Collect all post-setup commands
        let unique_commands = collect_post_setup_commands(&selected_configs);

        // Run post-setup commands
        if !unique_commands.is_empty() && args.should_run_install() {
            let should_run = if args.non_interactive {
                true
            } else {
                interactive::prompt_run_install(true)?
            };

            if should_run {
                run_post_setup_commands(&unique_commands, &target_path)?;
            }
        }
    }

    output::print_success();
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
