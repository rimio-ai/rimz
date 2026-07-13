use super::*;
use jiff::Timestamp;

#[test]
fn token_windows_and_native_sessions_populate_public_tallies() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let session = dir.path().join("sess-1");
    std::fs::create_dir_all(session.join("subagents")).unwrap();
    let main = write_jsonl(
        &session,
        "chat.jsonl",
        &[&format!(
            r#"{{"timestamp":"{today}T15:00:00.000Z","costUSD":0.5,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":12000,"output_tokens":64000,"cache_creation_input_tokens":12000,"cache_read_input_tokens":68000}}}}}}"#
        )],
    );
    let sub = write_jsonl(
        &session.join("subagents"),
        "worker.jsonl",
        &[&format!(
            r#"{{"timestamp":"{today}T15:01:00.000Z","costUSD":0.1,"requestId":"req-2","isSidechain":true,"message":{{"id":"msg-2","usage":{{"input_tokens":1000,"output_tokens":500,"cache_read_input_tokens":2000}}}}}}"#
        )],
    );
    let total = compute_total(&[main, sub], &mut SpendingDiskCache::default());
    assert_eq!(
        (
            total.headline.input,
            total.headline.output,
            total.headline.tokens,
            total.headline.cache_write,
            total.headline.cache_read,
            total.headline.sessions,
        ),
        (25_000, 64_500, 89_500, 12_000, 70_000, 1)
    );

    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;
    let aged = write_jsonl(
        dir.path(),
        "aged.jsonl",
        &[
            &claude_line_ago(2 * HOUR, 1.0, "msg-1", "req-1"),
            &claude_line_ago(3 * DAY, 0.5, "msg-2", "req-2"),
            &claude_line_ago(20 * DAY, 0.25, "msg-3", "req-3"),
            &claude_line_ago(100 * DAY, 0.1, "msg-4", "req-4"),
            &claude_line_ago(400 * DAY, 9.0, "msg-5", "req-5"),
        ],
    );
    let aged = compute_total(&[aged], &mut SpendingDiskCache::default());
    assert_eq!(
        (
            aged.headline.tokens,
            aged.week.tokens,
            aged.month.tokens,
            aged.year.tokens,
        ),
        (15, 30, 45, 60)
    );
    assert!((aged.headline.usd - 1.0).abs() < 1e-9);
    assert!((aged.week.usd - 1.5).abs() < 1e-9);
    assert!((aged.month.usd - 1.75).abs() < 1e-9);
    assert!((aged.year.usd - 1.85).abs() < 1e-9);

    let native_file = PathBuf::from("/x/opencode.db");
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file(
            &native_file,
            vec![
                cached_entry(NOW_SECS, 0.01, "session-a"),
                cached_entry(NOW_SECS - 2 * DAY, 0.01, "session-b"),
                cached_entry(NOW_SECS - 3 * DAY, 0.01, "session-b"),
            ],
        )]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![(opencode_adapter(), native_file)];
    let counted = dedup_cached_entries(&files, &cache, &HashSet::new()).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());
    assert_eq!(spending.total.headline.sessions, 1);
    assert_eq!(spending.total.week.sessions, 2);
    assert_eq!(spending.by_provider["opencode"].week.sessions, 2);
}

