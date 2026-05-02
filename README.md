# worktree-setup

Config-driven git worktree management — create, setup, clean, and remove worktrees with a single tool.

## What this does

When you create a new worktree, you usually need to:

- Copy `.env` and other untracked config files
- Re-run `npm install` or equivalent
- Maybe symlink large directories like `node_modules` to save space
- Clean up build artifacts across all worktrees at once
- Remove worktrees and their local branches when you're done

This tool reads a config file and handles all of that automatically.

## Install

```bash
cargo install worktree-setup
```

## Usage

The default command creates a worktree and applies configs. Subcommands handle other lifecycle operations:

| Command                        | Description                                     |
| ------------------------------ | ----------------------------------------------- |
| `worktree-setup <path>`        | Create worktree + apply configs (default)       |
| `worktree-setup setup [path]`  | Apply configs to an existing directory          |
| `worktree-setup clean [path]`  | Delete files/directories specified in configs   |
| `worktree-setup remove [path]` | Remove worktrees and optionally delete branches |

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

### Create a new branch based off a specific branch

```bash
# Creates feature-x branched from master (your current checkout stays unchanged)
worktree-setup ../new-worktree --new-branch feature-x --branch master
```

### List available configs

```bash
worktree-setup --list
```

### Interactive Mode

When creating a new worktree interactively, you'll be prompted to choose how to set it up:

```
How should the worktree be created?
> New branch (auto-named 'my-worktree')
  New branch (custom name)...
  Use current branch (feature-xyz)
  Use existing branch...
  Detached HEAD (current commit)
```

For new branches, you'll also be asked what to base them off:

```
Base the new branch off:
> Current HEAD
  master
  Enter custom branch/ref...
```

The default branch (e.g., `master`) is auto-detected from your repository.

## Subcommands

### setup

Apply worktree configs to an existing directory without creating a new worktree. Useful for re-running setup after config changes or on directories that were created manually.

```bash
# Apply configs to the current directory
worktree-setup setup

# Apply configs to a specific directory
worktree-setup setup ../existing-worktree

# Skip file operations, only run post-setup commands
worktree-setup setup --no-files

# Overwrite existing files during file operations
worktree-setup setup --overwrite
```

### clean

Delete files and directories specified in the `clean` field of your worktree configs. Supports exact paths and glob patterns.

```bash
# Clean the current worktree
worktree-setup clean

# Preview what would be deleted
worktree-setup clean --dry-run

# Skip confirmation prompt
worktree-setup clean --force

# Interactively select which worktrees to clean
worktree-setup clean --worktrees
```

The `--worktrees` flag opens a multi-select picker that shows all worktrees with live-updating sizes as paths are resolved in the background:

```
? Select worktrees to clean (space to toggle, enter to confirm):
> [ ] feature-a (/path/to/wt-a)  ████████ 3 items, 1.8 GiB
  [ ] feature-b (/path/to/wt-b)  ⠋ resolving...
  [ ] feature-c (/path/to/wt-c)  ∅ 2 empty dirs
```

Items are resolved in parallel — sizes appear as each worktree finishes scanning. The live checkbox list does not reorder while you are selecting. Once sizes are known in the final preview, worktrees and clean items are sorted largest-first; truly empty directories are shown separately from almost-empty entries.

### remove

Remove git worktrees and optionally delete their local branches.

```bash
# Remove a specific worktree
worktree-setup remove ../old-worktree

# Remove the current worktree (when inside a linked worktree)
worktree-setup remove

# Interactively select worktrees to remove
worktree-setup remove --worktrees

# Preview what would be removed
worktree-setup remove --dry-run

# Skip confirmation prompt
worktree-setup remove --force
```

When no path is given:

- If the current directory is inside a **linked worktree**, that worktree is removed
- If the current directory is the **main worktree**, an interactive picker is shown

The `--worktrees` flag opens the picker from anywhere. Worktrees with uncommitted changes are flagged in the picker:

```
? Select worktrees to remove (space to toggle, enter to confirm):
> [ ] feature-a (/path/to/wt-a) (has uncommitted changes)
  [ ] feature-b (/path/to/wt-b)
  [-] master (/path/to/main) [main]
```

Dirty-worktree checks run in the background — the picker appears instantly with spinners that resolve as each check completes.

