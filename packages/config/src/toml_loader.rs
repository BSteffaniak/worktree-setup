//! TOML configuration file loader.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::fs;
use std::path::Path;

use crate::error::ConfigError;
use crate::types::Config;

/// Load a TOML configuration file.
///
/// # Arguments
///
/// * `path` - Path to the TOML configuration file
///
/// # Errors
///
/// * If the file cannot be read
/// * If the file cannot be parsed as TOML
pub fn load_toml_config(path: &Path) -> Result<Config, ConfigError> {
    log::debug!("Loading TOML config from {}", path.display());

    let content = fs::read_to_string(path).map_err(|e| ConfigError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;

    let config: Config = toml::from_str(&content).map_err(|e| ConfigError::TomlParseError {
        path: path.to_path_buf(),
        source: e,
    })?;

    log::debug!("Loaded config: {:?}", config.description);

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_load_toml_config() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
description = "Test config"
symlinks = ["data/cache"]
copy = [".env.local"]
overwrite = ["config.json"]
copyGlob = ["**/*.env"]
copyUnstaged = true
postSetup = ["npm install"]
"#
        )
        .unwrap();

        let config = load_toml_config(file.path()).unwrap();

        assert_eq!(config.description, "Test config");
        assert_eq!(config.symlinks, vec!["data/cache"]);
        assert_eq!(config.copy, vec![".env.local"]);
        assert_eq!(config.overwrite, vec!["config.json"]);
        assert_eq!(config.copy_glob, vec!["**/*.env"]);
        assert!(config.copy_unstaged);
        assert_eq!(config.post_setup, vec!["npm install"]);
    }

    #[test]
    fn test_load_minimal_toml_config() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"description = "Minimal""#).unwrap();

        let config = load_toml_config(file.path()).unwrap();

        assert_eq!(config.description, "Minimal");
        assert!(config.symlinks.is_empty());
        assert!(config.copy.is_empty());
        assert!(!config.copy_unstaged);
    }
}
