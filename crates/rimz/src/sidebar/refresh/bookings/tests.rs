use std::path::{Path, PathBuf};

use super::*;
use crate::agents::{PermissionMode, WorkflowAttemptRef, WorkflowAttemptRole};
use crate::config::WorktreeConfig;
use crate::config::{TaskEntry, TaskTarget, Team};
use crate::harness::schedule::attention::{Attention, BlockingTerm};
use crate::harness::schedule::task_record::FireMark;
use crate::ids::{AgentKind, FireId, TaskId, WorkspaceId};

fn link(id: &str) -> LaunchedBy {
    LaunchedBy {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: id.into(),
    }
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn booked(root: &Path) -> TaskEntry {
    TaskEntry {
        kind: Some(crate::config::LaunchKind::Agent),
        target: Some("coder".to_owned()),
        args: argv(&["go"]),
        when: Some(vec!["pr[feature]=merged".to_owned()]),
        root: root.to_path_buf(),
        booked_by: vec![link("booker"), link("root")],
        ..TaskEntry::default()
    }
}

/// A booked row that launches `words`: an agent or team row when they are a `rimz agents` or
/// `rimz teams` launch, else a shell row running them.
fn launching(words: &[&str], root: &Path) -> TaskEntry {
    use crate::config::LaunchKind;
    let (kind, rest) = match words {
        ["rimz", "agents", rest @ ..] => (LaunchKind::Agent, rest),
        ["rimz", "teams", "launch", rest @ ..] | ["rimz", "teams", rest @ ..] => {
            (LaunchKind::Team, rest)
        }
        _ => {
            return TaskEntry {
                kind: None,
                target: None,
                args: argv(words),
                timeout: Some("1h".to_owned()),
                ..booked(root)
            };
        }
    };
    TaskEntry {
        kind: Some(kind),
        target: rest.first().map(|target| (*target).to_owned()),
        args: argv(rest.get(1..).unwrap_or_default()),
        ..booked(root)
    }
}

fn reading() -> TaskReading {
    TaskReading {
        attention: None,
        fired: false,
        blocking: None,
        unmet: Vec::new(),
        launch: None,
    }
}

fn worktrees() -> &'static WorktreeConfig {
    static CONFIG: std::sync::OnceLock<WorktreeConfig> = std::sync::OnceLock::new();
    CONFIG.get_or_init(WorktreeConfig::default)
}

fn records(runtime: &RuntimePaths) -> RoomRecords<'static> {
    RoomRecords {
        source: CiSource::read(runtime),
        workflows: Vec::new(),
        runs: Vec::new(),
        commands: BTreeMap::new(),
        rows: &[],
        worktrees: worktrees(),
    }
}

fn launch(target: &str, worktree: Option<&str>) -> FireLaunch {
    FireLaunch {
        team: false,
        target: target.to_owned(),
        worktree: worktree.map(|name| LaunchTree::Named(name.to_owned())),
        detach: false,
    }
}

fn room(dir: &Path) -> (PathBuf, RuntimePaths) {
    let root = dir.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(&root), dir).unwrap();
    runtime.ensure_dirs().unwrap();
    (root, runtime)
}

#[test]
fn only_agent_booked_one_off_launch_commands_are_bookings() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let config = |entry: TaskEntry| (entry, TaskSource::Config);
    let rows = [
        ("booked", config(booked(&root))),
        (
            "timed",
            config(TaskEntry {
                when: None,
                fire_at: Some("2026-11-13T18:00:00Z".parse().unwrap()),
                ..booked(&root)
            }),
        ),
        (
            "invalid-trigger",
            config(TaskEntry {
                when: Some(vec!["pr[=".to_owned()]),
                ..booked(&root)
            }),
        ),
        (
            "invalid-action",
            config(TaskEntry {
                timeout: None,
                ..booked(&root)
            }),
        ),
        (
            "detached",
            config(launching(
                &["rimz", "agents", "coder", "--detach", "go"],
                &root,
            )),
        ),
        (
            "recurring",
            config(TaskEntry {
                when: None,
                every: Some("1h".to_owned()),
                each_worktree: true,
                ..booked(&root)
            }),
        ),
        // A command that is no launch a person would type reads as no booking.
        (
            "in-a-shell",
            config(launching(&["sh", "-c", "rimz agents coder go"], &root)),
        ),
        (
            "a-listing",
            config(launching(&["rimz", "agents", "list"], &root)),
        ),
        (
            "unknown-flag",
            config(launching(
                &["rimz", "agents", "coder", "--wat", "go"],
                &root,
            )),
        ),
        (
            "from-a-shell",
            config(TaskEntry {
                booked_by: Vec::new(),
                ..booked(&root)
            }),
        ),
        (
            "delivery",
            config(TaskEntry {
                wait: Some(TaskTarget::default()),
                ..booked(&root)
            }),
        ),
        // The room's state tier keeps every row a lifetime makes ephemeral, a timed
        // `--in` booking included; its wake-me and session-pinned rows carry `wait`.
        (
            "room-timed",
            (
                TaskEntry {
                    when: None,
                    fire_at: Some("2026-11-13T18:00:00Z".parse().unwrap()),
                    ..launching(
                        &["rimz", "agents", "coder", "-w", "booked-launch", "go"],
                        &root,
                    )
                },
                TaskSource::Instance,
            ),
        ),
        ("room-condition", (booked(&root), TaskSource::Instance)),
        (
            "room-wake-me",
            (
                TaskEntry {
                    wait: Some(TaskTarget::default()),
                    ..booked(&root)
                },
                TaskSource::Instance,
            ),
        ),
        (
            "room-team-delivery",
            (
                TaskEntry {
                    wait: Some(TaskTarget::default()),
                    team: Some(crate::ids::TeamInstanceId::new("forge", None).unwrap()),
                    ..booked(&root)
                },
                TaskSource::Instance,
            ),
        ),
        (
            "project-row",
            (
                booked(&root),
                TaskSource::Project {
                    state: crate::trust::TrustState::Trusted,
                },
            ),
        ),
    ];
    let tasks: BTreeMap<String, LoadedTask> = rows
        .into_iter()
        .map(|(name, (entry, source))| (name.to_owned(), LoadedTask::new(name, entry, source)))
        .collect();
    let readings: BTreeMap<String, TaskReading> = tasks
        .keys()
        .map(|name| {
            let attention = name
                .starts_with("invalid")
                .then(|| (Attention::Invalid, format!("invalid: {name}")));
            (
                name.clone(),
                TaskReading {
                    attention,
                    ..reading()
                },
            )
        })
        .collect();

    let facts = collect(
        &tasks,
        &readings,
        &TeamsConfig::default(),
        &records(&runtime),
        &root,
    );

    let named: Vec<_> = facts
        .iter()
        .map(|facts| {
            (
                facts.task.as_str(),
                facts.needs_fix.as_deref(),
                facts.timing.is_some(),
            )
        })
        .collect();
    assert_eq!(
        named,
        [
            ("booked", None, false),
            ("detached", None, false),
            ("invalid-action", Some("invalid: invalid-action"), false),
            ("invalid-trigger", Some("invalid: invalid-trigger"), false),
            ("room-condition", None, false),
            ("room-timed", None, true),
            ("timed", None, true),
        ],
        "an invalid trigger or action is still a booking, with the loop's reason"
    );
    let room_timed = facts
        .iter()
        .find(|facts| facts.task == "room-timed")
        .unwrap();
    assert_eq!(
        room_timed.timing,
        tasks["room-timed"]
            .trigger()
            .as_ref()
            .ok()
            .map(|parsed| parsed.describe()),
        "a timed state-tier booking reads the loop's own timing words before its fire"
    );
    assert_eq!(facts[0].booked_by, [link("booker"), link("root")]);
    assert_eq!(facts[0].starts, BookingStarts::Profile("coder".to_owned()));
}

