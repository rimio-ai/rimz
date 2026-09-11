use super::*;
use crate::ids::WorkspaceId;
use crate::sidebar::test_support::{activity_row, pane, root_agent, worktree_group};
use crate::sidebar::timing::GIT_ACTIVITY_WINDOW;
use jiff::Timestamp;
use std::process::Command;

#[test]
fn git_cache_freshness_boundaries_are_inclusive() {
    let entry = DiffStatsCacheEntry {
        refreshed_at_ms: 1_000,
        commit_refreshed_at_ms: Some(1_000),
        added: None,
        removed: None,
        commits: None,
        behind: None,
        trunk: None,
        branch: None,
        clean: None,
        landed: None,
        did_work: None,
        merge_in_progress: None,
        ..DiffStatsCacheEntry::default()
    };
    let fast = DIFF_STATS_TTL.as_millis() as u64;
    let idle = DIFF_STATS_IDLE_TTL.as_millis() as u64;

    assert!(entry.local_fresh_for(1_000 + fast, DIFF_STATS_TTL));
    assert!(!entry.local_fresh_for(1_001 + fast, DIFF_STATS_TTL));
    assert!(entry.commit_fresh_for(1_000 + fast, DIFF_STATS_TTL));
    assert!(!entry.commit_fresh_for(1_001 + fast, DIFF_STATS_TTL));
    assert!(entry.local_fresh_for(1_000 + idle, DIFF_STATS_IDLE_TTL));
    assert!(!entry.local_fresh_for(1_001 + idle, DIFF_STATS_IDLE_TTL));
    // The tiering's whole point: a hot-stale entry is idle-fresh, so an idle
    // worktree skips the forks a hot one pays.
    assert!(entry.local_fresh_for(1_001 + fast, DIFF_STATS_IDLE_TTL));

    // The populated fields round-trip through the cache entry.
    let populated = DiffStatsCacheEntry {
        refreshed_at_ms: 1_000,
        commit_refreshed_at_ms: Some(1_000),
        added: Some(2),
        removed: Some(1),
        commits: Some(4),
        behind: Some(2),
        trunk: Some("main".to_owned()),
        branch: Some("feature-migration".to_owned()),
        clean: Some(true),
        landed: Some(true),
        did_work: Some(true),
        merge_in_progress: Some(false),
        ..DiffStatsCacheEntry::default()
    };
    assert_eq!(
        populated.stats(),
        Some(DiffStats {
            added: 2,
            removed: 1,
        })
    );

    // An old producer's cache entry predates the `clean`, `landed`, and
    // `from_pr` columns; serde defaults read them back as "not probed" (`None`),
    // never facts it cannot prove.
    let legacy: DiffStatsCacheEntry = serde_json::from_str(
            r#"{"refreshed_at_ms":1000,"added":0,"removed":0,"commits":0,"behind":3,"trunk":"main","branch":"feat"}"#,
        )
        .unwrap();
    assert_eq!(legacy.clean, None);
    assert_eq!(legacy.landed, None);
    assert_eq!(legacy.from_pr, None);
    assert_eq!(legacy.stats(), Some(DiffStats::default()));
    assert!(
        !legacy.commit_fresh_for(1_000, DIFF_STATS_TTL),
        "pre-split entries refresh commit facts once"
    );

    let cache = WorktreeRootsCache {
        refreshed_at_ms: 1_000,
        roots: Vec::new(),
        marker_names: Some(Default::default()),
    };
    let ttl = WORKTREE_ROOTS_TTL.as_millis() as u64;
    assert!(cache.is_fresh(1_000 + ttl));
    assert!(!cache.is_fresh(1_001 + ttl));
    // A clock that ran backwards reads fresh (saturating).
    assert!(cache.is_fresh(500));
}

