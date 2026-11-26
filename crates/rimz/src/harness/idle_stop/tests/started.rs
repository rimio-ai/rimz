//! The started read beside `decide`: each held stop lists what holds it, and no row outside
//! the holding group is one `decide` holds on.

use super::*;
use crate::address::{AddressContext, ChannelOrigin};
use crate::agents::LaunchedBy;
use crate::harness::idle_stop::started::{RowKind, Seat, Started, StartedRow, started};
use crate::harness::schedule::catalog::TaskCatalog;
use crate::store::message::{HarnessNotice, MessageSender};

fn read(
    store: &Store,
    root: &std::path::Path,
    agents: &[AgentState],
    agent: &AgentState,
    you: bool,
) -> Started {
    let snapshot = SidebarSnapshot::build_with_agents(
        store.paths().workspace_id.clone(),
        agents.to_vec(),
        ts(DUE),
    );
    let reader = AddressContext {
        channel: None,
        origin: ChannelOrigin::Root,
    };
    let shown = find_agent(&snapshot.agents, &agent.kind, &agent.agent_id).expect("shown agent");
    let seat = Seat {
        root,
        reader: &reader,
        you,
        rimz: "rimz",
    };
    started(store, &snapshot, shown, &seat, ts(DUE)).expect("started")
}

fn rows(rows: &[StartedRow]) -> Vec<(RowKind, &str)> {
    rows.iter()
        .map(|row| (row.kind, row.name.as_str()))
        .collect()
}

/// The verdict and the read together, over the same agents.
fn both(
    store: &Store,
    root: &std::path::Path,
    agents: &[AgentState],
    agent: &AgentState,
    now: i64,
) -> (Verdict, Started) {
    let (_, stop) = rested();
    let verdict = decide(store, agents, agent, &stop, ts(now)).expect("decide");
    (verdict, read(store, root, agents, agent, true))
}

/// A case name, the hold it expects, the row kinds it lists, and the change that stages it.
type Case<'a> = (&'a str, Hold, &'a [RowKind], &'a dyn Fn(&mut AgentState));

fn shell(id: &str) -> crate::agents::BackgroundShell {
    crate::agents::BackgroundShell {
        id: id.to_owned(),
        command: Some("cargo xtask gate".to_owned()),
        description: None,
        started_at: ts(900),
    }
}

fn command_wait(name: &str) -> PendingWait {
    PendingWait {
        class: crate::agents::PendingWaitClass::Working,
        name: name.to_owned(),
        trigger: PendingWaitTrigger::Command {
            command: "cargo xtask gate".to_owned(),
        },
        armed_at: Some(ts(900)),
    }
}

#[test]
fn each_rollup_hold_lists_its_holder_and_the_agents_own_turn_lists_none() {
    let (dir, store) = fixture();
    let (clear, _) = rested();
    let cases: [Case<'_>; 10] = [
        ("ended", Hold::Ended, &[], &|agent| {
            agent.ended_at = Some(ts(1_100));
        }),
        (
            "awaiting input",
            Hold::AwaitingInput,
            &[RowKind::Question],
            &|agent| {
                agent.status = AgentStatus::Waiting;
                agent.waiting_since = Some(ts(1_100));
            },
        ),
        (
            "budget park",
            Hold::BudgetPark,
            &[RowKind::Budget],
            &|agent| {
                agent.budget_park = Some(crate::agents::BudgetPark {
                    cap_usd: 1.0,
                    spend_usd: 1.0,
                    window: crate::agents::BudgetWindow::Session,
                    at: ts(1_000),
                    scope: crate::agents::BudgetScope::Agent,
                    account_kind: None,
                    resets_at: None,
                    reset_window: None,
                });
            },
        ),
        ("compacting", Hold::Compacting, &[], &|agent| {
            agent.compacting_since = Some(ts(1_100));
        }),
        ("mid-turn", Hold::Busy, &[], &|agent| {
            agent.status = AgentStatus::Running;
        }),
        // An errored or paused turn sits at rest until a new prompt: nothing ends it alone.
        ("failed turn", Hold::Busy, &[RowKind::Turn], &|agent| {
            agent.status = AgentStatus::Failed;
        }),
        ("paused turn", Hold::Busy, &[RowKind::Turn], &|agent| {
            agent.status = AgentStatus::Paused;
        }),
        ("parked on background work", Hold::Busy, &[], &|agent| {
            agent.status = AgentStatus::Running;
            agent.phase = TurnPhase::Parked;
        }),
        (
            "background shell",
            Hold::Busy,
            &[RowKind::Shell],
            &|agent| {
                agent.background_shells.push(shell("shell-1"));
            },
        ),
        (
            "sleeping on a wait",
            Hold::Busy,
            &[RowKind::Wait],
            &|agent| {
                agent.pending_waits.push(command_wait("wait-gate"));
            },
        ),
    ];
    for (name, hold, kinds, change) in cases {
        let mut agent = clear.clone();
        change(&mut agent);
        let (verdict, read) = both(
            &store,
            dir.path(),
            std::slice::from_ref(&agent),
            &agent,
            DUE,
        );
        assert_eq!(verdict, Verdict::Hold(hold), "{name}");
        let held: Vec<RowKind> = read.holding.iter().map(|row| row.kind).collect();
        assert_eq!(held, kinds, "{name}");
        if let Some(turn) = read.holding.iter().find(|row| row.kind == RowKind::Turn) {
            assert_eq!(turn.status, agent.status.as_str(), "{name}");
            assert_eq!(turn.command, None, "{name}: only a new prompt ends it");
        }
        assert!(
            read.closing.is_empty() && read.outliving.is_empty(),
            "{name}"
        );
    }

    let (verdict, read) = both(
        &store,
        dir.path(),
        std::slice::from_ref(&clear),
        &clear,
        DUE - 1,
    );
    assert_eq!(verdict, Verdict::Hold(Hold::Clock));
    assert!(read.is_empty(), "the clock is no row");
}

