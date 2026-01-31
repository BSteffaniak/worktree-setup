//! TypeScript configuration file loader.
//!
//! Evaluates TypeScript configuration files by spawning bun or deno.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::Path;
use std::process::Command;

use crate::error::ConfigError;
use crate::types::Config;

/// Load a TypeScript configuration file by evaluating it with bun or deno.
///
/// # Arguments
///
/// * `path` - Path to the TypeScript configuration file
///
/// # Errors
///
/// * If no JavaScript runtime (bun/deno) is found
/// * If the TypeScript evaluation fails
/// * If the JSON output cannot be parsed
pub fn load_ts_config(path: &Path) -> Result<Config, ConfigError> {
    log::debug!("Loading TypeScript config from {}", path.display());

    // Try bun first (fastest, native TS support)
    match try_load_with_bun(path) {
        Ok(config) => return Ok(config),
        Err(e) => log::debug!("bun failed: {e}"),
    }

    // Fall back to deno
    match try_load_with_deno(path) {
        Ok(config) => return Ok(config),
        Err(e) => log::debug!("deno failed: {e}"),
    }

    Err(ConfigError::NoJsRuntime)
}

/// Try to load the config using bun.
fn try_load_with_bun(path: &Path) -> Result<Config, ConfigError> {
    let path_str = path.to_string_lossy();

    // Use dynamic import and handle both default and named exports
    let script = format!(
        r#"const m = await import("file://{}"); console.log(JSON.stringify(m.default ?? m));"#,
        path_str
    );

    log::debug!("Evaluating with bun: {}", script);

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
    log::debug!("bun output: {}", stdout.trim());

    serde_json::from_str(stdout.trim()).map_err(|e| ConfigError::JsonParseError {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Try to load the config using deno.
fn try_load_with_deno(path: &Path) -> Result<Config, ConfigError> {
    let path_str = path.to_string_lossy();

    // Deno script with explicit allow flags
    let script = format!(
        r#"const m = await import("file://{}"); console.log(JSON.stringify(m.default ?? m));"#,
        path_str
    );

    log::debug!("Evaluating with deno: {}", script);

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
    log::debug!("deno output: {}", stdout.trim());

    serde_json::from_str(stdout.trim()).map_err(|e| ConfigError::JsonParseError {
        path: path.to_path_buf(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::Builder;

    #[test]
    fn test_load_ts_config_with_bun() {
        // Skip if bun is not installed
        if Command::new("bun").arg("--version").output().is_err() {
            eprintln!("Skipping test: bun not installed");
            return;
        }

        let dir = Builder::new().prefix("worktree-test").tempdir().unwrap();
        let path = dir.path().join("worktree.config.ts");

        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"
export default {{
    description: "Test TS config",
    symlinks: ["data/cache"],
    copyUnstaged: true,
}};
"#
        )
        .unwrap();

        let config = load_ts_config(&path).unwrap();

        assert_eq!(config.description, "Test TS config");
        assert_eq!(config.symlinks, vec!["data/cache"]);
        assert!(config.copy_unstaged);
    }
}
