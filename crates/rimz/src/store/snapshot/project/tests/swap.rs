//! Swaps of one agent: a launch whose reservation keeps the agent's launch
//! id and handle at a higher epoch, folded through the fences that keep a
//! replaced process's late events off the agent.

use super::*;
use crate::agents::state::{AgentEnd, LaunchHolders, agent_end, launch_holder, launch_members};
use crate::store::runtime::{RuntimeProjection, RuntimeScope};
use crate::store::snapshot::AgentRollup;

const LAUNCH: &str = "launch_a";

fn at(secs: i64) -> Timestamp {
    Timestamp::from_second(epoch().as_second() + secs).expect("offset from the test epoch")
}

/// A launch event for the agent's launch under row key `key` at `epoch`.
fn launch(
    secs: i64,
    state: AgentLaunchState,
    key: &str,
    epoch: u32,
    pane: Option<&str>,
) -> EventEnvelope {
    let mut payload = launch_payload(key, "coder");
    payload.launch_id = Some(LAUNCH.into());
    payload.launch.epoch = (epoch > 0).then_some(epoch);
    payload.state = state;
    payload.pane_id = pane.map(|raw| PaneId::parse(raw).expect("pane id"));
    let mut event = launch_event("codex", payload);
    event.timestamp = at(secs);
    event
}

/// A hook event of `session` on `pane`, stamped with the agent's launch at
/// `stamp`, or unstamped as an event written before the stamp existed.
fn hook(
    secs: i64,
    session: &str,
    stamp: Option<u32>,
    signal: &str,
    pane: Option<&str>,
) -> EventEnvelope {
    let mut params = json!({
        "agent_id": session,
        "agent_name": "coder",
        "signal": { "signal": signal },
    });
    if let Some(pane) = pane {
        params["pane_id"] = json!(pane);
    }
    if let Some(epoch) = stamp {
        params["launch_id"] = json!(LAUNCH);
        if epoch > 0 {
            params["epoch"] = json!(epoch);
        }
    }
    raw_lifecycle_at("codex", secs, params)
}

/// The agent at epoch 0: launched and registered as `s1` on pane `%1`.
fn first_session() -> Vec<EventEnvelope> {
    vec![
        launch(0, AgentLaunchState::Bound, LAUNCH, 0, Some("tmux:%1")),
        hook(1, "s1", Some(0), "registered", Some("tmux:%1")),
    ]
}

/// The first session plus a swap reservation `launch_b` at epoch 1, bound
/// on `pane` when given.
fn swapped(pane: Option<&str>) -> Vec<EventEnvelope> {
    let mut events = first_session();
    events.push(launch(2, AgentLaunchState::Starting, "launch_b", 1, None));
    if let Some(pane) = pane {
        events.push(launch(
            3,
            AgentLaunchState::Bound,
            "launch_b",
            1,
            Some(pane),
        ));
    }
    events
}

/// What the replacement's wrapper writes, in the order
/// `launch_and_supervise` writes it: the reservation's bind on `pane`, on a
/// resume the attach of `session` at the new epoch, then `session`'s first
/// hook stamped with that epoch.
fn wrapper_writes(secs: i64, session: &str, resumed: bool, pane: &str) -> Vec<EventEnvelope> {
    let mut writes = vec![launch(
        secs,
        AgentLaunchState::Bound,
        "launch_b",
        1,
        Some(pane),
    )];
    if resumed {
        let mut attach = EventEnvelope::agent_attached(
            workspace(),
            "session",
            &AgentKind::new_unchecked("codex"),
            AgentAttachPayload {
                epoch: Some(1),
                login: None,
                record: None,
                tier: None,
                mode: None,
                effective_isolation: None,
                agent_id: AgentSessionId::from(session),
                isolation: None,
                launch_id: Some(AgentSessionId::from(LAUNCH)),
                pane_id: PaneId::parse(pane).expect("pane id"),
                pane_pid: None,
                runtime_owner: RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    session,
                    9,
                    Some("start-9".into()),
                ),
            },
        );
        attach.timestamp = at(secs + 1);
        writes.push(attach);
    }
    writes.push(hook(secs + 2, session, Some(1), "registered", Some(pane)));
    writes
}

/// The first session and a swap's reservation, then the replacement's
/// wrapper writes for `session`.
fn swapped_into(session: &str, resumed: bool, pane: &str) -> Vec<EventEnvelope> {
    let mut events = swapped(None);
    events.extend(wrapper_writes(3, session, resumed, pane));
    events
}

fn row<'a>(agents: &'a [AgentState], id: &str) -> &'a AgentState {
    agents
        .iter()
        .find(|agent| agent.agent_id == id)
        .unwrap_or_else(|| panic!("row {id} in {agents:#?}"))
}

