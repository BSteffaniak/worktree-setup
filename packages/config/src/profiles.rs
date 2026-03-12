//! Profile resolution.
//!
//! Profiles are named presets declared in config files that control which
//! configs are auto-selected and provide defaults for operational settings.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::collections::BTreeSet;
use std::path::Path;

use crate::error::ConfigError;
use crate::types::{LoadedConfig, ResolvedProfile};

/// Resolve one or more profile names into a single `ResolvedProfile`.
///
/// Resolution scans all loaded configs for profile definitions declared
/// in their `profiles` map.
///
/// # Resolution Logic
///
/// For each profile name:
///
/// 1. Find all configs whose `profiles` map contains the name — these
///    are "declaring configs" and are always selected
/// 2. For each declaring config, resolve its `configs` glob patterns
///    to pull in additional configs
/// 3. Merge defaults from declaring configs in index order (later wins)
///
/// When multiple profiles are given, they are processed in order. Config
/// lists are unioned (deduped). For defaults, later profiles win over
/// earlier profiles.
///
/// # `configs` Pattern Resolution
///
/// * No leading `/` — glob-matched relative to the declaring config's directory
/// * Leading `/` — glob-matched relative to the repository root
///
/// The declaring config is always implicitly included regardless of patterns.
///
/// # Errors
///
/// * `ProfileNotFound` if no config declares the requested profile name
pub fn resolve_profiles(
    profile_names: &[String],
    all_configs: &[LoadedConfig],
    repo_root: &Path,
) -> Result<ResolvedProfile, ConfigError> {
    let mut resolved = ResolvedProfile {
        names: profile_names.to_vec(),
        ..Default::default()
    };

    let mut seen_indices = BTreeSet::new();

    for name in profile_names {
        // Find all configs that declare this profile
        let declaring: Vec<(usize, &LoadedConfig)> = all_configs
            .iter()
            .enumerate()
            .filter(|(_, c)| c.config.profiles.contains_key(name))
            .collect();

        if declaring.is_empty() {
            return Err(ConfigError::ProfileNotFound(name.clone()));
        }

        // Step 1: Include declaring configs + merge their defaults
        for &(idx, config) in &declaring {
            // Always include the declaring config itself
            if seen_indices.insert(idx) {
                resolved.config_indices.push(idx);
            }

            // Take description from the last declaring config that has one
            let def = &config.config.profiles[name];
            if !def.description.is_empty() {
                resolved.description.clone_from(&def.description);
            }

            // Merge defaults (later config index wins on conflict)
            resolved.defaults.merge(&def.defaults);
        }

        // Step 2: Resolve `configs` glob patterns from declaring configs
        for &(_, config) in &declaring {
            let def = &config.config.profiles[name];
            for pattern in &def.configs {
                let matched = match_config_pattern(pattern, config, all_configs, repo_root);
                for idx in matched {
                    if seen_indices.insert(idx) {
                        resolved.config_indices.push(idx);
                    }
                }
            }
        }
    }

    Ok(resolved)
}