/// A minimal agent/process row for the hot-set tests: only the fields
/// `hot_worktree_paths` reads vary.
/// The hot set, boundary-exact at `GIT_ACTIVITY_WINDOW`: running rows are hot
/// regardless of activity age, recent activity is hot through exactly the
/// window, process-only and `External`-kind groups are cold, and a dead dir
/// is excluded just as `needed_worktree_paths` excludes it.
#[test]
fn hot_worktree_paths_keys_on_running_or_recent_agent_rows() {
    let dir = tempfile::tempdir().unwrap();
    let wt = |name: &str| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let now = Timestamp::from_second(1_750_000_000).unwrap();
    let window = SignedDuration::try_from(GIT_ACTIVITY_WINDOW).unwrap();
    let stale_activity = now - window - SignedDuration::from_secs(1);

    let running = wt("running");
    let recent = wt("recent");
    let boundary = wt("boundary");
    let idle = wt("idle");
    let procs = wt("procs");
    let external_kind = wt("external-kind");
    let dead = dir.path().join("dead-dir");

    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(dir.path()), Vec::new(), now);
    snapshot.worktree_groups = vec![
        // Running carries hotness on its own — its activity stamp is stale.
        worktree_group(
            &running,
            vec![activity_row(
                true,
                Some(AgentStatus::Running),
                stale_activity,
                &running,
            )],
        ),
        worktree_group(
            &recent,
            vec![activity_row(
                true,
                Some(AgentStatus::Idle),
                now - SignedDuration::from_secs(1),
                &recent,
            )],
        ),
        // Exactly the window boundary stays hot (<=, matching the TTL gates).
        worktree_group(
            &boundary,
            vec![activity_row(
                true,
                Some(AgentStatus::Idle),
                now - window,
                &boundary,
            )],
        ),
        worktree_group(
            &idle,
            vec![activity_row(
                true,
                Some(AgentStatus::Idle),
                stale_activity,
                &idle,
            )],
        ),
        // A busy process row is not an agent row: cold by definition.
        worktree_group(&procs, vec![activity_row(false, None, now, &procs)]),
        // An External-kind group never reaches the git refresh.
        {
            let mut group = worktree_group(
                &external_kind,
                vec![activity_row(
                    true,
                    Some(AgentStatus::Running),
                    now,
                    &external_kind,
                )],
            );
            group.kind = crate::store::snapshot::SidebarWorktreeKind::External;
            group
        },
        // A running agent in a since-removed dir: hot <= needed, so excluded.
        worktree_group(
            &dead,
            vec![activity_row(true, Some(AgentStatus::Running), now, &dead)],
        ),
    ];

    let hot = hot_worktree_paths(&snapshot);

    let path_of = |p: &Path| p.display().to_string();
    assert!(hot.contains(&path_of(&running)));
    assert!(hot.contains(&path_of(&recent)));
    assert!(
        hot.contains(&path_of(&boundary)),
        "boundary-exact: <= window"
    );
    assert!(!hot.contains(&path_of(&idle)), "stale activity is cold");
    assert!(
        !hot.contains(&path_of(&procs)),
        "process rows carry no heat"
    );
    assert!(!hot.contains(&path_of(&external_kind)));
    assert!(!hot.contains(&path_of(&dead)), "hot is a subset of needed");
    assert_eq!(hot.len(), 3);
}

/// A future-stamped row (clock skew between writers) reads as hot — the safe
/// direction, mirroring the saturating TTL convention.
#[test]
fn hot_worktree_paths_treats_future_activity_as_hot() {
    let dir = tempfile::tempdir().unwrap();
    let now = Timestamp::from_second(1_750_000_000).unwrap();
    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(dir.path()), Vec::new(), now);
    snapshot.worktree_groups = vec![worktree_group(
        dir.path(),
        vec![activity_row(
            true,
            Some(AgentStatus::Idle),
            now + SignedDuration::from_secs(120),
            dir.path(),
        )],
    )];

    assert!(hot_worktree_paths(&snapshot).contains(&dir.path().display().to_string()));
}

#[test]
fn focused_worktree_paths_keys_on_viewed_row_panes() {
    let dir = tempfile::tempdir().unwrap();
    let wt = |name: &str| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let focused = wt("focused");
    let background = wt("background");
    let external_kind = wt("external-kind");
    let dead = dir.path().join("dead-dir");
    let now = Timestamp::from_second(1_750_000_000).unwrap();

    let row = |raw: &str, path: &Path| {
        let mut row = activity_row(false, None, now, path);
        row.pane = Some(pane(raw, "zsh", &path.display().to_string()));
        row
    };
    let focused_pane = row("terminal_1", &focused)
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let dead_pane = pane("terminal_4", "zsh", &dead.display().to_string()).pane_id;
    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(dir.path()), Vec::new(), now);
    snapshot.viewed_panes = vec![focused_pane, dead_pane];
    snapshot.worktree_groups = vec![
        worktree_group(&focused, vec![row("terminal_1", &focused)]),
        worktree_group(&background, vec![row("terminal_2", &background)]),
        {
            let mut group = worktree_group(&external_kind, vec![row("terminal_3", &external_kind)]);
            group.kind = crate::store::snapshot::SidebarWorktreeKind::External;
            group
        },
        worktree_group(&dead, vec![row("terminal_4", &dead)]),
    ];

    let focused_paths = focused_worktree_paths(&snapshot);
    let needed: std::collections::BTreeSet<_> =
        needed_worktree_paths(&snapshot).into_iter().collect();

    assert_eq!(
        focused_paths,
        std::collections::BTreeSet::from([focused.display().to_string()])
    );
    assert!(
        focused_paths.is_subset(&needed),
        "focused paths stay inside live needed worktrees"
    );
}

#[test]
fn root_pod_is_excluded_from_git_reads() {
    // The root pod of a non-repo room is a known non-repo: it never enters
    // the producer's git fan-out, while a git-backed row's resolved worktree
    // pod does.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let child = dir.path().join("query-engine");
    std::fs::create_dir_all(&child).unwrap();
    let root_cwd = dir.path().to_string_lossy().into_owned();
    let child_cwd = child.to_string_lossy().into_owned();
    let child_pane = pane("terminal_1", "claude", &child_cwd);
    let mut agent = root_agent("claude", "child", None);
    agent.pane = Some(child_pane.clone());
    agent.worktree_path = Some(child_cwd.clone());
    agent.worktree_branch = Some("main".to_owned());

    let snapshot = SidebarSnapshot::build_with_agents(workspace, vec![agent], Timestamp::now())
        .with_root_class(crate::workspace::RootClass::Directory)
        .with_project_root(Some(dir.path().to_path_buf()))
        .with_live_panes(vec![pane("terminal_0", "zsh", &root_cwd), child_pane], None);

    let kinds: Vec<SidebarWorktreeKind> = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![SidebarWorktreeKind::Worktree, SidebarWorktreeKind::Root]
    );
    assert_eq!(needed_worktree_paths(&snapshot), vec![child_cwd]);
}