#[test]
fn facts_name_the_team_branch_fire_and_end() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let entry = launching(&["rimz", "teams", "forge", "-w", "feat/lang", "go"], &root);
    let tasks = BTreeMap::from([(
        "lang".to_owned(),
        LoadedTask::new("lang", entry, TaskSource::Config),
    )]);
    let run = WorkflowRunId::new();
    let at: Timestamp = "2026-11-13T10:00:00Z".parse().unwrap();
    let ledger = FireMark {
        fire: FireId::new(&TaskId::new()),
        at,
        booked_by: Vec::new(),
        once: false,
        run: Some(run.clone()),
        scope: crate::store::workflow::ScopeKind::Project,
        result: None,
        kind: None,
        target: None,
        manual: false,
        opener: None,
        child: None,
    };
    let readings = BTreeMap::from([(
        "lang".to_owned(),
        TaskReading {
            fired: true,
            launch: Some(ledger),
            blocking: Some(BlockingTerm {
                key: "pr[feature]".to_owned(),
                reading: Some("open".to_owned()),
                pr: Some(7),
                more: 1,
            }),
            ..reading()
        },
    )]);
    let supervised = |started: &str, completed: &str, loop_task: Option<&str>, agent: &str| {
        let mut record = crate::store::run::RunRecord::new(
            WorkspaceId::from_project_root(&root),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "task".into(),
            root.clone(),
        );
        record.started_at = started.parse().unwrap();
        record.completed_at = Some(completed.parse().unwrap());
        record.loop_task = loop_task.map(str::to_owned);
        record.agent_id = Some(agent.into());
        record
    };
    // A blocking launch runs as a supervised run the task owns.
    let records = RoomRecords {
        runs: vec![
            supervised(
                "2026-11-13T10:01:00Z",
                "2026-11-13T11:00:00Z",
                Some("lang"),
                "a",
            ),
            supervised(
                "2026-11-13T10:02:00Z",
                "2026-11-13T11:30:00Z",
                Some("lang"),
                "b",
            ),
            supervised(
                "2026-11-13T09:00:00Z",
                "2026-11-13T12:30:00Z",
                Some("lang"),
                "earlier",
            ),
            supervised(
                "2026-11-13T10:03:00Z",
                "2026-11-13T12:00:00Z",
                None,
                "unrelated",
            ),
        ],
        ..records(&runtime)
    };
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        Team {
            leader: Some("planner".to_owned()),
            ..Team::default()
        },
    )]));

    let facts = collect(&tasks, &readings, &teams, &records, &root);

    let facts = &facts[0];
    assert_eq!(facts.starts, BookingStarts::Team("forge".to_owned()));
    assert_eq!(facts.leader.as_deref(), Some("planner"));
    assert_eq!(facts.branch.as_deref(), Some("feat/lang"));
    assert_eq!(
        facts.fire,
        Some(FireFacts {
            at,
            run: Some(run.clone()),
            ended_at: Some("2026-11-13T11:30:00Z".parse().unwrap()),
            pr: BookingPr::Unread,
            team_done: None,
            launches: Vec::new(),
        }),
        "the latest end of the runs the task owned since the fire; an unprobed branch is unread"
    );
    assert_eq!(
        facts
            .blocking
            .as_ref()
            .map(|term| (term.key.as_str(), term.pr, term.more)),
        Some(("pr[feature]", Some(7), 1))
    );
}

#[test]
fn destination_branch_reads_a_named_tree_only() {
    assert_eq!(
        destination_branch(Some("lang-typed")).as_deref(),
        Some("lang-typed")
    );
    assert_eq!(
        destination_branch(Some("feat/x")).as_deref(),
        Some("feat/x")
    );
    assert_eq!(destination_branch(Some("bad name")), None);
    // A bare `-w`, `--from-pr` or a fresh tree per fire names none.
    for words in [
        &["rimz", "agents", "coder", "go", "-w"][..],
        &["rimz", "agents", "coder", "--from-pr", "7", "go"],
    ] {
        let launch = read_fire_launch(&argv(words)).unwrap();
        assert_eq!(launch.worktree, Some(LaunchTree::Unnamed), "{words:?}");
        assert_eq!(destination_branch(named(&launch)), None, "{words:?}");
    }
    assert_eq!(destination_branch(None), None);
}

