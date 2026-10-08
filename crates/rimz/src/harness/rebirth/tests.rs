use super::*;
use crate::agents::{AgentLifecycleObservation, LifecycleSignal};
use crate::config::{Isolation, Profile, RoleBinding, Team};
use crate::harness::plan::CohortSeed;
use crate::harness::resume::RecoveryEntry;
use crate::ids::{MuxName, PaneId};

#[test]
fn fresh_replacements_end_only_after_their_tab_is_confirmed() {
    for (disposition, confirmed) in [
        (RebirthDisposition::RecoverKeep, true),
        (RebirthDisposition::RecoverKeep, false),
        (RebirthDisposition::RecoverDrop, true),
        (RebirthDisposition::RecoverDrop, false),
        (RebirthDisposition::Defer, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("forge");
        let fixture = Fixture::new(&[("planner", &worktree, true), ("coder", &worktree, true)]);
        fixture.stamp_team("planner", &worktree, "forge", "planner", "claude-plan");
        fixture.stamp_team("coder", &worktree, "forge", "coder", "codex-code");
        let transcript = fixture.project.join("planner.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let store = Store::open(fixture.paths.clone(), fixture.runtime.clone()).unwrap();
        for (id, path) in [
            ("planner", transcript),
            ("coder", fixture.project.join("missing.jsonl")),
        ] {
            let mut observation =
                AgentLifecycleObservation::new(Some(id.into()), LifecycleSignal::Registered);
            observation.transcript_path = Some(path.display().to_string());
            store
                .append_event(&crate::EventEnvelope::agent_lifecycle(
                    fixture.paths.workspace_id.clone(),
                    "rimz-test",
                    "claude",
                    "SessionStart",
                    &observation,
                ))
                .unwrap();
        }
        let plan = fixture.inspect_with(&team_machine(), false);
        assert_eq!(plan.preview().candidate_count(), 2);
        park_roster(&fixture.paths).unwrap();
        let seeded = plan.settle(disposition, "rimz-test");
        assert!(
            pending(&fixture).contains(&key("coder")),
            "not yet confirmed"
        );
        assert!(ended_events(&fixture).is_empty(), "not yet confirmed");
        let resume = seeded.confirm(|_, _| {
            if confirmed {
                Ok(())
            } else {
                Err("tab did not open")
            }
        });
        if !confirmed {
            assert!(pending(&fixture).contains(&key("coder")));
            assert!(ended_events(&fixture).is_empty());
            assert!(resume.tabs.is_empty());
            continue;
        }
        assert!(
            !pending(&fixture).contains(&key("coder")),
            "confirmed replacement must settle its old session"
        );
        assert_eq!(
            ended_events(&fixture),
            [("rimz.seat-refilled".into(), "coder".into())]
        );
        let sink = crate::diag::DiagSink::for_workspace(
            fixture.paths.workspace_id.clone(),
            "rimz-test",
            None,
        );
        let records = std::fs::read_to_string(sink.log_path().unwrap()).unwrap();
        let refills = records
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|row| row["event"]["kind"] == "recovery_seat_refilled")
            .collect::<Vec<_>>();
        assert_eq!(refills.len(), 1);
        assert_eq!(refills[0]["event"]["agent_id"], "coder");
        assert_eq!(refills[0]["severity"], "info");
    }
}

#[test]
fn live_replacements_settle_only_the_seats_they_fill() {
    for with_planner in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("forge");
        let mut agents = vec![("coder", worktree.as_path(), true)];
        if with_planner {
            agents.push(("planner", worktree.as_path(), true));
        }
        let fixture = Fixture::new(&agents);
        fixture.stamp_team("coder", &worktree, "forge", "coder", "codex-code");
        if with_planner {
            fixture.stamp_team("planner", &worktree, "forge", "planner", "claude-plan");
        }
        park_roster(&fixture.paths).unwrap();
        fixture.stamp_team("live-coder", &worktree, "forge", "coder", "codex-code");
        fixture.own("live-coder", std::process::id());
        assert!(matches!(
            owner_liveness(&fixture, "live-coder"),
            AgentLiveness::Live { .. }
        ));
        let plan = inspect_live_at(
            fixture.paths.clone(),
            fixture.runtime.clone(),
            &fixture.project,
            &team_machine(),
            false,
        );
        assert_eq!(plan.preview().candidate_count(), usize::from(with_planner));
        assert_eq!(plan.preview().refilled_count(), 1);
        assert!(
            plan.planned
                .entries
                .iter()
                .all(|entry| matches!(entry, RecoveryEntry::Flat(_)))
        );
        let resume = materialize(plan, RebirthDisposition::Defer, "rimz-test");
        assert!(resume.tabs.is_empty());
        assert_eq!(
            pending(&fixture),
            if with_planner {
                BTreeSet::from([key("planner")])
            } else {
                BTreeSet::new()
            }
        );
        assert_eq!(
            ended_events(&fixture),
            [("rimz.seat-refilled".into(), "coder".into())]
        );
    }
}

#[test]
fn invalid_effective_config_keeps_flat_recovery_without_fresh_team_seats() {
    let fixture = Fixture::new(&[]);
    std::fs::create_dir_all(fixture.project.join(".rimz")).unwrap();
    std::fs::write(fixture.project.join(".rimz/config.toml"), "[tiers]\n").unwrap();
    let machine = team_machine();
    let availability = crate::harness::plan::LaunchAvailability::read(
        &fixture.runtime,
        &fixture.paths,
        &machine,
        Timestamp::UNIX_EPOCH,
    );
    let (teams, profiles) = effective_teams_and_profiles(&machine, &fixture.project, &availability);
    assert!(
        teams.0.is_empty(),
        "an unrouted fresh seat must not be planned"
    );
    assert_eq!(profiles.0.len(), machine.agents.profiles.0.len());
}

struct Fixture {
    _dir: tempfile::TempDir,
    paths: StatePaths,
    runtime: RuntimePaths,
    project: PathBuf,
}

fn materialize(
    plan: RebirthPlan,
    disposition: RebirthDisposition,
    session_name: &str,
) -> ResumePlan {
    if plan.boundary {
        park_roster(&plan.paths).expect("park before birth");
    }
    plan.settle(disposition, session_name)
        .confirm(|_, _| Ok::<(), &str>(()))
}

fn auto_resume_assists(fixture: &Fixture) -> Vec<(usize, Vec<String>)> {
    crate::harness::assist_log::recent(&crate::disk::paths::logs_dir(), None)
        .into_iter()
        .filter_map(|record| match record.assist {
            crate::harness::assist_log::Assist::AutoResume {
                workspace_id,
                recovered,
                labels,
                ..
            } if workspace_id == fixture.paths.workspace_id => Some((recovered, labels)),
            _ => None,
        })
        .collect()
}

fn recovered_count(fixture: &Fixture) -> Option<usize> {
    let marker: LastDeathMarker =
        serde_json::from_slice(&std::fs::read(&fixture.paths.last_death_marker).expect("marker"))
            .expect("marker");
    marker.recovered
}

impl Fixture {
    fn new(agents: &[(&str, &Path, bool)]) -> Self {
        Self::with_children(agents, &[])
    }

