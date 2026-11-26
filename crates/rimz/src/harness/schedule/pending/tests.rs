use super::*;
use crate::config::TaskEntry;

#[test]
fn catalog_wait_classes_follow_the_kind_of_work() {
    use PendingWaitClass::{Sleeping, Working};
    let entries = [
        TaskEntry {
            fire_at: Some("2026-06-01T12:00:00Z".parse().unwrap()),
            ..TaskEntry::default()
        },
        TaskEntry {
            signal: Some("pr.merged".into()),
            ..TaskEntry::default()
        },
        TaskEntry {
            when: Some(vec!["pr=merged".into()]),
            ..TaskEntry::default()
        },
        TaskEntry {
            watch: Some(WatchSpec::File {
                file: "app.log".into(),
                grep: None,
                mark: None,
            }),
            ..TaskEntry::default()
        },
        TaskEntry {
            watch: Some(WatchSpec::Pid { pid: 42 }),
            ..TaskEntry::default()
        },
        TaskEntry {
            watch: Some(WatchSpec::Command("cargo test".into())),
            ..TaskEntry::default()
        },
        TaskEntry {
            watch: Some(WatchSpec::Check {
                check: "true".into(),
                every: "1s".into(),
                on: crate::config::CheckOn::Success,
            }),
            ..TaskEntry::default()
        },
    ];
    let classes: Vec<_> = entries
        .into_iter()
        .map(|entry| {
            let task = LoadedTask::new("wait", entry, TaskSource::Instance);
            pending_wait("wait", &task).unwrap().class
        })
        .collect();
    assert_eq!(
        classes,
        [
            Sleeping, Sleeping, Sleeping, Sleeping, Working, Working, Working
        ]
    );
}

fn projected_child_chain(
    status: AgentStatus,
    trigger: Option<PendingWaitTrigger>,
    shell: bool,
    terminal: bool,
    cycle: bool,
    age_secs: i64,
) -> SidebarSnapshot {
    projected_child_chain_with(status, trigger, shell, terminal, cycle, age_secs, |_| {})
}

fn projected_child_chain_with(
    status: AgentStatus,
    trigger: Option<PendingWaitTrigger>,
    shell: bool,
    terminal: bool,
    cycle: bool,
    age_secs: i64,
    edit: impl FnOnce(&mut [crate::agents::AgentState]),
) -> SidebarSnapshot {
    use crate::agents::{AgentState, BackgroundShell, PermissionMode};
    use crate::store::run::{self, RunRecord, RunStatus};
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    let now = jiff::Timestamp::from_second(4_000).unwrap();
    let mut agents: Vec<_> = ["parent", "child", "grandchild"]
        .into_iter()
        .map(|name| {
            let mut agent = AgentState::stub("claude", name, status);
            agent.name = Some(name.into());
            agent.last_activity = now - jiff::SignedDuration::from_secs(age_secs);
            agent
        })
        .collect();
    for (index, parent) in [(1, 0), (2, 1)].into_iter().chain(cycle.then_some((0, 1))) {
        agents[index].parent_agent_id = Some(agents[parent].agent_id.clone());
        agents[index].parent_agent_kind = Some(agents[parent].kind.clone());
        agents[index].launch_depth = Some(1);
        let mut record = RunRecord::new(
            paths.workspace_id.clone(),
            agents[index].kind.clone(),
            PermissionMode::Auto,
            "task".into(),
            dir.path().into(),
        );
        record.agent_id = Some(agents[index].agent_id.clone());
        if terminal {
            record.status = RunStatus::Completed;
        }
        run::write(&paths.runs_dir, &record).unwrap();
    }
    let mut snapshot = SidebarSnapshot::build_with_agents(paths.workspace_id.clone(), agents, now);
    // Catalog attachment replaces waits, so attach this fixture at the same boundary.
    if let Some(trigger) = trigger {
        Arc::make_mut(&mut snapshot.agents)[2]
            .pending_waits
            .push(PendingWait {
                name: "timer".into(),
                trigger,
                armed_at: Some(now),
                class: PendingWaitClass::Sleeping,
            });
    }
    if shell {
        Arc::make_mut(&mut snapshot.agents)[2]
            .background_shells
            .push(BackgroundShell {
                id: "gate".into(),
                command: None,
                description: None,
                started_at: now,
            });
    }
    edit(Arc::make_mut(&mut snapshot.agents).as_mut_slice());
    project_run_waits(&mut snapshot, &paths);
    snapshot
}