#[test]
fn git_line_returns_the_first_non_empty_line() {
    let fixture = GitFixture::init(&["init", "-b", "main"]);
    assert!(fixture.initialized);
    assert!(fixture.git(&["commit", "--allow-empty", "-m", "initial"]));
    assert_eq!(
        git_line(fixture.path(), &["rev-parse", "HEAD", "HEAD"]),
        git_line(fixture.path(), &["rev-parse", "HEAD"])
    );
}

struct GitFixture {
    dir: tempfile::TempDir,
    initialized: bool,
}

impl GitFixture {
    fn init(args: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let initialized = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        let fixture = Self { dir, initialized };
        if initialized {
            let _ = fixture.git(&["config", "user.email", "t@example.com"]);
            let _ = fixture.git(&["config", "user.name", "t"]);
        }
        fixture
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn path_str(&self) -> &str {
        self.path().to_str().unwrap()
    }

    fn git(&self, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.path().join(name), body).unwrap();
    }
}

fn runtime_for(path: &Path) -> (tempfile::TempDir, crate::RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        crate::RuntimePaths::under(crate::ids::WorkspaceId::from_project_root(path), dir.path())
            .unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

fn write_rimz_worktree_marker(repo: &GitFixture, base_ref: &str) {
    write_rimz_worktree_marker_with_pr(repo, "feature", base_ref, None);
}

fn write_rimz_worktree_marker_named(repo: &GitFixture, name: &str, base_ref: &str) {
    write_rimz_worktree_marker_with_pr(repo, name, base_ref, None);
}

fn write_rimz_worktree_marker_with_pr(
    repo: &GitFixture,
    name: &str,
    base_ref: &str,
    from_pr: Option<u64>,
) {
    let marker = crate::worktree::WorktreeMarker {
        version: 1,
        name: name.to_owned(),
        branch: "feature".to_owned(),
        base_branch: Some("main".to_owned()),
        from_pr,
        base_ref: base_ref.to_owned(),
        repo_root: repo.path().to_path_buf(),
        worktree_path: repo.path().to_path_buf(),
        created_at: jiff::Timestamp::now(),
    };
    let git_dir = crate::worktree::git_admin_dir_from_checkout_metadata(repo.path())
        .unwrap()
        .expect("git admin dir");
    crate::disk::atomic::write_temp_then_rename(&git_dir.join("rimz-worktree.json"), &marker)
        .unwrap();
}

#[test]
fn trunk_branch_recognizes_main_and_the_resolved_trunk() {
    assert!(is_trunk_branch("main", None));
    assert!(is_trunk_branch("main", Some("develop")));
    assert!(is_trunk_branch("main", Some("origin/main")));
    assert!(is_trunk_branch("develop", Some("develop")));
    assert!(is_trunk_branch("develop", Some("origin/develop")));
    assert!(!is_trunk_branch("feature", Some("origin/main")));
    assert!(!is_trunk_branch("origin/main", Some("origin/main")));
}

#[test]
fn from_pr_marker_stamps_the_diff_cache_entry() {
    let repo = GitFixture::init(&["init", "-q"]);
    write_rimz_worktree_marker_with_pr(&repo, "feature", "HEAD", Some(69));

    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);

    assert_eq!(entry.from_pr, Some(69));
}

fn channel_group(label: &str, path: &Path) -> SidebarWorktreeGroup {
    SidebarWorktreeGroup {
        key: format!("channel:{label}"),
        label: label.to_owned(),
        label_qualifier: None,
        kind: SidebarWorktreeKind::Channel,
        team: None,
        cohort_effort: None,
        status_counts: Vec::new(),
        rows: vec![activity_row(false, None, Timestamp::now(), path)],
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        worktree_backed: false,
        finished: false,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_ci: None,
        pr_number: None,
        pr_url: None,
    }
}

#[test]
fn git_backed_worktree_path_accepts_worktrees_and_marked_channels() {
    let repo = GitFixture::init(&["init", "-q"]);
    let worktree = crate::sidebar::test_support::worktree_group(repo.path(), Vec::new());
    assert_eq!(
        git_backed_worktree_path(&worktree).as_deref(),
        Some(repo.path_str())
    );

    let mut root = worktree.clone();
    root.kind = SidebarWorktreeKind::Root;
    assert_eq!(git_backed_worktree_path(&root), None);

    let mut external = worktree.clone();
    external.kind = SidebarWorktreeKind::External;
    assert_eq!(git_backed_worktree_path(&external), None);

    let unmarked = channel_group("feature", repo.path());
    assert_eq!(git_backed_worktree_path(&unmarked), None);

    if !repo.initialized {
        return;
    }

    write_rimz_worktree_marker_named(&repo, "feature", "HEAD");
    assert_eq!(
        git_backed_worktree_path(&channel_group("feature", repo.path())).as_deref(),
        Some(repo.path_str())
    );
    assert_eq!(
        git_backed_worktree_path(&channel_group("design", repo.path())),
        None,
        "named channels inside a RimZ worktree do not borrow the worktree story"
    );
}

#[test]
fn parse_numstat_sums_text_diff_and_ignores_binary_rows() {
    let stats = parse_numstat("12\t4\tsrc/lib.rs\n-\t-\tassets/logo.png\n3\t0\tREADME.md\n");

    assert_eq!(
        stats,
        DiffStats {
            added: 15,
            removed: 4,
        }
    );
}