#[test]
fn an_absent_unreadable_or_other_version_cache_reads_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (_root, runtime) = room(dir.path());
    let path = runtime.bookings_path();
    assert_eq!(read_bookings_cache(&path), BookingsCache::default());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{not json").unwrap();
    assert_eq!(read_bookings_cache(&path), BookingsCache::default());
    let mut facts = BookingFacts {
        task: "t".to_owned(),
        starts: BookingStarts::Profile("coder".to_owned()),
        leader: None,
        booked_by: vec![link("booker")],
        branch: None,
        worktree: false,
        needs_fix: None,
        blocking: None,
        unmet: Vec::new(),
        timing: None,
        fire: None,
    };
    let other = BookingsCache {
        version: BOOKINGS_CACHE_VERSION + 1,
        bookings: vec![facts.clone()],
    };
    crate::disk::atomic::write_temp_then_rename_cache(&path, &other).unwrap();
    assert_eq!(read_bookings_cache(&path), BookingsCache::default());
    facts.task = "u".to_owned();
    let current = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: vec![facts],
    };
    crate::disk::atomic::write_temp_then_rename_cache(&path, &current).unwrap();
    assert_eq!(read_bookings_cache(&path), current);
}

fn write_workspace(root: &Path, state: &StatePaths) {
    let workspace = crate::workspace::WorkspaceResolver::resolve(root, None).unwrap();
    crate::workspace::record::write(
        state,
        &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
    )
    .unwrap();
}

/// One seeded room read through `read_room` and through the published cache: the
/// entry's `⚠` reason, blocking term and unmet keys are the loop's own. The rows sit in
/// the room's state tier, where they are bookings and no card's pending waits.
#[test]
fn the_published_cache_carries_the_loops_own_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let state = StatePaths::for_project_root_under(&root, &dir.path().join("home")).unwrap();
    let runtime = RuntimePaths::for_state_under(&state, &dir.path().join("run"));
    write_workspace(&root, &state);
    let waiting = TaskEntry {
        when: Some(vec!["pr[feature]=merged && pr[other]=merged".to_owned()]),
        ..booked(&root)
    };
    let invalid = TaskEntry {
        when: Some(vec!["pr[=".to_owned()]),
        ..booked(&root)
    };
    crate::harness::schedule::instances::seed(&state, "waiting", &waiting);
    crate::harness::schedule::instances::seed(&state, "invalid", &invalid);
    let now = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    let stats = BTreeMap::new();
    let context = || ReadContext {
        open: true,
        stats: &stats,
        now: &now,
    };
    let direct = attention::read_room(&root, &state, &runtime, context());

    let catalog = TaskCatalog::load_room_lenient(&root, &state);
    let readings = attention::read_catalog(&catalog, &root, &state, &runtime, context());
    assert!(
        crate::harness::schedule::pending::pending_waits_by_session(&catalog, &root, &now)
            .is_empty(),
        "a state-tier booking is no pending wait"
    );
    let published = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: collect(
            catalog.visible(),
            &readings,
            &TeamsConfig::default(),
            &records(&runtime),
            &root,
        ),
    };
    crate::disk::atomic::write_temp_then_rename_cache(&runtime.bookings_path(), &published)
        .unwrap();
    let cache = read_bookings_cache(&runtime.bookings_path());

    assert_eq!(cache.bookings.len(), 2);
    for facts in &cache.bookings {
        let loop_says = &direct[&facts.task];
        assert_eq!(
            facts.needs_fix,
            loop_says
                .attention
                .as_ref()
                .map(|(_, reason)| reason.clone()),
            "{}: `⚠` exactly when the loop's verdict is set, in its words",
            facts.task
        );
        assert_eq!(
            facts
                .blocking
                .as_ref()
                .map(|term| (&term.key, &term.reading, term.pr, term.more)),
            loop_says
                .blocking
                .as_ref()
                .map(|term| (&term.key, &term.reading, term.pr, term.more)),
        );
        assert_eq!(facts.unmet, loop_says.unmet);
    }
    let invalid = cache
        .bookings
        .iter()
        .find(|facts| facts.task == "invalid")
        .unwrap();
    assert!(
        invalid
            .needs_fix
            .as_deref()
            .is_some_and(|reason| reason.starts_with("invalid: "))
    );
    let waiting = cache
        .bookings
        .iter()
        .find(|facts| facts.task == "waiting")
        .unwrap();
    assert_eq!(
        (waiting.needs_fix.as_deref(), waiting.unmet.len()),
        (None, 2)
    );
}

/// The lane reads the room and writes its cache alone, and only when it changes.
#[test]
fn the_lane_writes_only_its_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let state = StatePaths::for_project_root_under(&root, &dir.path().join("home")).unwrap();
    let runtime = RuntimePaths::for_state_under(&state, &dir.path().join("run"));
    write_workspace(&root, &state);
    crate::harness::schedule::instances::seed(&state, "waiting", &booked(&root));
    let files = |dir: &Path| {
        let mut files = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.push((path.clone(), std::fs::read(&path).unwrap()));
                }
            }
        }
        files.sort();
        files
    };
    let before = files(dir.path());
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(&root),
        Vec::new(),
        Timestamp::now(),
    );
    snapshot.project_root = Some(root.clone());
    let mut lane = BookingsState::default();
    let config = MachineConfig::default();
    refresh_bookings_for(&snapshot, &state, &runtime, &config, None, &mut lane);
    let after = files(dir.path());
    let added: Vec<_> = after
        .iter()
        .filter(|file| !before.contains(file))
        .map(|(path, _)| path.clone())
        .collect();
    assert_eq!(added, [runtime.bookings_path()]);
    assert_eq!(
        read_bookings_cache(&runtime.bookings_path()).version,
        BOOKINGS_CACHE_VERSION
    );
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
    std::fs::File::options()
        .write(true)
        .open(runtime.bookings_path())
        .unwrap()
        .set_modified(old)
        .unwrap();
    refresh_bookings_for(&snapshot, &state, &runtime, &config, None, &mut lane);
    assert_eq!(
        std::fs::metadata(runtime.bookings_path())
            .unwrap()
            .modified()
            .unwrap(),
        old,
        "an unchanged publication is left alone"
    );
}

