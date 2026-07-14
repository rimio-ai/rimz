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
    let user_inputs = user_inputs_from_counted(&counted);
    let live_excluded = BTreeSet::new();
    let workspace = scope.map(|scope| WorkspaceRollupScope {
        scope,
        live_excluded: &live_excluded,
    });
    let aggregate = aggregate_counted_rollups(
        files,
        cache,
        &counted,
        workspace,
        HeadlineContext {
            user_inputs: &user_inputs,
            now_secs,
            spec,
        },
        false,
    );
    (aggregate.spending, aggregate.workspace_tally)
}

fn aggregate_spending(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    counted: &[impl CountedPayload],
    now_secs: u64,
    spec: &HeadlineSpec,
) -> Spending {
    let user_inputs = user_inputs_from_counted(counted);
    aggregate_spending_with_user_inputs(files, cache, counted, &user_inputs, now_secs, spec)
}

fn aggregate_spending_with_user_inputs(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    counted: &[impl CountedPayload],
    user_inputs: &[user_input::UserInputRecord],
    now_secs: u64,
    spec: &HeadlineSpec,
) -> Spending {
    aggregate_counted_rollups(
        files,
        cache,
        counted,
        None,
        HeadlineContext {
            user_inputs,
            now_secs,
            spec,
        },
        false,
    )
    .spending
}

fn user_inputs_from_counted(counted: &[impl CountedPayload]) -> Vec<user_input::UserInputRecord> {
    counted
        .iter()
        .filter_map(|counted| {
            Some(user_input::UserInputRecord {
                at: jiff::Timestamp::from_second(i64::try_from(counted.entry().ts_secs).ok()?)
                    .ok()?,
                kind: crate::ids::AgentKind::new_unchecked(counted.kind()),
                origin: counted.origin().map(Path::to_path_buf),
            })
        })
        .collect()
}

fn user_inputs_from_cache(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
) -> Vec<user_input::UserInputRecord> {
    let counted = dedup_cached_entries(files, cache).into_counted();
    user_inputs_from_counted(&counted)
}

fn compute_daily_spend(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
) -> BTreeMap<i64, DaySpend> {
    let counted = dedup_cached_entries(files, cache).into_counted();
    let user_inputs = user_inputs_from_counted(&counted);
    aggregate_counted_rollups(
        files,
        cache,
        &counted,
        None,
        HeadlineContext {
            user_inputs: &user_inputs,
            now_secs: NOW_SECS,
            spec: &HeadlineSpec::default(),
        },
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
    let user_inputs = user_inputs_from_counted(&counted);
    aggregate_counted_rollups(
        files,
        cache,
        &counted,
        None,
        HeadlineContext {
            user_inputs: &user_inputs,
            now_secs,
            spec: &HeadlineSpec::default(),
        },
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
    let counted = dedup_cached_entries(files, cache).into_counted();
    let user_inputs = user_inputs_from_counted(&counted);
    compute_scoped_spending(
        files,
        cache,
        &user_inputs,
        scope,
        &BTreeSet::new(),
        now_secs,
        spec,
    )
    .tally
}

macro_rules! walk_spending {
    ($walker:expr, $cache_path:expr, $files:expr, $prices:expr, $now_secs:expr, $observer:expr) => {{
        let prices = $prices;
        let origin_overrides = HashMap::new();
        let user_inputs = Vec::new();
        let live_excluded = BTreeSet::new();
        let spec = HeadlineSpec::default();
        let req = WalkRequest {
            files: $files,
            prices: &prices,
            now_secs: $now_secs,
            origin_overrides: &origin_overrides,
            user_inputs: &user_inputs,
            scope: None,
            live_excluded: &live_excluded,
            spec: &spec,
        };
        $walker.walk($cache_path, &req, $observer)
    }};
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
        dedup_key: None,
        thread_id: Some(thread_id.to_owned()),
        is_sidechain: false,
        has_speed: false,
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
fn session_token_totals_sum_and_scope_fresh_tokens() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("opencode.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)")
        .unwrap();
    for (id, session_id, input, output) in [
        ("msg-1", "sess-1", 100, 20),
        ("msg-2", "sess-2", 900, 90),
        ("msg-3", "sess-1", 50, 10),
    ] {
        let data = format!(
            r#"{{"cost":0.10,"modelID":"gpt","providerID":"openai","time":{{"created":1780394400000}},"tokens":{{"input":{input},"output":{output},"cache":{{"read":80,"write":40}}}}}}"#
        );
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            (id, session_id, data),
        )
        .unwrap();
    }
    drop(conn);

    assert_eq!(
        session_token_totals(opencode_adapter(), "sess-1", &path, &PriceBook::default()),
        Some(SessionTokenTotals {
            input: 150,
            output: 30,
        })
    );
    assert_eq!(
        session_token_totals(opencode_adapter(), " ", &path, &PriceBook::default()),
        None
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

mod aggregation;
mod compaction;
mod published_cache;
mod walk_cache;
