# worktree-setup

A CLI tool for setting up git worktrees with project-specific configurations.

## Problem

Git worktrees are great for working on multiple branches in parallel, but setting them up often requires manual steps:

- Copying environment files (`.env.local`, `.dev.vars`)
- Symlinking large data directories to save disk space
- Copying configuration files with local modifications
- Running `npm install` or `bun install`
- Running project-specific setup commands

This tool automates all of that with a simple config file.

## Installation

```bash
# Install globally with bun
bun add -g worktree-setup

# Or with npm
npm install -g worktree-setup
```

## Usage

### Interactive Mode

Run from anywhere inside a git repository:

```bash
worktree-setup
```

The tool will:
1. Discover all `worktree.config.ts` files in the repo
2. Let you select which configs to apply
3. Prompt for the target worktree path
4. Optionally create the worktree if it doesn't exist
5. Apply the selected configurations

### Non-Interactive Mode

```bash
# Specify target path directly
worktree-setup ../my-feature-branch

# Create worktree from a branch
worktree-setup --branch feature/my-feature ../my-feature-branch

# Create a new branch
worktree-setup --new-branch feature/new-thing ../new-thing

# Use specific config file(s)
worktree-setup --config apps/my-app ../path

# Skip node_modules copy
worktree-setup --no-node-modules ../path

# Skip post-setup commands
worktree-setup --no-install ../path

# Full non-interactive mode
worktree-setup --non-interactive --branch main ../path
```

### List Discovered Configs

```bash
worktree-setup --list
```

## Configuration

Create a `worktree.config.ts` file in your project:

```typescript
import { defineWorktreeConfig } from "worktree-setup";

export default defineWorktreeConfig({
  // Human-readable description shown in selection UI
  description: "My project development environment",

  // Directories to symlink (saves disk space for large directories)
  symlinks: [
    "data/cache",
    "data/generated",
  ],

  // Files to copy exactly
  copy: [
    "wrangler.jsonc",
  ],

  // Glob patterns for files to copy
  copyGlob: [
    "**/.env.local",
    "**/.dev.vars",
  ],

  // Template files: copy source to target if target doesn't exist
  templates: [
    { source: "env.example", target: ".env.local" },
  ],

  // Whether to copy node_modules (default: true)
  copyNodeModules: true,

  // Commands to run after setup
  postSetup: ["bun install"],
});
```

### Config File Discovery

The tool searches for:
- `**/worktree.config.ts` - Default config files
- `**/worktree.*.config.ts` - Named variants (e.g., `worktree.minimal.config.ts`)

This allows you to have multiple config files:
- `apps/my-app/worktree.config.ts` - Full setup
- `apps/my-app/worktree.minimal.config.ts` - Minimal setup

### Path Resolution

All paths in the config are relative to the config file's directory. This keeps configs self-contained and portable.

## Features

- **Interactive selection**: Choose which configs to apply when you have multiple
- **Worktree creation**: Optionally create the git worktree as part of setup
- **Symlinks**: Link large directories instead of copying them
- **Glob patterns**: Copy files matching patterns (e.g., `**/.env.local`)
- **Templates**: Create files from examples if they don't exist
- **node_modules**: Copy node_modules for faster setup
- **Post-setup commands**: Run commands like `bun install` automatically
- **Idempotent**: Safe to run multiple times

## License

MIT
