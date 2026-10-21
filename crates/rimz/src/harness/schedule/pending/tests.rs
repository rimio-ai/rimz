use super::*;
use crate::config::TaskEntry;

#[test]
fn catalog_wait_classes_follow_the_kind_of_work() {
    use PendingWaitClass::{Sleeping, Working};
    let now = "2026-06-01T10:00:00Z[UTC]".parse().unwrap();
    let entries = [
        TaskEntry {
            at: Some("12:00".into()),
            ..TaskEntry::default()
        },
        TaskEntry {
            signal: Some("pr.merged".into()),
            once: Some(true),
            ..TaskEntry::default()
        },
        TaskEntry {
            when: Some(vec!["pr=merged".into()]),
            once: Some(true),
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
            pending_wait("wait", &task, &now).unwrap().class
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
    assert!(has_launched_work(&[parent.clone(), child]));

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
        instance: "forge#work".into(),
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
        instance: "forge#feat-x".into(),
    });
    for run in &runs {
        run::write(&paths.runs_dir, run).unwrap();
    }
    let agents = [vec![parent], children.to_vec(), vec![leader]].concat();
    let mut snapshot = SidebarSnapshot::build_with_agents(paths.workspace_id.clone(), agents, now);
    project_pending_waits(&mut snapshot, &paths, None, &MachineConfig::default());
    assert_eq!(
        snapshot.agents[0].pending_waits.len(),
        3,
        "two fleet rows and one team run"
    );
    assert_eq!(snapshot.agents[0].effective_status(), AgentStatus::Sleeping);
    for wait in &snapshot.agents[0].pending_waits[..2] {
        assert_eq!(wait.armed_at, Some(jiff::Timestamp::UNIX_EPOCH));
        let PendingWaitTrigger::Subagent {
            active_at,
            deadline_at,
            settled,
        } = &wait.trigger
        else {
            panic!("subagent comes before team")
        };
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
    std::fs::write(
        dir.path().join("blackboard.md"),
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
    std::fs::write(dir.path().join("blackboard.md"), "Stage: Done\n").unwrap();
    Arc::make_mut(&mut snapshot.agents)[0].pending_waits.clear();
    project_run_waits(&mut snapshot, &paths);
    assert!(
        snapshot.agents[0]
            .pending_waits
            .iter()
            .all(|wait| !matches!(wait.trigger, PendingWaitTrigger::Team { .. })),
        "a board at Done projects no team wait"
    );
    std::fs::remove_file(dir.path().join("blackboard.md")).unwrap();
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
    project_pending_waits(&mut snapshot, &paths, None, &MachineConfig::default());
    assert!(snapshot.agents[0].pending_waits.is_empty());
    assert_ne!(snapshot.agents[0].effective_status(), AgentStatus::Sleeping);
}

#[test]
fn pending_wait_requires_an_ephemeral_valid_trigger_and_one_shot_clock() {
    let deadline = "2026-06-01T12:00:00Z".parse().unwrap();
    let now = "2026-06-01T10:00:00Z[UTC]".parse().unwrap();
    for entry in [
        TaskEntry {
            signal: Some("pr.merged".into()),
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
        assert!(pending_wait("wait", &task, &now).is_none());
    }
    let task = LoadedTask::new(
        "signal",
        TaskEntry {
            signal: Some("pr.merged".into()),
            once: Some(true),
            ..TaskEntry::default()
        },
        TaskSource::Instance,
    );
    assert_eq!(
        pending_wait("signal", &task, &now).unwrap().trigger,
        PendingWaitTrigger::Signal {
            selector: "pr.merged".into(),
        }
    );
}

#[test]
fn pending_waits_preserve_pid_and_delay() {
    let now = "2026-06-01T10:00:00Z[UTC]".parse().unwrap();
    let armed_at = "2026-06-01T09:42:00Z".parse().unwrap();
    let meta = crate::config::WaitMeta {
        armed_at,
        delay: Some("30m".into()),
        reader: None,
    };
    let timer = LoadedTask::new(
        "timer",
        TaskEntry {
            at: Some("10:12".into()),
            wait_meta: Some(meta.clone()),
            ..TaskEntry::default()
        },
        TaskSource::Instance,
    );
    let wait = pending_wait("timer", &timer, &now).unwrap();
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
        assert_eq!(
            pending_wait("watch", &task, &now).unwrap().trigger,
            expected
        );
    }
}

#[test]
fn clock_due_uses_arm_time_or_the_snapshot_clock() {
    let now = "2026-06-02T10:00:00Z[UTC]".parse().unwrap();
    for (armed_at, expected) in [
        (Some("2026-06-01T10:00:00Z"), "2026-06-01T12:00:00Z"),
        (None, "2026-06-02T12:00:00Z"),
    ] {
        let task = LoadedTask::new(
            "timer",
            TaskEntry {
                at: Some("12:00".into()),
                wait_meta: armed_at.map(|at| crate::config::WaitMeta {
                    armed_at: at.parse().unwrap(),
                    delay: None,
                    reader: None,
                }),
                ..TaskEntry::default()
            },
            TaskSource::Instance,
        );
        assert_eq!(
            pending_wait("timer", &task, &now).unwrap().trigger,
            PendingWaitTrigger::Timer {
                due: expected.parse().unwrap(),
                delay: None,
            },
        );
    }
}

#[test]
fn fire_at_due_is_its_instant_even_when_armed_at_it() {
    let now = "2026-06-02T10:00:00Z[UTC]".parse().unwrap();
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
        pending_wait("reset", &task, &now).unwrap().trigger,
        PendingWaitTrigger::Timer {
            due: at,
            delay: None,
        },
    );
}
