//! Fast file counting and disk usage calculation using jwalk.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::Path;

/// Count files in a path.
///
/// - If path is a file: returns 1
/// - If path is a directory: returns count of all files recursively
/// - If path doesn't exist or is a symlink: returns 0
///
/// Uses `jwalk` with sorting disabled for maximum speed.
#[must_use]
pub fn count_files(path: &Path) -> u64 {
    count_files_with_progress(path, |_| {})
}

/// Count files in a directory with progress callback.
///
/// The callback is invoked every 100 files with the current count.
/// This allows updating a progress display during enumeration of large directories.
///
/// - If path is a file: returns 1
/// - If path is a directory: returns count of all files recursively
/// - If path doesn't exist or is a symlink: returns 0
///
/// # Arguments
///
/// * `path` - Path to count files in
/// * `on_progress` - Callback invoked every 100 files with current count
///
/// # Returns
///
/// Total file count
pub fn count_files_with_progress<F>(path: &Path, on_progress: F) -> u64
where
    F: Fn(u64),
{
    if !path.exists() {
        return 0;
    }

    // For symlinks, we don't count inside them
    if path.is_symlink() {
        return 0;
    }

    if path.is_file() {
        on_progress(1);
        return 1;
    }

    if !path.is_dir() {
        return 0;
    }

    // Use jwalk with sort disabled for speed
    let mut count = 0u64;
    for _entry in jwalk::WalkDir::new(path)
        .skip_hidden(false)
        .sort(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        count += 1;
        if count.is_multiple_of(100) {
            on_progress(count);
        }
    }

    // Final callback with total
    on_progress(count);
    count
}

/// Calculate the disk usage of a path (file or directory, recursive).
///
/// Uses `jwalk` for parallel directory traversal with `follow_links(false)`
/// to avoid traversing symlinks. On Unix, reports actual disk usage via
/// `st_blocks * 512` (matching `du` and `ncdu` behavior). On other
/// platforms, falls back to apparent size (`file.len()`).
///
/// - If path is a symlink: returns the symlink's own metadata size
/// - If path is a file: returns the file's disk usage
/// - If path is a directory: returns the sum of all regular files' disk usage
/// - If path doesn't exist: returns 0
#[must_use]
pub fn disk_usage(path: &Path) -> u64 {
    if path.is_symlink() {
        return path.symlink_metadata().map_or(0, |m| file_disk_usage(&m));
    }
    if path.is_file() {
        return path.metadata().map_or(0, |m| file_disk_usage(&m));
    }
    if !path.is_dir() {
        return 0;
    }

    let mut total = 0u64;

    for entry in jwalk::WalkDir::new(path)
        .skip_hidden(false)
        .sort(false)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        // Skip directories and symlinks — only count regular file sizes
        let ft = entry.file_type();
        if ft.is_dir() || ft.is_symlink() {
            continue;
        }

        if let Ok(meta) = entry.path().metadata() {
            total += file_disk_usage(&meta);
        }
    }

    total
}

/// Return the disk usage of a single file from its metadata.
///
/// On Unix, uses `st_blocks * 512` for actual disk usage (matching `du`).
/// On other platforms, falls back to the file's apparent size.
fn file_disk_usage(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_count_files_single_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "content").unwrap();

        assert_eq!(count_files(&file), 1);
    }

    #[test]
    fn test_count_files_directory() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("file1.txt"), "1").unwrap();
        fs::write(dir.path().join("file2.txt"), "2").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();
        fs::write(dir.path().join("subdir/file3.txt"), "3").unwrap();

        assert_eq!(count_files(dir.path()), 3);
    }

    #[test]
    fn test_count_files_empty_directory() {
        let dir = TempDir::new().unwrap();
        assert_eq!(count_files(dir.path()), 0);
    }

    #[test]
    fn test_count_files_nonexistent() {
        let path = Path::new("/nonexistent/path/that/does/not/exist");
        assert_eq!(count_files(path), 0);
    }

    #[test]
    fn test_disk_usage_single_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();

        let usage = disk_usage(&file);
        assert!(usage > 0, "disk usage of a non-empty file should be > 0");
    }

    #[test]
    fn test_disk_usage_directory() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "aaaa").unwrap();
        fs::write(dir.path().join("b.txt"), "bbbb").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/c.txt"), "cccc").unwrap();

        let usage = disk_usage(dir.path());
        assert!(
            usage > 0,
            "disk usage of a directory with files should be > 0"
        );
    }

    #[test]
    fn test_disk_usage_nonexistent() {
        let usage = disk_usage(Path::new("/nonexistent/path"));
        assert_eq!(usage, 0);
    }

    #[test]
    fn test_disk_usage_does_not_follow_symlinks() {
        let dir = TempDir::new().unwrap();
        let real_dir = dir.path().join("real");
        fs::create_dir(&real_dir).unwrap();
        fs::write(real_dir.join("big.txt"), vec![0u8; 10000]).unwrap();

        // Create a symlink to the real directory
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_dir, dir.path().join("link")).unwrap();

        // disk_usage of the parent should NOT double-count via the symlink
        let total = disk_usage(dir.path());
        let real_only = disk_usage(&real_dir);
        // Total should be approximately real_only (plus symlink entry itself),
        // NOT 2x real_only
        assert!(
            total < real_only * 2,
            "symlinks should not be followed: total={total}, real_only={real_only}"
        );
    }
}