#[test]
fn a_wait_offers_its_cancel_only_to_the_agent_it_wakes() {
    let (dir, store) = fixture();
    let (mut agent, _) = rested();
    agent.pending_waits.push(command_wait("wait-gate"));
    let agents = [agent.clone()];
    let own = read(&store, dir.path(), &agents, &agent, true);
    assert_eq!(
        own.holding[0].command.as_deref(),
        Some("rimz wait cancel wait-gate")
    );
    let other = read(&store, dir.path(), &agents, &agent, false);
    assert_eq!(other.holding[0].command, None);
    assert_eq!(other.holding[0].detail, ["wakes after cargo xtask gate"]);
}

#[test]
fn a_running_provider_native_subagent_holds_its_busy_parent() {
    let (dir, store) = fixture();
    let (mut agent, _) = rested();
    agent.status = AgentStatus::Running;
    let (mut native, _) = rested();
    native.agent_id = "native-1".into();
    native.name = Some("Explore".to_owned());
    native.parent_agent_id = Some(agent.agent_id.clone());
    native.status = AgentStatus::Running;
    let agents = [agent.clone(), native];
    let (verdict, read) = both(&store, dir.path(), &agents, &agent, DUE);
    assert_eq!(verdict, Verdict::Hold(Hold::Busy));
    assert_eq!(rows(&read.holding), [(RowKind::Native, "Explore")]);
}

#[test]
fn a_launched_child_holds_while_it_owes_and_closes_with_its_parent_once_collected() {
    for (status, joined, expected, holding) in [
        (
            RunStatus::Running,
            false,
            Verdict::Hold(Hold::Owed(OwedWake::Subagents)),
            true,
        ),
        (
            RunStatus::Completed,
            false,
            Verdict::Hold(Hold::Owed(OwedWake::Subagents)),
            true,
        ),
        (
            RunStatus::Completed,
            true,
            Verdict::Stop { idle_secs: 180 },
            false,
        ),
    ] {
        let (dir, store) = fixture();
        let (agent, _) = rested();
        register(&store, "session-1", None);
        register(&store, "child", Some(&agent));
        let mut run = RunRecord::new(
            store.paths().workspace_id.clone(),
            agent.kind.clone(),
            PermissionMode::Auto,
            "work".to_owned(),
            dir.path().to_owned(),
        );
        run.agent_id = Some("child".into());
        run.status = status;
        run.joined_at = joined.then_some(ts(1_100));
        crate::harness::run::create(store.paths(), &run).expect("run");
        let agents = [agent.clone(), kept_child(&agent, "child")];
        let (verdict, read) = both(&store, dir.path(), &agents, &agent, DUE);
        let case = format!("{status:?} joined={joined}");
        assert_eq!(verdict, expected, "{case}");
        let child = [(RowKind::Subagent, "@child")];
        let (held, closing): (&[_], &[_]) = if holding {
            (&child, &[])
        } else {
            (&[], &child)
        };
        assert_eq!(rows(&read.holding), held, "{case}");
        assert_eq!(rows(&read.closing), closing, "{case}");
        assert!(read.outliving.is_empty(), "{case}");
    }
}

#[test]
fn a_working_descendant_holds_the_tree_under_its_path() {
    let (dir, store) = fixture();
    let (parent, _) = rested();
    let child = kept_child(&parent, "child");
    let mut grandchild = kept_child(&child, "grandchild");
    grandchild.status = AgentStatus::Running;
    let agents = [parent.clone(), child, grandchild];
    let (verdict, read) = both(&store, dir.path(), &agents, &parent, DUE);
    assert_eq!(verdict, Verdict::Hold(Hold::Tree));
    assert_eq!(
        rows(&read.holding),
        [(RowKind::Subagent, "@child › @grandchild")]
    );
    assert_eq!(
        read.holding[0].command.as_deref(),
        Some("rimz agents stop @grandchild")
    );
    assert_eq!(rows(&read.closing), [(RowKind::Subagent, "@child")]);
    assert_eq!(read.closing[0].command, None, "closing with it is no hint");
}