/// A workflow run record as a loop fire writes it: scheduled, with the task's chain frozen.
fn fire_run(
    task: &str,
    root: &Path,
    scope: &Path,
    fire_tree: bool,
    terminal_at: Option<Timestamp>,
) -> crate::store::workflow::RunRecord {
    let mut record: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
        "version": crate::store::workflow::VERSION, "id": WorkflowRunId::new(),
        "workflow": task, "origin": "inline", "task": task,
        "scope": {"kind": "worktree", "path": scope},
        "fire_tree": fire_tree, "project_root": root,
        "created_at": "2026-11-13T10:00:00Z",
        "state": if terminal_at.is_some() { "done" } else { "active" }, "node": "agent",
        "entered_at": "2026-11-13T10:00:00Z", "visits": {}, "visit_grant": 0,
        "deadline_fired": false, "next_seq": 2, "loop_report": "owed",
        "fire": {"kind": "scheduled", "task": task, "booked_by": [
            {"kind": "claude", "agent_id": "booker"}, {"kind": "claude", "agent_id": "root"}
        ]}
    }))
    .unwrap();
    record.terminal_at = terminal_at;
    record
}

fn ledger_of(task: &str, run: &crate::store::workflow::RunRecord) -> BTreeMap<String, FireMark> {
    BTreeMap::from([(
        task.to_owned(),
        FireMark {
            fire: FireId::new(&TaskId::new()),
            at: run.created_at,
            booked_by: Vec::new(),
            once: false,
            run: Some(run.id.clone()),
            scope: crate::store::workflow::ScopeKind::Worktree,
            result: None,
            kind: None,
            target: None,
            manual: false,
            opener: None,
            child: None,
        },
    )])
}

/// A linked checkout whose `.git` file leads to a RimZ marker naming `branch`.
fn marked_tree(dir: &Path, name: &str, branch: &str, root: &Path) -> PathBuf {
    let tree = dir.join(name);
    let admin = dir.join("admin").join(name);
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    let marker = crate::worktree::WorktreeMarker {
        loop_fire: None,
        version: 4,
        name: name.to_owned(),
        branch: branch.to_owned(),
        base_branch: None,
        from_pr: None,
        base_ref: "main".to_owned(),
        repo_root: root.to_path_buf(),
        worktree_path: tree.clone(),
        created_at: "2026-11-13T10:00:00Z".parse().unwrap(),
        loop_task: Some("timed".to_owned()),
    };
    std::fs::write(
        admin.join("rimz-worktree.json"),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    tree
}

/// A `fire_at` row through its scheduled fire, which consumes the row: its entry is rebuilt
/// from the ledger and the fire's run, lives while the run is live, then for its window
/// after its end, and is gone past it. A row the user removed has no ledger entry.
#[test]
fn a_consumed_one_shot_keeps_its_fired_entry_for_its_lifetime() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let timed = TaskEntry {
        when: None,
        fire_at: Some("2026-11-13T10:00:00Z".parse().unwrap()),
        ..booked(&root)
    };
    let tasks = BTreeMap::from([(
        "timed".to_owned(),
        LoadedTask::new("timed", timed, TaskSource::Instance),
    )]);
    let readings = BTreeMap::from([("timed".to_owned(), reading())]);
    let before = collect(
        &tasks,
        &readings,
        &TeamsConfig::default(),
        &records(&runtime),
        &root,
    );
    assert_eq!(before.len(), 1, "waiting on its time while the row stands");
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: before,
    };

    let now: Timestamp = "2026-11-13T12:00:00Z".parse().unwrap();
    let window = 3_600;
    let consumed_at = |terminal_at: Option<Timestamp>| {
        let run = fire_run("timed", &root, &root, false, terminal_at);
        let ledger = ledger_of("timed", &run);
        let records = RoomRecords {
            commands: BTreeMap::from([(run.id.clone(), launch("coder", None))]),
            workflows: vec![run],
            ..records(&runtime)
        };
        consumed(
            &ledger,
            &TeamsConfig::default(),
            &records,
            &prior,
            &root,
            now,
            window,
        )
        .0
    };

    let live = consumed_at(None);
    assert_eq!(
        live.len(),
        1,
        "the consumed row's fire keeps its entry while its run lives"
    );
    let facts = &live[0];
    assert_eq!(facts.task, "timed");
    assert_eq!(facts.starts, BookingStarts::Profile("coder".to_owned()));
    assert_eq!(facts.booked_by, [link("booker"), link("root")]);
    assert!(
        !facts.worktree,
        "a fire into the project checkout has no worktree"
    );
    let fire = facts.fire.as_ref().unwrap();
    assert_eq!(fire.at, "2026-11-13T10:00:00Z".parse().unwrap());
    assert_eq!(fire.ended_at, None);

    let ended = now - jiff::SignedDuration::from_secs(60);
    let recent = consumed_at(Some(ended));
    assert_eq!(
        recent
            .first()
            .and_then(|facts| facts.fire.as_ref()?.ended_at),
        Some(ended),
        "ended inside its lifetime: still listed, with its end"
    );
    let past = now - jiff::SignedDuration::from_secs(window as i64 + 1);
    assert!(
        consumed_at(Some(past)).is_empty(),
        "ended past the window with no open pull request: gone"
    );

    let removed = consumed(
        &BTreeMap::new(),
        &TeamsConfig::default(),
        &records(&runtime),
        &prior,
        &root,
        now,
        window,
    )
    .0;
    assert!(
        removed.is_empty(),
        "`loop remove` clears the ledger: no entry"
    );
}

