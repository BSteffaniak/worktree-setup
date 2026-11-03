//! Hermetic end-to-end coverage of the Git reminder integration.
#![cfg(unix)]
#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for directory in ["repo", "home", "config", "bin"] {
            fs::create_dir(root.path().join(directory)).unwrap();
        }
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_worktree-setup"),
            root.path().join("bin/worktree-setup"),
        )
        .unwrap();
        let fixture = Self { root };
        fixture.git(&["init"]);
        fixture.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ]);
        fixture
    }

    fn command(&self, binary: &str, cwd: &Path) -> Command {
        let mut command = Command::new(binary);
        command.current_dir(cwd);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") || name == "WORKTREE_SETUP_CREATING" {
                command.env_remove(name);
            }
        }
        let mut paths = vec![self.root.path().join("bin")];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        command
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("HOME", self.root.path().join("home"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("GIT_CONFIG_GLOBAL", self.root.path().join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    fn run(&self, binary: &str, args: &[&str], cwd: &Path) -> Output {
        self.command(binary, cwd).args(args).output().unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.run(
            env!("CARGO_BIN_EXE_worktree-setup"),
            args,
            &self.root.path().join("repo"),
        )
    }

    fn git(&self, args: &[&str]) -> Output {
        let output = self.run("git", args, &self.root.path().join("repo"));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn config_path(&self) -> std::path::PathBuf {
        #[cfg(target_os = "macos")]
        let base = self.root.path().join("home/Library/Application Support");
        #[cfg(not(target_os = "macos"))]
        let base = self.root.path().join("config");
        base.join("worktree-setup/config.toml")
    }
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn reminder(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr).contains("worktree-setup setup .")
}

#[test]
fn rendering_matches_installer_and_quotes_absolute_executables() {
    let fixture = Fixture::new();
    let rendered = fixture.cli(&["hook", "render", "post-checkout"]);
    assert_ok(&rendered);
    assert_ok(&fixture.cli(&["init"]));
    assert_eq!(
        rendered.stdout,
        fs::read(fixture.root.path().join("repo/.git/hooks/post-checkout")).unwrap()
    );
    assert!(
        !fixture
            .cli(&[
                "hook",
                "render",
                "post-checkout",
                "--executable",
                "relative"
            ])
            .status
            .success()
    );
    assert!(
        !fixture
            .cli(&["hook", "render", "pre-commit"])
            .status
            .success()
    );

    // The pinned executable must work even when its path needs shell quoting,
    // independently of whether worktree-setup can be found through PATH.
    let pinned = fixture.root.path().join("pinned ' executable");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_worktree-setup"), &pinned).unwrap();
    let rendered = fixture.cli(&[
        "hook",
        "render",
        "post-checkout",
        "--executable",
        pinned.to_str().unwrap(),
    ]);
    assert_ok(&rendered);
    let hook = fixture.root.path().join("repo/.git/hooks/post-checkout");
    fs::write(&hook, rendered.stdout).unwrap();
    fs::remove_file(fixture.root.path().join("bin/worktree-setup")).unwrap();
    assert!(reminder(&fixture.git(&[
        "worktree",
        "add",
        "--detach",
        "../rendered"
    ])));
}

#[test]
fn local_hook_reminds_only_on_initial_linked_checkout_and_is_reversible() {
    let fixture = Fixture::new();
    assert_ok(&fixture.cli(&["init"]));
    assert_ok(&fixture.cli(&["init"]));
    let output = fixture.git(&["worktree", "add", "--detach", "../new tree"]);
    assert!(reminder(&output));
    let linked = fixture.root.path().join("new tree");
    let output = fixture.run("git", &["checkout", "--detach", "HEAD"], &linked);
    assert_ok(&output);
    assert!(!reminder(&output));
    // Init/uninit also work from linked worktrees, targeting the common directory.
    let output = fixture.run(env!("CARGO_BIN_EXE_worktree-setup"), &["uninit"], &linked);
    assert_ok(&output);
    assert!(
        !fixture
            .root
            .path()
            .join("repo/.git/hooks/post-checkout")
            .exists()
    );
    assert_ok(&fixture.cli(&["uninit"]));
    assert!(!reminder(&fixture.git(&[
        "worktree",
        "add",
        "--detach",
        "../another"
    ])));
}

#[test]
fn global_preferences_compose_with_repo_fields_and_explicit_overrides() {
    let fixture = Fixture::new();
    let config = fixture.config_path();
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        config,
        "[hooks]\nsetup_reminder = false\n[security]\nallow_path_escape = true\n",
    )
    .unwrap();
    fs::write(
        fixture.root.path().join("repo/.worktree-setup.toml"),
        "[remove]\nbranch_delete = 'NEVER'\n",
    )
    .unwrap();
    assert_ok(&fixture.cli(&["init"]));
    assert!(!reminder(
        &fixture.git(&["worktree", "add", "--detach", "../quiet"])
    ));
    fs::write(
        fixture.root.path().join("repo/.worktree-setup.toml"),
        "[hooks]\nsetup_reminder = true\n",
    )
    .unwrap();
    assert!(reminder(
        &fixture.git(&["worktree", "add", "--detach", "../loud"])
    ));
}

#[test]
fn global_installation_requires_explicit_opt_in_and_restores_routing() {
    let fixture = Fixture::new();
    assert_ok(&fixture.cli(&["init", "--global", "--yes"]));
    assert_ok(&fixture.cli(&["init", "--global", "--yes"]));
    assert!(reminder(&fixture.git(&[
        "worktree",
        "add",
        "--detach",
        "../global"
    ])));
    // Local install must not edit a global directory via effective hooksPath.
    assert!(!fixture.cli(&["init"]).status.success());
    assert_ok(&fixture.cli(&["uninit", "--global"]));
    assert_ok(&fixture.cli(&["uninit", "--global"]));
    let output = fixture.run(
        "git",
        &["config", "--global", "--get", "core.hooksPath"],
        &fixture.root.path().join("repo"),
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn existing_and_modified_hooks_are_never_overwritten_or_removed() {
    let fixture = Fixture::new();
    let hook = fixture.root.path().join("repo/.git/hooks/post-checkout");
    fs::write(&hook, "#!/bin/sh\nexit 7\n").unwrap();
    assert!(!fixture.cli(&["init"]).status.success());
    assert!(!fixture.cli(&["uninit"]).status.success());
    assert_eq!(fs::read_to_string(&hook).unwrap(), "#!/bin/sh\nexit 7\n");
    fs::remove_file(&hook).unwrap();
    std::os::unix::fs::symlink("nonexistent", &hook).unwrap();
    assert!(!fixture.cli(&["init"]).status.success());
    assert!(!fixture.cli(&["uninit"]).status.success());
}

#[test]
fn setup_recovers_files_and_commands_without_recreating_worktree() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.path().join("repo/worktree.config.toml"),
        "copy = ['.env.local']\nsymlinks = ['shared']\npostSetup = ['printf done > setup-done']\n",
    )
    .unwrap();
    fs::write(fixture.root.path().join("repo/.env.local"), "local secret").unwrap();
    fs::create_dir(fixture.root.path().join("repo/shared")).unwrap();
    assert_ok(&fixture.cli(&["init"]));
    assert!(reminder(&fixture.git(&[
        "worktree",
        "add",
        "--detach",
        "../recover"
    ])));
    let linked = fixture.root.path().join("recover");
    let output = fixture.run(
        env!("CARGO_BIN_EXE_worktree-setup"),
        &["setup", ".", "--non-interactive", "--no-progress"],
        &linked,
    );
    assert_ok(&output);
    assert_eq!(
        fs::read_to_string(linked.join(".env.local")).unwrap(),
        "local secret"
    );
    assert!(
        fs::symlink_metadata(linked.join("shared"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(linked.join("setup-done")).unwrap(),
        "done"
    );
    let output = fixture.cli(&[
        "../via-tool",
        "--new-branch",
        "via-tool",
        "--non-interactive",
        "--no-progress",
    ]);
    assert_ok(&output);
    assert!(!reminder(&output));
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("via-tool/.env.local")).unwrap(),
        "local secret"
    );
}

#[test]
fn custom_paths_and_additional_global_hooks_are_preserved() {
    let fixture = Fixture::new();
    let custom = fixture.root.path().join("custom");
    fs::create_dir(&custom).unwrap();
    fixture.git(&["config", "core.hooksPath", custom.to_str().unwrap()]);
    assert!(!fixture.cli(&["init"]).status.success());
    assert!(!custom.join("post-checkout").exists());
    fixture.git(&["config", "--unset", "core.hooksPath"]);
    fixture.git(&[
        "config",
        "--global",
        "core.hooksPath",
        custom.to_str().unwrap(),
    ]);
    assert_ok(&fixture.cli(&["init", "--global", "--yes"]));
    assert!(reminder(&fixture.git(&[
        "worktree",
        "add",
        "--detach",
        "../custom-worktree"
    ])));
    assert_ok(&fixture.cli(&["uninit", "--global"]));
    assert_eq!(
        String::from_utf8(
            fixture
                .git(&["config", "--global", "--get", "core.hooksPath"])
                .stdout
        )
        .unwrap()
        .trim(),
        custom.to_str().unwrap()
    );
    fixture.git(&["config", "--global", "--unset", "core.hooksPath"]);
    assert_ok(&fixture.cli(&["init", "--global", "--yes"]));
    let directory = fixture.config_path().parent().unwrap().join("hooks");
    fs::write(directory.join("pre-commit"), "other hook").unwrap();
    assert_ok(&fixture.cli(&["uninit", "--global"]));
    assert_eq!(
        fs::read_to_string(directory.join("pre-commit")).unwrap(),
        "other hook"
    );
    fixture.git(&["config", "--global", "--get", "core.hooksPath"]);
}

#[test]
fn tool_marker_and_no_checkout_are_silent_and_invalid_config_is_nonfatal() {
    let fixture = Fixture::new();
    assert_ok(&fixture.cli(&["init"]));
    let output = fixture
        .command("git", &fixture.root.path().join("repo"))
        .env("WORKTREE_SETUP_CREATING", "1")
        .args(["worktree", "add", "--detach", "../managed"])
        .output()
        .unwrap();
    assert_ok(&output);
    assert!(!reminder(&output));
    assert!(!reminder(&fixture.git(&[
        "worktree",
        "add",
        "--no-checkout",
        "--detach",
        "../empty"
    ])));
    fs::write(
        fixture.root.path().join("repo/.worktree-setup.toml"),
        "invalid [",
    )
    .unwrap();
    let output = fixture.git(&["worktree", "add", "--detach", "../invalid"]);
    assert!(reminder(&output));
}