/// Match a config pattern against the discovered config list.
///
/// * No leading `/` — relative to `declaring_config`'s directory
/// * Leading `/` — relative to the repository root
fn match_config_pattern(
    pattern: &str,
    declaring_config: &LoadedConfig,
    all_configs: &[LoadedConfig],
    repo_root: &Path,
) -> Vec<usize> {
    let (glob_pattern, use_repo_root) = pattern
        .strip_prefix('/')
        .map_or((pattern, false), |stripped| (stripped, true));

    let Ok(pat) = glob::Pattern::new(glob_pattern) else {
        log::warn!("Invalid glob pattern in configs: {pattern}");
        return Vec::new();
    };

    let match_options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    let mut results = Vec::new();

    for (idx, config) in all_configs.iter().enumerate() {
        // Build the path to match against
        let match_path = if use_repo_root {
            // Match against path relative to repo root
            config.relative_path.clone()
        } else {
            // Match against path relative to the declaring config's directory
            let declaring_dir = declaring_config
                .config_dir
                .strip_prefix(repo_root)
                .unwrap_or(&declaring_config.config_dir);
            let config_path = std::path::Path::new(&config.relative_path);
            config_path
                .strip_prefix(declaring_dir)
                .map_or(String::new(), |rel| rel.to_string_lossy().to_string())
        };

        if !match_path.is_empty() && pat.matches_with(&match_path, match_options) {
            results.push(idx);
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Config, ProfileDefaults, ProfileDefinition};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_loaded_config(relative_path: &str, description: &str) -> LoadedConfig {
        let parts: Vec<&str> = relative_path.rsplitn(2, '/').collect();
        let dir = if parts.len() == 2 { parts[1] } else { "" };
        LoadedConfig {
            config: Config {
                description: description.to_string(),
                ..Default::default()
            },
            config_path: PathBuf::from(format!("/repo/{relative_path}")),
            config_dir: PathBuf::from(format!("/repo/{dir}")),
            relative_path: relative_path.to_string(),
        }
    }

    fn make_loaded_config_with_profiles(
        relative_path: &str,
        description: &str,
        profiles: BTreeMap<String, ProfileDefinition>,
    ) -> LoadedConfig {
        let parts: Vec<&str> = relative_path.rsplitn(2, '/').collect();
        let dir = if parts.len() == 2 { parts[1] } else { "" };
        LoadedConfig {
            config: Config {
                description: description.to_string(),
                profiles,
                ..Default::default()
            },
            config_path: PathBuf::from(format!("/repo/{relative_path}")),
            config_dir: PathBuf::from(format!("/repo/{dir}")),
            relative_path: relative_path.to_string(),
        }
    }

    /// Helper to build a single-entry profiles map with no defaults.
    fn profile_membership(names: &[&str]) -> BTreeMap<String, ProfileDefinition> {
        names
            .iter()
            .map(|n| (n.to_string(), ProfileDefinition::default()))
            .collect()
    }

    /// Helper to build a profiles map with a single profile that has defaults.
    fn profile_with_defaults(
        name: &str,
        defaults: ProfileDefaults,
    ) -> BTreeMap<String, ProfileDefinition> {
        let mut map = BTreeMap::new();
        map.insert(
            name.to_string(),
            ProfileDefinition {
                defaults,
                ..Default::default()
            },
        );
        map
    }

    /// Helper to build a profiles map with configs patterns.
    fn profile_with_configs(
        name: &str,
        configs: Vec<&str>,
        defaults: ProfileDefaults,
    ) -> BTreeMap<String, ProfileDefinition> {
        let mut map = BTreeMap::new();
        map.insert(
            name.to_string(),
            ProfileDefinition {
                configs: configs.into_iter().map(String::from).collect(),
                defaults,
                ..Default::default()
            },
        );
        map
    }

    fn repo_root() -> PathBuf {
        PathBuf::from("/repo")
    }

    // ─── Basic resolution ──────────────────────────────────────────────

    #[test]
    fn test_resolve_profile_basic() {
        let configs = vec![
            make_loaded_config_with_profiles(
                "apps/my-app/worktree.config.ts",
                "my-app config",
                profile_with_defaults(
                    "my-app",
                    ProfileDefaults {
                        copy_unstaged: Some(true),
                        ..Default::default()
                    },
                ),
            ),
            make_loaded_config("apps/other/worktree.config.ts", "other config"),
        ];

        let resolved = resolve_profiles(&["my-app".to_string()], &configs, &repo_root()).unwrap();

        assert_eq!(resolved.config_indices, vec![0]);
        assert_eq!(resolved.defaults.copy_unstaged, Some(true));
    }

    #[test]
    fn test_resolve_profile_not_found() {
        let configs = vec![make_loaded_config("apps/a/worktree.config.ts", "app a")];

        let result = resolve_profiles(&["nonexistent".to_string()], &configs, &repo_root());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Profile not found: 'nonexistent'")
        );
    }

    #[test]
    fn test_resolve_profile_membership_multiple_configs() {
        // Two configs both declare the same profile
        let configs = vec![
            make_loaded_config_with_profiles(
                "apps/a/worktree.config.ts",
                "app a",
                profile_membership(&["full"]),
            ),
            make_loaded_config_with_profiles(
                "apps/b/worktree.config.ts",
                "app b",
                profile_membership(&["full", "minimal"]),
            ),
            make_loaded_config("apps/c/worktree.config.ts", "app c"),
        ];

        // "full" picks up both a and b
        let resolved = resolve_profiles(&["full".to_string()], &configs, &repo_root()).unwrap();
        assert_eq!(resolved.config_indices, vec![0, 1]);

        // "minimal" picks up only b
        let resolved = resolve_profiles(&["minimal".to_string()], &configs, &repo_root()).unwrap();
        assert_eq!(resolved.config_indices, vec![1]);
    }

    #[test]
    fn test_resolve_multiple_profiles() {
        let mut profiles_a = BTreeMap::new();
        profiles_a.insert(
            "dev".to_string(),
            ProfileDefinition {
                description: "Dev".to_string(),
                defaults: ProfileDefaults {
                    post_setup: Some(crate::types::PostSetupMode::Keyword(
                        crate::types::PostSetupKeyword::All,
                    )),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        profiles_a.insert("nix".to_string(), ProfileDefinition::default());

        let mut profiles_b = BTreeMap::new();
        profiles_b.insert(
            "nix".to_string(),
            ProfileDefinition {
                description: "Nix".to_string(),
                defaults: ProfileDefaults {
                    post_setup: Some(crate::types::PostSetupMode::Keyword(
                        crate::types::PostSetupKeyword::None,
                    )),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        profiles_b.insert("dev".to_string(), ProfileDefinition::default());

        let configs = vec![
            make_loaded_config_with_profiles(
                "apps/my-app/worktree.config.ts",
                "main config",
                profiles_a,
            ),
            make_loaded_config_with_profiles(
                "apps/my-app/worktree.nix.config.ts",
                "nix config",
                profiles_b,
            ),
        ];

        let resolved = resolve_profiles(
            &["dev".to_string(), "nix".to_string()],
            &configs,
            &repo_root(),
        )
        .unwrap();

        // Both configs selected (both declare both profiles)
        assert_eq!(resolved.config_indices, vec![0, 1]);
        // Later profile's description wins
        assert_eq!(resolved.description, "Nix");
        // Later profile's defaults win (nix overrides dev's post_setup)
        assert_eq!(
            resolved.defaults.post_setup,
            Some(crate::types::PostSetupMode::Keyword(
                crate::types::PostSetupKeyword::None
            ))
        );
    }

    // ─── Defaults merging ──────────────────────────────────────────────

    #[test]
    fn test_defaults_merge_later_config_wins() {
        // Two configs define the same profile with different defaults.
        // Later config (higher index) wins on conflict.
        let configs = vec![
            make_loaded_config_with_profiles(
                "apps/a/worktree.config.ts",
                "app a",
                profile_with_defaults(
                    "dev",
                    ProfileDefaults {
                        copy_unstaged: Some(true),
                        base_branch: Some("develop".to_string()),
                        ..Default::default()
                    },
                ),
            ),
            make_loaded_config_with_profiles(
                "apps/b/worktree.config.ts",
                "app b",
                profile_with_defaults(
                    "dev",
                    ProfileDefaults {
                        copy_unstaged: Some(false),
                        remote: Some("upstream".to_string()),
                        ..Default::default()
                    },
                ),
            ),
        ];

        let resolved = resolve_profiles(&["dev".to_string()], &configs, &repo_root()).unwrap();

        assert_eq!(resolved.config_indices, vec![0, 1]);
        // Config b wins on copy_unstaged
        assert_eq!(resolved.defaults.copy_unstaged, Some(false));
        // Config a set base_branch, b didn't override it
        assert_eq!(resolved.defaults.base_branch.as_deref(), Some("develop"));
        // Config b added remote
        assert_eq!(resolved.defaults.remote.as_deref(), Some("upstream"));
    }

    #[test]
    fn test_defaults_from_non_matching_profile_not_applied() {
        // Config declares two profiles with different defaults.
        // Resolving one profile should not apply the other's defaults.
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "dev".to_string(),
            ProfileDefinition {
                defaults: ProfileDefaults {
                    copy_unstaged: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        profiles.insert(
            "ci".to_string(),
            ProfileDefinition {
                defaults: ProfileDefaults {
                    post_setup: Some(crate::types::PostSetupMode::Keyword(
                        crate::types::PostSetupKeyword::None,
                    )),
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let configs = vec![make_loaded_config_with_profiles(
            "apps/my-app/worktree.config.ts",
            "my-app",
            profiles,
        )];

        let resolved = resolve_profiles(&["dev".to_string()], &configs, &repo_root()).unwrap();

        assert_eq!(resolved.config_indices, vec![0]);
        assert_eq!(resolved.defaults.copy_unstaged, Some(true));
        // "ci" defaults should NOT be applied
        assert_eq!(resolved.defaults.post_setup, None);
    }

    // ─── ProfileDefaults::merge() unit tests ───────────────────────────

    #[test]
    fn test_profile_defaults_merge() {
        let mut base = ProfileDefaults {
            post_setup: Some(crate::types::PostSetupMode::Keyword(
                crate::types::PostSetupKeyword::All,
            )),
            copy_unstaged: Some(true),
            base_branch: Some("master".to_string()),
            ..Default::default()
        };

        let overlay = ProfileDefaults {
            post_setup: Some(crate::types::PostSetupMode::Keyword(
                crate::types::PostSetupKeyword::None,
            )),
            remote: Some("upstream".to_string()),
            creation_method: Some(crate::types::CreationMethod::Remote),
            ..Default::default()
        };

        base.merge(&overlay);

        assert_eq!(
            base.post_setup,
            Some(crate::types::PostSetupMode::Keyword(
                crate::types::PostSetupKeyword::None
            ))
        ); // overridden
        assert_eq!(base.copy_unstaged, Some(true)); // kept
        assert_eq!(base.base_branch.as_deref(), Some("master")); // kept
        assert_eq!(base.remote.as_deref(), Some("upstream")); // new
        assert_eq!(base.new_branch, None); // still None
        assert_eq!(
            base.creation_method,
            Some(crate::types::CreationMethod::Remote)
        ); // new
    }

    #[test]
    fn test_profile_defaults_merge_skip_post_setup_vec() {
        // Non-empty overlay wins over empty base
        let mut base = ProfileDefaults::default();
        let overlay = ProfileDefaults {
            skip_post_setup: vec!["bun install".to_string()],
            ..Default::default()
        };
        base.merge(&overlay);
        assert_eq!(base.skip_post_setup, vec!["bun install".to_string()]);

        // Non-empty base kept when overlay is empty
        let mut base = ProfileDefaults {
            skip_post_setup: vec!["bun install".to_string()],
            ..Default::default()
        };
        let overlay = ProfileDefaults::default();
        base.merge(&overlay);
        assert_eq!(base.skip_post_setup, vec!["bun install".to_string()]);

        // Non-empty overlay replaces non-empty base
        let mut base = ProfileDefaults {
            skip_post_setup: vec!["bun install".to_string()],
            ..Default::default()
        };
        let overlay = ProfileDefaults {
            skip_post_setup: vec!["bun generate".to_string(), "bun build".to_string()],
            ..Default::default()
        };
        base.merge(&overlay);
        assert_eq!(
            base.skip_post_setup,
            vec!["bun generate".to_string(), "bun build".to_string()]
        );
    }

    #[test]
    fn test_profile_defaults_merge_auto_create() {
        let mut base = ProfileDefaults {
            auto_create: Some(false),
            ..Default::default()
        };
        let overlay = ProfileDefaults {
            auto_create: Some(true),
            ..Default::default()
        };
        base.merge(&overlay);
        assert_eq!(base.auto_create, Some(true));

        let mut base = ProfileDefaults {
            auto_create: Some(true),
            ..Default::default()
        };
        let overlay = ProfileDefaults::default();
        base.merge(&overlay);
        assert_eq!(base.auto_create, Some(true));
    }

    #[test]
    fn test_profile_defaults_merge_overwrite_existing() {
        let mut base = ProfileDefaults {
            overwrite_existing: Some(false),
            ..Default::default()
        };
        let overlay = ProfileDefaults {
            overwrite_existing: Some(true),
            ..Default::default()
        };
        base.merge(&overlay);
        assert_eq!(base.overwrite_existing, Some(true));

        let mut base = ProfileDefaults {
            overwrite_existing: Some(true),
            ..Default::default()
        };
        let overlay = ProfileDefaults::default();
        base.merge(&overlay);
        assert_eq!(base.overwrite_existing, Some(true));
    }

    #[test]
    fn test_profile_defaults_merge_new_branch() {
        let mut base = ProfileDefaults::default();
        let overlay = ProfileDefaults {
            new_branch: Some(true),
            ..Default::default()
        };
        base.merge(&overlay);
        assert_eq!(base.new_branch, Some(true));

        let mut base = ProfileDefaults {
            new_branch: Some(true),
            ..Default::default()
        };
        let overlay = ProfileDefaults::default();
        base.merge(&overlay);
        assert_eq!(base.new_branch, Some(true));
    }

    #[test]
    fn test_profile_defaults_merge_creation_method_variants() {
        let mut base = ProfileDefaults {
            creation_method: Some(crate::types::CreationMethod::Auto),
            ..Default::default()
        };
        let overlay = ProfileDefaults {
            creation_method: Some(crate::types::CreationMethod::Detach),
            ..Default::default()
        };
        base.merge(&overlay);
        assert_eq!(
            base.creation_method,
            Some(crate::types::CreationMethod::Detach)
        );

        let mut base = ProfileDefaults {
            creation_method: Some(crate::types::CreationMethod::Current),
            ..Default::default()
        };
        let overlay = ProfileDefaults::default();
        base.merge(&overlay);
        assert_eq!(
            base.creation_method,
            Some(crate::types::CreationMethod::Current)
        );
    }

    #[test]
    fn test_profile_defaults_merge_all_default_is_noop() {
        let mut base = ProfileDefaults {
            copy_unstaged: Some(true),
            overwrite_existing: Some(false),
            auto_create: Some(true),
            creation_method: Some(crate::types::CreationMethod::Remote),
            base_branch: Some("master".to_string()),
            new_branch: Some(true),
            remote: Some("origin".to_string()),
            post_setup: Some(crate::types::PostSetupMode::Keyword(
                crate::types::PostSetupKeyword::All,
            )),
            skip_post_setup: vec!["bun generate".to_string()],
        };

        let original_skip = base.skip_post_setup.clone();
        let overlay = ProfileDefaults::default();
        base.merge(&overlay);

        assert_eq!(base.copy_unstaged, Some(true));
        assert_eq!(base.overwrite_existing, Some(false));
        assert_eq!(base.auto_create, Some(true));
        assert_eq!(
            base.creation_method,
            Some(crate::types::CreationMethod::Remote)
        );
        assert_eq!(base.base_branch.as_deref(), Some("master"));
        assert_eq!(base.new_branch, Some(true));
        assert_eq!(base.remote.as_deref(), Some("origin"));
        assert_eq!(
            base.post_setup,
            Some(crate::types::PostSetupMode::Keyword(
                crate::types::PostSetupKeyword::All
            ))
        );
        assert_eq!(base.skip_post_setup, original_skip);
    }

    // ─── Config pattern matching (glob) ────────────────────────────────

    #[test]
    fn test_configs_pattern_relative_exact() {
        // Pattern without "/" matches relative to declaring config's directory
        let declaring = make_loaded_config_with_profiles(
            "apps/my-app/worktree.config.ts",
            "main",
            profile_with_configs(
                "dev",
                vec!["worktree.local.config.ts"],
                ProfileDefaults::default(),
            ),
        );

        let configs = vec![
            declaring,
            make_loaded_config("apps/my-app/worktree.local.config.ts", "local"),
            make_loaded_config("apps/other/worktree.local.config.ts", "other local"),
        ];

        let resolved = resolve_profiles(&["dev".to_string()], &configs, &repo_root()).unwrap();

        // Should select: config 0 (declaring) + config 1 (pattern match)
        // Config 2 is in a different directory, should NOT match
        assert_eq!(resolved.config_indices, vec![0, 1]);
    }

    #[test]
    fn test_configs_pattern_relative_glob() {
        // Glob pattern matches siblings
        let declaring = make_loaded_config_with_profiles(
            "apps/my-app/worktree.config.ts",
            "main",
            profile_with_configs("dev", vec!["*.config.ts"], ProfileDefaults::default()),
        );

        let configs = vec![
            declaring,
            make_loaded_config("apps/my-app/worktree.local.config.ts", "local"),
            make_loaded_config("apps/my-app/worktree.nix.config.ts", "nix"),
            make_loaded_config("apps/other/worktree.config.ts", "other"),
        ];

        let resolved = resolve_profiles(&["dev".to_string()], &configs, &repo_root()).unwrap();

        // All configs in apps/my-app/ match, but not apps/other/
        assert_eq!(resolved.config_indices, vec![0, 1, 2]);
    }

    #[test]
    fn test_configs_pattern_repo_root() {
        // Leading "/" matches relative to repo root
        let declaring = make_loaded_config_with_profiles(
            "apps/my-app/worktree.config.ts",
            "main",
            profile_with_configs(
                "dev",
                vec!["/libs/shared/*.config.ts"],
                ProfileDefaults::default(),
            ),
        );

        let configs = vec![
            declaring,
            make_loaded_config("libs/shared/worktree.config.ts", "shared"),
            make_loaded_config("libs/shared/worktree.extra.config.ts", "extra"),
            make_loaded_config("libs/other/worktree.config.ts", "other"),
        ];

        let resolved = resolve_profiles(&["dev".to_string()], &configs, &repo_root()).unwrap();

        // Config 0 (declaring) + configs 1, 2 (repo-root pattern match)
        // Config 3 is in libs/other/, should NOT match
        assert_eq!(resolved.config_indices, vec![0, 1, 2]);
    }

    #[test]
    fn test_configs_pattern_repo_root_recursive() {
        // "/**/" pattern for recursive matching
        let declaring = make_loaded_config_with_profiles(
            "apps/my-app/worktree.config.ts",
            "main",
            profile_with_configs(
                "full",
                vec!["/apps/**/*.config.ts"],
                ProfileDefaults::default(),
            ),
        );

        let configs = vec![
            declaring,
            make_loaded_config("apps/my-app/worktree.local.config.ts", "local"),
            make_loaded_config("apps/other/worktree.config.ts", "other"),
            make_loaded_config("libs/shared/worktree.config.ts", "shared"),
        ];

        let resolved = resolve_profiles(&["full".to_string()], &configs, &repo_root()).unwrap();

        // Config 0 (declaring), 1, 2 match /apps/**/*.config.ts
        // Config 3 is under libs/, should NOT match
        assert_eq!(resolved.config_indices, vec![0, 1, 2]);
    }

    #[test]
    fn test_configs_pattern_combined_with_defaults() {
        // A profile that pulls in extra configs AND has defaults
        let declaring = make_loaded_config_with_profiles(
            "apps/my-app/worktree.config.ts",
            "main",
            profile_with_configs(
                "local",
                vec!["worktree.local.config.ts"],
                ProfileDefaults {
                    auto_create: Some(true),
                    creation_method: Some(crate::types::CreationMethod::Auto),
                    ..Default::default()
                },
            ),
        );

        let configs = vec![
            declaring,
            make_loaded_config("apps/my-app/worktree.local.config.ts", "local"),
        ];

        let resolved = resolve_profiles(&["local".to_string()], &configs, &repo_root()).unwrap();

        assert_eq!(resolved.config_indices, vec![0, 1]);
        assert_eq!(resolved.defaults.auto_create, Some(true));
        assert_eq!(
            resolved.defaults.creation_method,
            Some(crate::types::CreationMethod::Auto)
        );
    }
}