    fn with_children(agents: &[(&str, &Path, bool)], children: &[&str]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).expect("project");
        let workspace = WorkspaceId::from_project_root(&project);
        let paths = StatePaths::under(workspace.clone(), &dir.path().join("state")).expect("paths");
        let runtime =
            RuntimePaths::under(workspace.clone(), &dir.path().join("runtime")).expect("runtime");
        let store = Store::open(paths.clone(), runtime.clone()).expect("store");
        let resolved = crate::workspace::WorkspaceResolver::resolve(&project, None).unwrap();
        store.record_workspace(&resolved).unwrap();
        write_boot_marker(&paths.boot_marker, "boot-a");
        let mut roster = BTreeSet::new();
        for (id, worktree, create_worktree) in agents {
            if *create_worktree {
                std::fs::create_dir_all(worktree).expect("worktree");
            }
            let mut observation = AgentLifecycleObservation::new(
                Some(AgentSessionId::from(*id)),
                LifecycleSignal::Registered,
            );
            observation.agent_name = Some((*id).to_owned());
            if children.contains(id) {
                observation.parent_agent_id = Some("root".into());
                observation.launch.parent_agent_id = Some("root".into());
                observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
                observation.launch.launch_depth = Some(1);
            }
            observation.worktree_path = Some(worktree.display().to_string());
            observation.worktree_branch = Some("feature".to_owned());
            observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, format!("%{id}")));
            store
                .append_event(&crate::EventEnvelope::agent_lifecycle(
                    workspace.clone(),
                    "rimz-test",
                    "claude",
                    "SessionStart",
                    &observation,
                ))
                .expect("agent event");
            roster.insert((
                AgentKind::new_unchecked("claude"),
                AgentSessionId::from(*id),
            ));
        }
        live_roster::publish(&paths.live_roster, roster).expect("roster");
        Self {
            _dir: dir,
            paths,
            runtime,
            project,
        }
    }

    fn inspect(&self, disabled: bool) -> RebirthPlan {
        self.inspect_with(&MachineConfig::default(), disabled)
    }

    fn inspect_with(&self, machine: &MachineConfig, disabled: bool) -> RebirthPlan {
        self.inspect_within(machine, disabled, OWNER_EXIT_BOUND)
    }

    fn inspect_within(
        &self,
        machine: &MachineConfig,
        disabled: bool,
        owner_exit_bound: Duration,
    ) -> RebirthPlan {
        inspect_at(
            self.paths.clone(),
            self.runtime.clone(),
            Some("boot-a".to_owned()),
            Vec::new(),
            &self.project,
            machine,
            disabled,
            owner_exit_bound,
        )
        .expect("inspect")
    }

    fn own(&self, id: &str, pid: u32) {
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(id)),
            LifecycleSignal::Registered,
        );
        observation.agent_pid = Some(pid);
        Store::open(self.paths.clone(), self.runtime.clone())
            .expect("store")
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                self.paths.workspace_id.clone(),
                "rimz-test",
                "claude",
                "SessionStart",
                &observation,
            ))
            .expect("owner event");
    }

    fn stamp_team(&self, id: &str, worktree: &Path, team: &str, role: &str, profile: &str) {
        let store = Store::open(self.paths.clone(), self.runtime.clone()).expect("store");
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(id)),
            LifecycleSignal::Registered,
        );
        observation.agent_name = Some(id.to_owned());
        observation.launch.team = Some(team.to_owned());
        observation.launch.role = Some(role.to_owned());
        observation.launch.profile = Some(profile.to_owned());
        observation.worktree_path = Some(worktree.display().to_string());
        observation.worktree_branch = Some("feature".to_owned());
        observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, format!("%{id}")));
        store
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                self.paths.workspace_id.clone(),
                "rimz-test",
                "claude",
                "SessionStart",
                &observation,
            ))
            .expect("team agent event");
    }

    fn touch_agent(&self, id: &str, worktree: &Path) {
        let store = Store::open(self.paths.clone(), self.runtime.clone()).expect("store");
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(id)),
            LifecycleSignal::Registered,
        );
        observation.agent_name = Some(id.to_owned());
        observation.worktree_path = Some(worktree.display().to_string());
        observation.worktree_branch = Some("feature".to_owned());
        observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, format!("%{id}")));
        store
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                self.paths.workspace_id.clone(),
                "rimz-test",
                "claude",
                "SessionStart",
                &observation,
            ))
            .expect("touch agent event");
    }
}

fn key(id: &str) -> (AgentKind, AgentSessionId) {
    (AgentKind::new_unchecked("claude"), AgentSessionId::from(id))
}

fn owner_liveness(fixture: &Fixture, id: &str) -> AgentLiveness {
    let projection = Store::open_existing(fixture.paths.clone(), fixture.runtime.clone())
        .expect("store")
        .runtime_projection(crate::RuntimeScope::Audit)
        .expect("projection");
    agent_liveness(
        find_agent(&projection.agents, "claude", &AgentSessionId::from(id)).expect("agent"),
    )
}

fn pending(fixture: &Fixture) -> BTreeSet<(AgentKind, AgentSessionId)> {
    pending_recovery::read(&fixture.paths.pending_recovery)
}

fn event_log(fixture: &Fixture) -> String {
    String::from_utf8_lossy(&std::fs::read(&fixture.paths.events_log).unwrap()).into_owned()
}

fn ended_events(fixture: &Fixture) -> Vec<(String, AgentSessionId)> {
    Store::open(fixture.paths.clone(), fixture.runtime.clone())
        .expect("store")
        .read_events()
        .expect("events")
        .into_iter()
        .filter_map(|event| {
            let crate::store::event::EventKind::AgentLifecycle(payload) = event.kind() else {
                return None;
            };
            matches!(payload.observation.signal, LifecycleSignal::Ended).then(|| {
                (
                    payload.event_name.clone().expect("ended event name"),
                    payload
                        .observation
                        .agent_id
                        .clone()
                        .expect("ended agent id"),
                )
            })
        })
        .collect()
}

#[test]
fn inspection_is_read_only_and_scopes_to_live_roster() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let mut roster = live_roster::read(&fixture.paths.live_roster)
        .expect("read roster")
        .agents;
    roster.insert((
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("not-in-audit"),
    ));
    live_roster::publish(&fixture.paths.live_roster, roster).expect("publish expanded roster");
    pending_recovery::park(&fixture.paths, &[key("parked-earlier")].into()).expect("park");
    let pending_before = std::fs::read(&fixture.paths.pending_recovery).expect("pending");
    let boot_before = std::fs::read(&fixture.paths.boot_marker).expect("boot marker");
    let roster_before = std::fs::read(&fixture.paths.live_roster).expect("roster");
    let events_before = std::fs::read(&fixture.paths.events_log).expect("events");

    let plan = fixture.inspect(false);

    assert_eq!(plan.preview().death().unwrap().lost_agents.len(), 1);
    assert_eq!(
        plan.preview().death().unwrap().lost_agents[0]
            .agent_id
            .as_str(),
        "live"
    );
    assert_eq!(
        std::fs::read(&fixture.paths.boot_marker).unwrap(),
        boot_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.live_roster).unwrap(),
        roster_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.pending_recovery).unwrap(),
        pending_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.events_log).unwrap(),
        events_before
    );
    assert!(!fixture.paths.last_death_marker.exists());
    assert!(!fixture.paths.crashes_dir.exists());
}

#[test]
fn recover_orders_death_ended_stamp_and_rebirth_then_consumes_roster() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let missing = dir.path().join("missing");
    let fixture = Fixture::new(&[("live", &live, true), ("missing", &missing, false)]);
    let plan = fixture.inspect(false);
    let planned_labels = plan.preview().labels().to_vec();

    let outcome = materialize(plan, RebirthDisposition::RecoverDrop, "rimz-test");

    assert_eq!(
        outcome
            .tabs
            .iter()
            .map(ResumeTab::pane_count)
            .sum::<usize>(),
        1
    );
    assert!(!fixture.paths.live_roster.exists());
    assert!(
        pending(&fixture).is_empty(),
        "resumed and worktree-gone agents are settled"
    );
    let events =
        String::from_utf8_lossy(&std::fs::read(&fixture.paths.events_log).unwrap()).into_owned();
    let death = events.find("session.death").expect("death");
    let ended = events.find("rimz.worktree-gone").expect("ended stamp");
    let rebirth = events.find("session.rebirth").expect("rebirth");
    assert!(death < ended && ended < rebirth, "{events}");
    let marker: LastDeathMarker =
        serde_json::from_slice(&std::fs::read(&fixture.paths.last_death_marker).unwrap())
            .expect("marker");
    assert_eq!(marker.recovered, Some(1));
    let assist = crate::harness::assist_log::recent(&crate::disk::paths::logs_dir(), None)
        .into_iter()
        .find(|record| {
            matches!(
                &record.assist,
                crate::harness::assist_log::Assist::AutoResume { workspace_id, .. }
                    if workspace_id == &fixture.paths.workspace_id
            )
        })
        .expect("auto-resume assist");
    assert!(matches!(
        assist.assist,
        crate::harness::assist_log::Assist::AutoResume {
            session_name,
            cause: Some(SessionDeathCause::Crash),
            recovered: 1,
            labels,
            ..
        } if session_name == "rimz-test" && labels == planned_labels
    ));
}

