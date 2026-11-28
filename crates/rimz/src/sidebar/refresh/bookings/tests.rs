use std::path::{Path, PathBuf};

use super::*;
use crate::agents::PermissionMode;
use crate::config::WorktreeConfig;
use crate::config::{LaunchKind, TaskEntry, TaskTarget, Team};
use crate::harness::schedule::attention::{Attention, BlockingTerm};
use crate::ids::{AgentKind, TaskId, WorkspaceId};

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
        kind: Some(LaunchKind::Agent),
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
        runs: Vec::new(),
        rows: &[],
        worktrees: worktrees(),
    }
}

/// A fire mark as a booked agent or team fire leaves it: closed `launched`, its launch child
/// gone, the booking chain frozen.
fn mark(at: Timestamp, kind: Option<LaunchKind>, target: &str) -> FireMark {
    FireMark {
        fire: FireId::new(&TaskId::new()),
        at,
        booked_by: vec![link("booker"), link("root")],
        on: None,
        report: None,
        once: false,
        run: None,
        scope: crate::store::workflow::ScopeKind::Project,
        result: Some(LoopRunResult::Launched),
        kind,
        target: Some(target.to_owned()),
        manual: false,
        opener: None,
        child: None,
    }
}

/// A row the fire `fire` launched: it carries the fire's id.
fn stamped(id: &str, fire: &FireId) -> AgentState {
    AgentState {
        launch_id: Some(format!("{id}-launch").as_str().into()),
        loop_fire: Some(fire.clone()),
        ..crate::sidebar::test_support::root_agent("claude", id, None)
    }
}