#[test]
fn headline_cutoffs_are_global_scoped_and_provider_local() {
    const HOUR: u64 = 3_600;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let other = dir.path().join("other");
    let claude_file = write_jsonl(dir.path(), "claude.jsonl", &[]);
    let codex_file = write_codex(dir.path(), &[]);
    let day_start = (NOW_SECS / 86_400) * 86_400;
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), claude_file.clone()),
        (codex_adapter(), codex_file.clone()),
    ];

    let today_cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(
                &claude_file,
                &project,
                vec![
                    cached_entry(day_start - 1, 1.0, "before-midnight"),
                    cached_entry(day_start + 60, 2.0, "after-midnight"),
                ],
            ),
            cached_file_with_origin(
                &codex_file,
                &other,
                vec![cached_entry(day_start + 60, 4.0, "outside")],
            ),
        ]),
        ..Default::default()
    };
    let today_spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };
    let counted = dedup_cached_entries(&files, &today_cache, &HashSet::new()).into_counted();
    let global = aggregate_spending(&files, &today_cache, &counted, NOW_SECS, &today_spec);
    let scoped = compute_scoped_tally(
        &files,
        &today_cache,
        &SpendScope::from_roots(Some(&project), &[]),
        NOW_SECS,
        &today_spec,
    );
    assert!((global.total.headline.usd - 6.0).abs() < 1e-9);
    assert!((scoped.headline.usd - 2.0).abs() < 1e-9);
    assert!((scoped.week.usd - 3.0).abs() < 1e-9);

    let mut session_cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(
                &claude_file,
                vec![
                    cached_entry(NOW_SECS - 8 * HOUR, 3.0, "claude"),
                    cached_entry(NOW_SECS, 1.0, "claude"),
                ],
            ),
            cached_file(
                &codex_file,
                vec![cached_entry(NOW_SECS - 4 * HOUR, 2.0, "codex")],
            ),
        ]),
        ..Default::default()
    };
    let session_spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };
    let (spending, _) = compute_spending_with_origins_and_scope(
        &files,
        &mut session_cache,
        &PriceBook::default(),
        NOW_SECS,
        &HashMap::new(),
        None,
        &session_spec,
    );
    assert!((spending.total.headline.usd - 6.0).abs() < 1e-9);
    assert!((spending.by_provider["claude"].headline.usd - 1.0).abs() < 1e-9);
    assert!((spending.by_provider["codex"].headline.usd - 2.0).abs() < 1e-9);

    let idle = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &claude_file,
            &project,
            vec![
                cached_entry(NOW_SECS - 8 * HOUR, 2.0, "idle"),
                cached_entry(NOW_SECS - 7 * HOUR, 3.0, "idle"),
            ],
        )]),
        ..Default::default()
    };
    let scoped = compute_scoped_tally(
        &files,
        &idle,
        &SpendScope::from_roots(Some(&project), &[]),
        NOW_SECS,
        &session_spec,
    );
    assert_eq!(scoped.headline.usd, 0.0);
    assert!((scoped.week.usd - 5.0).abs() < 1e-9);
}

#[test]
fn local_day_rollups_ignore_headline_mode_and_exclude_live_workspace_usd() {
    let now: Timestamp = "2025-06-01T04:30:00Z".parse().expect("now");
    let before: Timestamp = "2025-06-01T03:59:00Z".parse().expect("before midnight");
    let after: Timestamp = "2025-06-01T04:01:00Z".parse().expect("after midnight");
    let project = PathBuf::from("/repo/project");
    let file = PathBuf::from("/tmp/rimz/day.jsonl");
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![(claude_adapter(), file.clone())];
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &file,
            &project,
            vec![
                cached_entry(before.as_second() as u64, 1.0, "before"),
                cached_entry(after.as_second() as u64, 2.0, "live"),
            ],
        )]),
        ..Default::default()
    };
    let counted = dedup_cached_entries(&files, &cache, &HashSet::new()).into_counted();
    let live_excluded = BTreeSet::from(["claude:live".to_owned()]);
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: Some("America/New_York".to_owned()),
    };
    let rollups = aggregate_counted_rollups(
        &files,
        &cache,
        &counted,
        Some(WorkspaceRollupScope {
            scope: &scope,
            live_excluded: &live_excluded,
        }),
        now.as_second() as u64,
        &spec,
        false,
    );

    assert_eq!(
        rollups.day_cutoff_secs,
        "2025-06-01T04:00:00Z"
            .parse::<Timestamp>()
            .expect("cutoff")
            .as_second() as u64
    );
    assert!((rollups.provider_day["claude"].usd - 2.0).abs() < 1e-9);
    assert_eq!(rollups.workspace_day.usd, 0.0);
    assert_eq!(rollups.workspace_day.tokens, 15);
    assert!((rollups.workspace_tally.headline.usd - 1.0).abs() < 1e-9);
}

