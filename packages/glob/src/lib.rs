//! Glob pattern resolution with parallel `jwalk` + `globset` traversal.
//!
//! Provides a unified file-searching architecture for worktree-setup. All glob
//! pattern resolution goes through this crate, ensuring consistent behavior:
//!
//! * Symlinks are never followed or descended into
//! * Matched directories are pruned (not recursed into)
//! * Resolved paths are containment-checked against a boundary root
//! * Results are deduplicated by canonical path
//! * Configurable directory skipping (e.g., `node_modules`, `.git`, `target`)
//!
//! # Examples
//!
//! ```no_run
//! use worktree_setup_glob::{GlobResolver, GlobResolverOptions};
//! use std::path::PathBuf;
//!
//! let root = PathBuf::from("/repo/worktree");
//! let canonical = root.canonicalize().unwrap();
//! let options = GlobResolverOptions::default();
//! let mut resolver = GlobResolver::new(canonical, options);
//!
//! let results = resolver.resolve("**/dist", &root);
//! for r in &results {
//!     println!("{}", r.display);
//! }
//! ```

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

mod resolve;

pub use resolve::{
    DEFAULT_SKIP_DIRS, GlobResolver, GlobResolverOptions, ResolvedPath, filter_descendants,
    is_glob_pattern, resolve_exact, resolve_glob,
};
