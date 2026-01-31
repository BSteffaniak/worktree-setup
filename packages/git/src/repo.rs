//! Repository discovery and basic operations.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::{Path, PathBuf};

use git2::Repository;

use crate::error::GitError;

/// Open a git repository at the specified path.
///
/// # Arguments
///
/// * `path` - Path to the repository root or any directory within it
///
/// # Errors
///
/// * If the path is not a git repository
/// * If the repository cannot be opened
pub fn open_repo(path: &Path) -> Result<Repository, GitError> {
    log::debug!("Opening repository at {}", path.display());

    Repository::open(path).map_err(|e| GitError::OpenError {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Discover a git repository by searching upward from the given path.
///
/// # Arguments
///
/// * `path` - Path to start searching from
///
/// # Errors
///
/// * If no repository is found
pub fn discover_repo(path: &Path) -> Result<Repository, GitError> {
    log::debug!("Discovering repository from {}", path.display());

    Repository::discover(path).map_err(|e| GitError::DiscoverError {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Get the root directory of a repository.
///
/// # Arguments
///
/// * `repo` - The repository
///
/// # Errors
///
/// * If the repository is bare (has no working directory)
pub fn get_repo_root(repo: &Repository) -> Result<PathBuf, GitError> {
    repo.workdir()
        .map(Path::to_path_buf)
        .ok_or(GitError::BareRepository)
}

/// Get the current branch name.
///
/// Returns `None` if HEAD is detached.
///
/// # Arguments
///
/// * `repo` - The repository
///
/// # Errors
///
/// * If the HEAD reference cannot be read
pub fn get_current_branch(repo: &Repository) -> Result<Option<String>, GitError> {
    let head = repo.head().map_err(GitError::HeadError)?;

    if head.is_branch() {
        Ok(head.shorthand().map(String::from))
    } else {
        Ok(None)
    }
}

/// Get a list of local branch names.
///
/// # Arguments
///
/// * `repo` - The repository
///
/// # Errors
///
/// * If the branch list cannot be retrieved
pub fn get_local_branches(repo: &Repository) -> Result<Vec<String>, GitError> {
    let branches = repo
        .branches(Some(git2::BranchType::Local))
        .map_err(GitError::BranchListError)?;

    let mut names = Vec::new();
    for branch in branches {
        let (branch, _) = branch.map_err(GitError::BranchListError)?;
        if let Some(name) = branch.name().map_err(GitError::BranchListError)? {
            names.push(name.to_string());
        }
    }

    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_test_repo() -> (TempDir, Repository) {
        let dir = TempDir::new().unwrap();

        // Initialize a git repo
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

        // Create initial commit
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
    fn test_open_repo() {
        let (dir, _) = create_test_repo();
        let repo = open_repo(dir.path()).unwrap();
        assert!(repo.workdir().is_some());
    }

    #[test]
    fn test_get_repo_root() {
        let (dir, repo) = create_test_repo();
        let root = get_repo_root(&repo).unwrap();
        // Canonicalize both paths to handle macOS /var -> /private/var symlink
        let expected = dir.path().canonicalize().unwrap();
        let actual = root.canonicalize().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_get_current_branch() {
        let (_dir, repo) = create_test_repo();
        let branch = get_current_branch(&repo).unwrap();
        // Git defaults to "master" or "main" depending on config
        assert!(branch.is_some());
    }
}