#[test]
fn a_lingering_failed_child_lists_as_closing_unless_in_debt() {
    let (dir, store) = fixture();
    let (parent, _) = rested();
    register(&store, "session-1", None);
    register(&store, "child", Some(&parent));
    let mut run = RunRecord::new(
        store.paths().workspace_id.clone(),
        parent.kind.clone(),
        PermissionMode::Auto,
        "work".to_owned(),
        dir.path().to_owned(),
    );
    run.agent_id = Some("child".into());
    run.status = RunStatus::Failed;
    run.joined_at = Some(ts(1_100));
    crate::harness::run::create(store.paths(), &run).expect("run");
    let mut child = kept_child(&parent, "child");
    child.status = AgentStatus::Failed;
    let agents = [parent.clone(), child.clone()];
    let (verdict, read) = both(&store, dir.path(), &agents, &parent, DUE);
    assert_eq!(verdict, Verdict::Stop { idle_secs: 180 });
    assert!(read.holding.is_empty(), "{:?}", read.holding);
    assert_eq!(rows(&read.closing), [(RowKind::Subagent, "@child")]);

    let message = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &child,
        "retry".to_owned(),
        DeliveryGate::Done,
    );
    store
        .queue_message(&message, "idle-stop-test")
        .expect("queue");
    let (verdict, read) = both(&store, dir.path(), &agents, &parent, DUE);
    assert_eq!(verdict, Verdict::Hold(Hold::Tree));
    assert_eq!(rows(&read.holding), [(RowKind::Subagent, "@child")]);
    assert!(read.closing.is_empty(), "{:?}", read.closing);
}

/// A team leader seat the shown agent launched, registered in the log and on the row.
fn launched_team(store: &Store, worktree: &std::path::Path, agent: &AgentState) -> AgentState {
    let kind = agent.kind.clone();
    let mut observation =
        AgentLifecycleObservation::new(Some("leader".into()), LifecycleSignal::Registered);
    observation.agent_name = Some("leader".into());
    observation.launch.team = Some("forge".into());
    observation.launch.launched_by = Some(Box::new(LaunchedBy {
        kind: kind.clone(),
        agent_id: agent.agent_id.clone(),
    }));
    store
        .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
            session_name: "idle-stop-test",
            agent_kind: kind,
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .expect("register leader");
    let mut leader = store
        .snapshot()
        .expect("snapshot")
        .agents
        .iter()
        .find(|row| row.agent_id.as_str() == "leader")
        .expect("leader row")
        .clone();
    leader.worktree_path = Some(worktree.to_string_lossy().into_owned());
    leader
}

#[test]
fn a_launched_team_holds_until_its_board_reads_done_then_outlives() {
    for (done, detached, holds) in [
        (false, false, true),
        (true, false, false),
        (false, true, false),
    ] {
        let (dir, store) = fixture();
        let (agent, _) = rested();
        register(&store, "session-1", None);
        let leader = launched_team(&store, dir.path(), &agent);
        let mut record = RunRecord::new(
            store.paths().workspace_id.clone(),
            agent.kind.clone(),
            PermissionMode::Auto,
            "work".into(),
            dir.path().into(),
        );
        record.status = RunStatus::Running;
        if detached {
            record.report_to = ReportTo::Nobody;
        }
        record.team = Some(crate::store::run::TeamRun {
            launch_id: "leader".into(),
            instance: "forge#external".parse().unwrap(),
        });
        crate::harness::run::create(store.paths(), &record).unwrap();
        std::fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
        let stage = if done { "Done" } else { "Review" };
        std::fs::write(
            dir.path().join(".rimz/team/blackboard.md"),
            format!("Stage: {stage}\n"),
        )
        .unwrap();
        let agents = [agent.clone(), leader];
        let (verdict, read) = both(&store, dir.path(), &agents, &agent, DUE);
        let case = format!("done={done} detached={detached}");
        let team = [(RowKind::Team, "forge#external")];
        if holds {
            assert_eq!(verdict, Verdict::Hold(Hold::Owed(OwedWake::Team)), "{case}");
            assert_eq!(rows(&read.holding), team, "{case}");
            assert!(read.outliving.is_empty(), "{case}");
        } else {
            assert_eq!(verdict, Verdict::Stop { idle_secs: 180 }, "{case}");
            assert!(read.holding.is_empty(), "{case}");
            assert_eq!(rows(&read.outliving), team, "{case}");
        }
        let row = read.holding.iter().chain(&read.outliving).next().unwrap();
        assert_eq!(row.status, format!("stage {stage}"), "{case}");
        assert_eq!(
            row.command, None,
            "a cohort with no lane has no stop address: {case}"
        );
    }
}

