//! Producer-side git root enumeration for worktree grouping.
//!
//! Per-worktree diff/branch/landed facts live in `sidebar::refresh::git_stats`;
//! this module keeps the fetch-tick root probe that lets a repo room group
//! linked worktrees before any row resolves inside them.

mod roots;

pub(in crate::sidebar::produce) use roots::project_group_roots;

#[cfg(test)]
mod tests;
