//! Interactive prompts using dialoguer.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::io;
use std::path::{Path, PathBuf};

use dialoguer::{Confirm, Input, MultiSelect, Select};
use worktree_setup_config::LoadedConfig;
use worktree_setup_git::{
    Repository, WorktreeCreateOptions, fetch_remote, get_remote_branches, get_remotes,
};

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
/// * `recent_branches` - Recently checked-out branches from reflog
///
/// # Returns
///
/// `None` for current HEAD, `Some(branch)` for a specific branch/ref
///
/// # Errors
///
/// * If the user cancels the prompts
fn prompt_base_branch(
    default_branch: Option<&str>,
    recent_branches: &[String],
) -> io::Result<Option<String>> {
    use std::collections::BTreeSet;

    let mut options = vec!["Current HEAD".to_string()];
    let mut seen = BTreeSet::new();

    // Add default branch first
    if let Some(branch) = default_branch {
        options.push(branch.to_string());
        seen.insert(branch.to_string());
    }

    // Add recent branches (excluding duplicates)
    for branch in recent_branches {
        if !seen.contains(branch) {
            options.push(branch.clone());
            seen.insert(branch.clone());
        }
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
        // Selected a branch from the list
        Ok(Some(options[choice].clone()))
    }
}

/// Resolve which remote to use.
///
/// If `override_name` is provided, uses that directly. Otherwise auto-detects:
/// * Single remote: uses it automatically
/// * Multiple remotes: prompts the user to pick one
/// * No remotes: returns an error
///
/// # Errors
///
/// * If listing remotes fails
/// * If the user cancels the prompt
/// * If the repository has no remotes configured
fn resolve_remote(repo: &Repository, override_name: Option<&str>) -> io::Result<String> {
    if let Some(name) = override_name {
        return Ok(name.to_string());
    }

    let remotes =
        get_remotes(repo).map_err(|e| io::Error::other(format!("Failed to list remotes: {e}")))?;

    match remotes.len() {
        0 => Err(io::Error::other("No remotes configured in this repository")),
        1 => Ok(remotes.into_iter().next().unwrap_or_default()),
        _ => {
            let idx = Select::new()
                .with_prompt("Select remote")
                .items(&remotes)
                .default(0)
                .interact()?;
            Ok(remotes[idx].clone())
        }
    }
}

/// Prompt for tracking a remote branch.
///
/// Resolves the remote (auto-detect or prompt), optionally fetches, then
/// presents a picker of remote branches. Falls back to default options
/// if no remote branches are found.
///
/// # Arguments
///
/// * `repo` - The repository (needed for fetching and listing remote branches)
/// * `remote_override` - If set, use this remote name instead of auto-detecting
///
/// # Errors
///
/// * If the user cancels the prompts
/// * If fetching or listing remote branches fails
fn prompt_remote_branch(
    repo: &Repository,
    remote_override: Option<&str>,
) -> io::Result<WorktreeCreateOptions> {
    let remote = resolve_remote(repo, remote_override)?;

    let should_fetch = Confirm::new()
        .with_prompt(format!("Fetch latest from {remote}?"))
        .default(true)
        .interact()?;

    if should_fetch {
        println!("Fetching from {remote}...");
        fetch_remote(repo, &remote)
            .map_err(|e| io::Error::other(format!("Failed to fetch: {e}")))?;
    }

    let remote_branches = get_remote_branches(repo, &remote)
        .map_err(|e| io::Error::other(format!("Failed to list remote branches: {e}")))?;

    if remote_branches.is_empty() {
        println!("No remote branches found. Using auto-named branch instead.");
        return Ok(WorktreeCreateOptions::default());
    }

    let branch_idx = Select::new()
        .with_prompt("Select remote branch")
        .items(&remote_branches)
        .interact()?;

    // Strip the known remote prefix (e.g., "origin/feature/auth/login" -> "feature/auth/login").
    // We use strip_prefix with the exact remote name rather than splitting on '/'
    // to correctly handle branch names that contain slashes.
    let selected = &remote_branches[branch_idx];
    let remote_prefix = format!("{remote}/");
    let local_name = selected
        .strip_prefix(&remote_prefix)
        .unwrap_or(selected.as_str());

    Ok(WorktreeCreateOptions {
        branch: Some(local_name.to_string()),
        ..Default::default()
    })
}