#[test]
fn messages_and_runs_on_their_way_hold_as_their_own_rows() {
    // A prompt in flight, RimZ's own text, and a wake in flight.
    for (sender, expected) in [
        (MessageSender::Human, Hold::Owed(OwedWake::Message)),
        (MessageSender::System, Hold::Message),
        (
            MessageSender::Harness {
                notice: HarnessNotice::Wait,
            },
            Hold::Owed(OwedWake::WakeInFlight),
        ),
    ] {
        let (dir, store) = fixture();
        let (agent, _) = rested();
        register(&store, "session-1", None);
        let message = MessageRecord::new(
            store.paths().workspace_id.clone(),
            &agent,
            "rebase first".to_owned(),
            DeliveryGate::Done,
        )
        .with_sender(sender);
        store
            .queue_message(&message, "idle-stop-test")
            .expect("queue");
        let (verdict, read) = both(
            &store,
            dir.path(),
            std::slice::from_ref(&agent),
            &agent,
            DUE,
        );
        assert_eq!(verdict, Verdict::Hold(expected));
        // RimZ's own text stays behind the count `agents show` hides it under.
        // Listing them ends nothing, so their row offers no command.
        let (name, command) = if message.sender.is_conversation() {
            (
                message.message_id.to_string(),
                Some(format!("rimz message cancel {}", message.message_id)),
            )
        } else {
            ("1 system message".to_owned(), None)
        };
        assert_eq!(rows(&read.holding), [(RowKind::Message, name.as_str())]);
        assert_eq!(read.holding[0].command, command);
    }

    // A sent prompt still holds, but `rimz message cancel` refuses it.
    let (dir, store) = fixture();
    let (agent, _) = rested();
    register(&store, "session-1", None);
    let mut sent = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &agent,
        "rebase first".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Human);
    sent.status = MessageStatus::Sent;
    store.queue_message(&sent, "idle-stop-test").expect("queue");
    let (verdict, read) = both(
        &store,
        dir.path(),
        std::slice::from_ref(&agent),
        &agent,
        DUE,
    );
    assert!(matches!(verdict, Verdict::Hold(_)), "{verdict:?}");
    assert_eq!(
        rows(&read.holding),
        [(RowKind::Message, sent.message_id.as_str())]
    );
    assert_eq!(read.holding[0].command, None);
    drop(dir);

    let (dir, store) = fixture();
    let (mut agent, _) = rested();
    agent.launch_id = Some("launch-1".into());
    let mut run = RunRecord::new(
        store.paths().workspace_id.clone(),
        agent.kind.clone(),
        PermissionMode::Auto,
        "work".to_owned(),
        dir.path().to_owned(),
    );
    run.agent_id = Some(agent.agent_id.clone());
    run.status = RunStatus::Running;
    crate::harness::run::create(store.paths(), &run).expect("run");
    let (verdict, read) = both(
        &store,
        dir.path(),
        std::slice::from_ref(&agent),
        &agent,
        DUE,
    );
    assert_eq!(verdict, Verdict::Hold(Hold::OpenRun));
    assert_eq!(rows(&read.holding), [(RowKind::Run, run.run_id.as_str())]);
}

