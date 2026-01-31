/**
 * Interactive prompts using @inquirer/prompts.
 */

import { checkbox, input, select, confirm } from "@inquirer/prompts";
import { existsSync } from "fs";
import type { LoadedConfig } from "./types.js";
import { getConfigDisplayName } from "./config-loader.js";
import { getCurrentBranch, getLocalBranches, isExistingWorktree } from "./git.js";

/**
 * Prompt user to select which configs to apply.
 */
export async function selectConfigs(configs: LoadedConfig[]): Promise<LoadedConfig[]> {
  if (configs.length === 0) {
    return [];
  }

  if (configs.length === 1) {
    // Only one config, ask if they want to use it
    const use = await confirm({
      message: `Apply config: ${getConfigDisplayName(configs[0])} - ${configs[0].config.description}?`,
      default: true,
    });
    return use ? configs : [];
  }

  const choices = configs.map((config) => ({
    name: `${getConfigDisplayName(config)} - ${config.config.description}`,
    value: config,
    checked: true, // Default to all selected
  }));

  const selected = await checkbox({
    message: "Select worktree configs to apply:",
    choices,
  });

  return selected;
}

/**
 * Prompt user for the target worktree path.
 */
export async function promptWorktreePath(defaultPath?: string): Promise<string> {
  const path = await input({
    message: "Target worktree path:",
    default: defaultPath,
    validate: (value) => {
      if (!value.trim()) {
        return "Path is required";
      }
      return true;
    },
  });

  return path.trim();
}

/**
 * Options for creating a new worktree.
 */
export interface WorktreeCreateOptions {
  shouldCreate: boolean;
  branch?: string;
  newBranch?: string;
  detach?: boolean;
}

/**
 * Prompt user about creating a new worktree.
 */
export async function promptWorktreeCreate(targetPath: string): Promise<WorktreeCreateOptions> {
  // Check if path exists
  if (existsSync(targetPath)) {
    // Check if it's already a worktree
    const isWorktree = await isExistingWorktree(targetPath);
    if (isWorktree) {
      return { shouldCreate: false };
    }

    // Path exists but isn't a worktree - ask what to do
    const action = await select({
      message: `Path exists but is not a git worktree. What would you like to do?`,
      choices: [
        { name: "Cancel", value: "cancel" },
        { name: "Use existing directory (skip worktree creation)", value: "use" },
      ],
    });

    if (action === "cancel") {
      throw new Error("Operation cancelled");
    }

    return { shouldCreate: false };
  }

  // Path doesn't exist - offer to create worktree
  const currentBranch = await getCurrentBranch();
  const branches = await getLocalBranches();

  const createAction = await select({
    message: "Worktree path doesn't exist. Create from:",
    choices: [
      { name: `Current branch (${currentBranch || "HEAD"})`, value: "current" },
      { name: "Existing branch...", value: "branch" },
      { name: "New branch...", value: "new-branch" },
      { name: "Detached HEAD (current commit)", value: "detach" },
      { name: "Cancel", value: "cancel" },
    ],
  });

  if (createAction === "cancel") {
    throw new Error("Operation cancelled");
  }

  if (createAction === "current") {
    return {
      shouldCreate: true,
      branch: currentBranch || undefined,
      detach: !currentBranch,
    };
  }

  if (createAction === "detach") {
    return {
      shouldCreate: true,
      detach: true,
    };
  }

  if (createAction === "branch") {
    const branch = await select({
      message: "Select branch:",
      choices: branches.map((b) => ({ name: b, value: b })),
    });
    return {
      shouldCreate: true,
      branch,
    };
  }

  if (createAction === "new-branch") {
    const newBranch = await input({
      message: "New branch name:",
      validate: (value) => {
        if (!value.trim()) {
          return "Branch name is required";
        }
        if (branches.includes(value.trim())) {
          return "Branch already exists";
        }
        return true;
      },
    });
    return {
      shouldCreate: true,
      newBranch: newBranch.trim(),
    };
  }

  return { shouldCreate: false };
}

/**
 * Prompt user about copying node_modules.
 */
export async function promptCopyNodeModules(defaultValue = true): Promise<boolean> {
  return confirm({
    message: "Copy node_modules from main worktree?",
    default: defaultValue,
  });
}

/**
 * Prompt user about running post-setup commands.
 */
export async function promptRunInstall(defaultValue = true): Promise<boolean> {
  return confirm({
    message: "Run post-setup commands (e.g., bun install)?",
    default: defaultValue,
  });
}