/// One booking row and its reading after the fire `mark`.
fn fired(
    task: &str,
    entry: TaskEntry,
    mark: &FireMark,
) -> (BTreeMap<String, LoadedTask>, BTreeMap<String, TaskReading>) {
    (
        BTreeMap::from([(
            task.to_owned(),
            LoadedTask::new(task, entry, TaskSource::Config),
        )]),
        BTreeMap::from([(
            task.to_owned(),
            TaskReading {
                fired: true,
                launch: Some(mark.clone()),
                ..reading()
            },
        )]),
    )
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
                cron: Some("0 * * * *".to_owned()),
                each_worktree: true,
                ..booked(&root)
            }),
        ),
        // Only a declared agent or team launch is a booking: a shell row running one is not.
        (
            "in-a-shell",
            config(launching(&["sh", "-c", "rimz agents coder go"], &root)),
        ),
        (
            "a-workflow",
            config(TaskEntry {
                kind: Some(LaunchKind::Workflow),
                target: Some("nightly".to_owned()),
                args: Vec::new(),
                ..booked(&root)
            }),
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
        &BookingsCache::default(),
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
    let entry = launching(&["rimz", "teams", "forge#feat/lang", "go"], &root);
    let at: Timestamp = "2026-11-13T10:00:00Z".parse().unwrap();
    let fire = mark(at, Some(LaunchKind::Team), "forge#feat/lang");
    let (tasks, mut readings) = fired("lang", entry, &fire);
    readings.get_mut("lang").unwrap().blocking = Some(BlockingTerm {
        key: "pr[feature]".to_owned(),
        reading: Some("open".to_owned()),
        pr: Some(7),
        more: 1,
    });
    let supervised = |completed: &str, stamp: Option<&FireId>, agent: &str| {
        let mut record = crate::store::run::RunRecord::new(
            WorkspaceId::from_project_root(&root),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "task".into(),
            root.clone(),
        );
        record.completed_at = Some(completed.parse().unwrap());
        record.loop_fire = stamp.cloned();
        record.agent_id = Some(agent.into());
        record
    };
    let other = FireId::new(&fire.fire.task());
    // A `-p` launch runs as a supervised run carrying the fire's id; a run of another fire of
    // the same task, or of none, is not this fire's.
    let records = RoomRecords {
        runs: vec![
            supervised("2026-11-13T11:00:00Z", Some(&fire.fire), "a"),
            supervised("2026-11-13T11:30:00Z", Some(&fire.fire), "b"),
            supervised("2026-11-13T12:30:00Z", Some(&other), "earlier"),
            supervised("2026-11-13T12:00:00Z", None, "unrelated"),
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

    let facts = collect(
        &tasks,
        &readings,
        &teams,
        &records,
        &BookingsCache::default(),
        &root,
    );

    let facts = &facts[0];
    assert_eq!(facts.starts, BookingStarts::Team("forge".to_owned()));
    assert_eq!(facts.leader.as_deref(), Some("planner"));
    assert_eq!(facts.branch.as_deref(), Some("feat/lang"));
    assert!(facts.worktree, "a team's `#lane` is a worktree");
    assert_eq!(
        facts.fire,
        Some(FireFacts {
            at,
            fire: fire.fire.clone(),
            ended_at: Some("2026-11-13T11:30:00Z".parse().unwrap()),
            pr: BookingPr::Unread,
            team_done: None,
            launches: Vec::new(),
        }),
        "the latest end of the runs carrying the fire's id; an unprobed branch is unread"
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
fn destination_branch_reads_a_teams_lane_only() {
    let team = |target: &str| Booked::new(LaunchKind::Team, target).unwrap();
    assert_eq!(
        destination_branch(&team("forge#lang-typed")).as_deref(),
        Some("lang-typed")
    );
    assert_eq!(
        destination_branch(&team("forge#feat/x")).as_deref(),
        Some("feat/x")
    );
    assert_eq!(destination_branch(&team("forge#bad name")), None);
    assert_eq!(destination_branch(&team("forge")), None);
    assert_eq!(destination_branch(&team("forge#")), None);
    // An agent booking's destination is not read from its tail before it fires.
    let agent = Booked::new(LaunchKind::Agent, "coder").unwrap();
    assert_eq!(destination_branch(&agent), None);
    assert_eq!(Booked::new(LaunchKind::Workflow, "nightly"), None);
    assert_eq!(Booked::new(LaunchKind::Shell, "sh"), None);
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
        lane: None,
        description: None,
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
    let now = Timestamp::now().to_zoned_datetime(jiff::tz::TimeZone::UTC);
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
        crate::harness::schedule::pending::pending_waits_by_session(&catalog, &root).is_empty(),
        "a state-tier booking is no pending wait"
    );
    let published = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: collect(
            catalog.visible(),
            &readings,
            &TeamsConfig::default(),
            &records(&runtime),
            &BookingsCache::default(),
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
    refresh_bookings_for(&snapshot, None, &state, &runtime, &config, None, &mut lane);
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
    refresh_bookings_for(&snapshot, None, &state, &runtime, &config, None, &mut lane);
    assert_eq!(
        std::fs::metadata(runtime.bookings_path())
            .unwrap()
            .modified()
            .unwrap(),
        old,
        "an unchanged publication is left alone"
    );
}

/// The consumed fires of `marks` by task name.
fn ledger(marks: impl IntoIterator<Item = (&'static str, FireMark)>) -> BTreeMap<String, FireMark> {
    marks
        .into_iter()
        .map(|(task, mark)| (task.to_owned(), mark))
        .collect()
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
        external: None,
        version: 4,
        name: name.to_owned(),
        branch: branch.to_owned(),
        base_branch: None,
        from_pr: None,
        base_ref: "main".to_owned(),
        repo_root: root.to_path_buf(),
        worktree_path: tree.clone(),
        created_at: "2026-11-13T10:00:00Z".parse().unwrap(),
    };
    std::fs::write(
        admin.join("rimz-worktree.json"),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    tree
}

/// A `fire_at` row through its scheduled fire, which consumes the row: its entry is rebuilt
/// from the retired record's fire mark, lives while a row carrying the fire's id lives or its
/// launch is starting, then for its window after its end, and is gone past it. A row the user
/// removed before it fired has no fire mark.
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
        &BookingsCache::default(),
        &root,
    );
    assert_eq!(before.len(), 1, "waiting on its time while the row stands");
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: before,
    };

    let now: Timestamp = "2026-11-13T12:00:00Z".parse().unwrap();
    let window = 3_600;
    let fire = mark(
        "2026-11-13T10:00:00Z".parse().unwrap(),
        Some(LaunchKind::Agent),
        "coder",
    );
    let consumed_with = |fire: &FireMark, rows: &[AgentState]| {
        let records = RoomRecords {
            rows,
            ..records(&runtime)
        };
        consumed(
            &ledger([("timed", fire.clone())]),
            &TeamsConfig::default(),
            &records,
            &prior,
            &root,
            now,
            window,
        )
        .0
    };

    let live = consumed_with(&fire, &[stamped("lead", &fire.fire)]);
    assert_eq!(
        live.len(),
        1,
        "the consumed row's fire keeps its entry while its row lives"
    );
    let facts = &live[0];
    assert_eq!(facts.task, "timed");
    assert_eq!(facts.starts, BookingStarts::Profile("coder".to_owned()));
    assert_eq!(facts.booked_by, [link("booker"), link("root")]);
    assert!(
        !facts.worktree,
        "a launch into the project checkout has no worktree"
    );
    let fired = facts.fire.as_ref().unwrap();
    assert_eq!(fired.at, fire.at);
    assert_eq!(fired.fire, fire.fire);
    assert_eq!(fired.ended_at, None);
    assert_eq!(fired.launches, ["lead-launch"]);

    let ended_row = |ended: Timestamp| AgentState {
        ended_at: Some(ended),
        ..stamped("lead", &fire.fire)
    };
    let ended = now - jiff::SignedDuration::from_secs(60);
    let recent = consumed_with(&fire, &[ended_row(ended)]);
    assert_eq!(
        recent
            .first()
            .and_then(|facts| facts.fire.as_ref()?.ended_at),
        Some(ended),
        "ended inside its lifetime: still listed, with its end"
    );
    let past = now - jiff::SignedDuration::from_secs(window as i64 + 1);
    assert!(
        consumed_with(&fire, &[ended_row(past)]).is_empty(),
        "ended past the window with no open pull request: gone"
    );
    let unrelated = AgentState {
        loop_fire: Some(FireId::new(&fire.fire.task())),
        ..crate::sidebar::test_support::root_agent("claude", "other", None)
    };
    assert!(
        consumed_with(&fire, &[ended_row(past), unrelated]).is_empty(),
        "a living row of another fire of the same task keeps nothing"
    );

    let starting = mark(
        now - jiff::SignedDuration::from_secs(60),
        Some(LaunchKind::Agent),
        "coder",
    );
    assert_eq!(
        consumed_with(&starting, &[]).len(),
        1,
        "launched, nothing registered yet: starting, inside its window"
    );
    let failed = FireMark {
        result: Some(LoopRunResult::Failed),
        ..starting.clone()
    };
    assert_eq!(
        consumed_with(&failed, &[])[0]
            .fire
            .as_ref()
            .unwrap()
            .ended_at,
        Some(failed.at),
        "a failed launch made nothing: it ended at its fire"
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
    assert!(removed.is_empty(), "no fire mark: no entry");
}

/// A consumed fire is rebuilt only from a mark of an agent or team launch an agent booked:
/// not a workflow or shell fire, not an unbooked one, not a mark an older RimZ wrote with no
/// declared kind.
#[test]
fn only_a_booked_agent_or_team_fire_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let now: Timestamp = "2026-11-13T12:00:00Z".parse().unwrap();
    let at = now - jiff::SignedDuration::from_secs(60);
    let unbooked = FireMark {
        booked_by: Vec::new(),
        on: None,
        ..mark(at, Some(LaunchKind::Agent), "coder")
    };
    let marks = ledger([
        ("agent", mark(at, Some(LaunchKind::Agent), "coder")),
        ("team", mark(at, Some(LaunchKind::Team), "forge#lane")),
        ("workflow", mark(at, Some(LaunchKind::Workflow), "nightly")),
        ("shell", mark(at, Some(LaunchKind::Shell), "sh")),
        ("older", mark(at, None, "coder")),
        ("unbooked", unbooked),
    ]);
    let (kept, gone) = consumed(
        &marks,
        &TeamsConfig::default(),
        &records(&runtime),
        &BookingsCache::default(),
        &root,
        now,
        3_600,
    );
    let kept: Vec<_> = kept
        .iter()
        .map(|facts| (facts.task.as_str(), facts.starts.clone()))
        .collect();
    assert_eq!(
        kept,
        [
            ("agent", BookingStarts::Profile("coder".to_owned())),
            ("team", BookingStarts::Team("forge".to_owned())),
        ]
    );
    assert!(gone.is_empty());
}

/// Once the fire's rows registered, its destination branch is the marker branch of the tree
/// they run in; once the tree is removed, the entry keeps the branch the last publication
/// derived.
#[test]
fn a_fire_reads_its_branch_from_the_tree_its_rows_run_in() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let tree = marked_tree(dir.path(), "spike-1", "spike-20261113-100000", &root);
    let entry = launching(&["rimz", "agents", "coder", "go", "-w"], &root);
    let fire = mark(
        "2026-11-13T10:00:00Z".parse().unwrap(),
        Some(LaunchKind::Agent),
        "coder",
    );
    let (tasks, readings) = fired("spike", entry, &fire);
    let unfired = collect(
        &tasks,
        &BTreeMap::from([("spike".to_owned(), reading())]),
        &TeamsConfig::default(),
        &records(&runtime),
        &BookingsCache::default(),
        &root,
    );
    assert_eq!(
        (unfired[0].branch.as_deref(), unfired[0].worktree),
        (None, false),
        "an agent booking's tree is not read from its tail before it fires"
    );
    let rows = [AgentState {
        worktree_path: Some(tree.display().to_string()),
        ..stamped("lead", &fire.fire)
    }];
    let records = RoomRecords {
        rows: &rows,
        ..records(&runtime)
    };
    let fresh = collect(
        &tasks,
        &readings,
        &TeamsConfig::default(),
        &records,
        &BookingsCache::default(),
        &root,
    );
    assert_eq!(fresh[0].branch.as_deref(), Some("spike-20261113-100000"));
    assert!(fresh[0].worktree);

    std::fs::remove_dir_all(&tree).unwrap();
    let mut gone = collect(
        &tasks,
        &readings,
        &TeamsConfig::default(),
        &records,
        &BookingsCache::default(),
        &root,
    );
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
    assert!(gone[0].worktree);
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

/// A team booked into its `#lane`: the team's board is in the worktree the lane resolves to,
/// read before any row registers. Resolving the name creates nothing.
#[test]
fn a_lane_teams_done_is_read_in_that_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), Team::default())]));
    let entry = launching(&["rimz", "teams", "forge#lang-tree", "go"], &root);
    let tree = crate::worktree::worktree_path(&root, worktrees(), "lang-tree").unwrap();
    let fire = mark(
        "2026-11-13T10:00:00Z".parse().unwrap(),
        Some(LaunchKind::Team),
        "forge#lang-tree",
    );
    let (tasks, readings) = fired("lang", entry, &fire);
    let records = records(&runtime);
    let team_done = || {
        collect(
            &tasks,
            &readings,
            &teams,
            &records,
            &BookingsCache::default(),
            &root,
        )[0]
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
    let done = collect(
        &tasks,
        &readings,
        &teams,
        &records,
        &BookingsCache::default(),
        &root,
    );
    assert_eq!(done[0].fire.as_ref().unwrap().team_done, Some(true));

    std::fs::remove_dir_all(&tree).unwrap();
    let mut closed = collect(
        &tasks,
        &readings,
        &teams,
        &records,
        &BookingsCache::default(),
        &root,
    );
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
    let fire = mark(
        "2026-11-13T10:00:00Z".parse().unwrap(),
        Some(LaunchKind::Team),
        "forge",
    );
    let (tasks, readings) = fired("spike", entry, &fire);
    let rows = [AgentState {
        team: Some("forge".to_owned()),
        worktree_path: Some(tree.display().to_string()),
        ..stamped("lead", &fire.fire)
    }];
    let records = RoomRecords {
        rows: &rows,
        ..records(&runtime)
    };
    board(&tree, "Done");
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: collect(
            &tasks,
            &readings,
            &teams,
            &records,
            &BookingsCache::default(),
            &root,
        ),
    };
    assert_eq!(
        prior.bookings[0].fire.as_ref().unwrap().team_done,
        Some(true)
    );
    assert!(branch_demand(&prior).is_empty());

    board(&tree, "Review");
    let mut reopened = collect(
        &tasks,
        &readings,
        &teams,
        &records,
        &BookingsCache::default(),
        &root,
    );
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
    let mut unreadable = collect(
        &tasks,
        &readings,
        &teams,
        &records,
        &BookingsCache::default(),
        &root,
    );
    carry(&prior, &mut unreadable);
    assert_eq!(
        unreadable[0].fire.as_ref().unwrap().team_done,
        Some(true),
        "an unreadable board keeps the carried `Done`"
    );
}

