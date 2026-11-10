//! A wait that starts after a launched peer's run ended reads that run while
//! it still describes the peer's latest turn, and marks it read for its launcher.

use super::*;
use crate::common::wait::{register_agent, register_calling_agent};
use rimz::agents::LaunchedBy;
use rimz::ids::MessageId;
use rimz::store::message::{
    DeliveryGate, HarnessNotice, MessageRecord, MessageSender, MessageStatus,
};
use rimz::store::writer::AgentLifecycleIntent;

const LAUNCHER_LAUNCH: &str = "launch-session";
const LAUNCHER_SESSION: &str = "provider-session";

const fn turn_ended(parked_on_background: bool) -> LifecycleSignal {
    LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background,
        turn_id: None,
    }
}

const TURN_STARTED: LifecycleSignal = LifecycleSignal::TurnStarted { turn_id: None };

fn lifecycle(store: &rimz::Store, session: &str, signal: LifecycleSignal) {
    let observation = AgentLifecycleObservation::new(Some(AgentSessionId::from(session)), signal);
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .expect("append lifecycle");
}

/// A room whose launcher `@planner` holds an open turn.
fn launcher_room() -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    lifecycle(&env.store(), LAUNCHER_SESSION, TURN_STARTED);
    env
}

struct EndedPeer {
    session: String,
    run: RunRecord,
    digest: MessageId,
}

/// Launch `@<name>` from `@planner`, run one launcher-opened turn to its end
/// (parked on background work when `parked`), settle its run after the row's
/// close, and queue the digest that reports it.
fn ended_peer(env: &Env, name: &str, parked: bool) -> EndedPeer {
    let store = env.store();
    let session = format!("{name}-session");
    let launch = format!("{name}-launch");
    register_agent(
        env,
        &session,
        &launch,
        name,
        LaunchParams {
            launched_by: Some(Box::new(LaunchedBy {
                kind: AgentKind::new_unchecked("claude"),
                agent_id: AgentSessionId::from(LAUNCHER_LAUNCH),
            })),
            ..LaunchParams::default()
        },
    );
    lifecycle(&store, &session, TURN_STARTED);
    lifecycle(&store, &session, turn_ended(parked));
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        format!("task for {name}"),
        env.project_root.clone(),
    );
    run.agent_id = Some(AgentSessionId::from(session.as_str()));
    run.agent_name = Some(name.to_owned());
    run.reader = Some("planner".to_owned());
    run.peer = Some(rimz::store::run::PeerRun {
        launch_id: AgentSessionId::from(launch.as_str()),
        opened_by: Vec::new(),
    });
    run.last_message = Some(format!("{name} run answer"));
    run.cost_usd = Some(0.25);
    run.status = RunStatus::Completed;
    run.completed_at = Some(Timestamp::now());
    rimz::harness::run::create(store.paths(), &run).expect("create ended peer run");

    let snapshot = store.snapshot_cached().expect("snapshot");
    let launcher = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == LAUNCHER_SESSION)
        .expect("launcher row");
    let digest = MessageRecord::new(
        env.workspace_id.clone(),
        launcher,
        format!("@{name} completed: {name} run answer"),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::AgentReport,
    });
    store
        .queue_message(&digest, "test digest")
        .expect("queue digest");
    assert!(
        rimz::harness::run::report::record_report_messages(
            store.paths(),
            &[(run.run_id.clone(), 1)],
            Some(&digest.message_id),
        )
        .expect("link digest")
    );
    EndedPeer {
        session,
        run,
        digest: digest.message_id,
    }
}

fn wait_as_launcher(env: &Env, args: &[&str]) -> std::process::Output {
    env.rimz()
        .args(["agents", "wait"])
        .args(args)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, LAUNCHER_LAUNCH)
        .output()
        .expect("run agents wait")
}