#[test]
fn a_team_seat_lists_its_board_until_done() {
    let (dir, store) = fixture();
    let (mut agent, _) = rested();
    agent.team = Some("forge".to_owned());
    agent.worktree_path = Some(dir.path().to_string_lossy().into_owned());
    std::fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
    std::fs::write(
        dir.path().join(".rimz/team/blackboard.md"),
        "Stage: Review\n",
    )
    .unwrap();
    let (verdict, read) = both(
        &store,
        dir.path(),
        std::slice::from_ref(&agent),
        &agent,
        DUE,
    );
    assert_eq!(verdict, Verdict::Hold(Hold::Board));
    assert_eq!(rows(&read.holding), [(RowKind::Board, "forge#external")]);
    assert_eq!(read.holding[0].status, "stage Review");
    std::fs::write(dir.path().join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
    let (verdict, read) = both(
        &store,
        dir.path(),
        std::slice::from_ref(&agent),
        &agent,
        DUE,
    );
    assert_eq!(verdict, Verdict::Stop { idle_secs: 180 });
    assert!(read.is_empty());
}

fn peer(id: &str, launcher: &AgentState) -> AgentState {
    let (mut peer, _) = rested();
    peer.agent_id = id.into();
    peer.name = Some(id.to_owned());
    peer.launched_by = Some(LaunchedBy {
        kind: launcher.kind.clone(),
        agent_id: launcher.agent_id.clone(),
    });
    peer
}

#[test]
fn peers_down_the_chain_and_booked_launches_outlive_the_agent() {
    let (dir, store) = fixture();
    let (agent, _) = rested();
    let first = peer("first", &agent);
    let second = peer("second", &first);
    let peers_child = kept_child(&first, "peers-child");
    let mut ended = peer("ended", &agent);
    ended.ended_at = Some(ts(1_100));
    let unrelated = peer("unrelated", &peers_child);
    // A launch a booked task fired: its chain names a booker that has ended, then the agent.
    let (mut fired, _) = rested();
    fired.agent_id = "fired".into();
    fired.name = Some("fired".to_owned());
    fired.loop_booked_by = [&ended, &agent]
        .map(|link| LaunchedBy {
            kind: link.kind.clone(),
            agent_id: link.agent_id.clone(),
        })
        .to_vec();
    let mut fired_seat = fired.clone();
    fired_seat.agent_id = "seat".into();
    fired_seat.name = Some("seat".to_owned());
    fired_seat.team = Some("forge".to_owned());
    let agents = [
        agent.clone(),
        first,
        second,
        peers_child,
        ended,
        unrelated,
        fired,
        fired_seat,
    ];
    let (verdict, read) = both(&store, dir.path(), &agents, &agent, DUE);
    assert_eq!(verdict, Verdict::Stop { idle_secs: 180 });
    assert!(read.holding.is_empty() && read.closing.is_empty());
    assert_eq!(
        rows(&read.outliving),
        [
            (RowKind::Agent, "@first"),
            (RowKind::Agent, "@first › @second"),
            (RowKind::Agent, "@ended › @fired"),
            (RowKind::Team, "@ended › forge#external"),
        ],
        "a peer's own subagents close with that peer, and ended rows never print"
    );
    assert_eq!(
        read.outliving[1].command.as_deref(),
        Some("rimz agents stop @second")
    );
}

#[test]
fn an_owing_peer_holds_and_is_not_listed_again_as_outliving() {
    let (dir, store) = fixture();
    let (agent, _) = rested();
    register(&store, "session-1", None);
    let mut observation =
        AgentLifecycleObservation::new(Some("first".into()), LifecycleSignal::Registered);
    observation.agent_name = Some("first".into());
    observation.launch.launched_by = Some(Box::new(LaunchedBy {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
    }));
    store
        .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
            session_name: "idle-stop-test",
            agent_kind: agent.kind.clone(),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .expect("register peer");
    let mut run = RunRecord::new(
        store.paths().workspace_id.clone(),
        agent.kind.clone(),
        PermissionMode::Auto,
        "work".to_owned(),
        dir.path().to_owned(),
    );
    run.agent_id = Some("first".into());
    run.status = RunStatus::Running;
    crate::harness::run::create(store.paths(), &run).expect("run");
    let agents = [agent.clone(), peer("first", &agent)];
    let (verdict, read) = both(&store, dir.path(), &agents, &agent, DUE);
    assert_eq!(verdict, Verdict::Hold(Hold::Owed(OwedWake::Subagents)));
    assert_eq!(rows(&read.holding), [(RowKind::Agent, "@first")]);
    assert!(read.outliving.is_empty(), "one row per thing");
}

/// `decide` holds on a wait armed in the catalog, which the snapshot it reads does not carry,
/// so the read lists it from the catalog as `decide` sees it.
#[test]
fn an_armed_catalog_wait_holds_and_lists_its_wait_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = crate::workspace::WorkspaceResolver::resolve(dir.path(), None).expect("room");
    let state = StatePaths::for_project_root(dir.path()).expect("state");
    let runtime = RuntimePaths::for_state(&state).expect("runtime");
    let store = Store::open(state, runtime).expect("store");
    store.record_workspace(&workspace).expect("record");
    let (agent, _) = rested();
    register(&store, "session-1", None);
    crate::harness::schedule::instances::seed(
        store.paths(),
        "gate",
        &crate::config::TaskEntry {
            watch: Some(crate::config::WatchSpec::Command(
                "cargo xtask gate".to_owned(),
            )),
            root: dir.path().to_owned(),
            wait: Some(crate::config::TaskTarget {
                kind: agent.kind.clone(),
                session: agent.agent_id.clone(),
                handle: "@coder".into(),
            }),
            ..crate::config::TaskEntry::default()
        },
    );
    let (verdict, read) = both(
        &store,
        dir.path(),
        std::slice::from_ref(&agent),
        &agent,
        DUE,
    );
    assert_eq!(verdict, Verdict::Hold(Hold::Owed(OwedWake::Wait)));
    assert_eq!(rows(&read.holding), [(RowKind::Wait, "gate")]);
}

/// Register `id` as a peer `launcher` launched, in the log the audit rows fold.
fn register_peer(store: &Store, id: &str, kind: &str, launcher: &AgentState) {
    let mut observation =
        AgentLifecycleObservation::new(Some(id.into()), LifecycleSignal::Registered);
    observation.agent_name = Some(id.into());
    observation.launch.launched_by = Some(Box::new(LaunchedBy {
        kind: launcher.kind.clone(),
        agent_id: launcher.agent_id.clone(),
    }));
    store
        .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
            session_name: "idle-stop-test",
            agent_kind: AgentKind::new_unchecked(kind),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .expect("register peer");
}