/// Each pass records the launch ids of the rows a fire launched (every row carrying its id
/// and their cohorts) and the same fire keeps them once they are gone.
#[test]
fn a_fire_records_the_launches_of_its_rows_and_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let entry = launching(&["rimz", "agents", "forge", "go"], &root);
    let fire = mark(
        "2026-11-13T10:00:00Z".parse().unwrap(),
        Some(LaunchKind::Agent),
        "forge",
    );
    let (tasks, readings) = fired("lang", entry, &fire);
    let other = FireId::new(&fire.fire.task());
    let row = |id: &str, team: Option<&str>, stamp: Option<&FireId>| AgentState {
        team: team.map(str::to_owned),
        loop_fire: stamp.cloned(),
        ..stamped(id, &fire.fire)
    };
    let rows = [
        row("lead", Some("forge"), Some(&fire.fire)),
        row("member", Some("forge"), None),
        row("other", None, Some(&other)),
        row("elsewhere", Some("forge-2"), None),
    ];
    let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), Team::default())]));
    let seen = RoomRecords {
        rows: &rows,
        ..records(&runtime)
    };
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: collect(
            &tasks,
            &readings,
            &teams,
            &seen,
            &BookingsCache::default(),
            &root,
        ),
    };
    assert_eq!(
        prior.bookings[0].fire.as_ref().unwrap().launches,
        ["lead-launch", "member-launch"]
    );

    let mut later = collect(
        &tasks,
        &readings,
        &teams,
        &records(&runtime),
        &BookingsCache::default(),
        &root,
    );
    carry(&prior, &mut later);
    assert_eq!(
        later[0].fire.as_ref().unwrap().launches,
        ["lead-launch", "member-launch"],
        "carried for the same fire once its rows end"
    );
}

