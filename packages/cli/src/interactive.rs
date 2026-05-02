//! Interactive prompts using dialoguer.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use colored::Colorize;
use console::{Key, Term};
use dialoguer::{Confirm, Input, MultiSelect, Select};
use worktree_setup_config::{CreationMethod, LoadedConfig};
use worktree_setup_git::{
    Repository, WorktreeCreateOptions, WorktreeInfo, fetch_remote, get_remote_branches, get_remotes,
};

use crate::output;

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

/// Format a worktree as a display label for selection prompts.
///
/// Shows: `branch (path)` with `[main]` suffix for the main worktree,
/// or `detached@commit (path)` for detached HEAD.
#[must_use]
fn format_worktree_label(wt: &WorktreeInfo) -> String {
    let suffix = if wt.is_main { " [main]" } else { "" };

    wt.branch.as_ref().map_or_else(
        || {
            wt.commit.as_ref().map_or_else(
                || format!("({}){suffix}", wt.path.display()),
                |commit| format!("detached@{commit} ({}){suffix}", wt.path.display()),
            )
        },
        |branch| format!("{branch} ({}){suffix}", wt.path.display()),
    )
}

/// Resolved clean data for a single worktree, produced by a background thread.
pub struct WorktreeResolution {
    /// Index of the worktree in the original list.
    pub index: usize,
    /// Resolved absolute paths paired with their display strings.
    pub resolved: Vec<(PathBuf, String)>,
    /// Preview items with type and size info.
    pub items: Vec<output::CleanItem>,
    /// Structured summary data used for size heat rendering.
    pub stats: output::CleanStats,
}

/// Resolved warning for a single worktree, produced by a background thread.
pub struct WarningResolution {
    /// Index of the worktree in the original list.
    pub index: usize,
    /// Warning text, or `None` if the worktree is clean.
    pub warning: Option<String>,
}

/// Tri-state status for a worktree warning check.
#[derive(Clone)]
enum WarningStatus {
    /// Background thread has not yet reported for this worktree.
    Pending,
    /// Check completed — worktree is clean (no warning).
    Clean,
    /// Check completed — worktree has a warning to display.
    Warning(String),
}

/// Result type for [`select_worktrees_for_removal`]:
/// `(selected_indices_or_none, per_worktree_warnings)`.
pub type RemovalPickerResult = (Option<Vec<usize>>, Vec<Option<String>>);

/// Spinner frames (braille dots).
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Tick interval for the spinner animation (milliseconds).
const SPINNER_TICK_MS: u64 = 80;