#[test]
fn child_wait_chain_follows_background_shell() {
    let snapshot = projected_child_chain(AgentStatus::Success, None, true, false, false, 0);
    assert_eq!(
        snapshot.agents[0].pending_waits[0].class,
        PendingWaitClass::Working
    );
    assert_eq!(
        snapshot.agents[1].pending_waits[0].class,
        PendingWaitClass::Working
    );
}

#[test]
fn child_wait_chain_follows_an_owed_compaction_continue() {
    use crate::agents::state::SoftCompactMark;
    for (owed, compacting, expected) in [
        (Some(true), true, PendingWaitClass::Working),
        (Some(true), false, PendingWaitClass::Working),
        (Some(false), true, PendingWaitClass::Inert),
        (None, true, PendingWaitClass::Inert),
    ] {
        let snapshot = projected_child_chain_with(
            AgentStatus::Success,
            None,
            false,
            false,
            false,
            0,
            |agents| {
                let child = &mut agents[1];
                child.compacting_since = compacting.then_some(child.last_activity);
                let queued_at = child.last_activity;
                child.soft_compact = owed.map(|owed| SoftCompactMark {
                    compactions: Some(0),
                    continue_owed: owed.then(|| crate::agents::state::OwedContinue {
                        message_id: crate::ids::MessageId::new(),
                        queued_at,
                        past_stop: false,
                    }),
                    ..SoftCompactMark::default()
                });
            },
        );
        assert_eq!(
            snapshot.agents[0].pending_waits[0].class, expected,
            "{owed:?} compacting={compacting}"
        );
    }
}

#[test]
fn child_wait_chain_follows_timer() {
    let snapshot = projected_child_chain(
        AgentStatus::Success,
        Some(PendingWaitTrigger::Timer {
            due: jiff::Timestamp::from_second(5_000).unwrap(),
            delay: None,
        }),
        false,
        false,
        false,
        0,
    );
    assert_eq!(
        snapshot.agents[0].pending_waits[0].class,
        PendingWaitClass::Sleeping
    );
}

#[test]
fn settled_child_wait_is_inert_even_with_a_shell() {
    let snapshot = projected_child_chain(AgentStatus::Success, None, true, true, false, 0);
    assert_eq!(
        snapshot.agents[0].pending_waits[0].class,
        PendingWaitClass::Inert
    );
}

#[test]
fn cyclic_child_waits_are_inert() {
    let snapshot = projected_child_chain(AgentStatus::Success, None, false, false, true, 0);
    assert!(
        snapshot
            .agents
            .iter()
            .flat_map(|agent| &agent.pending_waits)
            .all(|wait| wait.class == PendingWaitClass::Inert)
    );
}

#[test]
fn child_wait_follows_live_turn_but_not_silent_turn() {
    let snapshot = projected_child_chain(AgentStatus::Running, None, false, false, false, 0);
    assert_eq!(
        snapshot.agents[0].pending_waits[0].class,
        PendingWaitClass::Working
    );
    let silent = projected_child_chain(AgentStatus::Running, None, false, false, false, 3_600);
    assert_eq!(
        silent.agents[0].pending_waits[0].class,
        PendingWaitClass::Inert
    );
}