#[test]
fn live_exclusion_suppresses_workspace_headline_usd_only() {
    let project = PathBuf::from("/repo/project");
    let file = PathBuf::from("/tmp/rimz/live.jsonl");
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![(claude_adapter(), file.clone())];
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &file,
            &project,
            vec![cached_entry(NOW_SECS, 1.25, "live")],
        )]),
        ..Default::default()
    };
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let live_excluded = BTreeSet::from(["claude:live".to_owned()]);
    let scoped = compute_scoped_spending(
        &files,
        &cache,
        &HashSet::new(),
        &scope,
        &live_excluded,
        NOW_SECS,
        &HeadlineSpec::default(),
    )
    .tally;
    assert_eq!(scoped.headline.usd, 0.0);
    assert_eq!(scoped.headline.tokens, 15);
    assert_eq!(scoped.headline.sessions, 1);
    assert!((scoped.week.usd - 1.25).abs() < 1e-9);

    let session = PathBuf::from("/tmp/claude/sess-1");
    let main = session.join("chat.jsonl");
    let sub = session.join("subagents/worker.jsonl");
    let mut main_entry = cached_entry(NOW_SECS, 0.50, "");
    main_entry.thread_id = None;
    let mut sub_entry = cached_entry(NOW_SECS, 0.10, "");
    sub_entry.thread_id = None;
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), main.clone()),
        (claude_adapter(), sub.clone()),
    ];
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(&main, &project, vec![main_entry]),
            cached_file_with_origin(&sub, &project, vec![sub_entry]),
        ]),
        ..Default::default()
    };
    let live_excluded = live_session_keys(claude_adapter(), "sess-1", &main)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let scoped = compute_scoped_spending(
        &files,
        &cache,
        &HashSet::new(),
        &scope,
        &live_excluded,
        NOW_SECS,
        &HeadlineSpec::default(),
    )
    .tally;
    assert_eq!(scoped.headline.usd, 0.0);
    assert_eq!(scoped.headline.tokens, 30);
    assert_eq!(scoped.headline.sessions, 1);
    assert!((scoped.week.usd - 0.60).abs() < 1e-9);

    let codex_file = PathBuf::from("/tmp/codex/rollout.jsonl");
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> =
        vec![(codex_adapter(), codex_file.clone())];
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &codex_file,
            &project,
            vec![
                cached_entry(NOW_SECS, 2.00, "live"),
                cached_entry(NOW_SECS, 3.00, "sibling"),
            ],
        )]),
        ..Default::default()
    };
    let live_excluded = live_session_keys(codex_adapter(), "live", &codex_file)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let scoped = compute_scoped_spending(
        &files,
        &cache,
        &HashSet::new(),
        &scope,
        &live_excluded,
        NOW_SECS,
        &HeadlineSpec::default(),
    )
    .tally;
    assert!((scoped.headline.usd - 3.00).abs() < 1e-9);
    assert_eq!(scoped.headline.tokens, 30);
    assert!((scoped.week.usd - 5.00).abs() < 1e-9);
}

