//! Projection scenarios over the sidebar view-model, grouped by concern:
//! provider aggregation, worktree grouping, subagent
//! nesting, pane binding, lazy-agent binding, displayed status, ranking, and
//! rate-limit windows.
//!
//! Every scenario builds at the testkit [`epoch`] and projects at that same
//! instant, so window verdicts (stall, compaction expiry, ghost TTLs,
//! rate-limit resets) are exact — the suite never reads the wall clock.

mod agent_panes;
mod grouping;
mod lazy_bind;
mod pane_binding;
mod providers;
mod ranking;
mod status;
mod subagents;
mod windows;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::aggregate::{attach_sub_agents, sub_agent_from_state};
use super::providers::fresh_windows;
use super::reap::GHOST_SESSION_TTL_SECS;
use super::rows::row_from_agent;
use super::{SidebarSnapshot, SidebarWorktreeKind, WorktreePrState, row_identity_violations};
use crate::agent_activity::AgentActivity;
use crate::agents::lifecycle::{LifecycleSignal, TurnPhase};
use crate::agents::{AgentAccount, AgentRateLimits, RateLimitWindow, SpendTally, SpendWindow};
use crate::agents::{AgentState, AgentStatus};
use crate::ids::AgentKind;
use crate::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};
use crate::store::snapshot::project::reduce_agent_states;
use crate::store::snapshot::row::SidebarRow;
use crate::store::snapshot::testkit::*;
use crate::store::subagent_context::SubagentContextRecord;
use crate::workspace::RootClass;

fn default_stall_secs() -> i64 {
    i64::from(crate::agents::DEFAULT_STALL_AFTER_SECS)
}

fn paneless_codex(id: &str, worktree: &str, rank: i64) -> AgentState {
    // The app-server daemon fires the hook with no mux pane env, so the
    // agent carries its worktree but never stamps a pane.
    agent("codex", id, AgentStatus::Running, rank).worktree(worktree)
}
