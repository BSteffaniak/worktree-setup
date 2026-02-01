# worktree-setup

Automates copying files and running setup commands when creating git worktrees.

## What this does

When you create a new worktree, you usually need to:

- Copy `.env` and other untracked config files
- Re-run `npm install` or equivalent
- Maybe symlink large directories like `node_modules` to save space

This tool reads a config file and does that automatically.

## Install

```bash
cargo install worktree-setup
```

## Usage

```bash
cd your-repo
worktree-setup ../new-worktree
```

This will:

1. Find all `worktree.config.toml` (or `.ts`) files in the repo
2. Prompt you to select which configs to apply
3. Create the worktree if it doesn't exist
4. Run the configured operations (symlinks, copies, etc.)
5. Run post-setup commands

### Non-interactive mode

```bash
worktree-setup ../new-worktree --non-interactive --branch master
```

### Create a new branch

```bash
worktree-setup ../new-worktree --new-branch feature-x
```

### List available configs

```bash
worktree-setup --list
```

## Configuration

Create `worktree.config.toml` in your repo root (or any subdirectory):

```toml
description = "Main workspace"

# Symlink these paths to the master worktree (saves space, stays in sync)
symlinks = [
    "node_modules",
    ".cache",
]

# Copy these if they don't exist in the new worktree
copy = [
    ".env.local",
    "config/local.json",
]

# Copy these, overwriting if they exist
overwrite = [
    "generated/schema.graphql",
]

# Copy files matching glob patterns
copyGlob = [
    "**/.env.development",
    "packages/*/.env",
]

# Copy files that have uncommitted changes in the master worktree
# Useful when you want to branch off mid-work
copyUnstaged = false

# Copy source file to target path (if target doesn't exist)
# Useful for initializing config from templates
templates = [
    { source = ".env.example", target = ".env" },
    { source = "config/default.json", target = "config/local.json" },
]

# Run these commands after setup completes
postSetup = [
    "npm install",
    "npm run db:migrate",
]
```

### Repo-Root-Relative Paths

By default, paths in the config are relative to the config file's directory. If you need to reference files at the repository root from a config in a subdirectory, prefix the path with `/`:

```toml
# Config in apps/frontend/worktree.config.toml

# These paths are relative to apps/frontend/
copy = [
    ".env.local",           # -> apps/frontend/.env.local
    "config/settings.json", # -> apps/frontend/config/settings.json
]

# These paths are relative to the repo root
symlinks = [
    "/.nix",                # -> .nix (at repo root)
    "/.envrc",              # -> .envrc (at repo root)
]
```

This is especially useful for:

- Referencing root-level nix/direnv configuration from app-specific configs
- Symlinking shared directories that live at the repo root
- Templates that are stored at the root but used by multiple apps

Templates also support mixed paths:

```toml
templates = [
    { source = "/.env.template", target = ".env.local" },  # source from root, target in config dir
]
```

## Config Reference

| Field          | Type     | Description                                        |
| -------------- | -------- | -------------------------------------------------- |
| `description`  | string   | Label shown during config selection                |
| `symlinks`     | string[] | Paths to symlink from master worktree              |
| `copy`         | string[] | Paths to copy (skipped if target exists)           |
| `overwrite`    | string[] | Paths to copy (always overwrites)                  |
| `copyGlob`     | string[] | Glob patterns to copy                              |
| `copyUnstaged` | bool     | Copy modified/untracked files from master worktree |
| `templates`    | array    | Copy source to target if target doesn't exist      |
| `postSetup`    | string[] | Commands to run after setup                        |

**Path resolution:** All paths are relative to the config file's directory by default. Prefix with `/` for repo-root-relative paths (e.g., `"/.envrc"` → `<repo-root>/.envrc`).

## CLI Flags