#[test]
fn automation_entries_do_not_bridge_session_idle_gaps() {
    const HOUR: u64 = 3_600;
    let first_file = PathBuf::from("/tmp/rimz/first.jsonl");
    let automation_file = PathBuf::from("/tmp/rimz/automation.jsonl");
    let second_file = PathBuf::from("/tmp/rimz/second.jsonl");
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), first_file.clone()),
        (claude_adapter(), automation_file.clone()),
        (claude_adapter(), second_file.clone()),
    ];
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(
                &first_file,
                vec![cached_entry(NOW_SECS - 9 * HOUR, 1.0, "session")],
            ),
            cached_file(
                &automation_file,
                vec![cached_entry(NOW_SECS - 4 * HOUR - HOUR / 2, 0.5, "session")],
            ),
            cached_file(&second_file, vec![cached_entry(NOW_SECS, 2.0, "session")]),
        ]),
        ..Default::default()
    };
    let automation_files = HashSet::from([automation_file]);
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };
    let baseline_counted = dedup_cached_entries(&files, &cache, &HashSet::new()).into_counted();
    let tagged_counted = dedup_cached_entries(&files, &cache, &automation_files).into_counted();

    let baseline = aggregate_spending(&files, &cache, &baseline_counted, NOW_SECS, &spec);
    let tagged = aggregate_spending(&files, &cache, &tagged_counted, NOW_SECS, &spec);

    assert!((baseline.total.headline.usd - 3.5).abs() < 1e-9);
    assert!((tagged.total.headline.usd - 2.0).abs() < 1e-9);
    assert_eq!(tagged.total.headline.tokens, 15);
    assert!((tagged.total.week.usd - 3.5).abs() < 1e-9);
    assert!((tagged.total.month.usd - 3.5).abs() < 1e-9);
    assert!((tagged.total.year.usd - 3.5).abs() < 1e-9);
    assert_eq!(tagged.total.year.tokens, 45);
    assert_eq!(
        tagged.total.headline.sessions,
        baseline.total.headline.sessions
    );
}

#[test]
fn automation_inside_human_burst_stays_in_session_window() {
    const HOUR: u64 = 3_600;
    let first_file = PathBuf::from("/tmp/rimz/burst-first.jsonl");
    let automation_file = PathBuf::from("/tmp/rimz/burst-automation.jsonl");
    let second_file = PathBuf::from("/tmp/rimz/burst-second.jsonl");
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), first_file.clone()),
        (claude_adapter(), automation_file.clone()),
        (claude_adapter(), second_file.clone()),
    ];
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(
                &first_file,
                vec![cached_entry(NOW_SECS - HOUR, 1.0, "human-a")],
            ),
            cached_file(
                &automation_file,
                vec![cached_entry(NOW_SECS - HOUR / 2, 0.5, "automation")],
            ),
            cached_file(&second_file, vec![cached_entry(NOW_SECS, 2.0, "human-b")]),
        ]),
        ..Default::default()
    };
    let automation_files = HashSet::from([automation_file]);
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };
    let baseline_counted = dedup_cached_entries(&files, &cache, &HashSet::new()).into_counted();
    let tagged_counted = dedup_cached_entries(&files, &cache, &automation_files).into_counted();

    let baseline = aggregate_spending(&files, &cache, &baseline_counted, NOW_SECS, &spec);
    let tagged = aggregate_spending(&files, &cache, &tagged_counted, NOW_SECS, &spec);

    assert_eq!(tagged.total.headline, baseline.total.headline);
    assert!((tagged.total.headline.usd - 3.5).abs() < 1e-9);
    assert_eq!(tagged.total.headline.tokens, 45);
    assert_eq!(tagged.total.headline.sessions, 3);
}