/// A fire ends with what carries its id: once every row has ended, the newest end; a living
/// row holds it open; a launch that registered nothing yet is starting, unless its fire
/// failed.
#[test]
fn a_fire_ends_with_the_rows_carrying_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let at = |time: &str| -> Timestamp { format!("2026-11-13T{time}:00Z").parse().unwrap() };
    let fire = mark(at("10:00"), Some(LaunchKind::Agent), "coder");
    let row = |id: &str, ended: Option<&str>| AgentState {
        ended_at: ended.map(at),
        ..stamped(id, &fire.fire)
    };
    let ended = |fire: &FireMark, rows: &[AgentState]| {
        let (tasks, readings) = fired(
            "lang",
            launching(&["rimz", "agents", "coder", "--detach", "go"], &root),
            fire,
        );
        let records = RoomRecords {
            rows,
            ..records(&runtime)
        };
        collect(
            &tasks,
            &readings,
            &TeamsConfig::default(),
            &records,
            &BookingsCache::default(),
            &root,
        )[0]
        .fire
        .as_ref()
        .unwrap()
        .ended_at
    };

    assert_eq!(
        ended(&fire, &[]),
        None,
        "launched, nothing registered yet: still starting"
    );
    let one_lives = [row("lead", None), row("member", Some("10:20"))];
    assert_eq!(ended(&fire, &one_lives), None, "a launched row lives");
    let all_ended = [row("lead", Some("10:30")), row("member", Some("10:20"))];
    assert_eq!(ended(&fire, &all_ended), Some(at("10:30")));
    // A provisional launch that failed is over by the terminal rule, though it stamps no end.
    let failed_start = AgentState {
        status: crate::agents::AgentStatus::Failed,
        ..row("launch_lead", None)
    };
    assert_eq!(
        ended(&fire, &[failed_start, row("member", Some("10:20"))]),
        Some(at("10:20"))
    );
    let failed = FireMark {
        result: Some(LoopRunResult::Failed),
        ..fire.clone()
    };
    assert_eq!(
        ended(&failed, &[]),
        Some(at("10:00")),
        "a launch that failed made nothing: it ended at its fire"
    );
}

/// A consumed team booking into its `#lane` keeps the lane's branch and worktree flag.
#[test]
fn a_consumed_lane_fire_keeps_its_branch_and_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let now: Timestamp = "2026-11-13T12:00:00Z".parse().unwrap();
    let fire = mark(
        now - jiff::SignedDuration::from_secs(60),
        Some(LaunchKind::Team),
        "forge#feat/lang",
    );
    let (kept, _) = consumed(
        &ledger([("timed", fire)]),
        &TeamsConfig::default(),
        &records(&runtime),
        &BookingsCache::default(),
        &root,
        now,
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
        ("kept", LaunchKind::Team, "forge#kept", None),
        ("merged", LaunchKind::Team, "forge#merged", None),
        (
            "checkout-gone",
            LaunchKind::Team,
            "forge#checkout-gone",
            None,
        ),
        ("treeless", LaunchKind::Agent, "coder", None),
        ("fresh", LaunchKind::Agent, "coder", Some(&fresh)),
    ];
    let mut marks = Vec::new();
    let mut rows = Vec::new();
    let mut names = BTreeMap::new();
    let mut prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: Vec::new(),
    };
    for (task, kind, target, tree) in shapes {
        let fire = mark(past, Some(kind), target);
        rows.push(AgentState {
            ended_at: Some(past),
            worktree_path: tree.map(|tree| tree.display().to_string()),
            ..stamped(task, &fire.fire)
        });
        if task == "merged" {
            prior.bookings.push(BookingFacts {
                task: task.to_owned(),
                starts: BookingStarts::Team("forge".to_owned()),
                leader: None,
                booked_by: vec![link("booker"), link("root")],
                branch: Some("merged".to_owned()),
                worktree: true,
                needs_fix: None,
                blocking: None,
                unmet: Vec::new(),
                timing: None,
                lane: None,
                description: None,
                fire: Some(FireFacts {
                    at: past,
                    fire: fire.fire.clone(),
                    ended_at: Some(past),
                    pr: BookingPr::Merged { number: Some(3) },
                    team_done: None,
                    launches: Vec::new(),
                }),
            });
        }
        names.insert(fire.fire.clone(), task);
        marks.push((task, fire));
    }
    let records = RoomRecords {
        rows: &rows,
        ..records(&runtime)
    };
    let (kept, gone) = consumed(
        &ledger(marks),
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
    let mut gone: Vec<_> = gone.iter().map(|fire| names[fire]).collect();
    gone.sort();
    assert_eq!(gone, ["checkout-gone", "merged", "treeless"]);
}

/// The lifetime reads no `Open` gate: with a worktree and its checkout present, an entry
/// past the window stays whatever its pull request reads but merged; anything alive keeps it.
#[test]
fn a_consumed_entrys_pull_request_reading_gates_only_on_merged() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _runtime) = room(dir.path());
    let now: Timestamp = "2026-11-13T12:00:00Z".parse().unwrap();
    let past = now - jiff::SignedDuration::from_secs(7_200);
    let fire = mark(past, Some(LaunchKind::Agent), "coder");
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
            lane: None,
            description: None,
            fire: Some(FireFacts {
                at: past,
                fire: fire.fire.clone(),
                ended_at: Some(past),
                pr,
                team_done: None,
                launches: Vec::new(),
            }),
        };
        assert_eq!(
            in_lifetime(&facts, Some(&root), false, now, 3_600),
            kept,
            "{pr:?}"
        );
        assert!(in_lifetime(&facts, Some(&root), true, now, 3_600));
    }
}

