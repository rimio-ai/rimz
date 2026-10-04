//! Soft stop: end an agent once it has rested for a requested duration with
//! nothing owed (`rimz agents stop --when-idle`).
//!
//! The elected producer reads the durable requests and spawns the detached
//! `rimz agents idle-stop` helper for each one whose cheap terms are due. The
//! helper re-runs the whole decision against fresh reads and owns the stop;
//! this module writes only a disposable spawn-pacing record.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::owed::OwedWake;
use crate::agents::state::IdleStop;
use crate::agents::{AgentState, AgentStatus, TurnPhase};
use crate::disk::atomic::write_temp_then_rename_cache;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::StoreErr;
use crate::store::snapshot::{SidebarSnapshot, find_agent};
use crate::{RuntimePaths, StatePaths, Store};

/// A declined helper leaves the request armed; this bounds how often the
/// producer asks again while a term only the helper reads still holds.
const IDLE_STOP_RESPAWN_THROTTLE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleStopHelperRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FireRecord {
    fired_at: Timestamp,
}

/// Why a requested stop waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    Ended,
    ProviderSubagent,
    AwaitingInput,
    BudgetPark,
    Compacting,
    /// Mid-turn, failed or paused, sleeping on a wait, parked on background
    /// work, or running a background shell.
    Busy,
    /// Rested, but for less than the requested duration.
    Clock,
    Owed(OwedWake),
    /// A message for this card has not reached a terminal status.
    Message,
    /// A run bound to the session is not terminal; stopping would cancel it.
    OpenRun,
    /// A team seat whose board does not read `Done` is owed a stage.
    Board,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Stop { idle_secs: u64 },
    Hold(Hold),
}

/// The terms the rollup alone answers: the producer's whole view of an agent.
fn resting(agent: &AgentState) -> Result<(), Hold> {
    if agent.ended_at.is_some() {
        return Err(Hold::Ended);
    }
    if agent.is_provider_subagent() {
        return Err(Hold::ProviderSubagent);
    }
    if agent.is_awaiting_input() {
        return Err(Hold::AwaitingInput);
    }
    if agent.budget_park.is_some() {
        return Err(Hold::BudgetPark);
    }
    if agent.compacting_since.is_some() {
        return Err(Hold::Compacting);
    }
    if !matches!(
        agent.effective_status(),
        AgentStatus::Idle | AgentStatus::Success
    ) || agent.phase == TurnPhase::Parked
        || !agent.background_shells.is_empty()
    {
        return Err(Hold::Busy);
    }
    Ok(())
}

/// A turn that ends after the request restarts the clock; a request against
/// an agent already resting waits its whole duration.
fn rested_since(agent: &AgentState, stop: &IdleStop) -> Timestamp {
    agent
        .turn_ended_at
        .map_or(stop.requested_at, |ended| ended.max(stop.requested_at))
}

/// When the stop falls due while the agent keeps resting, or `None` while a
/// term the rollup shows is holding the clock.
pub fn due_at(agent: &AgentState, stop: &IdleStop) -> Option<Timestamp> {
    resting(agent).ok()?;
    let after = i64::try_from(stop.after_secs).unwrap_or(i64::MAX);
    Timestamp::from_second(rested_since(agent, stop).as_second().saturating_add(after)).ok()
}

/// The whole decision, read fresh: the helper calls it immediately before it
/// stops the agent. `agent` comes from an enriched snapshot.
pub fn decide(
    store: &Store,
    agent: &AgentState,
    stop: &IdleStop,
    now: Timestamp,
) -> Result<Verdict, StoreErr> {
    if let Err(hold) = resting(agent) {
        return Ok(Verdict::Hold(hold));
    }
    if due_at(agent, stop).is_none_or(|due| now < due) {
        return Ok(Verdict::Hold(Hold::Clock));
    }
    if let Some(owed) = super::owed::owed_wake(store, &agent.kind, &agent.agent_id)? {
        return Ok(Verdict::Hold(Hold::Owed(owed)));
    }
    if store
        .list_messages()?
        .iter()
        .any(|message| message.same_agent_card(agent) && !message.status.is_terminal())
    {
        return Ok(Verdict::Hold(Hold::Message));
    }
    if super::run::list(store.paths())?
        .iter()
        .any(|run| run.matches_agent(agent) && !run.status.is_terminal())
    {
        return Ok(Verdict::Hold(Hold::OpenRun));
    }
    if agent.is_team_seat() && !board_done(agent) {
        return Ok(Verdict::Hold(Hold::Board));
    }
    let idle = now.as_second() - rested_since(agent, stop).as_second();
    Ok(Verdict::Stop {
        idle_secs: u64::try_from(idle).unwrap_or(0),
    })
}

