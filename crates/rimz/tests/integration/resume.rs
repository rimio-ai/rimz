//! Resume-on-rebirth, driven through the REAL fold.
//!
//! The bug these tests guard against: `plan_resume` reads the durable audit
//! rollup, but the rollup builds each agent's pane via `PaneRef::from_id`, which
//! leaves `session_name` empty. A prior filter compared that empty stamp against
//! the live session name and so dropped every candidate, on every rebirth, for
//! every workspace — the reborn room always came up bare.
//!
//! The load-bearing property here is that the `PaneRef` comes from the production
//! reducer (append real lifecycle events → `runtime_projection(Audit)`), not a
//! hand-built test value. The in-module unit tests in `src/resume.rs` cannot
//! catch a rollup-shape mismatch because they fabricate the `AgentState`; these
//! do, because the fold produces it.

use rimz::EventEnvelope;
use rimz::agents::AgentLifecycleObservation;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::ids::MuxName;
use rimz::ids::PaneId;
use std::path::Path;

use crate::common::Harness;

/// A `Registered` observation for a root agent that stamped a pane in a worktree
/// — the shape a `SessionStart` hook records. `name` is the durable card name a
/// launcher passes through; pinning it keeps the resume argv deterministic.
fn registered(
    agent_id: &str,
    name: &str,
    pane_raw: &str,
    worktree: &str,
    branch: &str,
) -> AgentLifecycleObservation {
    AgentLifecycleObservation {
        agent_id: Some(agent_id.into()),
        agent_name: Some(name.to_owned()),
        role: None,
        team: None,
        profile: None,
        kind_ordinal: None,
        signal: LifecycleSignal::Registered,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(worktree.to_owned()),
        worktree_branch: Some(branch.to_owned()),
        task: None,
        prompt: None,
        transcript_path: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        turn_error: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: None,
        todo_total: None,
        pane_id: Some(PaneId::from_parts(MuxName::Zellij, pane_raw)),
        parent_agent_id: None,
    }
}

/// Plan a resume from the workspace's real audit rollup, the way `plan_room_resume`
/// does, so the `AgentState` under test is whatever the fold actually produces.
fn plan_from_rollup(h: &Harness) -> rimz::resume::ResumePlan {
    let projection = h
        .ledger
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    let ended = rimz::ledger::snapshot::agent_tombstones_for_events(&projection.events);
    rimz::resume::plan_resume(
        &projection.agents,
        &ended,
        rimz::resume::DEFAULT_RESUME_MAX,
        |_| true,
        Path::new("/bin/rimz"),
    )
}

fn resume_argv(kind: &str, id: &str, name: &str) -> Vec<String> {
    vec![
        "/bin/rimz".to_owned(),
        "agents".to_owned(),
        "exec".to_owned(),
        kind.to_owned(),
        "--resume".to_owned(),
        id.to_owned(),
        "--close-pane-on-exit".to_owned(),
        "--agent-name".to_owned(),
        name.to_owned(),
    ]
}

fn lifecycle(
    h: &Harness,
    kind: &str,
    event: &str,
    obs: &AgentLifecycleObservation,
) -> EventEnvelope {
    EventEnvelope::agent_lifecycle(h.workspace_id.clone(), "rimz-test", kind, event, obs)
}

#[test]
fn resumes_an_agent_stamped_in_the_real_rollup() {
    // The regression case: under the old session-name filter this rollup yields
    // an empty plan because every fold-built pane carries an empty session_name.
    let h = Harness::new();
    let obs = registered(
        "sess-claude",
        "warm-drift",
        "terminal_3",
        "/repo/feature",
        "feature",
    );
    h.ledger
        .append_event(&lifecycle(&h, "claude", "SessionStart", &obs))
        .expect("append");

    let plan = plan_from_rollup(&h);
    assert_eq!(
        plan.tabs.len(),
        1,
        "the stamped agent is resumed from the real fold"
    );
    assert_eq!(
        plan.tabs[0].panes,
        vec![resume_argv("claude", "sess-claude", "warm-drift")]
    );
    assert_eq!(plan.tabs[0].label, "#feature");
}

#[test]
fn two_same_kind_agents_in_one_worktree_each_resume_their_own_pane() {
    // Two Claude sessions running side by side in one worktree, on distinct
    // panes. The fold keeps both stamped agents; resume keys on the pane, so
    // both come back — the `(kind, worktree, branch)` dedup used to collapse
    // them to one.
    let h = Harness::new();
    let first = registered("sess-a", "lane-a", "terminal_3", "/repo/shared", "main");
    let second = registered("sess-b", "lane-b", "terminal_4", "/repo/shared", "main");
    h.ledger
        .append_event(&lifecycle(&h, "claude", "SessionStart", &first))
        .expect("append first");
    h.ledger
        .append_event(&lifecycle(&h, "claude", "SessionStart", &second))
        .expect("append second");

    let plan = plan_from_rollup(&h);
    assert_eq!(
        plan.tabs.len(),
        1,
        "two concurrent same-kind agents in one worktree share one resume tab"
    );
    assert_eq!(plan.tabs[0].label, "#shared");
    assert_eq!(plan.tabs[0].panes.len(), 2);
}

