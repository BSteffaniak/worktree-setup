---
branch_name: "determinism-updates-${run_id}"
commit_message: "fix: replace non-deterministic collections with deterministic alternatives"
---

# Determinism Checker

You are auditing the codebase to ensure all code uses deterministic data structures and patterns.

## Critical Requirements

Per the project's AGENTS.md guidelines, this codebase MUST use:

- **`BTreeMap`** instead of `HashMap`
- **`BTreeSet`** instead of `HashSet`

These deterministic collections ensure consistent iteration order across runs, which is essential for reproducible builds, testing, and debugging.

## Your Task

1. **Search the entire codebase** for any usage of:
   - `HashMap` (from `std::collections::HashMap`)
   - `HashSet` (from `std::collections::HashSet`)
   - Any imports of these types

2. **Replace all occurrences** with their deterministic equivalents:
   - `HashMap` → `BTreeMap`
   - `HashSet` → `BTreeSet`

3. **Update imports** accordingly:
   - `use std::collections::HashMap` → `use std::collections::BTreeMap`
   - `use std::collections::HashSet` → `use std::collections::BTreeSet`

4. **Handle type constraints**: Ensure that key types implement `Ord` (required for BTree collections) instead of just `Hash + Eq`.

## Verification Checklist

After making changes, verify:

- [ ] Run `cargo fmt` to format all code
- [ ] Run `cargo clippy --all-targets -- -D warnings` to check for warnings
- [ ] Run `cargo test` to ensure tests pass
- [ ] Run `cargo doc --no-deps` to verify documentation builds

## Output Format

When you're done, output the commit message between these markers:

COMMIT_MESSAGE_START
fix: replace HashMap/HashSet with BTreeMap/BTreeSet for determinism

- Replaced all HashMap usages with BTreeMap
- Replaced all HashSet usages with BTreeSet
- Updated imports to use deterministic collections
- Ensures consistent iteration order across runs
COMMIT_MESSAGE_END
