/**
 * Config file discovery and loading.
 */

import { glob } from "glob";
import { resolve, dirname, relative, basename } from "path";
import type { WorktreeConfig, LoadedConfig } from "./types.js";

/**
 * Config file patterns to search for.
 * - worktree.config.ts - default config
 * - worktree.*.config.ts - named variants (e.g., worktree.minimal.config.ts)
 */
const CONFIG_PATTERNS = ["**/worktree.config.ts", "**/worktree.*.config.ts"];

/**
 * Directories to ignore when searching for config files.
 */
const IGNORE_PATTERNS = ["**/node_modules/**", "**/dist/**", "**/.git/**", "**/build/**", "**/.next/**"];

/**
 * Extract variant name from config filename.
 * - worktree.config.ts -> null (default)
 * - worktree.minimal.config.ts -> "minimal"
 * - worktree.data-only.config.ts -> "data-only"
 */
function extractVariantName(filename: string): string | null {
  const match = filename.match(/^worktree\.(.+)\.config\.ts$/);
  return match ? match[1] : null;
}

/**
 * Discover all worktree config files in the repository.
 *
 * @param repoRoot - The root directory of the git repository
 * @returns Array of absolute paths to config files, sorted by path
 */
export async function discoverConfigFiles(repoRoot: string): Promise<string[]> {
  const configFiles: string[] = [];

  for (const pattern of CONFIG_PATTERNS) {
    const matches = await glob(pattern, {
      cwd: repoRoot,
      ignore: IGNORE_PATTERNS,
      absolute: true,
    });
    configFiles.push(...matches);
  }

  // Remove duplicates and sort
  const unique = [...new Set(configFiles)];
  unique.sort();

  return unique;
}

/**
 * Load a single config file and return it with metadata.
 *
 * @param configPath - Absolute path to the config file
 * @param repoRoot - The root directory of the git repository
 * @returns LoadedConfig with the config and metadata
 */
export async function loadConfigFile(configPath: string, repoRoot: string): Promise<LoadedConfig> {
  // Dynamically import the config file
  const module = await import(configPath);
  const config: WorktreeConfig = module.default;

  if (!config || typeof config !== "object") {
    throw new Error(`Invalid config file: ${configPath} - must export a default WorktreeConfig object`);
  }

  if (!config.description) {
    throw new Error(`Invalid config file: ${configPath} - missing required 'description' field`);
  }

  const configDir = dirname(configPath);
  const relativePath = relative(repoRoot, configPath);
  const variantName = extractVariantName(basename(configPath));

  return {
    config,
    configPath,
    configDir,
    relativePath,
    variantName,
  };
}

/**
 * Discover and load all worktree configs in the repository.
 *
 * @param repoRoot - The root directory of the git repository
 * @returns Array of LoadedConfig objects
 */
export async function loadAllConfigs(repoRoot: string): Promise<LoadedConfig[]> {
  const configFiles = await discoverConfigFiles(repoRoot);
  const loadedConfigs: LoadedConfig[] = [];

  for (const configPath of configFiles) {
    try {
      const loaded = await loadConfigFile(configPath, repoRoot);
      loadedConfigs.push(loaded);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.warn(`Warning: Failed to load ${configPath}: ${message}`);
    }
  }

  return loadedConfigs;
}

/**
 * Get a display name for a loaded config.
 * Used in interactive selection UI.
 */
export function getConfigDisplayName(loaded: LoadedConfig): string {
  const dir = dirname(loaded.relativePath);
  const variant = loaded.variantName;

  if (dir === ".") {
    // Root config
    return variant ? `(root) [${variant}]` : "(root)";
  }

  return variant ? `${dir} [${variant}]` : dir;
}
