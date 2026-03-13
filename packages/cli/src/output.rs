//! Terminal output formatting.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use colored::Colorize;

/// Print a header message.
pub fn print_header(message: &str) {
    println!("\n🌳 {}\n", message.bold());
}

/// Print repository info.
pub fn print_repo_info(repo_root: &str) {
    println!("Repository: {}", repo_root.cyan());
}

/// Print a list of discovered configs.
pub fn print_config_list(configs: &[(String, String)]) {
    println!(
        "Found {} config{}:",
        configs.len(),
        if configs.len() == 1 { "" } else { "s" }
    );
    for (path, description) in configs {
        println!("  {} {} - {}", "•".dimmed(), path.yellow(), description);
    }
    println!();
}

/// Print a list of available profiles.
pub fn print_profile_list(profiles: &[(String, String, usize)]) {
    if profiles.is_empty() {
        return;
    }
    println!(
        "Available profile{}:",
        if profiles.len() == 1 { "" } else { "s" }
    );
    for (name, description, config_count) in profiles {
        let config_label = if *config_count == 1 {
            "config".to_string()
        } else {
            format!("{config_count} configs")
        };
        if description.is_empty() {
            println!("  {} {} ({})", "•".dimmed(), name.cyan(), config_label);
        } else {
            println!(
                "  {} {} - {} ({})",
                "•".dimmed(),
                name.cyan(),
                description,
                config_label
            );
        }
    }
    println!();
}

/// Print profile usage info.
pub fn print_using_profile(names: &[String]) {
    let label = if names.len() == 1 {
        format!("Using profile: {}", names[0])
    } else {
        format!("Using profiles: {}", names.join(", "))
    };
    println!("{}", label.cyan());
}

/// Print the configs selected by a profile.
pub fn print_profile_configs(configs: &[(String, String)]) {
    println!(
        "Using {} config{}:",
        configs.len(),
        if configs.len() == 1 { "" } else { "s" }
    );
    for (path, description) in configs {
        println!("  {} {} - {}", "•".dimmed(), path.yellow(), description);
    }
    println!();
}

/// Print a post-setup command being run.
pub fn print_command(cmd: &str) {
    println!("  {} {}", "$".dimmed(), cmd);
}

/// Print success message.
pub fn print_success() {
    println!("✅ Worktree setup complete!");
}

/// Print error message.
pub fn print_error(message: &str) {
    eprintln!("{} {}", "Error:".red().bold(), message);
}

/// Print a warning message.
pub fn print_warning(message: &str) {
    println!("{} {}", "Warning:".yellow().bold(), message);
}

/// Print an informational message.
pub fn print_info(message: &str) {
    println!("{} {}", "Info:".cyan().bold(), message);
}

/// An item resolved for cleaning (deletion).
#[derive(Clone)]
pub struct CleanItem {
    /// Display path relative to the target directory.
    pub relative_path: String,
    /// Whether this is a directory (vs a file).
    pub is_dir: bool,
    /// Size in bytes.
    pub size: u64,
}

/// Format a byte count as a human-readable size string.
///
/// Uses binary units: B, KB, MB, GB.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Print a detailed preview of items that will be cleaned (deleted).
///
/// Shows each item with its type (dir/file), relative path, and size.
/// Prints a summary line with total count and total size.
pub fn print_clean_preview(items: &[CleanItem]) {
    if items.is_empty() {
        println!("Nothing to clean.");
        return;
    }

    println!(
        "Will delete {} item{}:",
        items.len(),
        if items.len() == 1 { "" } else { "s" }
    );

    for item in items {
        let type_label = if item.is_dir { "dir " } else { "file" };
        let size_str = format_size(item.size);
        println!(
            "  {} {} {} {}",
            "•".dimmed(),
            format!("[{type_label}]").dimmed(),
            item.relative_path.yellow(),
            format!("({size_str})").dimmed(),
        );
    }

    let total_size: u64 = items.iter().map(|i| i.size).sum();
    println!(
        "\n  {} {}",
        "Total:".bold(),
        format!("{} items, {}", items.len(), format_size(total_size)).bold()
    );
}

/// Print a summary after cleaning completes.
pub fn print_clean_summary(deleted_count: usize, total_size: u64) {
    println!(
        "Deleted {} item{}, freed {}",
        deleted_count,
        if deleted_count == 1 { "" } else { "s" },
        format_size(total_size)
    );
}