fn holder_ids(agents: &[AgentState]) -> Vec<String> {
    let members = launch_members(agents, &AgentKind::new_unchecked("codex"), &LAUNCH.into());
    let mut ids = launch_holder(&members)
        .iter()
        .map(|agent| agent.agent_id.to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn end(agents: &[AgentState]) -> AgentEnd {
    agent_end(agents, &AgentKind::new_unchecked("codex"), &LAUNCH.into())
}

#[test]
fn a_late_end_of_the_replaced_session_leaves_the_replacement_its_reservation() {
    for (late_end, old_paneless) in [(Some(0), false), (None, false), (Some(0), true)] {
        let mut events = swapped(None);
        let mut writes = wrapper_writes(3, "s2", false, "tmux:%1");
        writes.insert(1, hook(5, "s1", late_end, "ended", Some("tmux:%1")));
        if old_paneless {
            writes.insert(1, hook(4, "s1", Some(0), "turn_started", None));
        }
        for (offset, write) in (3..).zip(&mut writes) {
            write.timestamp = at(offset);
        }
        events.extend(writes);

        let agents = reduce_agent_states(&events);

        let new = row(&agents, "s2");
        assert_eq!(new.launch_id.as_deref(), Some(LAUNCH), "{late_end:?}");
        assert_eq!(new.name.as_deref(), Some("coder"));
        assert_eq!(new.epoch, 1);
        let old = row(&agents, "s1");
        assert_eq!(old.launch_id.as_deref(), Some(LAUNCH));
        assert_eq!(old.name.as_deref(), Some("coder"));
        assert_eq!(old.epoch, 0);
        assert!(old.ended_at.is_some());
        assert!(agents.iter().all(|agent| agent.agent_id != "launch_b"));
        assert_eq!(holder_ids(&agents), ["s2"]);
    }
}

#[test]
fn a_resumed_swap_fences_the_old_process_off_the_shared_row() {
    let mut events = swapped_into("s1", true, "tmux:%2");
    events.push(hook(5, "s1", Some(0), "turn_started", Some("tmux:%1")));
    events.push(hook(6, "s1", Some(0), "ended", Some("tmux:%1")));

    let agents = reduce_agent_states(&events);

    assert_eq!(agents.len(), 1, "{agents:#?}");
    let shared = row(&agents, "s1");
    assert_eq!(shared.epoch, 1);
    assert!(shared.ended_at.is_none());
    assert_eq!(
        shared.pane.as_ref().map(|pane| pane.pane_id.to_string()),
        Some("tmux:%2".to_owned())
    );
    assert_eq!(shared.status, AgentStatus::Idle);
    assert_eq!(holder_ids(&agents), ["s1"]);
    assert_eq!(end(&agents), AgentEnd::Live);

    events.push(hook(7, "s1", Some(1), "ended", Some("tmux:%2")));
    let agents = reduce_agent_states(&events);
    assert_eq!(end(&agents), AgentEnd::Ended, "{agents:#?}");
}

#[test]
fn the_holder_follows_a_reservation_only_once_it_stands() {
    let starting = reduce_agent_states(&swapped(None));
    assert_eq!(holder_ids(&starting), ["s1"]);
    assert!(LaunchHolders::new(&starting).replaced(row(&starting, "launch_b")));

    let bound = reduce_agent_states(&swapped(Some("tmux:%2")));
    assert_eq!(holder_ids(&bound), ["launch_b"]);
    assert!(LaunchHolders::new(&bound).replaced(row(&bound, "s1")));

    let mut events = swapped(None);
    events.push(launch(4, AgentLaunchState::Failed, "launch_b", 1, None));
    let failed_unbound = reduce_agent_states(&events);
    assert_eq!(holder_ids(&failed_unbound), ["s1"]);
    assert_eq!(end(&failed_unbound), AgentEnd::Live);

    let mut events = swapped(Some("tmux:%2"));
    events.push(launch(4, AgentLaunchState::Failed, "launch_b", 1, None));
    let failed_bound = reduce_agent_states(&events);
    let reservation = row(&failed_bound, "launch_b");
    assert!(reservation.launch_bound, "the bound fact survives Failed");
    assert_eq!(holder_ids(&failed_bound), ["launch_b"]);
    assert_eq!(end(&failed_bound), AgentEnd::Ended);
    let runtime = RuntimeProjection::from_parts(
        &failed_bound.clone().into(),
        RuntimeScope::Runtime,
        Vec::new(),
    );
    assert_eq!(
        runtime
            .agents
            .iter()
            .map(|agent| (agent.agent_id.as_str(), agent.name.as_deref()))
            .collect::<Vec<_>>(),
        [("launch_b", Some("coder"))],
        "the failed card stays in Runtime scope under the handle"
    );
    assert_eq!(
        end(&reduce_agent_states(&[launch(
            0,
            AgentLaunchState::Failed,
            "launch_z",
            0,
            None
        )])),
        AgentEnd::Unknown
    );
}

#[test]
fn runtime_shows_one_row_per_agent_and_audit_marks_the_replaced_one() {
    let events = swapped_into("s2", false, "tmux:%2");
    let agents = reduce_agent_states(&events);
    let rollup = AgentRollup::from(agents.clone());

    let runtime = RuntimeProjection::from_parts(&rollup, RuntimeScope::Runtime, Vec::new());
    assert_eq!(
        runtime
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        ["s2"]
    );
    assert!(
        runtime
            .expelled
            .contains(&(AgentKind::new_unchecked("codex"), "s1".into())),
        "the live replaced session is fenced from rediscovery"
    );

    let audit = RuntimeProjection::from_parts(&rollup, RuntimeScope::Audit, Vec::new());
    let marks = audit
        .agents
        .iter()
        .map(|agent| (agent.agent_id.as_str(), agent.replaced))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(marks, BTreeMap::from([("s1", true), ("s2", false)]));
}

#[test]
fn an_unstamped_event_takes_only_an_epoch_zero_reservation() {
    let mut events = swapped(Some("tmux:%2"));
    events.push(hook(4, "s9", None, "registered", Some("tmux:%2")));
    let agents = reduce_agent_states(&events);
    assert!(
        agents.iter().any(|agent| agent.agent_id == "launch_b"),
        "the swap's reservation waits for its own stamp: {agents:#?}"
    );
    assert_ne!(row(&agents, "s9").launch_id.as_deref(), Some(LAUNCH));

    let fresh = reduce_agent_states(&[
        launch(0, AgentLaunchState::Bound, LAUNCH, 0, Some("tmux:%1")),
        hook(1, "s1", None, "registered", Some("tmux:%1")),
    ]);
    assert_eq!(row(&fresh, "s1").launch_id.as_deref(), Some(LAUNCH));
}

#[test]
fn a_swap_keeps_the_handle_and_a_late_event_does_not_rename_the_replaced_row() {
    let mut events = swapped_into("s2", false, "tmux:%2");
    events.push(hook(6, "s1", Some(0), "turn_started", Some("tmux:%1")));
    let agents = reduce_agent_states(&events);

    assert_eq!(row(&agents, "s2").name.as_deref(), Some("coder"));
    assert_eq!(row(&agents, "s1").name.as_deref(), Some("coder"));

    let mut other = launch_payload("launch_x", "coder");
    other.launch_id = Some("launch_x".into());
    events.push(launch_event("codex", other));
    let agents = reduce_agent_states(&events);
    assert!(
        agents.iter().all(|agent| agent.agent_id != "launch_x"),
        "another launch cannot take the agent's handle"
    );
}

#[test]
fn an_ended_swapped_agent_resumed_in_place_folds_its_new_events() {
    let mut events = swapped_into("s2", false, "tmux:%2");
    events.push(hook(6, "s2", Some(1), "ended", Some("tmux:%2")));
    events.push(hook(7, "s2", Some(1), "registered", Some("tmux:%3")));
    events.push(hook(8, "s2", Some(1), "turn_started", Some("tmux:%3")));

    let agents = reduce_agent_states(&events);

    let resumed = row(&agents, "s2");
    assert!(resumed.ended_at.is_none());
    assert_eq!(resumed.epoch, 1);
    assert_eq!(resumed.status, AgentStatus::Running);
    assert_eq!(holder_ids(&agents), ["s2"]);
}

#[test]
fn a_provisional_row_a_wrapper_attached_stands_beside_its_launchs_reservation() {
    let agents = reduce_agent_states(&[
        launch(0, AgentLaunchState::Starting, LAUNCH, 0, None),
        launch(1, AgentLaunchState::Bound, "launch_b", 0, Some("tmux:%9")),
        attach_event("codex", LAUNCH, Some("launch_b"), "tmux:%9", None, 9),
    ]);

    let attached = row(&agents, LAUNCH);
    assert!(
        attached.launch_bound,
        "the attach recorded the wrapper's pane bind"
    );
    assert!(!LaunchHolders::new(&agents).replaced(attached));
}

#[test]
fn only_a_swap_reservation_takes_the_handle_through_its_launch() {
    let relaunch = |secs: i64, key: &str, epoch: u32| {
        let mut payload = launch_payload(key, "coder");
        payload.launch_id = Some("launch_resume".into());
        payload.launch.epoch = (epoch > 0).then_some(epoch);
        payload.state = AgentLaunchState::Bound;
        payload.pane_id = Some(PaneId::parse("tmux:%9").expect("pane id"));
        let mut event = launch_event("codex", payload);
        event.timestamp = at(secs);
        event
    };
    let resumed = |secs: i64| {
        let mut event = attach_event("codex", "s1", Some("launch_resume"), "tmux:%9", None, 9);
        event.timestamp = at(secs);
        event
    };
    let mut events = first_session();
    events.extend([relaunch(2, "resume-row", 0), resumed(3)]);
    events.extend([relaunch(4, "resume-row", 0), resumed(5)]);
    let agents = reduce_agent_states(&events);
    assert_eq!(row(&agents, "s1").name.as_deref(), Some("coder"));
    assert_ne!(row(&agents, "resume-row").name.as_deref(), Some("coder"));

    events.push(relaunch(6, "launch_again", 0));
    let agents = reduce_agent_states(&events);
    assert!(
        agents.iter().all(|agent| agent.agent_id != "launch_again"),
        "a provisional launch under the owned handle at the owner's epoch is consumed"
    );

    events.push(relaunch(7, "launch_swap", 1));
    let agents = reduce_agent_states(&events);
    assert_eq!(row(&agents, "launch_swap").name.as_deref(), Some("coder"));
}
