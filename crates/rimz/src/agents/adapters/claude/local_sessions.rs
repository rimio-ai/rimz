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
    project_dir_name: Option<String>,
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
pub(super) fn conversation_present(session_id: &AgentSessionId, cwd: &Path) -> Option<bool> {
    let config_dirs = claude_config_dirs();
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
    let workspace = crate::worktree::normalize_path_lexical(cwd);
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
    fn conversation_presence_follows_the_workspace_the_resume_would_open() {
        let temp = tempfile::tempdir().unwrap();
        let config_dirs = vec![temp.path().to_path_buf()];
        let workspace = Path::new("/repo/account-change");
        let id = AgentSessionId::from("06e78f43-ecc1-486b-b50d-3c1f7770a5ae");
        let project = temp.path().join("projects").join("-repo-account-change");

        assert!(!conversation_present_under(&config_dirs, &id, workspace));

        write_session(&project, id.as_str(), &[r#"{"type":"user"}"#]);
        assert!(conversation_present_under(&config_dirs, &id, workspace));

        // The same id under another workspace is not this workspace's session.
        assert!(!conversation_present_under(
            &config_dirs,
            &id,
            Path::new("/repo/other")
        ));

        // An unwritten session carries an id and an empty file.
        let unwritten = AgentSessionId::from("11111111-1111-4111-8111-111111111111");
        write_session(&project, unwritten.as_str(), &[]);
        assert!(!conversation_present_under(
            &config_dirs,
            &unwritten,
            workspace
        ));
    }

    #[test]
    fn conversation_presence_normalizes_the_workspace_path() {
        let temp = tempfile::tempdir().unwrap();
        let config_dirs = vec![temp.path().to_path_buf()];
        let id = AgentSessionId::from("06e78f43-ecc1-486b-b50d-3c1f7770a5ae");
        write_session(
            &temp.path().join("projects").join("-repo-worktrees-lane"),
            id.as_str(),
            &[r#"{"type":"user"}"#],
        );
        assert!(conversation_present_under(
            &config_dirs,
            &id,
            Path::new("/repo/nested/../worktrees/lane")
        ));
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
            project_dir_name: None,
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
            project_dir_name: None,
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

    #[test]
    fn project_directory_override_precedes_the_flattened_fallback() {
        assert_eq!(
            project_directory_names_from(Path::new("/repo/main"), Some("short-name")),
            ["short-name", "-repo-main"]
        );
        assert_eq!(
            project_directory_names_from(Path::new("/repo/main"), Some("")),
            ["-repo-main"]
        );
        assert_eq!(
            project_directory_names_from(Path::new("/repo/main"), Some("-repo-main")),
            ["-repo-main"]
        );
    }

    #[test]
    fn override_bucket_discovery_keeps_transcript_cwd_authoritative() {
        let temp = tempfile::tempdir().unwrap();
        let first = PathBuf::from("/workspace/first");
        let second = PathBuf::from("/workspace/second");
        let shared = temp.path().join("projects/shared");
        write_session(
            &shared,
            "11111111-1111-4111-8111-111111111111",
            &[r#"{"timestamp":"2025-01-01T00:00:00Z","cwd":"/workspace/first"}"#],
        );
        write_session(
            &shared,
            "22222222-2222-4222-8222-222222222222",
            &[r#"{"timestamp":"2025-01-02T00:00:00Z","cwd":"/workspace/second"}"#],
        );
        let key = DiscoveryKey {
            config_dirs: vec![temp.path().to_path_buf()],
            workspaces: vec![first.clone(), second.clone()],
            project_dir_name: Some("shared".to_owned()),
        };

        let observations = ClaudeDiscoverySnapshot::default().refresh(key, Instant::now());
        assert_eq!(observations.len(), 2);
        assert!(observations.iter().any(|observation| {
            observation.session_id == "11111111-1111-4111-8111-111111111111"
                && observation.workspace == first
        }));
        assert!(observations.iter().any(|observation| {
            observation.session_id == "22222222-2222-4222-8222-222222222222"
                && observation.workspace == second
        }));
    }

    #[test]
    fn conversation_presence_checks_override_and_flattened_buckets() {
        let temp = tempfile::tempdir().unwrap();
        let config_dirs = vec![temp.path().to_path_buf()];
        let workspace = Path::new("/repo/main");
        let override_id = AgentSessionId::from("11111111-1111-4111-8111-111111111111");
        let fallback_id = AgentSessionId::from("22222222-2222-4222-8222-222222222222");
        write_session(
            &temp.path().join("projects/short-name"),
            override_id.as_str(),
            &[r#"{"type":"user"}"#],
        );
        write_session(
            &temp.path().join("projects/-repo-main"),
            fallback_id.as_str(),
            &[r#"{"type":"user"}"#],
        );

        assert!(conversation_present_under_with(
            &config_dirs,
            &override_id,
            workspace,
            Some("short-name")
        ));
        assert!(conversation_present_under_with(
            &config_dirs,
            &fallback_id,
            workspace,
            Some("short-name")
        ));
    }
}