#[test]
fn runs_are_read_only_when_an_agent_launched_children_or_a_team() {
    use crate::agents::{AgentState, AgentStatus, LaunchedBy};
    let parent = AgentState::stub("claude", "parent", AgentStatus::Idle);
    let mut peer = AgentState::stub("claude", "peer", AgentStatus::Idle);
    assert!(!has_launched_work(&[parent.clone(), peer.clone()]));

    let mut child = AgentState::stub("claude", "child", AgentStatus::Idle);
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    assert!(has_launched_work(&[parent.clone(), child.clone()]));
    // A child stamped with its parent's launch keeps it after the parent molts to another kind.
    let mut molted = AgentState::stub("codex", "molted", AgentStatus::Idle);
    molted.launch_id = Some("parent-launch".into());
    child.parent_agent_id = molted.launch_id.clone();
    assert!(has_launched_work(&[molted, child]));

    peer.team = Some("forge".into());
    assert!(
        !has_launched_work(&[parent.clone(), peer.clone()]),
        "a team seat nobody launched is not launched work"
    );
    peer.launched_by = Some(LaunchedBy {
        kind: parent.kind.clone(),
        agent_id: parent.agent_id.clone(),
    });
    assert!(has_launched_work(&[parent, peer]));
}

#[test]
fn run_waits_keep_launcher_member_and_turn_order() {
    use crate::agents::{AgentState, AgentStatus, LaunchedBy, PermissionMode};
    use crate::store::run::{self, PeerRun, ReportTo, RunRecord, RunStatus, TeamRun};
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    let now = jiff::Timestamp::from_second(4_000).unwrap();
    let mut first = AgentState::stub("claude", "first", AgentStatus::Idle);
    first.launch_id = Some("first-launch".into());
    let second = AgentState::stub("codex", "second", AgentStatus::Idle);
    let mut children: Vec<_> = ["plain", "peer", "other", "leader"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let launcher = if index >= 2 { &second } else { &first };
            let mut child = AgentState::stub(launcher.kind.as_str(), name, AgentStatus::Idle);
            child.name = Some(name.into());
            child.registered_at = Some(jiff::Timestamp::from_second(index as i64).unwrap());
            child.launched_by = Some(LaunchedBy {
                kind: launcher.kind.clone(),
                agent_id: launcher
                    .launch_id
                    .as_ref()
                    .unwrap_or(&launcher.agent_id)
                    .clone(),
            });
            child
        })
        .collect();
    children[1].launch_id = Some("peer-launch".into());
    children[3].launch_id = Some("leader-launch".into());
    children[3].team = Some("forge".into());
    let make_run = |child: &AgentState, started| {
        let mut record = RunRecord::new(
            paths.workspace_id.clone(),
            child.kind.clone(),
            PermissionMode::Auto,
            "task".into(),
            dir.path().into(),
        );
        record.agent_id = Some(child.agent_id.clone());
        record.started_at = jiff::Timestamp::from_second(started).unwrap();
        record.updated_at = record.started_at;
        record
    };
    let mut runs = vec![
        make_run(&children[0], 1),
        make_run(&children[0], 2),
        make_run(&children[1], 3),
        make_run(&children[1], 4),
        make_run(&children[2], 5),
        make_run(&children[3], 6),
    ];
    runs[0].status = RunStatus::Completed;
    runs[1].deadline_at = Some(now);
    runs[2].peer = Some(PeerRun {
        launch_id: "peer-launch".into(),
        opened_by: Vec::new(),
    });
    runs[2].agent_id = None;
    runs[3].peer = runs[2].peer.clone();
    runs[3].status = RunStatus::Completed;
    runs[4].agent_id = None;
    runs[4].agent_name = children[2].name.clone();
    runs[5].team = Some(TeamRun {
        launch_id: "leader-launch".into(),
        instance: "forge#work".parse().unwrap(),
    });
    let mut detached = make_run(&children[1], 7);
    detached.peer = runs[2].peer.clone();
    detached.report_to = ReportTo::Nobody;
    runs.push(detached);
    for run in &runs {
        run::write(&paths.runs_dir, run).unwrap();
    }
    let mut agents = vec![first, second];
    agents.extend(children);
    let mut snapshot = SidebarSnapshot::build_with_agents(paths.workspace_id.clone(), agents, now);
    project_run_waits(&mut snapshot, &paths);
    let first = &snapshot.agents[0].pending_waits;
    assert_eq!(
        first
            .iter()
            .map(|wait| wait.name.as_str())
            .collect::<Vec<_>>(),
        ["@plain", "@peer", "@peer"]
    );
    assert_eq!(
        first
            .iter()
            .map(|wait| wait.armed_at.unwrap().as_second())
            .collect::<Vec<_>>(),
        [2, 4, 3]
    );
    let second = &snapshot.agents[1].pending_waits;
    assert_eq!(
        second
            .iter()
            .map(|wait| wait.name.as_str())
            .collect::<Vec<_>>(),
        ["@other", "forge#work"]
    );
    assert!(matches!(second[1].trigger, PendingWaitTrigger::Team { .. }));
    assert!(
        snapshot.agents[2..]
            .iter()
            .all(|agent| agent.pending_waits.is_empty())
    );
}