#[test]
fn workspace_scope_uses_roots_worktree_home_and_file_origin() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let ts = format!("{today}T15:00:00.000Z");
    let project = dir.path().join("repo");
    let linked = dir.path().join("linked-worktree");
    let home = dir.path().join("repo-worktrees");
    let removed = home.join("budget-reset");
    let other = dir.path().join("other-project");
    let mut files = Vec::new();

    for (name, cwd, usd) in [
        ("project", project.join("src"), 1.0),
        ("linked", linked.clone(), 2.0),
        ("removed", removed.join("crates/rimz"), 3.0),
        ("other", other.clone(), 4.0),
    ] {
        let session = dir.path().join(format!("sessions/{name}"));
        std::fs::create_dir_all(&session).unwrap();
        files.push((
            claude_adapter(),
            write_jsonl(
                &session,
                "chat.jsonl",
                &[&claude_line_ts_in(
                    &ts,
                    usd,
                    &format!("msg-{name}"),
                    &format!("req-{name}"),
                    &cwd,
                )],
            ),
        ));
    }

    let origin_file = write_jsonl(
        dir.path(),
        "origin.jsonl",
        &[
            &format!(
                r#"{{"timestamp":"{today}T15:00:00.000Z","cwd":null,"costUSD":0.5,"requestId":"req-origin-a","message":{{"id":"msg-origin-a","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
            ),
            &claude_line_ts_in(
                &format!("{today}T15:01:00.000Z"),
                0.25,
                "msg-origin-b",
                "req-origin-b",
                &project,
            ),
        ],
    );
    files.push((claude_adapter(), origin_file.clone()));

    let mut cache = SpendingDiskCache::default();
    let scope =
        SpendScope::for_workspace(Some(&project), std::slice::from_ref(&linked), Some(&home));
    let (global, scoped) = compute_spending_with_origins_and_scope(
        &files,
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
        &HashMap::new(),
        Some(&scope),
        &HeadlineSpec::default(),
    );
    assert!((global.total.headline.usd - 10.75).abs() < 1e-9);
    assert!((scoped.headline.usd - 6.75).abs() < 1e-9);
    assert_eq!(scoped.headline.sessions, 4);
    assert_eq!(
        cache.files[&origin_file.to_string_lossy().into_owned()]
            .origin_path
            .as_deref(),
        Some(project.as_path())
    );

    let live_only = SpendScope::from_roots(Some(&project), std::slice::from_ref(&linked));
    let scoped = compute_scoped_tally(
        &files,
        &cache,
        &live_only,
        NOW_SECS,
        &HeadlineSpec::default(),
    );
    assert!((scoped.headline.usd - 3.75).abs() < 1e-9);
}

#[test]
fn claude_replay_dedup_collapses_before_store_and_rollups() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let main_line = claude_line(&today, 1.0, "msg-a", "req-a");
    let replay_line = claude_sidechain_line(&today, 9.0, "msg-a", "req-a");
    let mixed = write_jsonl(dir.path(), "mixed.jsonl", &[&replay_line, &main_line]);
    let duplicate = write_jsonl(dir.path(), "duplicate.jsonl", &[&main_line]);
    let lone_sidechain = write_jsonl(
        dir.path(),
        "sidechain-only.jsonl",
        &[&claude_sidechain_line(&today, 0.20, "msg-x", "req-x")],
    );

    let mut cache = SpendingDiskCache::default();
    let total = compute_total(&[mixed.clone(), duplicate], &mut cache);
    assert!((total.headline.usd - 1.0).abs() < 1e-9);
    assert_eq!(total.headline.tokens, 15);
    let stored = &cache.files[&mixed.to_string_lossy().into_owned()].entries;
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].is_sidechain);
    assert_eq!(stored[0].input, 10);

    let kept = compute_total(&[lone_sidechain], &mut SpendingDiskCache::default());
    assert!((kept.headline.usd - 0.20).abs() < 1e-9);
    assert_eq!(kept.headline.tokens, 50_005);
}

