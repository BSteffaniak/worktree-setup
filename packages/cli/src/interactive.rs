//! Interactive prompts using dialoguer.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::io;
use std::path::{Path, PathBuf};

use dialoguer::{Confirm, Input, MultiSelect, Select};
use worktree_setup_config::{CreationMethod, LoadedConfig};
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
        .defaults(&vec![true; items.len()])
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
/// * `profile_base` - If set by a profile, this branch is preselected as the default
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
    profile_base: Option<&str>,
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

    // Add the profile base branch if it's not already in the list
    if let Some(base) = profile_base
        && !seen.contains(base)
    {
        options.push(base.to_string());
        seen.insert(base.to_string());
    }

    options.push("Enter custom branch/ref...".to_string());

    // Determine default selection: profile base branch if set, otherwise Current HEAD
    let default_idx = profile_base
        .and_then(|base| options.iter().position(|o| o == base))
        .unwrap_or(0);

    let choice = Select::new()
        .with_prompt("Base the new branch off")
        .items(&options)
        .default(default_idx)
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
/// When `inferred_branch` is provided and a matching remote branch exists,
/// a confirm prompt is shown instead of the full branch picker.
///
/// # Arguments
///
/// * `repo` - The repository (needed for fetching and listing remote branches)
/// * `remote_override` - If set, use this remote name instead of auto-detecting
/// * `inferred_branch` - Branch name inferred from worktree directory (for auto-selection)
///
/// # Errors
///
/// * If the user cancels the prompts
/// * If fetching or listing remote branches fails
fn prompt_remote_branch(
    repo: &Repository,
    remote_override: Option<&str>,
    inferred_branch: Option<&str>,
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

    let remote_prefix = format!("{remote}/");

    // Try to auto-select from inferred branch name
    if let Some(inferred) = inferred_branch {
        let inferred_remote = format!("{remote_prefix}{inferred}");
        if remote_branches.iter().any(|b| b == &inferred_remote) {
            let use_inferred = Confirm::new()
                .with_prompt(format!("Use inferred remote branch '{inferred_remote}'?"))
                .default(true)
                .interact()?;

            if use_inferred {
                return Ok(WorktreeCreateOptions {
                    branch: Some(inferred.to_string()),
                    ..Default::default()
                });
            }
            // User declined — fall through to the full picker
        } else {
            println!("No remote branch matching '{inferred_remote}' found. Showing all branches.");
        }
    }

    let branch_idx = Select::new()
        .with_prompt("Select remote branch")
        .items(&remote_branches)
        .interact()?;

    // Strip the known remote prefix (e.g., "origin/feature/auth/login" -> "feature/auth/login").
    // We use strip_prefix with the exact remote name rather than splitting on '/'
    // to correctly handle branch names that contain slashes.
    let selected = &remote_branches[branch_idx];
    let local_name = selected
        .strip_prefix(&remote_prefix)
        .unwrap_or(selected.as_str());

    Ok(WorktreeCreateOptions {
        branch: Some(local_name.to_string()),
        ..Default::default()
    })
}

/// Profile-derived hints that control worktree creation prompts.
#[derive(Debug, Clone, Default)]
pub struct CreationProfileHints<'a> {
    /// Skip the "Create worktree?" confirmation.
    pub auto_create: bool,
    /// Skip the creation method picker and use this method directly.
    pub creation_method: Option<&'a CreationMethod>,
    /// Preselect this base branch for new branch creation.
    pub base_branch: Option<&'a str>,
    /// When `true` with `creation_method = Auto`, use `base_branch` without prompting.
    pub new_branch: bool,
    /// Override the remote name for remote branch operations.
    pub remote_override: Option<&'a str>,
    /// Branch name inferred from worktree directory (for remote tracking).
    pub inferred_branch: Option<&'a str>,
}