fn board_done(agent: &AgentState) -> bool {
    agent
        .worktree_path
        .as_deref()
        .and_then(|root| super::scratch::board_stage(Path::new(root)))
        .is_some_and(|stage| stage.name == crate::config::DONE_STAGE)
}

/// Attach each pending request to its session's rollup row, for the card and
/// `agents show`. Enrich-only: the durable record stays the truth.
pub(crate) fn project_requests(snapshot: &mut SidebarSnapshot, paths: &StatePaths) {
    let requests = crate::store::idle_stop::read(paths);
    for agent in &mut snapshot.agents {
        agent.idle_stop = requests
            .iter()
            .find(|request| {
                agent.ended_at.is_none()
                    && request.kind == agent.kind
                    && request.agent_id == agent.agent_id
            })
            .map(|request| request.stop.clone());
    }
}

/// Ask a helper to stop each agent whose request is due on the rollup's terms.
pub(crate) fn stop_idle_agents(
    snapshot: &SidebarSnapshot,
    paths: &StatePaths,
    runtime: &RuntimePaths,
) {
    stop_idle_agents_with(snapshot, paths, runtime, |request| {
        spawn_idle_stop(runtime, request)
    });
}

fn stop_idle_agents_with(
    snapshot: &SidebarSnapshot,
    paths: &StatePaths,
    runtime: &RuntimePaths,
    mut spawn: impl FnMut(&IdleStopHelperRequest) -> bool,
) {
    for request in crate::store::idle_stop::read(paths) {
        let Some(agent) = find_agent(&snapshot.agents, &request.kind, &request.agent_id) else {
            continue;
        };
        if due_at(agent, &request.stop).is_none_or(|due| snapshot.now < due) {
            continue;
        }
        let record_path = fire_record_path(runtime, &agent.kind, &agent.agent_id);
        if read_fire_record(&record_path).is_some_and(|record| {
            snapshot.now.as_second() - record.fired_at.as_second()
                < IDLE_STOP_RESPAWN_THROTTLE.as_secs() as i64
        }) {
            continue;
        }
        let Some(pane_id) = snapshot.live_agent_pane(&agent.kind, &agent.agent_id) else {
            continue;
        };
        let peers = crate::address::addressable_agents(snapshot);
        let helper = IdleStopHelperRequest {
            workspace_id: runtime.workspace_id.clone(),
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id,
            label: crate::address::agent_handle(agent, &peers, false),
        };
        if spawn(&helper)
            && let Err(err) = write_temp_then_rename_cache(
                &record_path,
                &FireRecord {
                    fired_at: snapshot.now,
                },
            )
        {
            tracing::warn!(
                tags.operation = "idle_stop.write_fire_record",
                error = &err as &dyn std::error::Error,
                "sidebar: failed to record idle-stop pacing",
            );
        }
    }
}