#[test]
fn decline_archives_crash_and_records_zero_recovered_without_tabs() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let plan = fixture.inspect(false);

    let outcome = materialize(plan, RebirthDisposition::Decline, "rimz-test");

    assert!(outcome.tabs.is_empty());
    assert!(!fixture.paths.live_roster.exists());
    assert!(pending(&fixture).is_empty());
    let marker: LastDeathMarker =
        serde_json::from_slice(&std::fs::read(&fixture.paths.last_death_marker).unwrap())
            .expect("marker");
    assert_eq!(marker.recovered, Some(0));
    assert_eq!(
        std::fs::read_dir(&fixture.paths.crashes_dir)
            .expect("crashes")
            .count(),
        1
    );
    let archive = std::fs::read_dir(&fixture.paths.crashes_dir)
        .expect("crashes")
        .next()
        .expect("archive")
        .expect("archive entry")
        .path();
    let roster: Vec<AgentState> = serde_json::from_slice(
        &std::fs::read(archive.join("roster.json")).expect("roster archive"),
    )
    .expect("archived roster json");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0].agent_id.as_str(), "live");
    let events =
        String::from_utf8_lossy(&std::fs::read(&fixture.paths.events_log).unwrap()).into_owned();
    assert!(events.find("session.death").unwrap() < events.find("session.rebirth").unwrap());
    assert!(!events.contains("rimz.worktree-gone"));
    assert_eq!(
        ended_events(&fixture),
        vec![("rimz.recovery-declined".to_owned(), "live".into())]
    );
    let projection = Store::open(fixture.paths.clone(), fixture.runtime.clone())
        .expect("store")
        .runtime_projection(crate::RuntimeScope::Audit)
        .expect("projection");
    assert!(
        projection
            .agents
            .iter()
            .find(|agent| agent.agent_id == "live")
            .is_some_and(|agent| agent.ended_at.is_some())
    );

    live_roster::publish(
        &fixture.paths.live_roster,
        [(
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("live"),
        )]
        .into_iter()
        .collect(),
    )
    .expect("replant stale roster");
    assert_eq!(fixture.inspect(false).preview().pane_count(), 0);
}

#[test]
fn missing_worktree_stays_pending_without_a_drop_decision() {
    for disposition in [RebirthDisposition::Defer, RebirthDisposition::RecoverKeep] {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let fixture = Fixture::new(&[("missing", &missing, false)]);
        materialize(fixture.inspect(false), disposition, "rimz-test");
        assert_eq!(ended_events(&fixture), [], "{disposition:?}");
        assert_eq!(pending(&fixture), [key("missing")].into());
    }
}

#[test]
fn recover_ends_the_unresumed_rest_only_when_the_user_drops_it() {
    for disposition in [
        RebirthDisposition::RecoverKeep,
        RebirthDisposition::RecoverDrop,
    ] {
        let dir = tempfile::tempdir().expect("worktrees");
        let newest = dir.path().join("newest");
        let older = dir.path().join("older");
        let missing = dir.path().join("missing");
        let fixture = Fixture::new(&[
            ("newest", &newest, true),
            ("older", &older, true),
            ("missing", &missing, false),
        ]);
        let mut machine = MachineConfig::default();
        machine.resume.max = 1;
        let plan = fixture.inspect_with(&machine, false);
        let resumed = plan.planned.resumed_keys();
        assert_eq!(resumed.len(), 1);
        let over_cap = ["newest", "older"]
            .map(key)
            .into_iter()
            .find(|key| !resumed.contains(key))
            .expect("over-cap agent");
        let preview = plan.preview();
        assert_eq!(preview.candidate_count(), 3);
        assert_eq!(
            preview
                .unresumable()
                .iter()
                .map(|agent| agent
                    .reason
                    .as_ref()
                    .map(|reason| reason.label().into_owned()))
                .collect::<Vec<_>>(),
            [
                Some("worktree gone".to_owned()),
                Some("over the resume cap".to_owned())
            ],
            "both skipped agents need a drop decision"
        );

        let outcome = materialize(plan, disposition, "rimz-test");

        assert_eq!(outcome.resumed, resumed);
        let dropped = disposition == RebirthDisposition::RecoverDrop;
        let ended = ended_events(&fixture);
        assert_eq!(
            ended
                .iter()
                .filter(|(event, _)| event == "rimz.not-resumed")
                .map(|(_, agent_id)| agent_id.clone())
                .collect::<Vec<_>>(),
            if dropped {
                vec![over_cap.1.clone()]
            } else {
                Vec::new()
            },
            "{disposition:?}"
        );
        assert_eq!(
            ended
                .iter()
                .filter(|(_, agent_id)| agent_id.as_str() == "missing")
                .map(|(event, _)| event.as_str())
                .collect::<Vec<_>>(),
            if dropped {
                vec!["rimz.worktree-gone"]
            } else {
                Vec::new()
            },
            "{disposition:?}"
        );
        assert_eq!(
            pending(&fixture),
            if dropped {
                BTreeSet::new()
            } else {
                [over_cap, key("missing")].into()
            },
            "{disposition:?}"
        );
    }
}

#[test]
fn rebirth_fails_peer_turns_even_when_the_peer_is_recovered() {
    use crate::store::run::{PeerRun, RunRecord, RunStatus};
    for choice in [
        RebirthDisposition::RecoverKeep,
        RebirthDisposition::Decline,
        RebirthDisposition::Defer,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::new(&[("peer", dir.path(), true)]);
        let mut plan = fixture.inspect(false);
        let peer = plan
            .crash_roster
            .iter_mut()
            .find(|agent| agent.agent_id.as_str() == "peer")
            .unwrap();
        peer.launch_id = Some("peer-launch".into());
        peer.launched_by = Some(crate::agents::LaunchedBy {
            kind: peer.kind.clone(),
            agent_id: "launcher".into(),
        });
        let mut record = RunRecord::new(
            fixture.paths.workspace_id.clone(),
            peer.kind.clone(),
            crate::agents::PermissionMode::Auto,
            "task".into(),
            dir.path().into(),
        );
        record.peer = Some(PeerRun {
            launch_id: "peer-launch".into(),
            opened_by: Vec::new(),
        });
        crate::harness::run::create(&fixture.paths, &record).unwrap();
        materialize(plan, choice, "rimz-test");
        assert_eq!(
            crate::harness::run::load(&fixture.paths, &record.run_id)
                .unwrap()
                .status,
            RunStatus::Failed,
            "{choice:?}"
        );
    }
}

