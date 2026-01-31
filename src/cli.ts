#!/usr/bin/env bun
/**
 * worktree-setup CLI
 *
 * A tool for setting up git worktrees with project-specific configurations.
 */

import { program } from "commander";
import { resolve } from "path";
import { existsSync } from "fs";

import { loadAllConfigs, getConfigDisplayName } from "./config-loader.js";
import { getRepoRoot, getMainWorktree, createWorktree } from "./git.js";
import { applyConfig, copyNodeModules, runPostSetupCommands, type OperationResult } from "./operations.js";
import {
  selectConfigs,
  promptWorktreePath,
  promptWorktreeCreate,
  promptCopyNodeModules,
  promptRunInstall,
} from "./interactive.js";
import type { LoadedConfig } from "./types.js";

const VERSION = "0.1.0";

/**
 * Print a result icon.
 */
function resultIcon(result: OperationResult): string {
  switch (result) {
    case "created":
      return "\u2713"; // checkmark
    case "exists":
      return "\u2022"; // bullet
    case "skipped":
      return "\u2717"; // x
  }
}

/**
 * Print a result status message.
 */
function resultStatus(result: OperationResult): string {
  switch (result) {
    case "created":
      return "";
    case "exists":
      return "(already exists)";
    case "skipped":
      return "(source not found)";
  }
}

/**
 * Main CLI entry point.
 */
async function main(): Promise<void> {
  program
    .name("worktree-setup")
    .description("Set up git worktrees with project-specific configurations")
    .version(VERSION)
    .argument("[target-path]", "Path to the target worktree")
    .option("--branch <branch>", "Create worktree from this branch")
    .option("--new-branch <name>", "Create a new branch for the worktree")
    .option("--config <path>", "Specific config file to use (can be specified multiple times)", collect, [])
    .option("--no-node-modules", "Skip copying node_modules")
    .option("--no-install", "Skip running post-setup commands")
    .option("--list", "List discovered configs and exit")
    .option("--non-interactive", "Run without prompts (requires target-path)")
    .action(runSetup);

  await program.parseAsync();
}

/**
 * Collect multiple values for an option.
 */
function collect(value: string, previous: string[]): string[] {
  return previous.concat([value]);
}

/**
 * Main setup logic.
 */