/// A merged reading is final for its fire: once the lane stops demanding the branch and its
/// row is pruned, the entry keeps `Merged`; a later fire of the name reads afresh.
#[test]
fn a_merged_reading_is_carried_for_the_same_fire_only() {
    let facts = |fire: &FireId, pr: BookingPr| BookingFacts {
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
        lane: None,
        description: None,
        fire: Some(FireFacts {
            at: "2026-11-13T10:00:00Z".parse().unwrap(),
            fire: fire.clone(),
            ended_at: None,
            pr,
            team_done: None,
            launches: Vec::new(),
        }),
    };
    let task = TaskId::new();
    let first = FireId::new(&task);
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
    let mut later = vec![facts(&FireId::new(&task), BookingPr::Unread)];
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
        lane: None,
        description: None,
        fire: Some(FireFacts {
            at: "2026-11-13T10:00:00Z".parse().unwrap(),
            fire: FireId::new(&TaskId::new()),
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

/// A launch into a tree it names or generates lives in the tree its rows got: once that tree
/// is gone the entry ends, and with no row it ends like a treeless one, past its window.
#[test]
fn a_rows_tree_booking_ends_once_its_tree_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let now: Timestamp = "2026-11-13T12:00:00Z".parse().unwrap();
    let past = now - jiff::SignedDuration::from_secs(7_200);
    let kept_tree = marked_tree(dir.path(), "generated-kept", "kept", &root);
    let removed_tree = marked_tree(dir.path(), "generated-removed", "removed", &root);
    std::fs::remove_dir_all(&removed_tree).unwrap();
    let mut marks = Vec::new();
    let mut rows = Vec::new();
    let mut names = BTreeMap::new();
    for (task, tree) in [
        ("tree-kept", Some(&kept_tree)),
        ("tree-removed", Some(&removed_tree)),
        ("no-agent", None),
    ] {
        let fire = mark(past, Some(LaunchKind::Agent), "coder");
        if let Some(tree) = tree {
            rows.push(AgentState {
                ended_at: Some(past),
                worktree_path: Some(tree.display().to_string()),
                ..stamped(task, &fire.fire)
            });
        }
        names.insert(fire.fire.clone(), task);
        marks.push((task, fire));
    }
    let records = RoomRecords {
        rows: &rows,
        ..records(&runtime)
    };
    let (kept, gone) = consumed(
        &ledger(marks),
        &TeamsConfig::default(),
        &records,
        &BookingsCache::default(),
        &root,
        now,
        3_600,
    );
    let kept: Vec<_> = kept.iter().map(|facts| facts.task.as_str()).collect();
    assert_eq!(kept, ["tree-kept"]);
    let mut gone: Vec<_> = gone.iter().map(|fire| names[fire]).collect();
    gone.sort();
    assert_eq!(gone, ["no-agent", "tree-removed"]);
}

/// An entry's text is its task's label, else its launch tail's `--description` in either
/// spelling before any `--`, single-lined, blank reading as none; a team names the lane its
/// target types, trunk's own lane and `external` none.
#[test]
fn facts_read_the_label_or_launch_description_and_a_teams_typed_lane() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let task = |name: &str, entry: TaskEntry| {
        (
            name.to_owned(),
            LoadedTask::new(name, entry, TaskSource::Config),
        )
    };
    let tasks = BTreeMap::from([
        task(
            "labelled",
            TaskEntry {
                label: Some("the label".to_owned()),
                ..launching(
                    &["rimz", "agents", "coder", "--description", "the tail"],
                    &root,
                )
            },
        ),
        task(
            "spaced",
            launching(
                &["rimz", "agents", "coder", "--description", "spaced text"],
                &root,
            ),
        ),
        task(
            "joined",
            launching(
                &["rimz", "agents", "coder", "--description=joined text", "go"],
                &root,
            ),
        ),
        task("bare", launching(&["rimz", "agents", "coder", "go"], &root)),
        task(
            "passthrough",
            launching(
                &["rimz", "agents", "coder", "go", "--", "--description=x"],
                &root,
            ),
        ),
        task(
            "blank",
            TaskEntry {
                label: Some("   ".to_owned()),
                ..launching(&["rimz", "agents", "coder", "go"], &root)
            },
        ),
        task(
            "multiline",
            TaskEntry {
                label: Some("two\n  lines".to_owned()),
                ..launching(&["rimz", "agents", "coder", "go"], &root)
            },
        ),
        task(
            "lane",
            launching(&["rimz", "teams", "forge#feat-x", "go"], &root),
        ),
        task(
            "trunk",
            launching(&["rimz", "teams", "forge#main", "go"], &root),
        ),
        task(
            "external",
            launching(&["rimz", "teams", "forge#external", "go"], &root),
        ),
        task(
            "laneless",
            launching(&["rimz", "teams", "forge", "go"], &root),
        ),
    ]);
    let readings = tasks.keys().map(|name| (name.clone(), reading())).collect();
    let facts = collect(
        &tasks,
        &readings,
        &TeamsConfig::default(),
        &records(&runtime),
        &BookingsCache::default(),
        &root,
    );
    let read: BTreeMap<&str, (Option<&str>, Option<&str>)> = facts
        .iter()
        .map(|facts| {
            (
                facts.task.as_str(),
                (facts.lane.as_deref(), facts.description.as_deref()),
            )
        })
        .collect();
    assert_eq!(read["labelled"], (None, Some("the label")));
    assert_eq!(read["spaced"], (None, Some("spaced text")));
    assert_eq!(read["joined"], (None, Some("joined text")));
    assert_eq!(read["bare"], (None, None));
    assert_eq!(read["passthrough"], (None, None), "the provider's own tail");
    assert_eq!(read["blank"], (None, None));
    assert_eq!(read["multiline"], (None, Some("two lines")));
    assert_eq!(read["lane"].0, Some("feat-x"));
    assert_eq!(read["trunk"].0, None);
    assert_eq!(read["external"].0, None);
    assert_eq!(read["laneless"].0, None);
}

/// A fired agent names the named lane its rows run in, never a checkout's own lane; once its
/// one-shot is consumed, the text read before the fire and the lane its rows had carry over.
#[test]
fn a_fired_agent_names_its_rows_named_lane_and_keeps_its_text_past_consumption() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let entry = TaskEntry {
        label: Some("type the language".to_owned()),
        when: None,
        fire_at: Some("2026-11-13T10:00:00Z".parse().unwrap()),
        ..booked(&root)
    };
    let tasks = BTreeMap::from([(
        "typed".to_owned(),
        LoadedTask::new("typed", entry, TaskSource::Instance),
    )]);
    let unfired = collect(
        &tasks,
        &BTreeMap::from([("typed".to_owned(), reading())]),
        &TeamsConfig::default(),
        &records(&runtime),
        &BookingsCache::default(),
        &root,
    );
    assert_eq!(unfired[0].description.as_deref(), Some("type the language"));
    assert_eq!(
        unfired[0].lane, None,
        "an agent names no lane before it fires"
    );
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: unfired,
    };
    let now: Timestamp = "2026-11-13T10:30:00Z".parse().unwrap();
    let fire = mark(
        "2026-11-13T10:00:00Z".parse().unwrap(),
        Some(LaunchKind::Agent),
        "coder",
    );
    let lane = |name: Option<&str>| {
        let own = crate::ids::Lane::own(crate::ids::LaneHome::Trunk, root.to_string_lossy());
        Some(match name {
            Some(name) => own.named(crate::ids::LaneName::dashed(name)),
            None => own,
        })
    };
    let consumed_with = |rows: &[AgentState], prior: &BookingsCache| {
        let records = RoomRecords {
            rows,
            ..records(&runtime)
        };
        consumed(
            &ledger([("typed", fire.clone())]),
            &TeamsConfig::default(),
            &records,
            prior,
            &root,
            now,
            3_600,
        )
        .0
    };
    let own = AgentState {
        lane: lane(None),
        ..stamped("lead", &fire.fire)
    };
    let fired = consumed_with(&[own], &prior);
    assert_eq!(fired[0].description.as_deref(), Some("type the language"));
    assert_eq!(fired[0].lane, None, "trunk's own lane is no lane");
    let named = AgentState {
        lane: lane(Some("lang")),
        ..stamped("lead", &fire.fire)
    };
    let fired = consumed_with(&[named], &prior);
    assert_eq!(fired[0].lane.as_deref(), Some("lang"));
    let seen = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: fired,
    };
    let gone = consumed_with(&[], &seen);
    assert_eq!(
        (gone[0].lane.as_deref(), gone[0].description.as_deref()),
        (Some("lang"), Some("type the language")),
        "the same fire keeps both once its rows are gone"
    );
}

