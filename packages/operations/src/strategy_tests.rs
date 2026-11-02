//! Per-entry planning, conflict, and execution coverage.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use crate::*;
use std::{fs, path::Path};
use worktree_setup_config::{
    Config, CopyEntry, CopyGlobEntry, CopyGlobOptions, CopyPathOptions, CopyStrategy, LoadedConfig,
    TemplateMapping,
};

fn loaded(root: &Path, config: Config) -> LoadedConfig {
    LoadedConfig {
        config,
        config_dir: root.to_path_buf(),
        config_path: root.join("worktree.config.toml"),
        relative_path: "worktree.config.toml".to_owned(),
    }
}
fn byte(path: &str) -> CopyEntry {
    CopyEntry::Options(CopyPathOptions {
        path: path.to_owned(),
        strategy: CopyStrategy::ByteCopy,
    })
}
fn glob(pattern: &str) -> CopyGlobEntry {
    CopyGlobEntry::Options(CopyGlobOptions {
        pattern: pattern.to_owned(),
        strategy: CopyStrategy::ByteCopy,
    })
}

#[test]
fn mixed_strategies_reach_every_operation_and_execute() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("data/nested")).unwrap();
    fs::write(source.path().join("data/nested/file"), "nested").unwrap();
    for file in ["auto", "overwrite", "match.env", "example"] {
        fs::write(source.path().join(file), file).unwrap();
    }
    fs::write(target.path().join("overwrite"), "old longer contents").unwrap();
    let config = loaded(
        source.path(),
        Config {
            copy: vec!["auto".into(), byte("data")],
            overwrite: vec![byte("overwrite")],
            copy_glob: vec![glob("*.env")],
            templates: vec![TemplateMapping {
                source: "example".to_owned(),
                target: "local".to_owned(),
                strategy: CopyStrategy::ByteCopy,
            }],
            ..Default::default()
        },
    );
    let ops = plan_operations(
        &config,
        source.path(),
        target.path(),
        &ApplyConfigOptions::default(),
    )
    .unwrap();
    assert_eq!(ops.len(), 5);
    for op in &ops {
        assert_eq!(
            op.copy_strategy,
            if op.display_path == "auto" {
                CopyStrategy::Auto
            } else {
                CopyStrategy::ByteCopy
            }
        );
        execute_operation(op, |_, _| {}).unwrap();
    }
    assert_eq!(
        fs::read_to_string(target.path().join("data/nested/file")).unwrap(),
        "nested"
    );
    assert_eq!(
        fs::read_to_string(target.path().join("overwrite")).unwrap(),
        "overwrite"
    );
    assert_eq!(
        fs::read_to_string(target.path().join("match.env")).unwrap(),
        "match.env"
    );
    assert_eq!(
        fs::read_to_string(target.path().join("local")).unwrap(),
        "example"
    );
    let unstaged = plan_unstaged_operations_with_strategy(
        &["auto".to_owned()],
        source.path(),
        target.path(),
        CopyStrategy::ByteCopy,
    );
    assert_eq!(unstaged[0].copy_strategy, CopyStrategy::ByteCopy);
    execute_operation(&unstaged[0], |_, _| {}).unwrap();
}

#[test]
fn conflicts_include_glob_duplicates_and_directory_descendants() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    fs::create_dir(source.path().join("data")).unwrap();
    fs::write(source.path().join("data/file"), "data").unwrap();
    for config in [
        Config {
            copy: vec!["data".into(), byte("data/file")],
            ..Default::default()
        },
        Config {
            copy_glob: vec!["data/*".into(), glob("data/*")],
            ..Default::default()
        },
        Config {
            copy: vec!["data/file".into()],
            copy_glob: vec![glob("data/*")],
            ..Default::default()
        },
    ] {
        let error = plan_operations(
            &loaded(source.path(), config),
            source.path(),
            target.path(),
            &ApplyConfigOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(error, OperationError::CopyStrategyConflict { .. }));
    }
    assert_eq!(fs::read_dir(target.path()).unwrap().count(), 0);
}

#[test]
fn cross_config_and_unstaged_conflicts_are_detected() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    fs::write(source.path().join("file"), "data").unwrap();
    let config = loaded(
        source.path(),
        Config {
            copy: vec!["file".into()],
            ..Default::default()
        },
    );
    let mut ops = plan_operations(
        &config,
        source.path(),
        target.path(),
        &ApplyConfigOptions::default(),
    )
    .unwrap();
    ops.extend(plan_unstaged_operations_with_strategy(
        &["file".to_owned()],
        source.path(),
        target.path(),
        CopyStrategy::ByteCopy,
    ));
    assert!(validate_copy_strategies(&ops).is_err());
    ops[1].copy_strategy = CopyStrategy::Auto;
    validate_copy_strategies(&ops).unwrap();
    ops[1].copy_strategy = CopyStrategy::ByteCopy;
    ops[1].target = target.path().join("other/../file");
    assert!(validate_copy_strategies(&ops).is_err());
}
