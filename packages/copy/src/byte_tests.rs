//! Byte-copy policy regression tests.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use super::*;

#[test]
fn byte_copy_never_calls_clone_and_handles_empty_and_multibuffer_files() {
    let dir = tempfile::tempdir().unwrap();
    for size in [0, 17, 3 * 128 * 1024 + 5] {
        let source = dir.path().join(format!("source-{size}"));
        let target = dir.path().join(format!("target-{size}"));
        let data: Vec<u8> = (0..size).map(|i| u8::try_from(i % 251).unwrap()).collect();
        fs::write(&source, &data).unwrap();
        let method = copy_new_with_strategy(&source, &target, CopyStrategy::ByteCopy, |_, _| {
            panic!("BYTE_COPY requested cloning")
        })
        .unwrap();
        assert_eq!(method, CopyMethod::ByteCopied);
        assert_eq!(fs::read(&target).unwrap(), data);
        fs::write(&target, "changed").unwrap();
        assert_eq!(fs::read(&source).unwrap(), data);
    }
}

#[test]
fn byte_copy_propagates_read_and_write_errors() {
    struct FailedReader;
    impl io::Read for FailedReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::PermissionDenied.into())
        }
    }
    assert_eq!(
        transfer_bytes(&mut FailedReader, &mut Vec::new())
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    let mut input: &[u8] = b"contents";
    let mut full_destination: &mut [u8] = &mut [];
    assert_eq!(
        transfer_bytes(&mut input, &mut full_destination)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WriteZero
    );
}

#[test]
fn failed_byte_copy_publication_cleans_staging_without_clobbering() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    fs::write(&source, "new").unwrap();
    fs::write(&target, "old").unwrap();
    assert_eq!(
        copy_new(&source, &target, CopyStrategy::ByteCopy)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn byte_copy_refuses_self_and_hardlink_before_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let alias = dir.path().join("alias");
    fs::write(&source, "must survive").unwrap();
    fs::hard_link(&source, &alias).unwrap();
    for target in [&source, &alias] {
        assert_eq!(
            byte_copy(&source, target).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), "must survive");
    }
}

#[test]
fn byte_copy_overwrite_truncates_and_keeps_hardlink_identity() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    let alias = dir.path().join("alias");
    fs::write(&source, "short").unwrap();
    fs::write(&target, "much longer original data").unwrap();
    fs::hard_link(&target, &alias).unwrap();
    crate::overwrite_file_with_strategy(&source, &target, CopyStrategy::ByteCopy, |_| {}).unwrap();
    assert_eq!(fs::read_to_string(&alias).unwrap(), "short");
}

#[cfg(unix)]
#[test]
fn byte_copy_handles_readonly_executable_source_and_symlink_overwrites() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    fs::write(&source, "readonly executable").unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o551)).unwrap();
    copy_new(&source, &target, CopyStrategy::ByteCopy).unwrap();
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o551
    );
    let actual = dir.path().join("actual");
    fs::write(&actual, "old").unwrap();
    let link = dir.path().join("link");
    symlink(&actual, &link).unwrap();
    crate::overwrite_file_with_strategy(&source, &link, CopyStrategy::ByteCopy, |_| {}).unwrap();
    assert!(link.is_symlink());
    assert_eq!(fs::read_to_string(&actual).unwrap(), "readonly executable");
    let self_link = dir.path().join("self-link");
    symlink(&actual, &self_link).unwrap();
    assert!(byte_copy(&actual, &self_link).is_err());
    assert_eq!(fs::read_to_string(actual).unwrap(), "readonly executable");
}

#[test]
fn byte_copy_directory_recurses_and_reports_completion() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    fs::create_dir_all(source.join("nested/empty")).unwrap();
    fs::write(source.join("nested/data"), "data").unwrap();
    let result =
        crate::copy_directory_with_strategy(&source, &target, CopyStrategy::ByteCopy, |_| {})
            .unwrap();
    assert_eq!(result, crate::CopyResult::Created { files_copied: 1 });
    assert_eq!(
        fs::read_to_string(target.join("nested/data")).unwrap(),
        "data"
    );
    assert!(target.join("nested/empty").is_dir());
}
