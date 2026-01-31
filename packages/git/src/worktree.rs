//! Worktree operations.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use std::path::{Path, PathBuf};
use std::process::Command;

use git2::Repository;

use crate::error::GitError;
use crate::repo::get_repo_root;

/// Information about a git worktree.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    /// Filesystem path to the worktree.
    pub path: PathBuf,
    /// Whether this is the main worktree.
    pub is_main: bool,
    /// Branch name if not detached.
    pub branch: Option<String>,
    /// Current commit hash.
    pub commit: Option<String>,
}

/// Options for creating a new worktree.
#[derive(Debug, Clone, Default)]
pub struct WorktreeCreateOptions {
    /// Branch to check out (existing branch).
    pub branch: Option<String>,
    /// Create and check out a new branch.
    pub new_branch: Option<String>,
    /// Create worktree with detached HEAD.
    pub detach: bool,
}

/// Get a list of all worktrees for a repository.
///
/// # Arguments
///
/// * `repo` - The repository
///
/// # Errors
///
/// * If the worktree list cannot be retrieved
/// * If worktree metadata cannot be read
pub fn get_worktrees(repo: &Repository) -> Result<Vec<WorktreeInfo>, GitError> {
    log::debug!("Getting worktree list");

    let mut worktrees = Vec::new();

    // First, add the main worktree
    let main_path = get_repo_root(repo)?;
    let main_info = get_worktree_info_from_repo(repo, &main_path, true)?;
    worktrees.push(main_info);

    // Then add linked worktrees
    let worktree_names = repo.worktrees().map_err(GitError::WorktreeListError)?;

    for name in worktree_names.iter().flatten() {
        if let Ok(wt) = repo.find_worktree(name) {
            if let Some(wt_path) = wt.path().parent() {
                // Open the worktree as a repo to get branch info
                if let Ok(wt_repo) = Repository::open(wt_path) {
                    if let Ok(info) = get_worktree_info_from_repo(&wt_repo, wt_path, false) {
                        worktrees.push(info);
                    }
                }
            }
        }
    }

    log::debug!("Found {} worktrees", worktrees.len());
    Ok(worktrees)
}

/// Get worktree info from a repository.
fn get_worktree_info_from_repo(
    repo: &Repository,
    path: &Path,
    is_main: bool,
) -> Result<WorktreeInfo, GitError> {
    let branch = if let Ok(head) = repo.head() {
        if head.is_branch() {
            head.shorthand().map(String::from)
        } else {
            None
        }
    } else {
        None
    };

    let commit = if let Ok(head) = repo.head() {
        head.target().map(|oid| oid.to_string()[..8].to_string())
    } else {
        None
    };

    Ok(WorktreeInfo {
        path: path.to_path_buf(),
        is_main,
        branch,
        commit,
    })
}

/// Get the main worktree.
///
/// # Arguments
///
/// * `repo` - The repository
///
/// # Errors
///
/// * If no main worktree is found
pub fn get_main_worktree(repo: &Repository) -> Result<WorktreeInfo, GitError> {
    let worktrees = get_worktrees(repo)?;
    worktrees
        .into_iter()
        .find(|wt| wt.is_main)
        .ok_or(GitError::NoMainWorktree)
}

/// Create a new worktree using git CLI.
///
/// We use the CLI here because git2's worktree API has tricky lifetime requirements
/// that make it difficult to set branch references.
///
/// # Arguments
///
/// * `repo` - The repository
/// * `path` - Path where the worktree should be created
/// * `options` - Creation options
///
/// # Errors
///
/// * If the worktree cannot be created
/// * If the specified branch does not exist
pub fn create_worktree(
    repo: &Repository,
    path: &Path,
    options: &WorktreeCreateOptions,
) -> Result<(), GitError> {
    log::info!("Creating worktree at {}", path.display());

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| GitError::InvalidPath(path.to_path_buf()))?;
    }

    let repo_root = get_repo_root(repo)?;

    // Build git worktree add command
    let mut args: Vec<&str> = vec!["worktree", "add"];

    if options.detach {
        args.push("--detach");
    }

    // Convert path to string for the command
    let path_str = path.to_string_lossy();

    // Handle new branch
    let new_branch_owned: String;
    if let Some(ref branch_name) = options.new_branch {
        args.push("-b");
        new_branch_owned = branch_name.clone();
        args.push(&new_branch_owned);
    }

    args.push(&path_str);

    // Handle existing branch
    let branch_owned: String;
    if let Some(ref branch_name) = options.branch {
        branch_owned = branch_name.clone();
        args.push(&branch_owned);
    }

    log::debug!("Running: git {}", args.join(" "));

    let output = Command::new("git")
        .args(&args)
        .current_dir(&repo_root)
        .output()
        .map_err(|e| GitError::WorktreeCreateError {
            path: path.to_path_buf(),
            source: git2::Error::from_str(&e.to_string()),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::WorktreeCreateError {
            path: path.to_path_buf(),
            source: git2::Error::from_str(&stderr),
        });
    }

    log::info!("Created worktree at {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_get_worktrees() {
        let (_dir, repo) = create_test_repo();
        let worktrees = get_worktrees(&repo).unwrap();
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].is_main);
    }

    #[test]
    fn test_get_main_worktree() {
        let (dir, repo) = create_test_repo();
        let main = get_main_worktree(&repo).unwrap();
        assert!(main.is_main);
        // Canonicalize both paths to handle macOS /var -> /private/var symlink
        let expected = dir.path().canonicalize().unwrap();
        let actual = main.path.canonicalize().unwrap();
        assert_eq!(actual, expected);
    }
}