#[test]
fn rebirth_cancels_unresumed_child_runs_and_wakes_waiters() {
    use crate::agents::PermissionMode;
    use crate::harness::run;
    use crate::store::run::{RunRecord, RunStatus, WakeupFrame, run_socket_path};
    use std::os::unix::net::UnixDatagram;

    for (choice, inspected) in [
        (RebirthDisposition::Defer, false),
        (RebirthDisposition::RecoverDrop, true),
        (RebirthDisposition::Decline, true),
        (RebirthDisposition::RecoverKeep, true),
        (RebirthDisposition::Defer, true),
    ] {
        let dir = tempfile::tempdir().expect("worktrees");
        let worktree = dir.path().join("lane");
        let mut fixture = Fixture::with_children(
            &[
                ("root", &worktree, true),
                ("child", &worktree, true),
                ("finished", &worktree, true),
            ],
            &["child", "finished"],
        );
        let sockets_dir = tempfile::Builder::new()
            .prefix("r")
            .tempdir_in("/tmp")
            .unwrap();
        fixture.runtime.sock_dir = sockets_dir.path().to_path_buf();
        let mut records = Vec::new();
        for id in ["child", "finished"] {
            let mut record = RunRecord::new(
                fixture.paths.workspace_id.clone(),
                AgentKind::new_unchecked("claude"),
                PermissionMode::Auto,
                "task".to_owned(),
                worktree.clone(),
            );
            record.agent_id = Some(id.into());
            record.keep = true;
            record.status = if id == "child" {
                RunStatus::Running
            } else {
                RunStatus::Completed
            };
            run::create(&fixture.paths, &record).unwrap();
            records.push(record);
        }
        let sockets = records
            .iter()
            .map(|record| {
                let socket =
                    UnixDatagram::bind(run_socket_path(&fixture.runtime, &record.run_id)).unwrap();
                socket.set_nonblocking(true).unwrap();
                socket
            })
            .collect::<Vec<_>>();
        let plan = fixture.inspect(false);
        assert_eq!(
            plan.planned.resumed_keys(),
            BTreeSet::from([(AgentKind::new_unchecked("claude"), "root".into())])
        );
        let preview = plan.preview();
        if inspected {
            materialize(plan, choice, "rimz-test");
        } else {
            park_roster(&fixture.paths).expect("park before birth");
            record_boundary_at(
                fixture.paths.clone(),
                fixture.runtime.clone(),
                &fixture.paths.workspace_id,
                "rimz-test",
            );
        }
        let ended = ended_events(&fixture);
        if inspected {
            for id in ["child", "finished"] {
                assert!(
                    ended.contains(&("rimz.child-not-resumed".to_owned(), id.into())),
                    "{choice:?}: child end missing for {id}: {ended:?}"
                );
            }
            assert!(preview.unresumable().is_empty());
            assert_eq!(preview.candidate_count(), 1);
            assert_eq!(
                pending(&fixture),
                if choice == RebirthDisposition::Defer {
                    BTreeSet::from([key("root")])
                } else {
                    BTreeSet::new()
                },
                "{choice:?}"
            );
            if choice == RebirthDisposition::Decline {
                assert!(ended.contains(&("rimz.recovery-declined".into(), "root".into())));
            } else {
                assert_eq!(ended.len(), 2);
            }
            let sink =
                DiagSink::for_workspace(fixture.paths.workspace_id.clone(), "rimz-test", None);
            let diagnostics = std::fs::read_to_string(sink.log_path().unwrap()).unwrap();
            let children = diagnostics
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .filter(|row| row["event"]["kind"] == "recovery_child_ended")
                .collect::<Vec<_>>();
            assert_eq!(children.len(), 2);
            for id in ["child", "finished"] {
                assert!(
                    children
                        .iter()
                        .any(|row| { row["event"]["agent_id"] == id && row["severity"] == "info" })
                );
            }
        } else {
            assert!(ended.is_empty());
            assert_eq!(
                pending(&fixture),
                ["root", "child", "finished"].map(key).into()
            );
        }
        assert_eq!(
            run::load(&fixture.paths, &records[0].run_id)
                .unwrap()
                .status,
            RunStatus::Canceled,
            "{choice:?}"
        );
        assert_eq!(
            run::load(&fixture.paths, &records[1].run_id).unwrap(),
            records[1]
        );
        let mut frame = [0; 4096];
        let count = sockets[0].recv(&mut frame).expect("child waiter awakened");
        let WakeupFrame::RunCompleted {
            workspace_id,
            run_id,
            status,
        } = serde_json::from_slice(&frame[..count]).unwrap();
        assert_eq!(workspace_id, fixture.paths.workspace_id);
        assert_eq!(run_id, records[0].run_id);
        assert_eq!(status, RunStatus::Canceled);
        assert_eq!(
            sockets[1].recv(&mut frame).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn live_entry_defer_ends_children_and_cancels_their_open_runs() {
    use crate::agents::PermissionMode;
    use crate::harness::run;
    use crate::store::run::{RunRecord, RunStatus};

    for with_root in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("lane");
        let mut agents = vec![("child", worktree.as_path(), true)];
        if with_root {
            agents.push(("root", worktree.as_path(), true));
        }
        let fixture = Fixture::with_children(&agents, &["child"]);
        park_roster(&fixture.paths).unwrap();
        let mut record = RunRecord::new(
            fixture.paths.workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "task".into(),
            worktree,
        );
        record.agent_id = Some("child".into());
        record.status = RunStatus::Running;
        run::create(&fixture.paths, &record).unwrap();
        let plan = inspect_live_scope(
            fixture.paths.clone(),
            fixture.runtime.clone(),
            &fixture.project,
            &MachineConfig::default(),
            false,
            pending(&fixture),
        );
        assert!(!plan.boundary);
        assert!(!plan.is_empty(), "a child-only settlement must still run");
        let preview = plan.preview();
        materialize(plan, RebirthDisposition::Defer, "rimz-test");
        assert_eq!(
            ended_events(&fixture),
            [("rimz.child-not-resumed".into(), "child".into())]
        );
        assert_eq!(preview.candidate_count(), usize::from(with_root));
        assert!(preview.unresumable().is_empty());
        assert_eq!(
            pending(&fixture),
            if with_root {
                BTreeSet::from([key("root")])
            } else {
                BTreeSet::new()
            }
        );
        assert_eq!(
            run::load(&fixture.paths, &record.run_id).unwrap().status,
            RunStatus::Canceled
        );
    }
}

#[test]
fn failed_rebirth_append_still_parks_roster() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let plan = fixture.inspect(false);
    std::fs::remove_file(&fixture.paths.events_log).expect("remove log");
    std::fs::create_dir(&fixture.paths.events_log).expect("block log append");

    materialize(plan, RebirthDisposition::Decline, "rimz-test");

    assert!(!fixture.paths.live_roster.exists());
    assert_eq!(
        pending(&fixture),
        [key("live")].into(),
        "an agent whose ended stamp did not land stays parked"
    );
}

#[test]
fn unattended_birth_with_recovery_off_parks_agents_for_a_later_recovery() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let plan = fixture.inspect(true);

    assert_eq!(plan.preview().pane_count(), 0);
    assert_eq!(plan.preview().candidate_count(), 1);
    let outcome = materialize(plan, RebirthDisposition::Defer, "rimz-test");
    assert!(outcome.tabs.is_empty());
    let marker: LastDeathMarker =
        serde_json::from_slice(&std::fs::read(&fixture.paths.last_death_marker).unwrap())
            .expect("marker");
    assert_eq!(
        marker.recovered,
        Some(0),
        "a deferred agent is not a recovered one"
    );
    assert_eq!(ended_events(&fixture), []);
    assert_eq!(pending(&fixture), [key("live")].into());
    assert!(!fixture.paths.live_roster.exists());
    let events = event_log(&fixture);
    assert!(events.find("session.death").unwrap() < events.find("session.rebirth").unwrap());

    // The headless room's sidebar publishes its own roster before the user returns.
    live_roster::publish(&fixture.paths.live_roster, BTreeSet::new()).expect("new roster");
    let plan = fixture.inspect(false);
    assert_eq!(plan.preview().pane_count(), 1);
    assert_eq!(plan.preview().unresumable(), []);

    let outcome = materialize(plan, RebirthDisposition::RecoverKeep, "rimz-test");

    assert_eq!(outcome.resumed, [key("live")].into());
    assert_eq!(outcome.tabs.len(), 1);
    assert!(pending(&fixture).is_empty());
    assert_eq!(ended_events(&fixture), []);
    let marker: LastDeathMarker =
        serde_json::from_slice(&std::fs::read(&fixture.paths.last_death_marker).unwrap())
            .expect("marker");
    assert_eq!(marker.recovered, Some(1), "a later recovery still counts");
}

#[test]
fn decline_with_recovery_off_ends_candidates() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let plan = fixture.inspect(true);
    assert!(plan.preview().recovery_off());

    let outcome = materialize(plan, RebirthDisposition::Decline, "rimz-test");

    assert!(outcome.tabs.is_empty());
    assert_eq!(
        ended_events(&fixture),
        vec![("rimz.recovery-declined".to_owned(), "live".into())]
    );
    assert!(pending(&fixture).is_empty());
    assert!(!fixture.inspect(false).preview().recovery_off());
}

