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

    /// Resolve multiple patterns against `base_dir` using a single
    /// filesystem walk.
    ///
    /// Equivalent in result to calling [`resolve`](Self::resolve) once per
    /// pattern in order, but walks the directory tree only once for the
    /// glob patterns (batched via [`globset::GlobSet`]). Exact patterns
    /// are resolved eagerly in order before the walk.
    ///
    /// # Ordering and dedup semantics (preserved from `resolve`)
    ///
    /// * Patterns are processed in the order given
    /// * Within the walk, matches are yielded in walker order (sorted
    ///   children), biased toward shallower matches first
    /// * The stateful `seen` set means the first occurrence of a
    ///   canonical path wins — later matches that resolve to the same
    ///   path or a descendant are silently skipped
    /// * On-match pruning: a directory that matches ANY glob in the set
    ///   is yielded but not descended into
    ///
    /// Use this in preference to looping over `resolve` when you have
    /// multiple patterns against the same `base_dir`; it is typically
    /// O(patterns) times faster.
    pub fn resolve_many(&mut self, patterns: &[&str], base_dir: &Path) -> Vec<ResolvedPath> {
        let mut results = Vec::new();

        // Split into exact vs. glob, preserving relative order.
        let mut glob_patterns: Vec<&str> = Vec::new();
        for pattern in patterns {
            if is_glob_pattern(pattern) {
                glob_patterns.push(pattern);
            } else if let Some(resolved) = resolve_exact(
                pattern,
                base_dir,
                &self.containment_root,
                &mut self.seen,
                &self.options,
            ) {
                results.push(resolved);
            }
        }

        if !glob_patterns.is_empty() {
            let matched = resolve_globs_batched(
                &glob_patterns,
                base_dir,
                &self.containment_root,
                &mut self.seen,
                &self.options,
            );
            results.extend(matched);
        }

        results
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

    // Use jwalk for directory traversal.
    //
    // We use `Parallelism::Serial` here because callers frequently run many
    // `resolve_glob` invocations concurrently (e.g. one per worktree).
    // jwalk's default `Parallelism::RayonDefaultPool` has a 1-second
    // busy_timeout on rayon pool startup — when the shared rayon thread
    // pool is saturated by many simultaneous walks, most spawns time out
    // and jwalk silently returns an error (which, when ignored, looks like
    // a clean-but-empty directory). Running the inner walk serially is
    // correct and predictable; parallelism is achieved at the caller level.
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
        .parallelism(jwalk::Parallelism::Serial)
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
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(err) => {
                // Surface walker errors so silent data loss is impossible —
                // particularly the jwalk busy-timeout that previously made
                // concurrent walks return empty results.
                log::warn!(
                    "Glob walker error for pattern {pattern:?} in {}: {err}",
                    base_dir.display()
                );
                continue;
            }
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

/// Resolve multiple glob patterns using a single filesystem walk.
///
/// Equivalent to calling [`resolve_glob`] once per pattern, but walks
/// the tree only once using [`globset::GlobSet::is_match`] against the
/// compiled set. Matches are yielded in walker order (shallower first).
///
/// Dedup and descendant-skipping semantics match [`resolve_glob`]:
/// the `seen` set is consulted and updated per match.
///
/// # Arguments
///
/// * `patterns` - Glob patterns (all must be globs; invalid patterns
///   are logged and skipped).
/// * `base_dir` - Directory to walk and match against.
/// * `containment_root` - Canonical boundary path for containment checks.
/// * `seen` - Set of already-resolved canonical paths (updated in place).
/// * `options` - Resolution options.
#[allow(clippy::too_many_lines)]
pub fn resolve_globs_batched(
    patterns: &[&str],
    base_dir: &Path,
    containment_root: &Path,
    seen: &mut BTreeSet<PathBuf>,
    options: &GlobResolverOptions,
) -> Vec<ResolvedPath> {
    if patterns.is_empty() {
        return Vec::new();
    }

    // Compile patterns into a GlobSet. Invalid patterns are logged and
    // skipped (same behavior as `resolve_glob`).
    let mut builder = globset::GlobSetBuilder::new();
    let mut valid_count = 0usize;
    for pattern in patterns {
        match globset::Glob::new(pattern) {
            Ok(g) => {
                builder.add(g);
                valid_count += 1;
            }
            Err(e) => {
                log::warn!("Invalid glob pattern '{pattern}': {e}");
            }
        }
    }
    if valid_count == 0 {
        return Vec::new();
    }
    let set = match builder.build() {
        Ok(s) => s,
        Err(e) => {
            log::warn!("Could not build glob set for patterns {patterns:?}: {e}");
            return Vec::new();
        }
    };

    let skip_symlinks = options.skip_symlinks;
    let skip_dirs = options.skip_dirs.clone();
    let base_dir_owned = base_dir.to_path_buf();
    let set_for_prune = set.clone();

    // Same walker config and pruning rules as `resolve_glob`; the only
    // difference is we match against a `GlobSet` instead of a single
    // compiled matcher. `Parallelism::Serial` is non-negotiable — see the
    // comments on `resolve_glob` and `test_resolve_glob_concurrent_callers`.
    let walker = jwalk::WalkDirGeneric::<((), ())>::new(base_dir)
        .skip_hidden(false)
        .follow_links(false)
        .sort(true)
        .parallelism(jwalk::Parallelism::Serial)
        .process_read_dir(move |depth, path, _state, children| {
            children.retain_mut(|entry_result| {
                let Ok(entry) = entry_result.as_mut() else {
                    return false;
                };

                if skip_symlinks && entry.file_type.is_symlink() {
                    return false;
                }

                let name = entry.file_name.to_string_lossy();

                if entry.file_type.is_dir() {
                    // jwalk calls process_read_dir for the walk root's
                    // *parent* first (depth=None). Skip pruning there.
                    if depth.is_none() {
                        return true;
                    }

                    if skip_dirs.iter().any(|skip| name.as_ref() == skip.as_str()) {
                        return false;
                    }

                    // On-match pruning: if this directory matches ANY glob
                    // in the set, yield it but don't descend.
                    let relative = path
                        .strip_prefix(&base_dir_owned)
                        .unwrap_or(path)
                        .join(name.as_ref());
                    if set_for_prune.is_match(&relative) {
                        entry.read_children_path = None;
                    }
                }

                true
            });
        });

    let mut results = Vec::new();

    for entry_result in walker {
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(err) => {
                log::warn!(
                    "Glob walker error for patterns {patterns:?} in {}: {err}",
                    base_dir.display()
                );
                continue;
            }
        };

        if entry.depth == 0 {
            continue;
        }

        let path = entry.path();

        let Ok(relative) = path.strip_prefix(base_dir) else {
            continue;
        };

        if !set.is_match(relative) {
            continue;
        }

        let Ok(canonical) = path.canonicalize() else {
            continue;
        };

        if options.enforce_containment && !canonical.starts_with(containment_root) {
            log::warn!("Glob match escapes containment boundary, skipping: {patterns:?}");
            continue;
        }

        // Skip if inside an already-resolved path
        if seen
            .iter()
            .any(|existing| canonical.starts_with(existing) && *existing != canonical)
        {
            continue;
        }

        if !seen.insert(canonical.clone()) {
            continue;
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

    /// Sanity check on the underlying `globset` crate: `**/name` should
    /// match a bare `name` (zero-depth match) per the documented semantics
    /// ("`**/foo` matches `foo` and `bar/foo`").
    #[test]
    fn test_globset_double_star_matches_bare_name() {
        let glob = globset::Glob::new("**/node_modules")
            .unwrap()
            .compile_matcher();
        // This is the documented behavior; if it fails here, the bug is in
        // globset itself.
        assert!(
            glob.is_match("node_modules"),
            "globset `**/node_modules` should match bare `node_modules`"
        );
        assert!(glob.is_match("a/node_modules"));
        assert!(glob.is_match("a/b/node_modules"));
    }

    /// **Regression test for clean-command bug.**
    ///
    /// Worktrees with a top-level `node_modules` directory were reporting
    /// "nothing to clean" despite many GiB of content, because the glob
    /// walker yielded the top-level match at `relative = "node_modules"`
    /// (a single-component relative path) and — depending on path form —
    /// `globset::is_match` did not match it against `**/node_modules`.
    #[test]
    fn test_resolve_glob_matches_top_level_for_double_star_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Top-level node_modules (this is what was being missed)
        fs::create_dir_all(root.join("node_modules/pkg-a")).unwrap();
        // And a deep one that was being matched
        fs::create_dir_all(root.join("apps/a/node_modules")).unwrap();

        let mut r = resolver(root);
        let results = r.resolve("**/node_modules", root);

        let displays: BTreeSet<&str> = results.iter().map(|r| r.display.as_str()).collect();
        assert!(
            displays.contains("node_modules"),
            "top-level `node_modules` must be matched, got: {displays:?}"
        );
        assert!(
            displays.contains("apps/a/node_modules"),
            "deep `apps/a/node_modules` must be matched, got: {displays:?}"
        );
        assert_eq!(results.len(), 2);
    }

    /// When a top-level `node_modules` contains a flattened package tree
    /// (as produced by bun/npm with thousands of nested packages, many of
    /// which have their own empty `node_modules/`), the top-level dir
    /// must still be yielded — AND the walker must prune into it to avoid
    /// a combinatorial descent.
    #[test]
    fn test_resolve_glob_top_level_prunes_inner_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Top-level node_modules with nested package that itself has
        // node_modules inside (pnpm/bun-style nested dep).
        fs::create_dir_all(root.join("node_modules/foo/node_modules/bar")).unwrap();
        fs::create_dir_all(root.join("node_modules/baz/node_modules")).unwrap();
        fs::write(root.join("node_modules/foo/index.js"), "x").unwrap();

        let mut r = resolver(root);
        let results = r.resolve("**/node_modules", root);

        // Only the TOP-LEVEL `node_modules` should be returned — inner
        // `node_modules/foo/node_modules` etc. are descendants and must
        // not pollute the result set (they would be wastefully re-walked
        // and then deleted by the caller via the outer match anyway).
        assert_eq!(
            results.len(),
            1,
            "expected only top-level node_modules, got: {:?}",
            results.iter().map(|r| &r.display).collect::<Vec<_>>()
        );
        assert_eq!(results[0].display, "node_modules");
    }

    /// `filter_descendants` must keep the top-level match and drop deeper
    /// ones when both are present.
    #[test]
    fn test_filter_descendants_keeps_top_level_node_modules() {
        let top = ResolvedPath {
            canonical: PathBuf::from("/wt/node_modules"),
            display: "node_modules".to_string(),
        };
        let inner = ResolvedPath {
            canonical: PathBuf::from("/wt/node_modules/foo/node_modules"),
            display: "node_modules/foo/node_modules".to_string(),
        };
        let deep_outside = ResolvedPath {
            canonical: PathBuf::from("/wt/apps/a/node_modules"),
            display: "apps/a/node_modules".to_string(),
        };

        let filtered = filter_descendants(&[top, inner, deep_outside]);
        let displays: BTreeSet<&str> = filtered.iter().map(|r| r.display.as_str()).collect();
        assert!(displays.contains("node_modules"));
        assert!(displays.contains("apps/a/node_modules"));
        assert!(!displays.contains("node_modules/foo/node_modules"));
        assert_eq!(filtered.len(), 2);
    }

    /// Cross-pattern case: `**/node_modules` resolves the top-level dir;
    /// `**/.turbo` running afterwards on the same resolver must not
    /// re-walk inside the already-resolved `node_modules` subtree.
    ///
    /// This is both a correctness check (no false `**/.turbo` matches
    /// inside `node_modules/*/.turbo`) and a perf check (the walker
    /// should prune the entire subtree).
    #[test]
    fn test_resolve_cross_pattern_prunes_previously_resolved() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Top-level node_modules with a .turbo dir inside some package
        fs::create_dir_all(root.join("node_modules/some-pkg/.turbo")).unwrap();
        // A real .turbo outside node_modules that we DO want to match
        fs::create_dir_all(root.join("apps/a/.turbo")).unwrap();

        let mut r = resolver(root);
        let r1 = r.resolve("**/node_modules", root);
        assert_eq!(r1.len(), 1, "top-level node_modules should match");

        let r2 = r.resolve("**/.turbo", root);
        let displays: BTreeSet<&str> = r2.iter().map(|r| r.display.as_str()).collect();
        assert!(
            displays.contains("apps/a/.turbo"),
            "apps/a/.turbo should match, got: {displays:?}"
        );
        assert!(
            !displays.contains("node_modules/some-pkg/.turbo"),
            ".turbo inside already-resolved node_modules must not match, got: {displays:?}"
        );
    }

    /// **Regression test for the "clean says nothing to clean" bug.**
    ///
    /// When many `resolve_glob` calls run concurrently (one per worktree in
    /// multi-worktree clean), jwalk's default `Parallelism::RayonDefaultPool`
    /// has a 1-second startup timeout that was causing most concurrent walks
    /// to silently return empty results. This test reproduces the scenario:
    /// spawn many threads that each invoke the resolver against their own
    /// tempdir, and assert that every thread gets the expected match count.
    ///
    /// Each root has a moderately sized tree so the walker can't trivially
    /// finish within the 1-second rayon startup timeout when contending
    /// with other concurrent walkers. This reliably reproduces the bug on
    /// the old code (`RayonDefaultPool`) and passes on the new code
    /// (`Parallelism::Serial`).
    #[test]
    fn test_resolve_glob_concurrent_callers() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Scale the tree so walks take non-trivial time (enough to contend
        // when many threads race on the rayon pool).
        const THREADS: usize = 48;
        const APPS_PER_ROOT: usize = 30;
        const SUBDIRS_PER_APP: usize = 30;

        let tmp = tempfile::tempdir().unwrap();
        let roots: Vec<std::path::PathBuf> = (0..THREADS)
            .map(|i| {
                let root = tmp.path().join(format!("wt-{i}"));
                for j in 0..APPS_PER_ROOT {
                    let app = root.join(format!("apps/app-{j}"));
                    // Each app has its own node_modules (a match target)
                    fs::create_dir_all(app.join("node_modules")).unwrap();
                    // Plus many unrelated subdirs to give the walker work to do
                    for k in 0..SUBDIRS_PER_APP {
                        fs::create_dir_all(app.join(format!("src/dir-{k}"))).unwrap();
                        fs::write(app.join(format!("src/dir-{k}/file.txt")), "noise").unwrap();
                    }
                }
                root
            })
            .collect();

        // Run resolvers concurrently, one per root, and assert each gets
        // the expected match count.
        let failures = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for root in roots {
            let failures = failures.clone();
            handles.push(std::thread::spawn(move || {
                let canonical = root.canonicalize().unwrap();
                let mut r = GlobResolver::new(canonical, GlobResolverOptions::default());
                let results = r.resolve("**/node_modules", &root);
                if results.len() != APPS_PER_ROOT {
                    eprintln!(
                        "concurrent walker returned {} results for {}, expected {}",
                        results.len(),
                        root.display(),
                        APPS_PER_ROOT
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
            "{failed}/{THREADS} concurrent resolvers returned wrong match counts — \
             likely a regression of the jwalk busy-timeout bug"
        );
    }

    // ---- resolve_many / resolve_globs_batched tests ----

    /// `resolve_many` must produce the same set of results as calling
    /// `resolve` once per pattern in the same order. This is the core
    /// equivalence guarantee for the batched path.
    #[test]
    fn test_resolve_many_equivalent_to_sequential_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("apps/a/node_modules")).unwrap();
        fs::create_dir_all(root.join("apps/b/node_modules")).unwrap();
        fs::create_dir_all(root.join("apps/a/.turbo")).unwrap();
        fs::create_dir_all(root.join("apps/c/dist")).unwrap();
        fs::create_dir_all(root.join("packages/p/node_modules")).unwrap();

        let patterns = ["**/node_modules", "**/.turbo", "**/dist"];

        // Sequential
        let mut r_seq = resolver(root);
        let mut seq: Vec<ResolvedPath> = Vec::new();
        for p in &patterns {
            seq.extend(r_seq.resolve(p, root));
        }
        let seq_displays: BTreeSet<String> = seq.iter().map(|r| r.display.clone()).collect();

        // Batched
        let mut r_batch = resolver(root);
        let batch = r_batch.resolve_many(&patterns, root);
        let batch_displays: BTreeSet<String> = batch.iter().map(|r| r.display.clone()).collect();

        assert_eq!(
            seq_displays, batch_displays,
            "resolve_many must return the same set as sequential resolve"
        );
        assert_eq!(seq.len(), batch.len());
    }

    /// When two patterns overlap, the earlier pattern in the list wins
    /// the dedup (because `seen` is checked in iteration order). For
    /// batched globs, this is weaker: within a single batch walk, the
    /// walker's sort ordering determines which display wins for a given
    /// canonical path. The guarantee we preserve is: the SAME canonical
    /// paths are yielded, and within a single call to `resolve_many`,
    /// each canonical path appears exactly once.
    #[test]
    fn test_resolve_many_dedup_across_patterns() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::create_dir_all(root.join("apps/a/node_modules")).unwrap();

        let mut r = resolver(root);
        // Two patterns, both of which would match `node_modules` and
        // `apps/a/node_modules`.
        let results = r.resolve_many(&["**/node_modules", "**/*_modules"], root);

        let displays: BTreeSet<&str> = results.iter().map(|r| r.display.as_str()).collect();
        // Only the two distinct canonical paths should appear.
        assert!(displays.contains("node_modules"));
        assert!(displays.contains("apps/a/node_modules"));
        assert_eq!(
            results.len(),
            2,
            "each canonical path must appear exactly once"
        );
    }

    /// Descendant pruning applies across patterns in a single batched
    /// walk: if one glob matches a directory, other globs cannot match
    /// inside it.
    #[test]
    fn test_resolve_many_prunes_matched_dirs_from_any_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // `node_modules` matches pattern 1; `.turbo` inside it would
        // match pattern 2 but must be pruned.
        fs::create_dir_all(root.join("node_modules/some-pkg/.turbo")).unwrap();
        // A real `.turbo` outside must be matched.
        fs::create_dir_all(root.join("apps/a/.turbo")).unwrap();

        let mut r = resolver(root);
        let results = r.resolve_many(&["**/node_modules", "**/.turbo"], root);

        let displays: BTreeSet<&str> = results.iter().map(|r| r.display.as_str()).collect();
        assert!(displays.contains("node_modules"));
        assert!(displays.contains("apps/a/.turbo"));
        assert!(
            !displays.contains("node_modules/some-pkg/.turbo"),
            ".turbo inside matched node_modules must be pruned across patterns"
        );
    }

    /// `resolve_many` should handle a mix of exact and glob patterns.
    /// Exact patterns are resolved eagerly (in order) before the walk.
    #[test]
    fn test_resolve_many_mixed_exact_and_glob() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::create_dir_all(root.join("apps/a/dist")).unwrap();
        fs::create_dir_all(root.join("apps/b/dist")).unwrap();
        fs::write(root.join("keep.txt"), "data").unwrap();

        let mut r = resolver(root);
        let results = r.resolve_many(&["node_modules", "**/dist"], root);

        let displays: BTreeSet<&str> = results.iter().map(|r| r.display.as_str()).collect();
        assert!(displays.contains("node_modules"));
        assert!(displays.contains("apps/a/dist"));
        assert!(displays.contains("apps/b/dist"));
        assert_eq!(results.len(), 3);
    }

    /// An empty pattern slice returns no results (and does not panic).
    #[test]
    fn test_resolve_many_empty_patterns() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("node_modules")).unwrap();

        let mut r = resolver(root);
        let results = r.resolve_many(&[], root);
        assert!(results.is_empty());
    }

    /// `resolve_many` preserves the cross-call `seen` state: a path
    /// already resolved by a prior call is not resolved again.
    #[test]
    fn test_resolve_many_dedup_across_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("node_modules")).unwrap();

        let mut r = resolver(root);
        let r1 = r.resolve_many(&["**/node_modules"], root);
        assert_eq!(r1.len(), 1);

        // Second call with the same pattern should return nothing.
        let r2 = r.resolve_many(&["**/node_modules"], root);
        assert!(r2.is_empty());

        // Also a different pattern matching the same path returns nothing.
        let r3 = r.resolve_many(&["*_modules"], root);
        assert!(r3.is_empty());
    }
}
