//! Per-operation copy configuration, with backwards-compatible string shorthands.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use serde::{Deserialize, Serialize};

/// How regular file contents are transferred. Does not change overwrite policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CopyStrategy {
    /// Attempt filesystem cloning, falling back to ordinary copying.
    #[default]
    Auto,
    /// Explicit buffered reads/writes, without requesting filesystem cloning.
    ByteCopy,
}

/// A copy path, optionally specifying its transfer strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CopyEntry {
    /// Existing shorthand; uses the automatic strategy.
    Path(String),
    /// Path with explicit options.
    Options(CopyPathOptions),
}

/// Options for one path (recursively applied when it is a directory).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyPathOptions {
    /// Source path relative to the config directory.
    pub path: String,
    /// Transfer strategy, defaulting to automatic.
    #[serde(default)]
    pub strategy: CopyStrategy,
}

impl CopyEntry {
    /// Source path.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Path(path) => path,
            Self::Options(options) => &options.path,
        }
    }
    /// Transfer strategy.
    #[must_use]
    pub const fn strategy(&self) -> CopyStrategy {
        match self {
            Self::Path(_) => CopyStrategy::Auto,
            Self::Options(options) => options.strategy,
        }
    }
}

impl From<String> for CopyEntry {
    fn from(path: String) -> Self {
        Self::Path(path)
    }
}
impl From<&str> for CopyEntry {
    fn from(path: &str) -> Self {
        Self::Path(path.to_owned())
    }
}

/// A glob pattern, optionally specifying the strategy for all matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CopyGlobEntry {
    /// Existing shorthand; uses the automatic strategy.
    Pattern(String),
    /// Pattern with explicit options.
    Options(CopyGlobOptions),
}

/// Options for one glob pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyGlobOptions {
    /// Pattern relative to the config directory.
    pub pattern: String,
    /// Transfer strategy, defaulting to automatic.
    #[serde(default)]
    pub strategy: CopyStrategy,
}
impl CopyGlobEntry {
    /// Source pattern.
    #[must_use]
    pub fn pattern(&self) -> &str {
        match self {
            Self::Pattern(pattern) => pattern,
            Self::Options(options) => &options.pattern,
        }
    }
    /// Transfer strategy.
    #[must_use]
    pub const fn strategy(&self) -> CopyStrategy {
        match self {
            Self::Pattern(_) => CopyStrategy::Auto,
            Self::Options(options) => options.strategy,
        }
    }
}
impl From<String> for CopyGlobEntry {
    fn from(pattern: String) -> Self {
        Self::Pattern(pattern)
    }
}
impl From<&str> for CopyGlobEntry {
    fn from(pattern: &str) -> Self {
        Self::Pattern(pattern.to_owned())
    }
}

/// Unstaged-file copying: a boolean shorthand or enabled copying with options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CopyUnstaged {
    /// Enable/disable copying using the automatic strategy.
    Enabled(bool),
    /// Enable copying with an explicit strategy.
    Options(CopyUnstagedOptions),
}
/// Options applied to all files selected by `copyUnstaged`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyUnstagedOptions {
    /// Transfer strategy, defaulting to automatic.
    #[serde(default)]
    pub strategy: CopyStrategy,
}
impl Default for CopyUnstaged {
    fn default() -> Self {
        Self::Enabled(false)
    }
}
impl CopyUnstaged {
    /// Whether unstaged copying is enabled.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        !matches!(self, Self::Enabled(false))
    }
    /// Transfer strategy.
    #[must_use]
    pub const fn strategy(&self) -> CopyStrategy {
        match self {
            Self::Enabled(_) => CopyStrategy::Auto,
            Self::Options(options) => options.strategy,
        }
    }
}
impl From<bool> for CopyUnstaged {
    fn from(enabled: bool) -> Self {
        Self::Enabled(enabled)
    }
}
