use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;

use tempfile::TempDir;

const NOW_SECS: u64 = 1_750_000_000;

fn claude_adapter() -> &'static dyn AgentAdapter {
    &crate::agents::ClaudeAdapter
}

fn codex_adapter() -> &'static dyn AgentAdapter {
    &crate::agents::CodexAdapter
}

fn opencode_adapter() -> &'static dyn AgentAdapter {
    &crate::agents::OpencodeAdapter
}

fn compute_spending(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
    now_secs: u64,
) -> Spending {
    compute_spending_with_origins(files, cache, prices, now_secs, &HashMap::new())
}

fn compute_spending_with_origins(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
    now_secs: u64,
    origin_overrides: &HashMap<PathBuf, PathBuf>,
) -> Spending {
    let spec = HeadlineSpec::default();
    compute_spending_with_origins_and_scope(
        files,
        cache,
        prices,
        now_secs,
        origin_overrides,
        None,
        &spec,
    )
    .0
}

fn compute_spending_with_origins_and_scope(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
    now_secs: u64,
    origin_overrides: &HashMap<PathBuf, PathBuf>,
    scope: Option<&SpendScope>,
    spec: &HeadlineSpec,
) -> (Spending, SpendTally) {
    let mut tick = |_: &SpendingDiskCache, _: SpendProgress| {};
    let mut on_jobs_scheduled = |_: &WalkStats| {};
    let mut stats = WalkStats::default();
    refresh_spending_cache(
        files,
        cache,
        prices,
        now_secs,
        origin_overrides,
        &mut stats,
        &mut RefreshCallbacks {
            on_jobs_scheduled: &mut on_jobs_scheduled,
            tick: &mut tick,
        },
    );
    let counted = dedup_cached_entries(files, cache).into_counted();
    let aggregate = aggregate_counted_rollups(files, cache, &counted, scope, now_secs, spec, false);
    (aggregate.spending, aggregate.workspace_tally)
}

fn aggregate_spending(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    counted: &[impl CountedPayload],
    now_secs: u64,
    spec: &HeadlineSpec,
) -> Spending {
    aggregate_counted_rollups(files, cache, counted, None, now_secs, spec, false).spending
}

fn compute_daily_spend(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
) -> BTreeMap<i64, DaySpend> {
    let counted = dedup_cached_entries(files, cache).into_counted();
    aggregate_counted_rollups(
        files,
        cache,
        &counted,
        None,
        NOW_SECS,
        &HeadlineSpec::default(),
        true,
    )
    .days
}

fn compute_model_breakdown(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    now_secs: u64,
) -> BTreeMap<String, SpendTally> {
    let counted = dedup_cached_entries(files, cache).into_counted();
    aggregate_counted_rollups(
        files,
        cache,
        &counted,
        None,
        now_secs,
        &HeadlineSpec::default(),
        true,
    )
    .models
}

fn compute_scoped_tally(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    scope: &SpendScope,
    now_secs: u64,
    spec: &HeadlineSpec,
) -> SpendTally {
    compute_scoped_spending(files, cache, scope, now_secs, spec).tally
}

fn compute_total(files: &[PathBuf], cache: &mut SpendingDiskCache) -> SpendTally {
    let tagged: Vec<(&'static dyn AgentAdapter, PathBuf)> = files
        .iter()
        .map(|file| (claude_adapter(), file.clone()))
        .collect();
    compute_spending(&tagged, cache, &PriceBook::default(), NOW_SECS).total
}

fn model_tally(tokens: u64, usd: f64, input: u64, output: u64, cache_read: u64) -> SpendTally {
    SpendTally {
        year: SpendWindow {
            usd,
            tokens,
            input,
            output,
            cache_read,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn iso_at(secs: u64) -> String {
    let date = utc_date(secs);
    let tod = secs % 86_400;
    format!(
        "{date}T{:02}:{:02}:{:02}.000Z",
        tod / 3_600,
        (tod % 3_600) / 60,
        tod % 60
    )
}

fn claude_line_ts(ts: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
    format!(
        r#"{{"timestamp":"{ts}","costUSD":{cost},"requestId":"{req_id}","message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
    )
}

fn claude_line_ts_in(ts: &str, cost: f64, msg_id: &str, req_id: &str, cwd: &Path) -> String {
    format!(
        r#"{{"timestamp":"{ts}","cwd":"{}","costUSD":{cost},"requestId":"{req_id}","message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#,
        cwd.display()
    )
}

fn claude_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
    claude_line_ts(&format!("{date}T15:00:00.000Z"), cost, msg_id, req_id)
}

fn claude_line_ago(secs_ago: u64, cost: f64, msg_id: &str, req_id: &str) -> String {
    claude_line_ts(
        &iso_at(NOW_SECS.saturating_sub(secs_ago)),
        cost,
        msg_id,
        req_id,
    )
}

fn claude_sidechain_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
    format!(
        r#"{{"timestamp":"{date}T15:00:00.000Z","costUSD":{cost},"requestId":"{req_id}","isSidechain":true,"message":{{"id":"{msg_id}","usage":{{"input_tokens":50000,"output_tokens":5}}}}}}"#
    )
}

fn write_jsonl(dir: &Path, filename: &str, lines: &[&str]) -> PathBuf {
    let path = dir.join(filename);
    let mut f = std::fs::File::create(&path).unwrap();
    for line in lines {
        writeln!(f, "{line}").unwrap();
    }
    path
}

fn append_line(path: &Path, line: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn set_file_mtime(path: &Path, secs: u64) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap();
}

fn codex_total_line(date: &str, input: u64, output: u64) -> String {
    format!(
        r#"{{"type":"event_msg","timestamp":"{date}T15:00:00.000Z","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}}}"#
    )
}

fn codex_token_line(date: &str, input: u64, cached: u64, output: u64) -> String {
    format!(
        r#"{{"type":"event_msg","timestamp":"{date}T15:00:00.000Z","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}}}}}}}}"#
    )
}

fn write_codex(dir: &Path, lines: &[&str]) -> PathBuf {
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    write_jsonl(&sessions, "sess.jsonl", lines)
}

fn gpt4o_book() -> PriceBook {
    PriceBook::from_litellm_json(
        r#"{"gpt-4o": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                           "cache_read_input_token_cost": 1e-7}}"#,
    )
}

fn cached_entry(ts_secs: u64, cost_usd: f64, thread_id: &str) -> CachedEntry {
    CachedEntry {
        ts_secs,
        cost_usd,
        input: 10,
        output: 5,
        cache_write: 0,
        cache_read: 0,
        message_id: None,
        request_id: None,
        thread_id: Some(thread_id.to_owned()),
        is_sidechain: false,
        model: None,
        rolled: false,
    }
}

fn cached_file(path: &Path, entries: Vec<CachedEntry>) -> (String, FileCacheEntry) {
    let (mtime_secs, len) = file_stat(path);
    let mtime_secs = entries
        .iter()
        .map(|entry| entry.ts_secs)
        .max()
        .map_or(mtime_secs, |entry_mtime| mtime_secs.max(entry_mtime));
    (
        path.to_string_lossy().into_owned(),
        FileCacheEntry {
            mtime_secs,
            len,
            cursor: SpendCursor::default(),
            origin_path: None,
            entries,
            unknown_models: BTreeMap::new(),
        },
    )
}

fn cached_file_with_origin(
    path: &Path,
    origin: &Path,
    entries: Vec<CachedEntry>,
) -> (String, FileCacheEntry) {
    let (key, mut file) = cached_file(path, entries);
    file.origin_path = Some(origin.to_path_buf());
    (key, file)
}

#[test]
fn cache_hit_skips_io_and_version_gate_discards_old_entries() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[&claude_line(&today, 0.5, "msg-1", "req-1")],
    );

    let mut cache = SpendingDiskCache::default();
    let first = compute_total(std::slice::from_ref(&file), &mut cache);
    assert_eq!(first.headline.tokens, 15);
    assert!(cache.dirty);

    cache.dirty = false;
    let second = compute_total(&[file], &mut cache);
    assert_eq!(second.headline.usd, first.headline.usd);
    assert!(!cache.dirty, "unchanged files should be served from cache");

    let path = dir.path().join("spending.json");
    let stale = SpendingDiskCache {
        version: SPENDING_CACHE_VERSION - 1,
        files: HashMap::from([(
            "/old/chat.jsonl".to_string(),
            FileCacheEntry {
                mtime_secs: 123,
                len: 0,
                cursor: SpendCursor::default(),
                origin_path: None,
                entries: vec![CachedEntry {
                    ts_secs: NOW_SECS,
                    cost_usd: 9.0,
                    input: 0,
                    output: 0,
                    cache_write: 0,
                    cache_read: 0,
                    message_id: Some("msg-old".to_string()),
                    request_id: Some("req-old".to_string()),
                    thread_id: None,
                    is_sidechain: false,
                    model: None,
                    rolled: false,
                }],
                unknown_models: BTreeMap::new(),
            },
        )]),
        ..Default::default()
    };
    write_spending_cache(&path, &stale);

    let healed = read_spending_cache(&path);
    assert_eq!(healed.version, SPENDING_CACHE_VERSION);
    assert!(healed.files.is_empty());
}

