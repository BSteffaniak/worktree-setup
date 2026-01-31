/**
 * Type definitions for worktree-setup configuration files.
 */

/**
 * Template file configuration: copy source to target if target doesn't exist.
 */
export interface TemplateConfig {
  /** Source file path (relative to config file directory) */
  source: string;
  /** Target file path (relative to config file directory) */
  target: string;
}

/**
 * Configuration for a worktree setup.
 * All paths are relative to the directory containing the worktree.config.ts file.
 */
export interface WorktreeConfig {
  /** Human-readable description shown in interactive selection UI */
  description: string;

  /**
   * Directories to symlink from main worktree.
   * These directories will be symlinked rather than copied, saving disk space.
   * Ideal for large cached/generated directories that don't change per-branch.
   */
  symlinks?: string[];

  /**
   * Files or directories to copy exactly from main worktree.
   * These are copied (not symlinked) so changes don't affect the main worktree.
   */
  copy?: string[];

  /**
   * Glob patterns for files to copy from main worktree.
   * Patterns are relative to the config file's directory.
   * Example: ["**\/.env.local", "**\/.dev.vars"]
   */
  copyGlob?: string[];

  /**
   * Template files: copy source to target if target doesn't exist.
   * Useful for creating initial files from examples.
   */
  templates?: TemplateConfig[];

  /**
   * Whether to copy node_modules from main worktree.
   * Can be overridden via CLI --no-node-modules flag.
   * @default true
   */
  copyNodeModules?: boolean;

  /**
   * Commands to run after setup is complete.
   * Commands are run from the worktree root directory.
   * Example: ["bun install", "bun run generate:types"]
   */
  postSetup?: string[];
}

/**
 * Loaded config with metadata about its source location.
 */
export interface LoadedConfig {
  /** The configuration object */
  config: WorktreeConfig;
  /** Absolute path to the config file */
  configPath: string;
  /** Directory containing the config file (for resolving relative paths) */
  configDir: string;
  /** Path relative to repo root, for display purposes */
  relativePath: string;
  /** Display name derived from filename (e.g., "minimal" from worktree.minimal.config.ts) */
  variantName: string | null;
}

/**
 * Helper function to define a worktree configuration with type checking.
 * Use this in your worktree.config.ts files for better IDE support.
 *
 * @example
 * ```typescript
 * import { defineWorktreeConfig } from "worktree-setup";
 *
 * export default defineWorktreeConfig({
 *   description: "My project development environment",
 *   symlinks: ["data/cache"],
 *   copy: [".env.local"],
 *   postSetup: ["bun install"],
 * });
 * ```
 */
export function defineWorktreeConfig(config: WorktreeConfig): WorktreeConfig {
  return config;
}
