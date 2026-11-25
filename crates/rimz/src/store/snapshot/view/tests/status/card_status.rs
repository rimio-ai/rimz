use super::*;
use crate::agents::{ParkDemotion, TurnErrorClass};
use crate::harness::CardStatus;

enum Evidence {
    Recoverable,
    Exhausted,
    ResetOnly,
    Refused,
    /// Refusals of both `REFUSED` and `MOVED_TO` on the login.
    RefusedBoth,
}

const REFUSED: &str = "claude-fable-5-2";
const MOVED_TO: &str = "claude-fable-5-5";
const MOVED_TO_SENTENCE: &str = "Claude Fable 5.5 is not available on your plan.";
const SENTENCE: &str =
    "Claude Fable 5.2 is not available on your plan. Switch models with /model to keep working.";

/// `session` on `model` (lifecycle and context alike), its marker, if any, stamped `refused_on`.
fn on_model(mut session: AgentState, model: &str, refused_on: Option<&str>) -> AgentState {
    session.model = Some(REFUSED.to_owned());
    let context = session.context.get_or_insert_with(|| {
        crate::agents::AgentContext::new("claude", epoch() - std::time::Duration::from_secs(600))
    });
    context.model_id = Some(model.to_owned());
    if let Some(marker) = context.turn_error.as_mut() {
        marker.model = refused_on.map(str::to_owned);
    }
    session
}

