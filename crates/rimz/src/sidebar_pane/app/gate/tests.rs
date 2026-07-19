use super::*;
use crate::WorkspaceId;
use crate::diag::record::GateRule;
use crate::sidebar_pane::app::fixtures::{
    agent_snapshot, pane, snapshot, snapshot_with_panes, workspace,
};
use crate::sidebar_pane::app::health::Health;
use crate::sidebar_pane::app::state::compute_next_state;
use crate::{SpendTally, SpendWindow};

fn gate_now() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

/// A snapshot whose single pane renders as a bare process row.
fn process_on(ws: &WorkspaceId, raw: &str) -> SidebarSnapshot {
    snapshot_with_panes(ws, vec![pane(raw, "tab_0", false)])
}

fn process_on_cmd(ws: &WorkspaceId, raw: &str, command: Option<&str>) -> SidebarSnapshot {
    let mut pane = pane(raw, "tab_0", false);
    pane.command = command.map(str::to_owned);
    snapshot_with_panes(ws, vec![pane])
}

fn spend_tally(year_usd: f64) -> SpendTally {
    SpendTally {
        year: SpendWindow {
            usd: year_usd,
            tokens: 1,
            ..SpendWindow::default()
        },
        ..SpendTally::default()
    }
}

fn provider(
    kind: &str,
    spending: Option<SpendTally>,
    used_percentage: u8,
) -> crate::SidebarProviderPanel {
    crate::SidebarProviderPanel {
        kind: kind.to_owned(),
        account_scope: Default::default(),
        product_name: kind.to_owned(),
        art: Vec::new(),
        art_tints: Vec::new(),
        color: 0,
        color_rgb: None,
        color_role: None,
        version: None,
        plan: None,
        metered: true,
        remote_control: Default::default(),
        active_sessions: 0,
        spending,
        day_budget: None,
        extra_credits: None,
        reset_credits: None,
        window_placeholders: Vec::new(),
        windows: vec![crate::agents::RateLimitWindow {
            used_percentage: Some(used_percentage),
            duration_mins: Some(300),
            ..Default::default()
        }],
    }
}

fn spend_snapshot(
    ws: &WorkspaceId,
    fleet_usd: Option<f64>,
    workspace_usd: Option<f64>,
    provider_usd: Option<f64>,
    used_percentage: u8,
) -> SidebarSnapshot {
    let mut snapshot = agent_snapshot(ws);
    snapshot.value_tally = fleet_usd.map(spend_tally);
    snapshot.workspace_value_tally = workspace_usd.map(spend_tally);
    snapshot.providers = vec![provider(
        "codex",
        provider_usd.map(spend_tally),
        used_percentage,
    )];
    snapshot
}

#[test]
fn gate_commit_covers_first_frame_and_regression_rules() {
    let ws = workspace();
    assert_eq!(
        gate_commit(
            &snapshot(&ws),
            &agent_snapshot(&ws),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::Accept
    );

    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_9"),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::KeepPrior(GateRule::AgentDemotedToProcess)
    );

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

    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_8"),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::Accept
    );

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
fn in_place_exit_commits_but_same_command_flicker_holds() {
    let ws = workspace();
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on_cmd(&ws, "terminal_9", Some("bash")),
            &GateState::default(),
            gate_now(),
        ),
        CommitDecision::Accept,
        "foreground command changed, so the agent exited in place"
    );
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_9"),
            &GateState::default(),
            gate_now(),
        ),
        CommitDecision::KeepPrior(GateRule::AgentDemotedToProcess),
        "same pane and command still models a phantom process flicker"
    );
}

#[test]
fn missing_foreground_command_keeps_demotion_protective() {
    let ws = workspace();
    let mut agent_without_command = agent_snapshot(&ws);
    agent_without_command.worktree_groups[0].rows[0]
        .pane
        .as_mut()
        .unwrap()
        .command = None;
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on_cmd(&ws, "terminal_9", None),
            &GateState::default(),
            gate_now(),
        ),
        CommitDecision::KeepPrior(GateRule::AgentDemotedToProcess),
        "missing command evidence is not enough to classify a real exit"
    );
    assert_eq!(
        gate_commit(
            &agent_without_command,
            &process_on_cmd(&ws, "terminal_9", Some("bash")),
            &GateState::default(),
            gate_now(),
        ),
        CommitDecision::KeepPrior(GateRule::AgentDemotedToProcess),
        "missing prior command evidence is not enough to classify a real exit"
    );
}