/// A fired team names the lane its cohort runs in, read from the rows carrying the fire's id,
/// trunk's none; while no row is registered it names the lane its target types.
#[test]
fn a_fired_team_names_its_cohorts_lane_from_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let now: Timestamp = "2026-11-13T10:30:00Z".parse().unwrap();
    let fire = mark(
        "2026-11-13T10:00:00Z".parse().unwrap(),
        Some(LaunchKind::Team),
        "forge",
    );
    let member = |lane: &str| {
        let own = crate::ids::Lane::own(crate::ids::LaneHome::Trunk, root.to_string_lossy());
        AgentState {
            team: Some("forge".to_owned()),
            lane: Some(match lane {
                "main" => own,
                lane => own.named(crate::ids::LaneName::dashed(lane)),
            }),
            ..stamped("planner", &fire.fire)
        }
    };
    let lane_with = |rows: &[AgentState]| {
        let records = RoomRecords {
            rows,
            ..records(&runtime)
        };
        consumed(
            &ledger([("docs", fire.clone())]),
            &TeamsConfig::default(),
            &records,
            &BookingsCache::default(),
            &root,
            now,
            3_600,
        )
        .0[0]
            .lane
            .clone()
    };
    assert_eq!(lane_with(&[member("docs")]).as_deref(), Some("docs"));
    assert_eq!(
        lane_with(&[member("main")]),
        None,
        "trunk's lane is no lane"
    );
    assert_eq!(lane_with(&[]), None, "no row: the typed target names none");
}

/// A room with a store and a private machine home, for a test that drives the lane entry: the
/// task record holds the booking's fire mark and the event log the rows its fire launched.
struct StoredRoom {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: StatePaths,
    runtime: RuntimePaths,
}

impl StoredRoom {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let state = StatePaths::for_project_root_under(&root, &dir.path().join("home")).unwrap();
        let runtime = RuntimePaths::for_state_under(&state, &dir.path().join("run"));
        write_workspace(&root, &state);
        Self {
            _dir: dir,
            root,
            state,
            runtime,
        }
    }

    /// Adds `mark` to the machine's task record as the fire of the task `name` this room binds;
    /// a consumed task's record is retired.
    fn record_fire(&self, name: &str, mark: &FireMark, consumed: bool) {
        use crate::harness::schedule::task_record::{Binding, Scope, TaskRecord};
        let binding = Binding {
            scope: Scope::Workspace(WorkspaceId::from_project_root(&self.root)),
            name: name.to_owned(),
        };
        let key = binding.key();
        let record = TaskRecord {
            binding,
            retired_at: consumed.then_some(mark.at),
            arming: None,
            strikes: 0,
            counted: Vec::new(),
            scheduled: BTreeMap::new(),
            quiet: None,
            marks: BTreeMap::from([(self.root.clone(), mark.clone())]),
        };
        let mut records = serde_json::to_value(task_record::load().unwrap()).unwrap();
        let id = serde_json::to_value(mark.fire.task()).unwrap();
        records["tasks"][id.as_str().unwrap()] = serde_json::to_value(record).unwrap();
        let loops = crate::disk::paths::loops_dir();
        std::fs::create_dir_all(&loops).unwrap();
        std::fs::write(loops.join("tasks.json"), records.to_string()).unwrap();
        assert_eq!(task_record::load().unwrap().live(&key).is_none(), consumed);
    }

    /// Appends `event` to the room's event log and returns its stamp.
    fn append(&self, event: &crate::EventEnvelope) -> Timestamp {
        std::fs::create_dir_all(self.state.events_log.parent().unwrap()).unwrap();
        crate::store::event_log::append(&self.state.events_log, event).unwrap();
        event.timestamp
    }

    /// The fire `fire` launching `launch` at `epoch`, in `tree` when given, bound in its pane,
    /// and its session `session` registering.
    fn launch(&self, fire: &FireId, launch: &str, epoch: u32, session: &str, tree: Option<&Path>) {
        let payload: crate::store::event::AgentLaunchPayload =
            serde_json::from_value(serde_json::json!({
                "agent_id": launch_id(launch, epoch), "agent_name": launch,
                "launch_id": launch_id(launch, 0),
                "state": "bound", "epoch": (epoch > 0).then_some(epoch), "loop_fire": fire,
                "worktree_path": tree,
            }))
            .unwrap();
        self.append(&crate::EventEnvelope::agent_launched(
            self.state.workspace_id.clone(),
            "room",
            &AgentKind::new_unchecked("claude"),
            payload,
        ));
        self.hook(
            launch,
            epoch,
            session,
            "test",
            crate::agents::LifecycleSignal::Registered,
        );
    }

    /// The session `session` of `launch` ending by the event `event_name` (`rimz agents stop`'s
    /// or the provider's SessionEnd hook); returns the end's stamp.
    fn end(&self, launch: &str, epoch: u32, session: &str, event_name: &str) -> Timestamp {
        self.hook(
            launch,
            epoch,
            session,
            event_name,
            crate::agents::LifecycleSignal::Ended,
        )
    }

    fn hook(
        &self,
        launch: &str,
        epoch: u32,
        session: &str,
        event_name: &str,
        signal: crate::agents::LifecycleSignal,
    ) -> Timestamp {
        let mut observation =
            crate::agents::AgentLifecycleObservation::new(Some(session.into()), signal);
        observation.agent_name = Some(launch.to_owned());
        observation.launch_id = Some(launch_id(launch, 0).as_str().into());
        observation.launch.epoch = (epoch > 0).then_some(epoch);
        self.append(&crate::EventEnvelope::agent_lifecycle(
            self.state.workspace_id.clone(),
            "room",
            "claude",
            event_name,
            &observation,
        ))
    }

    /// One lane pass as the host runs it, over the fold's Runtime rows and the room's store
    /// (`None` when it cannot be opened), then the published cache.
    fn publish(&self, store: bool, teams: Option<&TeamsConfig>) -> BookingsCache {
        let store = store.then(|| {
            crate::Store::open_existing(self.state.clone(), self.runtime.clone()).unwrap()
        });
        let mut snapshot = SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(&self.root),
            Vec::new(),
            Timestamp::now(),
        );
        snapshot.project_root = Some(self.root.clone());
        if let Some(store) = &store {
            snapshot.agents = store
                .runtime_projection(crate::RuntimeScope::Runtime)
                .map(|projection| projection.agents)
                .unwrap_or_default()
                .into();
        }
        refresh_bookings_for(
            &snapshot,
            store.as_ref(),
            &self.state,
            &self.runtime,
            &MachineConfig::default(),
            teams,
            &mut BookingsState::default(),
        );
        read_bookings_cache(&self.runtime.bookings_path())
    }
}

