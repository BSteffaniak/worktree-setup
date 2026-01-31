/**
 * File system operations for worktree setup.
 */

import { existsSync, mkdirSync, symlinkSync, lstatSync, copyFileSync, cpSync, rmSync, statSync } from "fs";
import { dirname, join, relative } from "path";
import { glob } from "glob";
import { $ } from "bun";
import type { LoadedConfig, TemplateConfig } from "./types.js";

export type OperationResult = "created" | "exists" | "skipped";

/**
 * Ensure a directory exists, creating parent directories as needed.
 */
function ensureParentDir(filePath: string): void {
  const dir = dirname(filePath);
  if (!existsSync(dir)) {
    mkdirSync(dir, { recursive: true });
  }
}

/**
 * Check if a path is a symlink.
 */
function isSymlink(path: string): boolean {
  try {
    return lstatSync(path).isSymbolicLink();
  } catch {
    return false;
  }
}

/**
 * Check if a path is a directory.
 */
function isDirectory(path: string): boolean {
  try {
    return statSync(path).isDirectory();
  } catch {
    return false;
  }
}

/**
 * Create a symlink from source to target.
 */
export function createSymlink(source: string, target: string): OperationResult {
  if (isSymlink(target)) {
    return "exists";
  }

  if (!existsSync(source)) {
    return "skipped";
  }

  ensureParentDir(target);

  // Remove existing directory/file if it exists (but not a symlink)
  if (existsSync(target)) {
    rmSync(target, { recursive: true });
  }

  symlinkSync(source, target);
  return "created";
}

/**
 * Copy a file from source to target.
 */
export function copyFile(source: string, target: string): OperationResult {
  if (!existsSync(source)) {
    return "skipped";
  }

  if (existsSync(target)) {
    return "exists";
  }

  ensureParentDir(target);
  copyFileSync(source, target);
  return "created";
}

/**
 * Copy a directory recursively from source to target.
 */
export function copyDirectory(source: string, target: string): OperationResult {
  if (!existsSync(source)) {
    return "skipped";
  }

  if (existsSync(target)) {
    return "exists";
  }

  ensureParentDir(target);
  cpSync(source, target, { recursive: true });
  return "created";
}

/**
 * Copy a file or directory from source to target.
 */
export function copyPath(source: string, target: string): OperationResult {
  if (!existsSync(source)) {
    return "skipped";
  }

  if (isDirectory(source)) {
    return copyDirectory(source, target);
  } else {
    return copyFile(source, target);
  }
}

/**
 * Get approximate directory size using du.
 */
export async function getDirectorySize(dir: string): Promise<number> {
  if (!existsSync(dir)) return 0;

  try {
    const output = await $`du -sk ${dir}`.text();
    const kb = parseInt(output.split("\t")[0], 10);
    return kb * 1024;
  } catch {
    return 0;
  }
}

/**
 * Format bytes as human-readable size.
 */
export function formatSize(bytes: number): string {
  const units = ["B", "KB", "MB", "GB"];
  let size = bytes;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex++;
  }
  return `${size.toFixed(1)} ${units[unitIndex]}`;
}

/**
 * Result of applying a single config.
 */
export interface ConfigApplyResult {
  config: LoadedConfig;
  symlinks: Array<{ path: string; result: OperationResult }>;
  copies: Array<{ path: string; result: OperationResult }>;
  templates: Array<{ source: string; target: string; result: OperationResult }>;
}

/**
 * Apply a loaded config to a target worktree.
 *
 * @param loaded - The loaded config to apply
 * @param mainWorktreePath - Path to the main worktree (source)
 * @param targetWorktreePath - Path to the target worktree (destination)
 */
export async function applyConfig(
  loaded: LoadedConfig,
  mainWorktreePath: string,
  targetWorktreePath: string,
): Promise<ConfigApplyResult> {
  const { config, configDir } = loaded;

  // Calculate the relative path from repo root to the config directory
  const configRelativeDir = relative(mainWorktreePath, configDir);

  const result: ConfigApplyResult = {
    config: loaded,
    symlinks: [],
    copies: [],
    templates: [],
  };

  // Process symlinks
  if (config.symlinks) {
    for (const symlinkPath of config.symlinks) {
      const sourcePath = join(mainWorktreePath, configRelativeDir, symlinkPath);
      const targetPath = join(targetWorktreePath, configRelativeDir, symlinkPath);
      const opResult = createSymlink(sourcePath, targetPath);
      result.symlinks.push({ path: join(configRelativeDir, symlinkPath), result: opResult });
    }
  }

  // Process explicit copies
  if (config.copy) {
    for (const copyPath of config.copy) {
      const sourcePath = join(mainWorktreePath, configRelativeDir, copyPath);
      const targetPath = join(targetWorktreePath, configRelativeDir, copyPath);
      const opResult = copyFile(sourcePath, targetPath);
      result.copies.push({ path: join(configRelativeDir, copyPath), result: opResult });
    }
  }

  // Process glob copies
  if (config.copyGlob) {
    for (const pattern of config.copyGlob) {
      const matches = await glob(pattern, {
        cwd: join(mainWorktreePath, configRelativeDir),
        ignore: ["**/node_modules/**"],
        dot: true,
      });

      for (const match of matches) {
        const sourcePath = join(mainWorktreePath, configRelativeDir, match);
        const targetPath = join(targetWorktreePath, configRelativeDir, match);
        const opResult = copyFile(sourcePath, targetPath);
        result.copies.push({ path: join(configRelativeDir, match), result: opResult });
      }
    }
  }

  // Process templates
  if (config.templates) {
    for (const template of config.templates) {
      const sourcePath = join(mainWorktreePath, configRelativeDir, template.source);
      const targetPath = join(targetWorktreePath, configRelativeDir, template.target);

      // Only copy if target doesn't exist
      let opResult: OperationResult;
      if (existsSync(targetPath)) {
        opResult = "exists";
      } else if (!existsSync(sourcePath)) {
        opResult = "skipped";
      } else {
        ensureParentDir(targetPath);
        copyFileSync(sourcePath, targetPath);
        opResult = "created";
      }

      result.templates.push({
        source: join(configRelativeDir, template.source),
        target: join(configRelativeDir, template.target),
        result: opResult,
      });
    }
  }

  return result;
}

/**
 * Copy node_modules from main worktree to target.
 */
export async function copyNodeModules(
  mainWorktreePath: string,
  targetWorktreePath: string,
  onProgress?: (message: string) => void,
): Promise<OperationResult> {
  const sourceNodeModules = join(mainWorktreePath, "node_modules");
  const targetNodeModules = join(targetWorktreePath, "node_modules");

  if (existsSync(targetNodeModules)) {
    return "exists";
  }

  if (!existsSync(sourceNodeModules)) {
    return "skipped";
  }

  const size = await getDirectorySize(sourceNodeModules);
  onProgress?.(`Copying node_modules (${formatSize(size)})...`);

  cpSync(sourceNodeModules, targetNodeModules, { recursive: true });

  return "created";
}

/**
 * Run post-setup commands in the target worktree.
 */
export async function runPostSetupCommands(
  commands: string[],
  targetWorktreePath: string,
  onCommand?: (command: string) => void,
): Promise<void> {
  for (const command of commands) {
    onCommand?.(command);
    await $`sh -c ${command}`.cwd(targetWorktreePath);
  }
}
