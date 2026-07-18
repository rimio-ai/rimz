//! Claude's machine-local session store mapped into provider-neutral observations.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::io::BufRead as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use jiff::Timestamp;
use serde::Deserialize;
use uuid::Uuid;

use super::spend::claude_config_dirs;
#[cfg(test)]
use crate::agents::local_session_cache::ValueRefreshKind;
use crate::agents::local_session_cache::{
    IncrementalCatalog, ProviderPathStamp, StableValueCache, StampedPaths,
    normalized_workspace_inputs,
};
use crate::agents::{LocalSessionObservation, LocalSessionProjection, read_transcript_tail};
use crate::ids::{AgentKind, AgentSessionId};

const MAX_DISCOVERED_SESSIONS: usize = 512;
const MAX_SESSION_HEAD_LINES: usize = 32;
const MAX_SESSION_HEAD_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeSessionRecord {
    timestamp: Option<String>,
    cwd: Option<PathBuf>,
    #[serde(default)]
    is_sidechain: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveryKey {
    config_dirs: Vec<PathBuf>,
    workspaces: Vec<PathBuf>,
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
}

pub(super) fn discover(workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
    let key = DiscoveryKey {
        config_dirs: claude_config_dirs(),
        workspaces: normalized_workspace_inputs(workspaces),
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
                    let project_dir = config_dir
                        .join("projects")
                        .join(project_directory_name(workspace));
                    collect_catalog(config_dir, workspace, &project_dir, topology, &mut catalog);
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

        self.candidates
            .retain(|entry| self.catalog.entries().contains(entry));
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
                    .then(|| observation(entry.path.clone(), &entry.workspace))
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
    };
    if key.workspaces.is_empty() {
        return Vec::new();
    }
    ClaudeDiscoverySnapshot::default().refresh(key, Instant::now())
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

fn observation(path: PathBuf, workspace: &Path) -> Option<LocalSessionObservation> {
    let session_id = path.file_stem()?.to_str()?;
    Uuid::parse_str(session_id).ok()?;
    let first = first_session_record(&path)?;
    if first.is_sidechain || first.cwd.as_deref() != Some(workspace) {
        return None;
    }
    let created_at = first.timestamp?.parse::<Timestamp>().ok()?;
    let last_activity = tail_timestamp(&path)
        .or_else(|| file_mtime(&path))
        .unwrap_or(created_at)
        .max(created_at);
    Some(LocalSessionObservation {
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
    let since_epoch = fs::metadata(path)
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

fn project_directory_name(workspace: &Path) -> String {
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

#[cfg(test)]
pub(super) fn fixture_observation() -> LocalSessionObservation {
    let created_at = "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
    LocalSessionObservation {
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
mod tests {
    use std::io::Write as _;
    use std::time::Duration;

    use super::*;

    fn write_session(dir: &Path, id: &str, records: &[&str]) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(format!("{id}.jsonl")), records.join("\n")).unwrap();
    }

    #[test]
    fn discovers_only_root_sessions_for_the_exact_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Path::new("/home/marvin/.agents");
        let project = temp.path().join("projects").join("-home-marvin--agents");
        write_session(
            &project,
            "11111111-1111-4111-8111-111111111111",
            &[
                r#"{"type":"mode","mode":"default"}"#,
                r#"{"type":"permission-mode","mode":"default"}"#,
                r#"{"type":"bridge-session","sessionId":"11111111-1111-4111-8111-111111111111"}"#,
                r#"{"type":"file-history-snapshot","snapshot":{}}"#,
                r#"{"timestamp":"2025-01-01T00:00:00Z","cwd":"/home/marvin/.agents"}"#,
                r#"{"timestamp":"2025-01-01T00:05:00Z","cwd":"/home/marvin/.agents"}"#,
            ],
        );
        write_session(
            &project,
            "22222222-2222-4222-8222-222222222222",
            &[r#"{"timestamp":"2025-01-02T00:00:00Z","cwd":"/home/marvin/.agents"}"#],
        );
        write_session(
            &project,
            "33333333-3333-4333-8333-333333333333",
            &[r#"{"timestamp":"2025-01-03T00:00:00Z","cwd":"/other"}"#],
        );
        write_session(
            &project,
            "44444444-4444-4444-8444-444444444444",
            &[
                r#"{"timestamp":"2025-01-04T00:00:00Z","cwd":"/home/marvin/.agents","isSidechain":true}"#,
            ],
        );
        write_session(&project, "55555555-5555-4555-8555-555555555555", &[]);
        write_session(
            &project,
            "agent-not-a-root-session",
            &[r#"{"timestamp":"2025-01-05T00:00:00Z","cwd":"/home/marvin/.agents"}"#],
        );

        let observations = discover_under(temp.path(), workspace);

        assert_eq!(
            observations
                .iter()
                .map(|observation| observation.session_id.as_str())
                .collect::<Vec<_>>(),
            [
                "22222222-2222-4222-8222-222222222222",
                "11111111-1111-4111-8111-111111111111"
            ]
        );
        assert_eq!(
            observations[1].last_activity,
            "2025-01-01T00:05:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(
            observations[1].created_at,
            "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap()
        );
        assert!(
            observations
                .iter()
                .all(|observation| observation.projection == LocalSessionProjection::IdentityOnly)
        );
    }

    #[test]
    fn cached_discovery_reparses_only_changed_candidates_and_backstops() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = PathBuf::from("/workspace/project");
        let project = temp.path().join("projects").join("-workspace-project");
        let id = "11111111-1111-4111-8111-111111111111";
        write_session(
            &project,
            id,
            &[r#"{"timestamp":"2025-01-01T00:00:00Z","cwd":"/workspace/project"}"#],
        );
        let key = DiscoveryKey {
            config_dirs: vec![temp.path().to_path_buf()],
            workspaces: vec![workspace],
        };
        let start = Instant::now();
        let mut snapshot = ClaudeDiscoverySnapshot::default();

        assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
        assert_eq!(snapshot.work.full_scans, 1);
        assert_eq!(snapshot.work.candidate_parses, 1);
        assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
        assert_eq!(snapshot.work.candidate_parses, 1);

        fs::OpenOptions::new()
            .append(true)
            .open(project.join(format!("{id}.jsonl")))
            .unwrap()
            .write_all(
                b"\n{\"timestamp\":\"2025-01-01T00:01:00Z\",\"cwd\":\"/workspace/project\"}\n",
            )
            .unwrap();
        assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
        assert_eq!(snapshot.work.full_scans, 1);
        assert_eq!(snapshot.work.candidate_parses, 2);

        assert_eq!(
            snapshot.refresh(key, start + Duration::from_secs(30)).len(),
            1
        );
        assert_eq!(snapshot.work.full_scans, 2);
        assert_eq!(snapshot.work.candidate_parses, 3);
    }

    #[test]
    fn batched_discovery_retains_the_cap_for_each_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().to_path_buf();
        let first = PathBuf::from("/workspace/first");
        let second = PathBuf::from("/workspace/second");
        let key = DiscoveryKey {
            config_dirs: vec![config_dir.clone()],
            workspaces: vec![first.clone(), second.clone()],
        };
        let start = Instant::now();
        let mut snapshot = ClaudeDiscoverySnapshot::default();
        let mut fixtures = Vec::new();

        for (workspace, count, timestamp) in [(&first, MAX_DISCOVERED_SESSIONS, 2), (&second, 2, 1)]
        {
            for index in 0..count {
                let path = temp.path().join(format!("{}-{index}.jsonl", timestamp));
                fs::write(&path, []).unwrap();
                let entry = CatalogEntry {
                    config_dir: config_dir.clone(),
                    workspace: workspace.clone(),
                    path: path.clone(),
                };
                let at = Timestamp::from_second(timestamp).unwrap();
                let observation = LocalSessionObservation {
                    kind: AgentKind::new_unchecked("claude"),
                    session_id: AgentSessionId::from(format!("{timestamp}-{index}")),
                    workspace: workspace.clone(),
                    transcript_path: path,
                    created_at: at,
                    fresh_binding_at: Some(at),
                    first_event_at: Some(at),
                    last_activity: at,
                    projection: LocalSessionProjection::IdentityOnly,
                };
                fixtures.push((entry, observation));
            }
        }
        snapshot.catalog.refresh(key.clone(), start, |_| {
            fixtures.iter().map(|(entry, _)| entry.clone()).collect()
        });
        for (entry, observation) in fixtures {
            snapshot.candidates.refresh(
                entry.clone(),
                StampedPaths::exact([entry.path.clone()]),
                false,
                |_| Some(observation),
            );
        }

        let observations = snapshot.refresh(key, start);
        assert_eq!(observations.len(), MAX_DISCOVERED_SESSIONS + 2);
        assert_eq!(
            observations
                .iter()
                .filter(|observation| observation.workspace == first)
                .count(),
            MAX_DISCOVERED_SESSIONS
        );
        assert_eq!(
            observations
                .iter()
                .filter(|observation| observation.workspace == second)
                .count(),
            2
        );
    }
}