#[test]
fn token_split_and_session_counts_populate_windows() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let session = dir.path().join("sess-1");
    std::fs::create_dir_all(session.join("subagents")).unwrap();
    let main_line = format!(
        r#"{{"timestamp":"{today}T15:00:00.000Z","costUSD":0.5,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":12000,"output_tokens":64000,"cache_creation_input_tokens":12000,"cache_read_input_tokens":68000}}}}}}"#
    );
    let sub_line = format!(
        r#"{{"timestamp":"{today}T15:01:00.000Z","costUSD":0.1,"requestId":"req-2","isSidechain":true,"message":{{"id":"msg-2","usage":{{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":0,"cache_read_input_tokens":2000}}}}}}"#
    );
    let main = write_jsonl(&session, "chat.jsonl", &[&main_line]);
    let subfile = write_jsonl(&session.join("subagents"), "worker.jsonl", &[&sub_line]);

    let mut cache = SpendingDiskCache::default();
    let total = compute_total(&[main, subfile], &mut cache);

    assert_eq!(total.headline.input, 25_000);
    assert_eq!(total.headline.output, 64_500);
    assert_eq!(total.headline.tokens, 89_500);
    assert_eq!(total.headline.cache_write, 12_000);
    assert_eq!(total.headline.cache_read, 70_000);
    assert_eq!(total.headline.sessions, 1);
    assert_eq!(total.year.sessions, 1);
}

#[test]
fn native_thread_ids_count_many_sessions_in_one_store() {
    let file = PathBuf::from("/x/opencode.db");
    let entry = |thread_id: &str, ts_secs: u64| CachedEntry {
        ts_secs,
        cost_usd: 0.01,
        input: 10,
        output: 5,
        cache_write: 0,
        cache_read: 0,
        message_id: None,
        request_id: None,
        thread_id: Some(thread_id.to_owned()),
        is_sidechain: false,
        model: Some("gpt-5".to_owned()),
        rolled: false,
    };
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file(
            &file,
            vec![
                entry("session-a", NOW_SECS),
                entry("session-b", NOW_SECS - 2 * 86_400),
                entry("session-b", NOW_SECS - 3 * 86_400),
            ],
        )]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![(opencode_adapter(), file)];

    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());

    assert_eq!(spending.total.headline.sessions, 1);
    assert_eq!(spending.total.week.sessions, 2);
    assert_eq!(spending.total.year.sessions, 2);
    assert_eq!(spending.by_provider["opencode"].week.sessions, 2);
}

#[test]
fn scoped_tally_includes_project_and_linked_worktree_roots_only() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let ts = format!("{today}T15:00:00.000Z");
    let project = dir.path().join("repo");
    let linked = dir.path().join("linked-worktree");
    let other = dir.path().join("other-project");
    let project_session = dir.path().join("sessions/project");
    let linked_session = dir.path().join("sessions/linked");
    let other_session = dir.path().join("sessions/other");
    let unknown_session = dir.path().join("sessions/unknown");
    std::fs::create_dir_all(&project_session).unwrap();
    std::fs::create_dir_all(&linked_session).unwrap();
    std::fs::create_dir_all(&other_session).unwrap();
    std::fs::create_dir_all(&unknown_session).unwrap();

    let project_file = write_jsonl(
        &project_session,
        "chat.jsonl",
        &[&claude_line_ts_in(
            &ts,
            1.0,
            "msg-project",
            "req-project",
            &project.join("src"),
        )],
    );
    let linked_file = write_jsonl(
        &linked_session,
        "chat.jsonl",
        &[&claude_line_ts_in(
            &ts,
            2.0,
            "msg-linked",
            "req-linked",
            &linked,
        )],
    );
    let other_file = write_jsonl(
        &other_session,
        "chat.jsonl",
        &[&claude_line_ts_in(
            &ts,
            4.0,
            "msg-other",
            "req-other",
            &other,
        )],
    );
    let unknown_file = write_jsonl(
        &unknown_session,
        "chat.jsonl",
        &[&claude_line_ts(&ts, 8.0, "msg-unknown", "req-unknown")],
    );
    let files = vec![
        (claude_adapter(), project_file),
        (claude_adapter(), linked_file),
        (claude_adapter(), other_file),
        (claude_adapter(), unknown_file),
    ];
    let mut cache = SpendingDiskCache::default();
    let global = compute_spending(&files, &mut cache, &PriceBook::default(), NOW_SECS);
    let scope = SpendScope::from_roots(Some(&project), std::slice::from_ref(&linked));

    let scoped = compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default());

    assert!((global.total.headline.usd - 15.0).abs() < 1e-9);
    assert!((scoped.headline.usd - 3.0).abs() < 1e-9);
    assert_eq!(scoped.headline.tokens, 30);
    assert_eq!(scoped.headline.sessions, 2);
    assert_eq!(scoped.week, scoped.headline);
}

#[test]
fn scoped_tally_counts_sessions_under_worktree_home_not_just_listed_roots() {
    // The regression: a session ran in a worktree that cleanup has since
    // removed, so it is no longer in `git worktree list`, but its transcript
    // (and recorded origin) survive. The durable worktree-home prefix must
    // still scope it in; a session outside the home must stay out.
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let ts = format!("{today}T15:00:00.000Z");
    let project = dir.path().join("repo");
    let home = dir.path().join("repo-worktrees");
    let removed_worktree = home.join("budget-reset");
    let outside = dir.path().join("other-project");
    let removed_session = dir.path().join("sessions/removed");
    let outside_session = dir.path().join("sessions/outside");
    std::fs::create_dir_all(&removed_session).unwrap();
    std::fs::create_dir_all(&outside_session).unwrap();

    let removed_file = write_jsonl(
        &removed_session,
        "chat.jsonl",
        &[&claude_line_ts_in(
            &ts,
            3.0,
            "msg-removed",
            "req-removed",
            &removed_worktree.join("crates/rimz"),
        )],
    );
    let outside_file = write_jsonl(
        &outside_session,
        "chat.jsonl",
        &[&claude_line_ts_in(
            &ts,
            9.0,
            "msg-outside",
            "req-outside",
            &outside,
        )],
    );
    let files = vec![
        (claude_adapter(), removed_file),
        (claude_adapter(), outside_file),
    ];
    let mut cache = SpendingDiskCache::default();
    let _ = compute_spending(&files, &mut cache, &PriceBook::default(), NOW_SECS);

    // `worktree_roots` is empty — the removed worktree is gone from the live
    // list — yet the home prefix keeps its spend in scope.
    let scope = SpendScope::for_workspace(Some(&project), &[], Some(&home));
    let scoped = compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default());

    assert!(
        (scoped.headline.usd - 3.0).abs() < 1e-9,
        "a removed-worktree session under the home still counts"
    );
    assert_eq!(
        scoped.headline.sessions, 1,
        "the outside session is excluded"
    );

    // Without the home prefix the removed worktree drops out — the bug.
    let live_only = SpendScope::from_roots(Some(&project), &[]);
    let live_scoped = compute_scoped_tally(
        &files,
        &cache,
        &live_only,
        NOW_SECS,
        &HeadlineSpec::default(),
    );
    assert_eq!(
        live_scoped.headline.usd, 0.0,
        "the live worktree list alone misses the removed worktree"
    );
}

#[test]
fn trailing_windows_bucket_by_age() {
    let dir = TempDir::new().unwrap();
    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            &claude_line_ago(2 * HOUR, 1.0, "msg-1", "req-1"),
            &claude_line_ago(3 * DAY, 0.5, "msg-2", "req-2"),
            &claude_line_ago(20 * DAY, 0.25, "msg-3", "req-3"),
            &claude_line_ago(100 * DAY, 0.1, "msg-4", "req-4"),
            &claude_line_ago(400 * DAY, 9.0, "msg-5", "req-5"),
        ],
    );

    let mut cache = SpendingDiskCache::default();
    let totals = compute_total(&[file], &mut cache);

    assert_eq!(totals.headline.tokens, 15);
    assert!((totals.headline.usd - 1.0).abs() < 1e-9);
    assert_eq!(totals.week.tokens, 30);
    assert!((totals.week.usd - 1.5).abs() < 1e-9);
    assert_eq!(totals.month.tokens, 45);
    assert!((totals.month.usd - 1.75).abs() < 1e-9);
    assert_eq!(totals.year.tokens, 60);
    assert!((totals.year.usd - 1.85).abs() < 1e-9);
}

#[test]
fn today_headline_window_starts_at_configured_local_midnight() {
    let file = PathBuf::from("/x/claude.jsonl");
    let day_start = (NOW_SECS / 86_400) * 86_400;
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file(
            &file,
            vec![
                cached_entry(day_start - 1, 1.0, "before-midnight"),
                cached_entry(day_start + 60, 2.0, "after-midnight"),
            ],
        )]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![(claude_adapter(), file)];
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };

    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &spec);

    assert!((spending.total.headline.usd - 2.0).abs() < 1e-9);
    assert_eq!(spending.total.headline.tokens, 15);
    assert_eq!(spending.total.headline.sessions, 1);
    assert!((spending.total.week.usd - 3.0).abs() < 1e-9);
    assert_eq!(spending.total.week.sessions, 2);
}