#[test]
fn parse_status_entries_reads_clean_dirty_and_untracked() {
    let clean = parse_status_entries("", |_| unreachable!("no entries to count"));
    assert!(clean.clean);
    assert_eq!(clean.untracked_added, 0);

    // One modified + two untracked entries; only the `??` paths feed the
    // line counter, and any entry at all reads dirty.
    let status =
        parse_status_entries(
            " M src/lib.rs\0?? notes.txt\0?? docs/new.md\0",
            |path| match path {
                "notes.txt" => 3,
                "docs/new.md" => 4,
                other => panic!("unexpected untracked path {other}"),
            },
        );
    assert!(!status.clean);
    assert_eq!(status.untracked_added, 7);

    // A rename entry — staged (`R `) or detected in the worktree column
    // (` R`) — carries its source path as a second NUL token; it must not
    // read as an entry (or an untracked path) of its own.
    let renamed = parse_status_entries(
        "R  new.rs\0old.rs\0 R moved.rs\0was.rs\0?? x.txt\0",
        |path| {
            assert_eq!(path, "x.txt");
            1
        },
    );

    assert!(!renamed.clean);
    assert_eq!(renamed.untracked_added, 1);
}

#[test]
fn count_added_lines_counts_text_and_skips_binary() {
    assert_eq!(count_added_lines(b""), 0);
    assert_eq!(count_added_lines(b"a\nb\n"), 2);
    // A trailing partial line still counts, the way numstat counts one.
    assert_eq!(count_added_lines(b"a\nb"), 2);
    // A NUL reads as binary — mirroring numstat's `-` cells, count nothing.
    assert_eq!(count_added_lines(b"a\x00b\n"), 0);
}

#[test]
fn untracked_added_lines_spends_a_shared_read_budget() {
    let dir = tempfile::tempdir().unwrap();
    let small = dir.path().join("small.txt");
    let big = dir.path().join("big.txt");
    std::fs::write(&small, "a\nb\n").unwrap();
    std::fs::write(&big, "c\nd\ne\n").unwrap();

    // The first file fits and spends its bytes; the second overflows what's
    // left, counts nothing (its status entry still dirties the tree), and
    // leaves the remainder intact.
    let mut budget = 6;
    let prior = UntrackedLineMemo::new();
    let mut next = UntrackedLineMemo::new();
    assert_eq!(
        memoized_untracked_added_lines(dir.path(), "small.txt", &prior, &mut next, &mut budget),
        2
    );
    assert_eq!(budget, 2);
    assert_eq!(
        memoized_untracked_added_lines(dir.path(), "big.txt", &prior, &mut next, &mut budget),
        0
    );
    assert_eq!(budget, 2);
}

#[test]
fn untracked_line_memo_skips_stable_content_reads_and_spends_logical_budget() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("new.txt"), b"one\ntwo\n").unwrap();
    let mut first = UntrackedLineMemo::new();
    let mut budget = 8;
    let mut reads = 0;
    let added = memoized_untracked_added_lines_with(
        dir.path(),
        "new.txt",
        &Default::default(),
        &mut first,
        &mut budget,
        &mut |path| {
            reads += 1;
            std::fs::read(path)
        },
    );
    assert_eq!(added, 2);
    assert_eq!(reads, 1);
    assert_eq!(budget, 0);

    let mut second = UntrackedLineMemo::new();
    let mut budget = 8;
    let added = memoized_untracked_added_lines_with(
        dir.path(),
        "new.txt",
        &first,
        &mut second,
        &mut budget,
        &mut |_| {
            reads += 1;
            Err(std::io::Error::other("memo hit must not read"))
        },
    );
    assert_eq!(added, 2);
    assert_eq!(reads, 1);
    assert_eq!(budget, 0, "hits consume the same logical bytes as misses");
    assert_eq!(second, first);
}

#[test]
fn untracked_line_memo_invalidates_on_length_and_caches_binary_zero() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.bin");
    std::fs::write(&path, b"a\0b").unwrap();
    let mut first = UntrackedLineMemo::new();
    let mut budget = 16;
    let mut reads = 0;
    assert_eq!(
        memoized_untracked_added_lines_with(
            dir.path(),
            "new.bin",
            &Default::default(),
            &mut first,
            &mut budget,
            &mut |path| {
                reads += 1;
                std::fs::read(path)
            },
        ),
        0
    );
    let mut hit = UntrackedLineMemo::new();
    let mut budget = 16;
    assert_eq!(
        memoized_untracked_added_lines_with(
            dir.path(),
            "new.bin",
            &first,
            &mut hit,
            &mut budget,
            &mut |_| panic!("binary zero is a cacheable hit"),
        ),
        0
    );

    std::fs::write(&path, b"one\ntwo\n").unwrap();
    let mut changed = UntrackedLineMemo::new();
    let mut budget = 16;
    assert_eq!(
        memoized_untracked_added_lines_with(
            dir.path(),
            "new.bin",
            &hit,
            &mut changed,
            &mut budget,
            &mut |path| {
                reads += 1;
                std::fs::read(path)
            },
        ),
        2
    );
    assert_eq!(reads, 2);
}