fn joined(env: &Env, peer: &EndedPeer) -> bool {
    rimz::harness::run::load(env.store().paths(), &peer.run.run_id)
        .expect("reload peer run")
        .joined_at
        .is_some()
}

fn digest_status(env: &Env, digest: &MessageId) -> MessageStatus {
    let store = env.store();
    if let Some(queued) = store
        .list_messages()
        .expect("list messages")
        .into_iter()
        .find(|message| &message.message_id == digest)
    {
        return queued.status;
    }
    store
        .list_message_history()
        .expect("list history")
        .into_iter()
        .find(|message| &message.message_id == digest)
        .expect("digest in queue or history")
        .status
}

fn assert_read(env: &Env, peer: &EndedPeer) {
    assert!(
        joined(env, peer),
        "late wait leaves {} unjoined",
        peer.session
    );
    assert_eq!(digest_status(env, &peer.digest), MessageStatus::Canceled);
    let reasons = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "message.canceled")
        .map(|event| {
            serde_json::from_str::<rimz::store::event::MessageEventPayload>(event.params.get())
                .expect("message event payload")
        })
        .filter(|payload| payload.message_id == peer.digest)
        .map(|payload| payload.reason)
        .collect::<Vec<_>>();
    assert_eq!(reasons, [Some("joined inline".to_owned())]);
}

fn assert_unread(env: &Env, peer: &EndedPeer) {
    assert!(!joined(env, peer), "{} joined", peer.session);
    assert_eq!(digest_status(env, &peer.digest), MessageStatus::Queued);
}

fn peer_row(env: &Env, session: &str) -> rimz::agents::AgentState {
    env.store()
        .snapshot_cached()
        .expect("snapshot")
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == session)
        .expect("peer row")
        .clone()
}

fn record_turn_message(env: &Env, session: &str, text: &str) {
    rimz::transcript::append(
        env.store().paths(),
        &rimz::transcript::TranscriptEntry::new(
            Timestamp::now(),
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from(session),
            rimz::transcript::TranscriptKind::Assistant,
            text.to_owned(),
        ),
    )
    .expect("record the turn's final message");
}