/// A peer the launcher awaits has no parent link, yet its wait names its row as a
/// child's does; a wait recorded before the field parses without one.
#[test]
fn a_peer_wait_names_the_awaited_row() {
    use crate::agents::{AgentState, AgentStatus, PendingWaitTarget, PermissionMode};
    use crate::store::run::{self, RunRecord};
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    let now = jiff::Timestamp::from_second(4_000).unwrap();
    let launcher = AgentState::stub("claude", "launcher", AgentStatus::Idle);
    let mut peer = AgentState::stub("codex", "peer", AgentStatus::Running);
    peer.name = Some("tidy-iris".into());
    peer.launched_by = Some(crate::agents::LaunchedBy {
        kind: launcher.kind.clone(),
        agent_id: launcher.agent_id.clone(),
    });
    let mut record = RunRecord::new(
        paths.workspace_id.clone(),
        peer.kind.clone(),
        PermissionMode::Auto,
        "work".into(),
        dir.path().into(),
    );
    record.agent_id = Some(peer.agent_id.clone());
    run::write(&paths.runs_dir, &record).unwrap();
    let expected = PendingWaitTarget {
        kind: peer.kind.clone(),
        agent_id: peer.agent_id.clone(),
    };
    let mut snapshot =
        SidebarSnapshot::build_with_agents(paths.workspace_id.clone(), vec![launcher, peer], now);
    project_run_waits(&mut snapshot, &paths);
    let [wait] = &snapshot.agents[0].pending_waits[..] else {
        panic!("one peer wait: {:?}", snapshot.agents[0].pending_waits)
    };
    let PendingWaitTrigger::Subagent { target, .. } = &wait.trigger else {
        panic!("a peer turn is a subagent wait")
    };
    assert_eq!(target.as_ref(), Some(&expected));
    let older: PendingWaitTrigger =
        serde_json::from_str(r#"{"kind":"subagent","active_at":"2026-11-16T00:00:00Z"}"#).unwrap();
    assert!(matches!(
        older,
        PendingWaitTrigger::Subagent { target: None, .. }
    ));
}

#[test]
fn launched_run_waits_precede_catalog_and_disappear_after_settlement() {
    use crate::agents::{AgentState, AgentStatus, PermissionMode};
    use crate::store::run::{self, RunRecord, RunStatus, TeamRun};
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    let now = jiff::Timestamp::from_second(4_000).unwrap();
    let parent = AgentState::stub("claude", "parent", AgentStatus::Idle);
    let children = ["calm-fox", "bright-owl"].map(|name| {
        let mut child = AgentState::stub("claude", name, AgentStatus::Idle);
        child.name = Some(name.into());
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child.last_activity = now;
        child
    });
    let mut leader = AgentState::stub("claude", "leader", AgentStatus::Idle);
    leader.team = Some("forge".into());
    leader.launch_id = Some("leader-launch".into());
    leader.launched_by = Some(crate::agents::LaunchedBy {
        kind: parent.kind.clone(),
        agent_id: parent.agent_id.clone(),
    });
    let mut runs: Vec<_> = children
        .iter()
        .chain([&leader])
        .map(|child| {
            let mut run = RunRecord::new(
                paths.workspace_id.clone(),
                child.kind.clone(),
                PermissionMode::Auto,
                "work".into(),
                dir.path().into(),
            );
            run.agent_id = Some(child.agent_id.clone());
            run.started_at = jiff::Timestamp::UNIX_EPOCH;
            run.deadline_at = Some(now);
            run
        })
        .collect();
    runs[1].status = RunStatus::Completed;
    runs[2].team = Some(TeamRun {
        launch_id: "leader-launch".into(),
        instance: "forge#feat-x".parse().unwrap(),
    });
    for run in &runs {
        run::write(&paths.runs_dir, run).unwrap();
    }
    let agents = [vec![parent], children.to_vec(), vec![leader]].concat();
    let mut snapshot = SidebarSnapshot::build_with_agents(paths.workspace_id.clone(), agents, now);
    project_pending_waits(&mut snapshot, &paths, None);
    assert_eq!(
        snapshot.agents[0].pending_waits.len(),
        3,
        "two fleet rows and one team run"
    );
    assert_eq!(
        snapshot.agents[0].effective_status(snapshot.now),
        AgentStatus::Sleeping
    );
    for wait in &snapshot.agents[0].pending_waits[..2] {
        assert_eq!(wait.armed_at, Some(jiff::Timestamp::UNIX_EPOCH));
        let PendingWaitTrigger::Subagent {
            active_at,
            target,
            deadline_at,
            settled,
        } = &wait.trigger
        else {
            panic!("subagent comes before team")
        };
        let child = children
            .iter()
            .find(|child| wait.name == format!("@{}", child.agent_id))
            .unwrap();
        assert_eq!(
            *target,
            Some(crate::agents::PendingWaitTarget {
                kind: child.kind.clone(),
                agent_id: child.agent_id.clone(),
            })
        );
        assert_eq!(*active_at, now);
        assert_eq!(*deadline_at, Some(now));
        assert_eq!(
            settled.as_deref(),
            (wait.name == "@bright-owl").then_some("completed")
        );
        assert!(["@calm-fox", "@bright-owl"].contains(&wait.name.as_str()));
    }
    assert_eq!(
        snapshot.agents[0].pending_waits[2].trigger,
        PendingWaitTrigger::Team { stage: None }
    );
    std::fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
    std::fs::write(
        dir.path().join(".rimz/team/blackboard.md"),
        "Stage: Review (@reviewer)\n",
    )
    .unwrap();
    let catalog_wait = PendingWait {
        class: PendingWaitClass::Sleeping,
        name: "ci".into(),
        trigger: PendingWaitTrigger::Signal {
            selector: "pr.checks".into(),
        },
        armed_at: None,
    };
    Arc::make_mut(&mut snapshot.agents)[0].pending_waits = vec![catalog_wait.clone()];
    project_run_waits(&mut snapshot, &paths);
    assert_eq!(
        snapshot.agents[0].pending_waits[2].trigger,
        PendingWaitTrigger::Team {
            stage: Some("Review".into())
        }
    );
    assert_eq!(snapshot.agents[0].pending_waits[3], catalog_wait);
    std::fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
    std::fs::write(dir.path().join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
    Arc::make_mut(&mut snapshot.agents)[0].pending_waits.clear();
    project_run_waits(&mut snapshot, &paths);
    assert!(
        snapshot.agents[0]
            .pending_waits
            .iter()
            .all(|wait| !matches!(wait.trigger, PendingWaitTrigger::Team { .. })),
        "a board at Done projects no team wait"
    );
    std::fs::remove_file(dir.path().join(".rimz/team/blackboard.md")).unwrap();
    for run in &mut runs {
        run.report_to = run::ReportTo::Nobody;
        run::write(&paths.runs_dir, run).unwrap();
    }
    Arc::make_mut(&mut snapshot.agents)[0].pending_waits.clear();
    project_run_waits(&mut snapshot, &paths);
    assert_eq!(
        snapshot.agents[0].pending_waits,
        Vec::new(),
        "detached launches leave their launcher waiting on nothing"
    );
    for run in &mut runs {
        run.report_to = run::ReportTo::Launcher;
    }
    for (index, run) in runs.iter_mut().enumerate() {
        run.status = RunStatus::Completed;
        if index == 0 {
            run.joined_at = Some(now);
        } else {
            run.report_message_id = Some(crate::MessageId::new());
        }
        run::write(&paths.runs_dir, run).unwrap();
    }
    project_pending_waits(&mut snapshot, &paths, None);
    assert!(snapshot.agents[0].pending_waits.is_empty());
    assert_ne!(
        snapshot.agents[0].effective_status(snapshot.now),
        AgentStatus::Sleeping
    );
}

#[test]
fn pending_wait_requires_an_ephemeral_valid_trigger_and_one_shot_clock() {
    let deadline = "2026-06-01T12:00:00Z".parse().unwrap();
    for entry in [
        TaskEntry {
            signal: Some("pr.merged".into()),
            repeat: true,
            ..TaskEntry::default()
        },
        TaskEntry {
            watch: Some(crate::config::WatchSpec::Command(String::new())),
            ..TaskEntry::default()
        },
        TaskEntry {
            every: Some("1h".into()),
            deadline: Some(deadline),
            ..TaskEntry::default()
        },
    ] {
        let task = LoadedTask::new("wait", entry, TaskSource::Instance);
        assert!(pending_wait("wait", &task).is_none());
    }
    let task = LoadedTask::new(
        "signal",
        TaskEntry {
            signal: Some("pr.merged".into()),
            ..TaskEntry::default()
        },
        TaskSource::Instance,
    );
    assert_eq!(
        pending_wait("signal", &task).unwrap().trigger,
        PendingWaitTrigger::Signal {
            selector: "pr.merged".into(),
        }
    );
}

#[test]
fn pending_waits_preserve_pid_and_delay() {
    let armed_at = "2026-06-01T09:42:00Z".parse().unwrap();
    let meta = crate::config::WaitMeta {
        armed_at,
        delay: Some("30m".into()),
        reader: None,
    };
    let timer = LoadedTask::new(
        "timer",
        TaskEntry {
            fire_at: Some("2026-06-01T10:12:00Z".parse().unwrap()),
            wait_meta: Some(meta.clone()),
            ..TaskEntry::default()
        },
        TaskSource::Instance,
    );
    let wait = pending_wait("timer", &timer).unwrap();
    assert_eq!(wait.armed_at, Some(armed_at));
    assert_eq!(
        wait.trigger,
        PendingWaitTrigger::Timer {
            due: "2026-06-01T10:12:00Z".parse().unwrap(),
            delay: Some("30m".into()),
        }
    );
    for (spec, expected) in [
        (
            WatchSpec::Command("cargo test".into()),
            PendingWaitTrigger::Command {
                command: "cargo test".into(),
            },
        ),
        (
            WatchSpec::Pid { pid: 16776 },
            PendingWaitTrigger::Pid { pid: 16776 },
        ),
        (
            WatchSpec::Check {
                check: "nc -z localhost 3000".into(),
                every: "1s".into(),
                on: crate::config::CheckOn::Success,
            },
            PendingWaitTrigger::Check {
                command: "nc -z localhost 3000".into(),
            },
        ),
    ] {
        let task = LoadedTask::new(
            "watch",
            TaskEntry {
                watch: Some(spec),
                wait_meta: Some(crate::config::WaitMeta {
                    delay: None,
                    ..meta.clone()
                }),
                ..TaskEntry::default()
            },
            TaskSource::Instance,
        );
        assert_eq!(pending_wait("watch", &task).unwrap().trigger, expected);
    }
}

#[test]
fn fire_at_due_is_its_instant_even_when_armed_at_it() {
    let at: jiff::Timestamp = "2026-06-02T10:00:00Z".parse().unwrap();
    let task = LoadedTask::new(
        "reset",
        TaskEntry {
            fire_at: Some(at),
            wait_meta: Some(crate::config::WaitMeta {
                armed_at: at,
                delay: None,
                reader: None,
            }),
            ..TaskEntry::default()
        },
        TaskSource::Instance,
    );
    assert_eq!(
        pending_wait("reset", &task).unwrap().trigger,
        PendingWaitTrigger::Timer {
            due: at,
            delay: None,
        },
    );
}

/// A swap from `old` to `new` within one launch, with `new` running as `kind`.
fn swapped(kind: &str) -> [AgentState; 2] {
    let at = jiff::Timestamp::UNIX_EPOCH;
    let mut old = crate::testkit::agent_state("claude", "old-session", at);
    old.launch_id = Some("launch-one".into());
    old.ended_at = Some(at);
    let mut new = crate::testkit::agent_state(kind, "new-session", at);
    new.launch_id = Some("launch-one".into());
    new.epoch = 1;
    [old, new]
}

fn target(kind: &str, session: &str) -> crate::config::TaskTarget {
    crate::config::TaskTarget {
        kind: AgentKind::new_unchecked(kind),
        session: session.into(),
        handle: "@planner".into(),
    }
}

#[test]
fn a_launch_target_follows_every_swap_and_a_session_target_only_one_within_its_kind() {
    for kind in ["claude", "codex"] {
        let rows = swapped(kind);
        let [old, new] = &rows;
        let claude = AgentKind::new_unchecked("claude");
        let holder = |session| target_holder(rows.iter(), &target("claude", session));
        assert_eq!(holder("launch-one"), Some(new), "{kind}");
        assert!(wait_for(&claude, &"launch-one".into(), new));
        assert!(wait_for(&claude, &"old-session".into(), old));
        assert!(!wait_for(&claude, &"old-session".into(), new));
        let molted = kind != "claude";
        assert_eq!(holder("old-session"), (!molted).then_some(new), "{kind}");
        let rows = rows.iter().collect::<Vec<_>>();
        let legacy = target("claude", "old-session");
        assert_eq!(wait_of(&rows, &legacy, new), !molted, "{kind}");
        assert_eq!(
            wait_of(&rows, &legacy, old),
            molted,
            "{kind}: the holder's alone"
        );
        assert!(wait_of(&rows, &target("claude", "launch-one"), new));
    }
}

#[test]
fn take_waits_takes_the_launch_key_and_the_holder_session_but_not_a_replaced_session() {
    let [_, new] = swapped("claude");
    let wait = |name: &str| PendingWait {
        name: name.into(),
        trigger: PendingWaitTrigger::Timer {
            due: jiff::Timestamp::UNIX_EPOCH,
            delay: None,
        },
        armed_at: None,
        class: PendingWaitClass::Sleeping,
    };
    let mut waits = BTreeMap::new();
    for (session, name) in [
        ("launch-one", "by-launch"),
        ("new-session", "by-session"),
        ("old-session", "replaced"),
    ] {
        waits.insert(
            (AgentKind::new_unchecked("claude"), session.into()),
            vec![wait(name)],
        );
    }
    let mut taken = take_waits(&mut waits, &new)
        .into_iter()
        .map(|wait| wait.name)
        .collect::<Vec<_>>();
    taken.sort();
    assert_eq!(taken, ["by-launch", "by-session"]);
    assert_eq!(waits.len(), 1);
}

#[test]
fn same_target_keys_a_launch_across_kinds_and_a_session_within_its_kind() {
    let rows = swapped("claude");
    let rows = rows.iter().collect::<Vec<_>>();
    let same =
        |a: (&str, &str), b: (&str, &str)| same_target(&rows, &target(a.0, a.1), &target(b.0, b.1));
    assert!(same(("claude", "launch-one"), ("codex", "launch-one")));
    assert!(same(("claude", "old-session"), ("claude", "launch-one")));
    assert!(same(("claude", "old-session"), ("claude", "new-session")));
    assert!(!same(("codex", "old-session"), ("claude", "old-session")));
    assert!(!same_target(
        &[],
        &target("plugin-a", "shared"),
        &target("plugin-b", "shared")
    ));
    let molted = swapped("codex");
    let molted = molted.iter().collect::<Vec<_>>();
    assert!(
        !same_target(
            &molted,
            &target("claude", "old-session"),
            &target("claude", "launch-one")
        ),
        "an older session target follows no swap across kinds"
    );
}

#[test]
fn an_older_session_wait_follows_its_holder_within_its_kind_only() {
    for kind in ["claude", "codex"] {
        let rows = swapped(kind);
        let [old, new] = &rows;
        let mut waits = SessionWaits {
            waits: BTreeMap::new(),
            paths: None,
        };
        waits.waits.insert(
            (AgentKind::new_unchecked("claude"), "old-session".into()),
            vec![PendingWait {
                name: "older".into(),
                trigger: PendingWaitTrigger::Timer {
                    due: jiff::Timestamp::UNIX_EPOCH,
                    delay: None,
                },
                armed_at: None,
                class: PendingWaitClass::Sleeping,
            }],
        );
        let mut waits = waits.following(&[old, new]).waits;
        let taken = take_waits(&mut waits, new).len();
        let molted = kind != "claude";
        assert_eq!(taken, usize::from(!molted), "{kind}: the holder's");
        assert_eq!(
            take_waits(&mut waits, old).len(),
            usize::from(molted),
            "{kind}"
        );
    }
}

/// A Runtime snapshot drops the replaced session's row, so an older wait on
/// that session is placed through the room's Audit rows.
#[test]
fn an_older_session_wait_follows_its_holder_past_a_runtime_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, store) = super::super::tests::swapped_store(dir.path());
    let holder = store
        .audit_rows()
        .unwrap()
        .iter()
        .find(|agent| agent.agent_id == "s2")
        .cloned()
        .unwrap();
    let mut waits = SessionWaits {
        waits: BTreeMap::new(),
        paths: Some(paths),
    };
    waits.waits.insert(
        (AgentKind::new_unchecked("claude"), "s1".into()),
        vec![PendingWait {
            name: "older".into(),
            trigger: PendingWaitTrigger::Timer {
                due: jiff::Timestamp::UNIX_EPOCH,
                delay: None,
            },
            armed_at: None,
            class: PendingWaitClass::Sleeping,
        }],
    );
    let mut waits = waits.following(&[&holder]).waits;
    assert_eq!(take_waits(&mut waits, &holder).len(), 1);
}