#[test]
fn untracked_line_memo_invalidates_same_length_mtime_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.txt");
    std::fs::write(&path, b"a\nb\n").unwrap();
    let mut first = UntrackedLineMemo::new();
    let mut budget = 16;
    assert_eq!(
        memoized_untracked_added_lines_with(
            dir.path(),
            "new.txt",
            &Default::default(),
            &mut first,
            &mut budget,
            &mut |path| std::fs::read(path),
        ),
        2
    );

    std::fs::write(&path, b"abc\n").unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(2))
        .unwrap();
    let mut changed = UntrackedLineMemo::new();
    let mut budget = 16;
    let mut reads = 0;
    assert_eq!(
        memoized_untracked_added_lines_with(
            dir.path(),
            "new.txt",
            &first,
            &mut changed,
            &mut budget,
            &mut |path| {
                reads += 1;
                std::fs::read(path)
            },
        ),
        1
    );
    assert_eq!(reads, 1);
    assert_ne!(changed, first);
}

#[test]
fn untracked_line_memo_hits_preserve_the_ordered_budget_prefix() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("first.txt"), b"a\nb\n").unwrap();
    std::fs::write(dir.path().join("second.txt"), b"c\nd\n").unwrap();
    let mut prior = UntrackedLineMemo::new();
    let mut budget = 8;
    for path in ["first.txt", "second.txt"] {
        assert_eq!(
            memoized_untracked_added_lines_with(
                dir.path(),
                path,
                &Default::default(),
                &mut prior,
                &mut budget,
                &mut |path| std::fs::read(path),
            ),
            2
        );
    }

    let mut next = UntrackedLineMemo::new();
    let mut budget = 4;
    assert_eq!(
        memoized_untracked_added_lines_with(
            dir.path(),
            "first.txt",
            &prior,
            &mut next,
            &mut budget,
            &mut |_| panic!("memo hit must not read"),
        ),
        2
    );
    assert_eq!(
        memoized_untracked_added_lines_with(
            dir.path(),
            "second.txt",
            &prior,
            &mut next,
            &mut budget,
            &mut |_| panic!("over-budget memo entry must not read"),
        ),
        0
    );
    assert_eq!(
        next.keys().map(String::as_str).collect::<Vec<_>>(),
        ["first.txt"]
    );
}

#[test]
fn git_refresh_state_prunes_worktrees_and_retains_failed_status_memos() {
    let entry = UntrackedLineMemoEntry {
        modified: SystemTime::now(),
        len: 4,
        added: 1,
    };
    let mut state = GitRefreshState {
        untracked: HashMap::from([
            (
                "/keep".to_owned(),
                BTreeMap::from([("a".to_owned(), entry)]),
            ),
            (
                "/drop".to_owned(),
                BTreeMap::from([("b".to_owned(), entry)]),
            ),
        ]),
    };
    let needed = ["/keep".to_owned()];
    state.retain_needed(&needed);
    assert_eq!(state.untracked.len(), 1);

    state.replace_if_successful("/keep".to_owned(), None);
    assert_eq!(state.untracked["/keep"]["a"], entry);

    state.replace_if_successful("/keep".to_owned(), Some(UntrackedLineMemo::new()));
    assert!(
        state.untracked["/keep"].is_empty(),
        "a successful empty status prunes paths that disappeared or became tracked"
    );
}

#[test]
fn head_facts_reads_live_branch_and_detached_head() {
    let repo = GitFixture::init(&["init", "-q"]);
    if !repo.initialized {
        // No git on PATH (or init failed); the helper degrades to None,
        // which is the documented fallback. Nothing to assert.
        assert_eq!(head_facts(repo.path()).branch, None);
        return;
    }
    let _ = repo.git(&["checkout", "-q", "-b", "feature-migration"]);
    repo.write("f", "x");
    let _ = repo.git(&["add", "f"]);
    let _ = repo.git(&["commit", "-q", "-m", "init"]);

    assert_eq!(
        head_facts(repo.path()).branch.as_deref(),
        Some("feature-migration"),
        "the live branch is read from the worktree, overriding any pinned label"
    );
    let _ = repo.git(&["checkout", "-q", "HEAD^0"]);
    assert_eq!(
        head_facts(repo.path()).branch,
        None,
        "a detached HEAD has no branch label"
    );
    // A non-repository path has no branch to track.
    let plain = tempfile::tempdir().unwrap();
    assert_eq!(head_facts(plain.path()).branch, None);
}

