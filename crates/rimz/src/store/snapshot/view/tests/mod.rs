//! Projection scenarios over the sidebar view-model, grouped by concern:
//! provider aggregation, worktree grouping, subagent
//! nesting, pane binding, lazy-agent binding, displayed status, ranking, and
//! rate-limit windows.
//!
//! Every scenario builds at the testkit [`epoch`] and projects at that same
//! instant, so window verdicts (stall, compaction expiry, ghost TTLs,
//! rate-limit resets) are exact — the suite never reads the wall clock.

mod agent_panes;
mod forks;
mod grouping;
mod lazy_bind;
mod needs_you;
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
use crate::store::session_death::GHOST_SESSION_TTL_SECS;
use crate::store::snapshot::project::reduce_agent_states;
use crate::store::snapshot::row::SidebarRow;
use crate::store::snapshot::testkit::*;
use crate::store::subagent_context::SubagentContextRecord;
use crate::workspace::RootClass;

fn default_stall_secs() -> i64 {
    i64::from(
        crate::config::AttentionConfig::default()
            .stalled_after_secs
            .get(),
    )
}

#[test]
fn displayed_idle_origin_uses_own_activity_and_closed_cache_requests() {
    use crate::store::snapshot::{CacheClock, agent_idle_since};

    let mut state = agent("claude", "parent", AgentStatus::Success, 1200);
    state.last_activity = ago(1200);
    state.last_seen = ago(1);
    let mut row = row_from_agent(&state, epoch());
    row.last_activity = ago(10);
    row.as_agent_mut().unwrap().own_last_activity = Some(ago(1200));
    for request in [None, Some(ago(1800)), Some(ago(120))] {
        row.as_agent_mut().unwrap().cache = Some(CacheClock {
            ceiling_secs: 1800,
            last_request_at: request,
            warm_until: None,
            rewarm_cost_usd: None,
        });
        let expected = if request == Some(ago(120)) {
            ago(120)
        } else {
            ago(1200)
        };
        assert_eq!(row.idle_since(), expected, "request {request:?}");
        assert_eq!(agent_idle_since(&state, Some(&row)), expected);
    }
}

#[test]
fn displayed_idle_origin_without_a_cache_excludes_children_and_liveness() {
    use crate::store::snapshot::agent_idle_since;

    let mut state = agent("claude", "parent", AgentStatus::Success, 1200);
    state.last_activity = ago(1200);
    state.last_seen = ago(1);
    state.pinged_at = Some(ago(120));
    let mut row = row_from_agent(&state, epoch());
    row.last_activity = ago(10);
    row.as_agent_mut().unwrap().own_last_activity = Some(ago(1200));
    assert_eq!(row.idle_since(), ago(1200));
    assert_eq!(agent_idle_since(&state, Some(&row)), ago(1200));
}

#[test]
fn displayed_idle_origin_for_a_rowless_child_is_real_activity() {
    use crate::store::snapshot::agent_idle_since;

    let mut state = agent("claude", "child", AgentStatus::Success, 1200);
    state.last_activity = ago(1200);
    state.last_seen = ago(1);
    state.pinged_at = Some(ago(120));
    assert_eq!(agent_idle_since(&state, None), ago(1200));
}

fn paneless_codex(id: &str, worktree: &str, rank: i64) -> AgentState {
    // The app-server daemon fires the hook with no mux pane env, so the
    // agent carries its worktree but never stamps a pane.
    agent("codex", id, AgentStatus::Running, rank).worktree(worktree)
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
fn apply_machine_config_stamps_sidebar_theme_keys_and_attention() {
    let mut config = crate::config::MachineConfig::parse_text(
        std::path::Path::new("config.toml"),
        "[keys.sidebar]\n'v' = 'move.down'\n",
        std::path::Path::new("missing-definitions"),
    )
    .unwrap();
    config.sidebar.trunk = Some("develop".into());
    config.theme.display.provider_list = vec!["codex".into()];
    config.agents.attention.stalled_after_secs = std::num::NonZeroU32::new(2700).unwrap();

    let mut snapshot = room(Vec::new());
    snapshot.apply_machine_config(&config);

    assert_eq!(snapshot.sidebar, config.sidebar);
    assert_eq!(snapshot.theme, config.theme);
    assert_eq!(snapshot.keys, config.sidebar_keymap());
    assert_ne!(snapshot.keys, Default::default());
    assert_eq!(snapshot.attention, config.agents.attention);
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

#[test]
fn skipped_settings_digest_names_the_set_of_warnings() {
    use crate::store::snapshot::SkippedSettings;
    use crate::workspace::record::SkippedSetting;

    let skipped = |pairs: &[(&str, &str)]| {
        let list = pairs
            .iter()
            .map(|&(setting, message)| SkippedSetting {
                setting: setting.into(),
                message: message.into(),
            })
            .collect::<Vec<_>>();
        SkippedSettings::of(&list)
    };
    let shown = skipped(&[("theme.x", "unknown key"), ("agents.y", "not a number")]).unwrap();
    assert_eq!(shown.count, 2);
    assert_eq!(
        skipped(&[("agents.y", "not a number"), ("theme.x", "unknown key")]),
        Some(shown.clone()),
        "order does not change the identity"
    );
    let reworded = skipped(&[("theme.x", "unknown key"), ("agents.y", "too large")]).unwrap();
    assert_eq!(reworded.count, 2);
    assert_ne!(reworded.digest, shown.digest);
    assert_ne!(
        skipped(&[("ab", "c")]).unwrap().digest,
        skipped(&[("a", "bc")]).unwrap().digest,
        "field boundaries are framed"
    );
    assert_eq!(skipped(&[]), None);
}