async function runSetup(
  targetPathArg: string | undefined,
  options: {
    branch?: string;
    newBranch?: string;
    config: string[];
    nodeModules: boolean;
    install: boolean;
    list: boolean;
    nonInteractive: boolean;
  },
): Promise<void> {
  try {
    // Get repo root
    const repoRoot = await getRepoRoot();
    console.log(`\n\u{1F333} Worktree Setup\n`);
    console.log(`Repository: ${repoRoot}\n`);

    // Load all configs
    const allConfigs = await loadAllConfigs(repoRoot);

    if (allConfigs.length === 0) {
      console.log("No worktree.config.ts files found in the repository.");
      console.log("Create a worktree.config.ts file to define your setup configuration.");
      process.exit(0);
    }

    console.log(`Found ${allConfigs.length} config${allConfigs.length === 1 ? "" : "s"}:`);
    for (const config of allConfigs) {
      console.log(`  \u2022 ${config.relativePath} - ${config.config.description}`);
    }
    console.log();

    // If --list, just exit
    if (options.list) {
      process.exit(0);
    }

    // Filter configs if specific ones requested
    let selectedConfigs: LoadedConfig[];
    if (options.config.length > 0) {
      selectedConfigs = allConfigs.filter((c) =>
        options.config.some((pattern) => c.relativePath.includes(pattern) || c.configPath.includes(pattern)),
      );
      if (selectedConfigs.length === 0) {
        console.error("No configs matched the specified patterns.");
        process.exit(1);
      }
    } else if (options.nonInteractive) {
      // Non-interactive mode uses all configs
      selectedConfigs = allConfigs;
    } else {
      // Interactive mode - let user select
      selectedConfigs = await selectConfigs(allConfigs);
      if (selectedConfigs.length === 0) {
        console.log("No configs selected. Exiting.");
        process.exit(0);
      }
    }

    // Get target path
    let targetPath: string;
    if (targetPathArg) {
      targetPath = resolve(targetPathArg);
    } else if (options.nonInteractive) {
      console.error("Target path is required in non-interactive mode.");
      process.exit(1);
    } else {
      targetPath = resolve(await promptWorktreePath());
    }

    // Get main worktree
    const mainWorktree = await getMainWorktree();

    // Check if target is the main worktree
    if (resolve(targetPath) === resolve(mainWorktree.path)) {
      console.error("Error: Cannot set up the main worktree.");
      console.error("This tool is for setting up secondary worktrees.");
      process.exit(1);
    }

    // Handle worktree creation
    if (!existsSync(targetPath)) {
      if (options.nonInteractive) {
        // Create with provided options
        console.log(`Creating worktree at ${targetPath}...`);
        await createWorktree(targetPath, {
          branch: options.branch,
          newBranch: options.newBranch,
          detach: !options.branch && !options.newBranch,
        });
      } else {
        // Interactive creation
        const createOptions = await promptWorktreeCreate(targetPath);
        if (createOptions.shouldCreate) {
          console.log(`\nCreating worktree at ${targetPath}...`);
          await createWorktree(targetPath, createOptions);
        }
      }
    }

    // Verify target exists now
    if (!existsSync(targetPath)) {
      console.error(`Error: Target path does not exist: ${targetPath}`);
      process.exit(1);
    }

    console.log(`\nSetting up worktree: ${targetPath}`);
    console.log(`Main worktree: ${mainWorktree.path}\n`);

    // Determine if we should copy node_modules
    let shouldCopyNodeModules = options.nodeModules;
    // Check if any config explicitly sets copyNodeModules
    const configNodeModulesSetting = selectedConfigs.find((c) => c.config.copyNodeModules !== undefined);
    if (configNodeModulesSetting) {
      shouldCopyNodeModules = shouldCopyNodeModules && (configNodeModulesSetting.config.copyNodeModules ?? true);
    }

    if (!options.nonInteractive && options.nodeModules) {
      shouldCopyNodeModules = await promptCopyNodeModules(shouldCopyNodeModules);
    }

    // Copy node_modules
    if (shouldCopyNodeModules) {
      process.stdout.write("Copying node_modules... ");
      const result = await copyNodeModules(mainWorktree.path, targetPath);
      if (result === "created") {
        console.log("done");
      } else if (result === "exists") {
        console.log("already exists");
      } else {
        console.log("skipped (not found in main worktree)");
      }
      console.log();
    }

    // Apply each selected config
    for (const loaded of selectedConfigs) {
      console.log(`[${getConfigDisplayName(loaded)}] Applying config...`);

      const result = await applyConfig(loaded, mainWorktree.path, targetPath);

      // Print symlinks
      if (result.symlinks.length > 0) {
        console.log("  Symlinks:");
        for (const { path, result: opResult } of result.symlinks) {
          console.log(`    ${resultIcon(opResult)} ${path} ${resultStatus(opResult)}`);
        }
      }

      // Print copies
      if (result.copies.length > 0) {
        console.log("  Copies:");
        for (const { path, result: opResult } of result.copies) {
          console.log(`    ${resultIcon(opResult)} ${path} ${resultStatus(opResult)}`);
        }
      }

      // Print templates
      if (result.templates.length > 0) {
        console.log("  Templates:");
        for (const { source, target, result: opResult } of result.templates) {
          console.log(`    ${resultIcon(opResult)} ${source} -> ${target} ${resultStatus(opResult)}`);
        }
      }

      console.log();
    }

    // Collect all post-setup commands
    const allPostSetupCommands: string[] = [];
    for (const loaded of selectedConfigs) {
      if (loaded.config.postSetup) {
        allPostSetupCommands.push(...loaded.config.postSetup);
      }
    }

    // Deduplicate commands
    const uniqueCommands = [...new Set(allPostSetupCommands)];

    // Run post-setup commands
    if (uniqueCommands.length > 0 && options.install) {
      let shouldRun = true;
      if (!options.nonInteractive) {
        shouldRun = await promptRunInstall(true);
      }

      if (shouldRun) {
        console.log("Running post-setup commands:");
        await runPostSetupCommands(uniqueCommands, targetPath, (cmd) => {
          console.log(`  $ ${cmd}`);
        });
        console.log();
      }
    }

    console.log("\u2705 Worktree setup complete!");
  } catch (error) {
    if (error instanceof Error && error.message === "Operation cancelled") {
      console.log("\nOperation cancelled.");
      process.exit(0);
    }
    throw error;
  }
}

main().catch((error) => {
  console.error("Error:", error.message || error);
  process.exit(1);
});
