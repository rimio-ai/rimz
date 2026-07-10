//! Cache refresh, parse scheduling, chunk deduplication, and unknown-model healing for spending walks.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

use crate::agents::AgentAdapter;
use crate::agents::pricing::PriceBook;

use super::aggregate::stamp_file_origin;
use super::aggregate::{cold_parse_out_of_window, normalized_absolute_path, within_widest_window};
use super::cache::{
    CachedEntry, FileCacheEntry, SpendCursor, SpendParse, SpendingDiskCache,
    compact_spending_cache, file_stat,
};
use super::{SpendProgress, WalkStats};

const MAX_SPENDING_PARSE_WORKERS: usize = 8;

type FastHashMap<K, V> = HashMap<K, V, foldhash::fast::RandomState>;

pub(crate) fn refresh_spending_cache(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
    now_secs: u64,
    origin_overrides: &HashMap<PathBuf, PathBuf>,
    stats: &mut WalkStats,
    callbacks: &mut RefreshCallbacks<'_>,
) {
    // First pass: refresh stale cache entries — pure hit, suffix parse, or
    // cold parse, decided from one stat per file.
    let total_files = files.len();
    let mut finished_files = 0;
    let mut jobs = Vec::new();
    for (adapter, file) in files {
        let (mtime, len) = file_stat(file);
        let key = file.to_string_lossy().into_owned();
        let entry = cache.files.get(&key);
        let override_origin = origin_overrides
            .get(file)
            .and_then(|origin| normalized_absolute_path(origin));
        let prior_origin = entry.and_then(|entry| entry.origin_path.clone());
        let parse_origin = override_origin.or(prior_origin);
        let heals = entry.is_some_and(|entry| has_healed_unknown(entry, prices, now_secs));
        let resume = match refresh_decision(entry, mtime, len, heals, now_secs) {
            RefreshDecision::Unchanged => {
                let mut changed = false;
                if let Some(entry) = cache.files.get_mut(&key)
                    && let Some(origin) = parse_origin.as_deref()
                    && stamp_file_origin(entry, origin)
                {
                    changed = true;
                }
                if changed {
                    cache.mark_changed();
                }
                finished_files += 1;
                (callbacks.tick)(
                    cache,
                    SpendProgress {
                        finished_files,
                        total_files,
                    },
                );
                continue;
            }
            RefreshDecision::SkipOutOfWindow => {
                finished_files += 1;
                (callbacks.tick)(
                    cache,
                    SpendProgress {
                        finished_files,
                        total_files,
                    },
                );
                continue;
            }
            RefreshDecision::Parse { resume } => resume,
        };
        stats.parse_jobs = stats.parse_jobs.saturating_add(1);
        stats.parse_bytes = stats.parse_bytes.saturating_add(
            resume
                .as_ref()
                .map_or(len, |cursor| len.saturating_sub(cursor.offset)),
        );
        jobs.push(SpendingParseJob {
            adapter: *adapter,
            file,
            key,
            mtime_secs: mtime,
            len,
            resume,
            parse_origin,
        });
    }

    (callbacks.on_jobs_scheduled)(stats);
    refresh_spending_cache_jobs(
        &jobs,
        cache,
        prices,
        &mut finished_files,
        total_files,
        callbacks.tick,
    );

    if compact_spending_cache(cache, files, now_secs) {
        cache.mark_changed();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RefreshDecision {
    Unchanged,
    SkipOutOfWindow,
    Parse { resume: Option<SpendCursor> },
}

pub(crate) fn refresh_decision(
    entry: Option<&FileCacheEntry>,
    mtime: u64,
    len: u64,
    heals: bool,
    now_secs: u64,
) -> RefreshDecision {
    if let Some(entry) = entry
        && !heals
        && entry.mtime_secs == mtime
        && entry.len == len
    {
        return RefreshDecision::Unchanged;
    }
    if entry.is_none() && !heals && cold_parse_out_of_window(mtime, now_secs) {
        return RefreshDecision::SkipOutOfWindow;
    }
    RefreshDecision::Parse {
        resume: entry
            .filter(|entry| !heals && len > entry.len)
            .map(|entry| entry.cursor.clone()),
    }
}

pub(crate) struct RefreshCallbacks<'a> {
    pub(crate) on_jobs_scheduled: &'a mut dyn FnMut(&WalkStats),
    pub(crate) tick: &'a mut dyn FnMut(&SpendingDiskCache, SpendProgress),
}

struct SpendingParseJob<'a> {
    adapter: &'static dyn AgentAdapter,
    file: &'a PathBuf,
    key: String,
    mtime_secs: u64,
    len: u64,
    resume: Option<SpendCursor>,
    parse_origin: Option<PathBuf>,
}