#[test]
fn settlement_drops_parked_agents_ended_by_other_means() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let closed = dir.path().join("closed");
    let fixture = Fixture::new(&[("live", &live, true), ("closed", &closed, true)]);
    park_roster(&fixture.paths).expect("park before birth");
    record_boundary_at(
        fixture.paths.clone(),
        fixture.runtime.clone(),
        &fixture.paths.workspace_id,
        "rimz-test",
    );
    Store::open(fixture.paths.clone(), fixture.runtime.clone())
        .expect("store")
        .append_event(&crate::EventEnvelope::agent_lifecycle(
            fixture.paths.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionEnd",
            &AgentLifecycleObservation::new(
                Some(AgentSessionId::from("closed")),
                LifecycleSignal::Ended,
            ),
        ))
        .expect("end by other means");
    assert_eq!(pending(&fixture), [key("closed"), key("live")].into());

    materialize(
        fixture.inspect(true),
        RebirthDisposition::Defer,
        "rimz-test",
    );

    assert_eq!(pending(&fixture), [key("live")].into());
}

#[test]
fn settlement_confirms_each_resume_tab_on_its_own() {
    for live in [false, true] {
        for (first_opens, second_opens) in
            [(true, true), (true, false), (false, true), (false, false)]
        {
            let dir = tempfile::tempdir().expect("worktrees");
            let first = dir.path().join("first");
            let second = dir.path().join("second");
            let fixture = Fixture::new(&[("first", &first, true), ("second", &second, true)]);
            let plan = if live {
                materialize(
                    fixture.inspect(true),
                    RebirthDisposition::Defer,
                    "rimz-test",
                );
                inspect_live_at(
                    fixture.paths.clone(),
                    fixture.runtime.clone(),
                    &fixture.project,
                    &MachineConfig::default(),
                    false,
                )
            } else {
                park_roster(&fixture.paths).expect("park before birth");
                fixture.inspect(false)
            };
            let case = format!("live={live} first={first_opens} second={second_opens}");

            let seeded = plan.settle(RebirthDisposition::RecoverKeep, "rimz-test");
            assert_eq!(seeded.tabs().len(), 2, "{case}");
            let outcome = seeded.confirm(|_, tab| {
                assert_eq!(
                    pending(&fixture),
                    [key("first"), key("second")].into(),
                    "a dying opener must leave its candidates parked"
                );
                let opens = if tab.cwd == first {
                    first_opens
                } else {
                    second_opens
                };
                if opens { Ok(()) } else { Err("no tab") }
            });

            let opened = [
                (first_opens, &first, "first"),
                (second_opens, &second, "second"),
            ];
            assert_eq!(
                outcome
                    .tabs
                    .iter()
                    .map(|tab| tab.cwd.as_path())
                    .collect::<BTreeSet<_>>(),
                opened
                    .iter()
                    .filter(|(opens, _, _)| *opens)
                    .map(|(_, cwd, _)| cwd.as_path())
                    .collect(),
                "{case}"
            );
            assert_eq!(
                pending(&fixture),
                opened
                    .iter()
                    .filter(|(opens, _, _)| !*opens)
                    .map(|(_, _, id)| key(id))
                    .collect(),
                "{case}"
            );
            let lost = opened.iter().filter(|(opens, _, _)| !*opens).count();
            assert_eq!(
                outcome.warnings.len(),
                lost,
                "{case}: {:?}",
                outcome.warnings
            );
            for warning in &outcome.warnings {
                assert!(
                    warning.starts_with("could not open resumed tab #")
                        && warning.ends_with(
                            ": no tab; its agents stay pending for a later rebirth or explicit resume"
                        ),
                    "{case}: {warning}"
                );
            }
            assert_eq!(ended_events(&fixture), [], "{case}");
            assert_eq!(recovered_count(&fixture), Some(2 - lost), "{case}");
            assert_eq!(
                auto_resume_assists(&fixture),
                if lost == 2 {
                    vec![]
                } else {
                    vec![(
                        2 - lost,
                        outcome.tabs.iter().map(|tab| tab.label.clone()).collect(),
                    )]
                },
                "{case}"
            );
        }
    }
}

#[test]
fn unconfirmed_team_tab_fails_its_launch_batch_and_reindexes_the_rest() {
    for team_opens in [false, true] {
        let dir = tempfile::tempdir().expect("worktrees");
        let team_worktree = dir.path().join("forge");
        let flat_worktree = dir.path().join("flat");
        let fixture = Fixture::new(&[
            ("planner", &team_worktree, true),
            ("flat", &flat_worktree, true),
        ]);
        fixture.stamp_team("planner", &team_worktree, "forge", "planner", "claude-plan");
        fixture.touch_agent("flat", &flat_worktree);
        let plan = fixture.inspect_with(&team_machine(), false);
        park_roster(&fixture.paths).expect("park before birth");

        let seeded = plan.settle(RebirthDisposition::RecoverKeep, "rimz-test");
        assert_eq!(
            seeded
                .tabs()
                .iter()
                .map(|tab| tab.cwd.as_path())
                .collect::<Vec<_>>(),
            [flat_worktree.as_path(), team_worktree.as_path()]
        );
        // The flat tab opens exactly when the team tab does not.
        let outcome = seeded.confirm(|index, _| {
            if (index == 1) == team_opens {
                Ok(())
            } else {
                Err("no tab")
            }
        });

        let failed_launches = event_log(&fixture).matches("\"failed\"").count();
        if team_opens {
            assert_eq!(outcome.tabs.len(), 1);
            assert_eq!(outcome.tabs[0].cwd, team_worktree);
            assert_eq!(outcome.team_launches.len(), 1);
            assert_eq!(outcome.team_launches[0].tab, 0);
            assert_eq!(failed_launches, 0, "{}", event_log(&fixture));
            assert_eq!(pending(&fixture), [key("flat")].into());
        } else {
            assert_eq!(outcome.tabs.len(), 1);
            assert_eq!(outcome.tabs[0].cwd, flat_worktree);
            assert!(outcome.team_launches.is_empty());
            assert!(failed_launches > 0, "{}", event_log(&fixture));
            assert_eq!(pending(&fixture), [key("planner")].into());
            // The failed seats do not stand in for the agent they were to resume.
            let again = inspect_live_at(
                fixture.paths.clone(),
                fixture.runtime.clone(),
                &fixture.project,
                &team_machine(),
                false,
            );
            assert_eq!(again.planned.resumed_keys(), [key("planner")].into());
        }
        assert_eq!(ended_events(&fixture), []);
    }
}

#[test]
fn boundary_inspection_offers_an_agent_whose_owner_exits_within_the_bound() {
    let dir = tempfile::tempdir().expect("worktrees");
    let exiting = dir.path().join("exiting");
    let fixture = Fixture::new(&[("exiting", &exiting, true)]);
    // `cat` lives until its stdin closes, so the owner exits only on release.
    let mut owner = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("owner process");
    fixture.own("exiting", owner.id());
    assert!(
        matches!(
            owner_liveness(&fixture, "exiting"),
            AgentLiveness::Live { .. }
        ),
        "the inspection's own liveness read sees the owner live"
    );

    let inspection = std::thread::spawn({
        let fixture_paths = (fixture.paths.clone(), fixture.runtime.clone());
        let project = fixture.project.clone();
        move || {
            inspect_at(
                fixture_paths.0,
                fixture_paths.1,
                Some("boot-a".to_owned()),
                Vec::new(),
                &project,
                &MachineConfig::default(),
                false,
                Duration::from_secs(60),
            )
            .expect("inspect")
        }
    });
    let held_from = Instant::now();
    while held_from.elapsed() < Duration::from_millis(300) && !inspection.is_finished() {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !inspection.is_finished(),
        "the inspection returned while the owner was still live"
    );
    drop(owner.stdin.take());
    owner.wait().expect("owner exit");
    let plan = inspection.join().expect("inspection");

    assert_eq!(plan.preview().candidate_count(), 1);
}

