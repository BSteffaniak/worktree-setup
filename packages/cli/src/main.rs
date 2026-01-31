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
use std::path::PathBuf;
use std::process::Command;

use clap::Parser;

use args::Args;
use progress::ProgressManager;
use worktree_setup_config::{LoadedConfig, discover_configs, load_config};
use worktree_setup_git::{
    WorktreeCreateOptions, create_worktree, discover_repo, get_current_branch, get_local_branches,
    get_main_worktree, get_repo_root,
};
use worktree_setup_operations::{
    ApplyConfigOptions, OperationType, execute_operation, plan_operations,
};

fn main() {
    let args = Args::parse();

    // Set up logging
    if args.verbose {
        // SAFETY: We're setting this before any other threads are spawned
        unsafe {
            env::set_var("RUST_LOG", "debug");
        }
    }
    pretty_env_logger::init();

    if let Err(e) = run(args) {
        output::print_error(&e.to_string());
        std::process::exit(1);
    }
}

/// Main application logic.
fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    // Discover repository
    let cwd = env::current_dir()?;
    let repo = discover_repo(&cwd)?;
    let repo_root = get_repo_root(&repo)?;

    output::print_header("Worktree Setup");
    output::print_repo_info(&repo_root.to_string_lossy());
    println!();

    // Discover configs
    let config_paths = discover_configs(&repo_root)?;

    if config_paths.is_empty() {
        println!("No worktree.config.toml or worktree.config.ts files found.");
        println!("Create a worktree.config.toml file to define your setup configuration.");
        return Ok(());
    }

    // Load all configs
    let mut all_configs: Vec<LoadedConfig> = Vec::new();
    for path in config_paths {
        match load_config(&path, &repo_root) {
            Ok(config) => all_configs.push(config),
            Err(e) => {
                output::print_warning(&format!("Failed to load {}: {}", path.display(), e));
            }
        }
    }

    if all_configs.is_empty() {
        output::print_error("No valid configurations found.");
        return Ok(());
    }

    // Print config list
    let config_display: Vec<(String, String)> = all_configs
        .iter()
        .map(|c| (c.relative_path.clone(), c.config.description.clone()))
        .collect();
    output::print_config_list(&config_display);

    // If --list, exit
    if args.list {
        return Ok(());
    }

    // Select configs
    let selected_indices = if !args.configs.is_empty() {
        // Filter by provided patterns
        all_configs
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                args.configs.iter().any(|p| {
                    c.relative_path.contains(p) || c.config_path.to_string_lossy().contains(p)
                })
            })
            .map(|(i, _)| i)
            .collect()
    } else if args.non_interactive {
        // Use all configs in non-interactive mode
        (0..all_configs.len()).collect()
    } else {
        // Interactive selection
        interactive::select_configs(&all_configs)?
    };

    if selected_indices.is_empty() {
        println!("No configs selected. Exiting.");
        return Ok(());
    }

    let selected_configs: Vec<&LoadedConfig> =
        selected_indices.iter().map(|&i| &all_configs[i]).collect();

    // Get target path
    let target_path = if let Some(ref path) = args.target_path {
        PathBuf::from(path)
    } else if args.non_interactive {
        output::print_error("Target path is required in non-interactive mode.");
        std::process::exit(1);
    } else {
        interactive::prompt_worktree_path()?
    };

    // Make target path absolute
    let target_path = if target_path.is_absolute() {
        target_path
    } else {
        cwd.join(&target_path)
    };

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
            if let Some(options) = interactive::prompt_worktree_create(
                &target_path,
                current_branch.as_deref(),
                &branches,
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

    println!("\nSetting up worktree: {}", target_path.display());
    println!("Main worktree: {}\n", main_worktree.path.display());

    // Create progress manager
    let progress_mgr = ProgressManager::new(args.should_show_progress());

    // Build options
    let options = ApplyConfigOptions {
        copy_unstaged: args.copy_unstaged_override(),
    };

    // Plan all operations across all configs
    let mut all_operations = Vec::new();
    for config in &selected_configs {
        let ops = plan_operations(config, &main_worktree.path, &target_path, &options)?;
        all_operations.extend(ops);
    }

    // Execute operations with progress
    for op in &all_operations {
        if op.will_skip {
            // Print skipped status
            let reason = op.skip_reason.as_deref().unwrap_or("skipped");
            progress_mgr.print_result(&op.display_path, reason, false);
            continue;
        }

        // Determine if this is a directory operation that needs a progress bar
        let needs_progress_bar = op.is_directory && op.file_count > 1;

        if needs_progress_bar {
            // Create and show progress bar for directory operations
            let bar = progress_mgr.create_file_bar(&op.display_path, op.file_count);

            let result = execute_operation(op, |completed, _total| {
                bar.set_position(completed);
            })?;

            // Clear the progress bar
            bar.finish_and_clear();

            // Print the final result with file count
            let result_str = format_result_string(result, op.operation_type);
            progress_mgr.print_result_with_count(&op.display_path, &result_str, op.file_count);
        } else {
            // Single file or symlink - just execute and print result
            let result = execute_operation(op, |_, _| {})?;
            let result_str = format_result_string(result, op.operation_type);
            progress_mgr.print_result(&op.display_path, &result_str, true);
        }
    }

    // Clear any remaining progress bars
    progress_mgr.clear();

    println!();

    // Collect all post-setup commands
    let all_post_setup: Vec<&str> = selected_configs
        .iter()
        .flat_map(|c| c.config.post_setup.iter().map(String::as_str))
        .collect();

    // Deduplicate commands
    let mut unique_commands: Vec<&str> = Vec::new();
    for cmd in all_post_setup {
        if !unique_commands.contains(&cmd) {
            unique_commands.push(cmd);
        }
    }

    // Run post-setup commands
    if !unique_commands.is_empty() && args.should_run_install() {
        let should_run = if args.non_interactive {
            true
        } else {
            interactive::prompt_run_install(true)?
        };

        if should_run {
            println!("Running post-setup commands:");
            for cmd in &unique_commands {
                output::print_command(cmd);

                let mut child = Command::new("sh")
                    .args(["-c", cmd])
                    .current_dir(&target_path)
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
        (OperationResult::Created, OperationType::Copy | OperationType::CopyGlob) => {
            "copied".to_string()
        }
        (OperationResult::Created, OperationType::Template) => "created".to_string(),
        (OperationResult::Created, OperationType::Unstaged) => "copied".to_string(),
        (OperationResult::Created, OperationType::Overwrite) => "copied".to_string(),
        (OperationResult::Overwritten, _) => "overwritten".to_string(),
        (OperationResult::Exists, _) => "exists".to_string(),
        (OperationResult::Skipped, _) => "skipped".to_string(),
    }
}