After removal, branch deletion is controlled by the `branch_delete` policy in your [global configuration](#global-configuration).

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

# Paths and patterns to delete with `worktree-setup clean`
clean = [
    "node_modules",
    ".turbo",
    "**/dist",
]
```

### Clean Paths

The `clean` field accepts exact relative paths and glob patterns:

```toml
clean = [
    "node_modules",       # exact directory
    ".turbo",             # exact directory
    "**/dist",            # glob: all dist/ dirs recursively
    "*.log",              # glob: all .log files in config dir
]
```

Paths are relative to the config file's directory. Prefix with `/` for repo-root-relative paths. All resolved paths must remain within the target worktree directory (paths that escape the worktree are rejected).

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

## Profiles

Profiles let you predefine groups of configs and default settings. Define profiles inside any `worktree.config.toml`:

```toml
description = "Main workspace"
symlinks = ["node_modules"]
postSetup = ["npm install"]

[profiles.dev]
description = "Development setup"
copyUnstaged = true
baseBranch = "develop"
autoCreate = true

[profiles.ci]
description = "CI environment"
postSetup = "none"
creationMethod = "detach"
```

Use a profile with the `--profile` flag:

```bash
# Create a worktree using the dev profile defaults
worktree-setup ../feature-branch --profile dev
```

A config "belongs to" a profile if it declares a `[profiles.<name>]` section. When you use `--profile`, all configs declaring that profile are auto-selected and the profile's defaults are applied.

### Auto-selecting configs

Profiles can pull in additional configs using glob patterns:

```toml
[profiles.frontend]
description = "Frontend development"
configs = ["apps/web/*.config.toml", "/worktree.local.config.ts"]
baseBranch = "master"
```

Patterns are relative to the config file's directory. Prefix with `/` for repo-root-relative patterns.

### Multiple profiles

You can combine profiles — later profiles override earlier ones on conflicting defaults:

```bash
worktree-setup ../my-worktree --profile dev --profile frontend
```

### Profile Defaults Reference

| Field               | Type     | Description                                      |
| ------------------- | -------- | ------------------------------------------------ |
| `description`       | string   | Label for the profile                            |
| `configs`           | string[] | Glob patterns to auto-select additional configs  |
| `copyUnstaged`      | bool     | Copy unstaged/untracked files                    |
| `overwriteExisting` | bool     | Overwrite existing files during file operations  |
| `autoCreate`        | bool     | Skip "Create worktree?" confirmation             |
| `creationMethod`    | string   | `"auto"`, `"current"`, `"remote"`, or `"detach"` |
| `baseBranch`        | string   | Base branch for new worktree branches            |
| `newBranch`         | bool     | Always create a new branch (auto-named)          |
| `remote`            | string   | Remote name for remote branch operations         |
| `postSetup`         | string   | `"all"`, `"none"`, or `["cmd1", "cmd2"]`         |
| `skipPostSetup`     | string[] | Commands to skip when `postSetup = "all"`        |

## Global Configuration

Global settings are loaded from two locations (repo-level overrides global):

| Location                               | Scope     |
| -------------------------------------- | --------- |
| `~/.config/worktree-setup/config.toml` | All repos |
| `.worktree-setup.toml` (at repo root)  | This repo |

If neither file exists, defaults are used. Example:

```toml
[remove]
branch_delete = "ASK"

[security]
allow_path_escape = false
```

### Branch Delete Policy

Controls whether local branches are deleted after removing a worktree:

| Value    | Behavior                   |
| -------- | -------------------------- |
| `ASK`    | Prompt each time (default) |
| `ALWAYS` | Delete without asking      |
| `NEVER`  | Never delete, don't ask    |

### Security

Controls containment enforcement for file operations:

| Field               | Type | Default | Description                                               |
| ------------------- | ---- | ------- | --------------------------------------------------------- |
| `allow_path_escape` | bool | `false` | When `false`, paths that escape the worktree are rejected |

Per-config `allowPathEscape` overrides the global setting. When neither is set, containment is enforced (paths must stay within the worktree boundary).

## Config Reference

| Field             | Type     | Description                                        |
| ----------------- | -------- | -------------------------------------------------- |
| `description`     | string   | Label shown during config selection                |
| `symlinks`        | string[] | Paths to symlink from master worktree              |
| `copy`            | string[] | Paths to copy (skipped if target exists)           |
| `overwrite`       | string[] | Paths to copy (always overwrites)                  |
| `copyGlob`        | string[] | Glob patterns to copy                              |
| `copyUnstaged`    | bool     | Copy modified/untracked files from master worktree |
| `templates`       | array    | Copy source to target if target doesn't exist      |
| `postSetup`       | string[] | Commands to run after setup                        |
| `clean`           | string[] | Paths and glob patterns to delete with `clean`     |
| `allowPathEscape` | bool     | Allow paths to escape the worktree boundary        |

**Path resolution:** All paths are relative to the config file's directory by default. Prefix with `/` for repo-root-relative paths (e.g., `"/.envrc"` → `<repo-root>/.envrc`).

## CLI Reference

### Default (create + setup)

| Flag                     | Description                                                      |
| ------------------------ | ---------------------------------------------------------------- |
| `<target-path>`          | Path where the worktree will be created                          |
| `--branch <name>`        | Check out this branch, or use as start point with `--new-branch` |
| `--new-branch <name>`    | Create a new branch for the worktree                             |
| `--remote-branch <name>` | Track a remote branch (fetches from origin first)                |
| `--remote <name>`        | Remote name to use (auto-detected if omitted)                    |
| `--no-infer-branch`      | Disable branch name inference from worktree directory name       |
| `-c, --config <pattern>` | Only use configs matching this pattern (can be repeated)         |
| `--profile <name>`       | Use a named profile (can be repeated)                            |
| `--unstaged`             | Copy unstaged/untracked files (overrides config)                 |
| `--no-unstaged`          | Don't copy unstaged files (overrides config)                     |
| `--no-install`           | Skip running post-setup commands                                 |
| `-f, --force`            | Force worktree creation even if path is already registered       |
| `--list`                 | List discovered configs and exit                                 |
| `--non-interactive`      | Run without prompts (requires target-path)                       |
| `--no-progress`          | Disable progress bars                                            |
| `-v, --verbose`          | Enable debug output                                              |

### setup

| Flag                     | Description                                              |
| ------------------------ | -------------------------------------------------------- |
| `[target-path]`          | Path to the target directory (defaults to current dir)   |
| `-c, --config <pattern>` | Only use configs matching this pattern (can be repeated) |
| `--profile <name>`       | Use a named profile (can be repeated)                    |
| `--no-files`             | Skip file operations (symlinks, copies, templates)       |
| `--overwrite`            | Overwrite existing files during file operations          |
| `--unstaged`             | Copy unstaged/untracked files (overrides config)         |
| `--no-unstaged`          | Don't copy unstaged files (overrides config)             |
| `--no-install`           | Skip running post-setup commands                         |
| `--non-interactive`      | Run without prompts, using defaults                      |
| `--no-progress`          | Disable progress bars                                    |
| `-v, --verbose`          | Enable debug output                                      |

### clean

| Flag                     | Description                                              |
| ------------------------ | -------------------------------------------------------- |
| `[target-path]`          | Path to the target directory (defaults to current dir)   |
| `-c, --config <pattern>` | Only use configs matching this pattern (can be repeated) |
| `--profile <name>`       | Use a named profile (can be repeated)                    |
| `-w, --worktrees`        | Interactively select worktrees to clean                  |
| `-f, --force`            | Skip confirmation prompt                                 |
| `--dry-run`              | Preview what would be deleted without deleting           |
| `--non-interactive`      | Run without prompts (requires `--force` or `--dry-run`)  |
| `--no-progress`          | Disable progress bars                                    |
| `--max-parallel <N>`     | Cap concurrent worktree resolutions (see notes below)    |
| `-v, --verbose`          | Enable debug output                                      |

#### `--max-parallel`

Controls the size of the thread pool used to resolve clean paths and
compute disk usage across worktrees.

- Default: `min(num_cpus, num_worktrees)` (or `1` if fewer)
- Environment variable fallback: `WORKTREE_SETUP_MAX_PARALLEL`
- CLI flag wins over the environment variable

`clean` is disk-I/O-bound: on most hardware, raising `--max-parallel`
above the default will _slow down_ the run because multiple walkers
contend for the same disk. Lower it (e.g. `--max-parallel 4`) for
network-mounted or slow disks; raise it only on fast NVMe arrays where
you can verify a measurable speedup.

### remove

| Flag                | Description                                             |
| ------------------- | ------------------------------------------------------- |
| `[target-path]`     | Path to the worktree to remove                          |
| `-w, --worktrees`   | Interactively select worktrees to remove                |
| `-f, --force`       | Skip confirmation prompt                                |
| `--dry-run`         | Preview what would be removed without removing          |
| `--non-interactive` | Run without prompts (requires `--force` or `--dry-run`) |
| `-v, --verbose`     | Enable debug output                                     |

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