fn seat(id: &str, team: &str, launcher: &AgentState) -> AgentState {
    let mut seat = peer(id, launcher);
    seat.team = Some(team.to_owned());
    seat
}

#[test]
fn the_outliving_walk_goes_through_ended_peers_and_team_seats() {
    let (dir, store) = fixture();
    let (agent, _) = rested();
    register(&store, "session-1", None);
    // `first` ended and left the snapshot; only the log still holds it.
    register_peer(&store, "first", "claude", &agent);
    observe(&store, "first", None, LifecycleSignal::Ended);
    let (mut first, _) = rested();
    first.agent_id = "first".into();
    let mut second = peer("second", &first);
    second.status = AgentStatus::Running;
    second.phase = TurnPhase::Reasoning;
    let review = seat("reviewer", "review", &second);
    let leader = seat("leader", "forge", &agent);
    let third = peer("third", &leader);
    let agents = [agent.clone(), second, review, leader, third];
    let (verdict, read) = both(&store, dir.path(), &agents, &agent, DUE);
    assert!(
        read.holding.is_empty() && read.closing.is_empty(),
        "{verdict:?}"
    );
    let mut listed = rows(&read.outliving);
    listed.sort_by_key(|(_, name)| *name);
    let mut expected = vec![
        (RowKind::Agent, "@first › @second"),
        (RowKind::Team, "forge#external"),
        (RowKind::Agent, "@leader › @third"),
        (RowKind::Team, "@first › @second › review#external"),
    ];
    expected.sort_by_key(|(_, name)| *name);
    assert_eq!(listed, expected);
    let second = read
        .outliving
        .iter()
        .find(|row| row.name.ends_with("@second"))
        .unwrap();
    assert_eq!(
        (second.tone, second.phase),
        (AgentStatus::Running, TurnPhase::Reasoning),
        "the state role reads the row's phase too"
    );
}

#[test]
fn a_peer_molted_across_kinds_lists_once_as_its_live_holder() {
    let (dir, store) = fixture();
    let (agent, _) = rested();
    register(&store, "session-1", None);
    register_peer(&store, "first", "claude", &agent);
    let mut run = RunRecord::new(
        store.paths().workspace_id.clone(),
        agent.kind.clone(),
        PermissionMode::Auto,
        "work".to_owned(),
        dir.path().to_owned(),
    );
    run.agent_id = Some("first".into());
    run.status = RunStatus::Running;
    crate::harness::run::create(store.paths(), &run).expect("run");
    // The molt keeps the launch id and changes the kind; the snapshot holds the new row.
    let mut molted = peer("first-codex", &agent);
    molted.kind = AgentKind::new_unchecked("codex");
    molted.name = Some("first".to_owned());
    molted.launch_id = Some("first".into());
    let agents = [agent.clone(), molted];
    let (verdict, read) = both(&store, dir.path(), &agents, &agent, DUE);
    assert_eq!(verdict, Verdict::Hold(Hold::Owed(OwedWake::Subagents)));
    // The molted row is the room's one codex agent, so its paste address is the bare kind.
    assert_eq!(rows(&read.holding), [(RowKind::Agent, "@codex")]);
    assert_eq!(
        (
            read.holding[0].provider.as_ref(),
            read.holding[0].status.as_str()
        ),
        (Some(&AgentKind::new_unchecked("codex")), "idle"),
        "the live holder, not the earlier row"
    );
    assert!(read.outliving.is_empty(), "one launch prints once");
}

#[test]
fn an_ended_agent_lists_only_what_outlives_it() {
    let (dir, store) = fixture();
    let (mut agent, _) = rested();
    agent.ended_at = Some(ts(1_100));
    let message = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &agent,
        "rebase first".to_owned(),
        DeliveryGate::Done,
    );
    store
        .queue_message(&message, "idle-stop-test")
        .expect("queue");
    let mut child = kept_child(&agent, "child");
    child.status = AgentStatus::Running;
    let agents = [agent.clone(), child, peer("first", &agent)];
    let read = read(&store, dir.path(), &agents, &agent, false);
    assert!(
        read.holding.is_empty() && read.closing.is_empty(),
        "{read:?}"
    );
    assert_eq!(rows(&read.outliving), [(RowKind::Agent, "@first")]);
}

/// A recorded room at `dir`, whose loop tasks and workflow runs the read finds.
fn room(dir: &std::path::Path) -> Store {
    let workspace = crate::workspace::WorkspaceResolver::resolve(dir, None).expect("room");
    let state = StatePaths::for_project_root(dir).expect("state");
    let runtime = RuntimePaths::for_state(&state).expect("runtime");
    let store = Store::open(state, runtime).expect("store");
    store.record_workspace(&workspace).expect("record");
    store
}

