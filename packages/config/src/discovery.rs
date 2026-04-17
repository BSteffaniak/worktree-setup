//! Configuration file discovery.
//!
//! Discovers worktree configuration files in a repository using fast parallel
//! filesystem traversal.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::{Path, PathBuf};

use worktree_setup_glob::DEFAULT_SKIP_DIRS;

use crate::error::ConfigError;
use crate::types::LoadedConfig;

/// Discover all worktree configuration files in a repository.
///
/// Searches for files matching `worktree.config.{toml,ts}` and
/// `worktree.*.config.{toml,ts}` patterns using fast parallel directory traversal.
///
/// Automatically prunes directories listed in [`DEFAULT_SKIP_DIRS`]
/// (`node_modules`, `.git`, `target`) at the directory level so their
/// contents are never traversed.
///
/// # Arguments
///
/// * `repo_root` - Path to the repository root
///
/// # Errors
///
/// * If the directory cannot be read
pub fn discover_configs(repo_root: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    log::debug!("Discovering configs in {}", repo_root.display());

    let mut configs: Vec<PathBuf> = jwalk::WalkDirGeneric::<((), ())>::new(repo_root)
        .skip_hidden(false)
        .sort(false)
        // `Parallelism::Serial` is intentional here: jwalk's default
        // `RayonDefaultPool` has a 1-second busy_timeout that causes
        // silent `Err(Error::busy())` returns when the shared rayon pool
        // is saturated by concurrent callers (see
        // `test_resolve_glob_concurrent_callers` in
        // `worktree_setup_glob`). This function is currently called once
        // per process, but choosing serial here guards against future
        // concurrent use and keeps our jwalk parallelism policy
        // consistent across the workspace.
        .parallelism(jwalk::Parallelism::Serial)
        .process_read_dir(|_depth, _path, _state, children| {
            // Prune directories we never want to enter.  Setting
            // `read_children_path = None` prevents jwalk from descending.
            // Removing entries entirely prevents them from appearing in
            // the iterator output at all.
            children.retain(|entry_result| {
                let Ok(entry) = entry_result.as_ref() else {
                    return false;
                };
                if entry.file_type.is_dir() {
                    let name = entry.file_name.to_string_lossy();
                    if DEFAULT_SKIP_DIRS.iter().any(|&skip| name == skip) {
                        return false;
                    }
                }
                true
            });
        })
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            // Only files
            if !entry.file_type().is_file() {
                return false;
            }

            // Match worktree.config.{toml,ts} or worktree.*.config.{toml,ts}
            let name = entry.file_name().to_string_lossy();
            name.starts_with("worktree")
                && name.contains(".config.")
                && (name.ends_with(".toml") || name.ends_with(".ts"))
        })
        .map(|entry| entry.path())
        .collect();

    configs.sort();
    log::debug!("Found {} config files", configs.len());

    Ok(configs)
}

/// Get a display name for a loaded configuration.
///
/// Returns a short, human-readable name based on the config's directory.
#[must_use]
pub fn get_config_display_name(config: &LoadedConfig) -> String {
    // Try to get the parent directory name
    if let Some(parent) = config.config_dir.file_name() {
        let parent_name = parent.to_string_lossy();
        // If parent is not a common name, use it
        if parent_name != "." && parent_name != ".." {
            return parent_name.to_string();
        }
    }

    // Fall back to relative path
    config.relative_path.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_config_display_name() {
        let config = LoadedConfig {
            config: crate::types::Config::default(),
            config_path: PathBuf::from("/repo/apps/my-app/worktree.config.toml"),
            config_dir: PathBuf::from("/repo/apps/my-app"),
            relative_path: "apps/my-app/worktree.config.toml".to_string(),
        };

        assert_eq!(get_config_display_name(&config), "my-app");
    }

    /// **Regression test for jwalk busy-timeout bug**, analogous to
    /// `test_resolve_glob_concurrent_callers` in the `glob` crate.
    ///
    /// When many `discover_configs` calls run concurrently, they must
    /// all produce correct results regardless of rayon pool contention.
    /// `Parallelism::Serial` in `discover_configs` makes this trivial;
    /// this test guards against any future revert.
    #[test]
    fn test_discover_configs_concurrent_callers() {
        use std::fs;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Moderate-sized tree with configs scattered across subdirs so
        // each walk does non-trivial work.
        const THREADS: usize = 48;
        const APPS_PER_ROOT: usize = 20;

        let tmp = tempfile::tempdir().unwrap();
        let roots: Vec<PathBuf> = (0..THREADS)
            .map(|i| {
                let root = tmp.path().join(format!("repo-{i}"));
                for j in 0..APPS_PER_ROOT {
                    let app = root.join(format!("apps/app-{j}"));
                    fs::create_dir_all(&app).unwrap();
                    // One config per app (TOML) and some noise files.
                    fs::write(app.join("worktree.config.toml"), "clean = []\n").unwrap();
                    fs::write(app.join("package.json"), "{}").unwrap();
                    fs::write(app.join("README.md"), "x").unwrap();
                    for k in 0..10 {
                        let sub = app.join(format!("src/dir-{k}"));
                        fs::create_dir_all(&sub).unwrap();
                        fs::write(sub.join("file.rs"), "fn main() {}").unwrap();
                    }
                }
                root
            })
            .collect();

        let failures = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for root in roots {
            let failures = failures.clone();
            handles.push(std::thread::spawn(move || {
                let found = discover_configs(&root).unwrap();
                if found.len() != APPS_PER_ROOT {
                    eprintln!(
                        "concurrent discover_configs returned {} configs for {}, expected {}",
                        found.len(),
                        root.display(),
                        APPS_PER_ROOT,
                    );
                    failures.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let failed = failures.load(Ordering::Relaxed);
        assert_eq!(
            failed, 0,
            "{failed}/{THREADS} concurrent discover_configs calls returned wrong counts — \
             likely a regression of the jwalk busy-timeout bug"
        );
    }
}
