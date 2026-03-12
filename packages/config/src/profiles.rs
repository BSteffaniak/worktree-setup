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
/// Resolution checks both the central profiles file and individual config
/// `profiles` / `profile_defaults` fields.
///
/// # Resolution Logic
///
/// For each profile name:
///
/// 1. Look it up in the central profiles file (if present) to get config
///    patterns and central defaults
/// 2. Match config patterns against `all_configs` by substring
/// 3. Also include any config that declares the profile name in its
///    `profiles` field
/// 4. Merge defaults: central profile defaults first, then per-config
///    `profile_defaults` for matching configs (per-config wins over central)
///
/// When multiple profiles are given, they are processed in order. Config
/// lists are unioned (deduped). For defaults, later profiles win over
/// earlier profiles.
///
/// A profile name does **not** need to exist in the central profiles file
/// if at least one config declares it in its `profiles` field.
///
/// # Errors
///
/// * `ProfileNotFound` if a requested profile name is not defined in
///   the central file AND no config declares it in `profiles`
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
        // Look up central profile definition (may not exist)
        let central_def = profiles_file.and_then(|pf| pf.profiles.get(name.as_str()));

        // Check if any config declares this profile
        let any_config_declares = all_configs
            .iter()
            .any(|c| c.config.profiles.iter().any(|p| p == name));

        // If neither central nor any config declares this profile, error
        if central_def.is_none() && !any_config_declares {
            return Err(ConfigError::ProfileNotFound(name.clone()));
        }

        // Use the last profile's description (from central definition)
        if let Some(def) = central_def
            && !def.description.is_empty()
        {
            resolved.description.clone_from(&def.description);
        }

        // Step 1: Merge central defaults first (lowest priority for this profile)
        if let Some(def) = central_def {
            resolved.defaults.merge(&def.defaults);
        }

        // Step 2: Match config patterns from central definition
        if let Some(def) = central_def {
            for pattern in &def.configs {
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
        }

        // Step 3: Include configs that declare this profile name
        for (idx, config) in all_configs.iter().enumerate() {
            if !seen_indices.contains(&idx) && config.config.profiles.iter().any(|p| p == name) {
                seen_indices.insert(idx);
                resolved.config_indices.push(idx);
            }
        }

        // Step 4: Merge per-config profile_defaults (higher priority than central)
        // Iterate over all selected configs (including newly added ones)
        // and merge any profile_defaults they declare for this profile name.
        for &idx in &resolved.config_indices {
            if let Some(config_defaults) = all_configs[idx].config.profile_defaults.get(name) {
                resolved.defaults.merge(config_defaults);
            }
        }
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
    use crate::types::{Config, ProfileDefaults, ProfileDefinition};
    use std::collections::BTreeMap;
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

    fn make_loaded_config_with_profiles(
        relative_path: &str,
        description: &str,
        profiles: Vec<&str>,
        profile_defaults: BTreeMap<String, ProfileDefaults>,
    ) -> LoadedConfig {
        LoadedConfig {
            config: Config {
                description: description.to_string(),
                profiles: profiles.into_iter().map(String::from).collect(),
                profile_defaults,
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

[profiles.remote]
description = "Remote branch tracking"
configs = ["apps/my-app/"]
trackRemoteBranch = true
remote = "origin"
"#
        )
        .unwrap();

        let profiles = load_profiles_file(file.path()).unwrap();
        assert_eq!(profiles.profiles.len(), 2);

        let profile = &profiles.profiles["my-app"];
        assert_eq!(profile.description, "My app development");
        assert_eq!(profile.configs, vec!["apps/my-app/worktree.config.ts"]);
        assert_eq!(profile.defaults.copy_unstaged, Some(true));
        assert_eq!(profile.defaults.base_branch.as_deref(), Some("main"));
        assert_eq!(profile.defaults.track_remote_branch, None);

        let remote_profile = &profiles.profiles["remote"];
        assert_eq!(remote_profile.description, "Remote branch tracking");
        assert_eq!(remote_profile.defaults.track_remote_branch, Some(true));
        assert_eq!(remote_profile.defaults.remote.as_deref(), Some("origin"));
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
            track_remote_branch: Some(true),
            ..Default::default()
        };

        base.merge(&overlay);

        assert_eq!(base.skip_post_setup, Some(true)); // overridden
        assert_eq!(base.copy_unstaged, Some(true)); // kept
        assert_eq!(base.base_branch.as_deref(), Some("main")); // kept
        assert_eq!(base.remote.as_deref(), Some("upstream")); // new
        assert_eq!(base.new_branch, None); // still None
        assert_eq!(base.track_remote_branch, Some(true)); // new
    }

    // ─── Phase 2: Per-config profile declarations ──────────────────────────

    #[test]
    fn test_config_declares_profile_membership() {
        // A config declares itself as belonging to profile "dev",
        // with no central profiles file at all.
        let configs = vec![
            make_loaded_config_with_profiles(
                "apps/my-app/worktree.config.ts",
                "my-app config",
                vec!["dev"],
                BTreeMap::new(),
            ),
            make_loaded_config("apps/other/worktree.config.ts", "other config"),
        ];

        let resolved = resolve_profiles(&["dev".to_string()], None, &configs).unwrap();

        assert_eq!(resolved.config_indices, vec![0]);
        // No central file, so no description
        assert_eq!(resolved.description, "");
    }

    #[test]
    fn test_config_profile_membership_no_central_file() {
        // Profile exists purely from per-config declarations, no central file
        let configs = vec![
            make_loaded_config_with_profiles(
                "apps/a/worktree.config.ts",
                "app a",
                vec!["full"],
                BTreeMap::new(),
            ),
            make_loaded_config_with_profiles(
                "apps/b/worktree.config.ts",
                "app b",
                vec!["full", "minimal"],
                BTreeMap::new(),
            ),
            make_loaded_config("apps/c/worktree.config.ts", "app c"),
        ];

        // "full" profile: picks up both a and b
        let resolved = resolve_profiles(&["full".to_string()], None, &configs).unwrap();
        assert_eq!(resolved.config_indices, vec![0, 1]);

        // "minimal" profile: picks up only b
        let resolved = resolve_profiles(&["minimal".to_string()], None, &configs).unwrap();
        assert_eq!(resolved.config_indices, vec![1]);
    }

    #[test]
    fn test_profile_not_found_when_no_source() {
        // Profile not in central file AND no config declares it -> error
        let configs = vec![make_loaded_config_with_profiles(
            "apps/a/worktree.config.ts",
            "app a",
            vec!["dev"],
            BTreeMap::new(),
        )];

        let profiles_file = ProfilesFile::default();
        let result = resolve_profiles(&["ci".to_string()], Some(&profiles_file), &configs);
        assert!(result.is_err());
    }

    #[test]
    fn test_central_and_config_declarations_combined() {
        // Central file matches config 0 via pattern.
        // Config 1 declares itself in the profile.
        // Both should be selected.
        let configs = vec![
            make_loaded_config("apps/my-app/worktree.config.ts", "matched by pattern"),
            make_loaded_config_with_profiles(
                "libs/shared/worktree.config.ts",
                "declares membership",
                vec!["dev"],
                BTreeMap::new(),
            ),
            make_loaded_config("apps/other/worktree.config.ts", "not matched"),
        ];

        let mut profiles_file = ProfilesFile::default();
        profiles_file.profiles.insert(
            "dev".to_string(),
            ProfileDefinition {
                description: "Dev".to_string(),
                configs: vec!["apps/my-app/".to_string()],
                defaults: ProfileDefaults::default(),
            },
        );

        let resolved =
            resolve_profiles(&["dev".to_string()], Some(&profiles_file), &configs).unwrap();

        assert_eq!(resolved.config_indices, vec![0, 1]);
        assert_eq!(resolved.description, "Dev");
    }

    #[test]
    fn test_per_config_defaults_override_central() {
        // Central file sets copy_unstaged = false.
        // A config overrides it to true for the same profile.
        // Per-config should win.
        let mut profile_defaults = BTreeMap::new();
        profile_defaults.insert(
            "dev".to_string(),
            ProfileDefaults {
                copy_unstaged: Some(true),
                base_branch: Some("develop".to_string()),
                ..Default::default()
            },
        );

        let configs = vec![make_loaded_config_with_profiles(
            "apps/my-app/worktree.config.ts",
            "my-app",
            vec!["dev"],
            profile_defaults,
        )];

        let mut profiles_file = ProfilesFile::default();
        profiles_file.profiles.insert(
            "dev".to_string(),
            ProfileDefinition {
                description: "Dev".to_string(),
                configs: vec!["apps/my-app/".to_string()],
                defaults: ProfileDefaults {
                    copy_unstaged: Some(false),
                    skip_post_setup: Some(true),
                    ..Default::default()
                },
            },
        );

        let resolved =
            resolve_profiles(&["dev".to_string()], Some(&profiles_file), &configs).unwrap();

        // Per-config overrides central for copy_unstaged
        assert_eq!(resolved.defaults.copy_unstaged, Some(true));
        // Per-config sets base_branch (central didn't)
        assert_eq!(resolved.defaults.base_branch.as_deref(), Some("develop"));
        // Central's skip_post_setup is kept (per-config didn't override it)
        assert_eq!(resolved.defaults.skip_post_setup, Some(true));
    }

    #[test]
    fn test_multiple_configs_with_profile_defaults() {
        // Two configs both declare profile_defaults for "dev".
        // Both are merged in order (later config index wins on conflict).
        let mut defaults_a = BTreeMap::new();
        defaults_a.insert(
            "dev".to_string(),
            ProfileDefaults {
                copy_unstaged: Some(true),
                base_branch: Some("develop".to_string()),
                ..Default::default()
            },
        );

        let mut defaults_b = BTreeMap::new();
        defaults_b.insert(
            "dev".to_string(),
            ProfileDefaults {
                copy_unstaged: Some(false),
                remote: Some("upstream".to_string()),
                ..Default::default()
            },
        );

        let configs = vec![
            make_loaded_config_with_profiles(
                "apps/a/worktree.config.ts",
                "app a",
                vec!["dev"],
                defaults_a,
            ),
            make_loaded_config_with_profiles(
                "apps/b/worktree.config.ts",
                "app b",
                vec!["dev"],
                defaults_b,
            ),
        ];

        let resolved = resolve_profiles(&["dev".to_string()], None, &configs).unwrap();

        assert_eq!(resolved.config_indices, vec![0, 1]);
        // Config b (later index) wins on copy_unstaged
        assert_eq!(resolved.defaults.copy_unstaged, Some(false));
        // Config a set base_branch, b didn't override it
        assert_eq!(resolved.defaults.base_branch.as_deref(), Some("develop"));
        // Config b added remote
        assert_eq!(resolved.defaults.remote.as_deref(), Some("upstream"));
    }

    #[test]
    fn test_per_config_defaults_only_for_matching_profile() {
        // Config declares profile_defaults for "ci", but we resolve "dev".
        // The "ci" defaults should NOT be applied.
        let mut profile_defaults = BTreeMap::new();
        profile_defaults.insert(
            "ci".to_string(),
            ProfileDefaults {
                skip_post_setup: Some(true),
                ..Default::default()
            },
        );

        let configs = vec![make_loaded_config_with_profiles(
            "apps/my-app/worktree.config.ts",
            "my-app",
            vec!["dev", "ci"],
            profile_defaults,
        )];

        let resolved = resolve_profiles(&["dev".to_string()], None, &configs).unwrap();

        assert_eq!(resolved.config_indices, vec![0]);
        // "ci" profile_defaults should NOT be applied when resolving "dev"
        assert_eq!(resolved.defaults.skip_post_setup, None);
    }
}
