//! Codex's active and archived rollout stores mapped into local observations.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use jiff::{Timestamp, civil::Date};
use uuid::Uuid;

use super::rollout::{CodexRolloutHeader, read_rollout_header};
#[cfg(test)]
use crate::agents::local_session_cache::ValueRefreshKind;
use crate::agents::local_session_cache::{
    IncrementalCatalog, ProviderPathStamp, StableValueCache, StampedPaths,
    normalized_workspace_inputs,
};
use crate::agents::{LocalSessionObservation, LocalSessionProjection};
use crate::ids::{AgentKind, AgentSessionId};

const MAX_EXAMINED_FILES: usize = 512;
const MAX_SESSION_AGE_DAYS: usize = 14;

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
}

pub(super) fn discover(workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
    let Some(home) = super::app_server::codex_home() else {
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
                let Ok(entries) = fs::read_dir(day) else {
                    continue;
                };
                catalog.extend(
                    entries
                        .filter_map(Result::ok)
                        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                        .map(|entry| entry.path())
                        .filter(|path| rollout_filename(path).is_some())
                        .map(|path| RolloutCandidate { path, active: true }),
                );
            }
            if let Ok(entries) = fs::read_dir(&archive) {
                catalog.extend(
                    entries
                        .filter_map(Result::ok)
                        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                        .map(|entry| entry.path())
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
            }
            catalog
        });
        let forced = scan.attempted();
        #[cfg(test)]
        if forced {
            self.work.full_scans += 1;
        }

        let active_session_ids = self
            .catalog
            .entries()
            .iter()
            .filter(|candidate| candidate.active)
            .filter_map(|candidate| session_id_from_filename(&candidate.path))
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
            let filename_session = session_id_from_filename(&candidate.path);
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
                if let Some(session_id) = filename_session {
                    session_ids_seen.insert(session_id);
                }
                session_ids_seen.insert(observation.session_id.to_string());
                observations.push(observation);
            }
        }
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

    fn refresh_header(
        &mut self,
        candidate: RolloutCandidate,
        forced: bool,
    ) -> (Option<CodexRolloutHeader>, ProviderPathStamp) {
        let dependencies = StampedPaths::exact([candidate.path.clone()]);
        let stamp = dependencies
            .iter()
            .next()
            .map(|(_, stamp)| stamp.clone())
            .unwrap_or_else(|| ProviderPathStamp::read(&candidate.path));
        let result = self
            .headers
            .refresh(candidate.clone(), dependencies, forced, |_| {
                stamp
                    .is_file()
                    .then(|| read_rollout_header(&candidate.path))
                    .flatten()
            });
        #[cfg(test)]
        if result.kind() != ValueRefreshKind::Cached {
            self.work.header_reads += 1;
        }
        (result.into_current().flatten(), stamp)
    }
}