#[test]
fn session_headline_window_uses_latest_activity_run_and_idles_to_zero() {
    const HOUR: u64 = 3_600;
    let file = PathBuf::from("/x/claude.jsonl");
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };
    let active_cache = SpendingDiskCache {
        files: HashMap::from([cached_file(
            &file,
            vec![
                cached_entry(NOW_SECS - 10 * HOUR, 1.0, "old"),
                cached_entry(NOW_SECS - 9 * HOUR, 1.0, "old"),
                cached_entry(NOW_SECS - 4 * HOUR, 2.0, "current"),
                cached_entry(NOW_SECS - HOUR, 3.0, "current"),
            ],
        )]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![(claude_adapter(), file.clone())];

    let counted = dedup_cached_entries(&files, &active_cache).into_counted();
    let active = aggregate_spending(&files, &active_cache, &counted, NOW_SECS, &spec);

    assert!((active.total.headline.usd - 5.0).abs() < 1e-9);
    assert_eq!(active.total.headline.tokens, 30);
    assert_eq!(active.total.headline.sessions, 1);
    assert!((active.total.week.usd - 7.0).abs() < 1e-9);

    let idle_cache = SpendingDiskCache {
        files: HashMap::from([cached_file(
            &file,
            vec![cached_entry(NOW_SECS - 5 * HOUR, 9.0, "idle")],
        )]),
        ..Default::default()
    };
    let counted = dedup_cached_entries(&files, &idle_cache).into_counted();
    let idle = aggregate_spending(&files, &idle_cache, &counted, NOW_SECS, &spec);

    assert_eq!(idle.total.headline.usd, 0.0);
    assert_eq!(idle.total.headline.tokens, 0);
    assert_eq!(idle.total.headline.sessions, 0);
    assert!((idle.total.week.usd - 9.0).abs() < 1e-9);
}

#[test]
fn scoped_headline_cutoffs_come_from_scoped_entries() {
    const HOUR: u64 = 3_600;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let other = dir.path().join("other");
    let project_file = dir.path().join("claude.jsonl");
    let other_file = dir.path().join("other.jsonl");
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), project_file.clone()),
        (claude_adapter(), other_file.clone()),
    ];
    let scope = SpendScope::from_roots(Some(&project), &[]);

    let day_start = (NOW_SECS / 86_400) * 86_400;
    let today_cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(
                &project_file,
                &project,
                vec![
                    cached_entry(day_start - 1, 1.0, "before-midnight"),
                    cached_entry(day_start + 60, 2.0, "after-midnight"),
                ],
            ),
            cached_file_with_origin(
                &other_file,
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

    let scoped = compute_scoped_tally(&files, &today_cache, &scope, NOW_SECS, &today_spec);

    assert!((scoped.headline.usd - 2.0).abs() < 1e-9);
    assert_eq!(scoped.headline.tokens, 15);
    assert_eq!(scoped.headline.sessions, 1);
    assert!((scoped.week.usd - 3.0).abs() < 1e-9);
    assert_eq!(scoped.week.sessions, 2);

    let session_spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };
    let session_cache = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &project_file,
            &project,
            vec![
                cached_entry(NOW_SECS - 10 * HOUR, 1.0, "old"),
                cached_entry(NOW_SECS - 9 * HOUR, 1.0, "old"),
                cached_entry(NOW_SECS - 4 * HOUR, 2.0, "current"),
                cached_entry(NOW_SECS - HOUR, 3.0, "current"),
            ],
        )]),
        ..Default::default()
    };
    let scoped = compute_scoped_tally(&files, &session_cache, &scope, NOW_SECS, &session_spec);

    assert!((scoped.headline.usd - 5.0).abs() < 1e-9);
    assert_eq!(scoped.headline.tokens, 30);
    assert_eq!(scoped.headline.sessions, 1);
    assert!((scoped.week.usd - 7.0).abs() < 1e-9);
    assert_eq!(scoped.week.sessions, 2);

    let idle_cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(
                &project_file,
                &project,
                vec![
                    cached_entry(NOW_SECS - 8 * HOUR, 2.0, "idle"),
                    cached_entry(NOW_SECS - 7 * HOUR, 3.0, "idle"),
                ],
            ),
            cached_file_with_origin(
                &other_file,
                &other,
                vec![
                    cached_entry(NOW_SECS - 4 * HOUR, 100.0, "outside"),
                    cached_entry(NOW_SECS, 100.0, "outside"),
                ],
            ),
        ]),
        ..Default::default()
    };
    let scoped = compute_scoped_tally(&files, &idle_cache, &scope, NOW_SECS, &session_spec);

    assert_eq!(scoped.headline.usd, 0.0);
    assert_eq!(scoped.headline.tokens, 0);
    assert_eq!(scoped.headline.sessions, 0);
    assert!((scoped.week.usd - 5.0).abs() < 1e-9);
    assert_eq!(scoped.week.sessions, 1);
}

#[test]
fn provider_session_headline_cutoffs_are_provider_local() {
    const HOUR: u64 = 3_600;
    let dir = TempDir::new().unwrap();
    let claude_file = write_jsonl(dir.path(), "claude.jsonl", &[]);
    let codex_file = write_codex(dir.path(), &[]);
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), claude_file.clone()),
        (codex_adapter(), codex_file.clone()),
    ];
    let mut cache = SpendingDiskCache {
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
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };

    let (spending, _) = compute_spending_with_origins_and_scope(
        &files,
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
        &HashMap::new(),
        None,
        &spec,
    );

    assert!((spending.total.headline.usd - 6.0).abs() < 1e-9);
    assert!((spending.by_provider["claude"].headline.usd - 1.0).abs() < 1e-9);
    assert!((spending.by_provider["codex"].headline.usd - 2.0).abs() < 1e-9);
}

#[test]
fn claude_dedup_keeps_one_exact_entry_and_suppresses_sidechain_replays() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let line = claude_line(&today, 1.0, "msg-a", "req-a");
    let file1 = write_jsonl(dir.path(), "session1.jsonl", &[&line, &line]);
    let file2 = write_jsonl(dir.path(), "session2.jsonl", &[&line]);
    let mut cache = SpendingDiskCache::default();
    let exact = compute_total(&[file1, file2], &mut cache);
    assert!((exact.headline.usd - 1.0).abs() < 1e-9);
    assert_eq!(exact.headline.tokens, 15);

    let main = write_jsonl(
        dir.path(),
        "main.jsonl",
        &[&claude_line(&today, 0.05, "msg-parent", "req-parent")],
    );
    let replay = write_jsonl(
        dir.path(),
        "replay.jsonl",
        &[&claude_sidechain_line(
            &today,
            5.00,
            "msg-parent",
            "req-sidechain",
        )],
    );
    let mut cache = SpendingDiskCache::default();
    let deduped = compute_total(&[main, replay], &mut cache);
    assert!((deduped.headline.usd - 0.05).abs() < 1e-9);
    assert_eq!(deduped.headline.tokens, 15);

    let lone_sidechain = write_jsonl(
        dir.path(),
        "sidechain-only.jsonl",
        &[&claude_sidechain_line(&today, 0.20, "msg-x", "req-x")],
    );
    let mut cache = SpendingDiskCache::default();
    let kept = compute_total(&[lone_sidechain], &mut cache);
    assert!((kept.headline.usd - 0.20).abs() < 1e-9);
    assert_eq!(kept.headline.tokens, 50_005);
}

