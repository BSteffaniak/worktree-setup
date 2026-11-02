//! Config parsing coverage for mixed per-entry copy policies.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use crate::{Config, CopyStrategy, load_toml_config, load_ts_config};

#[test]
fn mixed_toml_copy_entries_and_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worktree.config.toml");
    std::fs::write(
        &path,
        r#"
copy = ["deps", { path = "data", strategy = "BYTE_COPY" }, { path = "default" }]
overwrite = [{ path = "settings", strategy = "BYTE_COPY" }]
copyGlob = ["*.env", { pattern = "fixtures/**", strategy = "BYTE_COPY" }]
templates = [{ source = "example", target = "local", strategy = "BYTE_COPY" }]
copyUnstaged = { strategy = "BYTE_COPY" }
"#,
    )
    .unwrap();
    assert_mixed(&load_toml_config(&path).unwrap());
}

#[test]
fn mixed_typescript_copy_entries_and_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worktree.config.ts");
    std::fs::write(
        &path,
        r#"export default {
copy: ["deps", { path: "data", strategy: "BYTE_COPY" }, { path: "default" }],
overwrite: [{ path: "settings", strategy: "BYTE_COPY" }],
copyGlob: ["*.env", { pattern: "fixtures/**", strategy: "BYTE_COPY" }],
templates: [{ source: "example", target: "local", strategy: "BYTE_COPY" }],
copyUnstaged: { strategy: "BYTE_COPY" },
};"#,
    )
    .unwrap();
    assert_mixed(&load_ts_config(&path).unwrap());
}

fn assert_mixed(config: &Config) {
    assert_eq!(config.copy[0].strategy(), CopyStrategy::Auto);
    assert_eq!(config.copy[1].strategy(), CopyStrategy::ByteCopy);
    assert_eq!(config.copy[2].strategy(), CopyStrategy::Auto);
    assert_eq!(config.overwrite[0].strategy(), CopyStrategy::ByteCopy);
    assert_eq!(config.copy_glob[0].strategy(), CopyStrategy::Auto);
    assert_eq!(config.copy_glob[1].strategy(), CopyStrategy::ByteCopy);
    assert_eq!(config.templates[0].strategy, CopyStrategy::ByteCopy);
    assert!(config.copy_unstaged.enabled());
    assert_eq!(config.copy_unstaged.strategy(), CopyStrategy::ByteCopy);
}

#[test]
fn invalid_strategies_and_malformed_options_are_rejected() {
    for input in [
        r#"copy = [{ path = "data", strategy = "INVALID" }]"#,
        r#"copy = [{ path = "data", stratgy = "BYTE_COPY" }]"#,
        r#"copyGlob = [{ path = "*.env", strategy = "BYTE_COPY" }]"#,
        r#"copyUnstaged = { strategy = "INVALID" }"#,
        r#"templates = [{ source = "a", target = "b", strategy = "INVALID" }]"#,
    ] {
        assert!(toml::from_str::<Config>(input).is_err(), "accepted {input}");
    }
    for enabled in [false, true] {
        let config: Config = toml::from_str(&format!("copyUnstaged = {enabled}")).unwrap();
        assert_eq!(config.copy_unstaged.enabled(), enabled);
        assert_eq!(config.copy_unstaged.strategy(), CopyStrategy::Auto);
    }
}