fn pausing_agent(status: AgentStatus, class: TurnErrorClass) -> AgentState {
    let activity_age = if status == AgentStatus::Failed { 5 } else { 60 };
    agent("claude", "root", status, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(120)
        .active_ago(activity_age)
        .turn_error_class(10, "provider pause", class)
}

fn ended_turn_wait() -> AgentState {
    let mut session = pausing_agent(AgentStatus::Waiting, TurnErrorClass::PausedRateLimit);
    session
        .context
        .as_mut()
        .unwrap()
        .turn_error
        .as_mut()
        .unwrap()
        .ended_turn = true;
    session
}

fn budget_park(mut session: AgentState) -> AgentState {
    session.budget_park = Some(crate::agents::BudgetPark {
        cap_usd: 5.0,
        spend_usd: 5.25,
        window: crate::agents::BudgetWindow::Session,
        at: epoch(),
        scope: crate::agents::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
        reset_window: None,
    });
    session
}

#[test]
fn card_and_reader_agree_on_pausing_rows() {
    use AgentStatus::Success;
    use AgentStatus::{Failed, Idle, Paused, Running, Waiting};
    use Evidence::{Exhausted, Recoverable, Refused, RefusedBoth, ResetOnly};
    use TurnErrorClass::{PausedOverloaded, PausedRateLimit, PausedSpendLimit};

    for (label, session, evidence, with_live_child, expected) in [
        (
            "recoverable terminal rate limit",
            pausing_agent(Failed, PausedRateLimit),
            Recoverable,
            false,
            Paused,
        ),
        (
            "recoverable terminal spend limit",
            pausing_agent(Failed, PausedSpendLimit),
            Recoverable,
            false,
            Paused,
        ),
        (
            "recoverable terminal overload",
            pausing_agent(Failed, PausedOverloaded),
            Recoverable,
            false,
            Paused,
        ),
        (
            "recoverable active rate limit",
            pausing_agent(Running, PausedRateLimit),
            Recoverable,
            false,
            Paused,
        ),
        (
            "exhausted rate-limit resume",
            pausing_agent(Failed, PausedRateLimit),
            Exhausted,
            false,
            Failed,
        ),
        (
            "exhausted spend-limit resume",
            pausing_agent(Failed, PausedSpendLimit),
            Exhausted,
            false,
            Failed,
        ),
        (
            "exhausted overload resume",
            pausing_agent(Failed, PausedOverloaded),
            Exhausted,
            false,
            Failed,
        ),
        (
            "reset-only rate-limit window",
            pausing_agent(Failed, PausedRateLimit),
            ResetOnly,
            false,
            Failed,
        ),
        (
            "reset-only spend-limit window",
            pausing_agent(Failed, PausedSpendLimit),
            ResetOnly,
            false,
            Failed,
        ),
        (
            "reset-only window does not exhaust overload",
            pausing_agent(Failed, PausedOverloaded),
            ResetOnly,
            false,
            Paused,
        ),
        (
            "budget park without a provider marker",
            budget_park(agent("claude", "root", Running, 0)),
            Recoverable,
            false,
            Paused,
        ),
        (
            "budget park outranks exhausted provider pause",
            budget_park(pausing_agent(Failed, PausedRateLimit)),
            Exhausted,
            false,
            Paused,
        ),
        (
            "interrupted wait clears budget park",
            budget_park(
                agent("claude", "root", Waiting, 0)
                    .active_ago(60)
                    .settle(10, TurnSettleOutcome::Interrupted),
            ),
            Recoverable,
            false,
            Idle,
        ),
        (
            "ended-turn wait with recoverable pause",
            ended_turn_wait(),
            Recoverable,
            false,
            Paused,
        ),
        (
            "ended-turn wait with exhausted resume",
            ended_turn_wait(),
            Exhausted,
            false,
            Failed,
        ),
        (
            "live child cannot mask recoverable parent pause",
            pausing_agent(Running, PausedRateLimit),
            Recoverable,
            true,
            Paused,
        ),
        (
            "live child cannot mask exhausted parent pause",
            pausing_agent(Running, PausedRateLimit),
            Exhausted,
            true,
            Failed,
        ),
        (
            "a resting agent on a refused model",
            on_model(
                agent("claude", "root", Success, 0).active_ago(60),
                REFUSED,
                None,
            ),
            Refused,
            false,
            Failed,
        ),
        (
            "a pause on the refused model it landed on",
            on_model(
                pausing_agent(Running, PausedRateLimit),
                REFUSED,
                Some(REFUSED),
            ),
            Refused,
            false,
            Failed,
        ),
        (
            "a pause whose agent left the refused model",
            on_model(
                pausing_agent(Running, PausedRateLimit),
                "claude-fable-5-5",
                Some(REFUSED),
            ),
            Refused,
            false,
            Idle,
        ),
        (
            "a failed limit turn whose agent left the refused model",
            on_model(
                pausing_agent(Failed, PausedRateLimit),
                MOVED_TO,
                Some(REFUSED),
            ),
            Refused,
            false,
            Idle,
        ),
        (
            "a session that ended mid-turn after leaving the refused model reads as before",
            {
                let mut ended = on_model(
                    pausing_agent(Failed, PausedRateLimit),
                    MOVED_TO,
                    Some(REFUSED),
                );
                ended.ended_at = Some(ago(1));
                ended
            },
            Refused,
            false,
            Paused,
        ),
        (
            "a pause that left one refused model for another",
            on_model(
                pausing_agent(Running, PausedRateLimit),
                MOVED_TO,
                Some(REFUSED),
            ),
            RefusedBoth,
            false,
            Failed,
        ),
        (
            "a budget park whose agent left the refused model",
            budget_park(on_model(
                pausing_agent(Running, PausedRateLimit),
                MOVED_TO,
                Some(REFUSED),
            )),
            Refused,
            false,
            Paused,
        ),
        (
            "a sibling version beside a refused model",
            on_model(
                agent("claude", "root", Success, 0).active_ago(60),
                "claude-fable-5-5",
                None,
            ),
            Refused,
            false,
            Success,
        ),
        (
            "a running agent on a refused model",
            on_model(
                agent("claude", "root", Running, 0).active_ago(5),
                REFUSED,
                None,
            ),
            Refused,
            false,
            Running,
        ),
    ] {
        let capacities = match evidence {
            ResetOnly => provider_capacity("claude", vec![unprojectable_spent_window(-60)]),
            Refused => provider_capacity("claude", Vec::new())
                .into_iter()
                .map(|(login, capacity)| (login, capacity.with_refusal(REFUSED, ago(5), SENTENCE)))
                .collect(),
            RefusedBoth => provider_capacity("claude", Vec::new())
                .into_iter()
                .map(|(login, capacity)| {
                    let capacity = capacity.with_refusal(REFUSED, ago(5), SENTENCE);
                    (
                        login,
                        capacity.with_refusal(MOVED_TO, ago(5), MOVED_TO_SENTENCE),
                    )
                })
                .collect(),
            Recoverable | Exhausted => BTreeMap::new(),
        };
        let exhausted = match evidence {
            Exhausted => BTreeSet::from([(session.kind.clone(), session.agent_id.clone())]),
            Recoverable | ResetOnly | Refused | RefusedBoth => BTreeSet::new(),
        };
        let demotion = ParkDemotion::new(&capacities, exhausted.clone(), epoch());
        let reader = CardStatus::from_demotion(ParkDemotion::new(&capacities, exhausted, epoch()));
        let mut agents = vec![session];
        if with_live_child {
            agents.push(child_state("root", "child", Running, 5));
            // A fresh same-pane root challenges the other activity-clock fold too.
            agents.push(
                agent("claude", "fork", Running, 0)
                    .in_pane("%1")
                    .active_ago(5),
            );
        }
        let snapshot = room(agents);
        let rolled_up = rollup_agent(&snapshot, "root");
        let groups = snapshot.build_worktree_groups(
            vec![row_from_agent(rolled_up, epoch())],
            &demotion,
            &[],
            &[],
        );
        let card = &groups[0].rows[0];
        if with_live_child {
            assert_eq!(card.sub_agents().len(), 1, "{label}: child is projected");
            assert!(
                card.sub_agents()[0].holds_parent_turn(),
                "{label}: projected child holds the parent turn"
            );
        }
        assert_eq!(card.status(), Some(expected), "{label}: card status");
        if expected == Failed
            && let Some(sentence) = match evidence {
                Refused => Some(SENTENCE),
                RefusedBoth => Some(MOVED_TO_SENTENCE),
                Recoverable | Exhausted | ResetOnly => None,
            }
        {
            assert_eq!(
                card.turn_error_label(),
                Some(sentence),
                "{label}: card label"
            );
        }
        // The rowless path reads no live child, so only the refusal rows share the card's answer
        // there.
        if matches!(evidence, Refused | RefusedBoth) {
            assert_eq!(
                snapshot.agents[0].rowless_status(&demotion, epoch()).0,
                expected,
                "{label}: rowless status"
            );
        }
        assert_eq!(
            reader.status(&snapshot, rolled_up, epoch()),
            expected,
            "{label}: reader status"
        );
    }
}

#[test]
fn promoted_child_row_without_a_source_settles_on_its_own_card() {
    // A launched child whose parent has no row is promoted to a top-level card,
    // and the projection finds no source agent for it.
    let promoted = |status: AgentStatus| {
        let mut child = agent("claude", "child-1", status, 1_000)
            .worktree("/repo/child")
            .in_pane("%2");
        child.parent_agent_id = Some("parent-gone".into());
        child.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        child.launch_depth = Some(1);
        child
    };
    let project = |child: AgentState| {
        let snapshot = room(vec![child]).with_live_panes_and_provider_capacities(
            vec![pane("%2", "claude", "/repo/child")],
            None,
            &provider_capacity("claude", vec![unprojectable_spent_window(-60)]),
        );
        let row = row(&snapshot, "child-1");
        (row.status(), row.turn_error_label().map(str::to_owned))
    };

    let interrupted = promoted(AgentStatus::Waiting).settle(10, TurnSettleOutcome::Interrupted);
    assert_eq!(project(interrupted).0, Some(AgentStatus::Idle));

    // The spent window would demote a sourced pause to failed.
    let paused = promoted(AgentStatus::Running).paused_turn_error(10, "rate limited");
    assert_eq!(
        project(paused),
        (Some(AgentStatus::Paused), Some("rate limited".to_owned()))
    );

    let mut budgeted = promoted(AgentStatus::Running);
    budgeted.budget_park = Some(crate::agents::BudgetPark {
        cap_usd: 5.0,
        spend_usd: 5.25,
        window: crate::agents::BudgetWindow::Session,
        at: epoch(),
        scope: crate::agents::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
        reset_window: None,
    });
    assert_eq!(project(budgeted), (Some(AgentStatus::Running), None));
}

#[test]
fn launched_children_read_the_refusal_and_its_sentence_on_their_line_and_their_own_row() {
    use AgentStatus::{Failed, Idle, Running, Success};
    let refused = || -> BTreeMap<_, _> {
        provider_capacity("claude", Vec::new())
            .into_iter()
            .map(|(login, capacity)| (login, capacity.with_refusal(REFUSED, ago(5), SENTENCE)))
            .collect()
    };
    let launched = |mut child: AgentState, parent: &str| {
        child.launch_depth = Some(1);
        child.parent_agent_id = Some(parent.into());
        child.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        child
    };
    let parent = || {
        agent("claude", "root", Success, 0)
            .worktree("/repo/main")
            .active_ago(120)
    };

    // Refused through the record alone, under a rendered parent: the line quotes the sentence.
    let resting = launched(
        on_model(child_state("root", "child", Success, 60), REFUSED, None),
        "root",
    );
    let snapshot = room_with_agent_panes_and_capacities(vec![parent(), resting], refused());
    let line = &row(&snapshot, "root").sub_agents()[0];
    assert_eq!(
        (line.status, line.turn_error_label.as_deref()),
        (Failed, Some(SENTENCE)),
        "a refused child's line"
    );

    // A child that left the refused model rests with no old sentence beside it.
    let left = launched(
        on_model(
            child_state("root", "child", Running, 60)
                .turn_started_ago(120)
                .turn_error_class(10, "provider pause", TurnErrorClass::PausedRateLimit),
            MOVED_TO,
            Some(REFUSED),
        ),
        "root",
    );
    let snapshot = room_with_agent_panes_and_capacities(vec![parent(), left], refused());
    let line = &row(&snapshot, "root").sub_agents()[0];
    assert_eq!(
        (line.status, line.turn_error_label.as_deref()),
        (Idle, None),
        "a child that left the refused model"
    );

    // A launched child promoted to its own row, its parent not rendered, reads the refusal too.
    let promoted = launched(
        on_model(
            agent("claude", "child-1", Success, 1_000)
                .worktree("/repo/child")
                .in_pane("%2")
                .active_ago(60),
            REFUSED,
            None,
        ),
        "parent-gone",
    );
    let snapshot = room(vec![promoted]).with_live_panes_and_provider_capacities(
        vec![pane("%2", "claude", "/repo/child")],
        None,
        &refused(),
    );
    let promoted = row(&snapshot, "child-1");
    assert_eq!(
        (promoted.status(), promoted.turn_error_label()),
        (Some(Failed), Some(SENTENCE)),
        "a promoted child's own row"
    );
}

#[test]
fn promoted_launched_row_refusal_beats_waiting_child() {
    use AgentStatus::{Failed, Success, Waiting};
    let refused: BTreeMap<_, _> = provider_capacity("claude", Vec::new())
        .into_iter()
        .map(|(login, capacity)| (login, capacity.with_refusal(REFUSED, ago(5), SENTENCE)))
        .collect();

    let mut promoted = on_model(
        agent("claude", "child-1", Success, 1_000)
            .worktree("/repo/child")
            .in_pane("%2")
            .active_ago(60),
        REFUSED,
        None,
    );
    promoted.launch_depth = Some(1);
    promoted.parent_agent_id = Some("parent-gone".into());
    promoted.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));

    let mut grandchild = child_state("child-1", "grandchild", Waiting, 5);
    grandchild.waiting_since = Some(ago(5));

    let snapshot = room(vec![promoted, grandchild]).with_live_panes_and_provider_capacities(
        vec![pane("%2", "claude", "/repo/child")],
        None,
        &refused,
    );
    let promoted = row(&snapshot, "child-1");
    assert_eq!(
        (
            promoted.status(),
            promoted.turn_error_label(),
            promoted.delegated_wait().is_none(),
        ),
        (Some(Failed), Some(SENTENCE), true),
        "a promoted launched row resting on a refused model must keep Failed and carry no delegated wait"
    );
}

