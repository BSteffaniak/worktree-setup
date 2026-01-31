# worktree-setup Agent Guidelines

## Build/Test Commands

- **Rust build**: `cargo build`
- **Rust test**: `cargo test` (all packages), `cargo test -p <package>` (single package)
- **Rust lint**: `cargo clippy --all-targets`
- **Rust lint enforce no warnings**: `cargo clippy --all-targets -- -D warnings`
- **Format**: `cargo fmt` (Rust) for ALL packages in the workspace
- **Install globally**: `cargo install --path packages/cli`

## Code Style Guidelines

### Rust Patterns

- **Collections**: Always use `BTreeMap`/`BTreeSet`, never `HashMap`/`HashSet`
- **Dependencies**: Use `workspace = true`, never path dependencies or inline versions
- **New Dependencies**: When adding a new dependency:
    - Add to workspace `Cargo.toml` with `default-features = false`
    - Specify full version including patch (e.g., `"0.4.28"` not `"0.4"`)
    - Verify you're using the LATEST stable version from crates.io
    - In package `Cargo.toml`, use `workspace = true` and opt-in to specific features only
- **Clippy**: Required in every file:
    ```rust
    #![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
    #![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
    #![allow(clippy::multiple_crate_versions)]
    ```
- **Rustdoc Error docs**: Use asterisks (*) for bullet points, document all error conditions
- **Must use**: Add `#[must_use]` to constructors and getters that return types OTHER THAN Result or Option

### Package Organization

- **Naming**: All packages use underscore naming (`worktree_setup_config`)
- **Features**: Always include `fail-on-warnings = []` feature
- **Serde**: Use `SCREAMING_SNAKE_CASE` for rename attributes

### Documentation

- Document all public APIs with comprehensive error information
- Include examples for complex functions
