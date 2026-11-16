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
                every: Some("1h".to_owned()),
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

    let facts = collect(&tasks, &readings, &teams, &records, &root);

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
        bookings: collect(&tasks, &readings, &teams, &seen, &root),
    };
    assert_eq!(
        prior.bookings[0].fire.as_ref().unwrap().launches,
        ["lead-launch", "member-launch"]
    );

    let mut later = collect(&tasks, &readings, &teams, &records(&runtime), &root);
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
        collect(&tasks, &readings, &TeamsConfig::default(), &records, &root)[0]
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