/// A fire into a fresh tree reads its destination branch from that tree's marker; once the
/// tree is removed, the entry keeps the branch the last publication derived.
#[test]
fn a_fresh_tree_fire_reads_its_branch_from_the_fire_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let tree = marked_tree(dir.path(), "spike-1", "spike-20261113-100000", &root);
    let entry = launching(&["rimz", "agents", "coder", "go", "-w"], &root);
    let tasks = BTreeMap::from([(
        "spike".to_owned(),
        LoadedTask::new("spike", entry, TaskSource::Config),
    )]);
    let run = fire_run("spike", &root, &tree, true, None);
    let ledger = ledger_of("spike", &run);
    let launch = ledger.get("spike").cloned();
    let readings = BTreeMap::from([(
        "spike".to_owned(),
        TaskReading {
            fired: true,
            launch,
            ..reading()
        },
    )]);
    let records = RoomRecords {
        workflows: vec![run],
        ..records(&runtime)
    };
    let fresh = collect(&tasks, &readings, &TeamsConfig::default(), &records, &root);
    assert_eq!(fresh[0].branch.as_deref(), Some("spike-20261113-100000"));
    assert!(fresh[0].worktree);

    std::fs::remove_dir_all(&tree).unwrap();
    let mut gone = collect(&tasks, &readings, &TeamsConfig::default(), &records, &root);
    assert_eq!(
        gone[0].branch, None,
        "the removed tree's marker is unreadable"
    );
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: fresh,
    };
    carry(&prior, &mut gone);
    assert_eq!(
        gone[0].branch.as_deref(),
        Some("spike-20261113-100000"),
        "the same fire keeps the branch the last publication read"
    );
}

fn board(checkout: &Path, stage: &str) {
    std::fs::create_dir_all(checkout.join(".rimz/team")).unwrap();
    std::fs::write(
        checkout.join(".rimz/team/blackboard.md"),
        format!(
            "Stage: {stage}\n\n## Progress\n- 2026-11-13 11:00:00 @planner: Build -> {stage}\n"
        ),
    )
    .unwrap();
}

/// A team task's fire as the runner records it, with its reading.
fn team_fire(
    task: &str,
    entry: TaskEntry,
    run: &crate::store::workflow::RunRecord,
) -> (BTreeMap<String, LoadedTask>, BTreeMap<String, TaskReading>) {
    let launch = ledger_of(task, run).remove(task);
    (
        BTreeMap::from([(
            task.to_owned(),
            LoadedTask::new(task, entry, TaskSource::Config),
        )]),
        BTreeMap::from([(
            task.to_owned(),
            TaskReading {
                fired: true,
                launch,
                ..reading()
            },
        )]),
    )
}

/// A team booked into a named worktree: the runner's scope is the firing checkout (the
/// project root, `TaskFire::fire_dir`), and the team's board is in the worktree its name
/// resolves to. Resolving the name creates nothing.
#[test]
fn a_named_worktree_teams_done_is_read_in_that_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), Team::default())]));
    let entry = launching(&["rimz", "teams", "forge#lang-tree", "go"], &root);
    let tree = crate::worktree::worktree_path(&root, worktrees(), "lang-tree").unwrap();
    let run = fire_run("lang", &root, &root, false, None);
    let (tasks, readings) = team_fire("lang", entry, &run);
    let records = RoomRecords {
        workflows: vec![run],
        ..records(&runtime)
    };
    let team_done = || {
        collect(&tasks, &readings, &teams, &records, &root)[0]
            .fire
            .as_ref()
            .unwrap()
            .team_done
    };

    assert_eq!(team_done(), None, "no board read yet");
    assert!(
        !tree.exists(),
        "resolving the worktree's name created nothing"
    );
    board(&root, "Done");
    assert_ne!(
        team_done(),
        Some(true),
        "a `Done` board in the project root is not the worktree's"
    );
    board(&tree, "Build");
    assert_eq!(team_done(), Some(false));
    board(&tree, "Done");
    let done = collect(&tasks, &readings, &teams, &records, &root);
    assert_eq!(done[0].fire.as_ref().unwrap().team_done, Some(true));

    std::fs::remove_dir_all(&tree).unwrap();
    let mut closed = collect(&tasks, &readings, &teams, &records, &root);
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: done,
    };
    carry(&prior, &mut closed);
    assert_eq!(
        closed[0].fire.as_ref().unwrap().team_done,
        Some(true),
        "with its checkout gone, the same fire keeps its `Done`"
    );
}

/// A board reopened out of `Done` under the same fire unfinishes the entry, and its branch
/// returns to the pull request lane's demand; only an unreadable board keeps the `Done`.
#[test]
fn a_board_reopened_out_of_done_unfinishes_its_entry() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let tree = marked_tree(dir.path(), "spike-1", "spike-20261113-100000", &root);
    let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), Team::default())]));
    let entry = launching(&["rimz", "teams", "launch", "forge", "go"], &root);
    let run = fire_run("spike", &root, &tree, true, None);
    let (tasks, readings) = team_fire("spike", entry, &run);
    let records = RoomRecords {
        workflows: vec![run],
        ..records(&runtime)
    };
    board(&tree, "Done");
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: collect(&tasks, &readings, &teams, &records, &root),
    };
    assert_eq!(
        prior.bookings[0].fire.as_ref().unwrap().team_done,
        Some(true)
    );
    assert!(branch_demand(&prior).is_empty());

    board(&tree, "Review");
    let mut reopened = collect(&tasks, &readings, &teams, &records, &root);
    carry(&prior, &mut reopened);
    assert_eq!(reopened[0].fire.as_ref().unwrap().team_done, Some(false));
    let reopened = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: reopened,
    };
    assert_eq!(
        branch_demand(&reopened).into_iter().collect::<Vec<_>>(),
        ["spike-20261113-100000"]
    );

    std::fs::remove_file(tree.join(".rimz/team/blackboard.md")).unwrap();
    let mut unreadable = collect(&tasks, &readings, &teams, &records, &root);
    carry(&prior, &mut unreadable);
    assert_eq!(
        unreadable[0].fire.as_ref().unwrap().team_done,
        Some(true),
        "an unreadable board keeps the carried `Done`"
    );
}

