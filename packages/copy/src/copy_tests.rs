//! Regression tests for portable copy semantics.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use super::*;

#[test]
fn empty_directories_survive_copy() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    fs::create_dir_all(source.join("nested/empty")).unwrap();
    assert_eq!(
        copy_directory(&source, &target, |_| {}).unwrap(),
        CopyResult::Created { files_copied: 0 }
    );
    assert!(target.join("nested/empty").is_dir());
    fs::write(source.join("file"), "data").unwrap();
    let target = dir.path().join("mixed");
    copy_directory(&source, &target, |_| {}).unwrap();
    assert!(target.join("nested/empty").is_dir());
    assert_eq!(fs::read_to_string(target.join("file")).unwrap(), "data");
}

#[test]
fn new_copy_and_new_overwrite_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::write(&source, "original").unwrap();
    for overwrite in [false, true] {
        let target = dir.path().join(format!("target-{overwrite}"));
        if overwrite {
            overwrite_file(&source, &target, |_| {}).unwrap();
        } else {
            copy_file(&source, &target, |_| {}).unwrap();
        }
        fs::write(&target, "target edit").unwrap();
        assert_eq!(fs::read_to_string(&source).unwrap(), "original");
        fs::write(&source, "source edit").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "target edit");
        fs::write(&source, "original").unwrap();
    }
}

#[test]
fn overwrite_preserves_existing_hardlink_identity() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    let alias = dir.path().join("alias");
    fs::write(&source, "new").unwrap();
    fs::write(&target, "old contents longer than new").unwrap();
    fs::hard_link(&target, &alias).unwrap();
    overwrite_file(&source, &target, |_| {}).unwrap();
    assert_eq!(fs::read_to_string(alias).unwrap(), "new");
}

#[cfg(unix)]
#[test]
fn directory_symlinks_are_preserved_without_traversal() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    fs::create_dir(&source).unwrap();
    symlink("missing", source.join("dangling")).unwrap();
    symlink(".", source.join("loop")).unwrap();
    let result = copy_directory(&source, &target, |_| {}).unwrap();
    assert_eq!(result, CopyResult::Created { files_copied: 2 });
    assert_eq!(
        fs::read_link(target.join("dangling")).unwrap(),
        Path::new("missing")
    );
    assert_eq!(fs::read_link(target.join("loop")).unwrap(), Path::new("."));
}

#[cfg(unix)]
#[test]
fn copy_skips_dangling_destination_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    fs::write(&source, "data").unwrap();
    std::os::unix::fs::symlink("missing", &target).unwrap();
    assert_eq!(
        copy_file(&source, &target, |_| {}).unwrap(),
        CopyResult::Exists
    );
    assert!(target.is_symlink());
    assert!(!dir.path().join("missing").exists());
}

#[cfg(unix)]
#[test]
fn overwrite_follows_existing_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    let actual = dir.path().join("actual");
    fs::write(&source, "new").unwrap();
    fs::write(&actual, "old").unwrap();
    std::os::unix::fs::symlink(&actual, &target).unwrap();
    overwrite_file(&source, &target, |_| {}).unwrap();
    assert!(target.is_symlink());
    assert_eq!(fs::read_to_string(actual).unwrap(), "new");
}