#[test]
fn a_run_an_agent_started_shows_on_its_card_until_its_report_is_sent() {
    use crate::agents::{AgentState, AgentStatus};
    let now = jiff::Timestamp::from_second(4_000).unwrap();
    let mut planner = AgentState::stub("claude", "planner", AgentStatus::Idle);
    planner.launch_id = Some("launch_planner".into());
    let other = AgentState::stub("claude", "other", AgentStatus::Idle);
    let record = |launch: &str, state: &str, report: &str| -> crate::store::workflow::RunRecord {
        serde_json::from_value(serde_json::json!({
            "version":crate::store::workflow::VERSION, "id":crate::ids::WorkflowRunId::new(),
            "workflow":"review", "spec_dir":"/definitions/review", "prompt":"Review",
            "scope":{"kind":"worktree","path":"/project"}, "project_root":"/project",
            "created_at":"1970-01-01T00:00:00Z", "state":state, "park":null, "node":"pr",
            "entered_at":"1970-01-01T00:00:00Z", "input":null, "key":null, "recheck_at":null,
            "wait":null, "visits":{}, "key_visits":{}, "deadline":null,
            "deadline_fired":false, "next_seq":1, "attempt":null, "prev_attempt":null,
            "terminal_at":null, "starter":{"by":"agent", "kind":"claude", "launch":launch},
            "depth":1, "loop_report":report,
        }))
        .unwrap()
    };
    let records = [
        record("launch_planner", "active", "owed"),
        record("launch_planner", "done", "sent"),
        record("launch_planner", "active", "none"),
    ];
    let mut snapshot = SidebarSnapshot::build_with_agents(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
        vec![planner, other],
        now,
    );
    project_workflow_waits(&mut snapshot, &records);
    assert_eq!(
        snapshot.agents[0].pending_waits,
        vec![PendingWait {
            class: PendingWaitClass::Working,
            name: records[0].id.to_string(),
            armed_at: Some(jiff::Timestamp::UNIX_EPOCH),
            trigger: PendingWaitTrigger::Workflow {
                workflow: "review".into()
            },
        }]
    );
    assert_eq!(
        snapshot.agents[0].pending_waits[0].label(now),
        "wakes when workflow review ends"
    );
    assert!(snapshot.agents[1].pending_waits.is_empty());
}
