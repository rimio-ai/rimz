//! Producer-side backstop for subagents whose parent watchdog failed.
//!
//! The producer only reads durable records and starts a short-lived hidden
//! helper. That helper re-verifies the orphan and owns the diagnostic write
//! plus pane reclamation.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::AgentState;
use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
use crate::store::run::RunRecord;
use crate::{RuntimePaths, StatePaths};

use super::fleet::{FleetRuns, newest_run};
use super::schedule::pending::SessionWaits;

const ORPHAN_GRACE: Duration = Duration::from_secs(10 * 60);
#[cfg(any(test, feature = "testkit"))]
const TEST_GRACE_MS_ENV: &str = "RIMZ_TEST_SUBAGENT_ORPHAN_GRACE_MS";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanSubagentRequest {
    pub workspace_id: WorkspaceId,
    pub child_kind: AgentKind,
    pub child_agent_id: AgentSessionId,
    pub parent_agent_id: AgentSessionId,
}

#[derive(Serialize)]
struct SubagentDigestRequest {
    workspace_id: WorkspaceId,
    parent_agent_id: AgentSessionId,
}

#[derive(Clone, Debug)]
pub struct OrphanedSubagent {
    pub child: AgentState,
    pub run: Option<RunRecord>,
    pub orphaned_at: Timestamp,
}

#[derive(Debug, thiserror::Error)]
pub enum OrphanSweepErr {
    #[error(transparent)]
    Snapshot(#[from] crate::store::snapshot::SnapshotErr),
    #[error(transparent)]
    RunStore(#[from] crate::store::run::RunStoreErr),
}

/// Detect durable parent orphans and delegate each repair to a hidden helper.
pub(crate) fn enforce(
    paths: &StatePaths,
    runtime: &RuntimePaths,
    runs: &[RunRecord],
    now: Timestamp,
    rollup: &mut crate::store::snapshot::RollupCursor,
) {
    let (orphans, parents) = match scan(paths, runs, now, rollup) {
        Ok(decisions) => decisions,
        Err(err) => {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "sidebar: failed to scan for orphaned subagents",
            );
            return;
        }
    };
    for orphan in orphans {
        spawn_helper(runtime, &orphan);
    }
    for parent_agent_id in parents {
        spawn_digest_helper(runtime, parent_agent_id);
    }
}

/// Re-read one producer request immediately before the helper repairs it.
pub fn resolve(
    paths: &StatePaths,
    request: &OrphanSubagentRequest,
    now: Timestamp,
) -> Result<Option<OrphanedSubagent>, OrphanSweepErr> {
    let runs = crate::harness::run::list(paths)?;
    let (_, agents, _) = crate::store::snapshot::RollupCursor::new().fold(paths)?;
    Ok(agents
        .iter()
        .filter_map(|child| orphaned_child(child, agents.iter(), &runs, now))
        .find(|orphan| {
            orphan.child.kind == request.child_kind
                && orphan.child.agent_id == request.child_agent_id
                && orphan.child.parent_agent_id.as_ref() == Some(&request.parent_agent_id)
        }))
}

/// Decide the orphans and the digest parents from one fold of `rollup`,
/// walking both rollup layers borrowed so ended history is never copied.
fn scan(
    paths: &StatePaths,
    runs: &[RunRecord],
    now: Timestamp,
    rollup: &mut crate::store::snapshot::RollupCursor,
) -> Result<(Vec<OrphanedSubagent>, Vec<AgentSessionId>), OrphanSweepErr> {
    let (_, agents, _) = rollup.fold(paths)?;
    let orphans = agents
        .iter()
        .filter_map(|child| orphaned_child(child, agents.iter(), runs, now))
        .collect();
    let waits = std::cell::OnceCell::new();
    let parents = digest_parents_from(agents.iter(), runs, |kind, session| {
        waits
            .get_or_init(|| SessionWaits::load(paths))
            .contains(kind, session)
    });
    Ok((orphans, parents))
}

fn orphaned_child<'a>(
    child: &AgentState,
    agents: impl IntoIterator<Item = &'a AgentState> + Clone,
    runs: &[RunRecord],
    now: Timestamp,
) -> Option<OrphanedSubagent> {
    if child.ended_at.is_some() || !child.is_launched_child() {
        return None;
    }
    let run = newest_run(child, runs);
    if run.is_some_and(RunRecord::survives_parent) {
        return None;
    }
    let parent = crate::address::launched_parent(agents, child);
    let orphaned_at = match parent {
        Some(parent) => parent.ended_at?,
        None => child
            .registered_at
            .or_else(|| run.map(|run| run.started_at))?,
    };
    if orphaned_at + orphan_grace() > now {
        return None;
    }
    Some(OrphanedSubagent {
        child: child.clone(),
        run: run.cloned(),
        orphaned_at,
    })
}