#[test]
fn worktree_diff_stats_total_committed_staged_and_unstaged_over_trunk() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    // `-b main` needs Git >= 2.28; an older git fails init and the helper
    // degrades to None, which is the documented fallback.
    if !repo.initialized {
        assert_eq!(
            refresh_entry(repo.path_str(), None, DueFacts::all(), None).stats(),
            None
        );
        return;
    }

    // Fork point on `main`: a three-line tracked file.
    repo.write("base.txt", "a\nb\nc\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    let _ = repo.git(&["branch", "feature-migration"]);

    // `main` advances *after* the fork — a merge-base diff must ignore this,
    // so it never shows up as the worktree's own churn.
    repo.write("base.txt", "a\nB\nc\n");
    let _ = repo.git(&["commit", "-aqm", "trunk moves on"]);

    let _ = repo.git(&["checkout", "-q", "feature-migration"]);
    // Committed on the branch: a new two-line file.
    repo.write("feat.txt", "x\ny\n");
    let _ = repo.git(&["add", "feat.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "feature work"]);
    // Staged but uncommitted: a new one-line file.
    repo.write("staged.txt", "s\n");
    let _ = repo.git(&["add", "staged.txt"]);
    // Unstaged: one more line appended to a tracked file.
    repo.write("base.txt", "a\nb\nc\nd\n");

    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            // +2 committed, +1 staged, +1 unstaged — all measured from the
            // fork point, none from main's post-fork commit.
            added: 4,
            removed: 0,
        }),
        "the header counts committed + staged + unstaged over the trunk merge-base"
    );
    // One commit on the branch since the fork point — staged/unstaged change
    // does not bump the commit count.
    assert_eq!(
        entry.commits,
        Some(1),
        "the commit count is the branch's commits ahead of the trunk merge-base"
    );
    // Main's one post-fork commit is the branch's behind count, and the
    // resolved trunk names the header's `≡` marker.
    assert_eq!(
        entry.behind,
        Some(1),
        "the behind count is the trunk's commits past the merge-base"
    );
    assert_eq!(entry.trunk.as_deref(), Some("main"));
    // Staged + unstaged change reads dirty — the landed markers stay off.
    assert_eq!(entry.clean, Some(false));

    // A non-repository path has nothing to diff, count, or status-read.
    let plain = tempfile::tempdir().unwrap();
    let plain_entry = refresh_entry(plain.path().to_str().unwrap(), None, DueFacts::all(), None);
    assert_eq!(plain_entry.stats(), None);
    assert_eq!(plain_entry.commits, None);
    assert_eq!(plain_entry.behind, None);
    assert_eq!(plain_entry.trunk, None);
    assert_eq!(plain_entry.clean, None);
}

#[test]
fn orphan_branch_without_merge_base_omits_commit_counts() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        return;
    }
    repo.write("main.txt", "main\n");
    let _ = repo.git(&["add", "main.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "main"]);
    let _ = repo.git(&["checkout", "-q", "--orphan", "orphan"]);
    let _ = std::fs::remove_file(repo.path().join("main.txt"));
    repo.write("orphan.txt", "orphan\n");
    let _ = repo.git(&["add", "orphan.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "orphan"]);

    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);

    assert_eq!(entry.stats(), None);
    assert_eq!(entry.commits, None);
    assert_eq!(entry.behind, None);
}

#[test]
fn focused_diff_stats_refreshes_local_facts_before_commit_facts() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    repo.write("base.txt", "base\nedit\n");

    let (_runtime_dir, runtime) = runtime_for(repo.path());
    let cache_path = runtime.diff_stats_path();
    let path = repo.path_str().to_owned();
    let mut cache = DiffStatsCache::default();
    cache.entries.insert(
        path.clone(),
        DiffStatsCacheEntry {
            refreshed_at_ms: 1_000,
            commit_refreshed_at_ms: Some(1_000),
            added: Some(0),
            removed: Some(0),
            commits: Some(9),
            behind: Some(8),
            trunk: Some("main".to_owned()),
            branch: Some("main".to_owned()),
            clean: Some(true),
            landed: Some(false),
            did_work: Some(false),
            merge_in_progress: Some(false),
            ..DiffStatsCacheEntry::default()
        },
    );
    atomic::write_temp_then_rename_cache(&cache_path, &cache).unwrap();

    let refreshed = refresh_diff_stats(
        &cache_path,
        &runtime,
        std::slice::from_ref(&path),
        &BTreeSet::from([path.clone()]),
        &BTreeSet::new(),
        1_000 + DIFF_STATS_FOCUSED_LOCAL_TTL.as_millis() as u64 + 1,
        None,
        &mut GitRefreshState::default(),
    );
    let entry = refreshed.entries.get(&path).unwrap();

    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            added: 1,
            removed: 0
        })
    );
    assert_eq!(entry.clean, Some(false));
    assert_eq!(
        entry.commits,
        Some(9),
        "commit facts stay cached on the 3s pass"
    );
    assert_eq!(entry.behind, Some(8));
    assert_eq!(entry.landed, Some(false));
    assert_eq!(entry.commit_refreshed_at_ms, Some(1_000));
}

#[test]
fn non_focused_diff_stats_refreshes_local_and_commit_facts_together() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    repo.write("base.txt", "base\nedit\n");

    let (_runtime_dir, runtime) = runtime_for(repo.path());
    let cache_path = runtime.diff_stats_path();
    let path = repo.path_str().to_owned();
    let now_ms = 1_000 + DIFF_STATS_TTL.as_millis() as u64 + 1;
    let mut cache = DiffStatsCache::default();
    cache.entries.insert(
        path.clone(),
        DiffStatsCacheEntry {
            refreshed_at_ms: 1_000,
            commit_refreshed_at_ms: Some(now_ms),
            added: Some(0),
            removed: Some(0),
            commits: Some(9),
            behind: Some(8),
            trunk: Some("main".to_owned()),
            branch: Some("main".to_owned()),
            clean: Some(true),
            landed: Some(false),
            did_work: Some(false),
            merge_in_progress: Some(false),
            ..DiffStatsCacheEntry::default()
        },
    );
    atomic::write_temp_then_rename_cache(&cache_path, &cache).unwrap();

    let refreshed = refresh_diff_stats(
        &cache_path,
        &runtime,
        std::slice::from_ref(&path),
        &BTreeSet::new(),
        &BTreeSet::from([path.clone()]),
        now_ms,
        None,
        &mut GitRefreshState::default(),
    );
    let entry = refreshed.entries.get(&path).unwrap();

    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            added: 1,
            removed: 0
        })
    );
    assert_eq!(
        entry.commits,
        Some(0),
        "equal non-focused TTLs force commit refresh with local refresh"
    );
    assert_eq!(entry.behind, Some(0));
    assert_eq!(entry.landed, Some(true));
    assert!(
        entry
            .commit_refreshed_at_ms
            .is_some_and(|stamp| stamp > now_ms),
        "commit facts stamp at completion, not at stale-check time"
    );
}