#[test]
fn gate_releases_held_regression_by_count_or_timeout() {
    let ws = workspace();
    let gate = GateState {
        reject_streak: ACCEPT_REGRESSION_AFTER_REJECTS,
        rejecting_since: Some(gate_now()),
        spend_carry: SpendCarryEpisodes::default(),
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

    let base = 1_700_000_000i64;
    let gate = GateState {
        reject_streak: 1,
        rejecting_since: Some(Timestamp::from_second(base).unwrap()),
        spend_carry: SpendCarryEpisodes::default(),
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
fn reject_holds_prior_frame_as_committed_render() {
    let ws = workspace();
    let prior = agent_snapshot(&ws);
    let computed = compute_next_state(
        Ok(process_on(&ws, "terminal_9")),
        &prior,
        &Health::default(),
    );
    let (state, gate, rejected, released_via_escape_hatch, rejected_snapshot) =
        apply_gate(computed, true, &prior, &GateState::default(), gate_now());
    assert!(rejected);
    assert!(!released_via_escape_hatch);
    assert!(rejected_snapshot.is_some());
    // The committed render stays on the good frame, so the cache never
    // advances onto the demotion.
    assert!(state.snapshot.worktree_groups[0].rows[0].is_agent());
    assert_eq!(gate.reject_streak, 1);
    assert!(gate.rejecting_since.is_some());
    // Orthogonal to Health: a held regression is a *successful* fetch, so it
    // never arms the degraded alert nor counts toward self-close.
    assert!(state.health.alert.is_none());
    assert_eq!(state.health.failure_streak, 0);

    let computed = compute_next_state(Ok(snapshot(&ws)), &prior, &Health::default());

    let (state, gate, rejected, released_via_escape_hatch, rejected_snapshot) =
        apply_gate(computed, true, &prior, &GateState::default(), gate_now());

    assert!(rejected);
    assert!(!released_via_escape_hatch);
    assert!(rejected_snapshot.is_some());
    assert_eq!(
        state.snapshot.panes_produced_at_ms,
        prior.panes_produced_at_ms
    );
    assert!(state.snapshot.worktree_groups[0].rows[0].is_agent());
    assert_eq!(gate.reject_streak, 1);
}

#[test]
fn rejected_snapshot_then_failure_keeps_prior_frame_and_gate_episode() {
    let ws = workspace();
    let prior = agent_snapshot(&ws);
    let computed = compute_next_state(
        Ok(process_on(&ws, "terminal_9")),
        &prior,
        &Health::default(),
    );
    let (held, gate, rejected, _, _) =
        apply_gate(computed, true, &prior, &GateState::default(), gate_now());
    assert!(rejected);

    let failed = compute_next_state(
        Err("store not found".to_owned()),
        &held.snapshot,
        &held.health,
    );
    let (failed, next_gate, rejected, released, _) =
        apply_gate(failed, false, &held.snapshot, &gate, gate_now());

    assert!(!rejected);
    assert!(!released);
    assert!(failed.snapshot.worktree_groups[0].rows[0].is_agent());
    assert_eq!(next_gate, gate);
}

#[test]
fn accept_carries_collapsed_spend_without_touching_roster() {
    let ws = workspace();
    let mut prior = spend_snapshot(&ws, Some(12.50), Some(7.25), Some(4.00), 50);
    prior.today_spend_live_usd = Some(8.25);
    let incoming = spend_snapshot(&ws, None, None, None, 0);
    let incoming_row_id = incoming.worktree_groups[0].rows[0].id.clone();
    let computed = compute_next_state(Ok(incoming), &prior, &Health::default());

    let (state, gate, rejected, released_via_escape_hatch, _) =
        apply_gate(computed, true, &prior, &GateState::default(), gate_now());

    assert!(!rejected);
    assert!(!released_via_escape_hatch);
    assert_eq!(
        state
            .snapshot
            .value_tally
            .as_ref()
            .map(|tally| tally.year.usd),
        Some(12.50)
    );
    assert_eq!(
        state
            .snapshot
            .workspace_value_tally
            .as_ref()
            .map(|tally| tally.year.usd),
        Some(7.25)
    );
    assert_eq!(
        state.snapshot.providers[0]
            .spending
            .as_ref()
            .map(|tally| tally.year.usd),
        Some(4.00)
    );
    assert_eq!(state.snapshot.today_spend_live_usd, Some(8.25));
    assert_eq!(
        state.snapshot.providers[0].windows[0].used_percentage,
        Some(0),
        "mana windows stay detection-only"
    );
    assert_eq!(
        state.snapshot.worktree_groups[0].rows[0].id,
        incoming_row_id
    );
    assert_eq!(gate.spend_carry.fleet, Some(gate_now()));
    assert_eq!(gate.spend_carry.workspace, Some(gate_now()));
    assert_eq!(
        gate.spend_carry.providers[&crate::ids::AgentKind::new_unchecked("codex")],
        gate_now()
    );
}

#[test]
fn accept_keeps_different_nonzero_spend() {
    let ws = workspace();
    let prior = spend_snapshot(&ws, Some(12.50), Some(7.25), Some(4.00), 50);
    let incoming = spend_snapshot(&ws, Some(20.00), Some(18.00), Some(9.00), 0);
    let computed = compute_next_state(Ok(incoming), &prior, &Health::default());

    let (state, gate, rejected, released_via_escape_hatch, _) =
        apply_gate(computed, true, &prior, &GateState::default(), gate_now());

    assert!(!rejected);
    assert!(!released_via_escape_hatch);
    assert_eq!(
        state
            .snapshot
            .value_tally
            .as_ref()
            .map(|tally| tally.year.usd),
        Some(20.00)
    );
    assert_eq!(
        state.snapshot.providers[0]
            .spending
            .as_ref()
            .map(|tally| tally.year.usd),
        Some(9.00)
    );
    assert_eq!(gate, GateState::default());
}

#[test]
fn spend_carry_repairs_only_collapsed_families_and_recovers_independently() {
    let ws = workspace();
    let mut prior = spend_snapshot(&ws, Some(12.50), Some(7.25), Some(4.00), 50);
    prior.today_spend_live_usd = Some(8.25);
    prior.today_spend_epoch_secs = Some(7);
    prior
        .providers
        .push(provider("claude", Some(spend_tally(3.00)), 40));

    let mut incoming = spend_snapshot(&ws, Some(13.00), None, None, 0);
    incoming.today_spend_live_usd = Some(1.00);
    incoming.today_spend_epoch_secs = Some(8);
    incoming
        .providers
        .push(provider("claude", Some(spend_tally(3.50)), 0));
    let computed = compute_next_state(Ok(incoming), &prior, &Health::default());
    let (state, gate, _, _, _) =
        apply_gate(computed, true, &prior, &GateState::default(), gate_now());

    assert_eq!(state.snapshot.value_tally.as_ref().unwrap().year.usd, 13.00);
    assert_eq!(
        state
            .snapshot
            .workspace_value_tally
            .as_ref()
            .unwrap()
            .year
            .usd,
        7.25
    );
    assert_eq!(state.snapshot.today_spend_live_usd, Some(8.25));
    assert_eq!(state.snapshot.today_spend_epoch_secs, Some(7));
    assert_eq!(
        state.snapshot.providers[0]
            .spending
            .as_ref()
            .unwrap()
            .year
            .usd,
        4.00
    );
    assert_eq!(
        state.snapshot.providers[1]
            .spending
            .as_ref()
            .unwrap()
            .year
            .usd,
        3.50
    );
    assert!(gate.spend_carry.fleet.is_none());
    assert_eq!(gate.spend_carry.workspace, Some(gate_now()));
    assert_eq!(gate.spend_carry.providers.len(), 1);

    let mut recovered = spend_snapshot(&ws, Some(14.00), Some(8.00), None, 0);
    recovered.today_spend_live_usd = Some(9.00);
    recovered.today_spend_epoch_secs = Some(9);
    recovered
        .providers
        .push(provider("claude", Some(spend_tally(4.00)), 0));
    let computed = compute_next_state(Ok(recovered), &state.snapshot, &Health::default());
    let (state, gate, _, _, _) = apply_gate(computed, true, &state.snapshot, &gate, gate_now());

    assert_eq!(state.snapshot.workspace_value_tally.unwrap().year.usd, 8.00);
    assert_eq!(state.snapshot.today_spend_live_usd, Some(9.00));
    assert_eq!(
        state.snapshot.providers[0]
            .spending
            .as_ref()
            .unwrap()
            .year
            .usd,
        4.00
    );
    assert!(gate.spend_carry.workspace.is_none());
    assert_eq!(gate.spend_carry.providers.len(), 1);
}

#[test]
fn spend_carry_expires_each_family_without_coupling_mana_zero() {
    let ws = workspace();
    let prior = spend_snapshot(&ws, Some(12.50), Some(7.25), Some(4.00), 50);
    let incoming = spend_snapshot(&ws, None, None, None, 0);
    let base_ms = 1_700_000_000_000;
    let old = Timestamp::from_millisecond(base_ms).unwrap();
    let fresh = Timestamp::from_millisecond(
        base_ms + i64::try_from(ACCEPT_REGRESSION_AFTER.as_millis() / 2).unwrap(),
    )
    .unwrap();
    let gate = GateState {
        spend_carry: SpendCarryEpisodes {
            fleet: Some(old),
            workspace: Some(fresh),
            providers: BTreeMap::from([(crate::ids::AgentKind::new_unchecked("codex"), old)]),
        },
        ..GateState::default()
    };
    let now = Timestamp::from_millisecond(
        base_ms + i64::try_from(ACCEPT_REGRESSION_AFTER.as_millis()).unwrap(),
    )
    .unwrap();
    let computed = compute_next_state(Ok(incoming), &prior, &Health::default());
    let (state, gate, _, _, _) = apply_gate(computed, true, &prior, &gate, now);

    assert!(state.snapshot.value_tally.is_none());
    assert_eq!(state.snapshot.workspace_value_tally.unwrap().year.usd, 7.25);
    assert!(state.snapshot.providers[0].spending.is_none());
    assert_eq!(
        state.snapshot.providers[0].windows[0].used_percentage,
        Some(0)
    );
    assert!(gate.spend_carry.fleet.is_none());
    assert!(gate.spend_carry.workspace.is_some());
    assert!(gate.spend_carry.providers.is_empty());
}

#[test]
fn spend_carry_escape_hatch_commits_sustained_zero() {
    let ws = workspace();
    let prior = spend_snapshot(&ws, Some(12.50), Some(7.25), Some(4.00), 50);
    let incoming = spend_snapshot(&ws, None, None, None, 0);
    let computed = compute_next_state(Ok(incoming), &prior, &Health::default());
    let base = 1_700_000_000;
    let prev_gate = GateState {
        spend_carry: SpendCarryEpisodes {
            fleet: Some(Timestamp::from_second(base).unwrap()),
            workspace: Some(Timestamp::from_second(base).unwrap()),
            providers: BTreeMap::from([(
                crate::ids::AgentKind::new_unchecked("codex"),
                Timestamp::from_second(base).unwrap(),
            )]),
        },
        ..GateState::default()
    };
    let now = Timestamp::from_second(base + ACCEPT_REGRESSION_AFTER.as_secs() as i64).unwrap();

    let (state, gate, rejected, released_via_escape_hatch, _) =
        apply_gate(computed, true, &prior, &prev_gate, now);

    assert!(!rejected);
    assert!(!released_via_escape_hatch);
    assert!(state.snapshot.value_tally.is_none());
    assert!(state.snapshot.workspace_value_tally.is_none());
    assert!(state.snapshot.providers[0].spending.is_none());
    assert_eq!(
        state.snapshot.providers[0].windows[0].used_percentage,
        Some(0)
    );
    assert_eq!(gate, GateState::default());
}

#[test]
fn failed_fetch_keeps_a_gate_episode_open() {
    let ws = workspace();
    let prior = agent_snapshot(&ws);
    let computed = compute_next_state(
        Err("pane discovery failed".to_owned()),
        &prior,
        &Health::default(),
    );
    let prev_gate = GateState {
        reject_streak: 1,
        rejecting_since: Some(gate_now()),
        spend_carry: SpendCarryEpisodes::default(),
        rule: Some(GateRule::EmptyStampedFrame),
    };

    let (state, gate, rejected, released_via_escape_hatch, _) =
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
    let computed = compute_next_state(Ok(agent_snapshot(&ws)), &prior, &Health::default());
    // Carry a prior reject episode in; a clean accept clears it.
    let prev_gate = GateState {
        reject_streak: 2,
        rejecting_since: Some(gate_now()),
        spend_carry: SpendCarryEpisodes::default(),
        rule: Some(GateRule::AgentDemotedToProcess),
    };
    let (state, gate, rejected, released_via_escape_hatch, _) =
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
    let computed = compute_next_state(Ok(incoming), &prior, &Health::default());
    let prev_gate = GateState {
        reject_streak: ACCEPT_REGRESSION_AFTER_REJECTS,
        rejecting_since: Some(gate_now()),
        spend_carry: SpendCarryEpisodes::default(),
        rule: Some(GateRule::AgentDemotedToProcess),
    };

    let (state, gate, rejected, released_via_escape_hatch, _) =
        apply_gate(computed, true, &prior, &prev_gate, gate_now());

    assert!(!rejected);
    assert!(released_via_escape_hatch);
    assert_eq!(gate, GateState::default());
    assert!(state.snapshot.worktree_groups[0].rows[0].is_process());
}
