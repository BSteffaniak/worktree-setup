//! Core glob resolution implementation.
//!
//! Uses `jwalk` for parallel directory traversal and `globset` for pattern matching.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Directories commonly skipped during filesystem traversal.
///
/// These directories tend to be very large and are almost never relevant
/// to config discovery or file operations. Callers can pass this list
/// via [`GlobResolverOptions::skip_dirs`] to avoid descending into them.
///
/// Includes:
/// * `node_modules` — npm/pnpm/bun dependency trees
/// * `.git` — git internal data
/// * `target` — Rust/Cargo build artifacts
pub const DEFAULT_SKIP_DIRS: &[&str] = &["node_modules", ".git", "target"];

/// A resolved path from glob or exact pattern matching.
#[derive(Debug, Clone)]
pub struct ResolvedPath {
    /// Canonical absolute path (used for dedup and containment).
    pub canonical: PathBuf,
    /// Display-friendly relative path (for user-facing output).
    pub display: String,
}

/// Options controlling glob resolution behavior.
#[derive(Debug, Clone)]
pub struct GlobResolverOptions {
    /// Whether to skip symlink entries during traversal (default: `true`).
    ///
    /// When `true`, symlinks are neither matched nor descended into.
    pub skip_symlinks: bool,

    /// Whether to enforce containment — reject paths outside the boundary
    /// root (default: `true`).
    ///
    /// When `true`, any resolved path that does not start with the
    /// containment root is silently skipped.
    pub enforce_containment: bool,

    /// Directory names to skip during traversal (default: empty).
    ///
    /// When a directory's name matches any entry in this list, it is
    /// pruned entirely — neither yielded nor descended into.
    ///
    /// Use [`DEFAULT_SKIP_DIRS`] for a standard set of directories
    /// to skip (e.g., `node_modules`, `.git`, `target`).
    pub skip_dirs: Vec<String>,
}

impl Default for GlobResolverOptions {
    fn default() -> Self {
        Self {
            skip_symlinks: true,
            enforce_containment: true,
            skip_dirs: Vec::new(),
        }
    }
}

/// Stateful glob resolver that tracks previously seen paths for deduplication.
///
/// Create one resolver and call [`resolve`](GlobResolver::resolve) multiple
/// times to accumulate results across multiple patterns and base directories.
/// The internal `seen` set ensures no canonical path is returned twice.
pub struct GlobResolver {
    containment_root: PathBuf,
    options: GlobResolverOptions,
    seen: BTreeSet<PathBuf>,
}

impl GlobResolver {
    /// Create a new resolver.
    ///
    /// # Arguments
    ///
    /// * `containment_root` - Canonical path used for containment checks.
    ///   All resolved paths must be descendants of this path (when
    ///   `enforce_containment` is `true`).
    /// * `options` - Resolution options.
    #[must_use]
    pub const fn new(containment_root: PathBuf, options: GlobResolverOptions) -> Self {
        Self {
            containment_root,
            options,
            seen: BTreeSet::new(),
        }
    }

    /// Resolve a pattern (exact or glob) relative to `base_dir`.
    ///
    /// Automatically dispatches to [`resolve_exact`] or [`resolve_glob`]
    /// based on whether the pattern contains glob metacharacters.
    pub fn resolve(&mut self, pattern: &str, base_dir: &Path) -> Vec<ResolvedPath> {
        if is_glob_pattern(pattern) {
            resolve_glob(
                pattern,
                base_dir,
                &self.containment_root,
                &mut self.seen,
                &self.options,
            )
        } else {
            resolve_exact(
                pattern,
                base_dir,
                &self.containment_root,
                &mut self.seen,
                &self.options,
            )
            .into_iter()
            .collect()
        }
    }

    /// Consume the resolver and return the set of canonical paths seen so far.
    ///
    /// Useful when callers need to inspect or reuse the deduplication set.
    #[must_use]
    pub fn into_seen(self) -> BTreeSet<PathBuf> {
        self.seen
    }
}

