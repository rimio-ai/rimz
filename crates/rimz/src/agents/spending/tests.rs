use super::*;

use std::collections::BTreeMap;
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
    let counted = dedup_cached_entries(files, cache, &HashSet::new()).into_counted();
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
    let counted = dedup_cached_entries(files, cache, &HashSet::new()).into_counted();
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
    let counted = dedup_cached_entries(files, cache, &HashSet::new()).into_counted();
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
    compute_scoped_spending(files, cache, &HashSet::new(), scope, now_secs, spec).tally
}

macro_rules! walk_spending {
    ($walker:expr, $cache_path:expr, $files:expr, $prices:expr, $now_secs:expr, $observer:expr) => {{
        let prices = $prices;
        let origin_overrides = HashMap::new();
        let automation_files = HashSet::new();
        let spec = HeadlineSpec::default();
        let req = WalkRequest {
            files: $files,
            prices: &prices,
            now_secs: $now_secs,
            origin_overrides: &origin_overrides,
            automation_files: &automation_files,
            automation_signature: 0,
            scope: None,
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

fn sample_spending() -> Spending {
    let mut spending = Spending::default();
    spending.total.headline.usd = 1.25;
    spending.total.headline.tokens = 4_200;
    spending
        .by_provider
        .insert("claude".into(), spending.total.clone());
    spending
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

mod aggregation;
mod compaction;
mod live_overlay;
mod published_cache;
mod walk_cache;