/// Each pass records the launch ids of the rows a fire launched (its leader, the leader's
/// cohort, and the rows its run pinned) and the same fire keeps them once they are gone.
#[test]
fn a_fire_records_the_launches_of_its_rows_and_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let entry = launching(&["rimz", "agents", "forge", "go"], &root);
    let run = fire_run("lang", &root, &root, false, None);
    let (tasks, readings) = team_fire("lang", entry, &run);
    let row = |id: &str, team: Option<&str>, task: Option<&str>, pin: Option<&WorkflowRunId>| {
        AgentState {
            launch_id: Some(format!("{id}-launch").as_str().into()),
            team: team.map(str::to_owned),
            loop_task: task.map(str::to_owned),
            registered_at: Some("2026-11-13T10:01:00Z".parse().unwrap()),
            workflow: pin.map(|run| WorkflowAttemptRef {
                run: run.clone(),
                seq: 1,
                role: WorkflowAttemptRole::Step,
            }),
            ..crate::sidebar::test_support::root_agent("claude", id, None)
        }
    };
    let rows = [
        row("lead", Some("forge"), Some("lang"), Some(&run.id)),
        row("member", Some("forge"), None, None),
        row("pinned", None, None, Some(&run.id)),
        row("other", None, Some("other"), None),
        row("elsewhere", Some("forge-2"), None, None),
    ];
    let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), Team::default())]));
    let seen = RoomRecords {
        workflows: vec![run.clone()],
        rows: &rows,
        ..records(&runtime)
    };
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: collect(&tasks, &readings, &teams, &seen, &root),
    };
    assert_eq!(
        prior.bookings[0].fire.as_ref().unwrap().launches,
        ["lead-launch", "member-launch", "pinned-launch"]
    );

    let gone = RoomRecords {
        workflows: vec![run],
        ..records(&runtime)
    };
    let mut later = collect(&tasks, &readings, &teams, &gone, &root);
    carry(&prior, &mut later);
    assert_eq!(
        later[0].fire.as_ref().unwrap().launches,
        ["lead-launch", "member-launch", "pinned-launch"],
        "carried for the same fire once its rows end"
    );
}

/// A blocking launch's command returns when its agent ends, so its run's end ends the fire.
/// A detached launch's run ends once the launch is made: the rows it launched end the fire,
/// once every one of them has ended, unless its run failed.
#[test]
fn a_detached_launch_ends_with_its_rows_not_its_run() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let at = |time: &str| -> Timestamp { format!("2026-11-13T{time}:00Z").parse().unwrap() };
    let mut run = fire_run("lang", &root, &root, false, Some(at("10:05")));
    run.outcome = Some(crate::store::workflow::RunOutcome {
        ending: RunEnding::Done,
        cost_usd: None,
        acted: false,
        reason: None,
        quiet: false,
    });
    let row = |id: &str, ended: Option<&str>| AgentState {
        loop_task: Some("lang".to_owned()),
        registered_at: Some(at("10:01")),
        ended_at: ended.map(at),
        ..crate::sidebar::test_support::root_agent("claude", id, None)
    };
    let ended = |words: &[&str], run: &crate::store::workflow::RunRecord, rows: &[AgentState]| {
        let (tasks, readings) = team_fire("lang", launching(words, &root), run);
        let records = RoomRecords {
            workflows: vec![run.clone()],
            rows,
            ..records(&runtime)
        };
        collect(&tasks, &readings, &TeamsConfig::default(), &records, &root)[0]
            .fire
            .as_ref()
            .unwrap()
            .ended_at
    };
    let blocking = ["rimz", "agents", "coder", "go"];
    let detached = ["rimz", "agents", "coder", "--detach", "go"];

    assert_eq!(ended(&blocking, &run, &[]), Some(at("10:05")));
    assert_eq!(
        ended(&detached, &run, &[]),
        None,
        "launched, nothing registered yet: still starting"
    );
    let one_lives = [row("lead", None), row("member", Some("10:20"))];
    assert_eq!(
        ended(&detached, &run, &one_lives),
        None,
        "a launched row lives"
    );
    let all_ended = [row("lead", Some("10:30")), row("member", Some("10:20"))];
    assert_eq!(ended(&detached, &run, &all_ended), Some(at("10:30")));
    run.outcome.as_mut().unwrap().ending = RunEnding::Failed;
    assert_eq!(
        ended(&detached, &run, &[]),
        Some(at("10:05")),
        "a detached launch whose command failed ended with its run"
    );
}

/// A consumed booking into a named worktree: the runner's scope is the firing checkout,
/// and its branch and worktree flag come from the worktree its command names.
#[test]
fn a_consumed_named_worktree_fire_keeps_its_branch_and_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let run = fire_run("timed", &root, &root, false, None);
    let ledger = ledger_of("timed", &run);
    let records = RoomRecords {
        commands: BTreeMap::from([(run.id.clone(), launch("coder", Some("feat/lang")))]),
        workflows: vec![run],
        ..records(&runtime)
    };
    let (kept, _) = consumed(
        &ledger,
        &TeamsConfig::default(),
        &records,
        &BookingsCache::default(),
        &root,
        "2026-11-13T12:00:00Z".parse().unwrap(),
        3_600,
    );
    assert_eq!(kept[0].branch.as_deref(), Some("feat/lang"));
    assert!(kept[0].worktree);
    let cache = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: kept,
    };
    assert!(branch_demand(&cache).contains("feat/lang"));
}