/// Prompt for worktree creation options.
///
/// Returns `None` if the user doesn't want to create a worktree.
///
/// # Arguments
///
/// * `repo` - The repository (needed for fetching remote branches)
/// * `target_path` - The path where the worktree will be created
/// * `current_branch` - The current branch name, if on a branch (None if detached HEAD)
/// * `branches` - List of available local branches
/// * `default_branch` - The detected default branch (e.g., "main" or "master")
/// * `recent_branches` - Recently checked-out branches from reflog
/// * `remote_override` - If set, use this remote name instead of auto-detecting
///
/// # Errors
///
/// * If the user cancels the prompts
/// * If fetching remote branches fails
pub fn prompt_worktree_create(
    repo: &Repository,
    target_path: &Path,
    current_branch: Option<&str>,
    branches: &[String],
    default_branch: Option<&str>,
    recent_branches: &[String],
    remote_override: Option<&str>,
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

    // Option 4: Track remote branch
    options.push("Track remote branch...".to_string());
    option_values.push("remote");

    // Option 5: Detached HEAD (advanced)
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
            let base_branch = prompt_base_branch(default_branch, recent_branches)?;

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

            let base_branch = prompt_base_branch(default_branch, recent_branches)?;

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
        "remote" => prompt_remote_branch(repo, remote_override)?,
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

/// Result of the setup operations prompt.
#[derive(Debug, Clone)]
pub struct SetupOperationChoices {
    /// Whether to run file operations (symlinks, copies, templates).
    pub run_files: bool,
    /// Whether to overwrite existing files during file operations.
    pub overwrite_existing: bool,
    /// Whether to run post-setup commands.
    pub run_post_setup: bool,
}

/// Default values for setup operation checkboxes.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct SetupOperationDefaults {
    /// Whether the target is a secondary worktree (shows file ops when `true`).
    pub is_secondary_worktree: bool,
    /// Default checked state for file operations.
    pub files: bool,
    /// Default checked state for overwrite existing files.
    pub overwrite: bool,
    /// Default checked state for post-setup commands.
    pub post_setup: bool,
}

/// Prompt the user to select which setup operations to run.
///
/// Shows an interactive checklist with file operations, overwrite toggle,
/// and post-setup commands. Items are pre-checked based on defaults
/// (which can be influenced by CLI flags).
///
/// # Arguments
///
/// * `defaults` - Default checked states for each operation
/// * `post_setup_commands` - List of post-setup commands (shown inline for context)
///
/// # Errors
///
/// * If the user cancels the prompt
pub fn prompt_setup_operations(
    defaults: &SetupOperationDefaults,
    post_setup_commands: &[&str],
) -> io::Result<SetupOperationChoices> {
    let mut items: Vec<String> = Vec::new();
    let mut checked: Vec<bool> = Vec::new();

    // Track which index maps to which operation
    let mut file_ops_index: Option<usize> = None;
    let mut overwrite_index: Option<usize> = None;
    let mut post_setup_index: Option<usize> = None;

    if defaults.is_secondary_worktree {
        file_ops_index = Some(items.len());
        items.push("Apply file operations (symlinks, copies, templates)".to_string());
        checked.push(defaults.files);

        overwrite_index = Some(items.len());
        items.push("Overwrite existing files".to_string());
        checked.push(defaults.overwrite);
    }

    if !post_setup_commands.is_empty() {
        post_setup_index = Some(items.len());
        let cmds_display = post_setup_commands.join(", ");
        items.push(format!("Run post-setup commands ({cmds_display})"));
        checked.push(defaults.post_setup);
    }

    if items.is_empty() {
        // Nothing to prompt for
        return Ok(SetupOperationChoices {
            run_files: false,
            overwrite_existing: false,
            run_post_setup: false,
        });
    }

    let selections = MultiSelect::new()
        .with_prompt("Select what to run")
        .items(&items)
        .defaults(&checked)
        .interact()?;

    let run_files = file_ops_index.is_some_and(|i| selections.contains(&i));

    Ok(SetupOperationChoices {
        run_files,
        // Overwrite only matters if file operations are selected
        overwrite_existing: run_files && overwrite_index.is_some_and(|i| selections.contains(&i)),
        run_post_setup: post_setup_index.is_some_and(|i| selections.contains(&i)),
    })
}