| Flag                     | Description                                              |
| ------------------------ | -------------------------------------------------------- |
| `<target-path>`          | Path where the worktree will be created                  |
| `--branch <name>`        | Create worktree from this existing branch                |
| `--new-branch <name>`    | Create a new branch for the worktree                     |
| `-c, --config <pattern>` | Only use configs matching this pattern (can be repeated) |
| `--unstaged`             | Copy unstaged/untracked files (overrides config)         |
| `--no-unstaged`          | Don't copy unstaged files (overrides config)             |
| `--no-install`           | Skip running post-setup commands                         |
| `--list`                 | List discovered configs and exit                         |
| `--non-interactive`      | Run without prompts (requires target-path)               |
| `--no-progress`          | Disable progress bars                                    |
| `-v, --verbose`          | Enable debug output                                      |

## TypeScript Config

If you need programmatic configuration, create `worktree.config.ts`:

```typescript
export default {
  description: "Frontend workspace",
  symlinks: ["node_modules"],
  copy: [".env.local"],
  copyUnstaged: process.env.COPY_UNSTAGED === "true",
  postSetup: ["npm install"],
};
```

Requires [bun](https://bun.sh) or [deno](https://deno.land) to be installed.

## Multiple Configs

### Discovery

The tool discovers all `worktree.config.{toml,ts}` and `worktree.*.config.{toml,ts}` files in your repo:

```
my-monorepo/
├── worktree.config.toml           # Shared setup
├── worktree.local.config.ts       # Personal setup (gitignored)
├── apps/
│   ├── web/
│   │   └── worktree.config.ts     # Web-specific
│   └── api/
│       └── worktree.config.ts     # API-specific
```

### Composing Configs

Select multiple configs during setup and they all get applied. This lets you layer different concerns:

| Config                     | Purpose                                              | Tracked |
| -------------------------- | ---------------------------------------------------- | ------- |
| `worktree.config.ts`       | Team defaults (symlinks, env files, post-setup)      | ✓       |
| `worktree.local.config.ts` | Personal setup (editor configs, copy unstaged files) | ✗       |

**Example workflow:**

1. Select both configs when prompted
2. First config symlinks `node_modules` and copies `.env` template
3. Second config copies your uncommitted work-in-progress files

In a monorepo, you might also have app-specific configs:

```bash
$ worktree-setup --list
Found 4 configs:
  • worktree.config.toml - Shared workspace setup
  • worktree.local.config.ts - Personal configuration
  • apps/web/worktree.config.ts - Web app
  • apps/api/worktree.config.ts - API server
```

Select the shared config plus whichever app(s) you're working on.

### Gitignore Pattern

To keep personal configs untracked while preserving team configs:

```gitignore
# Ignore personal/machine-specific configs
worktree.*.config.ts
worktree.*.config.toml

# Keep the main config tracked
!worktree.config.ts
!worktree.config.toml
```

### Filtering Configs

Use `-c/--config` to filter by pattern instead of interactive selection:

```bash
# Only apply configs matching "web"
worktree-setup ../new-wt -c web

# Apply multiple specific configs
worktree-setup ../new-wt -c shared -c web
```

## How operations work

| Operation      | Behavior                                                          |
| -------------- | ----------------------------------------------------------------- |
| `symlinks`     | Creates symlink pointing to the path in the master worktree       |
| `copy`         | Copies file/directory if target doesn't exist, skips otherwise    |
| `overwrite`    | Always copies, replacing existing files                           |
| `copyGlob`     | Finds files matching the pattern and copies them (skip if exists) |
| `templates`    | Copies source to target path, only if target doesn't exist        |
| `copyUnstaged` | Copies files with uncommitted changes from master worktree        |

File copying uses reflink (copy-on-write) when the filesystem supports it (APFS on macOS, Btrfs on Linux). This makes copying large directories nearly instant.

## Requirements

- Git 2.5+
- For TypeScript configs: bun or deno

## Building from source

```bash
git clone https://github.com/BSteffaniak/worktree-setup
cd worktree-setup
cargo install --path packages/cli
```

## License

[MPL-2.0](https://www.mozilla.org/en-US/MPL/2.0/)
