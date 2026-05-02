//! Terminal output formatting.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::cmp::Ordering;

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
    /// Whether this is a truly empty directory.
    pub is_empty_dir: bool,
    /// Size in bytes.
    pub size: u64,
}

/// Aggregate clean preview statistics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanStats {
    /// Total number of cleanable items.
    pub item_count: usize,
    /// Total size in bytes.
    pub total_size: u64,
    /// Number of truly empty directories.
    pub empty_dir_count: usize,
    /// Whether the worktree could not be accessed.
    pub inaccessible: bool,
}

impl CleanStats {
    /// Create stats for an inaccessible worktree.
    #[must_use]
    pub const fn inaccessible() -> Self {
        Self {
            item_count: 0,
            total_size: 0,
            empty_dir_count: 0,
            inaccessible: true,
        }
    }
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

fn sorted_items_for_display(items: &[CleanItem]) -> Vec<&CleanItem> {
    let mut sorted_items: Vec<_> = items.iter().collect();
    sorted_items.sort_by(|left, right| {
        left.is_empty_dir
            .cmp(&right.is_empty_dir)
            .then_with(|| display_size_cmp(left.size, right.size))
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    sorted_items
}

fn display_size_cmp(left: u64, right: u64) -> Ordering {
    right.cmp(&left)
}

fn print_clean_item(item: &CleanItem, max_size: u64, indent: usize) {
    let padding = " ".repeat(indent);
    if item.is_empty_dir {
        println!(
            "{padding}{} {} {}",
            "∅".cyan().bold(),
            "[empty dir]".cyan().bold(),
            item.relative_path.cyan().dimmed(),
        );
        return;
    }

    let type_label = if item.is_dir { "dir " } else { "file" };
    let type_label = format!("[{type_label}]");
    let size_label = color_by_size(&format_size(item.size), item.size);
    println!(
        "{padding}{} {} {} {} ({size_label})",
        "•".dimmed(),
        type_label.dimmed(),
        item.relative_path.yellow(),
        heat_bar(item.size, max_size),
    );
}

fn heat_bar(size: u64, max_size: u64) -> String {
    const BAR_WIDTH: usize = 8;
    const BAR_WIDTH_U64: u64 = BAR_WIDTH as u64;

    if max_size == 0 || size == 0 {
        return "░░░░░░░░".dimmed().to_string();
    }

    let filled_u64 = size
        .saturating_mul(BAR_WIDTH_U64)
        .div_ceil(max_size)
        .clamp(1, BAR_WIDTH_U64);
    let filled = usize::try_from(filled_u64).map_or(BAR_WIDTH, |value| value);
    let empty = BAR_WIDTH - filled;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
    color_by_size(&bar, size)
}

fn color_by_size(text: &str, size: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    if size >= GIB {
        text.bright_red().bold().to_string()
    } else if size >= 512 * MIB {
        text.red().bold().to_string()
    } else if size >= 100 * MIB {
        text.red().to_string()
    } else if size >= 10 * MIB {
        text.yellow().bold().to_string()
    } else if size >= MIB {
        text.yellow().to_string()
    } else {
        text.dimmed().to_string()
    }
}

/// Build clean statistics for a list of clean items.
#[must_use]
pub fn clean_stats(items: &[CleanItem]) -> CleanStats {
    CleanStats {
        item_count: items.len(),
        total_size: items.iter().map(|i| i.size).sum(),
        empty_dir_count: items.iter().filter(|i| i.is_empty_dir).count(),
        inaccessible: false,
    }
}

/// Format clean statistics as plain text.
#[must_use]
pub fn format_clean_stats_plain(stats: &CleanStats) -> String {
    if stats.inaccessible {
        return "inaccessible".to_string();
    }
    if stats.item_count == 0 {
        return "nothing to clean".to_string();
    }
    if stats.empty_dir_count == stats.item_count {
        return format!(
            "{} empty dir{}",
            stats.empty_dir_count,
            if stats.empty_dir_count == 1 { "" } else { "s" }
        );
    }

    let empty_suffix = if stats.empty_dir_count == 0 {
        String::new()
    } else {
        format!(
            ", {} empty dir{}",
            stats.empty_dir_count,
            if stats.empty_dir_count == 1 { "" } else { "s" }
        )
    };
    format!(
        "{} item{}, {}{}",
        stats.item_count,
        if stats.item_count == 1 { "" } else { "s" },
        format_size(stats.total_size),
        empty_suffix
    )
}

/// Format clean statistics with heat indicators.
#[must_use]
pub fn format_clean_stats_heat(stats: &CleanStats, max_size: u64) -> String {
    if stats.inaccessible {
        return "inaccessible".red().bold().to_string();
    }
    if stats.item_count == 0 {
        return "nothing to clean".dimmed().to_string();
    }
    if stats.empty_dir_count == stats.item_count {
        return format!("∅ {}", format_clean_stats_plain(stats))
            .cyan()
            .bold()
            .to_string();
    }

    format!(
        "{} {}",
        heat_bar(stats.total_size, max_size),
        color_by_size(&format_clean_stats_plain(stats), stats.total_size)
    )
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

    let sorted_items = sorted_items_for_display(items);
    let max_size = sorted_items.iter().map(|item| item.size).max().unwrap_or(0);

    for item in sorted_items {
        print_clean_item(item, max_size, 2);
    }

    let stats = clean_stats(items);
    println!(
        "\n  {} {}",
        "Total:".bold(),
        format_clean_stats_plain(&stats).bold()
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

    let mut sorted_groups: Vec<_> = groups
        .iter()
        .filter(|(_, items)| !items.is_empty())
        .collect();
    sorted_groups.sort_by(|(left_label, left_items), (right_label, right_items)| {
        let left_stats = clean_stats(left_items);
        let right_stats = clean_stats(right_items);
        display_size_cmp(left_stats.total_size, right_stats.total_size)
            .then_with(|| left_stats.empty_dir_count.cmp(&right_stats.empty_dir_count))
            .then_with(|| left_label.cmp(right_label))
    });

    let max_group_size = sorted_groups
        .iter()
        .map(|(_, items)| clean_stats(items).total_size)
        .max()
        .unwrap_or(0);
    let max_item_size = sorted_groups
        .iter()
        .flat_map(|(_, items)| items.iter().map(|item| item.size))
        .max()
        .unwrap_or(0);

    for (label, items) in sorted_groups {
        let stats = clean_stats(items);
        println!(
            "  {} {}:",
            label.cyan().bold(),
            format!("({})", format_clean_stats_heat(&stats, max_group_size)).bold()
        );

        for item in sorted_items_for_display(items) {
            print_clean_item(item, max_item_size, 4);
        }
        println!();
    }

    let total_size: u64 = groups
        .iter()
        .flat_map(|(_, items)| items.iter().map(|i| i.size))
        .sum();
    let empty_dir_count: usize = groups
        .iter()
        .flat_map(|(_, items)| items.iter().filter(|i| i.is_empty_dir))
        .count();
    let worktree_count = groups.iter().filter(|(_, items)| !items.is_empty()).count();
    let stats = CleanStats {
        item_count: total_items,
        total_size,
        empty_dir_count,
        inaccessible: false,
    };
    println!(
        "  {} {} across {worktree_count} worktree{}",
        "Total:".bold(),
        format_clean_stats_plain(&stats).bold(),
        if worktree_count == 1 { "" } else { "s" },
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
