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
    /// Force creation even if the path is already registered as a worktree.
    pub force: bool,
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
    let main_info = get_worktree_info_from_repo(repo, &main_path, true);
    worktrees.push(main_info);

    // Then add linked worktrees
    let worktree_names = repo.worktrees().map_err(GitError::WorktreeListError)?;

    for name in worktree_names.iter().flatten() {
        if let Ok(wt) = repo.find_worktree(name) {
            let wt_path = wt.path();
            // Open the worktree as a repo to get branch info
            if let Ok(wt_repo) = Repository::open(wt_path) {
                worktrees.push(get_worktree_info_from_repo(&wt_repo, wt_path, false));
            }
        }
    }

    log::debug!("Found {} worktrees", worktrees.len());
    Ok(worktrees)
}

/// Get worktree info from a repository.
fn get_worktree_info_from_repo(repo: &Repository, path: &Path, is_main: bool) -> WorktreeInfo {
    let branch = repo.head().ok().and_then(|head| {
        if head.is_branch() {
            head.shorthand().map(String::from)
        } else {
            None
        }
    });

    let commit = repo
        .head()
        .ok()
        .and_then(|head| head.target().map(|oid| oid.to_string()[..8].to_string()));

    WorktreeInfo {
        path: path.to_path_buf(),
        is_main,
        branch,
        commit,
    }
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

    if options.force {
        args.push("-f");
    }

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

/// Prune stale worktree registrations using the git CLI.
///
/// Removes worktree entries whose directories no longer exist on disk.
///
/// # Arguments
///
/// * `repo` - The repository
///
/// # Errors
///
/// * If the prune command fails
pub fn prune_worktrees(repo: &Repository) -> Result<(), GitError> {
    let repo_root = get_repo_root(repo)?;

    log::info!("Pruning stale worktrees");

    let output = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| GitError::WorktreePruneError(git2::Error::from_str(&e.to_string())))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::WorktreePruneError(git2::Error::from_str(&stderr)));
    }

    log::info!("Pruned stale worktrees");
    Ok(())
}

/// Remove a linked worktree using the git CLI.
///
/// This removes the worktree directory and its git tracking. The main
/// worktree cannot be removed — attempting to do so returns an error.
///
/// # Arguments
///
/// * `repo` - The repository
/// * `worktree_path` - Path to the worktree to remove
/// * `force` - If true, pass `--force` to allow removal even with
///   uncommitted changes or if the working tree is dirty
///
/// # Errors
///
/// * If `worktree_path` is the main worktree
/// * If the git CLI command fails
pub fn remove_worktree(
    repo: &Repository,
    worktree_path: &Path,
    force: bool,
) -> Result<(), GitError> {
    // Guard: never remove the main worktree
    let repo_root = get_repo_root(repo)?;
    let main_canonical = repo_root
        .canonicalize()
        .map_err(|_| GitError::CannotRemoveMainWorktree(repo_root.to_string_lossy().to_string()))?;
    if let Ok(target_canonical) = worktree_path.canonicalize()
        && target_canonical == main_canonical
    {
        return Err(GitError::CannotRemoveMainWorktree(
            worktree_path.to_string_lossy().to_string(),
        ));
    }

    log::info!("Removing worktree at {}", worktree_path.display());

    let mut args: Vec<&str> = vec!["worktree", "remove"];

    if force {
        args.push("--force");
    }

    let path_str = worktree_path.to_string_lossy();
    args.push(&path_str);

    log::debug!("Running: git {}", args.join(" "));

    let output = Command::new("git")
        .args(&args)
        .current_dir(&repo_root)
        .output()
        .map_err(|e| GitError::WorktreeRemoveError {
            path: worktree_path.to_string_lossy().to_string(),
            message: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::WorktreeRemoveError {
            path: worktree_path.to_string_lossy().to_string(),
            message: stderr.trim().to_string(),
        });
    }

    log::info!("Removed worktree at {}", worktree_path.display());
    Ok(())
}