/// Custom multi-select widget that shows live-updating size info per worktree.
///
/// Displays all worktrees immediately with animated spinners. As background
/// threads resolve clean paths and compute sizes, spinners are replaced with
/// summary text (e.g., "3 items, 150.2 MiB" or "nothing to clean").
///
/// Key bindings match `dialoguer::MultiSelect`:
/// * `↑`/`k` — move cursor up
/// * `↓`/`j`/`Tab` — move cursor down
/// * `Space` — toggle selection
/// * `a` — toggle all
/// * `Enter` — confirm
/// * `Escape`/`q` — cancel
///
/// # Arguments
///
/// * `worktrees` - The worktrees to display
/// * `result_rx` - Channel receiving `WorktreeResolution`s from background threads
/// * `done` - Atomic flag signaling background work is complete
///
/// # Returns
///
/// `Ok((Some(indices), resolutions))` on confirm, `Ok((None, resolutions))` on cancel.
/// The `resolutions` vector contains all `WorktreeResolution`s received from
/// background threads during the session.
///
/// # Errors
///
/// * If terminal I/O fails
#[allow(clippy::too_many_lines)]
pub fn select_worktrees_with_sizes(
    worktrees: &[WorktreeInfo],
    result_rx: &mpsc::Receiver<WorktreeResolution>,
    done: &AtomicBool,
) -> io::Result<(Option<Vec<usize>>, Vec<WorktreeResolution>)> {
    let count = worktrees.len();
    if count == 0 {
        return Ok((Some(Vec::new()), Vec::new()));
    }

    let term = Term::stderr();
    let labels: Vec<String> = worktrees.iter().map(format_worktree_label).collect();

    let mut state = SelectState {
        checked: vec![false; count],
        statuses: vec![None; count],
        resolutions: Vec::new(),
        cursor: 0,
        spinner_frame: 0,
    };

    // Spawn a thread that reads keys and sends them over a channel.
    // This lets us poll for keys without blocking the render loop.
    let (key_tx, key_rx) = mpsc::channel::<Key>();
    let input_done = std::sync::Arc::new(AtomicBool::new(false));
    let input_done_clone = input_done.clone();

    let input_term = term.clone();
    let input_handle = std::thread::spawn(move || {
        loop {
            if input_done_clone.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(key) = input_term.read_key()
                && key_tx.send(key).is_err()
            {
                break;
            }
        }
    });

    term.hide_cursor()?;

    // Initial render — print the prompt header + all items
    let prompt_line = format!(
        "{} {}",
        "?".green().bold(),
        "Select worktrees to clean (space to toggle, enter to confirm):".bold()
    );
    term.write_line(&prompt_line)?;
    render_items(
        &term,
        &labels,
        &state.checked,
        &state.statuses,
        state.cursor,
        state.spinner_frame,
    )?;

    let result = run_select_loop(&term, &labels, &mut state, &key_rx, result_rx, done);

    // Cleanup: show cursor, clear the rendered lines, signal input thread to stop
    term.show_cursor()?;
    // +1 for the prompt line
    term.clear_last_lines(count + 1)?;
    input_done.store(true, Ordering::Relaxed);

    // We can't join the input thread (it's blocked on read_key), so detach it.
    drop(input_handle);

    // Return (selection, resolutions) — map the inner result
    result.map(|sel| (sel, state.resolutions))
}

/// Mutable state for the custom multi-select widget.
struct SelectState {
    checked: Vec<bool>,
    statuses: Vec<Option<output::CleanStats>>,
    resolutions: Vec<WorktreeResolution>,
    cursor: usize,
    spinner_frame: usize,
}

/// Main loop for the custom multi-select widget.
///
/// Polls for key events and background results, re-renders on changes.
#[allow(clippy::too_many_arguments)]
fn run_select_loop(
    term: &Term,
    labels: &[String],
    state: &mut SelectState,
    key_rx: &mpsc::Receiver<Key>,
    result_rx: &mpsc::Receiver<WorktreeResolution>,
    done: &AtomicBool,
) -> io::Result<Option<Vec<usize>>> {
    let count = labels.len();

    loop {
        // Drain all available background results
        let mut needs_redraw = false;
        while let Ok(res) = result_rx.try_recv() {
            if res.index < count {
                state.statuses[res.index] = Some(res.stats.clone());
                needs_redraw = true;
            }
            state.resolutions.push(res);
        }

        // Process all available key events
        while let Ok(key) = key_rx.try_recv() {
            needs_redraw = true;
            match key {
                // Move down
                Key::ArrowDown | Key::Tab | Key::Char('j') => {
                    state.cursor = (state.cursor + 1) % count;
                }
                // Move up
                Key::ArrowUp | Key::BackTab | Key::Char('k') => {
                    state.cursor = (state.cursor + count - 1) % count;
                }
                // Toggle current
                Key::Char(' ') => {
                    state.checked[state.cursor] = !state.checked[state.cursor];
                }
                // Toggle all
                Key::Char('a') => {
                    let all_checked = state.checked.iter().all(|&c| c);
                    for c in &mut state.checked {
                        *c = !all_checked;
                    }
                }
                // Confirm
                Key::Enter => {
                    let selected: Vec<usize> = state
                        .checked
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| **c)
                        .map(|(i, _)| i)
                        .collect();
                    return Ok(Some(selected));
                }
                // Cancel
                Key::Escape | Key::Char('q') => {
                    return Ok(None);
                }
                _ => {
                    needs_redraw = false;
                }
            }
        }

        // Advance spinner if there are still unresolved items
        let has_pending =
            state.statuses.iter().any(Option::is_none) && !done.load(Ordering::Relaxed);
        if has_pending {
            state.spinner_frame = (state.spinner_frame + 1) % SPINNER_FRAMES.len();
            needs_redraw = true;
        }

        if needs_redraw {
            // Clear previous render and redraw
            term.clear_last_lines(count)?;
            render_items(
                term,
                labels,
                &state.checked,
                &state.statuses,
                state.cursor,
                state.spinner_frame,
            )?;
        }

        // Sleep to avoid busy-waiting (also controls spinner speed)
        std::thread::sleep(Duration::from_millis(SPINNER_TICK_MS));
    }
}