fn digest_parents_from<'a>(
    agents: impl IntoIterator<Item = &'a AgentState> + Clone,
    runs: &'a [RunRecord],
    has_wait: impl Fn(&AgentKind, &AgentSessionId) -> bool,
) -> Vec<AgentSessionId> {
    agents
        .clone()
        .into_iter()
        .filter(|parent| parent.ended_at.is_none())
        .filter_map(|parent| {
            let fleet = FleetRuns::of(agents.clone(), runs, parent);
            let peer_needs_settlement = crate::address::launched_fleet(agents.clone(), parent)
                .into_iter()
                .filter(|peer| crate::address::is_launch_row(agents.clone(), peer))
                .any(|peer| {
                    newest_run(peer, runs).is_some_and(|run| {
                        run.peer.is_some()
                            && !run.status.is_terminal()
                            && (peer.ended_at.is_some()
                                || crate::store::runtime::agent_liveness(peer)
                                    == crate::store::runtime::AgentLiveness::Dead
                                || (run.parked_at.is_some()
                                    && {
                                        let owed = FleetRuns::of(agents.clone(), runs, peer);
                                        !owed.any_running() && owed.unreported().is_empty()
                                    }
                                    && !run
                                        .agent_id
                                        .as_ref()
                                        .is_some_and(|session| has_wait(&run.kind, session))))
                    })
                });
            (peer_needs_settlement
                || !super::fleet::ended_team_runs(agents.clone(), runs, parent).is_empty()
                || (!fleet.is_empty() && !fleet.any_running() && !fleet.unreported().is_empty()))
            .then(|| parent.agent_id.clone())
        })
        .collect()
}

fn spawn_helper(runtime: &RuntimePaths, orphan: &OrphanedSubagent) {
    let request = OrphanSubagentRequest {
        workspace_id: runtime.workspace_id.clone(),
        child_kind: orphan.child.kind.clone(),
        child_agent_id: orphan.child.agent_id.clone(),
        parent_agent_id: orphan
            .child
            .parent_agent_id
            .clone()
            .expect("launched child has a parent id"),
    };
    if let Err(err) = spawn_request(
        runtime,
        "orphan-subagent",
        &request,
        "orphan-subagent-repair",
    ) {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            child = %orphan.child.agent_id,
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn orphaned subagent repair helper",
        );
    }
}

pub fn spawn_digest_helper(runtime: &RuntimePaths, parent_agent_id: AgentSessionId) {
    let request = SubagentDigestRequest {
        workspace_id: runtime.workspace_id.clone(),
        parent_agent_id,
    };
    if let Err(err) = spawn_request(
        runtime,
        "subagent-digest",
        &request,
        "subagent-digest-backstop",
    ) {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn subagent digest backstop",
        );
    }
}

fn spawn_request<T: Serialize>(
    runtime: &RuntimePaths,
    verb: &str,
    request: &T,
    label: &'static str,
) -> std::io::Result<()> {
    let args = crate::child_process::agent_helper_argv(verb, request);
    crate::child_process::spawn_detached_rimz(runtime, args, label)
}

