//! Configuration loading for worktree-setup.
//!
//! This crate provides configuration types and loading functionality for the worktree-setup CLI.
//! It supports both TOML and TypeScript configuration files.
//!
//! # Supported Config Formats
//!
//! * TOML (`worktree.config.toml`) - Native Rust parsing
//! * TypeScript (`worktree.config.ts`) - Evaluated via bun subprocess
//!
//! # Example
//!
//! ```rust,ignore
//! use worktree_setup_config::{discover_configs, load_config};
//!
//! let configs = discover_configs("/path/to/repo")?;
//! for path in configs {
//!     let loaded = load_config(&path)?;
//!     println!("{}: {}", loaded.relative_path, loaded.config.description);
//! }
//! ```

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

mod discovery;
mod error;
mod toml_loader;
mod ts_loader;
mod types;

pub use discovery::{discover_configs, get_config_display_name};
pub use error::ConfigError;
pub use toml_loader::load_toml_config;
pub use ts_loader::load_ts_config;
pub use types::{Config, LoadedConfig, TemplateMapping};

use std::path::Path;

/// Load a configuration file, auto-detecting the format based on extension.
///
/// # Arguments
///
/// * `path` - Path to the configuration file
/// * `repo_root` - Path to the repository root (for calculating relative paths)
///
/// # Errors
///
/// * If the file extension is not supported
/// * If the file cannot be read
/// * If the file cannot be parsed
pub fn load_config(path: &Path, repo_root: &Path) -> Result<LoadedConfig, ConfigError> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let config = match extension {
        "toml" => load_toml_config(path)?,
        "ts" => load_ts_config(path)?,
        _ => return Err(ConfigError::UnsupportedFormat(extension.to_string())),
    };

    let config_dir = path
        .parent()
        .ok_or_else(|| ConfigError::InvalidPath(path.to_path_buf()))?
        .to_path_buf();

    let relative_path = path
        .strip_prefix(repo_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string());

    Ok(LoadedConfig {
        config,
        config_path: path.to_path_buf(),
        config_dir,
        relative_path,
    })
}
