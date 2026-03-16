//! TypeScript configuration file loader.
//!
//! Evaluates TypeScript configuration files using an embedded pipeline:
//!
//! 1. **SWC** strips TypeScript types → plain JavaScript (`StripOnly` mode)
//! 2. **`QuickJS`** (via `rquickjs`) evaluates the JS and extracts `export default`
//!
//! This avoids spawning external subprocesses (`bun`/`deno`) for each config
//! file, reducing N × 100–150 ms to a single ~5–10 ms in-process evaluation.
//!
//! Falls back to `bun`/`deno` subprocesses if embedded evaluation fails
//! (e.g., the config uses features unsupported by `QuickJS`).

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rquickjs::loader::{Loader, Resolver};
use rquickjs::{Context, Ctx, Module, Runtime, Value};

use crate::error::ConfigError;
use crate::types::Config;

// ─── Public API ─────────────────────────────────────────────────────────────

/// Load a TypeScript configuration file.
///
/// Tries the embedded SWC + `QuickJS` pipeline first, falling back to
/// `bun`/`deno` subprocesses if the embedded approach fails.
///
/// # Arguments
///
/// * `path` - Path to the TypeScript configuration file
///
/// # Errors
///
/// * If no evaluation method succeeds
/// * If the output cannot be parsed as a `Config`
pub fn load_ts_config(path: &Path) -> Result<Config, ConfigError> {
    log::debug!("Loading TypeScript config from {}", path.display());

    // Try embedded SWC + QuickJS first (fast path)
    match load_ts_embedded(path) {
        Ok(config) => return Ok(config),
        Err(e) => log::debug!("Embedded TS eval failed, falling back to subprocess: {e}"),
    }

    // Fall back to bun
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

// ─── Embedded SWC + QuickJS pipeline ────────────────────────────────────────

/// Strip TypeScript types from source code using SWC's `StripOnly` mode.
///
/// This replaces TypeScript-specific syntax (type annotations, `as const`,
/// `import type`, interfaces, type aliases, etc.) with whitespace, preserving
/// byte offsets. The result is valid JavaScript.
fn strip_ts_types(source: &str) -> Result<String, ConfigError> {
    use swc_common::SourceMap;
    use swc_common::errors::{HANDLER, Handler};
    use swc_common::sync::Lrc;
    use swc_ts_fast_strip::{Mode, Options};

    let cm = Lrc::new(SourceMap::default());

    // Create a handler that discards diagnostics (we only care about errors)
    let handler = Handler::with_emitter(false, false, Box::new(SilentEmitter));

    let result = HANDLER.set(&handler, || {
        swc_ts_fast_strip::operate(
            &cm,
            &handler,
            source.to_string(),
            Options {
                mode: Mode::StripOnly,
                ..Default::default()
            },
        )
    });

    match result {
        Ok(output) => Ok(output.code),
        Err(e) => Err(ConfigError::TypeScriptEvalError {
            path: PathBuf::new(),
            message: format!("SWC type stripping failed: {e}"),
        }),
    }
}

/// A silent SWC diagnostic emitter that discards all output.
struct SilentEmitter;

impl swc_common::errors::Emitter for SilentEmitter {
    fn emit(&mut self, _: &mut swc_common::errors::DiagnosticBuilder<'_>) {}
}

/// Custom module resolver for TypeScript config files.
///
/// Resolves relative imports by looking for files with `.ts`, `.js`, or
/// no extension in the importing module's directory.
struct TsResolver;

impl Resolver for TsResolver {
    fn resolve(&mut self, _ctx: &Ctx<'_>, base: &str, name: &str) -> rquickjs::Result<String> {
        let base_dir = Path::new(base).parent().unwrap_or_else(|| Path::new("."));

        let candidate = base_dir.join(name);

        // Try exact path first, then with extensions
        let extensions = ["", ".ts", ".js", "/index.ts", "/index.js"];
        for ext in &extensions {
            let path = if ext.is_empty() {
                candidate.clone()
            } else {
                PathBuf::from(format!("{}{ext}", candidate.display()))
            };
            if path.is_file() {
                // Return the canonical path as the module name
                return path
                    .canonicalize()
                    .map(|p| p.to_string_lossy().into_owned())
                    .map_err(|_| rquickjs::Error::new_resolving(base, name));
            }
        }

        Err(rquickjs::Error::new_resolving_message(
            base,
            name,
            "module not found",
        ))
    }
}

/// Custom module loader that reads files from disk and strips TypeScript.
struct TsLoader;

impl Loader for TsLoader {
    fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str) -> rquickjs::Result<Module<'js>> {
        let path = Path::new(name);
        let source =
            std::fs::read_to_string(path).map_err(|_| rquickjs::Error::new_loading(name))?;

        let js_source = if std::path::Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
        {
            strip_ts_types(&source)
                .map_err(|e| rquickjs::Error::new_loading_message(name, e.to_string()))?
        } else {
            source
        };

        Module::declare(ctx.clone(), name, js_source)
            .map_err(|e| rquickjs::Error::new_loading_message(name, e.to_string()))
    }
}