#[test]
fn boundary_inspection_classes_an_owner_that_records_its_end_during_the_wait_as_ended() {
    let dir = tempfile::tempdir().expect("worktrees");
    let exiting = dir.path().join("exiting");
    let fixture = Fixture::new(&[("exiting", &exiting, true)]);
    let mut owner = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("owner process");
    fixture.own("exiting", owner.id());

    let inspection = std::thread::spawn({
        let fixture_paths = (fixture.paths.clone(), fixture.runtime.clone());
        let project = fixture.project.clone();
        move || {
            inspect_at(
                fixture_paths.0,
                fixture_paths.1,
                Some("boot-a".to_owned()),
                Vec::new(),
                &project,
                &MachineConfig::default(),
                false,
                Duration::from_secs(60),
            )
            .expect("inspect")
        }
    });
    let held_from = Instant::now();
    while held_from.elapsed() < Duration::from_millis(300) && !inspection.is_finished() {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !inspection.is_finished(),
        "the inspection returned while the owner was still live"
    );
    // The dying provider's own session-end hook lands while the inspection waits.
    let mut end = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("exiting")),
        LifecycleSignal::Ended,
    );
    end.agent_pid = Some(owner.id());
    Store::open(fixture.paths.clone(), fixture.runtime.clone())
        .expect("store")
        .append_event(&crate::EventEnvelope::agent_lifecycle(
            fixture.paths.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionEnd",
            &end,
        ))
        .expect("ended event");
    drop(owner.stdin.take());
    owner.wait().expect("owner exit");
    let plan = inspection.join().expect("inspection");

    assert_eq!(plan.preview().candidate_count(), 0);
    assert_eq!(plan.ended, [key("exiting")].into());
}

#[test]
fn boundary_inspection_stops_waiting_for_a_live_owner_at_the_bound() {
    let dir = tempfile::tempdir().expect("worktrees");
    let running = dir.path().join("running");
    let fixture = Fixture::new(&[("running", &running, true)]);
    fixture.own("running", std::process::id());
    let bound = Duration::from_millis(150);

    let inspection = Instant::now();
    let plan = fixture.inspect_within(&MachineConfig::default(), false, bound);

    assert!(inspection.elapsed() >= bound, "{:?}", inspection.elapsed());
    assert!(
        matches!(
            owner_liveness(&fixture, "running"),
            AgentLiveness::Live { .. }
        ),
        "the bound, not an exit, ended the wait"
    );
    assert_eq!(plan.preview().candidate_count(), 0);
    assert_eq!(
        pending(&fixture),
        BTreeSet::new(),
        "inspection writes nothing"
    );
}

#[test]
fn boundary_inspection_waits_for_no_owner_it_would_not_offer() {
    let dir = tempfile::tempdir().expect("worktrees");
    let lost = dir.path().join("lost");
    let ended = dir.path().join("ended");
    let bystander = dir.path().join("bystander");
    let fixture = Fixture::new(&[
        ("lost", &lost, true),
        ("ended", &ended, true),
        ("bystander", &bystander, true),
    ]);
    // Live owners outside the offer: an ended agent, and one out of scope.
    fixture.own("ended", std::process::id());
    fixture.own("bystander", std::process::id());
    let mut end =
        AgentLifecycleObservation::new(Some(AgentSessionId::from("ended")), LifecycleSignal::Ended);
    end.agent_pid = Some(std::process::id());
    Store::open(fixture.paths.clone(), fixture.runtime.clone())
        .expect("store")
        .append_event(&crate::EventEnvelope::agent_lifecycle(
            fixture.paths.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionEnd",
            &end,
        ))
        .expect("ended event");
    live_roster::publish(
        &fixture.paths.live_roster,
        [key("lost"), key("ended")].into(),
    )
    .expect("roster");
    let bound = Duration::from_secs(60);

    let inspection = Instant::now();
    let plan = fixture.inspect_within(&MachineConfig::default(), false, bound);

    assert!(inspection.elapsed() < bound, "{:?}", inspection.elapsed());
    assert_eq!(plan.preview().candidate_count(), 1);
}

#[test]
fn live_settlement_writes_no_boundary_and_leaves_live_agents_alone() {
    for disposition in [RebirthDisposition::Decline, RebirthDisposition::RecoverKeep] {
        let dir = tempfile::tempdir().expect("worktrees");
        let lost = dir.path().join("lost");
        let alive = dir.path().join("alive");
        let fixture = Fixture::new(&[("lost", &lost, true), ("alive", &alive, true)]);
        park_roster(&fixture.paths).expect("park before birth");
        record_boundary_at(
            fixture.paths.clone(),
            fixture.runtime.clone(),
            &fixture.paths.workspace_id,
            "rimz-test",
        );
        // Resumed by hand since the boundary: its owner process is running.
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("alive")),
            LifecycleSignal::Registered,
        );
        observation.agent_pid = Some(std::process::id());
        Store::open(fixture.paths.clone(), fixture.runtime.clone())
            .expect("store")
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                fixture.paths.workspace_id.clone(),
                "rimz-test",
                "claude",
                "SessionStart",
                &observation,
            ))
            .expect("live owner event");
        live_roster::publish(&fixture.paths.live_roster, [key("alive")].into()).expect("roster");
        let roster_before = std::fs::read(&fixture.paths.live_roster).expect("roster");
        let boot_before = std::fs::read(&fixture.paths.boot_marker).expect("boot marker");

        let plan = inspect_live_at(
            fixture.paths.clone(),
            fixture.runtime.clone(),
            &fixture.project,
            &MachineConfig::default(),
            false,
        );
        assert_eq!(plan.preview().candidate_count(), 1);
        assert_eq!(plan.preview().pane_count(), 1);
        let outcome = materialize(plan, disposition, "rimz-test");

        let recovers = disposition.recovers();
        assert_eq!(outcome.tabs.len(), usize::from(recovers));
        assert_eq!(
            ended_events(&fixture),
            if recovers {
                Vec::new()
            } else {
                vec![("rimz.recovery-declined".to_owned(), "lost".into())]
            }
        );
        assert_eq!(pending(&fixture), [key("alive")].into(), "{disposition:?}");
        let events = event_log(&fixture);
        assert_eq!(events.matches("session.rebirth").count(), 1, "{events}");
        assert!(!events.contains("session.death"), "{events}");
        assert_eq!(
            std::fs::read(&fixture.paths.live_roster).unwrap(),
            roster_before
        );
        assert_eq!(
            std::fs::read(&fixture.paths.boot_marker).unwrap(),
            boot_before
        );
        assert!(!fixture.paths.last_death_marker.exists());
    }
}

#[test]
fn agentless_rebirth_seeds_nothing_whatever_the_retired_channel_record_holds() {
    for record in [
        br#"{"auth":{"name":"auth","created_at":"2026-01-01T00:00:00Z"}}"#.as_slice(),
        b"not json",
    ] {
        for disposition in [RebirthDisposition::Defer, RebirthDisposition::RecoverKeep] {
            let fixture = Fixture::new(&[]);
            let workspace = crate::workspace::WorkspaceResolver::resolve(&fixture.project, None)
                .expect("resolve workspace");
            crate::workspace::record::write(
                &fixture.paths,
                &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
            )
            .expect("workspace record");
            std::fs::write(fixture.paths.retired_channels_record(), record).expect("record");

            let outcome = materialize(fixture.inspect(false), disposition, "rimz-test");

            assert_eq!(outcome, ResumePlan::default(), "{disposition:?}");
        }
    }
}

#[test]
fn rebirth_recovery_globally_orders_fresher_flat_before_team() {
    let dir = tempfile::tempdir().expect("worktrees");
    let team_worktree = dir.path().join("forge");
    let flat_worktree = dir.path().join("flat");
    let fixture = Fixture::new(&[
        ("planner", &team_worktree, true),
        ("flat", &flat_worktree, true),
    ]);
    fixture.stamp_team("planner", &team_worktree, "forge", "planner", "claude-plan");
    fixture.touch_agent("flat", &flat_worktree);
    let plan = fixture.inspect_with(&team_machine(), false);

    assert_eq!(plan.preview().labels()[0], "#flat");
    assert_eq!(
        plan.checkout_roots(),
        BTreeSet::from([flat_worktree.as_path(), team_worktree.as_path()])
    );
    let outcome = materialize(plan, RebirthDisposition::RecoverKeep, "rimz-test");
    assert_eq!(outcome.tabs[0].cwd, flat_worktree);
    assert_eq!(outcome.tabs[1].cwd, team_worktree);
}