fn fire_record_path(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> PathBuf {
    runtime.live_path("idle-stop").join(format!(
        "{}.json",
        crate::store::sidecar::digest(kind.as_str(), agent_id.as_str())
    ))
}

fn read_fire_record(path: &Path) -> Option<FireRecord> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn spawn_idle_stop(runtime: &RuntimePaths, request: &IdleStopHelperRequest) -> bool {
    let args = crate::child_process::agent_helper_argv("idle-stop", request);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = %request.kind,
        "sidebar: stopping idle agent",
    );
    if let Err(err) = crate::child_process::spawn_detached_rimz(runtime, args, "agent-idle-stop") {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "idle_stop.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn agent idle stop",
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{PendingWait, PendingWaitTrigger, PermissionMode};
    use crate::ids::MuxName;
    use crate::store::idle_stop::IdleStopRequest;
    use crate::store::message::{DeliveryGate, MessageRecord, MessageStatus};
    use crate::store::run::{PeerRun, RunRecord, RunStatus};
    use crate::store::snapshot::PaneAgent;

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).expect("timestamp")
    }

    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = WorkspaceId::from_project_root(dir.path());
        let state = StatePaths::under(id.clone(), &dir.path().join("state")).expect("state");
        let runtime = RuntimePaths::under(id, &dir.path().join("runtime")).expect("runtime");
        (dir, Store::open(state, runtime).expect("store"))
    }

    /// Rested since 1000 with a three-minute stop requested at 500.
    fn rested() -> (AgentState, IdleStop) {
        let mut agent = AgentState::seed(
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("session-1"),
            AgentStatus::Idle,
            ts(1_000),
        );
        agent.name = Some("coder".to_owned());
        agent.turn_ended_at = Some(ts(1_000));
        let stop = IdleStop {
            after_secs: 180,
            requested_at: ts(500),
            requested_by: None,
        };
        (agent, stop)
    }

    const DUE: i64 = 1_180;

    fn verdict(store: &Store, agent: &AgentState, stop: &IdleStop, now: i64) -> Verdict {
        decide(store, agent, stop, ts(now)).expect("decide")
    }

    #[test]
    fn each_rollup_term_holds_the_stop_alone_and_a_clear_agent_stops() {
        let (_dir, store) = fixture();
        let (clear, stop) = rested();
        assert_eq!(
            verdict(&store, &clear, &stop, DUE),
            Verdict::Stop { idle_secs: 180 }
        );
        assert_eq!(due_at(&clear, &stop), Some(ts(DUE)));

        let hold = |name: &str, expected: Hold, change: &dyn Fn(&mut AgentState)| {
            let mut agent = clear.clone();
            change(&mut agent);
            assert_eq!(
                verdict(&store, &agent, &stop, DUE),
                Verdict::Hold(expected),
                "{name}"
            );
            assert_eq!(due_at(&agent, &stop), None, "{name}");
        };
        hold("ended", Hold::Ended, &|agent| {
            agent.ended_at = Some(ts(1_100))
        });
        hold("provider subagent", Hold::ProviderSubagent, &|agent| {
            agent.parent_agent_id = Some("parent".into());
        });
        hold("awaiting input", Hold::AwaitingInput, &|agent| {
            agent.status = AgentStatus::Waiting;
            agent.waiting_since = Some(agent.last_activity);
        });
        hold("budget park", Hold::BudgetPark, &|agent| {
            agent.budget_park = Some(crate::agents::BudgetPark {
                cap_usd: 1.0,
                spend_usd: 1.0,
                window: crate::agents::BudgetWindow::Session,
                at: ts(1_000),
                scope: crate::agents::BudgetScope::Agent,
                account_kind: None,
                resets_at: None,
            });
        });
        hold("compacting", Hold::Compacting, &|agent| {
            agent.compacting_since = Some(ts(1_100));
        });
        for status in [
            AgentStatus::Running,
            AgentStatus::Failed,
            AgentStatus::Paused,
        ] {
            hold(status.as_str(), Hold::Busy, &|agent| agent.status = status);
        }
        hold("parked on background work", Hold::Busy, &|agent| {
            agent.status = AgentStatus::Running;
            agent.phase = TurnPhase::Parked;
        });
        hold("background shell", Hold::Busy, &|agent| {
            agent
                .background_shells
                .push(crate::agents::BackgroundShell {
                    id: "shell-1".to_owned(),
                    command: None,
                    description: None,
                    started_at: ts(900),
                });
        });
        hold("sleeping on a wait", Hold::Busy, &|agent| {
            agent.pending_waits.push(PendingWait {
                name: "wait-command".to_owned(),
                trigger: PendingWaitTrigger::Command {
                    command: "cargo xtask gate".to_owned(),
                },
                armed_at: Some(ts(900)),
            });
        });
    }

    #[test]
    fn the_clock_runs_from_the_later_of_turn_end_and_request() {
        let (_dir, store) = fixture();
        let (mut agent, mut stop) = rested();
        assert_eq!(
            verdict(&store, &agent, &stop, DUE - 1),
            Verdict::Hold(Hold::Clock)
        );

        // A new turn before the deadline restarts the clock at its end.
        agent.turn_ended_at = Some(ts(1_100));
        assert_eq!(
            verdict(&store, &agent, &stop, DUE),
            Verdict::Hold(Hold::Clock)
        );
        assert_eq!(due_at(&agent, &stop), Some(ts(1_280)));
        assert_eq!(
            verdict(&store, &agent, &stop, 1_280),
            Verdict::Stop { idle_secs: 180 }
        );

        // A request against a long-idle agent waits the full duration from the request.
        stop.requested_at = ts(5_000);
        assert_eq!(
            verdict(&store, &agent, &stop, 5_179),
            Verdict::Hold(Hold::Clock)
        );
        assert_eq!(
            verdict(&store, &agent, &stop, 5_200),
            Verdict::Stop { idle_secs: 200 }
        );

        // An agent that never closed a turn rests from the request.
        agent.turn_ended_at = None;
        assert_eq!(due_at(&agent, &stop), Some(ts(5_180)));

        // Zero fires at the first evaluation.
        stop.after_secs = 0;
        assert_eq!(
            verdict(&store, &agent, &stop, 5_000),
            Verdict::Stop { idle_secs: 0 }
        );
    }

    #[test]
    fn an_undelivered_message_holds_until_it_is_terminal() {
        for status in [
            MessageStatus::Queued,
            MessageStatus::Sent,
            MessageStatus::Delivered,
        ] {
            let (_dir, store) = fixture();
            let (agent, stop) = rested();
            let mut message = MessageRecord::new(
                store.paths().workspace_id.clone(),
                &agent,
                "rebase first".to_owned(),
                DeliveryGate::Done,
            );
            // A scheduled message is owed as much as a ready one.
            message.not_before = Some(ts(9_000));
            message.status = status;
            store
                .queue_message(&message, "idle-stop-test")
                .expect("queue");
            let expected = if status.is_terminal() {
                Verdict::Stop { idle_secs: 180 }
            } else {
                Verdict::Hold(Hold::Message)
            };
            assert_eq!(verdict(&store, &agent, &stop, DUE), expected, "{status:?}");

            let (mut other, _) = rested();
            other.agent_id = "session-2".into();
            other.name = Some("other".to_owned());
            assert_eq!(
                verdict(&store, &other, &stop, DUE),
                Verdict::Stop { idle_secs: 180 },
                "another card's message holds nothing"
            );
        }
    }

    #[test]
    fn an_open_run_holds_so_the_stop_never_cancels() {
        for peer in [false, true] {
            for status in [RunStatus::Running, RunStatus::Completed] {
                let (dir, store) = fixture();
                let (mut agent, stop) = rested();
                agent.launch_id = Some("launch-1".into());
                let mut run = RunRecord::new(
                    store.paths().workspace_id.clone(),
                    agent.kind.clone(),
                    PermissionMode::Auto,
                    "work".to_owned(),
                    dir.path().to_owned(),
                );
                run.agent_id = Some(agent.agent_id.clone());
                run.peer = peer.then(|| PeerRun {
                    launch_id: "launch-1".into(),
                    opened_by: Vec::new(),
                });
                run.status = status;
                super::super::run::create(store.paths(), &run).expect("run");
                let expected = if status.is_terminal() {
                    Verdict::Stop { idle_secs: 180 }
                } else {
                    Verdict::Hold(Hold::OpenRun)
                };
                assert_eq!(
                    verdict(&store, &agent, &stop, DUE),
                    expected,
                    "peer={peer} {status:?}"
                );
            }
        }
    }

    #[test]
    fn a_team_seat_waits_for_its_board_to_read_done() {
        let (dir, store) = fixture();
        let (mut agent, stop) = rested();
        agent.team = Some("forge".to_owned());
        assert_eq!(
            verdict(&store, &agent, &stop, DUE),
            Verdict::Hold(Hold::Board),
            "a seat with no checkout has no board"
        );
        agent.worktree_path = Some(dir.path().to_string_lossy().into_owned());
        assert_eq!(
            verdict(&store, &agent, &stop, DUE),
            Verdict::Hold(Hold::Board),
            "no board file"
        );
        std::fs::write(dir.path().join("blackboard.md"), "Stage: Review (@judge)\n").unwrap();
        assert_eq!(
            verdict(&store, &agent, &stop, DUE),
            Verdict::Hold(Hold::Board)
        );
        std::fs::write(dir.path().join("blackboard.md"), "Stage: Done\n").unwrap();
        assert_eq!(
            verdict(&store, &agent, &stop, DUE),
            Verdict::Stop { idle_secs: 180 }
        );
    }

    #[test]
    fn requests_project_onto_their_sessions_and_stale_ones_clear() {
        let (_dir, store) = fixture();
        let (agent, stop) = rested();
        let (mut other, _) = rested();
        other.agent_id = "session-2".into();
        other.idle_stop = Some(stop.clone());
        crate::store::idle_stop::arm(
            store.paths(),
            IdleStopRequest {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
                stop: stop.clone(),
            },
        )
        .expect("arm");
        let mut snapshot = SidebarSnapshot::build_with_agents(
            store.paths().workspace_id.clone(),
            vec![agent, other],
            ts(DUE),
        );
        project_requests(&mut snapshot, store.paths());
        let pending = |id: &str| {
            find_agent(
                &snapshot.agents,
                &AgentKind::new_unchecked("claude"),
                &id.into(),
            )
            .expect("agent")
            .idle_stop
            .clone()
        };
        assert_eq!(pending("session-1"), Some(stop.clone()));
        assert_eq!(pending("session-2"), None);

        assert_eq!(stop.label(None, ts(DUE)), "after 3m idle");
        assert_eq!(
            stop.label(Some(ts(DUE)), ts(DUE - 61)),
            "after 3m idle, in 2m"
        );
        assert_eq!(stop.label(Some(ts(DUE)), ts(DUE)), "after 3m idle, due");
    }

    #[test]
    fn producer_spawns_for_a_due_request_paces_and_leaves_the_record_alone() {
        let (_dir, store) = fixture();
        let (paths, runtime) = (store.paths(), store.runtime_paths());
        let (agent, stop) = rested();
        let request = IdleStopRequest {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            stop,
        };
        crate::store::idle_stop::arm(paths, request.clone()).expect("arm");
        let snapshot_at = |agent: &AgentState, now: i64, pane: bool| {
            let mut snapshot = SidebarSnapshot::build_with_agents(
                paths.workspace_id.clone(),
                vec![agent.clone()],
                ts(now),
            );
            if pane {
                snapshot.agent_panes.push(PaneAgent {
                    kind: agent.kind.clone(),
                    kind_ordinal: None,
                    name: None,
                    name_explicit: false,
                    profile: None,
                    role: None,
                    channel: None,
                    agent_id: Some(agent.agent_id.clone()),
                    pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
                    pane_pid: None,
                    worktree_path: None,
                    worktree_branch: None,
                });
            }
            snapshot
        };
        let spawned = |snapshot: &SidebarSnapshot| {
            let mut requests = Vec::new();
            stop_idle_agents_with(snapshot, paths, runtime, |request| {
                requests.push(request.clone());
                true
            });
            requests
        };

        assert!(spawned(&snapshot_at(&agent, DUE - 1, true)).is_empty());
        assert!(spawned(&snapshot_at(&agent, DUE, false)).is_empty());
        let mut running = agent.clone();
        running.status = AgentStatus::Running;
        assert!(spawned(&snapshot_at(&running, DUE, true)).is_empty());

        assert_eq!(
            spawned(&snapshot_at(&agent, DUE, true)),
            [IdleStopHelperRequest {
                workspace_id: paths.workspace_id.clone(),
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
                pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
                label: "@claude".to_owned(),
            }]
        );
        // A declining helper leaves the request armed; the producer asks again
        // after the throttle with no new turn.
        assert!(spawned(&snapshot_at(&agent, DUE + 29, true)).is_empty());
        assert_eq!(spawned(&snapshot_at(&agent, DUE + 30, true)).len(), 1);
        assert_eq!(crate::store::idle_stop::read(paths), [request]);
        assert!(
            read_fire_record(&fire_record_path(runtime, &agent.kind, &agent.agent_id)).is_some()
        );
    }
}