/// Render all items for the custom multi-select widget.
///
/// Matches dialoguer's plain theme styling:
/// - `>` arrow for active item, 2-space indent for inactive
/// - `[x]` / `[ ]` ASCII checkboxes
/// - No color on checkbox text; size summary uses heat indicators
fn render_items(
    term: &Term,
    labels: &[String],
    checked: &[bool],
    statuses: &[Option<output::CleanStats>],
    cursor: usize,
    spinner_frame: usize,
) -> io::Result<()> {
    let max_size = statuses
        .iter()
        .filter_map(|status| status.as_ref().map(|stats| stats.total_size))
        .max()
        .unwrap_or(0);

    for (i, label) in labels.iter().enumerate() {
        let is_active = i == cursor;

        let prefix = if is_active { ">" } else { " " };
        let checkbox = if checked[i] { "[x]" } else { "[ ]" };

        let status = statuses[i].as_ref().map_or_else(
            || {
                let frame = SPINNER_FRAMES[spinner_frame];
                format!("  {frame} resolving...").yellow().to_string()
            },
            |stats| format!("  {}", output::format_clean_stats_heat(stats, max_size)),
        );

        term.write_line(&format!("{prefix} {checkbox} {label}{status}"))?;
    }

    term.flush()?;
    Ok(())
}

/// Custom multi-select widget for choosing worktrees to remove.
///
/// Displays all worktrees immediately with animated spinners while background
/// threads check each worktree for uncommitted changes. As checks complete,
/// spinners are replaced with warning text or disappear.
///
/// The main worktree is shown but **disabled**: it cannot be toggled and is
/// skipped by "toggle all".
///
/// Key bindings match `dialoguer::MultiSelect`:
/// * `↑`/`k` — move cursor up
/// * `↓`/`j`/`Tab` — move cursor down
/// * `Space` — toggle selection (no-op on disabled items)
/// * `a` — toggle all (excludes disabled items)
/// * `Enter` — confirm
/// * `Escape`/`q` — cancel
///
/// All items start **unchecked**.
///
/// # Arguments
///
/// * `worktrees` - The worktrees to display
/// * `warning_rx` - Channel receiving [`WarningResolution`]s from background threads
/// * `done` - Atomic flag signaling all background checks are complete
///
/// # Returns
///
/// `Ok((Some(indices), warnings))` on confirm, `Ok((None, warnings))` on cancel.
/// The `warnings` vector has the same length as `worktrees`, with resolved
/// warning text for each entry (`None` = clean or main worktree).
///
/// # Errors
///
/// * If terminal I/O fails
pub fn select_worktrees_for_removal(
    worktrees: &[WorktreeInfo],
    warning_rx: &mpsc::Receiver<WarningResolution>,
    done: &AtomicBool,
) -> io::Result<RemovalPickerResult> {
    let count = worktrees.len();
    if count == 0 {
        return Ok((Some(Vec::new()), Vec::new()));
    }

    let labels: Vec<String> = worktrees.iter().map(format_worktree_label).collect();
    let disabled: Vec<bool> = worktrees.iter().map(|wt| wt.is_main).collect();

    let mut state = RemovalSelectState {
        checked: vec![false; count],
        // Main worktrees start resolved (clean); linked start as pending.
        warnings: worktrees
            .iter()
            .map(|wt| {
                if wt.is_main {
                    WarningStatus::Clean
                } else {
                    WarningStatus::Pending
                }
            })
            .collect(),
        cursor: 0,
        spinner_frame: 0,
    };

    let term = Term::stderr();

    // Spawn a thread that reads keys and sends them over a channel.
    let (key_tx, key_rx) = mpsc::channel::<Key>();
    let input_done = std::sync::Arc::new(AtomicBool::new(false));
    let input_done_clone = input_done.clone();

    let input_term = term.clone();
    let input_handle = std::thread::spawn(move || {
        loop {
            if input_done_clone.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(key) = input_term.read_key()
                && key_tx.send(key).is_err()
            {
                break;
            }
        }
    });

    term.hide_cursor()?;

    // Prompt header
    let prompt_line = format!(
        "{} {}",
        "?".green().bold(),
        "Select worktrees to remove (space to toggle, enter to confirm):".bold()
    );
    term.write_line(&prompt_line)?;
    render_removal_items(
        &term,
        &labels,
        &state.checked,
        &disabled,
        &state.warnings,
        state.cursor,
        state.spinner_frame,
    )?;

    let result = run_removal_select_loop(
        &term, &labels, &mut state, &disabled, &key_rx, warning_rx, done,
    );

    // Cleanup
    term.show_cursor()?;
    term.clear_last_lines(count + 1)?; // +1 for prompt line
    input_done.store(true, Ordering::Relaxed);
    drop(input_handle);

    // Flatten the tri-state warnings into final resolved warnings
    let final_warnings: Vec<Option<String>> = state
        .warnings
        .into_iter()
        .map(|w| match w {
            WarningStatus::Warning(text) => Some(text),
            WarningStatus::Pending | WarningStatus::Clean => None,
        })
        .collect();

    result.map(|sel| (sel, final_warnings))
}

