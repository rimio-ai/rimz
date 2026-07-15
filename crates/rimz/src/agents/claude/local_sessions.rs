//! Claude's machine-local session store mapped into provider-neutral observations.

use std::collections::HashSet;
use std::fs;
use std::io::BufRead as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;
use uuid::Uuid;

use super::spend::claude_config_dirs;
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

pub(super) fn discover(workspace: &Path) -> Vec<LocalSessionObservation> {
    if !workspace.is_absolute() {
        return Vec::new();
    }
    let mut observations = claude_config_dirs()
        .into_iter()
        .flat_map(|config_dir| discover_under(&config_dir, workspace))
        .collect::<Vec<_>>();
    observations.sort_by(observation_cmp);
    let mut seen = HashSet::new();
    observations.retain(|observation| seen.insert(observation.session_id.clone()));
    observations.truncate(MAX_DISCOVERED_SESSIONS);
    observations
}

pub(super) fn discover_under(config_dir: &Path, workspace: &Path) -> Vec<LocalSessionObservation> {
    if !workspace.is_absolute() {
        return Vec::new();
    }
    let project_dir = config_dir
        .join("projects")
        .join(project_directory_name(workspace));
    let mut transcripts = Vec::new();
    crate::agents::transcript_fs::collect_jsonl(&project_dir, &mut transcripts);
    transcripts.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(std::cmp::Reverse)
    });
    let mut observations = transcripts
        .into_iter()
        .take(MAX_DISCOVERED_SESSIONS)
        .filter_map(|path| observation(path, workspace))
        .collect::<Vec<_>>();
    observations.sort_by(observation_cmp);
    observations
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
}
