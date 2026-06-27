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
        files: HashMap::from([(
            file.to_string_lossy().into_owned(),
            FileCacheEntry {
                mtime_secs: 1,
                len: 1,
                cursor: SpendCursor::default(),
                origin_path: None,
                entries: vec![
                    entry("session-a", NOW_SECS),
                    entry("session-b", NOW_SECS - 2 * 86_400),
                    entry("session-b", NOW_SECS - 3 * 86_400),
                ],
                unknown_models: BTreeMap::new(),
            },
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
        files: HashMap::from([(
            file.to_string_lossy().into_owned(),
            FileCacheEntry {
                mtime_secs: 1,
                len: 1,
                cursor: SpendCursor::default(),
                origin_path: None,
                entries: vec![
                    cached_entry(day_start - 1, 1.0, "before-midnight"),
                    cached_entry(day_start + 60, 2.0, "after-midnight"),
                ],
                unknown_models: BTreeMap::new(),
            },
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
fn scoped_today_headline_window_starts_at_configured_local_midnight() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let other = dir.path().join("other");
    let project_file = dir.path().join("claude.jsonl");
    let other_file = dir.path().join("other.jsonl");
    let day_start = (NOW_SECS / 86_400) * 86_400;
    let cache = SpendingDiskCache {
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
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), project_file),
        (claude_adapter(), other_file),
    ];
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };

    let scoped = compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &spec);

    assert!((scoped.headline.usd - 2.0).abs() < 1e-9);
    assert_eq!(scoped.headline.tokens, 15);
    assert_eq!(scoped.headline.sessions, 1);
    assert!((scoped.week.usd - 3.0).abs() < 1e-9);
    assert_eq!(scoped.week.sessions, 2);
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
        files: HashMap::from([(
            file.to_string_lossy().into_owned(),
            FileCacheEntry {
                mtime_secs: 1,
                len: 1,
                cursor: SpendCursor::default(),
                origin_path: None,
                entries: vec![
                    cached_entry(NOW_SECS - 10 * HOUR, 1.0, "old"),
                    cached_entry(NOW_SECS - 9 * HOUR, 1.0, "old"),
                    cached_entry(NOW_SECS - 4 * HOUR, 2.0, "current"),
                    cached_entry(NOW_SECS - HOUR, 3.0, "current"),
                ],
                unknown_models: BTreeMap::new(),
            },
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
        files: HashMap::from([(
            file.to_string_lossy().into_owned(),
            FileCacheEntry {
                mtime_secs: 1,
                len: 1,
                cursor: SpendCursor::default(),
                origin_path: None,
                entries: vec![cached_entry(NOW_SECS - 5 * HOUR, 9.0, "idle")],
                unknown_models: BTreeMap::new(),
            },
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
fn scoped_session_headline_window_uses_latest_activity_run() {
    const HOUR: u64 = 3_600;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let other = dir.path().join("other");
    let project_file = dir.path().join("claude.jsonl");
    let other_file = dir.path().join("other.jsonl");
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(
                &project_file,
                &project,
                vec![
                    cached_entry(NOW_SECS - 10 * HOUR, 1.0, "old"),
                    cached_entry(NOW_SECS - 9 * HOUR, 1.0, "old"),
                    cached_entry(NOW_SECS - 4 * HOUR, 2.0, "current"),
                    cached_entry(NOW_SECS - HOUR, 3.0, "current"),
                ],
            ),
            cached_file_with_origin(
                &other_file,
                &other,
                vec![cached_entry(NOW_SECS, 100.0, "outside")],
            ),
        ]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![
        (claude_adapter(), project_file),
        (claude_adapter(), other_file),
    ];
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };

    let scoped = compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &spec);

    assert!((scoped.headline.usd - 5.0).abs() < 1e-9);
    assert_eq!(scoped.headline.tokens, 30);
    assert_eq!(scoped.headline.sessions, 1);
    assert!((scoped.week.usd - 7.0).abs() < 1e-9);
    assert_eq!(scoped.week.sessions, 2);
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
fn codex_origin_overrides_scope_rollout_entries() {
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
    let _ =
        compute_spending_with_origins(&files, &mut cache, &gpt4o_book(), NOW_SECS, &HashMap::new());
    assert!(
        compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default()).is_zero(),
        "unknown-origin Codex rollout is omitted from cockpit scope"
    );

    let _ = compute_spending_with_origins(
        &files,
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
        &HashMap::from([(codex_file.clone(), project.clone())]),
    );
    assert_eq!(
        cache.files[&codex_file.to_string_lossy().into_owned()]
            .origin_path
            .as_deref(),
        Some(project.as_path())
    );
    let scoped = compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default());
    assert!((scoped.headline.usd - 0.00164).abs() < 1e-9);
    assert_eq!(scoped.headline.input, 600);
    assert_eq!(scoped.headline.output, 500);
    assert_eq!(scoped.headline.cache_read, 400);
    assert_eq!(scoped.headline.sessions, 1);
}

#[test]
fn codex_file_origin_survives_unknown_model_cold_reparse() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let project = dir.path().join("repo");
    let model = "gpt-rimz-new";
    let codex_file = write_codex(
        dir.path(),
        &[
            &format!(r#"{{"type":"turn_context","payload":{{"model":"{model}"}}}}"#),
            &codex_token_line(&today, 1000, 400, 500),
        ],
    );
    let files = vec![(codex_adapter(), codex_file.clone())];
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let mut cache = SpendingDiskCache::default();

    let first = compute_spending_with_origins(
        &files,
        &mut cache,
        &PriceBook::from_litellm_json("{}"),
        NOW_SECS,
        &HashMap::from([(codex_file.clone(), project.clone())]),
    );
    assert_eq!(first.total.headline.usd, 0.0);
    assert_eq!(first.total.headline.input, 600);
    assert_eq!(first.total.headline.output, 500);
    assert_eq!(first.total.headline.cache_read, 400);
    assert_eq!(first.total.headline.tokens, 1_100);
    assert_eq!(first.total.headline.sessions, 1);
    let cache_key = codex_file.to_string_lossy().into_owned();
    assert_eq!(
        cache.files[&cache_key].origin_path.as_deref(),
        Some(project.as_path()),
        "the learned Codex origin is stored even before priced entries exist"
    );

    let priced = PriceBook::from_litellm_json(&format!(
        r#"{{"{model}": {{"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                          "cache_read_input_token_cost": 1e-7}}}}"#
    ));
    let healed =
        compute_spending_with_origins(&files, &mut cache, &priced, NOW_SECS, &HashMap::new());
    assert!((healed.total.headline.usd - 0.00164).abs() < 1e-9);
    let scoped = compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default());
    assert!((scoped.headline.usd - 0.00164).abs() < 1e-9);
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
    let mut recent = cached_entry(NOW_SECS - 10 * 86_400, 4.0, "recent");
    recent.model = Some("claude-opus-4-8".to_owned());
    let mut cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(
                &project_file,
                &project,
                vec![old_project, old_project_same_bucket, recent.clone()],
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
    let before_days = compute_daily_spend(&files, &cache);
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
        ts_secs: NOW_SECS - 10 * 86_400,
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
fn cache_compaction_drops_expired_rollups() {
    let file = PathBuf::from("/x/claude.jsonl");
    let mut expired = cached_entry(NOW_SECS - 400 * 86_400, 1.0, "expired");
    expired.rolled = true;
    let mut cache = SpendingDiskCache {
        files: HashMap::from([cached_file(&file, vec![expired])]),
        ..Default::default()
    };
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = vec![(claude_adapter(), file.clone())];

    assert!(compact_spending_cache(&mut cache, &files, NOW_SECS));

    assert!(
        cache.files[&file.to_string_lossy().into_owned()]
            .entries
            .is_empty()
    );
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

    write_workspace_spending_cache(&path, 10_000, "scope-a", &tally);
    let cache = read_workspace_spending_cache(&path);

    assert_eq!(cache.version, WORKSPACE_SPENDING_VERSION);
    assert_eq!(cache.refreshed_at_ms, 10_000);
    assert_eq!(cache.scope_hash, "scope-a");
    assert_eq!(cache.tally, tally);
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
fn live_baselines_and_overlay_cases_stay_bounded() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("live-spend-baselines.json");
    let baselines = LiveSpendBaselines {
        observed_walk_ms: 12_345,
        baselines: BTreeMap::from([("claude-1".to_owned(), 1.05)]),
    };
    write_live_spend_baselines(&path, &baselines);
    assert_eq!(read_live_spend_baselines(&path), baselines);

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
            (
                claude_file.to_string_lossy().into_owned(),
                FileCacheEntry {
                    mtime_secs: 1,
                    len: 1,
                    cursor: SpendCursor::default(),
                    origin_path: None,
                    entries: vec![main, replay],
                    unknown_models: BTreeMap::new(),
                },
            ),
            (
                codex_file.to_string_lossy().into_owned(),
                FileCacheEntry {
                    mtime_secs: 1,
                    len: 1,
                    cursor: SpendCursor::default(),
                    origin_path: None,
                    entries: vec![codex, old],
                    unknown_models: BTreeMap::new(),
                },
            ),
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
    // input + cache_write + output, the main-chain turn alone.
    assert_eq!(daily[&key_a].tokens, 160);
    assert!((daily[&key_a].usd - 1.0).abs() < 1e-9);
    assert_eq!(daily[&key_b].tokens, 220);
    assert!((daily[&key_b].usd - 2.0).abs() < 1e-9);
    assert_eq!(daily[&key_c].tokens, 330);
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