/// Build the creation method options list and determine the default choice.
///
/// Returns `(display_labels, value_keys, default_index)`.
fn build_creation_options(
    worktree_name: &str,
    current_branch: Option<&str>,
    creation_method: Option<&CreationMethod>,
) -> (Vec<String>, Vec<&'static str>, usize) {
    let mut options: Vec<String> = Vec::new();
    let mut option_values: Vec<&str> = Vec::new();

    options.push(format!("New branch (auto-named '{worktree_name}')"));
    option_values.push("auto");

    options.push("New branch (custom name)...".to_string());
    option_values.push("new");

    if let Some(branch) = current_branch {
        options.push(format!("Use current branch ({branch})"));
        option_values.push("current");
    }

    options.push("Use existing branch...".to_string());
    option_values.push("existing");

    options.push("Track remote branch...".to_string());
    option_values.push("remote");

    options.push("Detached HEAD (current commit)".to_string());
    option_values.push("detach");

    let default_key = match creation_method {
        Some(CreationMethod::Remote) => "remote",
        Some(CreationMethod::Current) => "current",
        Some(CreationMethod::Detach) => "detach",
        _ => "auto",
    };

    let default_choice = option_values
        .iter()
        .position(|v| *v == default_key)
        .unwrap_or(0);

    (options, option_values, default_choice)
}

/// Dispatch a creation method directly, without showing the picker.
fn dispatch_creation_method(
    method: &CreationMethod,
    repo: &Repository,
    worktree_name: &str,
    current_branch: Option<&str>,
    hints: &CreationProfileHints<'_>,
) -> io::Result<WorktreeCreateOptions> {
    match method {
        CreationMethod::Auto => {
            let base_branch = hints.base_branch.map(String::from);
            if base_branch.is_some() {
                Ok(WorktreeCreateOptions {
                    new_branch: Some(worktree_name.to_string()),
                    branch: base_branch,
                    ..Default::default()
                })
            } else {
                // Current HEAD — let git handle auto-naming
                Ok(WorktreeCreateOptions::default())
            }
        }
        CreationMethod::Current => Ok(WorktreeCreateOptions {
            branch: current_branch.map(String::from),
            ..Default::default()
        }),
        CreationMethod::Remote => {
            prompt_remote_branch(repo, hints.remote_override, hints.inferred_branch)
        }
        CreationMethod::Detach => Ok(WorktreeCreateOptions {
            detach: true,
            ..Default::default()
        }),
    }
}