#[test]
fn resume_attach_isolation_reaches_launch_caller_and_rebirth() {
    use crate::agents::LaunchParams;
    use crate::harness::ancestry::{CallerIdentity, resolve_launch_caller};
    use crate::store::writer::{AgentLaunchName, AgentLaunchRequest, AgentLaunchScope};

    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let store = Store::open(fixture.paths.clone(), fixture.runtime.clone()).unwrap();
    let kind = AgentKind::new_unchecked("claude");
    let session = AgentSessionId::from("live");
    let batch = store
        .begin_agent_launch_batch(
            &[AgentLaunchRequest {
                login: crate::store::writer::LaunchLogin::RoomDefault,
                kind: kind.clone(),
                agent_id: session.clone(),
                name: AgentLaunchName::Mint,
                launch: LaunchParams {
                    isolation: Some(Isolation::Host),
                    ..Default::default()
                },
                run_id: None,
                prompt: None,
            }],
            AgentLaunchScope {
                session_name: "rimz-test".to_owned(),
                cwd: live,
                branch: None,
                description: None,
            },
        )
        .unwrap();
    let caller = CallerIdentity {
        kind: kind.clone(),
        launch_id: Some(batch.single_identity().unwrap().agent_id.clone()),
        pane_id: None,
        name: None,
        profile: None,
        role: None,
    };
    let projection = store
        .runtime_projection(crate::RuntimeScope::Audit)
        .unwrap();
    assert_eq!(
        resolve_launch_caller(&projection.agents, &caller)
            .unwrap()
            .isolation,
        Some(Isolation::Host)
    );
    assert!(!fixture.inspect(false).preview().requires_sandbox());

    store
        .attach_agent_pane(
            &kind,
            &session,
            caller.launch_id.as_ref(),
            &crate::ids::LoginName::default(),
            "rimz-test",
            &PaneId::from_parts(MuxName::Tmux, "%resumed"),
            crate::pane::RuntimeOwner::new(
                crate::pane::RuntimeOwnerKind::Agent,
                session.as_str(),
                // A dead owner: a live agent is not a recovery candidate.
                u32::MAX,
                None,
            ),
            Some(Isolation::Sandbox),
            Some(Isolation::Sandbox),
            None,
        )
        .unwrap();
    let projection = store
        .runtime_projection(crate::RuntimeScope::Audit)
        .unwrap();
    assert_eq!(
        resolve_launch_caller(&projection.agents, &caller)
            .unwrap()
            .isolation,
        Some(Isolation::Sandbox)
    );
    assert!(fixture.inspect(false).preview().requires_sandbox());
}

#[test]
fn sandbox_requirement_follows_resumed_agents_effective_isolation() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let missing = dir.path().join("missing");
    let fixture = Fixture::new(&[("live", &live, true), ("missing", &missing, false)]);
    let stamp_sandbox = |id: &str| {
        let store = Store::open(fixture.paths.clone(), fixture.runtime.clone()).expect("store");
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(id)),
            LifecycleSignal::Registered,
        );
        observation.launch.isolation = Some(Isolation::Sandbox);
        store
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                fixture.paths.workspace_id.clone(),
                "rimz-test",
                "claude",
                "SessionStart",
                &observation,
            ))
            .expect("isolation event");
    };
    let mut sandbox_machine = MachineConfig::default();
    sandbox_machine.agents.isolation = Isolation::Sandbox;

    assert!(!fixture.inspect(false).preview().requires_sandbox());
    assert!(
        fixture
            .inspect_with(&sandbox_machine, false)
            .preview()
            .requires_sandbox()
    );
    stamp_sandbox("missing");
    assert!(!fixture.inspect(false).preview().requires_sandbox());
    stamp_sandbox("live");
    assert!(fixture.inspect(false).preview().requires_sandbox());
    assert!(!fixture.inspect(true).preview().requires_sandbox());
}

#[test]
fn rebirth_preflights_the_current_profile_default() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let store = Store::open(fixture.paths.clone(), fixture.runtime.clone()).unwrap();
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("live")),
        LifecycleSignal::Registered,
    );
    observation.launch.profile = Some("boxed".to_owned());
    store
        .append_event(&crate::EventEnvelope::agent_lifecycle(
            fixture.paths.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
    let mut machine = MachineConfig::default();
    for (isolation, sandbox) in [("sandbox", true), ("host", false)] {
        machine.agents.profiles.0.insert(
            "boxed".to_owned(),
            toml::from_str(&format!("agent = 'claude'\nisolation = '{isolation}'")).unwrap(),
        );
        assert_eq!(
            fixture
                .inspect_with(&machine, false)
                .preview()
                .requires_sandbox(),
            sandbox
        );
    }
}

#[test]
fn rebirth_recovers_flat_tabs_when_store_is_unavailable() {
    let dir = tempfile::tempdir().expect("worktrees");
    let flat_worktree = dir.path().join("flat");
    let fixture = Fixture::new(&[("flat", &flat_worktree, true)]);
    let plan = fixture.inspect(false);
    assert_eq!(plan.preview().pane_count(), 1);
    std::fs::remove_file(&fixture.paths.events_log).expect("remove event log");
    std::fs::create_dir(&fixture.paths.events_log).expect("block store open");

    let outcome = materialize(plan, RebirthDisposition::RecoverKeep, "rimz-test");

    assert_eq!(outcome.tabs.len(), 1);
    assert_eq!(outcome.tabs[0].cwd, flat_worktree);
    assert!(pending(&fixture).is_empty(), "parking needs no store");
}

#[test]
fn team_recovery_allocates_fresh_role_and_keeps_other_tabs_after_team_failure() {
    let dir = tempfile::tempdir().expect("worktrees");
    let worktree = dir.path().join("forge");
    let flat_worktree = dir.path().join("flat");
    let fixture = Fixture::new(&[("planner", &worktree, true), ("flat", &flat_worktree, true)]);
    fixture.stamp_team("planner", &worktree, "forge", "planner", "claude-plan");
    let machine = team_machine();
    let mut plan = fixture.inspect_with(&machine, false);
    assert_eq!(
        plan.planned
            .entries
            .iter()
            .filter(|entry| matches!(entry, RecoveryEntry::Team(_)))
            .count(),
        1
    );
    assert_eq!(plan.preview().pane_count(), 3);

    let mut broken = plan
        .planned
        .entries
        .iter()
        .find_map(|entry| match entry {
            RecoveryEntry::Team(team) => Some(team.clone()),
            RecoveryEntry::Flat(_) => None,
        })
        .expect("team entry");
    broken.label = "#broken".to_owned();
    broken.team = "broken".to_owned();
    broken.cohort.seeds.clear();
    plan.planned.entries.insert(0, RecoveryEntry::Team(broken));

    let outcome = materialize(plan, RebirthDisposition::RecoverKeep, "rimz-test");

    assert!(outcome.tabs.iter().any(|tab| tab.label == "#forge"));
    assert!(outcome.tabs.iter().any(|tab| tab.cwd == flat_worktree));
    assert!(!outcome.tabs.iter().any(|tab| tab.label == "#broken"));
    let team = outcome
        .tabs
        .iter()
        .find(|tab| tab.label == "#forge")
        .expect("team tab");
    assert_eq!(team.pane_count(), 2);
    let argvs = team
        .layout
        .columns
        .iter()
        .flat_map(|column| column.panes.iter().map(|pane| &pane.argv))
        .collect::<Vec<_>>();
    let decode_request = |argv: &[String]| {
        let payload = argv
            .windows(2)
            .find_map(|pair| (pair[0] == "--request").then_some(pair[1].as_str()))
            .expect("exec request payload");
        crate::harness::launch::decode_exec_request(&argv[3], None, payload)
            .expect("decode exec request")
    };
    let planner = decode_request(argvs[0]);
    assert_eq!(planner.identity.name.as_deref(), Some("planner"));
    assert!(matches!(
        planner.action,
        crate::harness::launch::ExecAction::Resume { ref session_id, .. }
            if session_id == "planner"
    ));
    let coder = decode_request(argvs[1]);
    assert_eq!(coder.identity.params.role.as_deref(), Some("coder"));
    let store = Store::open(fixture.paths.clone(), fixture.runtime.clone()).expect("store");
    let projection = store
        .runtime_projection(crate::RuntimeScope::Audit)
        .expect("projection");
    let coder = projection
        .agents
        .iter()
        .find(|agent| agent.role.as_deref() == Some("coder"))
        .expect("fresh coder identity");
    assert_eq!(coder.kind.as_str(), "codex");
    assert_eq!(coder.team.as_deref(), Some("forge"));
    assert!(coder.agent_id.as_str().starts_with("launch_"));
    let events =
        String::from_utf8_lossy(&std::fs::read(&fixture.paths.events_log).unwrap()).into_owned();
    assert!(
        events.find("session.death").unwrap() < events.find("agent.launched").unwrap()
            && events.find("agent.launched").unwrap() < events.find("session.rebirth").unwrap(),
        "{events}"
    );
}

#[test]
fn failed_team_materialization_keeps_its_resume_seeds_pending_unless_dropped() {
    for disposition in [
        RebirthDisposition::RecoverKeep,
        RebirthDisposition::RecoverDrop,
    ] {
        let dir = tempfile::tempdir().expect("worktrees");
        let worktree = dir.path().join("forge");
        let fixture = Fixture::new(&[("planner", &worktree, true)]);
        fixture.stamp_team("planner", &worktree, "forge", "planner", "claude-plan");
        let mut plan = fixture.inspect_with(&team_machine(), false);
        let team = plan
            .planned
            .entries
            .iter_mut()
            .find_map(|entry| match entry {
                RecoveryEntry::Team(team) => Some(team),
                RecoveryEntry::Flat(_) => None,
            })
            .expect("team entry");
        assert!(matches!(
            team.cohort.seeds.first(),
            Some(CohortSeed::Resume(agent)) if agent.agent_id == "planner"
        ));
        team.cohort.seeds.truncate(1);

        let outcome = materialize(plan, disposition, "rimz-test");

        assert!(outcome.tabs.is_empty());
        if disposition == RebirthDisposition::RecoverKeep {
            assert_eq!(ended_events(&fixture), []);
            assert_eq!(pending(&fixture), [key("planner")].into());
            continue;
        }
        assert_eq!(
            ended_events(&fixture),
            vec![("rimz.not-resumed".to_owned(), "planner".into())]
        );
        assert!(pending(&fixture).is_empty());
        let events = event_log(&fixture);
        assert!(events.find("rimz.not-resumed").unwrap() < events.find("session.rebirth").unwrap());
    }
}

#[test]
fn boundary_without_inspection_parks_roster_and_ends_nobody() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);

    park_roster(&fixture.paths).expect("park before birth");
    record_boundary_at(
        fixture.paths.clone(),
        fixture.runtime.clone(),
        &fixture.paths.workspace_id,
        "rimz-test",
    );

    assert!(!fixture.paths.live_roster.exists());
    assert_eq!(pending(&fixture), [key("live")].into());
    assert_eq!(ended_events(&fixture), []);
    assert!(!fixture.paths.last_death_marker.exists());
    assert!(!fixture.paths.crashes_dir.exists());
    let events = event_log(&fixture);
    assert!(events.contains("session.rebirth"), "{events}");
    assert!(!events.contains("session.death"), "{events}");
    assert!(!events.contains("agent.launched"), "{events}");
}