fn recent_dates(today: Date) -> Vec<Date> {
    let mut dates = vec![today];
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
    if header.is_subagent {
        return None;
    }
    let workspace = *workspaces.get(header.cwd.as_deref()?)?;
    let session_id = header
        .session_id
        .or_else(|| session_id_from_filename(&path))?;
    let created_at = header.timestamp?;
    let last_activity = timestamp_from_stamp(&stamp)
        .or_else(|| file_mtime(&path))
        .unwrap_or(created_at)
        .max(created_at);
    Some(LocalSessionObservation {
        kind: AgentKind::new_unchecked("codex"),
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

fn timestamp_from_stamp(stamp: &ProviderPathStamp) -> Option<Timestamp> {
    let since_epoch = stamp.modified?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Timestamp::new(
        i64::try_from(since_epoch.as_secs()).ok()?,
        since_epoch.subsec_nanos() as i32,
    )
    .ok()
}

fn session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let suffix = stem.get(stem.len().checked_sub(36)?..)?;
    Uuid::parse_str(suffix).ok().map(|_| suffix.to_owned())
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

#[cfg(test)]
pub(super) fn fixture_observation() -> LocalSessionObservation {
    let created_at = "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
    LocalSessionObservation {
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
mod tests {
    use std::fs::FileTimes;
    use std::io::Write as _;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn date_path(root: &Path, archived: bool, days_ago: usize) -> PathBuf {
        let mut date = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
        for _ in 0..days_ago {
            date = date.yesterday().unwrap();
        }
        let root = root.join(if archived {
            "archived_sessions"
        } else {
            "sessions"
        });
        if archived {
            root
        } else {
            root.join(format!("{:04}", date.year()))
                .join(format!("{:02}", date.month()))
                .join(format!("{:02}", date.day()))
        }
    }

    fn write_rollout(
        dir: &Path,
        id: &str,
        cwd: &str,
        created_at: &str,
        modified_secs: u64,
    ) -> PathBuf {
        write_rollout_with_extra(dir, id, cwd, created_at, modified_secs, "")
    }

    fn write_rollout_with_extra(
        dir: &Path,
        id: &str,
        cwd: &str,
        created_at: &str,
        modified_secs: u64,
        extra: &str,
    ) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let today = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
        let path = dir.join(format!(
            "rollout-{:04}-{:02}-{:02}T00-00-00-{id}.jsonl",
            today.year(),
            today.month(),
            today.day()
        ));
        fs::write(
            &path,
            format!(
                r#"{{"timestamp":"{created_at}","type":"session_meta","payload":{{"id":"{id}","cwd":"{cwd}"{extra}}}}}"#
            ),
        )
        .unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(modified_secs)),
            )
            .unwrap();
        path
    }

    #[test]
    fn discovers_recent_matching_rollouts_and_prefers_active_twins() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Path::new("/workspace/project");
        let active_today = date_path(temp.path(), false, 0);
        let active_yesterday = date_path(temp.path(), false, 1);
        let archive_today = date_path(temp.path(), true, 0);
        let first = "11111111-1111-4111-8111-111111111111";
        let second = "22222222-2222-4222-8222-222222222222";
        let archived = "33333333-3333-4333-8333-333333333333";
        let active_first = write_rollout(
            &active_today,
            first,
            "/workspace/project",
            "2025-01-03T00:00:00Z",
            1_736_035_500,
        );
        write_rollout(
            &active_yesterday,
            second,
            "/workspace/project",
            "2025-01-02T00:00:00Z",
            1_735_949_000,
        );
        write_rollout(
            &active_today,
            "44444444-4444-4444-8444-444444444444",
            "/other",
            "2025-01-04T00:00:00Z",
            1_736_121_600,
        );
        write_rollout(
            &archive_today,
            archived,
            "/workspace/project",
            "2025-01-01T00:00:00Z",
            1_735_689_700,
        );
        write_rollout(
            &archive_today,
            first,
            "/workspace/project",
            "2025-01-03T00:00:00Z",
            1_736_200_000,
        );

        let observations = discover_under(temp.path(), &[workspace]);

        assert_eq!(
            observations
                .iter()
                .map(|observation| observation.session_id.as_str())
                .collect::<Vec<_>>(),
            [first, second, archived]
        );
        assert_eq!(observations[0].transcript_path, active_first);
        assert!(
            observations
                .iter()
                .all(|observation| observation.projection == LocalSessionProjection::IdentityOnly)
        );
    }

    #[test]
    fn discovers_requested_workspaces_in_one_rollout_batch() {
        let temp = tempfile::tempdir().unwrap();
        let active = date_path(temp.path(), false, 0);
        let first = "11111111-1111-4111-8111-111111111111";
        let second = "22222222-2222-4222-8222-222222222222";
        write_rollout(
            &active,
            first,
            "/workspace/first/.",
            "2025-01-01T00:00:00Z",
            1_735_689_600,
        );
        write_rollout(
            &active,
            second,
            "/workspace/second",
            "2025-01-02T00:00:00Z",
            1_735_776_000,
        );
        write_rollout(
            &active,
            "33333333-3333-4333-8333-333333333333",
            "/workspace/unrequested",
            "2025-01-03T00:00:00Z",
            1_735_862_400,
        );

        let observations = discover_under(
            temp.path(),
            &[
                Path::new("/workspace/first"),
                Path::new("/workspace/second"),
            ],
        );

        assert_eq!(
            observations
                .iter()
                .map(|observation| (
                    observation.session_id.as_str(),
                    observation.workspace.as_path()
                ))
                .collect::<Vec<_>>(),
            [
                (second, Path::new("/workspace/second")),
                (first, Path::new("/workspace/first")),
            ]
        );
        assert_eq!(
            observations[1].workspace.as_os_str().as_encoded_bytes(),
            b"/workspace/first"
        );
    }

    #[test]
    fn cached_discovery_reuses_headers_and_forces_the_backstop() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = PathBuf::from("/workspace/project");
        let today = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
        let active = date_path(temp.path(), false, 0);
        let id = "11111111-1111-4111-8111-111111111111";
        let path = write_rollout(
            &active,
            id,
            "/workspace/project",
            "2025-01-01T00:00:00Z",
            1_735_689_600,
        );
        let key = DiscoveryKey {
            home: temp.path().to_path_buf(),
            workspaces: vec![workspace],
            today,
        };
        let start = Instant::now();
        let mut snapshot = CodexDiscoverySnapshot::default();

        assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
        assert_eq!(snapshot.work.full_scans, 1);
        assert_eq!(snapshot.work.header_reads, 1);
        assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
        assert_eq!(snapshot.work.header_reads, 1);

        fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"\n{\"type\":\"event_msg\"}\n")
            .unwrap();
        assert_eq!(snapshot.refresh(key.clone(), start).len(), 1);
        assert_eq!(snapshot.work.full_scans, 1);
        assert_eq!(snapshot.work.header_reads, 2);

        assert_eq!(
            snapshot.refresh(key, start + Duration::from_secs(30)).len(),
            1
        );
        assert_eq!(snapshot.work.full_scans, 2);
        assert_eq!(snapshot.work.header_reads, 3);
    }

    #[test]
    fn examines_at_most_the_bounded_number_of_rollouts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Path::new("/workspace/project");
        let active = date_path(temp.path(), false, 0);
        for index in 0..=MAX_EXAMINED_FILES {
            let id = format!("{index:08x}-1111-4111-8111-{index:012x}");
            write_rollout(
                &active,
                &id,
                "/workspace/project",
                "2025-01-01T00:00:00Z",
                1_735_689_600 + index as u64,
            );
        }

        assert_eq!(
            discover_under(temp.path(), &[workspace]).len(),
            MAX_EXAMINED_FILES
        );
    }

    #[test]
    fn excludes_v2_and_structured_subagents_but_keeps_user_forks() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Path::new("/workspace/project");
        let active = date_path(temp.path(), false, 0);
        let archived = date_path(temp.path(), true, 0);
        let root = "11111111-1111-4111-8111-111111111111";
        let fork = "22222222-2222-4222-8222-222222222222";
        let v2_child = "33333333-3333-4333-8333-333333333333";
        let structured_child = "44444444-4444-4444-8444-444444444444";
        write_rollout(
            &active,
            root,
            "/workspace/project",
            "2026-01-01T00:00:00Z",
            100,
        );
        write_rollout_with_extra(
            &active,
            fork,
            "/workspace/project",
            "2026-01-01T00:00:01Z",
            200,
            &format!(r#","forked_from_id":"{root}","thread_source":"user""#),
        );
        write_rollout_with_extra(
            &active,
            v2_child,
            "/workspace/project",
            "2026-01-01T00:00:02Z",
            300,
            &format!(r#","thread_source":"subagent","parent_thread_id":"{root}""#),
        );
        write_rollout_with_extra(
            &archived,
            structured_child,
            "/workspace/project",
            "2026-01-01T00:00:03Z",
            400,
            &format!(
                r#","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{root}","depth":1}}}}}}"#
            ),
        );

        let ids = discover_under(temp.path(), &[workspace])
            .into_iter()
            .map(|observation| observation.session_id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(ids, [fork, root]);
    }
}