#[test]
fn late_wait_by_the_launcher_prints_and_marks_the_ended_peer_run() {
    let env = launcher_room();
    let peer = ended_peer(&env, "alpha", false);
    let out = wait_as_launcher(&env, &["@alpha", "--timeout", "10s"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "alpha run answer\n");
    assert_read(&env, &peer);
}

#[test]
fn late_json_wait_by_the_launcher_reports_the_run_record() {
    let env = launcher_room();
    let peer = ended_peer(&env, "alpha", false);
    let out = wait_as_launcher(&env, &["@alpha", "--json", "--timeout", "10s"]);
    assert!(out.status.success(), "{out:?}");
    let entries: serde_json::Value = serde_json::from_slice(&out.stdout).expect("wait JSON");
    assert_eq!(
        entries,
        json!({"@alpha": {
            "name": "alpha",
            "status": "completed",
            "exit": 0,
            "cost": 0.25,
            "last_message": "alpha run answer",
        }})
    );
    assert_read(&env, &peer);
}

#[test]
fn late_wait_on_two_ended_peers_marks_both_runs() {
    let env = launcher_room();
    let alpha = ended_peer(&env, "alpha", false);
    let beta = ended_peer(&env, "beta", false);
    let out = wait_as_launcher(&env, &["@alpha", "@beta", "--timeout", "10s"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("alpha run answer"), "{stdout}");
    assert!(stdout.contains("beta run answer"), "{stdout}");
    assert_read(&env, &alpha);
    assert_read(&env, &beta);
}

#[test]
fn late_wait_reads_the_turn_a_peer_closed_after_its_run() {
    for parked in [false, true] {
        let env = launcher_room();
        let peer = ended_peer(&env, "alpha", parked);
        let store = env.store();
        // A prompt into a row parked on background work leaves `turn_started_at`
        // at the run's turn; only the later close tells the turns apart.
        lifecycle(&store, &peer.session, TURN_STARTED);
        lifecycle(&store, &peer.session, turn_ended(false));
        // Date the run before the later close from the row's own stamp, so the
        // order does not rest on two wall-clock reads.
        let later_close = peer_row(&env, &peer.session)
            .turn_ended_at
            .expect("later close stamp");
        let mut run = rimz::harness::run::load(store.paths(), &peer.run.run_id).expect("reload");
        run.completed_at = Some(later_close - Duration::from_secs(1));
        rimz::harness::run::create(store.paths(), &run).expect("date run before the close");
        record_turn_message(&env, &peer.session, "later turn answer");
        let out = wait_as_launcher(&env, &["@alpha", "--timeout", "10s"]);
        assert!(out.status.success(), "parked {parked}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "later turn answer\n",
            "parked {parked}"
        );
        assert_unread(&env, &peer);
    }
}

#[test]
fn late_wait_blocks_on_an_open_turn_no_run_describes() {
    let env = launcher_room();
    let peer = ended_peer(&env, "alpha", false);
    lifecycle(&env.store(), &peer.session, TURN_STARTED);
    let out = wait_as_launcher(&env, &["@alpha", "--timeout", "1s"]);
    assert_eq!(
        out.status.code(),
        Some(RunStatus::TimedOut.exit_code()),
        "{out:?}"
    );
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_unread(&env, &peer);
}

/// A restart fails the peer's unsettled run (`fail_peer_run`) and the resumed
/// session registers again; the row keeps its last close, so a late wait reads
/// the failed run whether the restart came at rest or mid-turn.
#[test]
fn late_wait_reads_the_run_a_restart_failed() {
    for mid_turn in [false, true] {
        let env = launcher_room();
        let peer = ended_peer(&env, "alpha", false);
        let store = env.store();
        let mut run = rimz::harness::run::load(store.paths(), &peer.run.run_id).expect("reload");
        run.status = RunStatus::Running;
        run.completed_at = None;
        rimz::harness::run::create(store.paths(), &run).expect("reopen peer run");
        if mid_turn {
            lifecycle(&store, &peer.session, TURN_STARTED);
        }
        let row = peer_row(&env, &peer.session);
        let failed = rimz::harness::run::fail_peer_run(&store, &row, "peer restarted")
            .expect("fail peer run")
            .expect("an open peer run");
        lifecycle(&store, &peer.session, LifecycleSignal::Registered);
        assert_eq!(
            peer_row(&env, &peer.session).turn_ended_at,
            row.turn_ended_at,
            "mid_turn {mid_turn}: re-registration keeps the last close"
        );
        let out = wait_as_launcher(&env, &["@alpha", "--timeout", "2s"]);
        assert_eq!(
            out.status.code(),
            Some(RunStatus::Failed.exit_code()),
            "mid_turn {mid_turn}: {out:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "alpha run answer\n",
            "mid_turn {mid_turn}"
        );
        let stored = rimz::harness::run::load(store.paths(), &failed.run_id).expect("reload");
        assert_eq!(stored.status, RunStatus::Failed, "mid_turn {mid_turn}");
        assert_eq!(stored.failure_tail.as_deref(), Some("peer restarted"));
        assert_read(&env, &peer);
    }
}

#[test]
fn late_stream_on_an_ended_peer_ends_with_the_run_record() {
    let env = launcher_room();
    let mut peer = ended_peer(&env, "alpha", false);
    peer.run.status = RunStatus::Failed;
    rimz::harness::run::create(env.store().paths(), &peer.run).expect("fail peer run");
    let out = wait_as_launcher(&env, &["@alpha", "--stream", "--timeout", "10s"]);
    assert_eq!(
        out.status.code(),
        Some(RunStatus::Failed.exit_code()),
        "{out:?}"
    );
    assert_unread(&env, &peer);
}
