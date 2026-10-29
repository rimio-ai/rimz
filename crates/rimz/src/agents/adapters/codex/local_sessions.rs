//! Codex's active and archived rollout stores mapped into local observations.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use jiff::{Timestamp, civil::Date};
use uuid::Uuid;

use super::rollout::{CodexRolloutHeader, read_rollout_header};
use super::transcript::{find_session_transcript_under, rollout_filename_matches_session};
use crate::agents::capabilities::LaunchCapability;
#[cfg(test)]
use crate::agents::local_session_cache::ValueRefreshKind;
use crate::agents::local_session_cache::{
    IncrementalCatalog, ProviderPathStamp, StableValueCache, StampedPaths, ValueRefresh,
    entry_file_type, full_scan_due, metadata, normalized_workspace_inputs, read_dir,
};
use crate::agents::session::{NamedSession, PaneSessionDemand};
use crate::agents::{LocalSessionObservation, LocalSessionProjection};
use crate::ids::{AgentKind, AgentSessionId};

const MAX_EXAMINED_FILES: usize = 512;
const MAX_SESSION_AGE_DAYS: usize = 14;

pub(super) fn locate(
    session_id: &AgentSessionId,
    login_env: &std::collections::BTreeMap<String, String>,
) -> Option<LocalSessionObservation> {
    let home = super::CodexAdapter.config_home(login_env)?;
    for root in ["sessions", "archived_sessions"] {
        let mut dirs = vec![home.join(root)];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if kind.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if !kind.is_file() || path.extension().is_none_or(|ext| ext != "jsonl") {
                    continue;
                }
                let Some(header) = read_rollout_header(&path) else {
                    continue;
                };
                if header.session_id.as_deref() != Some(session_id.as_str()) {
                    continue;
                }
                let Some(cwd) = header.cwd.clone().filter(|cwd| cwd.is_absolute()) else {
                    continue;
                };
                let stamp = ProviderPathStamp::read(&path);
                if let Some(found) =
                    observation_from_header(path, header, stamp, &HashSet::from([cwd.as_path()]))
                {
                    return Some(found);
                }
            }
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RolloutCandidate {
    path: PathBuf,
    active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveryKey {
    home: PathBuf,
    workspaces: Vec<PathBuf>,
    today: Date,
}

#[derive(Default)]
struct CodexDiscoverySnapshot {
    catalog: IncrementalCatalog<DiscoveryKey, RolloutCandidate>,
    headers: StableValueCache<RolloutCandidate, Option<CodexRolloutHeader>>,
    #[cfg(test)]
    work: DiscoveryWork,
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct DiscoveryWork {
    full_scans: usize,
    header_reads: usize,
}

thread_local! {
    static DISCOVERY: RefCell<CodexDiscoverySnapshot> = RefCell::new(CodexDiscoverySnapshot::default());
    // Account entries expire after one discovery backstop without use.
    static PANE_DISCOVERY: RefCell<HashMap<PathBuf, (Instant, CodexPaneDiscovery)>> =
        RefCell::new(HashMap::new());
}

#[derive(Default)]
struct CodexPaneDiscovery {
    home: Option<PathBuf>,
    catalog: IncrementalCatalog<(Date, Timestamp), RolloutCandidate>,
    // Learned heads survive demand changes until a fresh listing removes them.
    headers: StableValueCache<RolloutCandidate, Option<CodexRolloutHeader>>,
    locations: HashMap<AgentSessionId, NamedRollout>,
}

#[derive(Default)]
struct NamedRollout {
    hint: Option<RolloutCandidate>,
    path: Option<RolloutCandidate>,
    last_lookup: Option<Instant>,
}

type CheckedHeaders = HashMap<RolloutCandidate, (Option<CodexRolloutHeader>, ProviderPathStamp)>;

pub(super) fn discover_panes(
    demand: &[PaneSessionDemand],
    login_env: &std::collections::BTreeMap<String, String>,
) -> Vec<LocalSessionObservation> {
    if demand.is_empty() {
        return Vec::new();
    }
    let Some(home) = super::CodexAdapter.config_home(login_env) else {
        return Vec::new();
    };
    let workspaces = demand
        .iter()
        .map(|demand| demand.workspace.as_path())
        .collect::<Vec<_>>();
    let key = DiscoveryKey {
        home: home.clone(),
        workspaces: normalized_workspace_inputs(&workspaces),
        today: Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date(),
    };
    let now = Instant::now();
    PANE_DISCOVERY.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.retain(|_, (last_used, _)| !full_scan_due(Some(*last_used), now));
        let (last_used, snapshot) = cache
            .entry(home)
            .or_insert_with(|| (now, CodexPaneDiscovery::default()));
        *last_used = now;
        snapshot.refresh(key, demand, now)
    })
}

impl CodexPaneDiscovery {
    fn refresh(
        &mut self,
        key: DiscoveryKey,
        demand: &[PaneSessionDemand],
        now: Instant,
    ) -> Vec<LocalSessionObservation> {
        if self.home.as_ref() != Some(&key.home) {
            *self = Self::default();
            self.home = Some(key.home.clone());
        }
        let mut checked = HashMap::new();
        let mut observations = Vec::new();
        let mut named_ids = HashSet::new();
        for demand in demand {
            for session in demand
                .panes
                .iter()
                .flat_map(|pane| pane.own.iter().chain(&pane.named))
            {
                named_ids.insert(session.id.clone());
                if let Some(found) =
                    self.named_session(&key.home, session, &demand.workspace, now, &mut checked)
                {
                    observations.push(found);
                }
            }
        }
        let windows = demand
            .iter()
            .filter_map(|demand| {
                demand
                    .fresh_window(|id| {
                        observations
                            .iter()
                            .any(|row| row.workspace == demand.workspace && &row.session_id == id)
                    })
                    .map(|since| (demand.workspace.clone(), since))
            })
            .collect::<Vec<_>>();
        let mut unread = Vec::new();
        if let Some(since) = windows.iter().map(|(_, since)| *since).min() {
            self.catalog.refresh((key.today, since), now, |topology| {
                enumerate_window_rollouts(&key.home, since, key.today, topology, &mut self.headers)
            });
            // Header creation is immutable. History needs no per-pass metadata;
            // unfinished headers, all dated inside the window's folders, remain
            // eligible for the next pass's validation.
            unread = self
                .catalog
                .entries()
                .iter()
                .filter(|candidate| self.headers.get(candidate).is_none_or(Option::is_none))
                .cloned()
                .collect();
        }
        for candidate in &unread {
            self.check_header(candidate, &mut checked);
        }
        for (workspace, since) in windows {
            let workspaces = HashSet::from([workspace.as_path()]);
            let workspace_named = demand
                .iter()
                .filter(|demand| demand.workspace == workspace)
                .flat_map(|demand| &demand.panes)
                .flat_map(|pane| pane.own.iter().chain(&pane.named))
                .map(|session| session.id.clone())
                .collect::<HashSet<_>>();
            let candidates = self.window_candidates(&workspace, since, &workspace_named);
            let mut window = Vec::new();
            for (id, paths) in candidates {
                let named = workspace_named.contains(&id);
                if !named && window.len() >= MAX_EXAMINED_FILES {
                    break;
                }
                for candidate in paths {
                    let (header, stamp) = self.check_header(&candidate, &mut checked);
                    let Some(found) = header
                        .and_then(|header| {
                            observation_from_header(
                                candidate.path.clone(),
                                header,
                                stamp,
                                &workspaces,
                            )
                        })
                        .filter(|row| row.session_id == id && (named || row.created_at >= since))
                    else {
                        continue;
                    };
                    if named {
                        self.locations.entry(id.clone()).or_default().path = Some(candidate);
                        observations.push(found);
                    } else {
                        window.push(found);
                    }
                    break;
                }
            }
            observations.extend(window);
        }
        let found = observations
            .iter()
            .map(|row| (row.workspace.clone(), row.session_id.clone()))
            .collect::<HashSet<_>>();
        observations.retain(|row| {
            demand.iter().any(|demand| {
                demand.workspace == row.workspace
                    && (demand
                        .panes
                        .iter()
                        .flat_map(|pane| pane.own.iter().chain(&pane.named))
                        .any(|session| session.id == row.session_id)
                        || demand
                            .fresh_window(|id| {
                                found.contains(&(demand.workspace.clone(), id.clone()))
                            })
                            .is_some_and(|since| row.created_at >= since))
            })
        });
        observations.sort_by(|left, right| {
            let left_active = left.transcript_path.starts_with(key.home.join("sessions"));
            let right_active = right.transcript_path.starts_with(key.home.join("sessions"));
            right_active
                .cmp(&left_active)
                .then_with(|| right.last_activity.cmp(&left.last_activity))
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        let mut seen = HashSet::new();
        observations.retain(|row| seen.insert((row.workspace.clone(), row.session_id.clone())));
        self.locations.retain(|id, _| named_ids.contains(id));
        observations
    }

    fn window_candidates(
        &self,
        workspace: &Path,
        since: Timestamp,
        named: &HashSet<AgentSessionId>,
    ) -> Vec<(AgentSessionId, Vec<RolloutCandidate>)> {
        let mut sessions: HashMap<AgentSessionId, Vec<RolloutCandidate>> = HashMap::new();
        for candidate in self.catalog.entries() {
            let Some(header) = self.headers.get(candidate).and_then(Option::as_ref) else {
                continue;
            };
            if header.is_non_root || header.cwd.as_deref() != Some(workspace) {
                continue;
            }
            let Some(id) = rollout_session_id(&candidate.path, header) else {
                continue;
            };
            if header.timestamp.is_some_and(|at| at >= since) || named.contains(&id) {
                sessions.entry(id).or_default().push(candidate.clone());
            }
        }
        let mut sessions = sessions.into_iter().collect::<Vec<_>>();
        sessions.sort_by_cached_key(|(id, paths)| {
            (
                !named.contains(id),
                std::cmp::Reverse(
                    paths
                        .iter()
                        .filter_map(|path| {
                            self.headers
                                .get(path)
                                .and_then(Option::as_ref)
                                .and_then(|header| header.timestamp)
                        })
                        .max(),
                ),
                id.clone(),
            )
        });
        for (_, paths) in &mut sessions {
            paths.sort_by(|left, right| revision_order(right).cmp(&revision_order(left)));
        }
        sessions
    }

    fn named_session(
        &mut self,
        home: &Path,
        session: &NamedSession,
        workspace: &Path,
        now: Instant,
        checked: &mut CheckedHeaders,
    ) -> Option<LocalSessionObservation> {
        Uuid::parse_str(session.id.as_str()).ok()?;
        let mut location = self.locations.remove(&session.id).unwrap_or_default();
        let hint = session
            .transcript_path
            .as_deref()
            .and_then(|path| named_rollout_path(home, path, &session.id));
        let mut paths = VecDeque::new();
        if hint != location.hint {
            location.hint = hint.clone();
            if let Some(hint) = hint {
                if !hint.active
                    && let Some(active) = rollout_twin(home, &hint)
                {
                    paths.push_back(active);
                }
                paths.push_back(hint);
            }
        }
        if let Some(path) = &location.path {
            paths.push_back(path.clone());
        }
        let mut found = None;
        let mut tried = HashSet::new();
        while let Some(path) = paths.pop_front() {
            if !tried.insert(path.clone()) {
                continue;
            }
            let (header, stamp) = self.check_header(&path, checked);
            if header.as_ref().is_some_and(|header| {
                rollout_session_id(&path.path, header).as_ref() == Some(&session.id)
            }) {
                found = header.map(|header| (path.clone(), header, stamp));
                break;
            }
            if !stamp.is_file()
                && let Some(twin) = rollout_twin(home, &path)
            {
                paths.push_back(twin);
            }
        }
        // A path no hook named is a cache from the pass that first remembers it:
        // a revert starts a newer file for the same id, which the backstop's
        // bounded lookup picks up.
        let remembered = found
            .as_ref()
            .is_some_and(|(path, _, _)| location.hint.as_ref() != Some(path));
        if remembered {
            location.last_lookup.get_or_insert(now);
        }
        if (found.is_none() || remembered) && full_scan_due(location.last_lookup, now) {
            location.last_lookup = Some(now);
            for root in ["sessions", "archived_sessions"] {
                let Some(path) =
                    find_session_transcript_under(&home.join(root), session.id.as_str())
                        .and_then(|path| named_rollout_path(home, &path, &session.id))
                else {
                    continue;
                };
                let (header, stamp) = self.check_header(&path, checked);
                if header.as_ref().is_some_and(|header| {
                    rollout_session_id(&path.path, header).as_ref() == Some(&session.id)
                }) {
                    if found.as_ref().is_none_or(|(current, _, _)| {
                        revision_order(&path) > revision_order(current)
                    }) {
                        found = header.map(|header| (path, header, stamp));
                    }
                    break;
                }
            }
        }
        if let Some((path, _, _)) = &found {
            location.path = Some(path.clone());
        } else if location.path.is_none() {
            location.path = location.hint.clone();
        }
        self.locations.insert(session.id.clone(), location);
        let (candidate, header, stamp) = found?;
        observation_from_header(candidate.path, header, stamp, &HashSet::from([workspace]))
    }

    fn check_header(
        &mut self,
        candidate: &RolloutCandidate,
        checked: &mut CheckedHeaders,
    ) -> (Option<CodexRolloutHeader>, ProviderPathStamp) {
        checked
            .entry(candidate.clone())
            .or_insert_with(|| {
                let (header, stamp) = cached_header(&mut self.headers, candidate, false);
                (header.into_current().flatten(), stamp)
            })
            .clone()
    }
}

fn named_rollout_path(home: &Path, path: &Path, id: &AgentSessionId) -> Option<RolloutCandidate> {
    let path = crate::utils::path::normalize_path_lexical(path);
    let home = crate::utils::path::normalize_path_lexical(home);
    let relative = path.strip_prefix(&home).ok()?;
    let parts = relative.components().collect::<Vec<_>>();
    let active = match parts.as_slice() {
        [root, _, _, _, _] if root.as_os_str() == "sessions" => true,
        [root, _] if root.as_os_str() == "archived_sessions" => false,
        _ => return None,
    };
    rollout_filename_matches_session(path.file_name()?.to_str()?, id.as_str())
        .then_some(RolloutCandidate { path, active })
}

/// A thread's current revision has the greatest rollout file name, as in the
/// full scan; an active file wins only over its archived twin of that name.
fn revision_order(candidate: &RolloutCandidate) -> (Option<&str>, bool) {
    (rollout_filename(&candidate.path), candidate.active)
}

fn rollout_twin(home: &Path, candidate: &RolloutCandidate) -> Option<RolloutCandidate> {
    let filename = candidate.path.file_name()?;
    let path = if candidate.active {
        home.join("archived_sessions").join(filename)
    } else {
        let date = rollout_filename(&candidate.path).and_then(rollout_filename_date)?;
        date_directory(home, date).join(filename)
    };
    Some(RolloutCandidate {
        path,
        active: !candidate.active,
    })
}

fn date_directory(home: &Path, date: Date) -> PathBuf {
    home.join("sessions")
        .join(format!("{:04}", date.year()))
        .join(format!("{:02}", date.month()))
        .join(format!("{:02}", date.day()))
}

fn enumerate_window_rollouts(
    home: &Path,
    since: Timestamp,
    today: Date,
    topology: &mut StampedPaths,
    headers: &mut StableValueCache<RolloutCandidate, Option<CodexRolloutHeader>>,
) -> Vec<RolloutCandidate> {
    let first = since.to_zoned(jiff::tz::TimeZone::UTC).date();
    let first = first.yesterday().unwrap_or(first);
    let last = today.tomorrow().unwrap_or(today);
    let mut date = first;
    let mut directories = Vec::new();
    while date <= last {
        directories.push((date_directory(home, date), true));
        let Ok(next) = date.tomorrow() else { break };
        date = next;
    }
    directories.push((home.join("archived_sessions"), false));
    let mut catalog = Vec::new();
    for (dir, active) in &directories {
        topology.record_exact(dir.clone());
        catalog.extend(rollout_files(dir).map(|path| RolloutCandidate {
            path,
            active: *active,
        }));
    }
    let watched = directories
        .iter()
        .map(|(dir, _)| dir.as_path())
        .collect::<HashSet<_>>();
    let paths = catalog
        .iter()
        .map(|candidate| &candidate.path)
        .collect::<HashSet<_>>();
    headers.retain(|candidate| {
        candidate
            .path
            .parent()
            .is_none_or(|dir| !watched.contains(dir))
            || paths.contains(&candidate.path)
    });
    catalog.retain(|candidate| {
        candidate.active
            || rollout_filename(&candidate.path)
                .and_then(rollout_filename_date)
                .is_some_and(|date| date >= first && date <= last)
    });
    catalog
}

pub(super) fn discover(
    workspaces: &[&Path],
    login_env: &std::collections::BTreeMap<String, String>,
) -> Vec<LocalSessionObservation> {
    let Some(home) = super::CodexAdapter.config_home(login_env) else {
        return Vec::new();
    };
    let today = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    discover_cached(&home, workspaces, today, Instant::now())
}

fn discover_cached(
    home: &Path,
    workspaces: &[&Path],
    today: Date,
    now: Instant,
) -> Vec<LocalSessionObservation> {
    let key = DiscoveryKey {
        home: home.to_path_buf(),
        workspaces: normalized_workspace_inputs(workspaces),
        today,
    };
    if key.workspaces.is_empty() {
        return Vec::new();
    }
    DISCOVERY.with(|snapshot| snapshot.borrow_mut().refresh(key, now))
}

impl CodexDiscoverySnapshot {
    fn refresh(&mut self, key: DiscoveryKey, now: Instant) -> Vec<LocalSessionObservation> {
        let scan = self.catalog.refresh(key.clone(), now, |topology| {
            enumerate_rollouts(&key, topology)
        });
        let forced = scan.attempted();
        #[cfg(test)]
        if forced {
            self.work.full_scans += 1;
        }

        let mut observations = self.fold_candidates(&key, forced);
        let catalog_paths = self
            .catalog
            .entries()
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        self.headers
            .retain(|candidate| catalog_paths.contains(candidate));
        observations.sort_by(|left, right| {
            right
                .last_activity
                .cmp(&left.last_activity)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        observations
    }

    /// Walk the ordered candidates, preferring an active rollout over its
    /// archived twin and stopping at the examination bound, and fold each
    /// distinct session's header into one observation.
    fn fold_candidates(
        &mut self,
        key: &DiscoveryKey,
        forced: bool,
    ) -> Vec<LocalSessionObservation> {
        let active_session_ids = self
            .catalog
            .entries()
            .iter()
            .filter(|candidate| candidate.active)
            .filter_map(|candidate| session_id_from_filename(&candidate.path))
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        let mut candidates = self.catalog.entries().to_vec();
        candidates.sort_by(|left, right| {
            rollout_filename(&right.path)
                .cmp(&rollout_filename(&left.path))
                .then_with(|| right.active.cmp(&left.active))
        });
        let workspace_lookup = key
            .workspaces
            .iter()
            .map(PathBuf::as_path)
            .collect::<HashSet<_>>();
        let mut examined = 0;
        let mut session_ids_seen = HashSet::new();
        let mut observations = Vec::new();
        for candidate in candidates {
            let filename_session = session_id_from_filename(&candidate.path).map(str::to_owned);
            if !candidate.active
                && filename_session
                    .as_ref()
                    .is_some_and(|session_id| active_session_ids.contains(session_id))
            {
                continue;
            }
            if examined >= MAX_EXAMINED_FILES {
                break;
            }
            examined += 1;
            if filename_session
                .as_ref()
                .is_some_and(|session_id| session_ids_seen.contains(session_id))
            {
                continue;
            }
            let (header, stamp) = self.refresh_header(candidate.clone(), forced);
            if let Some(observation) = header.and_then(|header| {
                observation_from_header(candidate.path, header, stamp, &workspace_lookup)
            }) {
                if !session_ids_seen.insert(observation.session_id.to_string()) {
                    continue;
                }
                if let Some(session_id) = filename_session {
                    session_ids_seen.insert(session_id);
                }
                observations.push(observation);
            }
        }
        observations
    }

    fn refresh_header(
        &mut self,
        candidate: RolloutCandidate,
        forced: bool,
    ) -> (Option<CodexRolloutHeader>, ProviderPathStamp) {
        let (result, stamp) = cached_header(&mut self.headers, &candidate, forced);
        #[cfg(test)]
        if result.kind() != ValueRefreshKind::Cached {
            self.work.header_reads += 1;
        }
        (result.into_current().flatten(), stamp)
    }
}

fn cached_header(
    headers: &mut StableValueCache<RolloutCandidate, Option<CodexRolloutHeader>>,
    candidate: &RolloutCandidate,
    forced: bool,
) -> (ValueRefresh<Option<CodexRolloutHeader>>, ProviderPathStamp) {
    let dependencies = StampedPaths::exact([candidate.path.clone()]);
    let stamp = dependencies
        .iter()
        .next()
        .map(|(_, stamp)| stamp.clone())
        .unwrap_or_else(|| ProviderPathStamp::read(&candidate.path));
    let result = headers.refresh(candidate.clone(), dependencies, forced, |_| {
        stamp
            .is_file()
            .then(|| read_rollout_header(&candidate.path))
            .flatten()
    });
    (result, stamp)
}

/// Every rollout worth considering: each recent day directory under `sessions`,
/// plus the archive filtered to the same cutoff. Stamps the directories it read
/// so an unchanged topology skips the next walk.
fn enumerate_rollouts(key: &DiscoveryKey, topology: &mut StampedPaths) -> Vec<RolloutCandidate> {
    let sessions = key.home.join("sessions");
    let archive = key.home.join("archived_sessions");
    let dates = recent_dates(key.today);
    let cutoff = *dates.last().unwrap_or(&key.today);
    let day_paths = dates
        .into_iter()
        .map(|date| {
            sessions
                .join(format!("{:04}", date.year()))
                .join(format!("{:02}", date.month()))
                .join(format!("{:02}", date.day()))
        })
        .collect::<Vec<_>>();
    topology.record_exact_many([sessions.clone(), archive.clone()]);
    topology.record_exact_many(day_paths.iter().cloned());

    let mut catalog = Vec::new();
    for day in day_paths {
        catalog.extend(rollout_files(&day).map(|path| RolloutCandidate { path, active: true }));
    }
    catalog.extend(
        rollout_files(&archive)
            .filter(|path| {
                rollout_filename(path)
                    .and_then(rollout_filename_date)
                    .is_some_and(|date| date >= cutoff)
            })
            .map(|path| RolloutCandidate {
                path,
                active: false,
            }),
    );
    catalog
}

/// Regular files in `dir` whose names parse as a rollout.
fn rollout_files(dir: &Path) -> impl Iterator<Item = PathBuf> {
    read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry_file_type(entry).is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| rollout_filename(path).is_some())
}

fn recent_dates(today: Date) -> Vec<Date> {
    let mut dates = Vec::new();
    if let Ok(tomorrow) = today.tomorrow() {
        dates.push(tomorrow);
    }
    dates.push(today);
    let mut date = today;
    for _ in 0..MAX_SESSION_AGE_DAYS {
        let Ok(previous) = date.yesterday() else {
            break;
        };
        dates.push(previous);
        date = previous;
    }
    dates
}

#[cfg(test)]
pub(super) fn discover_under(home: &Path, workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
    let key = DiscoveryKey {
        home: home.to_path_buf(),
        workspaces: normalized_workspace_inputs(workspaces),
        today: Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date(),
    };
    if key.workspaces.is_empty() {
        return Vec::new();
    }
    CodexDiscoverySnapshot::default().refresh(key, Instant::now())
}

fn rollout_filename(path: &Path) -> Option<&str> {
    if path.extension()? != "jsonl" {
        return None;
    }
    path.file_stem()?.to_str()?.strip_prefix("rollout-")
}

fn rollout_filename_date(filename: &str) -> Option<Date> {
    filename.get(..10)?.parse().ok()
}

fn observation_from_header(
    path: PathBuf,
    header: CodexRolloutHeader,
    stamp: ProviderPathStamp,
    workspaces: &HashSet<&Path>,
) -> Option<LocalSessionObservation> {
    if header.is_non_root {
        return None;
    }
    let workspace = *workspaces.get(header.cwd.as_deref()?)?;
    let session_id = rollout_session_id(&path, &header)?;
    let created_at = header.timestamp?;
    let last_activity = timestamp_from_stamp(&stamp)
        .or_else(|| file_mtime(&path))
        .unwrap_or(created_at)
        .max(created_at);
    Some(LocalSessionObservation {
        login: None,
        kind: AgentKind::new_unchecked("codex"),
        session_id,
        workspace: workspace.to_path_buf(),
        transcript_path: path,
        created_at,
        fresh_binding_at: Some(created_at),
        first_event_at: Some(created_at),
        last_activity,
        projection: LocalSessionProjection::IdentityOnly,
    })
}

fn rollout_session_id(path: &Path, header: &CodexRolloutHeader) -> Option<AgentSessionId> {
    header
        .session_id
        .clone()
        .or_else(|| session_id_from_filename(path).map(str::to_owned))
        .map(AgentSessionId::from)
}

fn timestamp_from_stamp(stamp: &ProviderPathStamp) -> Option<Timestamp> {
    let since_epoch = stamp.modified?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Timestamp::new(
        i64::try_from(since_epoch.as_secs()).ok()?,
        since_epoch.subsec_nanos() as i32,
    )
    .ok()
}

pub(super) fn session_id_from_filename(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    let suffix = stem.get(stem.len().checked_sub(36)?..)?;
    Uuid::parse_str(suffix).ok().map(|_| suffix)
}

fn file_mtime(path: &Path) -> Option<Timestamp> {
    let since_epoch = metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Timestamp::new(
        i64::try_from(since_epoch.as_secs()).ok()?,
        since_epoch.subsec_nanos() as i32,
    )
    .ok()
}

#[cfg(test)]
pub(super) fn fixture_observation() -> LocalSessionObservation {
    let created_at = "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
    LocalSessionObservation {
        login: None,
        kind: AgentKind::new_unchecked("codex"),
        session_id: AgentSessionId::from("11111111-1111-4111-8111-111111111111"),
        workspace: PathBuf::from("/workspace/project"),
        transcript_path: PathBuf::from(
            "/provider/sessions/2025/01/01/rollout-2025-01-01T00-00-00-11111111-1111-4111-8111-111111111111.jsonl",
        ),
        created_at,
        fresh_binding_at: Some(created_at),
        first_event_at: Some(created_at),
        last_activity: created_at,
        projection: LocalSessionProjection::IdentityOnly,
    }
}

#[cfg(test)]
mod tests;
