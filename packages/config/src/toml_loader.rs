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
        assert!(config.profiles.is_empty());
        assert!(config.profile_defaults.is_empty());
    }

    #[test]
    fn test_load_toml_config_with_profiles() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
description = "Config with profiles"
symlinks = [".env"]
profiles = ["dev", "ci"]

[profileDefaults.dev]
copyUnstaged = true
baseBranch = "develop"

[profileDefaults.ci]
postSetup = "none"
"#
        )
        .unwrap();

        let config = load_toml_config(file.path()).unwrap();

        assert_eq!(config.description, "Config with profiles");
        assert_eq!(config.profiles, vec!["dev", "ci"]);
        assert_eq!(config.profile_defaults.len(), 2);

        let dev_defaults = &config.profile_defaults["dev"];
        assert_eq!(dev_defaults.copy_unstaged, Some(true));
        assert_eq!(dev_defaults.base_branch.as_deref(), Some("develop"));
        assert_eq!(dev_defaults.post_setup, None);

        let ci_defaults = &config.profile_defaults["ci"];
        assert_eq!(
            ci_defaults.post_setup,
            Some(crate::types::PostSetupMode::Keyword(
                crate::types::PostSetupKeyword::None
            ))
        );
        assert_eq!(ci_defaults.copy_unstaged, None);
    }
}