/// The provisional id of the launch of the agent `name` at `epoch`; at epoch 0, its launch id.
fn launch_id(name: &str, epoch: u32) -> String {
    match epoch {
        0 => format!("launch_{name}"),
        _ => format!("launch_{name}_{epoch}"),
    }
}

/// A booked fire's mark as its fire leaves it, `minutes` ago.
fn fired_mark(minutes: i64, kind: LaunchKind, target: &str) -> FireMark {
    FireMark {
        report: None,
        once: true,
        ..mark(
            Timestamp::now() - jiff::SignedDuration::from_mins(minutes),
            Some(kind),
            target,
        )
    }
}

/// The published fire of the room's one booking.
fn published_fire(cache: &BookingsCache) -> &FireFacts {
    assert_eq!(cache.bookings.len(), 1, "{cache:?}");
    cache.bookings[0].fire.as_ref().unwrap()
}

/// An interactive agent a catalog booking launched, ended by `rimz agents stop` or by its
/// SessionEnd hook, publishes its row's end: the room's durable rows keep the ended row the
/// fold's Runtime rows drop. Its tree still names the entry's branch after the end.
#[test]
fn an_ended_interactive_booking_publishes_its_rows_end() {
    if crate::harness::schedule::test_homes::reexec_with_private_homes(
        "sidebar::refresh::bookings::tests::an_ended_interactive_booking_publishes_its_rows_end",
    ) {
        return;
    }
    for event_name in ["rimz.agent-ended", "SessionEnd"] {
        let room = StoredRoom::new();
        let entry = TaskEntry {
            once_per_checkout: Some(true),
            ..launching(&["rimz", "agents", "coder", "go"], &room.root)
        };
        crate::harness::schedule::instances::seed(&room.state, "lang", &entry);
        let fire = fired_mark(1, LaunchKind::Agent, "coder");
        room.record_fire("lang", &fire, false);
        let tree = marked_tree(room._dir.path(), "booked", "booked-branch", &room.root);
        room.launch(&fire.fire, "booked", 0, "sess", Some(&tree));

        let live = room.publish(true, None);
        assert_eq!(
            published_fire(&live).ended_at,
            None,
            "{event_name}: a live row"
        );

        let end = room.end("booked", 0, "sess", event_name);
        let ended = room.publish(true, None);
        assert_eq!(published_fire(&ended).ended_at, Some(end), "{event_name}");
        assert_eq!(
            (
                ended.bookings[0].branch.as_deref(),
                ended.bookings[0].worktree
            ),
            (Some("booked-branch"), true),
            "{event_name}: the ended row still names its tree"
        );
    }
}

/// A consumed one-shot whose interactive agent ended publishes the row's end and, with no
/// tree, stays for its window after that end though its fire is older than the window.
#[test]
fn an_ended_interactive_one_shot_publishes_its_rows_end() {
    if crate::harness::schedule::test_homes::reexec_with_private_homes(
        "sidebar::refresh::bookings::tests::an_ended_interactive_one_shot_publishes_its_rows_end",
    ) {
        return;
    }
    let room = StoredRoom::new();
    let fire = fired_mark(20, LaunchKind::Agent, "coder");
    room.record_fire("timed", &fire, true);
    room.launch(&fire.fire, "booked", 0, "sess", None);
    let end = room.end("booked", 0, "sess", "rimz.agent-ended");

    assert_eq!(
        published_fire(&room.publish(true, None)).ended_at,
        Some(end)
    );
}

/// A team fire ends once every seat carrying its id is over, at the newest seat's end; while
/// one seat lives it has no end.
#[test]
fn a_team_fire_ends_with_its_last_seat() {
    if crate::harness::schedule::test_homes::reexec_with_private_homes(
        "sidebar::refresh::bookings::tests::a_team_fire_ends_with_its_last_seat",
    ) {
        return;
    }
    let room = StoredRoom::new();
    let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), Team::default())]));
    let fire = fired_mark(1, LaunchKind::Team, "forge");
    room.record_fire("crew", &fire, true);
    room.launch(&fire.fire, "lead", 0, "lead-sess", None);
    room.launch(&fire.fire, "member", 0, "member-sess", None);
    room.end("member", 0, "member-sess", "SessionEnd");
    assert_eq!(
        published_fire(&room.publish(true, Some(&teams))).ended_at,
        None,
        "one seat lives"
    );

    let last = room.end("lead", 0, "lead-sess", "rimz.agent-ended");
    assert_eq!(
        published_fire(&room.publish(true, Some(&teams))).ended_at,
        Some(last)
    );
}