/// Check whether a pattern contains glob metacharacters (`*`, `?`, or `[`).
#[must_use]
pub fn is_glob_pattern(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

/// Filter out paths that are descendants of other paths in the list.
///
/// For example, if the list contains both `node_modules/` and
/// `node_modules/.bun/foo/dist/`, the latter is removed because it is
/// already inside the former.
#[must_use]
pub fn filter_descendants(results: &[ResolvedPath]) -> Vec<ResolvedPath> {
    results
        .iter()
        .filter(|r| {
            !results.iter().any(|other| {
                other.canonical != r.canonical && r.canonical.starts_with(&other.canonical)
            })
        })
        .cloned()
        .collect()
}

/// Resolve a single exact (non-glob) path.
///
/// Returns `None` if the path does not exist, fails canonicalization,
/// or escapes the containment root.
///
/// # Arguments
///
/// * `pattern` - The exact path pattern (no glob metacharacters).
/// * `base_dir` - Directory to resolve the pattern relative to.
/// * `containment_root` - Canonical boundary path for containment checks.
/// * `seen` - Set of already-resolved canonical paths (for dedup).
/// * `options` - Resolution options.
pub fn resolve_exact(
    pattern: &str,
    base_dir: &Path,
    containment_root: &Path,
    seen: &mut BTreeSet<PathBuf>,
    options: &GlobResolverOptions,
) -> Option<ResolvedPath> {
    let candidate = base_dir.join(pattern);

    if !candidate.exists() {
        log::debug!("Path does not exist, skipping: {}", candidate.display());
        return None;
    }

    let Ok(canonical) = candidate.canonicalize() else {
        log::warn!("Could not canonicalize path: {}", candidate.display());
        return None;
    };

    if options.enforce_containment && !canonical.starts_with(containment_root) {
        log::warn!("Path escapes containment boundary, skipping: {pattern}");
        return None;
    }

    if !seen.insert(canonical.clone()) {
        return None; // already seen
    }

    let display = canonical.strip_prefix(containment_root).map_or_else(
        |_| candidate.to_string_lossy().to_string(),
        |r| r.to_string_lossy().to_string(),
    );

    Some(ResolvedPath { canonical, display })
}

/// Resolve a glob pattern using parallel `jwalk` traversal and `globset` matching.
///
/// Walks the directory tree under `base_dir` with `follow_links(false)`.
/// Each entry's relative path is matched against the compiled glob pattern.
/// When a directory matches, it is added to results and pruned (not recursed
/// into) — this avoids walking into matched directories like `node_modules/`
/// which may contain thousands of entries.
///
/// Directories listed in [`GlobResolverOptions::skip_dirs`] are pruned
/// at the directory level and never yielded or descended into.
///
/// # Arguments
///
/// * `pattern` - The glob pattern (e.g., `"**/dist"`, `"*.log"`).
/// * `base_dir` - Directory to walk and match against.
/// * `containment_root` - Canonical boundary path for containment checks.
/// * `seen` - Set of already-resolved canonical paths (for dedup and
///   ancestor checks).
/// * `options` - Resolution options.
pub fn resolve_glob(
    pattern: &str,
    base_dir: &Path,
    containment_root: &Path,
    seen: &mut BTreeSet<PathBuf>,
    options: &GlobResolverOptions,
) -> Vec<ResolvedPath> {
    let glob = match globset::Glob::new(pattern) {
        Ok(g) => g.compile_matcher(),
        Err(e) => {
            log::warn!("Invalid glob pattern '{pattern}': {e}");
            return Vec::new();
        }
    };

    let skip_symlinks = options.skip_symlinks;
    let skip_dirs = options.skip_dirs.clone();
    let base_dir_owned = base_dir.to_path_buf();
    let glob_for_prune = glob.clone();

    // Use jwalk for parallel directory traversal.
    //
    // The `process_read_dir` callback handles three kinds of pruning:
    //
    // 1. **skip_dirs** — directories whose names match the skip list are
    //    removed from children entirely (not yielded, not descended into).
    //
    // 2. **Glob-matched directories** — directories that match the glob
    //    pattern have `read_children_path` set to `None`, so they are
    //    yielded but not descended into. This is the on-match pruning
    //    optimization that prevents walking into e.g. matched
    //    `node_modules/` trees.
    //
    // 3. **Symlinks** — when `skip_symlinks` is true, symlink entries
    //    are removed from children entirely.
    let walker = jwalk::WalkDirGeneric::<((), ())>::new(base_dir)
        .skip_hidden(false)
        .follow_links(false)
        .sort(true)
        .process_read_dir(move |depth, path, _state, children| {
            children.retain_mut(|entry_result| {
                let Ok(entry) = entry_result.as_mut() else {
                    return false;
                };

                // Skip symlinks if configured
                if skip_symlinks && entry.file_type.is_symlink() {
                    return false;
                }

                let name = entry.file_name.to_string_lossy();

                if entry.file_type.is_dir() {
                    // jwalk calls process_read_dir for the walk root's
                    // *parent* first (depth=None). In this callback `path`
                    // is the parent of the walk root and the single child
                    // IS the walk root. strip_prefix(base_dir) fails here,
                    // producing a bogus absolute relative path. Skip all
                    // pruning — we always want to descend into the root.
                    if depth.is_none() {
                        return true;
                    }

                    // Prune skip_dirs entirely (don't yield, don't descend)
                    if skip_dirs.iter().any(|skip| name.as_ref() == skip.as_str()) {
                        return false;
                    }

                    // On-match pruning: if this directory matches the glob,
                    // yield it but don't descend into it.
                    let relative = path
                        .strip_prefix(&base_dir_owned)
                        .unwrap_or(path)
                        .join(name.as_ref());
                    if glob_for_prune.is_match(&relative) {
                        entry.read_children_path = None;
                    }
                }

                true
            });
        });

    let mut results = Vec::new();

    for entry_result in walker {
        let Ok(entry) = entry_result else {
            continue;
        };

        // Skip the root directory itself (depth 0)
        if entry.depth == 0 {
            continue;
        }

        let path = entry.path();

        let Ok(relative) = path.strip_prefix(base_dir) else {
            continue;
        };

        if !glob.is_match(relative) {
            continue;
        }

        // We have a match — canonicalize and apply containment/dedup checks
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };

        if options.enforce_containment && !canonical.starts_with(containment_root) {
            log::warn!("Glob match escapes containment boundary, skipping: {pattern}");
            continue;
        }

        // Skip if this path is inside an already-resolved path
        if seen
            .iter()
            .any(|existing| canonical.starts_with(existing) && *existing != canonical)
        {
            continue;
        }

        if !seen.insert(canonical.clone()) {
            continue; // already seen
        }

        let display = canonical.strip_prefix(containment_root).map_or_else(
            |_| path.to_string_lossy().to_string(),
            |r| r.to_string_lossy().to_string(),
        );
        results.push(ResolvedPath { canonical, display });
    }

    results
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    /// Helper: create a resolver with default options against the given root.
    fn resolver(root: &Path) -> GlobResolver {
        let canonical = root.canonicalize().unwrap();
        GlobResolver::new(canonical, GlobResolverOptions::default())
    }

    #[test]
    fn test_is_glob_pattern() {
        assert!(is_glob_pattern("*.log"));
        assert!(is_glob_pattern("**/dist"));
        assert!(is_glob_pattern("foo?bar"));
        assert!(is_glob_pattern("foo[0-9]"));
        assert!(!is_glob_pattern("node_modules"));
        assert!(!is_glob_pattern(".turbo"));
        assert!(!is_glob_pattern("path/to/file"));
    }

    #[test]
    fn test_resolve_exact_existing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("node_modules")).unwrap();

        let mut r = resolver(root);
        let results = r.resolve("node_modules", root);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "node_modules");
    }

    #[test]
    fn test_resolve_exact_nonexistent_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let mut r = resolver(root);
        let results = r.resolve("nonexistent", root);
        assert!(results.is_empty());
    }

    #[test]
    fn test_resolve_exact_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("node_modules")).unwrap();

        let mut r = resolver(root);
        let r1 = r.resolve("node_modules", root);
        let r2 = r.resolve("node_modules", root);
        assert_eq!(r1.len(), 1);
        assert!(r2.is_empty()); // second call is deduped
    }

    #[test]
    fn test_resolve_exact_containment_rejects_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let inner = root.join("inner");
        fs::create_dir_all(&inner).unwrap();
        // Create a file outside `inner`
        fs::write(root.join("outside.txt"), "data").unwrap();

        let canonical_inner = inner.canonicalize().unwrap();
        let mut r = GlobResolver::new(canonical_inner, GlobResolverOptions::default());
        let results = r.resolve("../outside.txt", &inner);
        assert!(results.is_empty());
    }

    #[test]
    fn test_resolve_exact_containment_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let inner = root.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(root.join("outside.txt"), "data").unwrap();

        let canonical_inner = inner.canonicalize().unwrap();
        let options = GlobResolverOptions {
            enforce_containment: false,
            ..Default::default()
        };
        let mut r = GlobResolver::new(canonical_inner, options);
        let results = r.resolve("../outside.txt", &inner);
        assert_eq!(results.len(), 1); // allowed when containment disabled
    }

    #[test]
    fn test_resolve_glob_patterns() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("a/dist")).unwrap();
        fs::create_dir_all(root.join("b/dist")).unwrap();
        fs::create_dir_all(root.join("c/src")).unwrap();

        let mut r = resolver(root);
        let results = r.resolve("**/dist", root);
        assert_eq!(results.len(), 2);

        let displays: BTreeSet<&str> = results.iter().map(|r| r.display.as_str()).collect();
        assert!(displays.contains("a/dist"));
        assert!(displays.contains("b/dist"));
    }

    #[test]
    fn test_resolve_glob_skips_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("real/dist")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        symlink(root.join("real"), root.join("linked")).unwrap();

        let mut r = resolver(root);
        let results = r.resolve("**/dist", root);
        // Only the real directory should match, not the symlinked one
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "real/dist");
    }

    #[test]
    fn test_resolve_glob_prunes_matched_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Create node_modules with nested content
        fs::create_dir_all(root.join("node_modules/.cache/foo")).unwrap();
        fs::write(root.join("node_modules/.cache/foo/bar.js"), "x").unwrap();
        // Create another dir that should also match
        fs::create_dir_all(root.join("other_modules")).unwrap();

        let mut r = resolver(root);
        let results = r.resolve("*_modules", root);
        // Should match node_modules and other_modules, but NOT descend
        // into node_modules to find nested entries
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_resolve_glob_containment_rejects_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let inner = root.join("inner");
        fs::create_dir_all(inner.join("ok_dir")).unwrap();

        // Glob should only match within containment root
        let canonical = inner.canonicalize().unwrap();
        let mut r = GlobResolver::new(canonical, GlobResolverOptions::default());
        let results = r.resolve("*", &inner);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "ok_dir");
    }

    #[test]
    fn test_filter_descendants() {
        let parent = ResolvedPath {
            canonical: PathBuf::from("/repo/node_modules"),
            display: "node_modules".to_string(),
        };
        let child = ResolvedPath {
            canonical: PathBuf::from("/repo/node_modules/.cache/dist"),
            display: "node_modules/.cache/dist".to_string(),
        };
        let unrelated = ResolvedPath {
            canonical: PathBuf::from("/repo/packages/dist"),
            display: "packages/dist".to_string(),
        };

        let input = vec![parent, child, unrelated];
        let filtered = filter_descendants(&input);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].display, "node_modules");
        assert_eq!(filtered[1].display, "packages/dist");
    }

    #[test]
    fn test_resolve_mixed_exact_and_glob() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::create_dir_all(root.join("a/dist")).unwrap();
        fs::create_dir_all(root.join("b/dist")).unwrap();
        fs::write(root.join("keep.txt"), "data").unwrap();

        let mut r = resolver(root);
        let mut all = Vec::new();
        all.extend(r.resolve("node_modules", root));
        all.extend(r.resolve("**/dist", root));
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_resolve_dedup_across_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("shared")).unwrap();

        let mut r = resolver(root);
        let r1 = r.resolve("shared", root);
        let r2 = r.resolve("sha*", root); // glob that also matches "shared"
        assert_eq!(r1.len(), 1);
        assert!(r2.is_empty()); // deduped by the seen set
    }

    #[test]
    fn test_resolve_glob_with_skip_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Create dist directories both inside and outside node_modules
        fs::create_dir_all(root.join("src/dist")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg/dist")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();

        let canonical = root.canonicalize().unwrap();
        let options = GlobResolverOptions {
            skip_dirs: DEFAULT_SKIP_DIRS.iter().map(|&s| s.to_string()).collect(),
            ..Default::default()
        };
        let mut r = GlobResolver::new(canonical, options);
        let results = r.resolve("**/dist", root);

        // Only src/dist should match; node_modules is skipped entirely
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "src/dist");
    }

    #[test]
    fn test_default_skip_dirs_constant() {
        assert!(DEFAULT_SKIP_DIRS.contains(&"node_modules"));
        assert!(DEFAULT_SKIP_DIRS.contains(&".git"));
        assert!(DEFAULT_SKIP_DIRS.contains(&"target"));
    }
}
