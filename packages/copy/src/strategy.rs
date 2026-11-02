//! Transfer policies independent of configuration and command-line parsing.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

/// How regular file contents are copied; unrelated to destination overwrite policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CopyStrategy {
    /// Request a clone when possible, otherwise use ordinary copying.
    #[default]
    Auto,
    /// Use bounded buffered reads/writes, never requesting filesystem cloning.
    /// Transparent storage-layer deduplication cannot be prevented.
    ByteCopy,
}
