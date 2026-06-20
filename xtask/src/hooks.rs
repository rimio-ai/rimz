//! Git hook activation: point `core.hooksPath` at the tracked `.githooks/`.

use std::path::Path;

use anyhow::Result;

use crate::runner::run;

/// Activate the repo's tracked git hooks by setting the local `core.hooksPath`
/// to `.githooks/`, so the committed `pre-commit` fmt gate runs on every commit.
/// Idempotent — re-running rewrites the same value. The hook script routes git's
/// call back through `cargo xtask`, so the gate definition stays single-sourced.
pub(crate) fn install(root: &Path) -> Result<()> {
    run(root, "git", ["config", "core.hooksPath", ".githooks"])
}
