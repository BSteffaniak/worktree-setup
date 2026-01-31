//! File status detection.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use git2::{Repository, Status, StatusOptions};

use crate::error::GitError;

/// Get a list of unstaged and untracked files.
///
/// Returns file paths relative to the repository root.
///
/// # Arguments
///
/// * `repo` - The repository
///
/// # Errors
///
/// * If the repository status cannot be read
pub fn get_unstaged_and_untracked_files(repo: &Repository) -> Result<Vec<String>, GitError> {
    log::debug!("Getting unstaged and untracked files");

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true);

    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(GitError::StatusError)?;

    let mut files = Vec::new();

    for entry in statuses.iter() {
        let status = entry.status();

        // Check for unstaged modifications or untracked files
        let dominated_by_unstaged = status.intersects(
            Status::WT_MODIFIED
                | Status::WT_DELETED
                | Status::WT_TYPECHANGE
                | Status::WT_RENAMED
                | Status::WT_NEW,
        );

        if dominated_by_unstaged {
            if let Some(path) = entry.path() {
                files.push(path.to_string());
            }
        }
    }

    files.sort();
    files.dedup();

    log::debug!("Found {} unstaged/untracked files", files.len());
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_test_repo() -> (TempDir, Repository) {
        let dir = TempDir::new().unwrap();

        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        std::fs::write(dir.path().join("README.md"), "# Test").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let repo = Repository::open(dir.path()).unwrap();
        (dir, repo)
    }

    #[test]
    fn test_get_unstaged_files_empty() {
        let (_dir, repo) = create_test_repo();
        let files = get_unstaged_and_untracked_files(&repo).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_get_unstaged_files_with_changes() {
        let (dir, repo) = create_test_repo();

        // Create an untracked file
        std::fs::write(dir.path().join("untracked.txt"), "new file").unwrap();

        // Modify a tracked file
        std::fs::write(dir.path().join("README.md"), "# Modified").unwrap();

        let files = get_unstaged_and_untracked_files(&repo).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"README.md".to_string()));
        assert!(files.contains(&"untracked.txt".to_string()));
    }
}
