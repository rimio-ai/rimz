//! First-wins account-day spend records, independent of transcript and cursor retention.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};

use super::aggregate::{
    CountedPayload, DedupPayload, SessionKey, WIDEST_SPEND_WINDOW_SECS,
    dedup_cached_entry_locations, file_session_key, indexed_counted_entries, session_key,
    within_widest_window,
};
use super::cache::{CacheStamp, cache_stamp};
use super::discovery::HistoryAccount;
use super::history::{self, Maintenance};
use super::{CachedEntry, ResponseTiming, SharedName, SpendingDiskCache, SpendingFile, ToolCounts};
use crate::disk::atomic::{self, AtomicErr};
use crate::disk::paths::SpendingHistoryPaths;
use crate::ids::LoginKey;

const DAY_SECS: u64 = 86_400;
const CLOSE_LAG_SECS: u64 = 14 * DAY_SECS;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredSession {
    Native(String),
    File(PathBuf),
}

impl StoredSession {
    fn new(session: SessionKey<'_>, home: &Path) -> Self {
        match session {
            SessionKey::Native { thread_id, .. } => Self::Native(thread_id.to_owned()),
            SessionKey::File(path) => {
                Self::File(path.strip_prefix(home).unwrap_or(path).to_path_buf())
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct SpendDay {
    kind: String,
    account: PathBuf,
    day: Date,
    rows: Vec<StoredRow>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum StoredRow {
    Spend(SpendRow),
    SessionOnly { session: StoredSession, ts: u64 },
}

#[derive(Clone, Serialize, Deserialize)]
struct SpendRow {
    session: StoredSession,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<SharedName>,
    ts: u64,
    usd: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    input: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    output: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    cache_write: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    cache_read: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timing: Option<StoredTiming>,
    #[serde(default, skip_serializing_if = "ToolCounts::is_empty")]
    tools: ToolCounts,
    #[serde(default, skip_serializing_if = "is_false")]
    fast: bool,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct StoredTiming {
    ms: u64,
    output: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl SpendRow {
    fn new(row: &impl CountedPayload, home: &Path) -> Self {
        let entry = row.entry();
        Self {
            session: StoredSession::new(row.session_key(), home),
            origin: row.origin().map(Path::to_path_buf),
            model: entry.model.clone(),
            ts: entry.ts_secs,
            usd: entry.cost_usd,
            input: entry.input,
            output: entry.output,
            cache_write: entry.cache_write,
            cache_read: entry.cache_read,
            timing: entry.timing.map(|timing| StoredTiming {
                ms: timing.ms,
                output: timing.output,
            }),
            tools: entry.tool_calls.clone(),
            fast: entry.fast,
        }
    }

    fn merge(&mut self, other: Self) {
        self.ts = self.ts.max(other.ts);
        self.usd += other.usd;
        self.input += other.input;
        self.output += other.output;
        self.cache_write += other.cache_write;
        self.cache_read += other.cache_read;
        self.fast |= other.fast;
        if let Some(timing) = other.timing {
            let total = self.timing.get_or_insert(StoredTiming { ms: 0, output: 0 });
            total.ms += timing.ms;
            total.output += timing.output;
        }
        for (name, count) in other.tools.iter() {
            self.tools.add(name, count);
        }
    }

    fn into_entry(self) -> CachedEntry {
        CachedEntry {
            ts_secs: self.ts,
            cost_usd: self.usd,
            input: self.input,
            output: self.output,
            cache_write: self.cache_write,
            cache_read: self.cache_read,
            timing: self.timing.map(|timing| ResponseTiming {
                ms: timing.ms,
                output: timing.output,
            }),
            tool_calls: self.tools,
            model: self.model,
            fast: self.fast,
            rolled: true,
            ..CachedEntry::new(0, 0.0, &Default::default())
        }
    }
}

pub(super) struct HistoryRow {
    login: LoginKey,
    kind: &'static str,
    session: StoredSession,
    origin: Option<PathBuf>,
    entry: CachedEntry,
}

impl DedupPayload for HistoryRow {
    fn entry(&self) -> &CachedEntry {
        &self.entry
    }
}

impl CountedPayload for HistoryRow {
    fn login(&self) -> &LoginKey {
        &self.login
    }

    fn kind(&self) -> &'static str {
        self.kind
    }

    fn origin(&self) -> Option<&Path> {
        self.origin.as_deref()
    }

    fn session_key(&self) -> SessionKey<'_> {
        match &self.session {
            StoredSession::Native(thread_id) => SessionKey::Native {
                kind: self.kind,
                thread_id,
            },
            StoredSession::File(path) => SessionKey::File(path),
        }
    }
}

#[derive(Default)]
pub(super) struct SpendIndex {
    covered: HashMap<LoginKey, BTreeSet<u64>>,
    pub(super) rows: Vec<HistoryRow>,
    pub(super) sessions: Vec<HistoryRow>,
}

impl SpendIndex {
    pub(super) fn replaces(&self, login: &LoginKey, ts: u64) -> bool {
        self.covered
            .get(login)
            .is_some_and(|days| days.contains(&(ts / DAY_SECS)))
    }
}

#[derive(Clone, Eq, PartialEq)]
struct AccountKey {
    login: LoginKey,
    kind: &'static str,
    home: PathBuf,
    identity: PathBuf,
    sources: Vec<Vec<u8>>,
}

#[derive(Clone, Eq, PartialEq)]
struct ReadKey {
    root: PathBuf,
    accounts: Vec<AccountKey>,
    stamps: Vec<(PathBuf, CacheStamp)>,
}

#[derive(Eq, PartialEq)]
struct RecordingKey {
    today: u64,
    read: ReadKey,
}

struct Segment {
    stamp: CacheStamp,
    days: Vec<SpendDay>,
}

#[derive(Default)]
pub(super) struct ClosedDays {
    segments: BTreeMap<PathBuf, Segment>,
    read_key: Option<ReadKey>,
    recorded: Option<RecordingKey>,
    index: SpendIndex,
    #[cfg(test)]
    pub(super) recording_dedups: usize,
}

impl ClosedDays {
    pub(super) fn invalidate_recording(&mut self) {
        self.recorded = None;
    }

    pub(super) fn invalidate_for_parsed(
        &mut self,
        cache: &SpendingDiskCache,
        files: &[&SpendingFile],
        now_secs: u64,
    ) {
        let Some(recorded) = &self.recorded else {
            return;
        };
        let closed_before = now_secs.saturating_sub(CLOSE_LAG_SECS) / DAY_SECS;
        if files.iter().any(|file| {
            if !recorded
                .read
                .accounts
                .iter()
                .any(|account| account.login == file.login)
            {
                return false;
            }
            let Some(cached) = cache.files.get(file.path.to_string_lossy().as_ref()) else {
                return false;
            };
            cached.entries.iter().any(|entry| {
                entry.ts_secs / DAY_SECS < closed_before
                    && within_widest_window(entry.ts_secs, now_secs)
                    && !self.index.replaces(&file.login, entry.ts_secs)
            })
        }) {
            self.invalidate_recording();
        }
    }

    pub(super) fn load(
        &mut self,
        paths: &SpendingHistoryPaths,
        accounts: &[HistoryAccount],
    ) -> &SpendIndex {
        if let Err(error) = self.refresh(paths, accounts) {
            tracing::warn!(%error, "failed to read closed-day spending history");
            self.index = SpendIndex::default();
            self.read_key = None;
        }
        &self.index
    }

    fn refresh(
        &mut self,
        paths: &SpendingHistoryPaths,
        accounts: &[HistoryAccount],
    ) -> Result<(), AtomicErr> {
        let root = paths
            .history_root
            .parent()
            .and_then(|root| std::fs::canonicalize(root).ok());
        let accounts = accounts
            .iter()
            .map(|account| AccountKey {
                login: account.login.clone(),
                kind: account.adapter.spec().kind,
                home: account.home.clone(),
                identity: root
                    .as_ref()
                    .and_then(|root| account.canonical_home.strip_prefix(root).ok())
                    .unwrap_or(&account.canonical_home)
                    .to_path_buf(),
                sources: account
                    .sources
                    .iter()
                    .map(|source| source.fingerprint())
                    .collect(),
            })
            .collect::<Vec<_>>();
        let mut stamps = Vec::new();
        if !accounts.is_empty() {
            for path in history::segments(&paths.spend_dir())? {
                if history::segment_year(&path).is_none() {
                    continue;
                }
                let Some(stamp) = cache_stamp(&path) else {
                    let source = match std::fs::metadata(&path) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => error,
                        Ok(_) => std::io::Error::other("unreadable spending history file stamp"),
                    };
                    return Err(AtomicErr::Io { path, source });
                };
                stamps.push((path, stamp));
            }
        }
        let key = ReadKey {
            root: paths.history_root.clone(),
            accounts,
            stamps,
        };
        if self.read_key.as_ref() == Some(&key) {
            return Ok(());
        }
        self.segments
            .retain(|path, _| key.stamps.iter().any(|(file, _)| file == path));
        for (path, stamp) in &key.stamps {
            if self
                .segments
                .get(path)
                .is_some_and(|segment| segment.stamp == *stamp)
            {
                continue;
            }
            let bytes = std::fs::read(path).map_err(|source| AtomicErr::Io {
                path: path.clone(),
                source,
            })?;
            let days = bytes
                .split(|byte| *byte == b'\n')
                .filter_map(|line| serde_json::from_slice(line).ok())
                .collect();
            self.segments.insert(
                path.clone(),
                Segment {
                    stamp: *stamp,
                    days,
                },
            );
        }
        let mut index = SpendIndex::default();
        let mut seen = BTreeSet::new();
        for segment in self.segments.values() {
            for day in &segment.days {
                let Some(account) = key
                    .accounts
                    .iter()
                    .find(|account| account.kind == day.kind && account.identity == day.account)
                else {
                    continue;
                };
                if !seen.insert((day.kind.clone(), day.account.clone(), day.day)) {
                    continue;
                }
                let Some(start) = day_start(day.day) else {
                    continue;
                };
                index
                    .covered
                    .entry(account.login.clone())
                    .or_default()
                    .insert(start / DAY_SECS);
                for stored in &day.rows {
                    let (session, origin, entry) = match stored {
                        StoredRow::Spend(row) => {
                            (&row.session, row.origin.clone(), row.clone().into_entry())
                        }
                        StoredRow::SessionOnly { session, ts } => (
                            session,
                            None,
                            CachedEntry::new(*ts, 0.0, &Default::default()),
                        ),
                    };
                    let session = match session {
                        StoredSession::Native(id) => StoredSession::Native(id.clone()),
                        StoredSession::File(path) => StoredSession::File(account.home.join(path)),
                    };
                    let row = HistoryRow {
                        login: account.login.clone(),
                        kind: account.kind,
                        session,
                        origin,
                        entry,
                    };
                    match stored {
                        StoredRow::Spend(_) => index.rows.push(row),
                        StoredRow::SessionOnly { .. } => index.sessions.push(row),
                    }
                }
            }
        }
        self.index = index;
        self.read_key = Some(key);
        Ok(())
    }

    pub(super) fn record(
        &mut self,
        hold: &Maintenance,
        cache: &SpendingDiskCache,
        files: &[SpendingFile],
        unread: &[&SpendingFile],
        accounts: &[HistoryAccount],
        now_secs: u64,
    ) -> Result<(), AtomicErr> {
        self.refresh(&hold.paths, accounts)?;
        let incomplete = unread
            .iter()
            .map(|file| &file.login)
            .collect::<BTreeSet<_>>();
        if !incomplete.is_empty() {
            self.invalidate_recording();
        }
        let key = RecordingKey {
            today: now_secs / DAY_SECS,
            // refresh always seeds the read key, even for an empty store.
            read: self.read_key.clone().expect("history refreshed above"),
        };
        if self.recorded.as_ref() == Some(&key) {
            return Ok(());
        }
        if key.read.accounts.is_empty() {
            self.recorded = Some(key);
            return Ok(());
        }
        let mut all_files = files.to_vec();
        let present = files
            .iter()
            .map(|file| file.path.as_path())
            .collect::<BTreeSet<_>>();
        let present_native_files = files
            .iter()
            .filter_map(|file| {
                let session @ SessionKey::Native { .. } =
                    file_session_key(file.adapter, &file.path)
                else {
                    return None;
                };
                Some((&file.login, file.path.file_name()?, session))
            })
            .collect::<BTreeSet<_>>();
        let mut absent = cache
            .files
            .keys()
            .map(PathBuf::from)
            .filter(|path| !present.contains(path.as_path()))
            .collect::<Vec<_>>();
        absent.sort();
        for path in absent {
            let Some(account) = accounts
                .iter()
                .find(|account| account.sources.iter().any(|source| source.contains(&path)))
            else {
                continue;
            };
            if let session @ SessionKey::Native { .. } = file_session_key(account.adapter, &path)
                && path.file_name().is_some_and(|name| {
                    present_native_files.contains(&(&account.login, name, session))
                })
            {
                continue;
            }
            all_files.push(SpendingFile {
                adapter: account.adapter,
                login: account.login.clone(),
                path,
            });
        }
        let recordable_account = |login: &LoginKey, ts: u64| {
            if ts / DAY_SECS >= now_secs.saturating_sub(CLOSE_LAG_SECS) / DAY_SECS
                || !within_widest_window(ts, now_secs)
                || self.index.replaces(login, ts)
                || incomplete.contains(login)
            {
                return None;
            }
            key.read
                .accounts
                .iter()
                .find(|account| &account.login == login)
        };
        let mut sessions = BTreeMap::<_, BTreeMap<StoredSession, u64>>::new();
        for file in &all_files {
            let Some(cached) = cache.files.get(file.path.to_string_lossy().as_ref()) else {
                continue;
            };
            let file_key = file_session_key(file.adapter, &file.path);
            for entry in &cached.entries {
                let Some(account) = recordable_account(&file.login, entry.ts_secs) else {
                    continue;
                };
                let session =
                    StoredSession::new(session_key(account.kind, file_key, entry), &account.home);
                sessions
                    .entry((
                        account.kind,
                        account.identity.clone(),
                        entry.ts_secs / DAY_SECS,
                    ))
                    .or_default()
                    .entry(session)
                    .and_modify(|ts| *ts = (*ts).max(entry.ts_secs))
                    .or_insert(entry.ts_secs);
            }
        }
        let mut days = BTreeMap::<_, BTreeMap<_, SpendRow>>::new();
        #[cfg(test)]
        {
            self.recording_dedups += 1;
        }
        let locations = dedup_cached_entry_locations(&all_files, cache);
        for (row, location) in indexed_counted_entries(&all_files, cache, &locations)
            .into_iter()
            .zip(locations)
        {
            let entry = row.entry();
            let day = entry.ts_secs / DAY_SECS;
            let Some(account) = recordable_account(row.login(), entry.ts_secs) else {
                continue;
            };
            let stored = SpendRow::new(&row, &account.home);
            let rows = days
                .entry((account.kind, account.identity.clone(), day))
                .or_default();
            let row_key = (
                location.file_index,
                stored.session.clone(),
                stored.model.clone(),
                stored.origin.clone(),
            );
            match rows.entry(row_key) {
                std::collections::btree_map::Entry::Occupied(mut row) => {
                    row.get_mut().merge(stored)
                }
                std::collections::btree_map::Entry::Vacant(row) => {
                    row.insert(stored);
                }
            }
        }
        for ((kind, account, day), sessions) in sessions {
            let rows = days
                .remove(&(kind, account.clone(), day))
                .unwrap_or_default();
            let winners = rows
                .values()
                .map(|row| &row.session)
                .collect::<BTreeSet<_>>();
            let session_only = sessions
                .into_iter()
                .filter(|(session, _)| !winners.contains(session))
                .map(|(session, ts)| StoredRow::SessionOnly { session, ts })
                .collect::<Vec<_>>();
            let rows = rows
                .into_values()
                .map(StoredRow::Spend)
                .chain(session_only)
                .collect();
            let Some(day) = i64::try_from(day * DAY_SECS)
                .ok()
                .and_then(|ts| Timestamp::from_second(ts).ok())
                .map(|ts| ts.to_zoned(TimeZone::UTC).date())
            else {
                continue;
            };
            let record = SpendDay {
                kind: kind.to_owned(),
                account,
                day,
                rows,
            };
            let mut bytes = vec![b'\n'];
            serde_json::to_writer(&mut bytes, &record)?;
            bytes.push(b'\n');
            history::append(&hold.paths, &hold.paths.spend(day.year()), &bytes)?;
        }
        self.refresh(&hold.paths, accounts)?;
        self.recorded = incomplete.is_empty().then(|| RecordingKey {
            read: self.read_key.clone().expect("history refreshed above"),
            ..key
        });
        Ok(())
    }
}

fn day_start(day: Date) -> Option<u64> {
    u64::try_from(
        day.at(0, 0, 0, 0)
            .to_zoned(TimeZone::UTC)
            .ok()?
            .timestamp()
            .as_second(),
    )
    .ok()
}

pub(super) fn expire(hold: &Maintenance, now_secs: u64) -> Result<(), AtomicErr> {
    let mut removed = false;
    for path in history::segments(&hold.paths.spend_dir())? {
        let Some(end) = history::segment_year(&path)
            .and_then(|year| Date::new(year + 1, 1, 1).ok())
            .and_then(day_start)
        else {
            continue;
        };
        if end > now_secs.saturating_sub(WIDEST_SPEND_WINDOW_SECS) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(AtomicErr::Io { path, source }),
        }
    }
    if removed {
        atomic::sync_dir(&hold.paths.spend_dir())?;
    }
    Ok(())
}
