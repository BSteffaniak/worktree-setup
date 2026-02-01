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

/// Prompt for which branch to base a new branch off.
///
/// # Arguments
///
/// * `default_branch` - The detected default branch (e.g., "main" or "master")
///
/// # Returns
///
/// `None` for current HEAD, `Some(branch)` for a specific branch/ref
///
/// # Errors
///
/// * If the user cancels the prompts
fn prompt_base_branch(default_branch: Option<&str>) -> io::Result<Option<String>> {
    let mut options = vec!["Current HEAD".to_string()];

    if let Some(branch) = default_branch {
        options.push(branch.to_string());
    }

    options.push("Enter custom branch/ref...".to_string());

    let choice = Select::new()
        .with_prompt("Base the new branch off")
        .items(&options)
        .default(0)
        .interact()?;

    let last_idx = options.len() - 1;

    if choice == 0 {
        Ok(None) // Current HEAD
    } else if choice == last_idx {
        // Custom input
        let custom: String = Input::new()
            .with_prompt("Enter branch name or ref")
            .interact_text()?;
        Ok(Some(custom))
    } else {
        // Selected the default branch
        Ok(Some(options[choice].clone()))
    }
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
/// * `default_branch` - The detected default branch (e.g., "main" or "master")
///
/// # Errors
///
/// * If the user cancels the prompts
pub fn prompt_worktree_create(
    target_path: &PathBuf,
    current_branch: Option<&str>,
    branches: &[String],
    default_branch: Option<&str>,
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
    options.push(format!("New branch (auto-named '{worktree_name}')"));
    option_values.push("auto");

    // Option 1: Custom-named branch
    options.push("New branch (custom name)...".to_string());
    option_values.push("new");

    // Option 2: Use current branch (only if we're on a branch)
    if let Some(branch) = current_branch {
        options.push(format!("Use current branch ({branch})"));
        option_values.push("current");
    }

    // Option 3: Use existing branch
    options.push("Use existing branch...".to_string());
    option_values.push("existing");

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
            // Let git create an auto-named branch, but ask what to base it off
            let base_branch = prompt_base_branch(default_branch)?;

            // For auto-named branch with a custom base, we need to explicitly
            // create the branch with -b, otherwise git just checks out the base branch
            if base_branch.is_some() {
                WorktreeCreateOptions {
                    new_branch: Some(worktree_name.to_string()),
                    branch: base_branch,
                    ..Default::default()
                }
            } else {
                // Current HEAD - let git handle auto-naming
                WorktreeCreateOptions::default()
            }
        }
        "new" => {
            let branch_name: String = Input::new()
                .with_prompt("Enter new branch name")
                .interact_text()?;

            let base_branch = prompt_base_branch(default_branch)?;

            WorktreeCreateOptions {
                new_branch: Some(branch_name),
                branch: base_branch,
                ..Default::default()
            }
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