/// Load a TypeScript config using the embedded SWC + `QuickJS` pipeline.
fn load_ts_embedded(path: &Path) -> Result<Config, ConfigError> {
    // Canonicalize the entry path so relative imports resolve correctly
    let canonical_path = path
        .canonicalize()
        .map_err(|e| ConfigError::TypeScriptEvalError {
            path: path.to_path_buf(),
            message: format!("Failed to canonicalize path: {e}"),
        })?;
    let module_name = canonical_path.to_string_lossy().to_string();

    // Read and strip the entry file
    let ts_source =
        std::fs::read_to_string(&canonical_path).map_err(|e| ConfigError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;
    let js_source = strip_ts_types(&ts_source).map_err(|e| ConfigError::TypeScriptEvalError {
        path: path.to_path_buf(),
        message: format!("{e}"),
    })?;

    // Create QuickJS runtime with custom loader for imports
    let rt = Runtime::new().map_err(|e| ConfigError::TypeScriptEvalError {
        path: path.to_path_buf(),
        message: format!("Failed to create JS runtime: {e}"),
    })?;
    rt.set_loader(TsResolver, TsLoader);

    let ctx = Context::full(&rt).map_err(|e| ConfigError::TypeScriptEvalError {
        path: path.to_path_buf(),
        message: format!("Failed to create JS context: {e}"),
    })?;

    ctx.with(|ctx| {
        // Declare and evaluate the module
        let module =
            Module::declare(ctx.clone(), module_name.as_str(), js_source).map_err(|e| {
                ConfigError::TypeScriptEvalError {
                    path: path.to_path_buf(),
                    message: format!("Failed to parse JS module: {e}"),
                }
            })?;

        let (module, promise) = module
            .eval()
            .map_err(|e| ConfigError::TypeScriptEvalError {
                path: path.to_path_buf(),
                message: format!("Failed to evaluate JS module: {e}"),
            })?;

        // Drive the job queue to completion
        promise
            .finish::<()>()
            .map_err(|e| ConfigError::TypeScriptEvalError {
                path: path.to_path_buf(),
                message: format!("Module evaluation failed: {e}"),
            })?;

        // Extract the default export
        let default_val: Value =
            module
                .get("default")
                .map_err(|e| ConfigError::TypeScriptEvalError {
                    path: path.to_path_buf(),
                    message: format!("No default export found: {e}"),
                })?;

        // Serialize to JSON via QuickJS, then deserialize with serde
        let json_string = ctx
            .json_stringify(default_val)
            .map_err(|e| ConfigError::TypeScriptEvalError {
                path: path.to_path_buf(),
                message: format!("Failed to stringify default export: {e}"),
            })?
            .ok_or_else(|| ConfigError::TypeScriptEvalError {
                path: path.to_path_buf(),
                message: "Default export is undefined".to_string(),
            })?
            .to_string()
            .map_err(|e| ConfigError::TypeScriptEvalError {
                path: path.to_path_buf(),
                message: format!("Failed to convert JSON to string: {e}"),
            })?;

        log::debug!("Embedded eval output: {json_string}");

        serde_json::from_str(&json_string).map_err(|e| ConfigError::JsonParseError {
            path: path.to_path_buf(),
            source: e,
        })
    })
}

// ─── Subprocess fallbacks ───────────────────────────────────────────────────

/// Try to load the config using bun.
fn try_load_with_bun(path: &Path) -> Result<Config, ConfigError> {
    let path_str = path.to_string_lossy();

    // Use dynamic import and handle both default and named exports
    let script = format!(
        r#"const m = await import("file://{path_str}"); console.log(JSON.stringify(m.default ?? m));"#
    );

    log::debug!("Evaluating with bun: {script}");

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
        r#"const m = await import("file://{path_str}"); console.log(JSON.stringify(m.default ?? m));"#
    );

    log::debug!("Evaluating with deno: {script}");

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
    fn test_strip_ts_types_basic() {
        let ts = r#"
const x: string = "hello";
export default { description: "test" as const };
"#;
        let js = strip_ts_types(ts).unwrap();
        // Type annotations should be stripped
        assert!(!js.contains(": string"));
        assert!(!js.contains("as const"));
        // Values should remain
        assert!(js.contains("\"hello\""));
        assert!(js.contains("\"test\""));
    }

    #[test]
    fn test_strip_ts_types_import_type() {
        let ts = r#"
import type { Config } from "worktree-setup";
export default { description: "test" };
"#;
        let js = strip_ts_types(ts).unwrap();
        // import type should be removed
        assert!(!js.contains("import type"));
        // export default should remain
        assert!(js.contains("export default"));
    }

    #[test]
    fn test_load_ts_embedded_simple() {
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

        let config = load_ts_embedded(&path).unwrap();
        assert_eq!(config.description, "Test TS config");
        assert_eq!(config.symlinks, vec!["data/cache"]);
        assert!(config.copy_unstaged);
    }

    #[test]
    fn test_load_ts_embedded_with_type_annotations() {
        let dir = Builder::new().prefix("worktree-test").tempdir().unwrap();
        let path = dir.path().join("worktree.config.ts");

        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"
interface Config {{
    description: string;
    symlinks: string[];
}}

const config: Config = {{
    description: "Typed config",
    symlinks: ["lib"],
}};

export default config;
"#
        )
        .unwrap();

        let config = load_ts_embedded(&path).unwrap();
        assert_eq!(config.description, "Typed config");
        assert_eq!(config.symlinks, vec!["lib"]);
    }

    #[test]
    fn test_load_ts_embedded_with_import() {
        let dir = Builder::new().prefix("worktree-test").tempdir().unwrap();

        // Create a base module
        let base_path = dir.path().join("base.ts");
        let mut base_file = std::fs::File::create(&base_path).unwrap();
        writeln!(
            base_file,
            r#"
export const baseSymlinks: string[] = ["data/cache"];
export const baseDescription: string = "Base config";
"#
        )
        .unwrap();

        // Create the main config that imports from base
        let config_path = dir.path().join("worktree.config.ts");
        let mut config_file = std::fs::File::create(&config_path).unwrap();
        writeln!(
            config_file,
            r#"
import {{ baseSymlinks, baseDescription }} from "./base.ts";

export default {{
    description: baseDescription,
    symlinks: baseSymlinks,
}};
"#
        )
        .unwrap();

        let config = load_ts_embedded(&config_path).unwrap();
        assert_eq!(config.description, "Base config");
        assert_eq!(config.symlinks, vec!["data/cache"]);
    }

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