/// Print a detailed preview of items grouped by worktree.
///
/// Shows a header for each worktree, items underneath, and a grand total.
pub fn print_multi_worktree_clean_preview(groups: &[(String, Vec<CleanItem>)]) {
    let total_items: usize = groups.iter().map(|(_, items)| items.len()).sum();

    if total_items == 0 {
        println!("Nothing to clean across selected worktrees.");
        return;
    }

    for (label, items) in groups {
        if items.is_empty() {
            continue;
        }

        println!(
            "  {} ({} item{}):",
            label.cyan().bold(),
            items.len(),
            if items.len() == 1 { "" } else { "s" }
        );

        for item in items {
            let type_label = if item.is_dir { "dir " } else { "file" };
            let size_str = format_size(item.size);
            println!(
                "    {} {} {} {}",
                "•".dimmed(),
                format!("[{type_label}]").dimmed(),
                item.relative_path.yellow(),
                format!("({size_str})").dimmed(),
            );
        }
        println!();
    }

    let total_size: u64 = groups
        .iter()
        .flat_map(|(_, items)| items.iter().map(|i| i.size))
        .sum();
    let worktree_count = groups.iter().filter(|(_, items)| !items.is_empty()).count();
    println!(
        "  {} {}",
        "Total:".bold(),
        format!(
            "{total_items} items across {worktree_count} worktree{}, {}",
            if worktree_count == 1 { "" } else { "s" },
            format_size(total_size)
        )
        .bold()
    );
}

/// Print a summary after multi-worktree cleaning completes.
pub fn print_multi_worktree_clean_summary(
    deleted_count: usize,
    total_size: u64,
    worktree_count: usize,
) {
    println!(
        "Deleted {} item{} across {} worktree{}, freed {}",
        deleted_count,
        if deleted_count == 1 { "" } else { "s" },
        worktree_count,
        if worktree_count == 1 { "" } else { "s" },
        format_size(total_size)
    );
}

// ---------------------------------------------------------------------------
// Remove subcommand output
// ---------------------------------------------------------------------------

/// Information about a worktree to be removed, for display purposes.
pub struct RemoveDisplayInfo {
    /// Branch name (if any).
    pub branch: Option<String>,
    /// Filesystem path.
    pub path: String,
    /// Whether the worktree has uncommitted changes.
    pub has_changes: bool,
}

/// Print a preview of worktrees that will be removed.
///
/// Lists each worktree with its branch and path. Worktrees with
/// uncommitted changes are flagged with a warning.
pub fn print_remove_preview(worktrees: &[RemoveDisplayInfo]) {
    if worktrees.is_empty() {
        println!("No worktrees to remove.");
        return;
    }

    println!(
        "\nWill remove {} worktree{}:",
        worktrees.len(),
        if worktrees.len() == 1 { "" } else { "s" }
    );

    for wt in worktrees {
        let label = wt.branch.as_deref().unwrap_or("(detached)");
        let warning = if wt.has_changes {
            format!(" {}", "(has uncommitted changes)".yellow())
        } else {
            String::new()
        };
        println!(
            "  {} {} {}{}",
            "•".dimmed(),
            label.cyan(),
            wt.path.dimmed(),
            warning,
        );
    }
    println!();
}

/// Print a summary after worktree removal completes.
pub fn print_remove_summary(removed: usize, failed: usize) {
    if failed == 0 {
        println!(
            "{}",
            format!(
                "Removed {} worktree{}.",
                removed,
                if removed == 1 { "" } else { "s" }
            )
            .green()
        );
    } else {
        println!(
            "Removed {} worktree{}, {} failed.",
            removed,
            if removed == 1 { "" } else { "s" },
            failed,
        );
    }
}

/// Print a summary of branches that were deleted after worktree removal.
pub fn print_branch_delete_summary(deleted: &[String]) {
    if deleted.is_empty() {
        return;
    }

    println!(
        "Deleted {} branch{}:",
        deleted.len(),
        if deleted.len() == 1 { "" } else { "es" }
    );
    for branch in deleted {
        println!("  {} {}", "•".dimmed(), branch.cyan());
    }
}

/// Print a note that the user's CWD was inside a removed worktree.
pub fn print_cwd_removed_note() {
    println!(
        "\n{} Your current directory was inside the removed worktree. Run {} to return to a valid directory.",
        "Note:".yellow().bold(),
        "cd ..".bold(),
    );
}