#[test]
fn a_relaunch_reusing_one_pane_resumes_only_the_newest() {
    // Sequential relaunch in place: a second session re-used the first's pane.
    // The audit fold keeps both stamped agents (it never collapses across
    // agent ids), so resume must dedup by pane and seed exactly one.
    let h = Harness::new();
    let older = registered("sess-old", "ember", "terminal_3", "/repo/work", "main");
    let newer = registered("sess-new", "ember", "terminal_3", "/repo/work", "main");
    h.ledger
        .append_event(&lifecycle(&h, "claude", "SessionStart", &older))
        .expect("append older");
    h.ledger
        .append_event(&lifecycle(&h, "claude", "SessionStart", &newer))
        .expect("append newer");

    let plan = plan_from_rollup(&h);
    assert_eq!(
        plan.tabs.len(),
        1,
        "a relaunch sharing a pane resumes a single seed, not a ghost double"
    );
}

#[test]
fn a_rebirth_boundary_clears_a_prior_stamp_so_it_is_not_resumed() {
    // The pane stamp recorded before the boundary names a dead pane; the fold
    // clears it, so `plan_resume` (which runs before the next boundary) sees no
    // surviving pane and leaves the agent out.
    let h = Harness::new();
    let obs = registered("sess-old", "old-ember", "terminal_3", "/repo/old", "old");
    h.ledger
        .append_event(&lifecycle(&h, "claude", "SessionStart", &obs))
        .expect("append agent");
    h.ledger
        .append_event(&EventEnvelope::session_rebirth(
            h.workspace_id.clone(),
            "rimz-test",
        ))
        .expect("append rebirth");

    let plan = plan_from_rollup(&h);
    assert!(
        plan.is_empty(),
        "an agent whose only stamp predates the rebirth boundary is not resumed"
    );
}

#[test]
fn a_stamp_after_the_rebirth_boundary_survives_and_is_resumed() {
    // A resumed agent re-stamps its new pane after the boundary; that fresh stamp
    // survives the fold's clear, so the next rebirth brings it back. This is what
    // keeps recovery working across repeated reboots.
    let h = Harness::new();
    let before = registered(
        "sess-codex",
        "calm-harbor",
        "terminal_3",
        "/repo/work",
        "work",
    );
    h.ledger
        .append_event(&lifecycle(&h, "codex", "SessionStart", &before))
        .expect("append pre-boundary");
    h.ledger
        .append_event(&EventEnvelope::session_rebirth(
            h.workspace_id.clone(),
            "rimz-test",
        ))
        .expect("append rebirth");
    // Same agent id, a fresh pane id (panes renumber on rebirth), after the boundary.
    let after = registered(
        "sess-codex",
        "calm-harbor",
        "terminal_1",
        "/repo/work",
        "work",
    );
    h.ledger
        .append_event(&lifecycle(&h, "codex", "SessionStart", &after))
        .expect("append post-boundary");

    let plan = plan_from_rollup(&h);
    assert_eq!(plan.tabs.len(), 1, "the post-boundary re-stamp is resumed");
    assert_eq!(
        plan.tabs[0].panes,
        vec![resume_argv("codex", "sess-codex", "calm-harbor")]
    );
}

#[test]
fn an_agent_ended_trace_is_not_resumed() {
    let h = Harness::new();
    let obs = registered(
        "sess-claude",
        "warm-drift",
        "terminal_3",
        "/repo/work",
        "work",
    );
    h.ledger
        .append_event(&lifecycle(&h, "claude", "SessionStart", &obs))
        .expect("append start");
    let ended = AgentLifecycleObservation::new(Some("sess-claude".into()), LifecycleSignal::Ended);
    h.ledger
        .append_event(&lifecycle(&h, "claude", "rimz.agent-ended", &ended))
        .expect("append ended");

    let plan = plan_from_rollup(&h);
    assert!(
        plan.is_empty(),
        "ended agents leave the resume candidate set"
    );
}

#[test]
fn missing_worktree_candidate_is_tombstoned_not_reported() {
    let h = Harness::new();
    let obs = registered(
        "sess-claude",
        "warm-drift",
        "terminal_3",
        "/repo/gone",
        "gone",
    );
    h.ledger
        .append_event(&lifecycle(&h, "claude", "SessionStart", &obs))
        .expect("append start");
    let projection = h
        .ledger
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    let ended = rimz::ledger::snapshot::agent_tombstones_for_events(&projection.events);
    let plan = rimz::resume::plan_resume(
        &projection.agents,
        &ended,
        rimz::resume::DEFAULT_RESUME_MAX,
        |_| false,
        Path::new("/bin/rimz"),
    );
    assert!(plan.tabs.is_empty());
    assert!(plan.skipped.is_empty());
    assert_eq!(
        plan.tombstone,
        vec![(
            rimz::ids::AgentKind::new_unchecked("claude"),
            "sess-claude".into()
        )]
    );

    let ended = AgentLifecycleObservation::new(Some("sess-claude".into()), LifecycleSignal::Ended);
    h.ledger
        .append_event(&lifecycle(&h, "claude", "rimz.worktree-gone", &ended))
        .expect("append tombstone");
    assert!(
        plan_from_rollup(&h).is_empty(),
        "a follow-up plan sees the durable tombstone"
    );
}