#[test]
fn file_change_cache_paths_parse_suffix_or_reparse_cold() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);

    let suffix_file = write_jsonl(
        dir.path(),
        "suffix.jsonl",
        &[&claude_line(&today, 1.0, "msg-1", "req-1")],
    );
    let mut cache = SpendingDiskCache::default();
    let first = compute_spending(
        &[(claude_adapter(), suffix_file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((first.total.headline.usd - 1.0).abs() < 1e-9);
    let prefix_len = std::fs::metadata(&suffix_file).unwrap().len() as usize;
    {
        use std::io::{Seek as _, SeekFrom};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&suffix_file)
            .unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&vec![b'x'; prefix_len - 1]).unwrap();
    }
    append_line(&suffix_file, &claude_line(&today, 0.25, "msg-2", "req-2"));
    let suffix = compute_spending(
        &[(claude_adapter(), suffix_file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((suffix.total.headline.usd - 1.25).abs() < 1e-9);

    let line_a = claude_line(&today, 1.0, "msg-a", "req-a");
    let line_b = claude_line(&today, 0.5, "msg-b", "req-b");
    let truncated_file = write_jsonl(dir.path(), "truncated.jsonl", &[&line_a, &line_b]);
    let mut cache = SpendingDiskCache::default();
    compute_spending(
        &[(claude_adapter(), truncated_file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    write_jsonl(dir.path(), "truncated.jsonl", &[&line_a]);
    let truncated = compute_spending(
        &[(claude_adapter(), truncated_file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((truncated.total.headline.usd - 1.0).abs() < 1e-9);

    let rewrite_file = write_jsonl(
        dir.path(),
        "rewrite.jsonl",
        &[&claude_line(&today, 1.0, "msg-r", "req-r")],
    );
    let mut cache = SpendingDiskCache::default();
    compute_spending(
        &[(claude_adapter(), rewrite_file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    write_jsonl(
        dir.path(),
        "rewrite.jsonl",
        &[&claude_line(&today, 3.0, "msg-r", "req-r")],
    );
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&rewrite_file)
        .unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
        .unwrap();
    let rewritten = compute_spending(
        &[(claude_adapter(), rewrite_file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((rewritten.total.headline.usd - 3.0).abs() < 1e-9);
}

#[test]
fn cold_parse_skip_ignores_new_files_outside_widest_window() {
    assert!(!cold_parse_out_of_window(
        1_000,
        1_000 + WIDEST_SPEND_WINDOW_SECS + SKIP_PARSE_MARGIN_SECS
    ));
    assert!(cold_parse_out_of_window(
        1_000,
        1_001 + WIDEST_SPEND_WINDOW_SECS + SKIP_PARSE_MARGIN_SECS
    ));

    let dir = TempDir::new().unwrap();
    let file = write_jsonl(dir.path(), "old.jsonl", &[]);
    let (mtime, _) = file_stat(&file);
    std::fs::write(
        &file,
        format!("{}\n", claude_line_ts(&iso_at(mtime), 1.0, "old", "old")),
    )
    .unwrap();
    let (mtime, _) = file_stat(&file);
    let files = vec![(claude_adapter(), file.clone())];
    let mut cache = SpendingDiskCache::default();

    let skipped = compute_spending(
        &files,
        &mut cache,
        &PriceBook::default(),
        mtime + WIDEST_SPEND_WINDOW_SECS + SKIP_PARSE_MARGIN_SECS + 1,
    );

    assert_eq!(skipped.total.year.usd, 0.0);
    assert!(
        !cache
            .files
            .contains_key(&file.to_string_lossy().into_owned())
    );
    assert!(!cache.dirty);

    let fresh_file = write_jsonl(
        dir.path(),
        "fresh.jsonl",
        &[&claude_line_ts(&iso_at(mtime), 1.0, "fresh", "fresh")],
    );
    let mut fresh_cache = SpendingDiskCache::default();
    let parsed = compute_spending(
        &[(claude_adapter(), fresh_file)],
        &mut fresh_cache,
        &PriceBook::default(),
        mtime + 60,
    );

    assert!((parsed.total.year.usd - 1.0).abs() < 1e-9);
}

#[test]
fn spending_walk_observer_checkpoints_on_first_interval() {
    struct CaptureObserver {
        cache_path: PathBuf,
        intervals: usize,
        saw_cursor_file: bool,
    }

    impl WalkObserver for CaptureObserver {
        fn on_interval(&mut self, _cache: &SpendingDiskCache) {
            self.intervals += 1;
            self.saw_cursor_file |= self.cache_path.exists();
        }
    }

    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let files = (0..4)
        .map(|i| {
            let file = write_jsonl(
                dir.path(),
                &format!("cold-{i}.jsonl"),
                &[&claude_line(
                    &today,
                    1.0,
                    &format!("msg-{i}"),
                    &format!("req-{i}"),
                )],
            );
            (claude_adapter(), file)
        })
        .collect::<Vec<_>>();
    let cache_path = dir.path().join("spending.json");
    let mut observer = CaptureObserver {
        cache_path: cache_path.clone(),
        intervals: 0,
        saw_cursor_file: false,
    };
    let mut walker = SpendingWalker::new();

    let result = walker.walk(
        &cache_path,
        &files,
        &PriceBook::default(),
        NOW_SECS,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
        &mut observer,
    );

    assert!(observer.intervals >= 1);
    assert!(observer.saw_cursor_file);
    assert!((result.spending.total.year.usd - 4.0).abs() < 1e-9);
}

#[test]
fn parallel_cold_parse_aggregates_deterministically() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let files = (0..16)
        .map(|i| {
            let cost = (i + 1) as f64 / 10.0;
            let file = write_jsonl(
                dir.path(),
                &format!("parallel-{i}.jsonl"),
                &[&claude_line(
                    &today,
                    cost,
                    &format!("msg-parallel-{i}"),
                    &format!("req-parallel-{i}"),
                )],
            );
            (claude_adapter(), file)
        })
        .collect::<Vec<_>>();
    let expected = (1..=16).map(|i| i as f64 / 10.0).sum::<f64>();

    let mut first_walker = SpendingWalker::new();
    let mut second_walker = SpendingWalker::new();
    let first = first_walker.walk(
        &dir.path().join("first-spending.json"),
        &files,
        &PriceBook::default(),
        NOW_SECS,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
        &mut SilentWalk,
    );
    let second = second_walker.walk(
        &dir.path().join("second-spending.json"),
        &files,
        &PriceBook::default(),
        NOW_SECS,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
        &mut SilentWalk,
    );

    assert!((first.spending.total.year.usd - expected).abs() < 1e-9);
    assert_eq!(first.spending, second.spending);
    assert_eq!(
        serde_json::to_vec(&first.spending).unwrap(),
        serde_json::to_vec(&second.spending).unwrap()
    );
}

#[test]
fn codex_pricing_resume_state_and_provider_breakdown_stay_intact() {
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
    assert_eq!(first.total.headline.input, 1000);
    assert_eq!(first.total.headline.output, 500);

    append_line(&resumable, &codex_total_line(&today, 1600, 800));
    let second = compute_spending(
        &[(codex_adapter(), resumable)],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    assert_eq!(second.total.headline.input, 1600);
    assert_eq!(second.total.headline.output, 800);

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
    let mut cache = SpendingDiskCache::default();
    let spending = compute_spending(
        &[
            (claude_adapter(), claude_file),
            (codex_adapter(), codex_file),
        ],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    let codex = &spending.by_provider["codex"];
    assert!((codex.headline.usd - 0.00164).abs() < 1e-9);
    assert_eq!(codex.headline.input, 600);
    assert_eq!(codex.headline.output, 500);
    assert_eq!(codex.headline.cache_read, 400);
    assert!((spending.by_provider["claude"].headline.usd - 0.5).abs() < 1e-9);
    assert!((spending.total.headline.usd - 0.50164).abs() < 1e-9);
}

#[test]
fn codex_origin_override_scopes_and_survives_unknown_model_heal() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let project = dir.path().join("repo");
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
    assert_eq!(first.total.headline.input, 600);
    assert_eq!(first.total.headline.output, 500);
    assert_eq!(first.total.headline.cache_read, 400);
    assert_eq!(first.total.headline.tokens, 1_100);
    assert_eq!(first.total.headline.sessions, 1);
    assert!(
        compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default()).is_zero(),
        "unknown-origin Codex rollout is omitted from cockpit scope"
    );

    let _ = compute_spending_with_origins(
        &files,
        &mut cache,
        &unpriced,
        NOW_SECS,
        &HashMap::from([(codex_file.clone(), project.clone())]),
    );
    let cache_key = codex_file.to_string_lossy().into_owned();
    assert_eq!(
        cache.files[&cache_key].origin_path.as_deref(),
        Some(project.as_path())
    );

    let healed =
        compute_spending_with_origins(&files, &mut cache, &gpt4o_book(), NOW_SECS, &HashMap::new());
    assert!((healed.total.headline.usd - 0.00164).abs() < 1e-9);
    assert_eq!(healed.total.headline.input, 600);
    assert_eq!(healed.total.headline.output, 500);
    assert_eq!(healed.total.headline.cache_read, 400);
    assert_eq!(healed.total.headline.tokens, 1_100);
    assert_eq!(healed.total.headline.sessions, 1);
    let scoped = compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default());
    assert!((scoped.headline.usd - 0.00164).abs() < 1e-9);
    assert_eq!(scoped.headline.input, 600);
    assert_eq!(scoped.headline.output, 500);
    assert_eq!(scoped.headline.cache_read, 400);
    assert_eq!(scoped.headline.tokens, 1_100);
    assert_eq!(scoped.headline.sessions, 1);
    assert_eq!(
        cache.files[&cache_key].origin_path.as_deref(),
        Some(project.as_path())
    );
}

#[test]
fn dedup_chunk_collapses_duplicate_claude_turns_before_store() {
    let mut sidechain = cached_entry(NOW_SECS, 9.0, "thread");
    sidechain.input = 9_000;
    sidechain.message_id = Some("msg-1".to_owned());
    sidechain.request_id = Some("req-1".to_owned());
    sidechain.is_sidechain = true;

    let mut id_free = cached_entry(NOW_SECS, 2.0, "codex");
    id_free.input = 20;

    let mut main = cached_entry(NOW_SECS, 1.0, "thread");
    main.message_id = Some("msg-1".to_owned());
    main.request_id = Some("req-1".to_owned());

    let mut entries = vec![sidechain, id_free.clone(), main.clone()];
    dedup_chunk(&mut entries);

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].cost_usd, main.cost_usd);
    assert!(!entries[0].is_sidechain);
    assert_eq!(entries[1].cost_usd, id_free.cost_usd);
    assert!(entries[1].message_id.is_none());
}

#[test]
fn claude_file_origin_from_non_null_line_scopes_whole_file() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let project = dir.path().join("repo");
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            &format!(
                r#"{{"timestamp":"{today}T15:00:00.000Z","cwd":null,"costUSD":0.5,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
            ),
            &claude_line_ts_in(
                &format!("{today}T15:01:00.000Z"),
                0.25,
                "msg-2",
                "req-2",
                &project,
            ),
        ],
    );
    let files = vec![(claude_adapter(), file.clone())];
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let mut cache = SpendingDiskCache::default();

    let (_, scoped) = compute_spending_with_origins_and_scope(
        &files,
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
        &HashMap::new(),
        Some(&scope),
        &HeadlineSpec::default(),
    );

    assert_eq!(
        cache.files[&file.to_string_lossy().into_owned()]
            .origin_path
            .as_deref(),
        Some(project.as_path())
    );
    assert!((scoped.headline.usd - 0.75).abs() < 1e-9);
    assert_eq!(scoped.headline.sessions, 1);
}

#[test]
fn cache_compaction_rolls_old_entries_losslessly_and_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let other = dir.path().join("other");
    let project_file = dir.path().join("claude.jsonl");
    let other_file = dir.path().join("other.jsonl");
    let mut old_project = cached_entry(NOW_SECS - 40 * 86_400, 1.0, "old-project");
    old_project.model = Some("claude-opus-4-8".to_owned());
    let mut old_project_same_bucket = cached_entry(NOW_SECS - 40 * 86_400, 2.0, "old-project");
    old_project_same_bucket.model = Some("claude-opus-4-8".to_owned());
    let mut old_other = cached_entry(NOW_SECS - 60 * 86_400, 3.0, "old-other");
    old_other.model = Some("claude-sonnet-4-6".to_owned());
    let mut recent = cached_entry(NOW_SECS - RAW_RETAIN_SECS + 86_400, 4.0, "recent");
    recent.model = Some("claude-opus-4-8".to_owned());
    let expired_rollup_ts = NOW_SECS - 400 * 86_400;
    let mut expired_rollup = cached_entry(expired_rollup_ts, 9.0, "expired");
    expired_rollup.model = Some("claude-opus-4-8".to_owned());
    expired_rollup.rolled = true;
    let mut cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(
                &project_file,
                &project,
                vec![
                    old_project,
                    old_project_same_bucket,
                    recent.clone(),
                    expired_rollup,
                ],
            ),
            cached_file_with_origin(&other_file, &other, vec![old_other]),
        ]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), project_file.clone()),
        (claude_adapter(), other_file.clone()),
    ];
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let before_counted = dedup_cached_entries(&files, &cache).into_counted();
    let before_spending = aggregate_spending(
        &files,
        &cache,
        &before_counted,
        NOW_SECS,
        &HeadlineSpec::default(),
    );
    let mut expected_cache = cache.clone();
    expected_cache
        .files
        .get_mut(&project_file.to_string_lossy().into_owned())
        .unwrap()
        .entries
        .retain(|entry| entry.ts_secs != expired_rollup_ts);
    let before_days = compute_daily_spend(&files, &expected_cache);
    let before_models = compute_model_breakdown(&files, &cache, NOW_SECS);
    let before_scoped =
        compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default());

    assert!(compact_spending_cache(&mut cache, &files, NOW_SECS));

    let after_counted = dedup_cached_entries(&files, &cache).into_counted();
    assert_eq!(
        aggregate_spending(
            &files,
            &cache,
            &after_counted,
            NOW_SECS,
            &HeadlineSpec::default()
        ),
        before_spending
    );
    assert_eq!(compute_daily_spend(&files, &cache), before_days);
    assert_eq!(
        compute_model_breakdown(&files, &cache, NOW_SECS),
        before_models
    );
    assert_eq!(
        compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default()),
        before_scoped
    );
    let entries = &cache.files[&project_file.to_string_lossy().into_owned()].entries;
    assert!(
        entries
            .iter()
            .any(|entry| !entry.rolled && entry.ts_secs == recent.ts_secs)
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.ts_secs != expired_rollup_ts)
    );
    assert_eq!(
        cache
            .files
            .values()
            .flat_map(|file| &file.entries)
            .filter(|entry| entry.rolled)
            .count(),
        2
    );

    let encoded = serde_json::to_value(&cache.files).unwrap();
    assert!(!compact_spending_cache(&mut cache, &files, NOW_SECS));
    assert_eq!(serde_json::to_value(&cache.files).unwrap(), encoded);
}