/// Mutable state for the removal multi-select widget.
struct RemovalSelectState {
    checked: Vec<bool>,
    /// Per-worktree warning check status.
    warnings: Vec<WarningStatus>,
    cursor: usize,
    spinner_frame: usize,
}

/// Main loop for the removal multi-select widget.
///
/// Polls for key events and background warning results, re-renders on changes.
#[allow(clippy::too_many_arguments)]
fn run_removal_select_loop(
    term: &Term,
    labels: &[String],
    state: &mut RemovalSelectState,
    disabled: &[bool],
    key_rx: &mpsc::Receiver<Key>,
    warning_rx: &mpsc::Receiver<WarningResolution>,
    done: &AtomicBool,
) -> io::Result<Option<Vec<usize>>> {
    let count = labels.len();

    loop {
        // Drain all available background results
        let mut needs_redraw = false;
        while let Ok(res) = warning_rx.try_recv() {
            if res.index < count {
                state.warnings[res.index] = res
                    .warning
                    .map_or(WarningStatus::Clean, WarningStatus::Warning);
                needs_redraw = true;
            }
        }

        // Process all available key events
        while let Ok(key) = key_rx.try_recv() {
            needs_redraw = true;
            match key {
                // Move down
                Key::ArrowDown | Key::Tab | Key::Char('j') => {
                    state.cursor = (state.cursor + 1) % count;
                }
                // Move up
                Key::ArrowUp | Key::BackTab | Key::Char('k') => {
                    state.cursor = (state.cursor + count - 1) % count;
                }
                // Toggle current (only if not disabled)
                Key::Char(' ') => {
                    if !disabled[state.cursor] {
                        state.checked[state.cursor] = !state.checked[state.cursor];
                    }
                }
                // Toggle all (excludes disabled items)
                Key::Char('a') => {
                    let all_enabled_checked = state
                        .checked
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !disabled[*i])
                        .all(|(_, &c)| c);
                    for (i, c) in state.checked.iter_mut().enumerate() {
                        if !disabled[i] {
                            *c = !all_enabled_checked;
                        }
                    }
                }
                // Confirm
                Key::Enter => {
                    let selected: Vec<usize> = state
                        .checked
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| **c)
                        .map(|(i, _)| i)
                        .collect();
                    return Ok(Some(selected));
                }
                // Cancel
                Key::Escape | Key::Char('q') => {
                    return Ok(None);
                }
                _ => {
                    needs_redraw = false;
                }
            }
        }

        // Advance spinner if there are still unresolved items
        let has_pending = state
            .warnings
            .iter()
            .any(|w| matches!(w, WarningStatus::Pending))
            && !done.load(Ordering::Relaxed);
        if has_pending {
            state.spinner_frame = (state.spinner_frame + 1) % SPINNER_FRAMES.len();
            needs_redraw = true;
        }

        if needs_redraw {
            // Clear previous render and redraw
            term.clear_last_lines(count)?;
            render_removal_items(
                term,
                labels,
                &state.checked,
                disabled,
                &state.warnings,
                state.cursor,
                state.spinner_frame,
            )?;
        }

        // Sleep to avoid busy-waiting (also controls spinner speed)
        std::thread::sleep(Duration::from_millis(SPINNER_TICK_MS));
    }
}