#[test]
fn claude_duplicate_dedup_keeps_the_richest_main_thread_record() {
    let dir = TempDir::new().unwrap();
    let paths = [
        "sidechain.jsonl",
        "small.jsonl",
        "large.jsonl",
        "fast.jsonl",
    ]
    .map(|name| write_jsonl(dir.path(), name, &[]));
    let base = CachedEntry {
        ts_secs: NOW_SECS,
        cost_usd: 9.0,
        input: 9_000,
        output: 0,
        cache_write: 0,
        cache_read: 0,
        message_id: Some("msg-rich".to_owned()),
        request_id: Some("req-rich".to_owned()),
        dedup_key: None,
        thread_id: None,
        is_sidechain: true,
        has_speed: false,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: false,
    };
    let small = CachedEntry {
        cost_usd: 1.0,
        input: 10,
        is_sidechain: false,
        ..base.clone()
    };
    let large = CachedEntry {
        cost_usd: 2.0,
        input: 20,
        ..small.clone()
    };
    let fast = CachedEntry {
        cost_usd: 4.0,
        has_speed: true,
        ..large.clone()
    };
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(&paths[0], vec![base]),
            cached_file(&paths[1], vec![small]),
            cached_file(&paths[2], vec![large]),
            cached_file(&paths[3], vec![fast]),
        ]),
        ..Default::default()
    };
    let files = paths
        .into_iter()
        .map(|path| (claude_adapter(), path))
        .collect::<Vec<_>>();

    let counted = dedup_cached_entries(&files, &cache, &HashSet::new()).into_counted();
    assert_eq!(counted.len(), 1);
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());
    assert_eq!(spending.total.headline.input, 20);
    assert_eq!(spending.total.headline.usd, 4.0);
}

#[test]
fn codex_cross_file_dedup_uses_the_exact_native_event_fingerprint() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let duplicate = codex_token_line(&today, 1_000, 400, 500);
    let distinct_millisecond = duplicate.replace("15:00:00.000Z", "15:00:00.001Z");
    let model = r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#;
    let files = [
        write_codex(&dir.path().join("one"), &[model, &duplicate]),
        write_codex(&dir.path().join("two"), &[model, &duplicate]),
        write_codex(&dir.path().join("three"), &[model, &distinct_millisecond]),
    ];
    let tagged = files
        .into_iter()
        .map(|path| (codex_adapter(), path))
        .collect::<Vec<_>>();

    let spending = compute_spending(
        &tagged,
        &mut SpendingDiskCache::default(),
        &gpt4o_book(),
        NOW_SECS,
    );
    assert_eq!(spending.total.headline.input, 1_200);
    assert_eq!(spending.total.headline.cache_read, 800);
    assert_eq!(spending.total.headline.output, 1_000);
    assert_eq!(
        spending.total.headline.sessions, 3,
        "event dedup preserves the fact that each session ran"
    );
    assert!((spending.total.headline.usd - 0.003_28).abs() < 1e-9);
}

#[test]
fn codex_resume_pricing_provider_origin_and_unknown_heal_stay_intact() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let resumable = write_codex(
        dir.path(),
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
            &codex_total_line(&today, 1000, 500),
        ],
    );
    let mut cache = SpendingDiskCache::default();
    let first = compute_spending(
        &[(codex_adapter(), resumable.clone())],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    append_line(&resumable, &codex_total_line(&today, 1600, 800));
    let second = compute_spending(
        &[(codex_adapter(), resumable)],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    assert_eq!(
        (first.total.headline.input, first.total.headline.output),
        (1000, 500)
    );
    assert_eq!(
        (second.total.headline.input, second.total.headline.output),
        (1600, 800)
    );

    let project = dir.path().join("repo");
    let claude_file = write_jsonl(
        dir.path(),
        "claude.jsonl",
        &[&claude_line(&today, 0.5, "msg-1", "req-1")],
    );
    let codex_file = write_codex(
        dir.path(),
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
            &codex_token_line(&today, 1000, 400, 500),
        ],
    );
    let files = vec![(codex_adapter(), codex_file.clone())];
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let mut cache = SpendingDiskCache::default();
    let unpriced = PriceBook::from_litellm_json("{}");
    let first =
        compute_spending_with_origins(&files, &mut cache, &unpriced, NOW_SECS, &HashMap::new());
    assert_eq!(first.total.headline.usd, 0.0);
    assert_eq!(
        (
            first.total.headline.input,
            first.total.headline.output,
            first.total.headline.cache_read,
            first.total.headline.tokens,
        ),
        (600, 500, 400, 1_100)
    );
    assert!(
        compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default()).is_zero()
    );

    let _ = compute_spending_with_origins(
        &files,
        &mut cache,
        &unpriced,
        NOW_SECS,
        &HashMap::from([(codex_file.clone(), project.clone())]),
    );
    let cache_key = codex_file.to_string_lossy().into_owned();
    cache.dirty = false;
    let healed =
        compute_spending_with_origins(&files, &mut cache, &gpt4o_book(), NOW_SECS, &HashMap::new());
    let scoped = compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default());
    assert!((healed.total.headline.usd - 0.00164).abs() < 1e-9);
    assert!(cache.files[&cache_key].unknown_models.is_empty());
    assert!(cache.dirty);
    assert!((scoped.headline.usd - 0.00164).abs() < 1e-9);
    assert_eq!(
        cache.files[&cache_key].origin_path.as_deref(),
        Some(project.as_path())
    );

    let spending = compute_spending(
        &[
            (claude_adapter(), claude_file),
            (codex_adapter(), codex_file),
        ],
        &mut SpendingDiskCache::default(),
        &gpt4o_book(),
        NOW_SECS,
    );
    let codex = &spending.by_provider["codex"];
    assert!((codex.headline.usd - 0.00164).abs() < 1e-9);
    assert_eq!(
        (
            codex.headline.input,
            codex.headline.output,
            codex.headline.cache_read
        ),
        (600, 500, 400)
    );
    assert!((spending.by_provider["claude"].headline.usd - 0.5).abs() < 1e-9);
    assert!((spending.total.headline.usd - 0.50164).abs() < 1e-9);
}

