use super::*;
use crate::WorkspaceId;
use crate::schema::diag::GateRule;
use crate::sidebar_pane::app::fixtures::{
    agent_snapshot, pane, snapshot, snapshot_with_panes, workspace,
};
use crate::sidebar_pane::app::health::Health;
use crate::sidebar_pane::app::state::compute_next_state;

fn gate_now() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

/// A snapshot whose single pane renders as a bare process row.
fn process_on(ws: &WorkspaceId, raw: &str) -> SidebarSnapshot {
    snapshot_with_panes(ws, vec![pane(raw, "tab_0", false)])
}

#[test]
fn gate_accepts_first_frame_against_placeholder() {
    let ws = workspace();
    // The placeholder prev has no panes; the first real frame is never a
    // regression to hold.
    assert_eq!(
        gate_commit(
            &snapshot(&ws),
            &agent_snapshot(&ws),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::Accept
    );
}

#[test]
fn gate_holds_transient_agent_to_process_demotion() {
    let ws = workspace();
    // Same pane set {terminal_9}, but the agent row became a bare process —
    // the phantom flicker. Held until the escape hatch opens.
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_9"),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::KeepPrior(GateRule::AgentDemotedToProcess)
    );
}

#[test]
fn gate_holds_frameless_snapshot_over_prior_frame() {
    let ws = workspace();
    let mut frameless = snapshot(&ws);
    frameless.agents = agent_snapshot(&ws).agents;
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &frameless,
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::KeepPrior(GateRule::FramelessOverFrame),
        "a no-frame fallback must not replace a jumpable frame-backed render"
    );
}

#[test]
fn gate_holds_empty_stamped_frame_over_populated_frame() {
    let ws = workspace();
    let mut empty_stamped = snapshot(&ws);
    empty_stamped.panes_produced_at_ms = Some(
        agent_snapshot(&ws)
            .panes_produced_at_ms
            .unwrap_or_default()
            .saturating_add(1),
    );

    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &empty_stamped,
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::KeepPrior(GateRule::EmptyStampedFrame),
    );
}

#[test]
fn gate_accepts_frameless_cold_start_against_placeholder() {
    let ws = workspace();
    let frameless = snapshot(&ws);
    assert_eq!(
        gate_commit(
            &snapshot(&ws),
            &frameless,
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::Accept
    );
}

#[test]
fn gate_releases_demotion_after_reject_count() {
    let ws = workspace();
    let gate = GateState {
        reject_streak: ACCEPT_REGRESSION_AFTER_REJECTS,
        rejecting_since: Some(gate_now()),
        rule: Some(GateRule::AgentDemotedToProcess),
    };
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_9"),
            &gate,
            gate_now()
        ),
        CommitDecision::AcceptViaEscapeHatch,
        "a stuck demotion must surface, not freeze forever"
    );
}

#[test]
fn gate_releases_demotion_after_timeout_but_holds_while_brief() {
    let ws = workspace();
    let base = 1_700_000_000;
    let gate = GateState {
        reject_streak: 1,
        rejecting_since: Some(Timestamp::from_second(base).unwrap()),
        rule: Some(GateRule::AgentDemotedToProcess),
    };
    let ceiling = ACCEPT_REGRESSION_AFTER.as_secs() as i64;
    // Still brief: held.
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_9"),
            &gate,
            Timestamp::from_second(base + ceiling - 1).unwrap()
        ),
        CommitDecision::KeepPrior(GateRule::AgentDemotedToProcess)
    );
    // Past the ceiling: released.
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_9"),
            &gate,
            Timestamp::from_second(base + ceiling).unwrap()
        ),
        CommitDecision::AcceptViaEscapeHatch
    );
}

#[test]
fn gate_accepts_when_the_panel_set_changes() {
    let ws = workspace();
    // A pane closed (the demotion is on a different id): the room genuinely
    // changed, so accept rather than hold against a stale baseline.
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_8"),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::Accept
    );
}

#[test]
fn gate_accepts_a_non_regression() {
    let ws = workspace();
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &agent_snapshot(&ws),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::Accept
    );
}

