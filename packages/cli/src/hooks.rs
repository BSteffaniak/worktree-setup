//! Explicit installation and fail-open execution of setup reminder hooks.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use worktree_setup_config::{global_config_path, load_global_config};
use worktree_setup_git::{discover_repo, get_repo_root};

use crate::args::HookInstallArgs;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const HOOK: &str = "#!/bin/sh\n# worktree-setup reminder v1 (managed; do not edit)\nif [ -z \"${WORKTREE_SETUP_CREATING:-}\" ] && command -v worktree-setup >/dev/null 2>&1; then\n    worktree-setup hook-post-checkout \"$@\" || :\nfi\nexit 0\n";
const MANAGED_PATH: &str = "worktreeSetup.managedHooksPath";

fn git(args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).output()?;
    if !output.status.success() {
        return Err(format!("Git failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(String::from_utf8(output.stdout)?
        .trim_end_matches('\n')
        .to_owned())
}

fn config(scope: &str, key: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["config", scope, "--get", key])
        .output()?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err("Could not read Git configuration".into());
    }
    Ok(Some(
        String::from_utf8(output.stdout)?
            .trim_end_matches('\n')
            .to_owned(),
    ))
}

fn local_directory() -> Result<PathBuf> {
    // A shared/relative custom hooksPath may affect more than this repository.
    // Do not modify it implicitly, even when its hook slot is empty.
    if config("--local", "core.hooksPath")?.is_some()
        || Command::new("git")
            .args(["config", "--get", "core.hooksPath"])
            .output()?
            .status
            .success()
    {
        return Err("core.hooksPath is configured; no files changed. Integrate manually into your existing post-checkout hook:\n  worktree-setup hook-post-checkout \"$@\" || :".into());
    }
    Ok(PathBuf::from(git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ])?)
    .join("hooks"))
}

fn global_directory() -> Result<(PathBuf, bool)> {
    if let Some(path) = config("--global", "core.hooksPath")? {
        // Relative and tilde paths require Git-specific expansion and cwd rules.
        // Avoid guessing which filesystem object a global setting refers to.
        let directory = PathBuf::from(path);
        if !directory.is_absolute() {
            return Err(
                "Global core.hooksPath is not absolute; integrate the reminder manually instead"
                    .into(),
            );
        }
        return Ok((directory, false));
    }
    let directory = global_config_path()
        .ok_or("Could not determine config directory")?
        .parent()
        .ok_or("Invalid config directory")?
        .join("hooks");
    Ok((directory, true))
}

fn owned_hook(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || fs::read(path)? != HOOK.as_bytes() {
                return Err(format!("Refusing to overwrite/remove existing or modified hook: {}. Integrate manually:\n  worktree-setup hook-post-checkout \"$@\" || :", path.display()).into());
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub fn install(options: &HookInstallArgs) -> Result<()> {
    let (directory, configure) = if options.global {
        eprintln!(
            "Global Git hooks do not merge with repository-local hooks. Repositories overriding core.hooksPath will not use this reminder."
        );
        if !options.yes
            && !dialoguer::Confirm::new()
                .with_prompt("Install the global reminder integration?")
                .default(false)
                .interact()?
        {
            return Ok(());
        }
        global_directory()?
    } else {
        (local_directory()?, false)
    };
    let path = directory.join("post-checkout");
    if !owned_hook(&path)? {
        fs::create_dir_all(&directory)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o755);
        }
        let mut file = options.open(&path)?;
        file.write_all(HOOK.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    if configure {
        let value = directory.to_str().ok_or("Hook directory is not UTF-8")?;
        // Record ownership before changing routing, allowing interrupted installs
        // to be retried without losing uninstall information.
        git(&["config", "--global", MANAGED_PATH, value])?;
        git(&["config", "--global", "core.hooksPath", value])?;
    }
    println!("Installed setup reminder: {}", path.display());
    Ok(())
}

pub fn uninstall(global: bool) -> Result<()> {
    let directory = if global {
        let Some(path) = config("--global", "core.hooksPath")? else {
            println!("No global hooksPath configured; nothing removed.");
            return Ok(());
        };
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("Global core.hooksPath is not absolute; nothing removed".into());
        }
        path
    } else {
        local_directory()?
    };
    let path = directory.join("post-checkout");
    if owned_hook(&path)? {
        fs::remove_file(&path)?;
    }
    if global && config("--global", MANAGED_PATH)?.as_deref() == directory.to_str() {
        // Restore default routing only if no other hooks/files have been added.
        if fs::read_dir(&directory)?.next().is_some() {
            eprintln!("Kept global core.hooksPath: the directory contains other files.");
        } else {
            git(&["config", "--global", "--unset-all", "core.hooksPath"])?;
            git(&["config", "--global", "--unset-all", MANAGED_PATH])?;
        }
    }
    println!("Removed setup reminder: {}", path.display());
    Ok(())
}

pub fn remind(old_head: &str, branch_checkout: &str) {
    if std::env::var_os("WORKTREE_SETUP_CREATING").is_some()
        || branch_checkout != "1"
        || !matches!(old_head.len(), 40 | 64)
        || !old_head.bytes().all(|byte| byte == b'0')
    {
        return;
    }
    // All failures are deliberately non-fatal to the Git operation. No prompts,
    // TypeScript evaluation, config operation discovery, or setup execution.
    let result = (|| -> Result<()> {
        let repo = discover_repo(&std::env::current_dir()?)?;
        if !repo.is_worktree() {
            return Ok(());
        }
        let root = get_repo_root(&repo)?;
        if load_global_config(Some(&root))?.hooks.setup_reminder {
            eprintln!(
                "Worktree created. Apply project configuration from the new worktree with:\n\n  worktree-setup setup .\n\nThis applies selected copies, symlinks, templates, and post-setup commands."
            );
        }
        Ok(())
    })();
    if result.is_err() {
        eprintln!(
            "worktree-setup: could not read reminder preferences; run `worktree-setup setup .` in the new worktree if needed."
        );
    }
}