#[test]
fn spending_walk_persists_cursor_before_aggregate() {
    let dir = TempDir::new().unwrap();
    let cache_path = dir.path().join("spending.json");
    let transcript = write_jsonl(dir.path(), "claude.jsonl", &[]);
    let mut old_a = cached_entry(NOW_SECS - 40 * 86_400, 1.0, "old");
    old_a.model = Some("claude-opus-4-8".to_owned());
    let mut old_b = cached_entry(NOW_SECS - 40 * 86_400, 2.0, "old");
    old_b.model = Some("claude-opus-4-8".to_owned());
    let mut cache = read_spending_cache(&cache_path);
    cache.files = HashMap::from([cached_file(&transcript, vec![old_a, old_b])]);
    write_spending_cache(&cache_path, &cache);
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> =
        vec![(claude_adapter(), transcript.clone())];
    let mut walker = SpendingWalker::new();

    panic_after_next_refresh_for_test();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut observer = SilentWalk;
        walker.walk(
            &cache_path,
            &files,
            &PriceBook::default(),
            NOW_SECS,
            &Default::default(),
            None,
            &HeadlineSpec::default(),
            &mut observer,
        )
    }));

    assert!(
        result.is_err(),
        "test panic fires after refresh, before aggregation"
    );
    let persisted = read_spending_cache(&cache_path);
    let entries = &persisted.files[&transcript.to_string_lossy().into_owned()].entries;
    assert_eq!(entries.len(), 1);
    assert!(entries[0].rolled);
    assert_eq!(entries[0].cost_usd, 3.0);
}

#[test]
fn spending_walk_gates_warm_cursor_persists() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let cache_path = dir.path().join("spending.json");
    let transcript = write_jsonl(
        dir.path(),
        "claude.jsonl",
        &[&claude_line(&today, 1.0, "msg-1", "req-1")],
    );
    set_file_mtime(&transcript, NOW_SECS);
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> =
        vec![(claude_adapter(), transcript.clone())];
    let mut walker = SpendingWalker::new();

    let first = walker.walk(
        &cache_path,
        &files,
        &PriceBook::default(),
        NOW_SECS,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
        &mut SilentWalk,
    );
    assert!(first.stats.cache_written);
    assert_eq!(
        read_spending_cache(&cache_path).files[&transcript.to_string_lossy().into_owned()]
            .entries
            .len(),
        1
    );
    let first_stamp = cache_stamp(&cache_path);

    append_line(&transcript, &claude_line(&today, 2.0, "msg-2", "req-2"));
    set_file_mtime(&transcript, NOW_SECS + 60);
    let second = walker.walk(
        &cache_path,
        &files,
        &PriceBook::default(),
        NOW_SECS + 60,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
        &mut SilentWalk,
    );
    assert!(!second.stats.cache_written);
    assert!((second.spending.total.year.usd - 3.0).abs() < 1e-9);
    assert_eq!(cache_stamp(&cache_path), first_stamp);
    assert_eq!(
        read_spending_cache(&cache_path).files[&transcript.to_string_lossy().into_owned()]
            .entries
            .len(),
        1,
        "warm suffix stays in memory until the persist interval expires"
    );

    let third = walker.walk(
        &cache_path,
        &files,
        &PriceBook::default(),
        NOW_SECS + SPENDING_PERSIST_MIN_INTERVAL + 61,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
        &mut SilentWalk,
    );
    assert!(third.stats.cache_written);
    assert_eq!(
        read_spending_cache(&cache_path).files[&transcript.to_string_lossy().into_owned()]
            .entries
            .len(),
        2,
        "post-interval walk lands previously dirty in-memory state"
    );

    let padding = "x".repeat(SPENDING_PERSIST_PARSE_BYTES as usize);
    append_line(
        &transcript,
        &format!(
            r#"{{"timestamp":"{today}T15:02:00.000Z","costUSD":3.0,"requestId":"req-big","padding":"{padding}","message":{{"id":"msg-big","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
        ),
    );
    set_file_mtime(&transcript, NOW_SECS + SPENDING_PERSIST_MIN_INTERVAL + 62);
    let fourth = walker.walk(
        &cache_path,
        &files,
        &PriceBook::default(),
        NOW_SECS + SPENDING_PERSIST_MIN_INTERVAL + 62,
        &Default::default(),
        None,
        &HeadlineSpec::default(),
        &mut SilentWalk,
    );
    assert!(fourth.stats.parse_bytes >= SPENDING_PERSIST_PARSE_BYTES);
    assert!(fourth.stats.cache_written);
    assert!((fourth.spending.total.year.usd - 6.0).abs() < 1e-9);
    assert_eq!(
        read_spending_cache(&cache_path).files[&transcript.to_string_lossy().into_owned()]
            .entries
            .len(),
        3,
        "large parse work persists regardless of the interval"
    );
}

