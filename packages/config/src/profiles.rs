//! Profile loading and resolution.
//!
//! Profiles are named presets that control which configs are auto-selected
//! and provide defaults for operational settings.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::error::ConfigError;
use crate::types::{LoadedConfig, ProfilesFile, ResolvedProfile};

/// Load a profiles file, auto-detecting format by extension.
///
/// Supports both TOML (`.toml`) and TypeScript (`.ts`) formats.
///
/// # Arguments
///
/// * `path` - Path to the profiles file
///
/// # Errors
///
/// * If the file cannot be read or parsed
/// * If the file extension is not supported
pub fn load_profiles_file(path: &Path) -> Result<ProfilesFile, ConfigError> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    match extension {
        "toml" => load_toml_profiles(path),
        "ts" => load_ts_profiles(path),
        _ => Err(ConfigError::UnsupportedFormat(extension.to_string())),
    }
}

/// Resolve one or more profile names into a single `ResolvedProfile`.
///
/// For Phase 1, this only checks the central profiles file. Individual
/// config `profiles` / `profile_defaults` fields will be added in Phase 2.
///
/// # Resolution Logic
///
/// 1. For each profile name, look it up in the profiles file
/// 2. Match the profile's config patterns against `all_configs` by substring
/// 3. When multiple profiles are given, union the config lists and merge
///    defaults (later profile's defaults win on conflicts)
/// 4. Dedup config indices
///
/// # Errors
///
/// * `ProfileNotFound` if any requested profile name is not defined
pub fn resolve_profiles(
    profile_names: &[String],
    profiles_file: Option<&ProfilesFile>,
    all_configs: &[LoadedConfig],
) -> Result<ResolvedProfile, ConfigError> {
    let mut resolved = ResolvedProfile {
        names: profile_names.to_vec(),
        ..Default::default()
    };

    let mut seen_indices = BTreeSet::new();

    for name in profile_names {
        // Look up the profile definition
        let definition = profiles_file
            .and_then(|pf| pf.profiles.get(name.as_str()))
            .ok_or_else(|| ConfigError::ProfileNotFound(name.clone()))?;

        // Use the last profile's description
        if !definition.description.is_empty() {
            resolved.description.clone_from(&definition.description);
        }

        // Match config patterns against all_configs
        for pattern in &definition.configs {
            for (idx, config) in all_configs.iter().enumerate() {
                if !seen_indices.contains(&idx)
                    && (config.relative_path.contains(pattern.as_str())
                        || config
                            .config_path
                            .to_string_lossy()
                            .contains(pattern.as_str()))
                {
                    seen_indices.insert(idx);
                    resolved.config_indices.push(idx);
                }
            }
        }

        // Merge defaults (later profile wins)
        resolved.defaults.merge(&definition.defaults);
    }

    Ok(resolved)
}

