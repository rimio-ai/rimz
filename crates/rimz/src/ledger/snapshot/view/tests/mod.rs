//! Projection scenarios over the sidebar view-model, grouped by concern:
//! feed classification, provider aggregation, worktree grouping, subagent
//! nesting, pane binding, lazy-agent binding, displayed status, ranking, and
//! rate-limit windows.
//!
//! Every scenario builds at the testkit [`epoch`] and projects at that same
//! instant, so window verdicts (stall, compaction expiry, ghost TTLs,
//! rate-limit resets) are exact — the suite never reads the wall clock.

mod feed;
mod grouping;
mod lazy_bind;
mod pane_binding;
mod providers;
mod ranking;
mod status;
mod subagents;
mod windows;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::aggregate::{attach_sub_agents, sub_agent_from_state};
use super::layout::is_within;
use super::providers::{default_provider_style, stable_window, stable_windows};
use super::reap::GHOST_SESSION_TTL_SECS;
use super::rows::row_from_agent;
use super::{SidebarSnapshot, SidebarWorktreeKind, row_identity_violations};
use crate::agent_activity::AgentActivity;
use crate::agents::lifecycle::{LifecycleSignal, TurnPhase};
use crate::agents::{AgentAccount, RateLimitWindow, SpendTally, SpendWindow};
use crate::feed::{
    AgentState, AgentStatus, FeedItem, FeedKind, FeedStatus, PaneRef, RuntimeOwner,
    RuntimeOwnerKind, Surface,
};
use crate::ids::AgentKind;
use crate::ledger::snapshot::project::reduce_agent_states;
use crate::ledger::snapshot::row::SidebarRow;
use crate::ledger::snapshot::testkit::*;
use crate::ledger::subagent_context::SubagentContextRecord;
use crate::workspace::RootClass;

/// A pending agent-hook ask naming `session_id`, homed at `/repo/main` like
/// the agents it joins.
fn agent_ask(kind: FeedKind, source: &str, session_id: &str) -> FeedItem {
    let mut item = FeedItem::new(
        workspace(),
        Surface::NativeUi,
        kind,
        format!("{source} needs attention"),
        source,
        "agent-hook",
    );
    item.worktree_path = Some("/repo/main".to_owned());
    item.payload = serde_json::json!({ "session_id": session_id });
    item
}

fn default_stall_secs() -> i64 {
    i64::from(crate::feed::DEFAULT_STALL_AFTER_SECS)
}

fn paneless_codex(id: &str, worktree: &str, rank: i64) -> AgentState {
    // The app-server daemon fires the hook with no mux pane env, so the
    // agent carries its worktree but never stamps a pane.
    agent("codex", id, AgentStatus::Running, rank).worktree(worktree)
}