#[test]
fn cache_compaction_dedups_replays_before_rollup() {
    let session = PathBuf::from("/x/session");
    let main = session.join("chat.jsonl");
    let replay = session.join("subagents/worker.jsonl");
    let ts = NOW_SECS - 40 * 86_400;
    let main_entry = CachedEntry {
        ts_secs: ts,
        cost_usd: 1.0,
        input: 10,
        output: 5,
        cache_write: 0,
        cache_read: 0,
        message_id: Some("msg-1".to_owned()),
        request_id: Some("req-1".to_owned()),
        thread_id: None,
        is_sidechain: false,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: false,
    };
    let replay_entry = CachedEntry {
        cost_usd: 9.0,
        input: 9_000,
        is_sidechain: true,
        ..main_entry.clone()
    };
    let mut cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(&main, vec![main_entry]),
            cached_file(&replay, vec![replay_entry]),
        ]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), main.clone()),
        (claude_adapter(), replay.clone()),
    ];

    assert!(compact_spending_cache(&mut cache, &files, NOW_SECS));
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());

    assert_eq!(spending.total.year.usd, 1.0);
    assert_eq!(spending.total.year.tokens, 15);
    assert_eq!(
        cache
            .files
            .values()
            .flat_map(|file| &file.entries)
            .filter(|entry| entry.rolled)
            .count(),
        1
    );
}

#[test]
fn cache_compaction_defers_message_ids_with_recent_replays() {
    let file = PathBuf::from("/x/claude.jsonl");
    let old_main = CachedEntry {
        ts_secs: NOW_SECS - 40 * 86_400,
        cost_usd: 1.0,
        input: 10,
        output: 5,
        cache_write: 0,
        cache_read: 0,
        message_id: Some("msg-1".to_owned()),
        request_id: Some("req-1".to_owned()),
        thread_id: None,
        is_sidechain: false,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: false,
    };
    let recent_replay = CachedEntry {
        ts_secs: NOW_SECS - RAW_RETAIN_SECS + 86_400,
        cost_usd: 9.0,
        input: 9_000,
        is_sidechain: true,
        ..old_main.clone()
    };
    let mut cache = SpendingDiskCache {
        files: HashMap::from([cached_file(&file, vec![old_main, recent_replay])]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![(claude_adapter(), file.clone())];

    assert!(!compact_spending_cache(&mut cache, &files, NOW_SECS));
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());

    assert_eq!(spending.total.year.usd, 1.0);
    assert!(
        cache.files[&file.to_string_lossy().into_owned()]
            .entries
            .iter()
            .all(|entry| !entry.rolled)
    );
}

#[test]
fn cache_compaction_evicts_dead_file_records() {
    let dir = TempDir::new().unwrap();
    let old_mtime = NOW_SECS - WIDEST_SPEND_WINDOW_SECS - SKIP_PARSE_MARGIN_SECS - 10;
    let recent_mtime = NOW_SECS - 60;
    let discovered_old = write_jsonl(dir.path(), "old.jsonl", &[]);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&discovered_old)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(old_mtime))
        .unwrap();
    let (discovered_mtime, discovered_len) = file_stat(&discovered_old);
    let deleted_old = dir.path().join("deleted.jsonl");
    let recent_absent = dir.path().join("recent.jsonl");
    let discovered_key = discovered_old.to_string_lossy().into_owned();
    let deleted_key = deleted_old.to_string_lossy().into_owned();
    let recent_key = recent_absent.to_string_lossy().into_owned();
    let mut cache = SpendingDiskCache {
        files: HashMap::from([
            (
                discovered_key.clone(),
                FileCacheEntry {
                    mtime_secs: discovered_mtime,
                    len: discovered_len,
                    cursor: SpendCursor {
                        offset: discovered_len,
                        state: None,
                    },
                    origin_path: None,
                    entries: Vec::new(),
                    unknown_models: BTreeMap::new(),
                },
            ),
            (
                deleted_key.clone(),
                FileCacheEntry {
                    mtime_secs: old_mtime,
                    len: 0,
                    cursor: SpendCursor::default(),
                    origin_path: None,
                    entries: vec![cached_entry(old_mtime, 1.0, "deleted-old")],
                    unknown_models: BTreeMap::new(),
                },
            ),
            (
                recent_key.clone(),
                FileCacheEntry {
                    mtime_secs: recent_mtime,
                    len: 0,
                    cursor: SpendCursor::default(),
                    origin_path: None,
                    entries: vec![cached_entry(recent_mtime, 2.0, "recent-absent")],
                    unknown_models: BTreeMap::new(),
                },
            ),
        ]),
        ..Default::default()
    };

    compute_spending(
        &[(claude_adapter(), discovered_old)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );

    assert!(cache.dirty);
    assert!(!cache.files.contains_key(&discovered_key));
    assert!(!cache.files.contains_key(&deleted_key));
    assert!(cache.files.contains_key(&recent_key));
}

#[test]
fn cache_compaction_preserves_old_native_thread_sessions() {
    let file = PathBuf::from("/x/opencode.db");
    let mut cache = SpendingDiskCache {
        files: HashMap::from([cached_file(
            &file,
            vec![
                cached_entry(NOW_SECS - 40 * 86_400, 1.0, "session-a"),
                cached_entry(NOW_SECS - 40 * 86_400 + 60, 2.0, "session-b"),
            ],
        )]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![(opencode_adapter(), file)];

    assert!(compact_spending_cache(&mut cache, &files, NOW_SECS));
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());

    assert_eq!(spending.total.year.sessions, 2);
    assert_eq!(spending.by_provider["opencode"].year.sessions, 2);
}

#[test]
fn unknown_model_chase_records_and_heals_active_files() {
    assert!(!is_priceable_model_name("<synthetic>"));
    assert!(!is_priceable_model_name("   "));
    assert!(is_priceable_model_name("claude-new"));

    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let model = "new-claude-pricing-test-model";
    let line = format!(
        r#"{{"timestamp":"{today}T15:00:00.000Z","requestId":"req-1","message":{{"id":"msg-1","model":"{model}","usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#
    );
    let file = write_jsonl(dir.path(), "chat.jsonl", &[&line]);
    let mut cache = SpendingDiskCache::default();

    let first = compute_spending(
        &[(claude_adapter(), file.clone())],
        &mut cache,
        &PriceBook::from_litellm_json("{}"),
        NOW_SECS,
    );
    assert_eq!(first.total.headline.usd, 0.0);
    assert_eq!(first.total.headline.tokens, 150);
    assert_eq!(first.total.headline.sessions, 1);
    assert_eq!(
        recorded_unknown_models(&[(claude_adapter(), file.clone())], &cache, NOW_SECS),
        BTreeSet::from([model.to_owned()])
    );

    let priced = PriceBook::from_litellm_json(&format!(
        r#"{{"{model}": {{"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}}}}"#
    ));
    let cache_key = file.to_string_lossy().into_owned();
    cache.dirty = false;
    let healed = compute_spending(
        &[(claude_adapter(), file.clone())],
        &mut cache,
        &priced,
        NOW_SECS,
    );
    assert!((healed.total.headline.usd - 0.0002).abs() < 1e-12);
    assert!(cache.files[&cache_key].unknown_models.is_empty());
    assert!(cache.dirty);

    let stale = PathBuf::from("/tmp/stale.jsonl");
    cache.files.insert(
        stale.to_string_lossy().into_owned(),
        FileCacheEntry {
            mtime_secs: 0,
            len: 0,
            cursor: SpendCursor::default(),
            origin_path: None,
            entries: Vec::new(),
            unknown_models: BTreeMap::from([(
                "stale-model".to_owned(),
                NOW_SECS.saturating_sub(WIDEST_SPEND_WINDOW_SECS),
            )]),
        },
    );
    assert_eq!(
        recorded_unknown_models(
            &[(claude_adapter(), file), (claude_adapter(), stale)],
            &cache,
            NOW_SECS
        ),
        BTreeSet::new()
    );
}

fn sample_spending() -> Spending {
    let mut spending = Spending::default();
    spending.total.headline.usd = 1.25;
    spending.total.headline.tokens = 4_200;
    spending
        .by_provider
        .insert("claude".into(), spending.total.clone());
    spending
}

#[test]
fn provider_cache_staleness_and_error_cases_are_explicit() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("provider-spending.json");
    let spending = sample_spending();

    write_provider_spending_cache(&path, 12_345, &spending);
    let cache = read_provider_spending_cache(&path);
    assert_eq!(cache.version, PROVIDER_SPENDING_VERSION);
    assert_eq!(cache.refreshed_at_ms, 12_345);
    assert_eq!(cache.spending, spending);
    assert!(cache.days.is_empty());
    assert!(cache.models.is_empty());
    assert!(cache.is_fresh(12_345));

    let days = BTreeMap::from([(
        NOW_SECS as i64 / 86_400,
        DaySpend {
            usd: 1.25,
            tokens: 4_200,
        },
    )]);
    let models = BTreeMap::from([(
        "claude-opus-4-8".to_owned(),
        model_tally(4_200, 1.25, 3_000, 1_200, 0),
    )]);
    write_provider_spending_cache_with_rollups(&path, 12_346, &spending, &days, &models);
    let cache = read_provider_spending_cache(&path);
    assert_eq!(cache.version, PROVIDER_SPENDING_VERSION);
    assert_eq!(cache.refreshed_at_ms, 12_346);
    assert_eq!(cache.spending, spending);
    assert_eq!(cache.days, days);
    assert_eq!(cache.models, models);

    std::fs::write(&path, serde_json::to_vec(&spending).unwrap()).unwrap();
    let pre_stamp = read_provider_spending_cache(&path);
    assert_eq!(pre_stamp.refreshed_at_ms, 0);
    assert_eq!(pre_stamp.spending, spending);
    assert!(pre_stamp.days.is_empty());
    assert!(pre_stamp.models.is_empty());
    assert!(!pre_stamp.is_fresh(NOW_SECS * 1_000));

    let now_ms = NOW_SECS * 1_000;
    let stale_shape = ProviderSpendingCache {
        version: 0,
        refreshed_at_ms: now_ms,
        spending: spending.clone(),
        ..ProviderSpendingCache::default()
    };
    std::fs::write(&path, serde_json::to_vec(&stale_shape).unwrap()).unwrap();
    let version_mismatch = read_provider_spending_cache(&path);
    assert_eq!(version_mismatch.spending, spending);
    assert!(!version_mismatch.is_fresh(now_ms));

    let v7_shape = ProviderSpendingCache {
        version: 7,
        refreshed_at_ms: now_ms,
        spending: spending.clone(),
        ..ProviderSpendingCache::default()
    };
    std::fs::write(&path, serde_json::to_vec(&v7_shape).unwrap()).unwrap();
    let stale_v7 = read_provider_spending_cache(&path);
    assert_eq!(stale_v7.spending, spending);
    assert!(!stale_v7.is_current_version());

    std::fs::write(&path, b"not json").unwrap();
    let corrupt = read_provider_spending_cache(&path);
    assert_eq!(corrupt.refreshed_at_ms, 0);
    assert_eq!(corrupt.spending, Spending::default());

    let ttl_ms = SPENDING_TTL.as_millis() as u64;
    let fresh = ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        refreshed_at_ms: 1_000,
        ..ProviderSpendingCache::default()
    };
    assert!(fresh.is_fresh(1_000 + ttl_ms));
    assert!(!fresh.is_fresh(1_001 + ttl_ms));
    assert!(fresh.is_fresh(500));
}