/// Prompt for worktree creation options.
///
/// Returns `None` if the user doesn't want to create a worktree.
///
/// Profile hints control which prompts are skipped:
/// * `auto_create` — skip the "Create it?" confirmation
/// * `creation_method` — skip the creation method picker
/// * `base_branch` / `new_branch` — skip the base branch prompt
/// * `inferred_branch` — auto-select a remote branch by name
///
/// # Errors
///
/// * If the user cancels the prompts
/// * If fetching remote branches fails
#[allow(clippy::too_many_arguments)]
pub fn prompt_worktree_create(
    repo: &Repository,
    target_path: &Path,
    current_branch: Option<&str>,
    branches: &[String],
    default_branch: Option<&str>,
    recent_branches: &[String],
    hints: &CreationProfileHints<'_>,
) -> io::Result<Option<WorktreeCreateOptions>> {
    // Step 1: Confirm creation (skip if auto_create)
    if !hints.auto_create {
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
    }

    let worktree_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("worktree");

    // Step 2: If creation_method is fully determined, dispatch directly
    if let Some(method) = hints.creation_method {
        let options = dispatch_creation_method(method, repo, worktree_name, current_branch, hints)?;
        return Ok(Some(options));
    }

    // Step 3: Show creation method picker
    let (options, option_values, default_choice) =
        build_creation_options(worktree_name, current_branch, hints.creation_method);

    let choice = Select::new()
        .with_prompt("How should the worktree be created?")
        .items(&options)
        .default(default_choice)
        .interact()?;

    let selected_value = option_values[choice];

    let result = match selected_value {
        "auto" => {
            let base_branch = if hints.new_branch && hints.base_branch.is_some() {
                hints.base_branch.map(String::from)
            } else {
                prompt_base_branch(default_branch, recent_branches, hints.base_branch)?
            };

            if base_branch.is_some() {
                WorktreeCreateOptions {
                    new_branch: Some(worktree_name.to_string()),
                    branch: base_branch,
                    ..Default::default()
                }
            } else {
                WorktreeCreateOptions::default()
            }
        }
        "new" => {
            let branch_name: String = Input::new()
                .with_prompt("Enter new branch name")
                .interact_text()?;

            let base_branch =
                prompt_base_branch(default_branch, recent_branches, hints.base_branch)?;

            WorktreeCreateOptions {
                new_branch: Some(branch_name),
                branch: base_branch,
                ..Default::default()
            }
        }
        "current" => WorktreeCreateOptions {
            branch: current_branch.map(String::from),
            ..Default::default()
        },
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
        "remote" => prompt_remote_branch(repo, hints.remote_override, hints.inferred_branch)?,
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

/// Recovery action for a stale worktree registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleWorktreeAction {
    /// Run `git worktree prune` and retry.
    Prune,
    /// Retry with `--force`.
    Force,
    /// Cancel and return the original error.
    Cancel,
}

/// Prompt the user for how to handle a stale worktree registration.
///
/// Shown when `git worktree add` fails because the path is already
/// registered but missing from disk.
///
/// # Errors
///
/// * If the user cancels the prompt
#[must_use = "caller must act on the chosen recovery action"]
pub fn prompt_stale_worktree_recovery() -> io::Result<StaleWorktreeAction> {
    let options = [
        "Prune stale worktrees and retry",
        "Force create (overwrite registration)",
        "Cancel",
    ];

    let choice = Select::new()
        .with_prompt("This path is registered as a stale worktree. How would you like to proceed?")
        .items(options)
        .default(0)
        .interact()?;

    Ok(match choice {
        0 => StaleWorktreeAction::Prune,
        1 => StaleWorktreeAction::Force,
        _ => StaleWorktreeAction::Cancel,
    })
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

/// Input for each setup operation: pre-determined by profile or needs prompting.
#[derive(Debug, Clone)]
pub struct SetupOperationInputs {
    /// Whether the target is a secondary worktree (controls file ops visibility).
    pub is_secondary_worktree: bool,
    /// File operations: `Some(value)` = determined, `None` = prompt.
    pub files: Option<bool>,
    /// Overwrite existing: `Some(value)` = determined, `None` = prompt.
    pub overwrite: Option<bool>,
    /// Post-setup commands: `Some(value)` = determined, `None` = prompt.
    pub post_setup: Option<bool>,
}

/// Determine setup operations, prompting only for undetermined values.
///
/// If all values are pre-determined (by profile + CLI flags), no prompt
/// is shown. Otherwise, only undetermined items appear in the checklist.
///
/// # Arguments
///
/// * `inputs` - Which operations are determined vs need prompting
/// * `post_setup_commands` - Post-setup commands (shown inline for context)
///
/// # Errors
///
/// * If the user cancels the prompt
pub fn prompt_setup_operations(
    inputs: &SetupOperationInputs,
    post_setup_commands: &[&str],
) -> io::Result<SetupOperationChoices> {
    // Start with pre-determined values
    let mut result = SetupOperationChoices {
        run_files: inputs.files.unwrap_or(inputs.is_secondary_worktree),
        overwrite_existing: inputs.overwrite.unwrap_or(false),
        run_post_setup: inputs.post_setup.unwrap_or(!post_setup_commands.is_empty()),
    };

    // Build checklist of only undetermined items
    let mut items: Vec<String> = Vec::new();
    let mut checked: Vec<bool> = Vec::new();

    // Track which checklist index maps to which operation
    let mut file_ops_index: Option<usize> = None;
    let mut overwrite_index: Option<usize> = None;
    let mut post_setup_index: Option<usize> = None;

    if inputs.is_secondary_worktree && inputs.files.is_none() {
        file_ops_index = Some(items.len());
        items.push("Apply file operations (symlinks, copies, templates)".to_string());
        checked.push(result.run_files);
    }

    if inputs.is_secondary_worktree && inputs.overwrite.is_none() {
        overwrite_index = Some(items.len());
        items.push("Overwrite existing files".to_string());
        checked.push(result.overwrite_existing);
    }

    if !post_setup_commands.is_empty() && inputs.post_setup.is_none() {
        post_setup_index = Some(items.len());
        let cmds_display = post_setup_commands.join(", ");
        items.push(format!("Run post-setup commands ({cmds_display})"));
        checked.push(result.run_post_setup);
    }

    // If nothing needs prompting, return the pre-determined values
    if items.is_empty() {
        return Ok(result);
    }

    let selections = MultiSelect::new()
        .with_prompt("Select what to run")
        .items(&items)
        .defaults(&checked)
        .interact()?;

    // Update only the prompted items
    if let Some(i) = file_ops_index {
        result.run_files = selections.contains(&i);
    }
    if let Some(i) = overwrite_index {
        result.overwrite_existing = result.run_files && selections.contains(&i);
    }
    if let Some(i) = post_setup_index {
        result.run_post_setup = selections.contains(&i);
    }

    Ok(result)
}