fn booked_task(store: &Store, root: &std::path::Path, name: &str, agent: &AgentState) {
    crate::harness::schedule::instances::seed(
        store.paths(),
        name,
        &crate::config::TaskEntry {
            every: Some("1h".to_owned()),
            args: vec!["cargo".to_owned(), "xtask".to_owned(), "gate".to_owned()],
            root: root.to_owned(),
            booked_by: vec![LaunchedBy {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
            }],
            ..crate::config::TaskEntry::default()
        },
    );
}

/// A live workflow run a loop fire of `task`, booked by `agent`, started.
fn fired_run(
    store: &Store,
    root: &std::path::Path,
    task: &str,
    agent: &AgentState,
) -> crate::store::workflow::RunRecord {
    let run: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
        "version": crate::store::workflow::VERSION, "id": crate::ids::WorkflowRunId::new(),
        "workflow": "command", "task": task,
        "fire": {"kind": "scheduled", "task": task, "booked_by": [{
            "kind": agent.kind.as_str(), "agent_id": agent.agent_id.as_str(),
        }]},
        "scope": {"kind": "worktree", "path": root},
        "project_root": root, "created_at": ts(1_000), "entered_at": ts(1_000),
        "state": "active", "node": "command", "visits": {},
        "deadline_fired": false, "next_seq": 1,
    }))
    .expect("run");
    let guard = crate::disk::lock::WorkspaceLock::acquire(&store.paths().workspace_lock).unwrap();
    crate::store::workflow::initialize(store.paths(), &run, &guard).expect("initialize");
    run
}

#[test]
fn a_booked_task_fires_from_its_last_fire_and_carries_its_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = room(dir.path());
    let (agent, _) = rested();
    booked_task(&store, dir.path(), "nightly", &agent);
    // The task fired 50 minutes ago, so its hourly trigger is due in ten.
    let stamps = store.runtime_paths().lane_path("loop-fire.json");
    std::fs::create_dir_all(stamps.parent().unwrap()).unwrap();
    std::fs::write(
        &stamps,
        serde_json::json!({"nightly": ts(DUE - 3_000)}).to_string(),
    )
    .unwrap();
    let recurring = fired_run(&store, dir.path(), "nightly", &agent);
    // A one-shot task's row is consumed at its fire; only its run is left.
    let one_shot = fired_run(&store, dir.path(), "gate", &agent);
    let read = read(
        &store,
        dir.path(),
        std::slice::from_ref(&agent),
        &agent,
        true,
    );
    assert_eq!(
        rows(&read.outliving),
        [
            (RowKind::Booked, "nightly"),
            (RowKind::Workflow, one_shot.id.as_str())
        ]
    );
    let task = &read.outliving[0];
    assert_eq!(
        (task.status.as_str(), task.due),
        ("fires", Some(ts(DUE + 600)))
    );
    assert!(
        task.detail
            .iter()
            .any(|fact| fact.contains(recurring.id.as_str())),
        "{:?}",
        task.detail
    );
    assert_eq!(
        read.outliving[1].command,
        Some(format!("rimz workflow cancel {}", one_shot.id))
    );
}

/// A run started by `launch`, in `state` with its report `report`, and fired by a task the
/// agent booked when `booked`.
fn started_run(
    store: &Store,
    agent: &AgentState,
    launch: &str,
    (state, report, booked): (&str, &str, bool),
) -> crate::ids::WorkflowRunId {
    let id = crate::ids::WorkflowRunId::new();
    let paths = store.paths().workflow(&id);
    std::fs::create_dir_all(&paths.home).unwrap();
    let mut record = serde_json::json!({
        "version": crate::store::workflow::VERSION, "id": id, "workflow": "review",
        "spec_dir": "/definitions/review", "prompt": "Review",
        "scope": {"kind": "worktree", "path": "/project"}, "project_root": "/project",
        "created_at": ts(900), "state": state, "node": "pr", "entered_at": ts(900),
        "visits": {}, "deadline_fired": false, "next_seq": 1,
        "starter": {"by": "agent", "kind": agent.kind.as_str(), "launch": launch},
        "depth": 1, "loop_report": report,
    });
    if booked {
        record["fire"] = serde_json::json!({"kind": "scheduled", "task": "gate", "booked_by": [{
            "kind": agent.kind.as_str(), "agent_id": agent.agent_id.as_str(),
        }]});
    }
    std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
    id
}