/// A launch swapped to a new session keeps its fire alive: its first session's ended row and
/// the live replacement are one launch id.
#[test]
fn a_swapped_launch_with_one_ended_session_lives() {
    if crate::harness::schedule::test_homes::reexec_with_private_homes(
        "sidebar::refresh::bookings::tests::a_swapped_launch_with_one_ended_session_lives",
    ) {
        return;
    }
    let room = StoredRoom::new();
    let fire = fired_mark(1, LaunchKind::Agent, "coder");
    room.record_fire("timed", &fire, true);
    room.launch(&fire.fire, "booked", 0, "old", None);
    room.launch(&fire.fire, "booked", 1, "new", None);
    room.end("booked", 0, "old", "SessionEnd");

    let rows = crate::Store::open_existing(room.state.clone(), room.runtime.clone())
        .unwrap()
        .runtime_projection(crate::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert_eq!(
        rows.iter()
            .map(|row| (row.agent_id.as_str(), row.ended_at.is_some()))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([("old", true), ("new", false)]),
        "the room holds the ended and the live session of the launch"
    );
    assert_eq!(published_fire(&room.publish(true, None)).ended_at, None);
}

/// A pass that cannot read the room's rows leaves the published cache as it was; a room with
/// no booking reads none and publishes its empty cache.
#[test]
fn an_unreadable_store_leaves_the_cache_and_no_booking_reads_none() {
    if crate::harness::schedule::test_homes::reexec_with_private_homes(
        "sidebar::refresh::bookings::tests::an_unreadable_store_leaves_the_cache_and_no_booking_reads_none",
    ) {
        return;
    }
    let room = StoredRoom::new();
    let empty = room.publish(false, None);
    assert_eq!(
        empty.version, BOOKINGS_CACHE_VERSION,
        "published with no store to read"
    );
    assert!(empty.bookings.is_empty());

    let fire = fired_mark(1, LaunchKind::Agent, "coder");
    room.record_fire("timed", &fire, true);
    room.launch(&fire.fire, "booked", 0, "sess", None);
    let end = room.end("booked", 0, "sess", "SessionEnd");
    let published = room.publish(true, None);
    assert_eq!(published_fire(&published).ended_at, Some(end));
    let bytes = std::fs::read(room.runtime.bookings_path()).unwrap();
    assert_eq!(
        room.publish(false, None),
        published,
        "no store: the cache stands"
    );
    std::fs::remove_file(&room.state.events_log).unwrap();
    std::fs::create_dir(&room.state.events_log).unwrap();
    room.publish(true, None);
    assert_eq!(
        std::fs::read(room.runtime.bookings_path()).unwrap(),
        bytes,
        "an unreadable event log: the cache stands"
    );
}

/// A fired one-off that is no booking reads no rows: with no store to read, the pass still
/// publishes the unfired booking beside it.
#[test]
fn a_fired_one_off_that_is_no_booking_reads_no_rows() {
    if crate::harness::schedule::test_homes::reexec_with_private_homes(
        "sidebar::refresh::bookings::tests::a_fired_one_off_that_is_no_booking_reads_no_rows",
    ) {
        return;
    }
    let room = StoredRoom::new();
    crate::harness::schedule::instances::seed(&room.state, "waiting", &booked(&room.root));
    let chore = TaskEntry {
        once_per_checkout: Some(true),
        ..launching(&["make", "check"], &room.root)
    };
    crate::harness::schedule::instances::seed(&room.state, "chore", &chore);
    let fire = FireMark {
        result: Some(LoopRunResult::Completed),
        ..fired_mark(1, LaunchKind::Shell, "make")
    };
    room.record_fire("chore", &fire, false);

    let cache = room.publish(false, None);
    assert_eq!(
        cache
            .bookings
            .iter()
            .map(|facts| (facts.task.as_str(), facts.fire.is_some()))
            .collect::<Vec<_>>(),
        [("waiting", false)]
    );
}

/// A fire whose rows were pruned keeps the end its last publication held, by either way in
/// (a catalog booking's and a consumed one-shot's), until something carrying its id is alive
/// again; a treeless consumed entry whose kept end is past the window is dropped as before.
#[test]
fn a_fire_keeps_its_published_end_once_its_rows_are_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let (root, runtime) = room(dir.path());
    let now: Timestamp = "2026-11-13T12:00:00Z".parse().unwrap();
    let past = now - jiff::SignedDuration::from_secs(15 * 86_400);
    let path = crate::worktree::worktree_path(&root, worktrees(), "kept").unwrap();
    std::fs::create_dir_all(&path).unwrap();
    let published = |task: &str, fire: &FireMark, worktree: bool| BookingFacts {
        task: task.to_owned(),
        starts: BookingStarts::Team("forge".to_owned()),
        leader: None,
        booked_by: vec![link("booker")],
        branch: worktree.then(|| task.to_owned()),
        worktree,
        needs_fix: None,
        blocking: None,
        unmet: Vec::new(),
        timing: None,
        lane: None,
        description: None,
        fire: Some(FireFacts {
            at: past,
            fire: fire.fire.clone(),
            ended_at: Some(past),
            pr: BookingPr::Open { number: Some(3) },
            team_done: None,
            launches: vec!["gone".to_owned()],
        }),
    };
    let kept = mark(past, Some(LaunchKind::Team), "forge#kept");
    let revived = mark(past, Some(LaunchKind::Team), "forge#kept");
    let treeless = mark(past, Some(LaunchKind::Agent), "coder");
    let catalog = mark(past, Some(LaunchKind::Agent), "coder");
    let prior = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings: vec![
            published("kept", &kept, true),
            published("revived", &revived, true),
            published("treeless", &treeless, false),
            published("lang", &catalog, false),
        ],
    };
    let end = |facts: &BookingFacts| facts.fire.as_ref().unwrap().ended_at;

    let living = [stamped("revived", &revived.fire)];
    let records = RoomRecords {
        rows: &living,
        ..records(&runtime)
    };
    let ledger = ledger(vec![
        ("kept", kept),
        ("revived", revived.clone()),
        ("treeless", treeless.clone()),
    ]);
    let (kept, gone) = consumed(
        &ledger,
        &TeamsConfig::default(),
        &records,
        &prior,
        &root,
        now,
        3_600,
    );
    let ends: Vec<_> = kept
        .iter()
        .map(|facts| (facts.task.as_str(), end(facts)))
        .collect();
    assert_eq!(
        ends,
        [("kept", Some(past)), ("revived", None)],
        "a pruned worktree entry keeps its end; one alive again publishes none"
    );
    assert_eq!(
        gone,
        [treeless.fire],
        "a treeless kept end past the window drops"
    );

    let entry = launching(&["rimz", "agents", "coder", "go"], &root);
    let (tasks, readings) = fired("lang", entry.clone(), &catalog);
    let pruned = collect(
        &tasks,
        &readings,
        &TeamsConfig::default(),
        &records,
        &prior,
        &root,
    );
    assert_eq!(
        end(&pruned[0]),
        Some(past),
        "a catalog fire keeps its end too"
    );
    let (tasks, readings) = fired("lang", entry, &revived);
    let alive = collect(
        &tasks,
        &readings,
        &TeamsConfig::default(),
        &records,
        &prior,
        &root,
    );
    assert_eq!(
        end(&alive[0]),
        None,
        "a catalog fire alive again publishes none"
    );
}