/// A consumed entry with a worktree stays, whatever its pull request reads, until that pull
/// request merges or its checkout is gone; one with no worktree stays its window after its
/// end. Only those that left are remembered as expired.
#[test]
fn a_consumed_entry_with_a_worktree_stays_until_merged_or_its_checkout_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let now: Timestamp = "2026-11-13T12:00:00Z".parse().unwrap();
    let past = now - jiff::SignedDuration::from_secs(7_200);
    let window = 3_600;
    let tree = |name: &str| {
        let path = crate::worktree::worktree_path(&root, worktrees(), name).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    tree("kept");
    tree("merged");
    let fresh = marked_tree(dir.path(), "spike-1", "spike-20261113-100000", &root);
    let shapes = [
        ("kept", Some("kept"), None),
        ("merged", Some("merged"), None),
        ("checkout-gone", Some("checkout-gone"), None),
        ("treeless", None, None),
        ("fresh", None, Some(&fresh)),
    ];
    let mut ledger = BTreeMap::new();
    let mut workflows = Vec::new();
    let mut commands = BTreeMap::new();
    let mut prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: Vec::new(),
    };
    for (task, worktree, fresh) in shapes {
        let run = match fresh {
            Some(tree) => fire_run(task, &root, tree, true, Some(past)),
            None => fire_run(task, &root, &root, false, Some(past)),
        };
        ledger.extend(ledger_of(task, &run));
        commands.insert(run.id.clone(), launch("coder", worktree));
        if task == "merged" {
            prior.bookings.push(BookingFacts {
                task: task.to_owned(),
                starts: BookingStarts::Profile("coder".to_owned()),
                leader: None,
                booked_by: vec![link("booker"), link("root")],
                branch: Some("merged".to_owned()),
                worktree: true,
                needs_fix: None,
                blocking: None,
                unmet: Vec::new(),
                timing: None,
                fire: Some(FireFacts {
                    at: run.created_at,
                    run: Some(run.id.clone()),
                    ended_at: Some(past),
                    pr: BookingPr::Merged { number: Some(3) },
                    team_done: None,
                    launches: Vec::new(),
                }),
            });
        }
        workflows.push(run);
    }
    let records = RoomRecords {
        workflows,
        commands,
        ..records(&runtime)
    };
    let (kept, gone) = consumed(
        &ledger,
        &TeamsConfig::default(),
        &records,
        &prior,
        &root,
        now,
        window,
    );
    let kept: Vec<_> = kept
        .iter()
        .map(|facts| (facts.task.as_str(), facts.fire.as_ref().unwrap().pr))
        .collect();
    assert_eq!(
        kept,
        [("fresh", BookingPr::Unread), ("kept", BookingPr::Unread)],
        "past the window, an unread pull request on a cold cache does not end it"
    );
    let mut gone: Vec<_> = gone.into_iter().map(|(task, _)| task).collect();
    gone.sort();
    assert_eq!(gone, ["checkout-gone", "merged", "treeless"]);
}

/// The lifetime reads no `Open` gate: with a worktree and its checkout present, an entry
/// past the window stays whatever its pull request reads but merged.
#[test]
fn a_consumed_entrys_pull_request_reading_gates_only_on_merged() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let now: Timestamp = "2026-11-13T12:00:00Z".parse().unwrap();
    let run = fire_run(
        "lang",
        &root,
        &root,
        false,
        Some(now - jiff::SignedDuration::from_secs(7_200)),
    );
    let records = RoomRecords {
        workflows: vec![run.clone()],
        ..records(&runtime)
    };
    for (pr, kept) in [
        (BookingPr::None, true),
        (BookingPr::Unread, true),
        (BookingPr::Open { number: Some(1) }, true),
        (BookingPr::Closed { number: Some(1) }, true),
        (BookingPr::Merged { number: Some(1) }, false),
    ] {
        let facts = BookingFacts {
            task: "lang".to_owned(),
            starts: BookingStarts::Profile("coder".to_owned()),
            leader: None,
            booked_by: vec![link("booker")],
            branch: Some("lang".to_owned()),
            worktree: true,
            needs_fix: None,
            blocking: None,
            unmet: Vec::new(),
            timing: None,
            fire: Some(FireFacts {
                at: run.created_at,
                run: Some(run.id.clone()),
                ended_at: run.terminal_at,
                pr,
                team_done: None,
                launches: Vec::new(),
            }),
        };
        assert_eq!(
            in_lifetime(&facts, Some(&root), &records, now, 3_600),
            kept,
            "{pr:?}"
        );
    }
}

/// A scheduled, booked fire of a loop row's command is rebuilt from the launch its sealed
/// command types; a deployed workflow, or a command that is no launch, is not.
#[test]
fn only_a_scheduled_launch_command_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let state = StatePaths::for_project_root_under(&root, &dir.path().join("home")).unwrap();
    // The staying run has ended and its home sits in the finished directory: the lane finds it
    // by id there.
    let write_run = |run: &crate::store::workflow::RunRecord, text: &str| {
        let paths = if run.task.as_deref() == Some("staying") {
            state.finished_workflow(&run.id)
        } else {
            state.workflow(&run.id)
        };
        std::fs::create_dir_all(&paths.home).unwrap();
        std::fs::write(&paths.record, serde_json::to_vec(run).unwrap()).unwrap();
        let definition = run.definition_dir(&paths);
        std::fs::create_dir_all(&definition).unwrap();
        std::fs::write(crate::disk::paths::workflow_spec_file(&definition), text).unwrap();
    };
    let sealed = |name: &str, words: &[&str]| {
        let entry = TaskEntry {
            kind: None,
            target: None,
            args: argv(words),
            timeout: Some("1h".to_owned()),
            ..booked(&root)
        };
        crate::harness::schedule::runner::command_definition(name, &entry, &entry.args)
            .unwrap()
            .text
    };
    let staying = fire_run(
        "staying",
        &root,
        &root,
        false,
        Some("2026-11-13T11:00:00Z".parse().unwrap()),
    );
    write_run(
        &staying,
        &sealed(
            "staying",
            &["rimz", "agents", "coder", "-w", "lang-tree", "go"],
        ),
    );
    let team = fire_run("team", &root, &root, false, None);
    write_run(
        &team,
        &sealed(
            "team",
            &["rimz", "teams", "forge#lane", "--detach", "it's go"],
        ),
    );
    let shell = fire_run("shell", &root, &root, false, None);
    write_run(
        &shell,
        &sealed("shell", &["sh", "-c", "rimz agents coder go"]),
    );
    let mut deployed = fire_run("deployed", &root, &root, false, None);
    deployed.origin = crate::store::workflow::RunOrigin::Deployed;
    write_run(
        &deployed,
        &sealed("deployed", &["rimz", "agents", "coder", "go"]),
    );
    let workflows = [
        staying.clone(),
        team.clone(),
        shell.clone(),
        deployed.clone(),
    ];
    let candidates: Vec<_> = workflows
        .iter()
        .map(|run| (run.task.clone().unwrap(), run.id.clone()))
        .collect();

    let homes = run_homes(&state, candidates.iter().map(|(_, run)| run));
    assert_eq!(homes.len(), 4);

    let (booked, rejected) = booked_commands(&homes, &candidates);

    assert_eq!(
        booked,
        BTreeMap::from([
            (staying.id.clone(), launch("coder", Some("lang-tree"))),
            (
                team.id.clone(),
                FireLaunch {
                    team: true,
                    target: "forge".to_owned(),
                    worktree: Some(LaunchTree::Named("lane".to_owned())),
                    detach: true,
                }
            ),
        ])
    );
    let mut rejected: Vec<_> = rejected.into_iter().map(|(task, _)| task).collect();
    rejected.sort();
    assert_eq!(rejected, ["deployed", "shell"]);
}