#[test]
fn reject_holds_prior_frame_as_render_and_baseline() {
    let ws = workspace();
    let prior = agent_snapshot(&ws);
    // A fresh fetch that demoted the agent on terminal_9 to a process row.
    let computed = compute_next_state(
        &ws,
        None,
        Ok(process_on(&ws, "terminal_9")),
        Some(prior.clone()),
        &Health::default(),
    );
    let (state, gate, rejected, released_via_escape_hatch) =
        apply_gate(computed, true, &prior, &GateState::default(), gate_now());
    assert!(rejected);
    assert!(!released_via_escape_hatch);
    // Both the rendered frame AND the next-tick baseline stay the good
    // frame, so the cache never advances onto the demotion.
    assert!(state.snapshot.worktree_groups[0].rows[0].is_agent());
    let baseline = state.last_snapshot.expect("baseline retained");
    assert!(baseline.worktree_groups[0].rows[0].is_agent());
    assert_eq!(gate.reject_streak, 1);
    assert!(gate.rejecting_since.is_some());
    // Orthogonal to Health: a held regression is a *successful* fetch, so it
    // never arms the degraded alert nor counts toward self-close.
    assert!(state.health.alert.is_none());
    assert_eq!(state.health.failure_streak, 0);
}

#[test]
fn reject_holds_prior_frame_over_frameless_fetch() {
    let ws = workspace();
    let prior = agent_snapshot(&ws);
    let computed = compute_next_state(
        &ws,
        None,
        Ok(snapshot(&ws)),
        Some(prior.clone()),
        &Health::default(),
    );

    let (state, gate, rejected, released_via_escape_hatch) =
        apply_gate(computed, true, &prior, &GateState::default(), gate_now());

    assert!(rejected);
    assert!(!released_via_escape_hatch);
    assert_eq!(
        state.snapshot.panes_produced_at_ms,
        prior.panes_produced_at_ms
    );
    assert!(state.snapshot.worktree_groups[0].rows[0].is_agent());
    assert_eq!(gate.reject_streak, 1);
}

#[test]
fn failed_fetch_keeps_a_gate_episode_open() {
    let ws = workspace();
    let prior = agent_snapshot(&ws);
    let computed = compute_next_state(
        &ws,
        None,
        Err("pane discovery failed".to_owned()),
        Some(prior.clone()),
        &Health::default(),
    );
    let prev_gate = GateState {
        reject_streak: 1,
        rejecting_since: Some(gate_now()),
        rule: Some(GateRule::EmptyStampedFrame),
    };

    let (state, gate, rejected, released_via_escape_hatch) =
        apply_gate(computed, false, &prior, &prev_gate, gate_now());

    assert!(!rejected);
    assert!(!released_via_escape_hatch);
    assert_eq!(
        gate, prev_gate,
        "a failed fetch is not an accepted frame and must not release the gate"
    );
    assert!(state.snapshot.worktree_groups[0].rows[0].is_agent());
}

#[test]
fn accept_resets_the_gate() {
    let ws = workspace();
    let prior = agent_snapshot(&ws);
    let computed = compute_next_state(
        &ws,
        None,
        Ok(agent_snapshot(&ws)),
        Some(prior.clone()),
        &Health::default(),
    );
    // Carry a prior reject episode in; a clean accept clears it.
    let prev_gate = GateState {
        reject_streak: 2,
        rejecting_since: Some(gate_now()),
        rule: Some(GateRule::AgentDemotedToProcess),
    };
    let (state, gate, rejected, released_via_escape_hatch) =
        apply_gate(computed, true, &prior, &prev_gate, gate_now());
    assert!(!rejected);
    assert!(!released_via_escape_hatch);
    assert_eq!(gate, GateState::default());
    assert!(state.snapshot.worktree_groups[0].rows[0].is_agent());
}

#[test]
fn escape_release_reports_escape_hatch() {
    let ws = workspace();
    let prior = agent_snapshot(&ws);
    let incoming = process_on(&ws, "terminal_9");
    let computed = compute_next_state(
        &ws,
        None,
        Ok(incoming),
        Some(prior.clone()),
        &Health::default(),
    );
    let prev_gate = GateState {
        reject_streak: ACCEPT_REGRESSION_AFTER_REJECTS,
        rejecting_since: Some(gate_now()),
        rule: Some(GateRule::AgentDemotedToProcess),
    };

    let (state, gate, rejected, released_via_escape_hatch) =
        apply_gate(computed, true, &prior, &prev_gate, gate_now());

    assert!(!rejected);
    assert!(released_via_escape_hatch);
    assert_eq!(gate, GateState::default());
    assert!(state.snapshot.worktree_groups[0].rows[0].is_process());
}
