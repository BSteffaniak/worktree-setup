//! Capability-based copying without exposing filesystem details to callers.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use crate::CopyStrategy;
use std::{fs, io, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyMethod {
    Cloned,
    Copied,
    ByteCopied,
}

/// Stage in the destination filesystem, then publish without replacing an existing path.
/// A private directory confines cleanup to files owned by this operation, even when a
/// failed backend leaves a partial file. Never delete the user's destination to retry.
pub fn copy_new(source: &Path, target: &Path, strategy: CopyStrategy) -> io::Result<CopyMethod> {
    copy_new_with_strategy(source, target, strategy, |from, to| {
        reflink_copy::reflink(from, to)
    })
}

#[cfg(test)]
fn copy_new_with(
    source: &Path,
    target: &Path,
    clone: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<CopyMethod> {
    copy_new_with_strategy(source, target, CopyStrategy::Auto, clone)
}

fn copy_new_with_strategy(
    source: &Path,
    target: &Path,
    strategy: CopyStrategy,
    clone: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<CopyMethod> {
    if !fs::metadata(source)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "copy source must be a regular file",
        ));
    }
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".worktree-copy-")
        .tempdir_in(parent)?;
    let staged = staging.path().join("file");
    let method = if strategy == CopyStrategy::ByteCopy {
        byte_copy(source, &staged)?;
        CopyMethod::ByteCopied
    } else {
        match clone(source, &staged) {
            Ok(()) => CopyMethod::Cloned,
            Err(error) => {
                log::debug!(
                    "Clone unavailable for {} -> {}: {error}; using ordinary copy",
                    source.display(),
                    target.display()
                );
                // Some backends leave a partial (possibly read-only) destination.
                // Remove it rather than writing through an unknown backend artifact.
                #[cfg(windows)]
                if let Ok(metadata) = staged.symlink_metadata() {
                    if metadata.is_file() && metadata.permissions().readonly() {
                        let mut permissions = metadata.permissions();
                        permissions.set_readonly(false);
                        fs::set_permissions(&staged, permissions)?;
                    }
                }
                match fs::remove_file(&staged) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                fs::copy(source, &staged)?;
                CopyMethod::Copied
            }
        }
    };
    // Linux FICLONE duplicates contents but not permissions. Match fs::copy on
    // every backend (including executable bits), rather than depending on the OS.
    fs::set_permissions(&staged, fs::metadata(source)?.permissions())?;
    tempfile::TempPath::from_path(staged)
        .persist_noclobber(target)
        .map_err(|error| error.error)?;
    log::trace!("{method:?}: {} -> {}", source.display(), target.display());
    Ok(method)
}

/// Buffered transfer without std/OS copy acceleration. Open the destination without
/// truncating, compare file identities (including hard links), then truncate it.
pub fn byte_copy(source: &Path, target: &Path) -> io::Result<u64> {
    use std::io::Write;
    let mut input = fs::File::open(source)?;
    if !input.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "copy source must be a regular file",
        ));
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(target)?;
    let source_handle = same_file::Handle::from_file(input.try_clone()?)?;
    let target_handle = same_file::Handle::from_file(output.try_clone()?)?;
    if source_handle == target_handle {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source and destination are the same file",
        ));
    }
    output.set_len(0)?;
    let written = transfer_bytes(&mut input, &mut output)?;
    output.flush()?;
    output.set_permissions(input.metadata()?.permissions())?;
    Ok(written)
}

fn transfer_bytes(input: &mut impl io::Read, output: &mut impl io::Write) -> io::Result<u64> {
    let mut buffer = vec![0_u8; 128 * 1024];
    let mut written = 0;
    loop {
        let count = match input.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        written += count as u64;
    }
    Ok(written)
}

