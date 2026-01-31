//! Fast file counting using jwalk.

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
    if !path.exists() {
        return 0;
    }

    // For symlinks, we don't count inside them
    if path.is_symlink() {
        return 0;
    }

    if path.is_file() {
        return 1;
    }

    if !path.is_dir() {
        return 0;
    }

    // Use jwalk with sort disabled for speed
    jwalk::WalkDir::new(path)
        .skip_hidden(false)
        .sort(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .count() as u64
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
}
