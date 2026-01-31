/**
 * Git worktree operations.
 */

import { mkdirSync } from "fs";
import { dirname } from "path";
import { $ } from "bun";

export interface WorktreeInfo {
  path: string;
  isMain: boolean;
  branch: string | null;
  commit: string | null;
}

/**
 * Get the root directory of the current git repository.
 */
export async function getRepoRoot(): Promise<string> {
  const output = await $`git rev-parse --show-toplevel`.text();
  return output.trim();
}

/**
 * Get list of all worktrees for the repository.
 */
export async function getWorktrees(): Promise<WorktreeInfo[]> {
  const output = await $`git worktree list --porcelain`.text();
  const worktrees: WorktreeInfo[] = [];

  let currentPath = "";
  let currentBranch: string | null = null;
  let currentCommit: string | null = null;

  for (const line of output.split("\n")) {
    if (line.startsWith("worktree ")) {
      currentPath = line.slice(9);
    } else if (line.startsWith("HEAD ")) {
      currentCommit = line.slice(5);
    } else if (line.startsWith("branch ")) {
      currentBranch = line.slice(7).replace("refs/heads/", "");
    } else if (line === "bare" || line === "detached" || line === "") {
      if (currentPath) {
        worktrees.push({
          path: currentPath,
          isMain: worktrees.length === 0,
          branch: currentBranch,
          commit: currentCommit,
        });
        currentPath = "";
        currentBranch = null;
        currentCommit = null;
      }
    }
  }

  return worktrees;
}

/**
 * Get the main (first) worktree of the repository.
 */
export async function getMainWorktree(): Promise<WorktreeInfo> {
  const worktrees = await getWorktrees();
  const main = worktrees.find((w) => w.isMain);
  if (!main) {
    throw new Error("Could not find main worktree");
  }
  return main;
}

/**
 * Check if a path is an existing worktree.
 */
export async function isExistingWorktree(path: string): Promise<boolean> {
  const worktrees = await getWorktrees();
  return worktrees.some((w) => w.path === path);
}

/**
 * Get the current branch name.
 */
export async function getCurrentBranch(): Promise<string | null> {
  try {
    const output = await $`git rev-parse --abbrev-ref HEAD`.text();
    const branch = output.trim();
    return branch === "HEAD" ? null : branch;
  } catch {
    return null;
  }
}

/**
 * Get list of local branches.
 */
export async function getLocalBranches(): Promise<string[]> {
  const output = await $`git branch --format="%(refname:short)"`.text();
  return output
    .split("\n")
    .map((b) => b.trim())
    .filter(Boolean);
}

/**
 * Create a new worktree.
 *
 * @param path - Path where the worktree will be created
 * @param options - Options for creating the worktree
 */
export async function createWorktree(
  path: string,
  options: {
    branch?: string;
    newBranch?: string;
    commit?: string;
    detach?: boolean;
  } = {},
): Promise<void> {
  // Ensure parent directory exists
  mkdirSync(dirname(path), { recursive: true });

  const args: string[] = ["git", "worktree", "add"];

  if (options.detach) {
    args.push("--detach");
  }

  if (options.newBranch) {
    args.push("-b", options.newBranch);
  }

  args.push(path);

  if (options.branch) {
    args.push(options.branch);
  } else if (options.commit) {
    args.push(options.commit);
  }

  // Use shell array expansion
  const cmd = args.join(" ");
  const result = await $`sh -c ${cmd}`.nothrow();
  if (result.exitCode !== 0) {
    const stderr = result.stderr.toString().trim();
    throw new Error(`Git worktree creation failed (exit code ${result.exitCode}): ${stderr}`);
  }
}

/**
 * Remove a worktree.
 */
export async function removeWorktree(path: string, force = false): Promise<void> {
  const args = force ? ["git", "worktree", "remove", path, "--force"] : ["git", "worktree", "remove", path];
  const cmd = args.join(" ");
  const result = await $`sh -c ${cmd}`.nothrow();
  if (result.exitCode !== 0) {
    const stderr = result.stderr.toString().trim();
    throw new Error(`Git worktree removal failed (exit code ${result.exitCode}): ${stderr}`);
  }
}
