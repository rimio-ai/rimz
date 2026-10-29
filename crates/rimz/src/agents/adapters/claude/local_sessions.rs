//! Claude's machine-local session store mapped into provider-neutral observations.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::BufRead as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use jiff::Timestamp;
use serde::Deserialize;
use uuid::Uuid;

use super::spend::{claude_config_dirs_from, claude_config_roots};
#[cfg(test)]
use crate::agents::local_session_cache::ValueRefreshKind;
use crate::agents::local_session_cache::{
    IncrementalCatalog, ProviderPathStamp, StableValueCache, StampedPaths, entry_file_type,
    full_scan_due, metadata, normalized_workspace_inputs, read_dir,
};
use crate::agents::session::PaneSessionDemand;
use crate::agents::{LocalSessionObservation, LocalSessionProjection, read_transcript_tail};
use crate::ids::{AgentKind, AgentSessionId};

const MAX_DISCOVERED_SESSIONS: usize = 512;
const MAX_SESSION_HEAD_LINES: usize = 32;
const MAX_SESSION_HEAD_BYTES: u64 = 64 * 1024;

pub(super) fn locate(
    session_id: &AgentSessionId,
    login_env: &std::collections::BTreeMap<String, String>,
) -> Option<LocalSessionObservation> {
    use crate::agents::capabilities::LaunchCapability as _;

    Uuid::parse_str(session_id.as_str()).ok()?;
    let home = super::ClaudeAdapter.config_home(login_env)?;
    let projects = fs::read_dir(home.join("projects")).ok()?;
    for project in projects.filter_map(Result::ok) {
        if !project.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = project.path().join(format!("{session_id}.jsonl"));
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        let Some(first) = std::io::BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<ClaudeSessionRecord>(&line).ok())
            .find(|record| record.cwd.is_some() && record.timestamp.is_some())
        else {
            continue;
        };
        let Some(cwd) = first.cwd.clone().filter(|cwd| cwd.is_absolute()) else {
            continue;
        };
        if let Some(found) = observation(path, &cwd, first) {
            return Some(found);
        }
    }
    None
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeSessionRecord {
    timestamp: Option<String>,
    cwd: Option<PathBuf>,
    #[serde(default)]
    is_sidechain: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DiscoveryKey {
    config_dirs: Vec<PathBuf>,
    workspaces: Vec<PathBuf>,
    project_dir_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PaneDiscoveryKey {
    config_dirs: Vec<PathBuf>,
    project_dir_name: Option<String>,
}

impl From<&DiscoveryKey> for PaneDiscoveryKey {
    fn from(key: &DiscoveryKey) -> Self {
        Self {
            config_dirs: key.config_dirs.clone(),
            project_dir_name: key.project_dir_name.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CatalogEntry {
    config_dir: PathBuf,
    workspace: PathBuf,
    path: PathBuf,
}

#[derive(Default)]
struct ClaudeDiscoverySnapshot {
    catalog: IncrementalCatalog<DiscoveryKey, CatalogEntry>,
    candidates: StableValueCache<CatalogEntry, Option<LocalSessionObservation>>,
    #[cfg(test)]
    work: DiscoveryWork,
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct DiscoveryWork {
    full_scans: usize,
    candidate_parses: usize,
}

thread_local! {
    static DISCOVERY: RefCell<ClaudeDiscoverySnapshot> = RefCell::new(ClaudeDiscoverySnapshot::default());
    // Account entries expire after one discovery backstop without use.
    static PANE_DISCOVERY: RefCell<HashMap<PaneDiscoveryKey, (Instant, ClaudePaneDiscovery)>> =
        RefCell::new(HashMap::new());
}

#[derive(Default)]
struct ClaudePaneDiscovery {
    key: Option<PaneDiscoveryKey>,
    catalog: IncrementalCatalog<Vec<(PathBuf, Timestamp)>, CatalogEntry>,
    // Learned heads survive demand changes until a fresh listing removes them.
    candidates: StableValueCache<CatalogEntry, PaneCandidate>,
}

#[derive(Clone)]
enum PaneCandidate {
    /// No readable head yet, with the file's modification time when checked.
    Pending(Option<SystemTime>),
    Rejected,
    Found(Box<LocalSessionObservation>),
}

impl PaneCandidate {
    fn observation(&self) -> Option<&LocalSessionObservation> {
        match self {
            Self::Found(observation) => Some(observation),
            Self::Pending(_) | Self::Rejected => None,
        }
    }

    /// A pending head last written before `since` cannot start a session
    /// inside the window, so it waits for the next listing.
    fn pending_since(&self, since: Timestamp) -> bool {
        match self {
            Self::Pending(modified) => modified.is_none_or(|modified| {
                Timestamp::try_from(modified).is_ok_and(|modified| modified >= since)
            }),
            Self::Rejected | Self::Found(_) => false,
        }
    }
}

pub(super) fn discover_panes(
    demand: &[PaneSessionDemand],
    login_env: &std::collections::BTreeMap<String, String>,
) -> Vec<LocalSessionObservation> {
    if demand.is_empty() {
        return Vec::new();
    }
    let workspaces = demand
        .iter()
        .map(|demand| demand.workspace.as_path())
        .collect::<Vec<_>>();
    let key = DiscoveryKey {
        config_dirs: claude_config_roots(login_env),
        workspaces: normalized_workspace_inputs(&workspaces),
        project_dir_name: project_directory_name_override(),
    };
    let now = Instant::now();
    PANE_DISCOVERY.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.retain(|_, (last_used, _)| !full_scan_due(Some(*last_used), now));
        let (last_used, snapshot) = cache
            .entry(PaneDiscoveryKey::from(&key))
            .or_insert_with(|| (now, ClaudePaneDiscovery::default()));
        *last_used = now;
        snapshot.refresh(key, demand, now)
    })
}

impl ClaudePaneDiscovery {
    fn refresh(
        &mut self,
        key: DiscoveryKey,
        demand: &[PaneSessionDemand],
        now: Instant,
    ) -> Vec<LocalSessionObservation> {
        let account = PaneDiscoveryKey::from(&key);
        if self.key.as_ref() != Some(&account) {
            *self = Self::default();
            self.key = Some(account);
        }
        let mut checked = HashMap::new();
        let mut observations = Vec::new();
        for demand in demand {
            for session in demand
                .panes
                .iter()
                .flat_map(|pane| pane.own.iter().chain(&pane.named))
            {
                if Uuid::parse_str(session.id.as_str()).is_err() {
                    continue;
                }
                for (config_dir, project) in project_directories(&key, &demand.workspace) {
                    let entry = CatalogEntry {
                        config_dir,
                        workspace: demand.workspace.clone(),
                        path: project.join(format!("{}.jsonl", session.id)),
                    };
                    if let Some(found) = self.check_candidate(&entry, &mut checked) {
                        observations.push(found);
                    }
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
        if !windows.is_empty() {
            let refresh = self.catalog.refresh(windows.clone(), now, |topology| {
                let mut entries = Vec::new();
                let mut seen = HashSet::new();
                let mut listed = HashSet::new();
                for (workspace, _) in &windows {
                    for (config_dir, project) in project_directories(&key, workspace) {
                        if !seen.insert((workspace.clone(), project.clone())) {
                            continue;
                        }
                        topology.record_exact(project.clone());
                        let Ok(files) = read_dir(&project) else {
                            continue;
                        };
                        listed.insert(project);
                        entries.extend(files.filter_map(Result::ok).filter_map(|file| {
                            let path = file.path();
                            if !entry_file_type(&file).is_ok_and(|kind| kind.is_file())
                                || path.extension().is_none_or(|ext| ext != "jsonl")
                                || path
                                    .file_stem()
                                    .and_then(|id| id.to_str())
                                    .is_none_or(|id| Uuid::parse_str(id).is_err())
                            {
                                return None;
                            }
                            Some(CatalogEntry {
                                config_dir: config_dir.clone(),
                                workspace: workspace.clone(),
                                path,
                            })
                        }));
                    }
                }
                let paths = entries
                    .iter()
                    .map(|entry| &entry.path)
                    .collect::<HashSet<_>>();
                self.candidates.retain(|entry| {
                    entry.path.parent().is_none_or(|dir| !listed.contains(dir))
                        || paths.contains(&entry.path)
                });
                entries
            });
            // Creation is immutable. Learn it once for history, then stat only
            // the selected window; a pending head is rechecked each pass while
            // written inside its window, and otherwise when the listing reruns.
            let due = |entry: &CatalogEntry| match self.candidates.get(entry) {
                None => true,
                Some(PaneCandidate::Pending(_)) if refresh.attempted() => true,
                Some(candidate) => windows.iter().any(|(workspace, since)| {
                    *workspace == entry.workspace && candidate.pending_since(*since)
                }),
            };
            unread = self
                .catalog
                .entries()
                .iter()
                .filter(|entry| due(entry))
                .cloned()
                .collect();
        }
        for entry in &unread {
            self.check_candidate(entry, &mut checked);
        }
        for (workspace, since) in windows {
            let named = demand
                .iter()
                .filter(|demand| demand.workspace == workspace)
                .flat_map(|demand| &demand.panes)
                .flat_map(|pane| pane.own.iter().chain(&pane.named))
                .map(|session| &session.id)
                .collect::<HashSet<_>>();
            let mut sessions: HashMap<AgentSessionId, Vec<CatalogEntry>> = HashMap::new();
            for entry in self
                .catalog
                .entries()
                .iter()
                .filter(|entry| entry.workspace == workspace)
            {
                let Some(row) = self
                    .candidates
                    .get(entry)
                    .and_then(PaneCandidate::observation)
                else {
                    continue;
                };
                if row.created_at >= since && !named.contains(&row.session_id) {
                    sessions
                        .entry(row.session_id.clone())
                        .or_default()
                        .push(entry.clone());
                }
            }
            let mut candidates = sessions.into_iter().collect::<Vec<_>>();
            candidates.sort_by_cached_key(|(id, paths)| {
                (
                    std::cmp::Reverse(
                        paths
                            .iter()
                            .filter_map(|entry| {
                                self.candidates
                                    .get(entry)
                                    .and_then(PaneCandidate::observation)
                            })
                            .map(|row| row.created_at)
                            .max(),
                    ),
                    id.clone(),
                )
            });
            let mut window = candidates
                .into_iter()
                .filter_map(|(_, paths)| {
                    paths
                        .iter()
                        .filter_map(|entry| self.check_candidate(entry, &mut checked))
                        .filter(|row| row.created_at >= since)
                        .min_by(observation_cmp)
                })
                .take(MAX_DISCOVERED_SESSIONS)
                .collect::<Vec<_>>();
            window.sort_by(|left, right| {
                right
                    .created_at
                    .cmp(&left.created_at)
                    .then_with(|| observation_cmp(left, right))
            });
            observations.extend(window);
        }
        observations.sort_by(observation_cmp);
        let mut seen = HashSet::new();
        observations.retain(|row| seen.insert((row.workspace.clone(), row.session_id.clone())));
        observations
    }

    fn check_candidate(
        &mut self,
        entry: &CatalogEntry,
        checked: &mut HashMap<CatalogEntry, Option<LocalSessionObservation>>,
    ) -> Option<LocalSessionObservation> {
        checked
            .entry(entry.clone())
            .or_insert_with(|| {
                let dependencies = StampedPaths::exact([entry.path.clone()]);
                self.candidates
                    .refresh(entry.clone(), dependencies, false, |dependencies| {
                        let Some((_, stamp)) = dependencies
                            .iter()
                            .next()
                            .filter(|(_, stamp)| stamp.is_file())
                        else {
                            return PaneCandidate::Rejected;
                        };
                        let Some(first) = first_session_record(&entry.path) else {
                            return PaneCandidate::Pending(stamp.modified);
                        };
                        observation(entry.path.clone(), &entry.workspace, first)
                            .map_or(PaneCandidate::Rejected, |row| {
                                PaneCandidate::Found(Box::new(row))
                            })
                    })
                    .into_current()
                    .and_then(|candidate| candidate.observation().cloned())
            })
            .clone()
    }
}

fn project_directories(key: &DiscoveryKey, workspace: &Path) -> Vec<(PathBuf, PathBuf)> {
    key.config_dirs
        .iter()
        .flat_map(|root| {
            project_directory_names_from(workspace, key.project_dir_name.as_deref())
                .into_iter()
                .filter_map(|name| {
                    let projects = root.join("projects");
                    let path = projects.join(name);
                    let normalized = crate::utils::path::normalize_path_lexical(&path);
                    let projects = crate::utils::path::normalize_path_lexical(&projects);
                    (normalized.starts_with(&projects) && normalized != projects)
                        .then(|| (root.clone(), path))
                })
        })
        .collect()
}

pub(super) fn discover(
    workspaces: &[&Path],
    login_env: &std::collections::BTreeMap<String, String>,
) -> Vec<LocalSessionObservation> {
    let key = DiscoveryKey {
        config_dirs: claude_config_dirs_from(login_env),
        workspaces: normalized_workspace_inputs(workspaces),
        project_dir_name: project_directory_name_override(),
    };
    if key.config_dirs.is_empty() || key.workspaces.is_empty() {
        return Vec::new();
    }
    DISCOVERY.with(|snapshot| snapshot.borrow_mut().refresh(key, Instant::now()))
}

impl ClaudeDiscoverySnapshot {
    fn refresh(&mut self, key: DiscoveryKey, now: Instant) -> Vec<LocalSessionObservation> {
        let scan = self.catalog.refresh(key.clone(), now, |topology| {
            let mut catalog = Vec::new();
            for config_dir in &key.config_dirs {
                for workspace in &key.workspaces {
                    for project in
                        project_directory_names_from(workspace, key.project_dir_name.as_deref())
                    {
                        let project_dir = config_dir.join("projects").join(project);
                        collect_catalog(
                            config_dir,
                            workspace,
                            &project_dir,
                            topology,
                            &mut catalog,
                        );
                    }
                }
            }
            catalog
        });
        let forced = scan.attempted();
        #[cfg(test)]
        if forced {
            self.work.full_scans += 1;
        }

        let mut observations = Vec::new();
        for workspace in &key.workspaces {
            let mut workspace_observations = Vec::new();
            for config_dir in &key.config_dirs {
                let mut bucket = self
                    .catalog
                    .entries()
                    .iter()
                    .filter(|entry| {
                        &entry.config_dir == config_dir && &entry.workspace == workspace
                    })
                    .filter_map(|entry| {
                        let stamp = ProviderPathStamp::read(&entry.path);
                        stamp.is_file().then(|| (entry.clone(), stamp))
                    })
                    .collect::<Vec<_>>();
                bucket.sort_by(|left, right| {
                    right
                        .1
                        .modified
                        .cmp(&left.1.modified)
                        .then_with(|| left.0.path.cmp(&right.0.path))
                });
                workspace_observations.extend(
                    bucket
                        .into_iter()
                        .take(MAX_DISCOVERED_SESSIONS)
                        .filter_map(|(entry, _)| self.refresh_candidate(entry, forced)),
                );
            }
            workspace_observations.sort_by(observation_cmp);
            let mut seen = HashSet::new();
            workspace_observations
                .retain(|observation| seen.insert(observation.session_id.clone()));
            workspace_observations.truncate(MAX_DISCOVERED_SESSIONS);
            observations.extend(workspace_observations);
        }

        let live: HashSet<&CatalogEntry> = self.catalog.entries().iter().collect();
        self.candidates.retain(|entry| live.contains(entry));
        observations
    }

    fn refresh_candidate(
        &mut self,
        entry: CatalogEntry,
        forced: bool,
    ) -> Option<LocalSessionObservation> {
        let dependencies = StampedPaths::exact([entry.path.clone()]);
        let result = self
            .candidates
            .refresh(entry.clone(), dependencies, forced, |dependencies| {
                dependencies
                    .iter()
                    .next()
                    .is_some_and(|(_, stamp)| stamp.is_file())
                    .then(|| {
                        let first = first_session_record(&entry.path)?;
                        observation(entry.path.clone(), &entry.workspace, first)
                    })
                    .flatten()
            });
        #[cfg(test)]
        if result.kind() != ValueRefreshKind::Cached {
            self.work.candidate_parses += 1;
        }
        result.into_current().flatten()
    }
}

fn collect_catalog(
    config_dir: &Path,
    workspace: &Path,
    dir: &Path,
    topology: &mut StampedPaths,
    catalog: &mut Vec<CatalogEntry>,
) {
    let stamp = ProviderPathStamp::read(dir);
    topology.record_exact(dir.to_path_buf());
    if !stamp.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_catalog(config_dir, workspace, &path, topology, catalog);
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
            catalog.push(CatalogEntry {
                config_dir: config_dir.to_path_buf(),
                workspace: workspace.to_path_buf(),
                path,
            });
        }
    }
}

#[cfg(test)]
pub(super) fn discover_under(config_dir: &Path, workspace: &Path) -> Vec<LocalSessionObservation> {
    let key = DiscoveryKey {
        config_dirs: vec![config_dir.to_path_buf()],
        workspaces: normalized_workspace_inputs(&[workspace]),
        project_dir_name: None,
    };
    if key.workspaces.is_empty() {
        return Vec::new();
    }
    ClaudeDiscoverySnapshot::default().refresh(key, Instant::now())
}

/// Resolve the one file `claude --resume <session_id>` opens from `cwd`, and
/// report whether it holds a conversation.
///
/// Claude keys its store by config dir, then by the flattened workspace path,
/// then by session id, so this resolves the same location the provider's own
/// resume resolves — an id recorded against another workspace reads absent
/// here exactly as it would there. `None` means no config dir is readable, so
/// the caller keeps its recorded-transcript fallback rather than declaring a
/// live session gone.
pub(super) fn conversation_present(
    session_id: &AgentSessionId,
    cwd: &Path,
    login_env: &std::collections::BTreeMap<String, String>,
) -> Option<bool> {
    let config_dirs = claude_config_dirs_from(login_env);
    if config_dirs.is_empty() {
        return None;
    }
    Some(conversation_present_under_with(
        &config_dirs,
        session_id,
        cwd,
        project_directory_name_override().as_deref(),
    ))
}

#[cfg(test)]
fn conversation_present_under(
    config_dirs: &[PathBuf],
    session_id: &AgentSessionId,
    cwd: &Path,
) -> bool {
    conversation_present_under_with(config_dirs, session_id, cwd, None)
}

fn conversation_present_under_with(
    config_dirs: &[PathBuf],
    session_id: &AgentSessionId,
    cwd: &Path,
    project_dir_name: Option<&str>,
) -> bool {
    let workspace = crate::utils::path::normalize_path_lexical(cwd);
    let file = format!("{}.jsonl", session_id.as_str());
    config_dirs.iter().any(|config_dir| {
        project_directory_names_from(&workspace, project_dir_name)
            .into_iter()
            .any(|project| {
                fs::metadata(config_dir.join("projects").join(project).join(&file))
                    .is_ok_and(|meta| meta.is_file() && meta.len() > 0)
            })
    })
}

fn observation_cmp(
    left: &LocalSessionObservation,
    right: &LocalSessionObservation,
) -> std::cmp::Ordering {
    right
        .last_activity
        .cmp(&left.last_activity)
        .then_with(|| left.session_id.cmp(&right.session_id))
}

fn observation(
    path: PathBuf,
    workspace: &Path,
    first: ClaudeSessionRecord,
) -> Option<LocalSessionObservation> {
    let session_id = path.file_stem()?.to_str()?;
    Uuid::parse_str(session_id).ok()?;
    if first.is_sidechain || first.cwd.as_deref() != Some(workspace) {
        return None;
    }
    let created_at = first.timestamp?.parse::<Timestamp>().ok()?;
    let last_activity = tail_timestamp(&path)
        .or_else(|| file_mtime(&path))
        .unwrap_or(created_at)
        .max(created_at);
    Some(LocalSessionObservation {
        login: None,
        kind: AgentKind::new_unchecked("claude"),
        session_id: AgentSessionId::from(session_id),
        workspace: workspace.to_path_buf(),
        transcript_path: path,
        created_at,
        fresh_binding_at: Some(created_at),
        first_event_at: Some(created_at),
        last_activity,
        projection: LocalSessionProjection::IdentityOnly,
    })
}

fn first_session_record(path: &Path) -> Option<ClaudeSessionRecord> {
    #[cfg(any(test, feature = "testkit"))]
    crate::agents::local_session_cache::testkit::transcript_read();
    let file = fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader
        .take(MAX_SESSION_HEAD_BYTES)
        .lines()
        .take(MAX_SESSION_HEAD_LINES)
    {
        let Ok(line) = line else { break };
        let Ok(record) = serde_json::from_str::<ClaudeSessionRecord>(&line) else {
            continue;
        };
        if record.cwd.is_some() && record.timestamp.is_some() {
            return Some(record);
        }
    }
    None
}

fn tail_timestamp(path: &Path) -> Option<Timestamp> {
    read_transcript_tail(path)?
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<ClaudeSessionRecord>(line).ok())
        .find_map(|record| record.timestamp?.parse::<Timestamp>().ok())
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

pub(super) fn project_directory_name(workspace: &Path) -> String {
    workspace
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() {
                char::from(*byte)
            } else {
                '-'
            }
        })
        .collect()
}

pub(super) fn project_directory_names(workspace: &Path) -> Vec<String> {
    let project_dir_name = project_directory_name_override();
    project_directory_names_from(workspace, project_dir_name.as_deref())
}

fn project_directory_name_override() -> Option<String> {
    std::env::var("CLAUDE_CODE_PROJECT_DIR_NAME")
        .ok()
        .filter(|value| !value.is_empty())
}

fn project_directory_names_from(workspace: &Path, override_name: Option<&str>) -> Vec<String> {
    let flattened = project_directory_name(workspace);
    match override_name.filter(|name| !name.is_empty() && *name != flattened) {
        Some(name) => vec![name.to_owned(), flattened],
        None => vec![flattened],
    }
}

#[cfg(test)]
pub(super) fn fixture_observation() -> LocalSessionObservation {
    let created_at = "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
    LocalSessionObservation {
        login: None,
        kind: AgentKind::new_unchecked("claude"),
        session_id: AgentSessionId::from("11111111-1111-4111-8111-111111111111"),
        workspace: PathBuf::from("/workspace/project"),
        transcript_path: PathBuf::from(
            "/provider/projects/-workspace-project/11111111-1111-4111-8111-111111111111.jsonl",
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
