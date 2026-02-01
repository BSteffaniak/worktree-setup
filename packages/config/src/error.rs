//! Error types for configuration loading.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::PathBuf;

use thiserror::Error;

/// Errors that can occur during configuration loading.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Failed to read configuration file.
    #[error("Failed to read config file {}: {source}", path.display())]
    ReadError {
        /// Path to the file that couldn't be read.
        path: PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse TOML configuration.
    #[error("Failed to parse TOML config {}: {source}", path.display())]
    TomlParseError {
        /// Path to the file that couldn't be parsed.
        path: PathBuf,
        /// The underlying TOML error.
        #[source]
        source: toml::de::Error,
    },

    /// Failed to parse JSON from TypeScript evaluation.
    #[error("Failed to parse JSON from TypeScript config {}: {source}", path.display())]
    JsonParseError {
        /// Path to the file that couldn't be parsed.
        path: PathBuf,
        /// The underlying JSON error.
        #[source]
        source: serde_json::Error,
    },

    /// TypeScript evaluation failed.
    #[error("TypeScript evaluation failed for {}: {message}", path.display())]
    TypeScriptEvalError {
        /// Path to the file that couldn't be evaluated.
        path: PathBuf,
        /// Error message from the subprocess.
        message: String,
    },

    /// No JavaScript runtime (bun/deno) found.
    #[error("No JavaScript runtime found. Please install bun or deno.")]
    NoJsRuntime,

    /// Unsupported configuration format.
    #[error("Unsupported config format: {0}")]
    UnsupportedFormat(String),

    /// Invalid configuration path.
    #[error("Invalid config path: {}", .0.display())]
    InvalidPath(PathBuf),

    /// IO error during config discovery.
    #[error("IO error during config discovery: {0}")]
    IoError(#[from] std::io::Error),
}