/// Load a TOML profiles file.
fn load_toml_profiles(path: &Path) -> Result<ProfilesFile, ConfigError> {
    log::debug!("Loading TOML profiles from {}", path.display());

    let content = fs::read_to_string(path).map_err(|e| ConfigError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;

    let profiles_file: ProfilesFile =
        toml::from_str(&content).map_err(|e| ConfigError::TomlParseError {
            path: path.to_path_buf(),
            source: e,
        })?;

    log::debug!("Loaded {} profiles", profiles_file.profiles.len());

    Ok(profiles_file)
}

/// Load a TypeScript profiles file by evaluating it with bun or deno.
fn load_ts_profiles(path: &Path) -> Result<ProfilesFile, ConfigError> {
    log::debug!("Loading TypeScript profiles from {}", path.display());

    // Try bun first, then deno
    match try_load_ts_with_bun(path) {
        Ok(profiles) => return Ok(profiles),
        Err(e) => log::debug!("bun failed for profiles: {e}"),
    }

    match try_load_ts_with_deno(path) {
        Ok(profiles) => return Ok(profiles),
        Err(e) => log::debug!("deno failed for profiles: {e}"),
    }

    Err(ConfigError::NoJsRuntime)
}

/// Try to load the profiles file using bun.
fn try_load_ts_with_bun(path: &Path) -> Result<ProfilesFile, ConfigError> {
    let path_str = path.to_string_lossy();
    let script = format!(
        r#"const m = await import("file://{path_str}"); console.log(JSON.stringify(m.default ?? m));"#
    );

    let output = Command::new("bun")
        .args(["-e", &script])
        .output()
        .map_err(|e| ConfigError::TypeScriptEvalError {
            path: path.to_path_buf(),
            message: format!("Failed to run bun: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ConfigError::TypeScriptEvalError {
            path: path.to_path_buf(),
            message: stderr.to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).map_err(|e| ConfigError::JsonParseError {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Try to load the profiles file using deno.
fn try_load_ts_with_deno(path: &Path) -> Result<ProfilesFile, ConfigError> {
    let path_str = path.to_string_lossy();
    let script = format!(
        r#"const m = await import("file://{path_str}"); console.log(JSON.stringify(m.default ?? m));"#
    );

    let output = Command::new("deno")
        .args(["eval", "--allow-read", &script])
        .output()
        .map_err(|e| ConfigError::TypeScriptEvalError {
            path: path.to_path_buf(),
            message: format!("Failed to run deno: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ConfigError::TypeScriptEvalError {
            path: path.to_path_buf(),
            message: stderr.to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).map_err(|e| ConfigError::JsonParseError {
        path: path.to_path_buf(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Config, ProfileDefaults};
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn make_loaded_config(relative_path: &str, description: &str) -> LoadedConfig {
        LoadedConfig {
            config: Config {
                description: description.to_string(),
                ..Default::default()
            },
            config_path: PathBuf::from(format!("/repo/{relative_path}")),
            config_dir: PathBuf::from("/repo"),
            relative_path: relative_path.to_string(),
        }
    }

    #[test]
    fn test_load_toml_profiles() {
        let mut file = NamedTempFile::with_suffix(".toml").unwrap();
        writeln!(
            file,
            r#"
[profiles.my-app]
description = "My app development"
configs = ["apps/my-app/worktree.config.ts"]
copyUnstaged = true
baseBranch = "main"
"#
        )
        .unwrap();

        let profiles = load_profiles_file(file.path()).unwrap();
        assert_eq!(profiles.profiles.len(), 1);

        let profile = &profiles.profiles["my-app"];
        assert_eq!(profile.description, "My app development");
        assert_eq!(profile.configs, vec!["apps/my-app/worktree.config.ts"]);
        assert_eq!(profile.defaults.copy_unstaged, Some(true));
        assert_eq!(profile.defaults.base_branch.as_deref(), Some("main"));
    }

    #[test]
    fn test_resolve_profiles_basic() {
        let configs = vec![
            make_loaded_config("apps/my-app/worktree.config.ts", "my-app config"),
            make_loaded_config("apps/my-app/worktree.local.config.ts", "my-app local"),
            make_loaded_config("apps/other/worktree.config.ts", "other config"),
        ];

        let mut profiles_file = ProfilesFile::default();
        profiles_file.profiles.insert(
            "my-app".to_string(),
            crate::types::ProfileDefinition {
                description: "My App".to_string(),
                configs: vec!["apps/my-app/".to_string()],
                defaults: ProfileDefaults {
                    copy_unstaged: Some(true),
                    ..Default::default()
                },
            },
        );

        let resolved =
            resolve_profiles(&["my-app".to_string()], Some(&profiles_file), &configs).unwrap();

        assert_eq!(resolved.config_indices, vec![0, 1]);
        assert_eq!(resolved.description, "My App");
        assert_eq!(resolved.defaults.copy_unstaged, Some(true));
    }

    #[test]
    fn test_resolve_profiles_not_found() {
        let profiles_file = ProfilesFile::default();
        let result = resolve_profiles(&["nonexistent".to_string()], Some(&profiles_file), &[]);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Profile not found: 'nonexistent'"));
    }

    #[test]
    fn test_resolve_multiple_profiles() {
        let configs = vec![
            make_loaded_config("apps/my-app/worktree.config.ts", "main config"),
            make_loaded_config("apps/my-app/worktree.local.config.ts", "local config"),
            make_loaded_config("apps/my-app/worktree.nix.config.ts", "nix config"),
        ];

        let mut profiles_file = ProfilesFile::default();
        profiles_file.profiles.insert(
            "dev".to_string(),
            crate::types::ProfileDefinition {
                description: "Dev".to_string(),
                configs: vec![
                    "worktree.config.ts".to_string(),
                    "worktree.local.config.ts".to_string(),
                ],
                defaults: ProfileDefaults {
                    skip_post_setup: Some(false),
                    ..Default::default()
                },
            },
        );
        profiles_file.profiles.insert(
            "nix".to_string(),
            crate::types::ProfileDefinition {
                description: "Nix".to_string(),
                configs: vec![
                    "worktree.config.ts".to_string(),
                    "worktree.nix.config.ts".to_string(),
                ],
                defaults: ProfileDefaults {
                    skip_post_setup: Some(true),
                    ..Default::default()
                },
            },
        );

        let resolved = resolve_profiles(
            &["dev".to_string(), "nix".to_string()],
            Some(&profiles_file),
            &configs,
        )
        .unwrap();

        // Union of configs, deduped (0 appears in both, should only be listed once)
        assert_eq!(resolved.config_indices, vec![0, 1, 2]);
        // Later profile's description wins
        assert_eq!(resolved.description, "Nix");
        // Later profile's defaults win
        assert_eq!(resolved.defaults.skip_post_setup, Some(true));
    }

    #[test]
    fn test_profile_defaults_merge() {
        let mut base = ProfileDefaults {
            skip_post_setup: Some(false),
            copy_unstaged: Some(true),
            base_branch: Some("main".to_string()),
            ..Default::default()
        };

        let overlay = ProfileDefaults {
            skip_post_setup: Some(true),
            remote: Some("upstream".to_string()),
            ..Default::default()
        };

        base.merge(&overlay);

        assert_eq!(base.skip_post_setup, Some(true)); // overridden
        assert_eq!(base.copy_unstaged, Some(true)); // kept
        assert_eq!(base.base_branch.as_deref(), Some("main")); // kept
        assert_eq!(base.remote.as_deref(), Some("upstream")); // new
        assert_eq!(base.new_branch, None); // still None
    }
}