#[cfg(test)]
#[path = "byte_tests.rs"]
mod byte_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn unsupported(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    #[test]
    fn fallback_is_independent_and_preserves_contents() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        fs::write(&source, "original").unwrap();
        assert_eq!(
            copy_new_with(&source, &target, unsupported).unwrap(),
            CopyMethod::Copied
        );
        assert_eq!(fs::read(&source).unwrap(), fs::read(&target).unwrap());
        fs::write(&target, "destination edit").unwrap();
        assert_eq!(fs::read_to_string(&source).unwrap(), "original");
        fs::write(&source, "source edit").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "destination edit");
    }

    #[test]
    fn failed_clone_artifact_is_removed_before_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        fs::write(&source, "short").unwrap();
        let method = copy_new_with(&source, &target, |_, staged| {
            fs::write(staged, "partial clone with trailing garbage")?;
            Err(io::ErrorKind::Unsupported.into())
        })
        .unwrap();
        assert_eq!(method, CopyMethod::Copied);
        assert_eq!(fs::read_to_string(target).unwrap(), "short");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn publication_never_clobbers_a_racing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        fs::write(&source, "source").unwrap();
        let error = copy_new_with(&source, &target, |_, _| {
            fs::write(&target, "created concurrently")?;
            Err(io::ErrorKind::Unsupported.into())
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(target).unwrap(), "created concurrently");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn failed_fallback_leaves_destination_untouched_and_cleans_staging() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, "data").unwrap();
        let error = copy_new_with(&source, &dir.path().join("target"), |from, staged| {
            fs::write(staged, "partial")?;
            fs::remove_file(from)?;
            Err(io::ErrorKind::PermissionDenied.into())
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn clone_and_fallback_preserve_executable_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, "executable").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o751)).unwrap();
        // Simulate a successful content-only clone, as on Linux.
        let cloned = dir.path().join("cloned");
        assert_eq!(
            copy_new_with(&source, &cloned, |from, to| {
                fs::write(to, fs::read(from)?)?;
                fs::set_permissions(to, fs::Permissions::from_mode(0o600))
            })
            .unwrap(),
            CopyMethod::Cloned
        );
        let copied = dir.path().join("copied");
        copy_new_with(&source, &copied, unsupported).unwrap();
        for path in [&cloned, &copied] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o751
            );
        }
    }

    /// Destination root must be on another volume or an unsupported filesystem.
    #[test]
    #[ignore = "set WORKTREE_COPY_FALLBACK_ROOT to a cross-volume or non-cloning destination"]
    fn real_filesystem_fallback() {
        let root = std::env::var_os("WORKTREE_COPY_FALLBACK_ROOT")
            .expect("set WORKTREE_COPY_FALLBACK_ROOT");
        let source_dir = tempfile::tempdir().unwrap();
        let destination_dir = tempfile::tempdir_in(root).unwrap();
        let source = source_dir.path().join("source");
        let target = destination_dir.path().join("target");
        fs::write(&source, vec![7_u8; 1024 * 1024]).unwrap();
        assert_eq!(
            copy_new(&source, &target, CopyStrategy::Auto).unwrap(),
            CopyMethod::Copied,
            "chosen paths support cloning; this is not fallback coverage"
        );
        assert_eq!(fs::read(source).unwrap(), fs::read(target).unwrap());
    }

    /// Dependency-free, opt-in wall-clock benchmark. Includes staging and publication
    /// in both paths; does not interpret filesystem allocation as exclusive storage.
    #[test]
    #[ignore = "manual benchmark: run in release mode with --ignored --nocapture"]
    fn benchmark_copy_backends() {
        use std::time::Instant;
        let dir = tempfile::tempdir().unwrap();
        for (name, count, size) in [("large", 4, 16 * 1024 * 1024), ("small", 2000, 1024)] {
            let source = dir.path().join(format!("source-{name}"));
            fs::create_dir(&source).unwrap();
            let data = vec![53_u8; size];
            for index in 0..count {
                fs::write(source.join(index.to_string()), &data).unwrap();
            }
            for mode in ["auto", "fallback", "byte-copy"] {
                let target = dir.path().join(format!("target-{name}-{mode}"));
                fs::create_dir(&target).unwrap();
                let started = Instant::now();
                let mut cloned = 0;
                for index in 0..count {
                    let from = source.join(index.to_string());
                    let to = target.join(index.to_string());
                    let method = match mode {
                        "auto" => copy_new(&from, &to, CopyStrategy::Auto),
                        "byte-copy" => copy_new(&from, &to, CopyStrategy::ByteCopy),
                        _ => copy_new_with(&from, &to, unsupported),
                    }
                    .unwrap();
                    cloned += usize::from(method == CopyMethod::Cloned);
                }
                eprintln!(
                    "{name}: mode={mode}, {count} files, {cloned} cloned, elapsed={:?}",
                    started.elapsed()
                );
                let started = Instant::now();
                for index in 0..count {
                    crate::overwrite_file_with_strategy(
                        &source.join(index.to_string()),
                        &target.join(index.to_string()),
                        if mode == "byte-copy" {
                            CopyStrategy::ByteCopy
                        } else {
                            CopyStrategy::Auto
                        },
                        |_| {},
                    )
                    .unwrap();
                }
                eprintln!(
                    "{name}: existing-target overwrite elapsed={:?}",
                    started.elapsed()
                );
            }
            let target = dir.path().join(format!("tree-{name}"));
            let started = Instant::now();
            crate::copy_directory(&source, &target, |_| {}).unwrap();
            eprintln!(
                "{name}: parallel directory copy elapsed={:?}",
                started.elapsed()
            );
        }
    }

    /// Opt-in: an unsupported filesystem must not masquerade as a clone success.
    #[test]
    #[ignore = "requires a clone-capable filesystem; run explicitly on APFS/Btrfs/XFS/ReFS"]
    fn real_clone_is_independent() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        fs::write(&source, vec![42_u8; 1024 * 1024]).unwrap();
        assert_eq!(
            copy_new(&source, &target, CopyStrategy::Auto).unwrap(),
            CopyMethod::Cloned,
            "filesystem did not clone; fallback is not clone coverage"
        );
        fs::write(&target, "target edit").unwrap();
        assert_eq!(fs::metadata(&source).unwrap().len(), 1024 * 1024);
        fs::write(&source, "source edit").unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "target edit");
    }
}