#[test]
fn promoted_raw_failed_launched_child_reads_paused_on_current_turn_limit_marker() {
    use AgentStatus::{Failed, Paused};

    let promoted = || {
        let mut child = agent("antigravity", "child-1", Failed, 1_000)
            .worktree("/repo/child")
            .in_pane("%2")
            .turn_started_ago(120)
            .active_ago(5);
        child.parent_agent_id = Some("parent-gone".into());
        child.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        child.launch_depth = Some(1);
        child
    };
    let project = |child: AgentState| {
        let snapshot = room(vec![child]).with_live_panes_and_provider_capacities(
            vec![pane("%2", "agy", "/repo/child")],
            None,
            &BTreeMap::new(),
        );
        let row = row(&snapshot, "child-1");
        (row.status(), row.turn_error_label().map(str::to_owned))
    };

    let paused = promoted().turn_error_class(
        10,
        "Individual quota reached",
        TurnErrorClass::PausedRateLimit,
    );
    assert_eq!(
        project(paused),
        (Some(Paused), Some("Individual quota reached".to_owned())),
        "a promoted raw-failed child with a current-turn limit marker reads paused"
    );

    let stale = promoted().turn_error_class(
        200,
        "Individual quota reached",
        TurnErrorClass::PausedRateLimit,
    );
    assert_eq!(
        project(stale),
        (Some(Failed), None),
        "a marker older than turn_started_at leaves the promoted row failed"
    );

    assert_eq!(
        project(promoted()),
        (Some(Failed), None),
        "without a marker the promoted raw-failed row stays failed"
    );
}