fn orphan_grace() -> Duration {
    #[cfg(any(test, feature = "testkit"))]
    if let Some(ms) = std::env::var(TEST_GRACE_MS_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    ORPHAN_GRACE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::agents::PermissionMode;
    use std::path::{Path, PathBuf};

    fn run(name: &str, at: Timestamp) -> RunRecord {
        let mut run = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/tmp/orphan-sweep")),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "work".to_owned(),
            PathBuf::from("/tmp/orphan-sweep"),
        );
        run.agent_name = Some(name.to_owned());
        run.started_at = at;
        run
    }

    #[test]
    fn sweep_materializes_no_rows_cold_or_warm_as_carryover_grows() {
        let before = crate::store::snapshot::fold_testkit::rollup_rows_materialized();
        for rows in [5, 500] {
            let dir = tempfile::tempdir().unwrap();
            let id = WorkspaceId::from_project_root(dir.path());
            let paths = StatePaths::under(id.clone(), dir.path()).unwrap();
            let runtime = RuntimePaths::under(id, dir.path()).unwrap();
            let store = crate::Store::open(paths.clone(), runtime).unwrap();
            crate::testkit::fleet::seed_ended_carryover(&store, rows).unwrap();
            crate::testkit::fleet::seed_fleet_store(&paths, 1, 1).unwrap();
            let mut cursor = crate::store::snapshot::RollupCursor::new();
            for _ in 0..2 {
                let (orphans, parents) = scan(&paths, &[], Timestamp::now(), &mut cursor).unwrap();
                assert!(orphans.is_empty());
                assert!(parents.is_empty());
            }
        }
        assert_eq!(
            crate::store::snapshot::fold_testkit::rollup_rows_materialized() - before,
            0,
            "cold and warm sweeps must walk both rollup layers borrowed"
        );
    }

    #[test]
    fn sweep_reads_ended_parent_from_carryover_for_a_recent_logged_child() {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(id.clone(), dir.path()).unwrap();
        let store = crate::Store::open(paths.clone(), runtime).unwrap();
        crate::testkit::fleet::seed_ended_carryover(&store, 1).unwrap();
        let now = Timestamp::now() + orphan_grace() + Duration::from_secs(1);
        let mut observation = crate::agents::AgentLifecycleObservation::new(
            Some("child".into()),
            crate::agents::LifecycleSignal::Registered,
        );
        observation.parent_agent_id = Some("history-0".into());
        observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        observation.launch.launch_depth = Some(1);
        let mut event = crate::store::event::EventEnvelope::agent_lifecycle(
            id,
            "child",
            "codex",
            "SessionStart",
            &observation,
        );
        event.timestamp = now;
        crate::store::event_log::append(&paths.events_log, &event).unwrap();
        let (orphans, parents) = scan(
            &paths,
            &[],
            now,
            &mut crate::store::snapshot::RollupCursor::new(),
        )
        .unwrap();
        assert_eq!(
            orphans.len(),
            1,
            "the ended parent, not absence, proves the orphan"
        );
        assert_eq!(orphans[0].child.agent_id, "child");
        assert_eq!(orphans[0].child.registered_at, Some(now));
        assert!(orphans[0].orphaned_at + orphan_grace() <= now);
        assert!(parents.is_empty());
    }

    #[test]
    fn only_old_unkept_children_of_ended_or_missing_parents_are_orphans() {
        let now = Timestamp::from_second(1_000).unwrap();
        let old = now - Duration::from_secs(601);
        let mut parent = crate::testkit::agent_state("codex", "parent", old);
        parent.ended_at = Some(old);
        let mut child = crate::testkit::agent_state("codex", "child", old);
        child.name = Some("child".to_owned());
        child.status = AgentStatus::Idle;
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child.registered_at = Some(old);
        let mut child_run = run("child", old);

        assert!(
            orphaned_child(
                &child,
                &[parent.clone(), child.clone()],
                &[child_run.clone()],
                now
            )
            .is_some()
        );

        child_run.keep = true;
        assert!(
            orphaned_child(
                &child,
                &[parent.clone(), child.clone()],
                &[child_run.clone()],
                now
            )
            .is_none()
        );

        child_run.keep = false;
        child_run.report_to = crate::store::run::ReportTo::Nobody;
        assert!(
            orphaned_child(
                &child,
                &[parent.clone(), child.clone()],
                &[child_run.clone()],
                now
            )
            .is_none(),
            "a detached child outlives the parent that launched it"
        );

        child_run.report_to = crate::store::run::ReportTo::Launcher;
        parent.ended_at = None;
        assert!(
            orphaned_child(&child, &[parent, child.clone()], &[child_run.clone()], now).is_none()
        );
        assert!(orphaned_child(&child, &[child.clone()], &[child_run], now).is_some());

        child.registered_at = Some(now);
        assert!(orphaned_child(&child, &[child.clone()], &[], now).is_none());
    }

    #[test]
    fn parent_successor_prevents_orphaning_until_the_whole_launch_ends() {
        let now = Timestamp::from_second(1_000).unwrap();
        let at = now - Duration::from_secs(601);
        let mut old = crate::testkit::agent_state("codex", "OLD", at);
        old.launch_id = Some(AgentSessionId::from("L"));
        old.ended_at = Some(at);
        let mut new = crate::testkit::agent_state("codex", "NEW", at);
        new.launch_id = old.launch_id.clone();
        let mut child = crate::testkit::agent_state("codex", "child", at);
        child.launch_depth = Some(1);
        child.registered_at = Some(at);
        for parent_id in ["L", "OLD"] {
            child.parent_agent_id = Some(AgentSessionId::from(parent_id));
            for ended in [false, true] {
                new.ended_at = ended.then_some(at);
                let agents = [old.clone(), new.clone(), child.clone()];
                assert_eq!(orphaned_child(&child, &agents, &[], now).is_some(), ended);
            }
        }
    }

    #[test]
    fn adopted_live_parent_still_matches_the_childs_launch_id() {
        let now = Timestamp::from_second(1_000).unwrap();
        let old = now - Duration::from_secs(601);
        let mut parent = crate::testkit::agent_state("codex", "parent-session", old);
        parent.launch_id = Some(AgentSessionId::from("launch-parent"));
        let mut child = crate::testkit::agent_state("codex", "child-session", old);
        child.parent_agent_id = Some(AgentSessionId::from("launch-parent"));
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child.registered_at = Some(old);

        assert!(orphaned_child(&child, &[parent, child.clone()], &[], now).is_none());
    }

    #[test]
    fn terminal_unreported_fleet_needs_one_digest_backstop() {
        let at = Timestamp::from_second(1_000).unwrap();
        let parent = crate::testkit::agent_state("codex", "parent", at);
        let children = ["first", "second"].map(|name| {
            let mut child = crate::testkit::agent_state("codex", name, at);
            child.name = Some(name.to_owned());
            child.parent_agent_id = Some(parent.agent_id.clone());
            child.parent_agent_kind = Some(parent.kind.clone());
            child.launch_depth = Some(1);
            child
        });
        let mut runs = children.each_ref().map(|child| {
            let mut run = run(child.name.as_deref().unwrap(), at);
            run.agent_id = Some(child.agent_id.clone());
            run.status = crate::store::run::RunStatus::Completed;
            run.subagent = true;
            run
        });
        let agents = [std::slice::from_ref(&parent), &children].concat();

        assert_eq!(
            digest_parents_from(&agents, &runs, |_, _| false),
            vec![parent.agent_id]
        );
        let message_id = crate::MessageId::new();
        for run in &mut runs {
            run.report_message_id = Some(message_id.clone());
        }
        assert!(digest_parents_from(&agents, &runs, |_, _| false).is_empty());
    }

    #[test]
    fn open_dead_or_parked_peer_needs_digest_backstop() {
        let at = Timestamp::from_second(1_000).unwrap();
        let launcher = crate::testkit::agent_state("codex", "launcher", at);
        let mut peer = crate::testkit::agent_state("codex", "peer", at);
        peer.launch_id = Some("peer-launch".into());
        peer.launched_by = Some(crate::agents::LaunchedBy {
            kind: launcher.kind.clone(),
            agent_id: launcher.agent_id.clone(),
        });
        let mut record = run("peer", at);
        record.peer = Some(crate::store::run::PeerRun {
            launch_id: "peer-launch".into(),
            opened_by: Vec::new(),
        });
        for pending in [true, false] {
            record.status = if pending {
                crate::store::run::RunStatus::Pending
            } else {
                crate::store::run::RunStatus::Running
            };
            for (ended, parked, expected) in [
                (false, false, false),
                (true, false, true),
                (false, true, true),
            ] {
                peer.ended_at = ended.then_some(at);
                record.parked_at = parked.then_some(at);
                let parents = digest_parents_from(
                    &[launcher.clone(), peer.clone()],
                    std::slice::from_ref(&record),
                    |_, _| false,
                );
                assert_eq!(
                    parents.contains(&launcher.agent_id),
                    expected,
                    "pending={pending}, ended={ended}, parked={parked}"
                );
            }
        }
    }

    #[test]
    fn parked_peer_backstop_checks_the_runs_wait_identity() {
        let at = Timestamp::from_second(1_000).unwrap();
        let launcher = crate::testkit::agent_state("codex", "launcher", at);
        let mut peer = crate::testkit::agent_state("codex", "row-session", at);
        peer.launch_id = Some("peer-launch".into());
        peer.launched_by = Some(crate::agents::LaunchedBy {
            kind: launcher.kind.clone(),
            agent_id: launcher.agent_id.clone(),
        });
        let mut record = run("peer", at);
        record.agent_id = Some("run-session".into());
        record.status = crate::store::run::RunStatus::Running;
        record.parked_at = Some(at);
        record.peer = Some(crate::store::run::PeerRun {
            launch_id: "peer-launch".into(),
            opened_by: Vec::new(),
        });
        for (wait_kind, wait_session, ended, has_session, expected) in [
            ("codex", "row-session", false, true, true),
            ("claude", "run-session", false, true, true),
            ("codex", "run-session", true, true, true),
            ("codex", "run-session", false, false, true),
            ("codex", "run-session", false, true, false),
        ] {
            peer.ended_at = ended.then_some(at);
            record.agent_id = has_session.then(|| "run-session".into());
            let parents = digest_parents_from(
                &[launcher.clone(), peer.clone()],
                std::slice::from_ref(&record),
                |kind, session| {
                    kind == &AgentKind::new_unchecked(wait_kind)
                        && session == &AgentSessionId::from(wait_session)
                },
            );
            assert_eq!(
                parents.contains(&launcher.agent_id),
                expected,
                "wait={wait_kind}/{wait_session}, ended={ended}, has_session={has_session}"
            );
        }
    }

    #[test]
    fn ended_predecessor_row_does_not_settle_live_successor_turn() {
        let at = Timestamp::from_second(1_000).unwrap();
        let launcher = crate::testkit::agent_state("codex", "launcher", at);
        let rows = ["cleared", "peer"].map(|id| {
            let mut row = crate::testkit::agent_state("codex", id, at);
            row.launch_id = Some("peer-launch".into());
            row.launched_by = Some(crate::agents::LaunchedBy {
                kind: launcher.kind.clone(),
                agent_id: launcher.agent_id.clone(),
            });
            row
        });
        let [mut cleared, peer] = rows;
        cleared.ended_at = Some(at);
        let mut record = run("peer", at);
        record.agent_id = Some(peer.agent_id.clone());
        record.status = crate::store::run::RunStatus::Running;
        record.peer = Some(crate::store::run::PeerRun {
            launch_id: "peer-launch".into(),
            opened_by: Vec::new(),
        });
        let mut agents = [launcher.clone(), cleared, peer];
        let runs = std::slice::from_ref(&record);
        assert!(!digest_parents_from(&agents, runs, |_, _| false).contains(&launcher.agent_id));
        agents[2].ended_at = Some(at);
        assert!(
            digest_parents_from(&agents, runs, |_, _| false).contains(&launcher.agent_id),
            "the launch's own end still settles its turn"
        );
    }

    #[test]
    fn parked_peer_waits_for_its_own_fleet_before_digest_backstop() {
        let at = Timestamp::from_second(1_000).unwrap();
        let launcher = crate::testkit::agent_state("codex", "launcher", at);
        let mut peer = crate::testkit::agent_state("codex", "peer", at);
        peer.launch_id = Some("peer-launch".into());
        peer.launched_by = Some(crate::agents::LaunchedBy {
            kind: launcher.kind.clone(),
            agent_id: launcher.agent_id.clone(),
        });
        let mut child = crate::testkit::agent_state("codex", "child", at);
        child.parent_agent_id = peer.launch_id.clone();
        child.parent_agent_kind = Some(peer.kind.clone());
        child.launch_depth = Some(2);
        let mut peer_run = run("peer", at);
        peer_run.peer = Some(crate::store::run::PeerRun {
            launch_id: "peer-launch".into(),
            opened_by: Vec::new(),
        });
        peer_run.status = crate::store::run::RunStatus::Running;
        peer_run.parked_at = Some(at);
        peer_run.agent_id = Some(peer.agent_id.clone());
        let mut child_run = run("child", at);
        child_run.agent_id = Some(child.agent_id.clone());
        child_run.subagent = true;
        child_run.status = crate::store::run::RunStatus::Running;
        let agents = [launcher.clone(), peer, child];
        let mut runs = [peer_run, child_run];
        let no_lookup = |_: &AgentKind, _: &AgentSessionId| -> bool {
            panic!("a peer whose fleet is still owed needs no wait lookup")
        };
        assert!(!digest_parents_from(&agents, &runs, no_lookup).contains(&launcher.agent_id));
        runs[1].status = crate::store::run::RunStatus::Completed;
        assert!(!digest_parents_from(&agents, &runs, no_lookup).contains(&launcher.agent_id));
        runs[1].report_message_id = Some(crate::MessageId::new());
        assert_eq!(
            digest_parents_from(&agents, &runs, |_, _| false),
            vec![launcher.agent_id]
        );
    }

    #[test]
    fn dead_team_cohort_needs_its_live_launchers_backstop() {
        let at = Timestamp::from_second(1_000).unwrap();
        let launcher = crate::testkit::agent_state("codex", "launcher", at);
        let mut leader = crate::testkit::agent_state("codex", "leader", at);
        leader.team = Some("forge".into());
        leader.launch_id = Some("leader-launch".into());
        leader.launched_by = Some(crate::agents::LaunchedBy {
            kind: launcher.kind.clone(),
            agent_id: launcher.agent_id.clone(),
        });
        let mut record = run("leader", at);
        record.status = crate::store::run::RunStatus::Running;
        record.team = Some(crate::store::run::TeamRun {
            launch_id: "leader-launch".into(),
            instance: format!(
                "forge#{}",
                leader.channel().unwrap_or_else(|| "external".into())
            ),
        });
        let mut agents = [launcher.clone(), leader];
        let runs = [record];
        assert!(digest_parents_from(&agents, &runs, |_, _| false).is_empty());
        agents[1].ended_at = Some(at);
        assert_eq!(
            digest_parents_from(&agents, &runs, |_, _| false),
            vec![launcher.agent_id]
        );
        agents[0].ended_at = Some(at);
        assert!(digest_parents_from(&agents, &runs, |_, _| false).is_empty());
    }

    #[test]
    fn terminal_background_peer_needs_its_launchers_digest_backstop() {
        let at = Timestamp::from_second(1_000).unwrap();
        let launcher = crate::testkit::agent_state("codex", "launcher", at);
        let mut peer = crate::testkit::agent_state("codex", "peer", at);
        peer.name = Some("peer".to_owned());
        peer.launch_depth = Some(1);
        peer.launched_by = Some(crate::agents::LaunchedBy {
            kind: launcher.kind.clone(),
            agent_id: launcher.agent_id.clone(),
        });
        let mut peer_run = run("peer", at);
        peer_run.agent_id = Some(peer.agent_id.clone());
        peer_run.status = crate::store::run::RunStatus::Completed;
        let agents = [launcher.clone(), peer];

        assert_eq!(
            digest_parents_from(&agents, std::slice::from_ref(&peer_run), |_, _| false),
            vec![launcher.agent_id]
        );
        peer_run.joined_at = Some(at);
        assert!(digest_parents_from(&agents, &[peer_run], |_, _| false).is_empty());
    }
}
