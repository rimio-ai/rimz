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
use super::rows::row_from_agent;
use super::{SidebarSnapshot, SidebarWorktreeKind, WorktreePrState, row_identity_violations};
use crate::agent_activity::AgentActivity;
use crate::agents::lifecycle::{LifecycleSignal, TurnPhase};
use crate::agents::{
    AgentAccount, AgentRateLimits, RateLimitWindow, SpendTally, SpendWindow, TurnSettleOutcome,
};
use crate::agents::{AgentState, AgentStatus};
use crate::ids::AgentKind;
use crate::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};
use crate::store::active_time::ActiveTimeRecord;
use crate::store::session_death::GHOST_SESSION_TTL_SECS;
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

#[test]
fn active_time_fold_stamps_only_roots_and_preserves_existing_clocks() {
    let mut root = agent("claude", "root", AgentStatus::Running, 0);
    root.last_activity = ago(30);
    let mut child = agent("claude", "child", AgentStatus::Running, 1);
    child.parent_agent_id = Some(root.agent_id.clone());
    child.subagent_started_at = Some(ago(60));
    let root_activity = root.last_activity;
    let child_started_at = child.subagent_started_at;
    let records = [
        ActiveTimeRecord {
            kind: root.kind.clone(),
            agent_id: root.agent_id.clone(),
            credited_ms: 10_000,
            last_progress: ago(300),
            active: true,
        },
        ActiveTimeRecord {
            kind: child.kind.clone(),
            agent_id: child.agent_id.clone(),
            credited_ms: 999_000,
            last_progress: ago(1),
            active: false,
        },
    ];

    let snapshot = room(vec![root, child]).with_active_time(&records);
    let root = rollup_agent(&snapshot, "root");
    let child = rollup_agent(&snapshot, "child");

    assert_eq!(root.estimated_active_secs, Some(190));
    assert_eq!(root.last_activity, root_activity);
    assert_eq!(child.estimated_active_secs, None);
    assert_eq!(child.subagent_started_at, child_started_at);
    assert_eq!(
        row_from_agent(root, epoch())
            .as_agent()
            .and_then(|card| card.estimated_active_secs),
        Some(190)
    );
}

#[test]
fn inactive_active_time_record_projects_frozen_credit() {
    let root = agent("codex", "root", AgentStatus::Idle, 0);
    let record = ActiveTimeRecord {
        kind: root.kind.clone(),
        agent_id: root.agent_id.clone(),
        credited_ms: 12_500,
        last_progress: ago(900),
        active: false,
    };

    let snapshot = room(vec![root]).with_active_time(&[record]);

    assert_eq!(
        rollup_agent(&snapshot, "root").estimated_active_secs,
        Some(12)
    );
}

#[test]
fn row_handle_prefers_role_then_explicit_name_then_profile() {
    let mut named = agent("claude", "named", AgentStatus::Idle, 0);
    named.name = Some("writer".to_owned());
    named.name_explicit = true;
    named.profile = Some("docs".to_owned());
    assert_eq!(
        row_from_agent(&named, epoch())
            .as_agent()
            .and_then(|card| card.handle.as_deref()),
        Some("writer")
    );

    named.role = Some("coder".to_owned());
    assert_eq!(
        row_from_agent(&named, epoch())
            .as_agent()
            .and_then(|card| card.handle.as_deref()),
        Some("coder")
    );

    let mut minted = agent("claude", "minted", AgentStatus::Idle, 0);
    minted.name = Some("lucid-atlas".to_owned());
    assert_eq!(
        row_from_agent(&minted, epoch())
            .as_agent()
            .and_then(|card| card.handle.as_deref()),
        None
    );
}

#[test]
fn row_preserves_unknown_context_percentage() {
    let cursor = agent("cursor", "sess-1", AgentStatus::Running, 0);
    let row = row_from_agent(&cursor, epoch());
    let card = row.as_agent().expect("agent card");
    assert_eq!(card.usage.context_pct, None);
    assert_eq!(card.context_gauge_percent(), None);
}

#[test]
fn capacity_and_session_usage_do_not_fabricate_a_context_percentage() {
    let mut state = agent("droid", "sess", AgentStatus::Running, 0);
    state.usage.context_pct = None;
    let mut context = crate::agents::AgentContext::new("droid", epoch());
    context.tokens = Some(crate::agents::AgentTokenUsage {
        context_window_size: Some(200_000),
        used_percentage: None,
        remaining_percentage: None,
        current_context_tokens: None,
        current_usage: None,
        session_usage: Some(crate::agents::AgentSessionUsage {
            input_tokens: Some(12_000),
            ..Default::default()
        }),
    });
    state.context = Some(context);

    let row = row_from_agent(&state, epoch());
    assert_eq!(row.as_agent().unwrap().usage.context_pct, None);
    assert_eq!(row.context_gauge_percent(), None);
    assert_eq!(row.context_used_tokens(), None);
}
