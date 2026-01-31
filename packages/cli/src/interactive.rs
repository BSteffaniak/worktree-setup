//! Interactive prompts using dialoguer.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::io;
use std::path::PathBuf;

use dialoguer::{Confirm, Input, MultiSelect, Select};
use worktree_setup_config::LoadedConfig;
use worktree_setup_git::WorktreeCreateOptions;

/// Select which configs to apply from a list.
///
/// # Errors
///
/// * If the user cancels the selection
pub fn select_configs(configs: &[LoadedConfig]) -> io::Result<Vec<usize>> {
    if configs.len() == 1 {
        // If there's only one config, auto-select it
        return Ok(vec![0]);
    }

    let items: Vec<String> = configs
        .iter()
        .map(|c| format!("{} - {}", c.relative_path, c.config.description))
        .collect();

    let selections = MultiSelect::new()
        .with_prompt("Select configurations to apply")
        .items(&items)
        .interact()?;

    Ok(selections)
}

/// Prompt for the target worktree path.
///
/// # Errors
///
/// * If the user cancels the input
pub fn prompt_worktree_path() -> io::Result<PathBuf> {
    let path: String = Input::new()
        .with_prompt("Enter the path for the new worktree")
        .interact_text()?;

    Ok(PathBuf::from(path))
}

/// Prompt for worktree creation options.
///
/// Returns `None` if the user doesn't want to create a worktree.
///
/// # Arguments
///
/// * `target_path` - The path where the worktree will be created
/// * `current_branch` - The current branch name, if on a branch (None if detached HEAD)
/// * `branches` - List of available local branches
///
/// # Errors
///
/// * If the user cancels the prompts
pub fn prompt_worktree_create(
    target_path: &PathBuf,
    current_branch: Option<&str>,
    branches: &[String],
) -> io::Result<Option<WorktreeCreateOptions>> {
    let should_create = Confirm::new()
        .with_prompt(format!(
            "Worktree does not exist at {}. Create it?",
            target_path.display()
        ))
        .default(true)
        .interact()?;

    if !should_create {
        return Ok(None);
    }

    // Get the worktree name from the path (for auto-named branch option)
    let worktree_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("worktree");

    // Build the options list dynamically
    let mut options: Vec<String> = Vec::new();
    let mut option_values: Vec<&str> = Vec::new();

    // Option 0: Auto-named branch (git's default behavior) - ALWAYS FIRST/DEFAULT
    options.push(format!(
        "New branch from HEAD (auto-named '{worktree_name}')"
    ));
    option_values.push("auto");

    // Option 1: Use current branch (only if we're on a branch)
    if let Some(branch) = current_branch {
        options.push(format!("Use current branch ({branch})"));
        option_values.push("current");
    }

    // Option 2: Use existing branch
    options.push("Use existing branch...".to_string());
    option_values.push("existing");

    // Option 3: Create new branch with custom name
    options.push("Create new branch (custom name)...".to_string());
    option_values.push("new");

    // Option 4: Detached HEAD (advanced)
    options.push("Detached HEAD (current commit)".to_string());
    option_values.push("detach");

    let choice = Select::new()
        .with_prompt("How should the worktree be created?")
        .items(&options)
        .default(0) // Default to auto-named branch
        .interact()?;

    let selected_value = option_values[choice];

    let result = match selected_value {
        "auto" => {
            // Let git create an auto-named branch (default behavior)
            // Don't set branch, new_branch, or detach - git will create a branch named after the path
            WorktreeCreateOptions::default()
        }
        "current" => {
            // Use the current branch
            WorktreeCreateOptions {
                branch: current_branch.map(String::from),
                ..Default::default()
            }
        }
        "existing" => {
            if branches.is_empty() {
                println!("No local branches found. Using auto-named branch instead.");
                WorktreeCreateOptions::default()
            } else {
                let branch_idx = Select::new()
                    .with_prompt("Select branch")
                    .items(branches)
                    .interact()?;

                WorktreeCreateOptions {
                    branch: Some(branches[branch_idx].clone()),
                    ..Default::default()
                }
            }
        }
        "new" => {
            let branch_name: String = Input::new()
                .with_prompt("Enter new branch name")
                .interact_text()?;

            WorktreeCreateOptions {
                new_branch: Some(branch_name),
                ..Default::default()
            }
        }
        "detach" => WorktreeCreateOptions {
            detach: true,
            ..Default::default()
        },
        _ => unreachable!(),
    };

    Ok(Some(result))
}

/// Prompt whether to run post-setup commands.
///
/// # Errors
///
/// * If the user cancels the prompt
pub fn prompt_run_install(default: bool) -> io::Result<bool> {
    Ok(Confirm::new()
        .with_prompt("Run post-setup commands (e.g., bun install)?")
        .default(default)
        .interact()?)
}