/// Render all items for the removal multi-select widget.
///
/// Matches dialoguer's plain theme styling:
/// - `>` arrow for active item, 2-space indent for inactive
/// - `[x]` / `[ ]` ASCII checkboxes
/// - Disabled items (main worktree) shown with dim text and `[-]` checkbox
/// - Pending checks shown with animated spinner
/// - Warning text (e.g., "has uncommitted changes") shown in yellow after the label
fn render_removal_items(
    term: &Term,
    labels: &[String],
    checked: &[bool],
    disabled: &[bool],
    warnings: &[WarningStatus],
    cursor: usize,
    spinner_frame: usize,
) -> io::Result<()> {
    for (i, label) in labels.iter().enumerate() {
        let is_active = i == cursor;
        let prefix = if is_active { ">" } else { " " };

        if disabled[i] {
            let line = format!("{prefix} [-] {label}").dimmed();
            term.write_line(&line.to_string())?;
        } else {
            let checkbox = if checked[i] { "[x]" } else { "[ ]" };
            let suffix = match warnings.get(i) {
                // Still checking — show spinner
                Some(WarningStatus::Pending) | None => {
                    let frame = SPINNER_FRAMES[spinner_frame];
                    format!("  {frame} checking...").yellow().to_string()
                }
                // Resolved with warning
                Some(WarningStatus::Warning(w)) => {
                    format!(" {}", format!("({w})").yellow())
                }
                // Resolved clean — no suffix
                Some(WarningStatus::Clean) => String::new(),
            };
            term.write_line(&format!("{prefix} {checkbox} {label}{suffix}"))?;
        }
    }

    term.flush()?;
    Ok(())
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
/// * `default_branch` - The detected default branch (e.g., "master" or "main")
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

/// Drain any buffered keystrokes from stdin.
///
/// Prevents stale input from leaking into the next interactive prompt.
/// This is important when a prompt follows another prompt or a
/// non-trivial operation — the user may have pressed extra keys
/// (e.g. Enter or `y`) while waiting, and those buffered keystrokes
/// would be consumed immediately by the next prompt.
///
/// Uses non-blocking reads to drain all pending bytes, then restores
/// the original file descriptor flags.
#[cfg(unix)]
pub fn flush_stdin() {
    use std::os::fd::AsRawFd;

    let fd = std::io::stdin().as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return;
        }
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        let mut buf = [0u8; 256];
        while libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) > 0 {}
        libc::fcntl(fd, libc::F_SETFL, flags);
    }
}

/// Drain any buffered keystrokes from stdin (no-op on non-Unix).
#[cfg(not(unix))]
pub fn flush_stdin() {}

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

/// Recovery action when a branch already exists during worktree creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchExistsAction {
    /// Use the existing branch instead of creating a new one.
    UseExisting,
    /// Delete the existing branch and retry creation.
    DeleteAndCreate,
    /// Cancel and return the original error.
    Cancel,
}

/// Prompt the user for how to handle an existing branch conflict.
///
/// Shown when `git worktree add -b <branch>` fails because a branch
/// with that name already exists.
///
/// # Errors
///
/// * If the terminal prompt fails
#[must_use = "caller must act on the chosen recovery action"]
pub fn prompt_branch_exists_recovery(branch: &str) -> io::Result<BranchExistsAction> {
    let options = [
        "Use the existing branch",
        "Delete the branch and create fresh",
        "Cancel",
    ];

    let choice = Select::new()
        .with_prompt(format!(
            "Branch '{branch}' already exists. What would you like to do?"
        ))
        .items(options)
        .default(0)
        .interact()?;

    Ok(match choice {
        0 => BranchExistsAction::UseExisting,
        1 => BranchExistsAction::DeleteAndCreate,
        _ => BranchExistsAction::Cancel,
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