/// A merged reading is final for its fire: once the lane stops demanding the branch and its
/// row is pruned, the entry keeps `Merged`; a later fire of the name reads afresh.
#[test]
fn a_merged_reading_is_carried_for_the_same_fire_only() {
    let facts = |run: &WorkflowRunId, pr: BookingPr| BookingFacts {
        task: "lang".to_owned(),
        starts: BookingStarts::Profile("coder".to_owned()),
        leader: None,
        booked_by: vec![link("booker")],
        branch: Some("lang".to_owned()),
        worktree: true,
        needs_fix: None,
        blocking: None,
        unmet: Vec::new(),
        timing: None,
        fire: Some(FireFacts {
            at: "2026-11-13T10:00:00Z".parse().unwrap(),
            run: Some(run.clone()),
            ended_at: None,
            pr,
            team_done: None,
            launches: Vec::new(),
        }),
    };
    let first = WorkflowRunId::new();
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: vec![facts(&first, BookingPr::Merged { number: Some(7) })],
    };
    let mut same = vec![facts(&first, BookingPr::Unread)];
    carry(&prior, &mut same);
    assert_eq!(
        same[0].fire.as_ref().unwrap().pr,
        BookingPr::Merged { number: Some(7) }
    );
    let mut later = vec![facts(&WorkflowRunId::new(), BookingPr::Unread)];
    carry(&prior, &mut later);
    assert_eq!(later[0].fire.as_ref().unwrap().pr, BookingPr::Unread);
}

/// The pull request lane's branch demand: each fired, unfinished booking's own branch.
#[test]
fn fired_unfinished_bookings_demand_their_branch() {
    let fired = |task: &str, pr: BookingPr, team_done: Option<bool>| BookingFacts {
        task: task.to_owned(),
        starts: BookingStarts::Profile("coder".to_owned()),
        leader: None,
        booked_by: vec![link("booker")],
        branch: Some(task.to_owned()),
        worktree: true,
        needs_fix: None,
        blocking: None,
        unmet: Vec::new(),
        timing: None,
        fire: Some(FireFacts {
            at: "2026-11-13T10:00:00Z".parse().unwrap(),
            run: Some(WorkflowRunId::new()),
            ended_at: None,
            pr,
            team_done,
            launches: Vec::new(),
        }),
    };
    let waiting = BookingFacts {
        fire: None,
        ..fired("waiting", BookingPr::Unread, None)
    };
    let cache = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: vec![
            fired("unread", BookingPr::Unread, None),
            fired("open", BookingPr::Open { number: Some(1) }, None),
            fired("none", BookingPr::None, None),
            fired("merged", BookingPr::Merged { number: Some(2) }, None),
            fired("done", BookingPr::Unread, Some(true)),
            fired("reopened", BookingPr::Unread, Some(false)),
            waiting,
        ],
    };
    assert_eq!(
        branch_demand(&cache).into_iter().collect::<Vec<_>>(),
        ["none", "open", "reopened", "unread"]
    );
}

/// A launch into an unnamed worktree (a bare `-w`, `--from-pr`) lives in the tree its agent
/// got, never the firing checkout: once a joined agent's tree is gone the entry ends, and with
/// no joined agent it ends like a treeless one, past its window.
#[test]
fn an_unnamed_tree_booking_reads_its_agents_tree_and_ends_once_it_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let now: Timestamp = "2026-11-13T12:00:00Z".parse().unwrap();
    let past = now - jiff::SignedDuration::from_secs(7_200);
    let kept_tree = dir.path().join("generated-kept");
    std::fs::create_dir_all(&kept_tree).unwrap();
    let removed_tree = dir.path().join("generated-removed");
    let mut ledger = BTreeMap::new();
    let mut workflows = Vec::new();
    let mut commands = BTreeMap::new();
    let mut rows = Vec::new();
    for (task, tree) in [
        ("tree-kept", Some(&kept_tree)),
        ("tree-removed", Some(&removed_tree)),
        ("no-agent", None),
    ] {
        let run = fire_run(task, &root, &root, false, Some(past));
        ledger.extend(ledger_of(task, &run));
        commands.insert(
            run.id.clone(),
            FireLaunch {
                worktree: Some(LaunchTree::Unnamed),
                ..launch("coder", None)
            },
        );
        if let Some(tree) = tree {
            rows.push(AgentState {
                launch_id: Some(format!("{task}-launch").as_str().into()),
                loop_task: Some(task.to_owned()),
                registered_at: Some(run.created_at),
                ended_at: Some(past),
                worktree_path: Some(tree.display().to_string()),
                ..crate::sidebar::test_support::root_agent("claude", task, None)
            });
        }
        workflows.push(run);
    }
    let records = RoomRecords {
        workflows,
        commands,
        rows: &rows,
        ..records(&runtime)
    };
    let (kept, gone) = consumed(
        &ledger,
        &TeamsConfig::default(),
        &records,
        &BookingsCache::default(),
        &root,
        now,
        3_600,
    );
    let kept: Vec<_> = kept.iter().map(|facts| facts.task.as_str()).collect();
    assert_eq!(kept, ["tree-kept"]);
    let mut gone: Vec<_> = gone.into_iter().map(|(task, _)| task).collect();
    gone.sort();
    assert_eq!(gone, ["no-agent", "tree-removed"]);
}
