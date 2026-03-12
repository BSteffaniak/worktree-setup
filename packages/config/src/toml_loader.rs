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
        assert!(config.clean.is_empty());
    }

    #[test]
    fn test_load_toml_config_with_clean() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
description = "Config with clean paths"
clean = ["node_modules", ".turbo", "**/dist", "*.log"]
"#
        )
        .unwrap();

        let config = load_toml_config(file.path()).unwrap();

        assert_eq!(config.description, "Config with clean paths");
        assert_eq!(
            config.clean,
            vec!["node_modules", ".turbo", "**/dist", "*.log"]
        );
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
        assert!(config.clean.is_empty());
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn test_load_toml_config_with_profiles() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
description = "Config with profiles"
symlinks = [".env"]

[profiles.dev]
copyUnstaged = true
baseBranch = "develop"

[profiles.ci]
postSetup = "none"
"#
        )
        .unwrap();

        let config = load_toml_config(file.path()).unwrap();

        assert_eq!(config.description, "Config with profiles");
        assert_eq!(config.profiles.len(), 2);
        assert!(config.profiles.contains_key("dev"));
        assert!(config.profiles.contains_key("ci"));

        let dev = &config.profiles["dev"];
        assert_eq!(dev.defaults.copy_unstaged, Some(true));
        assert_eq!(dev.defaults.base_branch.as_deref(), Some("develop"));
        assert_eq!(dev.defaults.post_setup, None);

        let ci = &config.profiles["ci"];
        assert_eq!(
            ci.defaults.post_setup,
            Some(crate::types::PostSetupMode::Keyword(
                crate::types::PostSetupKeyword::None
            ))
        );
        assert_eq!(ci.defaults.copy_unstaged, None);
    }

    #[test]
    fn test_load_toml_config_with_all_new_profile_defaults() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
description = "Config with all Phase 3 fields"

[profiles.full]
autoCreate = true
creationMethod = "remote"
baseBranch = "master"
newBranch = false
remote = "upstream"
overwriteExisting = true
copyUnstaged = false
postSetup = "all"
skipPostSetup = ["bun generate", "bun build"]
"#
        )
        .unwrap();

        let config = load_toml_config(file.path()).unwrap();

        let defaults = &config.profiles["full"].defaults;
        assert_eq!(defaults.auto_create, Some(true));
        assert_eq!(
            defaults.creation_method,
            Some(crate::types::CreationMethod::Remote)
        );
        assert_eq!(defaults.base_branch.as_deref(), Some("master"));
        assert_eq!(defaults.new_branch, Some(false));
        assert_eq!(defaults.remote.as_deref(), Some("upstream"));
        assert_eq!(defaults.overwrite_existing, Some(true));
        assert_eq!(defaults.copy_unstaged, Some(false));
        assert_eq!(
            defaults.post_setup,
            Some(crate::types::PostSetupMode::Keyword(
                crate::types::PostSetupKeyword::All
            ))
        );
        assert_eq!(
            defaults.skip_post_setup,
            vec!["bun generate".to_string(), "bun build".to_string()]
        );
    }

    #[test]
    fn test_load_toml_config_with_post_setup_commands_list() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
description = "Config with commands list"

[profiles.selective]
postSetup = ["bun install", "bun migrate"]
"#
        )
        .unwrap();

        let config = load_toml_config(file.path()).unwrap();

        let defaults = &config.profiles["selective"].defaults;
        assert_eq!(
            defaults.post_setup,
            Some(crate::types::PostSetupMode::Commands(vec![
                "bun install".to_string(),
                "bun migrate".to_string(),
            ]))
        );
        assert!(defaults.skip_post_setup.is_empty());
    }
}
