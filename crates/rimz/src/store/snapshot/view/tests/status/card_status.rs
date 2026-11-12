use super::*;
use crate::agents::{ParkDemotion, TurnErrorClass};
use crate::harness::CardStatus;

enum Evidence {
    Recoverable,
    Exhausted,
    ResetOnly,
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
    use AgentStatus::{Failed, Idle, Paused, Running, Waiting};
    use Evidence::{Exhausted, Recoverable, ResetOnly};
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
    ] {
        let capacities = match evidence {
            ResetOnly => provider_capacity("claude", vec![unprojectable_spent_window(-60)]),
            Recoverable | Exhausted => BTreeMap::new(),
        };
        let exhausted = match evidence {
            Exhausted => BTreeSet::from([(session.kind.clone(), session.agent_id.clone())]),
            Recoverable | ResetOnly => BTreeSet::new(),
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
        assert_eq!(
            reader.status(&snapshot, rolled_up, epoch()),
            expected,
            "{label}: reader status"
        );
    }
}
