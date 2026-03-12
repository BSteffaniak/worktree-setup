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
