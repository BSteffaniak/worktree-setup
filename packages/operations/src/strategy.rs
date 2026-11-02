//! Cross-entry strategy conflict validation before execution.

#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

use crate::{OperationError, OperationType, PlannedOperation};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use worktree_setup_config::CopyStrategy;

/// Reject differing strategies targeting the same file or overlapping directory trees.
/// Call on the complete plan, including all configs and unstaged selections.
///
/// # Errors
///
/// * If two copy operations with differing strategies overlap at their destination.
pub fn validate_copy_strategies(operations: &[PlannedOperation]) -> Result<(), OperationError> {
    let mut targets: BTreeMap<PathBuf, Vec<&PlannedOperation>> = BTreeMap::new();
    for op in operations
        .iter()
        .filter(|op| op.operation_type != OperationType::Symlink)
    {
        targets.entry(normalize(&op.target)).or_default().push(op);
    }
    for (target, ops) in &targets {
        for op in ops {
            for ancestor in target.ancestors() {
                if let Some(others) = targets.get(ancestor) {
                    for other in others {
                        if op.copy_strategy != other.copy_strategy
                            && (ancestor == target || other.is_directory)
                        {
                            return Err(OperationError::CopyStrategyConflict {
                                target: target.clone(),
                                first: strategy_name(other.copy_strategy).to_owned(),
                                second: strategy_name(op.copy_strategy).to_owned(),
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

const fn strategy_name(strategy: CopyStrategy) -> &'static str {
    match strategy {
        CopyStrategy::Auto => "AUTO",
        CopyStrategy::ByteCopy => "BYTE_COPY",
    }
}

// Resolve existing ancestors too, so equivalent paths (including parent symlinks)
// do not hide an overlap just because the final destination does not exist yet.
fn normalize(path: &Path) -> PathBuf {
    let clean = path_clean::clean(path);
    for ancestor in clean.ancestors() {
        if let Ok(canonical) = ancestor.canonicalize() {
            return canonical.join(clean.strip_prefix(ancestor).expect("ancestor prefix"));
        }
    }
    clean
}