#[test]
fn a_run_the_agent_started_holds_until_its_report_is_sent_and_a_detached_one_outlives() {
    // The case, the run's starter, state, report and booking, whether `decide` holds on it,
    // the group that lists it, and whether that row offers `rimz workflow cancel`.
    for (case, launch, run, holds, group, cancel) in [
        (
            "going",
            "session-1",
            ("active", "owed", false),
            true,
            Some("holding"),
            true,
        ),
        (
            "reporting",
            "session-1",
            ("done", "owed", false),
            true,
            Some("holding"),
            false,
        ),
        (
            "booked too",
            "session-1",
            ("active", "owed", true),
            true,
            Some("holding"),
            true,
        ),
        (
            "detached",
            "session-1",
            ("active", "none", false),
            false,
            Some("outliving"),
            true,
        ),
        (
            "detached, booked",
            "session-1",
            ("active", "none", true),
            false,
            Some("outliving"),
            true,
        ),
        (
            "reported",
            "session-1",
            ("done", "sent", false),
            false,
            None,
            false,
        ),
        (
            "another's",
            "session-2",
            ("active", "owed", false),
            false,
            None,
            false,
        ),
    ] {
        let (dir, store) = fixture();
        let (agent, _) = rested();
        register(&store, "session-1", None);
        register(&store, "session-2", None);
        let id = started_run(&store, &agent, launch, run);
        let (verdict, read) = both(
            &store,
            dir.path(),
            std::slice::from_ref(&agent),
            &agent,
            DUE,
        );
        if holds {
            assert_eq!(
                verdict,
                Verdict::Hold(Hold::Owed(OwedWake::Workflow)),
                "{case}"
            );
        } else {
            assert_eq!(verdict, Verdict::Stop { idle_secs: 180 }, "{case}");
        }
        let listed = [(RowKind::Workflow, id.as_str())];
        let (holding, outliving): (&[_], &[_]) = match group {
            Some("holding") => (&listed, &[]),
            Some(_) => (&[], &listed),
            None => (&[], &[]),
        };
        assert_eq!(rows(&read.holding), holding, "{case}");
        assert_eq!(rows(&read.outliving), outliving, "{case}");
        let row = read.holding.iter().chain(&read.outliving).next();
        assert_eq!(
            row.and_then(|row| row.command.clone()),
            cancel.then(|| format!("rimz workflow cancel {id}")),
            "{case}"
        );
    }
}

#[test]
fn a_disabled_booked_task_reads_disabled() {
    if crate::harness::schedule::test_homes::reexec_with_private_homes(
        "harness::idle_stop::tests::started::a_disabled_booked_task_reads_disabled",
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let store = room(dir.path());
    let (agent, _) = rested();
    booked_task(&store, dir.path(), "nightly", &agent);
    let catalog = TaskCatalog::load_room_lenient(dir.path(), store.paths());
    let task = &catalog.visible()["nightly"];
    crate::harness::schedule::arming::disable(&task.binding("nightly"), None).expect("disable");
    let read = read(
        &store,
        dir.path(),
        std::slice::from_ref(&agent),
        &agent,
        true,
    );
    assert_eq!(rows(&read.outliving), [(RowKind::Booked, "nightly")]);
    assert_eq!(
        (read.outliving[0].status.as_str(), read.outliving[0].due),
        ("disabled", None)
    );
}

#[test]
fn pending_ingress_holds_without_a_row() {
    use crate::disk::lock::SharedAppendLock;
    use crate::store::ingress::{self, HookIngress};
    let (dir, store) = fixture();
    let (agent, _) = rested();
    let frame = HookIngress {
        soft_compact_claim: None,
        boot_id: None,
        schema_version: "1".into(),
        event_id: crate::ids::EventId::new(),
        ts: ts(501),
        source: agent.kind.clone(),
        event: Some("UserPromptSubmit".into()),
        payload: "{}".into(),
        cwd: dir.path().into(),
        hook_pid: std::process::id(),
        env: Default::default(),
    };
    let lock = SharedAppendLock::acquire(&store.paths().hook_ingress_lock).unwrap();
    ingress::append(store.paths(), &frame, &lock).unwrap();
    drop(lock);
    let (verdict, read) = both(
        &store,
        dir.path(),
        std::slice::from_ref(&agent),
        &agent,
        DUE,
    );
    assert_eq!(verdict, Verdict::Hold(Hold::Busy));
    assert!(read.is_empty(), "the agent's own turn is landing: {read:?}");
}

#[test]
fn an_owed_compaction_continue_holds_on_its_message_row() {
    let (dir, store) = fixture();
    let (agent, _) = rested();
    register(&store, "session-1", None);
    let mut message = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &agent,
        "continue".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::System);
    message.soft_compact = Some(crate::store::message::SoftCompactStep::Continue);
    store
        .queue_message(&message, "idle-stop-test")
        .expect("queue");
    let mut shown = store
        .snapshot()
        .expect("snapshot")
        .agents
        .iter()
        .find(|row| row.agent_id == agent.agent_id)
        .expect("row")
        .clone();
    shown.turn_ended_at = agent.turn_ended_at;
    shown.last_activity = agent.last_activity;
    let (verdict, read) = both(&store, dir.path(), &[shown.clone()], &shown, DUE);
    assert_eq!(verdict, Verdict::Hold(Hold::Owed(OwedWake::SoftCompact)));
    assert_eq!(
        rows(&read.holding),
        [(RowKind::Message, "1 system message")]
    );
}