#[test]
fn diff_stats_refresh_stamps_fact_groups_at_completion() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);

    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);

    assert!(entry.refreshed_at_ms > 0);
    assert!(entry.commit_refreshed_at_ms.is_some_and(|stamp| stamp > 0));
}

#[test]
fn warm_unchanged_refs_skip_trunk_base_and_head_forks() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    let cold = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert!(cold.head_sha.is_some());
    assert!(cold.trunk_sha.is_some());
    assert!(cold.merge_base.is_some());

    let before = crate::proc::testkit::spawn_count();
    let warm = refresh_entry(
        repo.path_str(),
        Some(&cold),
        DueFacts {
            local: true,
            commit: false,
        },
        None,
    );

    assert_eq!(
        crate::proc::testkit::spawn_count() - before,
        2,
        "warm unchanged refs pay only diff and status"
    );
    assert_eq!(warm.branch.as_deref(), Some("main"));
    assert_eq!(warm.merge_base, cold.merge_base);
}

#[test]
fn warm_unchanged_head_skips_commit_forks() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    let base_ref = git_line(repo.path(), &["rev-parse", "HEAD"]).unwrap();
    write_rimz_worktree_marker(&repo, &base_ref);

    assert!(repo.git(&["checkout", "-q", "-b", "feature"]));
    repo.write("feature.txt", "feature\n");
    let _ = repo.git(&["add", "feature.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "feature"]);

    let cold = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(cold.clean, Some(true));
    assert_eq!(cold.commits, Some(1));
    assert_eq!(cold.behind, Some(0));
    assert_eq!(cold.landed, Some(false));
    assert_eq!(cold.did_work, Some(true));
    assert!(cold.head_sha.is_some());
    assert!(cold.trunk_sha.is_some());
    assert!(cold.merge_base.is_some());

    let before = crate::proc::testkit::spawn_count();
    let warm = refresh_entry(repo.path_str(), Some(&cold), DueFacts::all(), None);
    assert_eq!(
        crate::proc::testkit::spawn_count() - before,
        2,
        "unchanged HEAD pays only diff and status; commit facts carry forward"
    );
    assert_eq!(warm.commits, cold.commits);
    assert_eq!(warm.behind, cold.behind);
    assert_eq!(warm.landed, cold.landed);
    assert_eq!(warm.did_work, cold.did_work);

    repo.write("dirty.txt", "dirty\n");
    let before = crate::proc::testkit::spawn_count();
    let dirty = refresh_entry(repo.path_str(), Some(&warm), DueFacts::all(), None);
    assert!(
        crate::proc::testkit::spawn_count() - before > 2,
        "changed clean verdict re-probes commit facts even with stable HEAD"
    );
    assert_eq!(dirty.commits, Some(1));
    assert_eq!(dirty.landed, None);

    let before = crate::proc::testkit::spawn_count();
    let dirty_warm = refresh_entry(repo.path_str(), Some(&dirty), DueFacts::all(), None);
    assert_eq!(
        crate::proc::testkit::spawn_count() - before,
        2,
        "unchanged dirty HEAD still carries forward commit facts"
    );
    assert_eq!(dirty_warm.commits, dirty.commits);
    assert_eq!(dirty_warm.landed, dirty.landed);

    repo.write("second.txt", "second\n");
    let _ = repo.git(&["add", "dirty.txt", "second.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "second"]);
    let before = crate::proc::testkit::spawn_count();
    let moved = refresh_entry(repo.path_str(), Some(&dirty_warm), DueFacts::all(), None);
    assert!(
        crate::proc::testkit::spawn_count() - before > 2,
        "moved HEAD re-probes commit facts"
    );
    assert_eq!(moved.commits, Some(2));
}

#[test]
fn worktree_status_folds_untracked_into_churn_and_reads_clean() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    // `-b main` needs Git >= 2.28; an older git fails init and the helper
    // degrades to None, which is the documented fallback.
    if !repo.initialized {
        assert_eq!(
            refresh_entry(repo.path_str(), None, DueFacts::all(), None).clean,
            None
        );
        return;
    }
    repo.write("base.txt", "a\nb\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);

    // A pristine checkout at the trunk tip: proven clean, zero churn — the
    // exact reading the `≡` marker requires.
    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(entry.clean, Some(true));
    assert_eq!(entry.stats(), Some(DiffStats::default()));

    // An untracked two-line file nested in an untracked directory: invisible
    // to `git diff`, so the status probe must flag the tree dirty and fold
    // the lines into `+` (`--untracked-files=all` reaches inside the dir).
    std::fs::create_dir_all(repo.path().join("sub")).unwrap();
    repo.write("sub/notes.txt", "n1\nn2\n");
    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(entry.clean, Some(false));
    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            added: 2,
            removed: 0,
        }),
        "untracked lines count as churn"
    );

    // An untracked binary contributes no lines but still dirties the tree.
    std::fs::write(repo.path().join("blob.bin"), b"\x00\x01\x02").unwrap();
    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(entry.clean, Some(false));
    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            added: 2,
            removed: 0,
        }),
        "a binary blob dirties the tree without inventing a line count"
    );
}

