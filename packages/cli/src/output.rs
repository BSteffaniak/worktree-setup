//! Terminal output formatting.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use colored::Colorize;

/// Print a header message.
pub fn print_header(message: &str) {
    println!("\n{} {}\n", "🌳", message.bold());
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

/// Print a post-setup command being run.
pub fn print_command(cmd: &str) {
    println!("  {} {}", "$".dimmed(), cmd);
}

/// Print success message.
pub fn print_success() {
    println!("{} Worktree setup complete!", "✅");
}

/// Print error message.
pub fn print_error(message: &str) {
    eprintln!("{} {}", "Error:".red().bold(), message);
}

/// Print a warning message.
pub fn print_warning(message: &str) {
    println!("{} {}", "Warning:".yellow().bold(), message);
}