#[test]
fn workspace_spending_cache_is_scope_keyed_and_ttl_gated() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("workspace-spending.json");
    let mut tally = SpendTally::default();
    tally.headline.usd = 3.21;
    tally.headline.tokens = 1234;

    let written = WorkspaceSpendingCache {
        refreshed_at_ms: 10_000,
        scope_hash: "scope-a".to_owned(),
        tally: tally.clone(),
        headline_cutoff_secs: 123,
        carry_usd: 0.45,
        live_baselines: BTreeMap::from([("claude-1".to_owned(), 1.05)]),
        ..Default::default()
    };
    write_workspace_spending_cache(&path, &written);
    let cache = read_workspace_spending_cache(&path);

    assert_eq!(cache.version, WORKSPACE_SPENDING_VERSION);
    assert_eq!(cache.refreshed_at_ms, 10_000);
    assert_eq!(cache.scope_hash, "scope-a");
    assert_eq!(cache.tally, tally);
    assert_eq!(cache.headline_cutoff_secs, 123);
    assert_eq!(cache.carry_usd, 0.45);
    assert_eq!(
        cache.live_baselines,
        BTreeMap::from([("claude-1".to_owned(), 1.05)])
    );
    assert!(cache.is_fresh(10_000, "scope-a"));
    assert!(!cache.is_fresh(10_000, "scope-b"));
    assert!(!cache.is_fresh(10_001 + SPENDING_TTL.as_millis() as u64, "scope-a"));

    std::fs::write(&path, b"not json").unwrap();
    assert_eq!(
        read_workspace_spending_cache(&path),
        WorkspaceSpendingCache::default()
    );
}