fn refresh_spending_cache_jobs(
    jobs: &[SpendingParseJob<'_>],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
    finished_files: &mut usize,
    total_files: usize,
    tick: &mut dyn FnMut(&SpendingDiskCache, SpendProgress),
) {
    if jobs.is_empty() {
        return;
    }

    let workers = spending_parse_workers(jobs.len());
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::sync_channel(workers);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let next = &next;
            let shared_jobs = jobs;
            let shared_prices = prices;
            scope.spawn(move || {
                loop {
                    let job_index = next.fetch_add(1, Ordering::Relaxed);
                    if job_index >= shared_jobs.len() {
                        break;
                    }
                    let job = &shared_jobs[job_index];
                    let mut parsed =
                        job.adapter
                            .parse_spend(job.file, job.resume.as_ref(), shared_prices);
                    dedup_chunk(&mut parsed.entries);
                    if tx.send((job_index, parsed)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for (job_index, parsed) in rx {
            let job = &jobs[job_index];
            fold_spending_parse_job(cache, job, parsed);
            cache.mark_changed();
            *finished_files += 1;
            tick(
                cache,
                SpendProgress {
                    finished_files: *finished_files,
                    total_files,
                },
            );
        }
    });
}

fn spending_parse_workers(job_count: usize) -> usize {
    std::thread::available_parallelism()
        .map(|workers| workers.get())
        .unwrap_or(4)
        .min(job_count)
        .min(MAX_SPENDING_PARSE_WORKERS)
}

fn fold_spending_parse_job(
    cache: &mut SpendingDiskCache,
    job: &SpendingParseJob<'_>,
    parsed: SpendParse,
) {
    let file_origin = job.parse_origin.clone().or(parsed.origin);
    if job.resume.is_some() && !parsed.replace_entries {
        // Grown jobs are created only from an existing cache entry.
        let entry = cache
            .files
            .get_mut(&job.key)
            .expect("grown spending parse job must have a cache entry");
        entry.entries.extend(parsed.entries);
        entry.unknown_models.extend(parsed.unknown_models);
        entry.cursor = parsed.cursor;
        entry.mtime_secs = job.mtime_secs;
        entry.len = job.len;
        if let Some(origin) = file_origin.as_deref() {
            stamp_file_origin(entry, origin);
        }
        return;
    }

    cache.files.insert(
        job.key.clone(),
        FileCacheEntry {
            mtime_secs: job.mtime_secs,
            len: job.len,
            cursor: parsed.cursor,
            origin_path: file_origin,
            entries: parsed.entries,
            unknown_models: parsed.unknown_models,
        },
    );
}

pub(crate) fn is_priceable_model_name(model: &str) -> bool {
    let model = model.trim();
    !model.is_empty() && !model.starts_with('<')
}

/// Record a priceable model lookup miss at `ts_secs`, keeping the youngest
/// timestamp so the chase stops once every occurrence ages out of the widest
/// spend window.
pub(crate) fn record_unknown_model(
    unknowns: &mut BTreeMap<String, u64>,
    model: &str,
    ts_secs: u64,
) {
    let model = model.trim();
    if !is_priceable_model_name(model) {
        return;
    }
    unknowns
        .entry(model.to_owned())
        .and_modify(|seen| *seen = (*seen).max(ts_secs))
        .or_insert(ts_secs);
}

/// Unknown models recorded by files still discovered in this spending pass.
/// Deleted or moved transcripts do not keep a never-resolving name alive.
pub fn recorded_unknown_models(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    now_secs: u64,
) -> BTreeSet<String> {
    let mut unknowns = BTreeSet::new();
    for (_, file) in files {
        let key = file.to_string_lossy().into_owned();
        if let Some(entry) = cache.files.get(&key) {
            unknowns.extend(
                entry
                    .unknown_models
                    .iter()
                    .filter(|(_, ts_secs)| within_widest_window(**ts_secs, now_secs))
                    .map(|(model, _)| model.clone()),
            );
        }
    }
    unknowns
}

pub(crate) fn has_healed_unknown(
    entry: &FileCacheEntry,
    prices: &PriceBook,
    now_secs: u64,
) -> bool {
    entry.unknown_models.iter().any(|(model, ts_secs)| {
        within_widest_window(*ts_secs, now_secs) && prices.price(model).is_some()
    })
}

pub(crate) fn dedup_chunk(entries: &mut Vec<CachedEntry>) {
    let mut deduped = Vec::with_capacity(entries.len());
    let mut by_exact_key = FastHashMap::<(String, Option<String>), usize>::default();
    for entry in entries.drain(..) {
        let Some(msg_id) = entry.message_id.clone() else {
            deduped.push(entry);
            continue;
        };
        let exact_key = (msg_id, entry.request_id.clone());
        match by_exact_key.entry(exact_key) {
            std::collections::hash_map::Entry::Occupied(slot) => {
                let existing = &mut deduped[*slot.get()];
                if existing.is_sidechain && !entry.is_sidechain {
                    *existing = entry;
                }
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(deduped.len());
                deduped.push(entry);
            }
        }
    }
    *entries = deduped;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts_secs: u64) -> CachedEntry {
        CachedEntry {
            ts_secs,
            cost_usd: 1.0,
            input: 1,
            output: 0,
            cache_write: 0,
            cache_read: 0,
            message_id: Some(format!("m-{ts_secs}")),
            request_id: None,
            thread_id: None,
            is_sidechain: false,
            model: Some("fixture".to_owned()),
            rolled: false,
        }
    }

    #[test]
    fn authoritative_parse_replaces_a_grown_files_cached_entries() {
        let path = PathBuf::from("/tmp/rewindable.jsonl");
        let key = path.to_string_lossy().into_owned();
        let mut cache = SpendingDiskCache::default();
        cache.files.insert(
            key.clone(),
            FileCacheEntry {
                mtime_secs: 1,
                len: 10,
                cursor: SpendCursor {
                    offset: 10,
                    state: None,
                },
                origin_path: None,
                entries: vec![entry(1)],
                unknown_models: BTreeMap::new(),
            },
        );
        let job = SpendingParseJob {
            adapter: crate::agents::registry::ADAPTERS[0],
            file: &path,
            key,
            mtime_secs: 2,
            len: 20,
            resume: Some(SpendCursor {
                offset: 10,
                state: None,
            }),
            parse_origin: None,
        };
        fold_spending_parse_job(
            &mut cache,
            &job,
            SpendParse {
                entries: vec![entry(2)],
                replace_entries: true,
                ..SpendParse::default()
            },
        );
        assert_eq!(cache.files[&job.key].entries, vec![entry(2)]);
    }
}