#[test]
fn did_work_reads_head_against_rimz_worktree_marker_base_ref() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        assert_eq!(
            refresh_entry(repo.path_str(), None, DueFacts::all(), None).did_work,
            None
        );
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    let base_ref = git_line(repo.path(), &["rev-parse", "HEAD"]).unwrap();
    write_rimz_worktree_marker(&repo, &base_ref);

    assert_eq!(
        refresh_entry(repo.path_str(), None, DueFacts::all(), None).did_work,
        Some(false),
        "a fresh fork has not moved past its marker base_ref"
    );

    assert!(repo.git(&["checkout", "-q", "-b", "feature"]));
    repo.write("feature.txt", "feature\n");
    let _ = repo.git(&["add", "feature.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "feature"]);
    assert_eq!(
        refresh_entry(repo.path_str(), None, DueFacts::all(), None).did_work,
        Some(true),
        "a worktree commit is visible even when later ancestry collapses"
    );
}

#[test]
fn did_work_treats_rebased_empty_worktree_as_trunk_lineage() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        assert_eq!(
            refresh_entry(repo.path_str(), None, DueFacts::all(), None).did_work,
            None
        );
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    let base_ref = git_line(repo.path(), &["rev-parse", "HEAD"]).unwrap();
    write_rimz_worktree_marker(&repo, &base_ref);

    assert!(repo.git(&["checkout", "-q", "-b", "feature"]));
    assert!(repo.git(&["checkout", "-q", "main"]));
    repo.write("other.txt", "other\n");
    let _ = repo.git(&["add", "other.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "other"]);
    assert!(repo.git(&["checkout", "-q", "feature"]));
    assert!(repo.git(&["rebase", "main"]));

    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(
        entry.did_work,
        Some(false),
        "a rebase that only tracks trunk does not create own work"
    );
    assert_eq!(entry.commits, Some(0));
    assert_eq!(entry.behind, Some(0));

    assert!(repo.git(&["checkout", "-q", "main"]));
    repo.write("later.txt", "later\n");
    let _ = repo.git(&["add", "later.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "later"]);
    assert!(repo.git(&["checkout", "-q", "feature"]));

    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(
        entry.did_work,
        Some(false),
        "trunk advancing again must not turn tracked trunk history into work"
    );
    assert_eq!(entry.commits, Some(0));
    assert_eq!(entry.behind, Some(1));
}

#[test]
fn did_work_stays_true_for_no_ff_merged_branch_tip() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        assert_eq!(
            refresh_entry(repo.path_str(), None, DueFacts::all(), None).did_work,
            None
        );
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    let base_ref = git_line(repo.path(), &["rev-parse", "HEAD"]).unwrap();
    write_rimz_worktree_marker(&repo, &base_ref);

    assert!(repo.git(&["checkout", "-q", "-b", "feature"]));
    repo.write("feature.txt", "feature\n");
    let _ = repo.git(&["add", "feature.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "feature"]);
    assert!(repo.git(&["checkout", "-q", "main"]));
    assert!(repo.git(&["merge", "--no-ff", "feature", "-m", "merge feature"]));
    assert!(repo.git(&["checkout", "-q", "feature"]));

    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(
        entry.did_work,
        Some(true),
        "side-lineage branch tips remain own work after a no-ff merge"
    );
    assert_eq!(entry.landed, Some(true));
    assert_eq!(entry.commits, Some(0));
}

#[test]
fn merge_in_progress_reads_git_state_paths() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        assert_eq!(head_facts(repo.path()).merge_in_progress, None);
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);

    assert_eq!(head_facts(repo.path()).merge_in_progress, Some(false));

    let merge_head = git_line(repo.path(), &["rev-parse", "--git-path", "MERGE_HEAD"]).unwrap();
    let merge_head = Path::new(&merge_head);
    let merge_head = if merge_head.is_absolute() {
        merge_head.to_path_buf()
    } else {
        repo.path().join(merge_head)
    };
    std::fs::write(merge_head, "deadbeef\n").unwrap();

    assert_eq!(head_facts(repo.path()).merge_in_progress, Some(true));
}

#[test]
fn trunk_ladder_prefers_a_configured_branch_that_resolves() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        assert_eq!(trunk_ref(repo.path(), Some("develop")), None);
        return;
    }
    repo.write("f", "x");
    let _ = repo.git(&["add", "f"]);
    let _ = repo.git(&["commit", "-q", "-m", "init"]);
    let _ = repo.git(&["branch", "develop"]);

    // The configured branch exists here, so it wins over `main`.
    assert_eq!(
        trunk_ref(repo.path(), Some("develop")).as_deref(),
        Some("develop")
    );
    // A machine-wide preference this repo lacks falls through to detection
    // rather than losing the repo's stats.
    assert_eq!(
        trunk_ref(repo.path(), Some("absent")).as_deref(),
        Some("main")
    );
    // An option-shaped name is never handed to git; detection stands alone.
    assert_eq!(
        trunk_ref(repo.path(), Some("--help")).as_deref(),
        Some("main")
    );
    assert_eq!(trunk_ref(repo.path(), None).as_deref(), Some("main"));
}