#[test]
fn crash_archives_remain_until_gc() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let crashes = &paths.crashes_dir;
    for index in 0..7 {
        archive_crash(
            &paths,
            &CrashCacheSnapshot::default(),
            &[],
            Timestamp::from_second(index * 86400).unwrap(),
        )
        .unwrap();
    }

    let mut kept = std::fs::read_dir(crashes)
        .expect("read")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    kept.sort();
    assert_eq!(
        kept,
        vec![
            "19700101T000000Z",
            "19700102T000000Z",
            "19700103T000000Z",
            "19700104T000000Z",
            "19700105T000000Z",
            "19700106T000000Z",
            "19700107T000000Z",
        ]
    );
}

#[cfg(unix)]
#[test]
fn crash_copy_preserves_relative_cache_paths_and_skips_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    let source = cache.join("zellij/session");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(source.join("state.kdl"), "state").expect("state");
    symlink(source.join("state.kdl"), source.join("link.kdl")).expect("symlink");
    let mux_cache = dir.path().join("archive/mux-cache");
    let snapshot = capture_cache_sources(&cache, std::slice::from_ref(&source));

    write_cache_snapshot(&snapshot, &mux_cache).expect("write snapshot");

    let destination = mux_cache.join("zellij/session");
    assert_eq!(destination, mux_cache.join("zellij/session"));
    assert!(destination.join("state.kdl").is_file());
    assert!(!destination.join("link.kdl").exists());
}

#[test]
fn crash_archive_uses_cache_bytes_captured_before_room_birth() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let source = dir.path().join("session-info");
    std::fs::create_dir_all(&source).expect("cache source");
    std::fs::write(source.join("state.kdl"), "crashed").expect("crashed cache");
    let plan = inspect_at(
        fixture.paths.clone(),
        fixture.runtime.clone(),
        Some("boot-a".to_owned()),
        vec![source.clone()],
        &fixture.project,
        &MachineConfig::default(),
        false,
        OWNER_EXIT_BOUND,
    )
    .expect("inspect");

    std::fs::write(source.join("state.kdl"), "reborn").expect("reborn cache");
    materialize(plan, RebirthDisposition::Decline, "rimz-test");

    let archive = std::fs::read_dir(&fixture.paths.crashes_dir)
        .expect("crashes")
        .next()
        .expect("archive")
        .expect("archive entry")
        .path();
    assert_eq!(
        std::fs::read_to_string(archive.join("mux-cache/session-info/state.kdl"))
            .expect("archived cache"),
        "crashed"
    );
}

#[test]
fn boot_helpers_parse_stable_tokens() {
    assert!(boot_changed(None, Some("boot-a")));
    assert!(!boot_changed(Some("boot-a"), Some("boot-a")));
    assert!(boot_changed(Some("boot-a"), Some("boot-b")));
    assert_eq!(
        parse_proc_btime("cpu 1 2\nbtime 1780040667\n"),
        Some("1780040667".to_owned())
    );
    assert_eq!(
        parse_kern_boottime("{ sec = 1780040667, usec = 0 }"),
        Some("1780040667".to_owned())
    );
}

fn team_machine() -> MachineConfig {
    let mut machine = MachineConfig::default();
    machine.agents.profiles.0.insert(
        "claude-plan".to_owned(),
        Profile {
            allowed_tools: None,
            definition_renders: None,
            model_tier: None,
            tier_stamp: None,
            isolation: None,
            auto_compact: None,
            agent: "claude".to_owned(),
            description: None,
            subagents: None,
            model_reminder: None,
            keep_warm: None,
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            skills: None,
            args: None,
        },
    );
    machine.agents.profiles.0.insert(
        "codex-code".to_owned(),
        Profile {
            allowed_tools: None,
            definition_renders: None,
            model_tier: None,
            tier_stamp: None,
            isolation: None,
            auto_compact: None,
            agent: "codex".to_owned(),
            description: None,
            subagents: None,
            model_reminder: None,
            keep_warm: None,
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            skills: None,
            args: None,
        },
    );
    machine.agents.teams.0.insert(
        "forge".to_owned(),
        Team {
            roles: vec![
                RoleBinding {
                    signals: Vec::new(),
                    owns: Vec::new(),
                    flip_compact: None,
                    idle_compact: None,
                    keep_warm: None,
                    auto_compact: None,
                    role: "planner".to_owned(),
                    profile: "claude-plan".to_owned(),
                    mode: None,
                    model: None,
                    effort: None,
                    budget: None,
                    system_prompt_file: None,
                    append_system_prompt_files: Vec::new(),
                    args: None,
                },
                RoleBinding {
                    signals: Vec::new(),
                    owns: Vec::new(),
                    flip_compact: None,
                    idle_compact: None,
                    keep_warm: None,
                    auto_compact: None,
                    role: "coder".to_owned(),
                    profile: "codex-code".to_owned(),
                    mode: None,
                    model: None,
                    effort: None,
                    budget: None,
                    system_prompt_file: None,
                    append_system_prompt_files: Vec::new(),
                    args: None,
                },
            ],
            leader: None,
            layout: Some("planner,coder".to_owned()),
            scratch_files: None,
            consensus_file: None,
            append_system_prompt_files: Vec::new(),
            stages: Vec::new(),
        },
    );
    machine
}