#[test]
fn cache_version_prefix_gates_reads_and_writes() {
    fn write_cursor(path: &Path) {
        let cursor = SpendingDiskCache {
            version: SPENDING_CACHE_VERSION,
            ..SpendingDiskCache::default()
        };
        assert!(write_spending_cache(path, &cursor));
    }

    fn write_provider(path: &Path) {
        write_provider_spending_cache(path, 12_345, &sample_spending());
    }

    fn write_workspace(path: &Path) {
        write_workspace_spending_cache(
            path,
            &WorkspaceSpendingCache {
                refreshed_at_ms: 12_345,
                scope_hash: "scope".to_owned(),
                ..Default::default()
            },
        );
    }

    let dir = TempDir::new().unwrap();
    let cursor_path = dir.path().join("spending.json");
    let provider_path = dir.path().join("provider-spending.json");
    let workspace_path = dir.path().join("workspace-spending.json");

    for (path, write_cache, expected_version) in [
        (
            cursor_path.as_path(),
            write_cursor as fn(&Path),
            SPENDING_CACHE_VERSION,
        ),
        (
            provider_path.as_path(),
            write_provider as fn(&Path),
            PROVIDER_SPENDING_VERSION,
        ),
        (
            workspace_path.as_path(),
            write_workspace as fn(&Path),
            WORKSPACE_SPENDING_VERSION,
        ),
    ] {
        write_cache(path);
        assert_eq!(peek_cache_version(path), Some(expected_version));

        let newer = br#"{"version":9999,"sentinel":true}"#.to_vec();
        std::fs::write(path, &newer).unwrap();
        write_cache(path);
        assert_eq!(std::fs::read(path).unwrap(), newer);

        let current = format!(r#"{{"version":{expected_version},"sentinel":true}}"#).into_bytes();
        std::fs::write(path, &current).unwrap();
        write_cache(path);
        assert_eq!(peek_cache_version(path), Some(expected_version));
        assert_ne!(std::fs::read(path).unwrap(), current);
    }

    assert_eq!(peek_cache_version(&dir.path().join("missing.json")), None);
    let no_version = dir.path().join("no-version.json");
    std::fs::write(&no_version, br#"{"refreshed_at_ms":123,"version":9999}"#).unwrap();
    assert_eq!(peek_cache_version(&no_version), None);
}

#[test]
fn spending_cursor_cache_wire_shape_uses_short_keys() {
    let full = CachedEntry {
        ts_secs: 12_345,
        cost_usd: 0.125,
        input: 10,
        output: 20,
        cache_write: 3,
        cache_read: 4,
        message_id: Some("msg-1".to_owned()),
        request_id: Some("req-1".to_owned()),
        thread_id: Some("thread-1".to_owned()),
        is_sidechain: true,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: true,
    };
    let codex = CachedEntry {
        ts_secs: 67_890,
        cost_usd: 0.001,
        input: 100,
        output: 50,
        cache_write: 0,
        cache_read: 25,
        message_id: None,
        request_id: None,
        thread_id: None,
        is_sidechain: false,
        model: Some("gpt-5-codex".to_owned()),
        rolled: false,
    };
    let entries = vec![full.clone(), codex.clone()];
    let value = serde_json::to_value(&entries).unwrap();

    insta::assert_json_snapshot!(value, @r###"
        [
          {
            "d": true,
            "h": "thread-1",
            "i": 10,
            "l": "claude-opus-4-8",
            "m": "msg-1",
            "o": 20,
            "q": "req-1",
            "r": 4,
            "s": true,
            "t": 12345,
            "u": 0.125,
            "w": 3
          },
          {
            "i": 100,
            "l": "gpt-5-codex",
            "o": 50,
            "r": 25,
            "t": 67890,
            "u": 0.001
          }
        ]
        "###);
    assert_eq!(
        serde_json::from_value::<Vec<CachedEntry>>(value).unwrap(),
        entries
    );

    let file = FileCacheEntry {
        mtime_secs: 88_888,
        len: 123,
        cursor: SpendCursor {
            offset: 77,
            state: Some(serde_json::json!({"acc": 3})),
        },
        origin_path: Some(PathBuf::from("/tmp/repo")),
        entries: vec![codex],
        unknown_models: BTreeMap::from([("new-model".to_owned(), 66_666)]),
    };
    let file_value = serde_json::to_value(&file).unwrap();
    assert!(file_value.get("mtime_secs").is_none());
    assert_eq!(file_value["m"], 88_888);
    assert_eq!(file_value["n"], 123);
    assert_eq!(file_value["c"]["o"], 77);
    assert_eq!(file_value["c"]["s"], serde_json::json!({"acc": 3}));
    assert_eq!(
        serde_json::from_value::<FileCacheEntry>(file_value).unwrap(),
        file
    );
}

#[test]
fn live_overlay_cases_stay_bounded() {
    for (name, walked, live, baselines, published_at, expected) in [
        (
            "overshoot",
            10.0,
            vec![("a", 1.30, Some(5_000)), ("b", 2.50, Some(5_000))],
            BTreeMap::from([("a".to_owned(), 1.00), ("b".to_owned(), 2.50)]),
            9_000,
            10.30,
        ),
        (
            "new session",
            1.00,
            vec![("fresh", 0.40, Some(9_500))],
            BTreeMap::new(),
            9_000,
            1.40,
        ),
        (
            "unbaselined old sessions",
            5.00,
            vec![("old", 3.00, Some(8_000)), ("unstamped", 2.00, None)],
            BTreeMap::new(),
            9_000,
            5.00,
        ),
        (
            "negative delta",
            6.00,
            vec![("a", 3.20, Some(5_000))],
            BTreeMap::from([("a".to_owned(), 4.00)]),
            9_000,
            6.00,
        ),
    ] {
        let blended = today_spend_live_usd(walked, live.into_iter(), &baselines, published_at);
        assert!((blended - expected).abs() < 1e-9, "{name}");
    }
}

fn tally_with_headline_usd(usd: f64) -> SpendTally {
    let mut tally = SpendTally::default();
    tally.headline.usd = usd;
    tally
}

fn workspace_cache_for_carry(
    scope_hash: &str,
    walked: f64,
    cutoff: u64,
    carry: f64,
    refreshed_at_ms: u64,
    live_baselines: BTreeMap<String, f64>,
) -> WorkspaceSpendingCache {
    WorkspaceSpendingCache {
        version: WORKSPACE_SPENDING_VERSION,
        refreshed_at_ms,
        scope_hash: scope_hash.to_owned(),
        tally: tally_with_headline_usd(walked),
        headline_cutoff_secs: cutoff,
        carry_usd: carry,
        live_baselines,
    }
}

fn displayed_workspace_usd(
    cache: &WorkspaceSpendingCache,
    live_costs: &[(String, f64, Option<u64>)],
) -> f64 {
    today_spend_live_usd(
        cache.tally.headline.usd + cache.carry_usd,
        live_costs
            .iter()
            .map(|(id, usd, registered_at)| (id.as_str(), *usd, *registered_at)),
        &cache.live_baselines,
        cache.refreshed_at_ms,
    )
}

fn publish_workspace_for_carry(
    prev: &WorkspaceSpendingCache,
    scope_hash: &str,
    walked: f64,
    cutoff: u64,
    refreshed_at_ms: u64,
    live_costs: &[(String, f64, Option<u64>)],
) -> WorkspaceSpendingCache {
    let (carry_usd, live_baselines) =
        reconcile_workspace_carry(prev, scope_hash, walked, cutoff, live_costs);
    workspace_cache_for_carry(
        scope_hash,
        walked,
        cutoff,
        carry_usd,
        refreshed_at_ms,
        live_baselines,
    )
}

#[test]
fn workspace_carry_absorbs_publish_dip_and_recaptures_baselines() {
    let prev = workspace_cache_for_carry(
        "scope",
        100.0,
        123,
        0.0,
        10_000,
        BTreeMap::from([("a".to_owned(), 5.0)]),
    );
    let live_costs = vec![("a".to_owned(), 7.0, Some(1_000))];

    let (carry, baselines) = reconcile_workspace_carry(&prev, "scope", 101.0, 123, &live_costs);

    assert!((carry - 1.0).abs() < 1e-9);
    assert_eq!(baselines, BTreeMap::from([("a".to_owned(), 7.0)]));
}

#[test]
fn workspace_carry_shrinks_as_walked_spend_catches_up() {
    let prev = workspace_cache_for_carry(
        "scope",
        101.0,
        123,
        1.0,
        20_000,
        BTreeMap::from([("a".to_owned(), 7.0)]),
    );
    let live_costs = vec![("a".to_owned(), 7.0, Some(1_000))];

    let (carry, _) = reconcile_workspace_carry(&prev, "scope", 101.75, 123, &live_costs);

    assert!((carry - 0.25).abs() < 1e-9);
}

#[test]
fn workspace_carry_resets_on_epoch_scope_or_version_mismatch() {
    let prev = workspace_cache_for_carry(
        "scope",
        100.0,
        123,
        2.0,
        10_000,
        BTreeMap::from([("a".to_owned(), 5.0)]),
    );
    let live_costs = vec![("a".to_owned(), 8.0, Some(1_000))];

    let (epoch_carry, epoch_baselines) =
        reconcile_workspace_carry(&prev, "scope", 100.0, 456, &live_costs);
    assert_eq!(epoch_carry, 0.0);
    assert_eq!(epoch_baselines, BTreeMap::from([("a".to_owned(), 8.0)]));

    let (scope_carry, _) = reconcile_workspace_carry(&prev, "other", 100.0, 123, &live_costs);
    assert_eq!(scope_carry, 0.0);

    let old_version = WorkspaceSpendingCache {
        version: WORKSPACE_SPENDING_VERSION - 1,
        ..prev
    };
    let (version_carry, _) =
        reconcile_workspace_carry(&old_version, "scope", 100.0, 123, &live_costs);
    assert_eq!(version_carry, 0.0);
}

#[test]
fn workspace_carry_keeps_display_monotone_across_leading_statusline_publishes() {
    let scope = "scope";
    let cutoff = 123;
    let mut cache = workspace_cache_for_carry(
        scope,
        100.0,
        cutoff,
        0.0,
        10_000,
        BTreeMap::from([("a".to_owned(), 10.0)]),
    );
    let mut displays = Vec::new();

    let live_a = vec![("a".to_owned(), 12.0, Some(1_000))];
    displays.push(displayed_workspace_usd(&cache, &live_a));
    cache = publish_workspace_for_carry(&cache, scope, 101.0, cutoff, 20_000, &live_a);
    displays.push(displayed_workspace_usd(&cache, &live_a));

    let live_b = vec![("a".to_owned(), 13.0, Some(1_000))];
    displays.push(displayed_workspace_usd(&cache, &live_b));
    cache = publish_workspace_for_carry(&cache, scope, 102.0, cutoff, 30_000, &live_b);
    displays.push(displayed_workspace_usd(&cache, &live_b));

    cache = publish_workspace_for_carry(&cache, scope, 103.0, cutoff, 40_000, &live_b);
    displays.push(displayed_workspace_usd(&cache, &live_b));

    assert_eq!(displays, vec![102.0, 102.0, 103.0, 103.0, 103.0]);
}

#[test]
fn daily_spend_buckets_by_utc_day_and_drops_sidechain_replays() {
    let day_a = NOW_SECS;
    let day_b = NOW_SECS - 2 * 86_400;
    let day_c = NOW_SECS - 40 * 86_400;

    // A Claude main-chain turn and its sidechain replay share
    // `(message_id, request_id)`; only the main-chain turn must count, so the
    // inflated replay can never double a day's tokens.
    let main = CachedEntry {
        ts_secs: day_a,
        cost_usd: 1.0,
        input: 100,
        output: 50,
        cache_write: 10,
        cache_read: 70,
        message_id: Some("msg-1".to_owned()),
        request_id: Some("req-1".to_owned()),
        thread_id: None,
        is_sidechain: false,
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
    // An id-free Codex turn on an earlier day buckets under its own date.
    let codex = CachedEntry {
        ts_secs: day_b,
        cost_usd: 2.0,
        input: 200,
        output: 20,
        cache_write: 0,
        cache_read: 40,
        message_id: None,
        request_id: None,
        thread_id: None,
        is_sidechain: false,
        model: Some("gpt-5-codex".to_owned()),
        rolled: false,
    };
    // A model older than 30 days still lands in the year bucket, not month/week.
    let old = CachedEntry {
        ts_secs: day_c,
        cost_usd: 3.0,
        input: 300,
        output: 30,
        cache_write: 0,
        cache_read: 60,
        message_id: None,
        request_id: None,
        thread_id: None,
        is_sidechain: false,
        model: Some("gpt-5-old".to_owned()),
        rolled: false,
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

    let key_a = (day_a / 86_400) as i64;
    let key_b = (day_b / 86_400) as i64;
    let key_c = (day_c / 86_400) as i64;
    assert_eq!(daily.len(), 3);
    // input + cache_write + output + cache_read, the main-chain turn alone.
    assert_eq!(daily[&key_a].tokens, 230);
    assert!((daily[&key_a].usd - 1.0).abs() < 1e-9);
    assert_eq!(daily[&key_b].tokens, 260);
    assert!((daily[&key_b].usd - 2.0).abs() < 1e-9);
    assert_eq!(daily[&key_c].tokens, 390);
    assert!((daily[&key_c].usd - 3.0).abs() < 1e-9);

    // The per-model breakdown rides the same dedup: the sidechain replay is
    // suppressed, so Opus keeps the main-chain turn's `input + cache_write`
    // (110) and output (50), and Codex buckets under its own model.
    let by_model = compute_model_breakdown(&files, &cache, NOW_SECS);
    assert_eq!(by_model.len(), 3);
    let opus = &by_model["claude-opus-4-8"];
    assert_eq!(
        (
            opus.year.input,
            opus.year.output,
            opus.year.cache_read,
            opus.year.tokens
        ),
        (110, 50, 70, 160)
    );
    assert_eq!(opus.week.tokens, 160);
    assert_eq!(opus.month.tokens, 160);
    assert!((opus.year.usd - 1.0).abs() < 1e-9);
    let codex = &by_model["gpt-5-codex"];
    assert_eq!(
        (
            codex.year.input,
            codex.year.output,
            codex.year.cache_read,
            codex.year.tokens
        ),
        (200, 20, 40, 220)
    );
    assert_eq!(codex.week.tokens, 220);
    assert_eq!(codex.month.tokens, 220);
    let old = &by_model["gpt-5-old"];
    assert_eq!(
        (
            old.year.input,
            old.year.output,
            old.year.cache_read,
            old.year.tokens
        ),
        (300, 30, 60, 330)
    );
    assert_eq!(old.week.tokens, 0);
    assert_eq!(old.month.tokens, 0);
}