/// Delete a local branch using the git CLI.
///
/// # Arguments
///
/// * `repo` - The repository
/// * `branch` - Name of the branch to delete
/// * `force` - If true, uses `-D` (force delete) instead of `-d`
///   (safe delete). Safe delete refuses to delete branches that have
///   not been fully merged into their upstream or HEAD.
///
/// # Errors
///
/// * If the git CLI command fails (e.g., branch not found or not merged)
pub fn delete_branch(repo: &Repository, branch: &str, force: bool) -> Result<(), GitError> {
    let repo_root = get_repo_root(repo)?;
    let flag = if force { "-D" } else { "-d" };

    log::info!("Deleting branch '{branch}' (force={force})");
    log::debug!("Running: git branch {flag} {branch}");

    let output = Command::new("git")
        .args(["branch", flag, branch])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| GitError::BranchDeleteError {
            branch: branch.to_string(),
            message: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::BranchDeleteError {
            branch: branch.to_string(),
            message: stderr.trim().to_string(),
        });
    }

    log::info!("Deleted branch '{branch}'");
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

    #[test]
    fn test_get_worktrees_finds_linked() {
        let (dir, repo) = create_test_repo();

        // Create a linked worktree
        let wt_path = dir.path().join("linked-wt");
        Command::new("git")
            .args(["worktree", "add", "-b", "linked-branch"])
            .arg(&wt_path)
            .current_dir(dir.path())
            .output()
            .unwrap();

        let worktrees = get_worktrees(&repo).unwrap();

        assert_eq!(worktrees.len(), 2, "should find main + linked worktree");

        let main = worktrees.iter().find(|w| w.is_main).unwrap();
        let linked = worktrees.iter().find(|w| !w.is_main).unwrap();

        // Main worktree
        let expected_main = dir.path().canonicalize().unwrap();
        let actual_main = main.path.canonicalize().unwrap();
        assert_eq!(actual_main, expected_main);

        // Linked worktree
        let expected_linked = wt_path.canonicalize().unwrap();
        let actual_linked = linked.path.canonicalize().unwrap();
        assert_eq!(actual_linked, expected_linked);
        assert_eq!(linked.branch.as_deref(), Some("linked-branch"));

        // Clean up
        Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&wt_path)
            .current_dir(dir.path())
            .output()
            .unwrap();
    }

    #[test]
    fn test_remove_worktree() {
        let (dir, repo) = create_test_repo();

        // Create a linked worktree
        let wt_path = dir.path().join("to-remove");
        Command::new("git")
            .args(["worktree", "add", "-b", "remove-branch"])
            .arg(&wt_path)
            .current_dir(dir.path())
            .output()
            .unwrap();

        assert!(wt_path.exists(), "worktree dir should exist before removal");
        assert_eq!(get_worktrees(&repo).unwrap().len(), 2);

        // Remove it
        remove_worktree(&repo, &wt_path, false).unwrap();

        assert!(
            !wt_path.exists(),
            "worktree dir should be gone after removal"
        );

        // Re-open repo to get fresh worktree list (git2 caches)
        let repo = Repository::open(dir.path()).unwrap();
        let worktrees = get_worktrees(&repo).unwrap();
        assert_eq!(worktrees.len(), 1, "only main worktree should remain");
        assert!(worktrees[0].is_main);
    }

    #[test]
    fn test_remove_worktree_force() {
        let (dir, repo) = create_test_repo();

        // Create a linked worktree
        let wt_path = dir.path().join("dirty-wt");
        Command::new("git")
            .args(["worktree", "add", "-b", "dirty-branch"])
            .arg(&wt_path)
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Make it dirty (uncommitted changes)
        std::fs::write(wt_path.join("dirty-file.txt"), "uncommitted").unwrap();
        Command::new("git")
            .args(["add", "dirty-file.txt"])
            .current_dir(&wt_path)
            .output()
            .unwrap();

        // Non-force removal should fail on dirty worktree
        let result = remove_worktree(&repo, &wt_path, false);
        assert!(
            result.is_err(),
            "non-force removal of dirty worktree should fail"
        );

        // Force removal should succeed
        remove_worktree(&repo, &wt_path, true).unwrap();
        assert!(
            !wt_path.exists(),
            "worktree should be gone after force removal"
        );
    }

    #[test]
    fn test_remove_main_worktree_rejected() {
        let (dir, repo) = create_test_repo();

        let result = remove_worktree(&repo, dir.path(), false);
        assert!(result.is_err(), "removing main worktree should fail");

        match result.unwrap_err() {
            GitError::CannotRemoveMainWorktree(_) => {}
            other => panic!("expected CannotRemoveMainWorktree, got: {other}"),
        }
    }

    #[test]
    fn test_delete_branch() {
        let (dir, repo) = create_test_repo();

        // Create a branch (merged into current HEAD, so -d will work)
        Command::new("git")
            .args(["branch", "to-delete"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Verify branch exists
        let output = Command::new("git")
            .args(["branch", "--list", "to-delete"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("to-delete"),
            "branch should exist before delete"
        );

        // Delete it (safe mode)
        delete_branch(&repo, "to-delete", false).unwrap();

        // Verify branch is gone
        let output = Command::new("git")
            .args(["branch", "--list", "to-delete"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("to-delete"),
            "branch should be gone after delete"
        );
    }

    #[test]
    fn test_delete_branch_force() {
        let (dir, repo) = create_test_repo();

        // Create a worktree with a new branch, add a commit, then remove the worktree.
        // The branch will be "unmerged" relative to the main branch.
        let wt_path = dir.path().join("unmerged-wt");
        Command::new("git")
            .args(["worktree", "add", "-b", "unmerged-branch"])
            .arg(&wt_path)
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Add a commit on the unmerged branch
        std::fs::write(wt_path.join("new-file.txt"), "content").unwrap();
        Command::new("git")
            .args(["add", "new-file.txt"])
            .current_dir(&wt_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "unmerged commit"])
            .current_dir(&wt_path)
            .output()
            .unwrap();

        // Remove the worktree (force because it has extra commit)
        remove_worktree(&repo, &wt_path, true).unwrap();

        // Safe delete should fail (branch is not merged)
        let result = delete_branch(&repo, "unmerged-branch", false);
        assert!(
            result.is_err(),
            "safe delete of unmerged branch should fail"
        );

        // Force delete should succeed
        delete_branch(&repo, "unmerged-branch", true).unwrap();

        // Verify branch is gone
        let output = Command::new("git")
            .args(["branch", "--list", "unmerged-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("unmerged-branch"),
            "branch should be gone after force delete"
        );
    }

    #[test]
    fn test_delete_nonexistent_branch_fails() {
        let (_dir, repo) = create_test_repo();

        let result = delete_branch(&repo, "does-not-exist", false);
        assert!(result.is_err(), "deleting nonexistent branch should fail");

        match result.unwrap_err() {
            GitError::BranchDeleteError { branch, .. } => {
                assert_eq!(branch, "does-not-exist");
            }
            other => panic!("expected BranchDeleteError, got: {other}"),
        }
    }
}