#[test]
fn daily_and_model_rollups_share_the_dedup_pass() {
    let main = CachedEntry {
        ts_secs: NOW_SECS,
        cost_usd: 1.0,
        input: 100,
        output: 50,
        cache_write: 10,
        cache_read: 70,
        message_id: Some("msg-1".to_owned()),
        request_id: Some("req-1".to_owned()),
        dedup_key: None,
        thread_id: None,
        is_sidechain: false,
        has_speed: false,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: false,
    };
    let replay = CachedEntry {
        is_sidechain: true,
        cost_usd: 9.0,
        input: 9_000,
        cache_read: 900,
        ..main.clone()
    };
    let codex = CachedEntry {
        ts_secs: NOW_SECS - 2 * 86_400,
        cost_usd: 2.0,
        input: 200,
        output: 20,
        cache_write: 0,
        cache_read: 40,
        message_id: None,
        request_id: None,
        dedup_key: None,
        thread_id: None,
        is_sidechain: false,
        has_speed: false,
        model: Some("gpt-5-codex".to_owned()),
        rolled: false,
    };
    let old = CachedEntry {
        ts_secs: NOW_SECS - 40 * 86_400,
        cost_usd: 3.0,
        input: 300,
        output: 30,
        cache_read: 60,
        model: Some("gpt-5-old".to_owned()),
        ..codex.clone()
    };
    let claude_file = PathBuf::from("/x/claude.jsonl");
    let codex_file = PathBuf::from("/x/codex.jsonl");
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(&claude_file, vec![main, replay]),
            cached_file(&codex_file, vec![codex, old]),
        ]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), claude_file),
        (codex_adapter(), codex_file),
    ];

    let daily = compute_daily_spend(&files, &cache);
    assert_eq!(daily[&((NOW_SECS / 86_400) as i64)].tokens, 230);
    assert_eq!(
        daily[&(((NOW_SECS - 2 * 86_400) / 86_400) as i64)].tokens,
        260
    );
    assert_eq!(
        daily[&(((NOW_SECS - 40 * 86_400) / 86_400) as i64)].tokens,
        390
    );
    let by_model = compute_model_breakdown(&files, &cache, NOW_SECS);
    assert_eq!(by_model["claude-opus-4-8"].year.tokens, 160);
    assert_eq!(by_model["gpt-5-codex"].year.tokens, 220);
    assert_eq!(by_model["gpt-5-old"].year.tokens, 330);
    assert_eq!(by_model["gpt-5-old"].month.tokens, 0);
}
