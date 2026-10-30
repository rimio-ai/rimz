//! Durable, account-shared session provenance for spending attribution.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::atomic::{self, AtomicErr};
use crate::disk::parse_cache::{FileStamp, ParseCache, StampedPath};
use crate::disk::paths::{RuntimePaths, SpendingHistoryPaths};
use crate::ids::{AgentKind, AgentSessionId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSeat {
    Solo,
    #[serde(untagged)]
    Team {
        team: String,
        role: String,
    },
}

impl AgentSeat {
    pub fn from_identity(
        profile: Option<&str>,
        team: Option<&str>,
        role: Option<&str>,
        kind: &str,
    ) -> Self {
        match team {
            Some(team) => Self::Team {
                team: team.to_owned(),
                role: role.or(profile).unwrap_or(kind).to_owned(),
            },
            None => Self::Solo,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SubagentOwner {
    Team { team: String, role: String },
    Solo { profile: String },
}

pub fn resolve_owner(
    agents: &[crate::agents::AgentState],
    launch_id: &AgentSessionId,
) -> Option<SubagentOwner> {
    let child = agents.iter().find(|agent| agent.answers_to(launch_id))?;
    let parent = agents.iter().find(|parent| child.parent_is(parent))?;
    Some(
        match AgentSeat::from_identity(
            parent.profile.as_deref(),
            parent.team.as_deref(),
            parent.role.as_deref(),
            parent.kind.as_str(),
        ) {
            AgentSeat::Team { team, role } => SubagentOwner::Team { team, role },
            AgentSeat::Solo => SubagentOwner::Solo {
                profile: parent
                    .profile
                    .as_deref()
                    .unwrap_or(parent.kind.as_str())
                    .to_owned(),
            },
        },
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum SessionRole {
    Agent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seat: Option<AgentSeat>,
    },
    Subagent {
        profile: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<SubagentOwner>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Placement<'a> {
    Solo { profile: &'a str },
    Team { team: &'a str, role: &'a str },
    Subagent { profile: &'a str },
}

impl SessionRole {
    pub(super) fn owner_placement(&self) -> Option<Placement<'_>> {
        match self {
            Self::Subagent {
                owner: Some(SubagentOwner::Team { team, role }),
                ..
            } => Some(Placement::Team { team, role }),
            Self::Subagent {
                owner: Some(SubagentOwner::Solo { profile }),
                ..
            } => Some(Placement::Solo { profile }),
            _ => None,
        }
    }

    pub(super) fn placement<'a>(&'a self, provider: &'a str) -> Placement<'a> {
        match self {
            Self::Agent {
                seat: Some(AgentSeat::Team { team, role }),
                ..
            } => Placement::Team { team, role },
            Self::Agent { profile, seat } => {
                if seat.is_none()
                    && let Some((team, role)) = profile
                        .as_deref()
                        .and_then(|profile| profile.split_once('.'))
                    && !team.is_empty()
                    && !role.is_empty()
                {
                    return Placement::Team { team, role };
                }
                Placement::Solo {
                    profile: profile.as_deref().unwrap_or(provider),
                }
            }
            Self::Subagent { profile, .. } => Placement::Subagent { profile },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRoleRecord {
    pub kind: AgentKind,
    pub session: AgentSessionId,
    #[serde(flatten)]
    pub role: SessionRole,
    pub at: Timestamp,
}

pub(super) type RoleMap = BTreeMap<AgentKind, BTreeMap<AgentSessionId, SessionRole>>;

#[derive(Default)]
pub(super) struct RoleIndex {
    pub(super) roles: RoleMap,
    pub(super) recorded_since_secs: Option<u64>,
    pub(super) effort: Vec<Arc<Vec<super::session_effort::SessionEffortRecord>>>,
}

#[derive(Default)]
struct Segment {
    records: Vec<SessionRoleRecord>,
    unfinished_tail: bool,
}

static SEGMENTS: Mutex<BTreeMap<PathBuf, ParseCache<Segment>>> = Mutex::new(BTreeMap::new());

#[must_use = "durability barrier; check the result"]
pub fn record(paths: &RuntimePaths, record: &SessionRoleRecord) -> Result<(), AtomicErr> {
    let year = record.at.to_zoned(jiff::tz::TimeZone::UTC).year();
    let path = paths.shared_session_roles(year);
    let segments = read_segments(&paths.spending_history, year);
    let matches_session = |candidate: &SessionRoleRecord| {
        candidate.kind == record.kind && candidate.session == record.session
    };
    if segments
        .iter()
        .filter(|(segment_year, _)| *segment_year == year)
        .flat_map(|(_, segment)| segment.records.iter())
        .any(matches_session)
    {
        return Ok(());
    }
    let mut record = record.clone();
    if let Some(previous) = segments
        .iter()
        .filter(|(segment_year, _)| *segment_year == year - 1)
        .flat_map(|(_, segment)| segment.records.iter())
        .filter(|candidate| matches_session(candidate))
        .min_by_key(|record| record.at)
    {
        record.role = previous.role.clone();
    }
    let mut bytes = Vec::new();
    if read_segment(&path).unfinished_tail {
        bytes.push(b'\n');
    }
    serde_json::to_writer(&mut bytes, &record)?;
    bytes.push(b'\n');
    super::history::append(&paths.spending_history, &path, &bytes)
}

pub(super) fn load(paths: &SpendingHistoryPaths, now_secs: u64) -> RoleIndex {
    let Some(at) = i64::try_from(now_secs)
        .ok()
        .and_then(|second| Timestamp::from_second(second).ok())
    else {
        return RoleIndex::default();
    };
    let year = at.to_zoned(jiff::tz::TimeZone::UTC).year();
    let segments = read_segments(paths, year);
    let mut records = BTreeMap::<_, &SessionRoleRecord>::new();
    for record in segments.iter().flat_map(|(_, segment)| &segment.records) {
        let earliest = records
            .entry((record.kind.clone(), record.session.clone()))
            .or_insert(record);
        if record.at < earliest.at {
            *earliest = record;
        }
    }
    let mut roles = RoleMap::new();
    let mut recorded_since_secs = None;
    for ((kind, session), record) in records {
        roles
            .entry(kind)
            .or_default()
            .insert(session, record.role.clone());
        recorded_since_secs = recorded_since_secs
            .into_iter()
            .chain(u64::try_from(record.at.as_second()).ok())
            .min();
    }
    RoleIndex {
        roles,
        recorded_since_secs,
        ..Default::default()
    }
}

pub(super) fn load_history(paths: &SpendingHistoryPaths, now_secs: u64) -> RoleIndex {
    RoleIndex {
        effort: super::session_effort::load(paths, now_secs),
        ..load(paths, now_secs)
    }
}

fn read_segments(paths: &SpendingHistoryPaths, year: i16) -> Vec<(i16, Arc<Segment>)> {
    let files: BTreeMap<_, _> = super::history::segments(&paths.roles_dir())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| {
            let segment_year = super::history::segment_year(&path)?;
            (segment_year == year || segment_year == year - 1).then_some((path, segment_year))
        })
        .collect();
    SEGMENTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|path, _| files.contains_key(path));
    files
        .into_iter()
        .map(|(path, year)| (year, read_segment(&path)))
        .collect()
}

fn read_segment(path: &Path) -> Arc<Segment> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Arc::default();
    };
    let stamped = StampedPath {
        path: path.to_path_buf(),
        stamp: FileStamp::from_metadata(&meta),
    };
    let mut memos = SEGMENTS.lock().unwrap_or_else(PoisonError::into_inner);
    let memo = memos
        .entry(path.to_path_buf())
        .or_insert_with(ParseCache::new);
    if let Some(segment) = memo.get_stamped(&stamped) {
        return segment;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Arc::default();
    };
    let segment = Arc::new(Segment {
        records: bytes
            .split(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect(),
        unfinished_tail: bytes.last().is_some_and(|byte| *byte != b'\n'),
    });
    memo.store_stamped(&stamped, Arc::clone(&segment));
    segment
}

pub(super) fn prune_segments(
    hold: &super::history::Maintenance,
    now_secs: u64,
) -> Result<(), AtomicErr> {
    let Some(at) = i64::try_from(now_secs)
        .ok()
        .and_then(|second| Timestamp::from_second(second).ok())
    else {
        return Ok(());
    };
    let year = at.to_zoned(jiff::tz::TimeZone::UTC).year();
    let directory = hold.paths.roles_dir();
    let mut removed = false;
    for path in super::history::segments(&directory)? {
        if !super::history::segment_year(&path).is_some_and(|old| old < year - 1) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(AtomicErr::Io { path, source }),
        }
    }
    if removed {
        atomic::sync_dir(&directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_uses_recorded_seats_before_legacy_profile_splitting() {
        use Placement::{Solo, Subagent, Team};
        use serde_json::json;

        for (row, expected) in [
            (
                json!({"role":"agent", "profile":"forge.coder"}),
                Team {
                    team: "forge",
                    role: "coder",
                },
            ),
            (
                json!({"role":"agent", "profile":"forge.sub.coder"}),
                Team {
                    team: "forge",
                    role: "sub.coder",
                },
            ),
            (
                json!({"role":"agent", "profile":"forge.coder", "seat":{"team":"forge", "role":"coder"}}),
                Team {
                    team: "forge",
                    role: "coder",
                },
            ),
            (
                json!({"role":"agent", "profile":"plain-profile", "seat":{"team":"forge", "role":"coder"}}),
                Team {
                    team: "forge",
                    role: "coder",
                },
            ),
            (
                json!({"role":"agent", "profile":"plain-profile"}),
                Solo {
                    profile: "plain-profile",
                },
            ),
            (
                json!({"role":"agent", "profile":"solo.dotted", "seat":"solo"}),
                Solo {
                    profile: "solo.dotted",
                },
            ),
            (
                json!({"role":"agent", "profile":"solo.dotted"}),
                Team {
                    team: "solo",
                    role: "dotted",
                },
            ),
            (json!({"role":"agent"}), Solo { profile: "claude" }),
            (
                json!({"role":"agent", "seat":"solo"}),
                Solo { profile: "claude" },
            ),
            (
                json!({"role":"agent", "profile":".coder"}),
                Solo { profile: ".coder" },
            ),
            (
                json!({"role":"agent", "profile":"forge."}),
                Solo { profile: "forge." },
            ),
            (
                json!({"role":"subagent", "profile":"forge.coder"}),
                Subagent {
                    profile: "forge.coder",
                },
            ),
        ] {
            let role: SessionRole = serde_json::from_value(row.clone()).unwrap();
            assert_eq!(role.placement("claude"), expected, "{row}");
        }
    }

    #[test]
    fn recorded_seats_round_trip_and_old_rows_remain_unrecorded() {
        for row in [
            serde_json::json!({"role":"agent", "profile":"solo.dotted", "seat":"solo"}),
            serde_json::json!({"role":"agent", "profile":"plain-profile", "seat":{"team":"forge", "role":"coder"}}),
            serde_json::json!({"role":"agent", "profile":"forge.coder"}),
            serde_json::json!({"role":"subagent", "profile":"fast"}),
            serde_json::json!({
                "role":"subagent", "profile":"fast",
                "owner":{"team":"forge", "role":"coder"},
            }),
            serde_json::json!({
                "role":"subagent", "profile":"fast", "owner":{"profile":"solo.dotted"},
            }),
        ] {
            let role: SessionRole = serde_json::from_value(row.clone()).unwrap();
            assert_eq!(serde_json::to_value(role).unwrap(), row);
        }
        let role: SessionRole = serde_json::from_value(serde_json::json!({
            "role":"subagent", "profile":"fast", "future":true,
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(role).unwrap(),
            serde_json::json!({
                "role":"subagent", "profile":"fast",
            })
        );
    }

    #[test]
    fn owner_resolves_the_parent_seat_through_launch_identities() {
        use crate::agents::{AgentState, AgentStatus};
        use serde_json::json;

        let at = "2026-06-01T00:00:00Z".parse().unwrap();
        let parent = AgentState::seed(
            AgentKind::new_unchecked("codex"),
            "parent-session".into(),
            AgentStatus::Running,
            at,
        );
        let mut child = AgentState::seed(
            AgentKind::new_unchecked("claude"),
            "child-session".into(),
            AgentStatus::Running,
            at,
        );
        child.launch_id = Some("child-launch".into());
        child.parent_agent_id = Some("parent-launch".into());
        child.parent_agent_kind = Some(parent.kind.clone());
        let mut actual = Vec::new();
        for (team, role, profile, expected) in [
            (
                Some("forge"),
                Some("coder"),
                Some("plain"),
                json!({"team":"forge", "role":"coder"}),
            ),
            (
                Some("forge"),
                None,
                Some("plain"),
                json!({"team":"forge", "role":"plain"}),
            ),
            (
                Some("forge"),
                None,
                None,
                json!({"team":"forge", "role":"codex"}),
            ),
            (
                None,
                None,
                Some("solo.dotted"),
                json!({"profile":"solo.dotted"}),
            ),
            (None, None, None, json!({"profile":"codex"})),
        ] {
            let mut parent = parent.clone();
            parent.prior_launch_ids.push("parent-launch".into());
            parent.team = team.map(Into::into);
            parent.role = role.map(Into::into);
            parent.profile = profile.map(Into::into);
            actual.push((
                serde_json::to_value(resolve_owner(
                    &[child.clone(), parent],
                    &"child-launch".into(),
                ))
                .unwrap(),
                expected,
            ));
        }
        for rows in [
            vec![parent.clone()],
            vec![child.clone()],
            vec![
                AgentState {
                    parent_agent_id: None,
                    ..child
                },
                parent,
            ],
        ] {
            actual.push((
                serde_json::to_value(resolve_owner(&rows, &"child-launch".into())).unwrap(),
                json!(null),
            ));
        }
        assert_eq!(
            actual.iter().map(|(actual, _)| actual).collect::<Vec<_>>(),
            actual
                .iter()
                .map(|(_, expected)| expected)
                .collect::<Vec<_>>()
        );
    }

    fn load(paths: &RuntimePaths, at: Timestamp) -> RoleMap {
        super::load(&paths.spending_history, at.as_second() as u64).roles
    }

    fn fixture() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let paths = RuntimePaths::under(id, dir.path()).unwrap();
        (dir, paths)
    }

    fn row(year: i16, role: SessionRole) -> SessionRoleRecord {
        SessionRoleRecord {
            kind: AgentKind::new_unchecked("codex"),
            session: "session-a".into(),
            role,
            at: format!("{year}-06-01T00:00:00Z").parse().unwrap(),
        }
    }

    fn agent() -> SessionRole {
        SessionRole::Agent {
            profile: None,
            seat: None,
        }
    }

    fn child() -> SessionRole {
        SessionRole::Subagent {
            profile: "fast".into(),
            owner: None,
        }
    }

    fn append_fixture(paths: &RuntimePaths, record: &SessionRoleRecord) {
        let path = paths.shared_session_roles(record.at.to_zoned(jiff::tz::TimeZone::UTC).year());
        let mut bytes = serde_json::to_vec(record).unwrap();
        bytes.push(b'\n');
        crate::disk::atomic::append_record_bytes(&path, &bytes).unwrap();
    }

    #[test]
    fn record_if_absent_preserves_role_and_profile() {
        let (_dir, paths) = fixture();
        let first = row(2026, child());
        record(&paths, &first).unwrap();
        assert_eq!(
            load(&paths, first.at)
                .get(&first.kind)
                .and_then(|rows| rows.get(&first.session)),
            Some(&child())
        );
        let path = paths.shared_session_roles(2026);
        let before = std::fs::read(&path).unwrap();
        record(&paths, &row(2026, agent())).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn previous_segment_wins_and_torn_tail_is_skipped() {
        let (_dir, paths) = fixture();
        let first = row(2025, child());
        append_fixture(&paths, &first);
        append_fixture(&paths, &row(2026, agent()));
        crate::disk::atomic::append_record_bytes(&paths.shared_session_roles(2026), b"{\"kind\":")
            .unwrap();
        let now = row(2026, agent()).at;
        let loaded = super::load(&paths.spending_history, now.as_second() as u64);
        assert_eq!(
            loaded.recorded_since_secs,
            Some(first.at.as_second() as u64)
        );
        assert_eq!(
            load(&paths, now)
                .get(&first.kind)
                .and_then(|rows| rows.get(&first.session)),
            Some(&child())
        );
        let next = SessionRoleRecord {
            session: "after-torn-tail".into(),
            ..row(2026, agent())
        };
        record(&paths, &next).unwrap();
        assert_eq!(load(&paths, now)[&next.kind][&next.session], agent());
    }

    #[test]
    fn new_year_carries_forward_without_pruning_older_segments() {
        let (_dir, paths) = fixture();
        append_fixture(&paths, &row(2024, agent()));
        append_fixture(&paths, &row(2025, child()));
        let current = row(2026, agent());
        record(&paths, &current).unwrap();
        assert!(paths.shared_session_roles(2024).exists());
        assert!(paths.shared_session_roles(2025).exists());
        let saved: SessionRoleRecord =
            serde_json::from_slice(&std::fs::read(paths.shared_session_roles(2026)).unwrap())
                .unwrap();
        assert_eq!(saved.role, child());
        assert_eq!(saved.at, current.at);
        assert_eq!(
            load(&paths, row(2027, agent()).at)[&current.kind][&current.session],
            child()
        );
    }

    #[test]
    fn dormant_session_is_recorded_fresh_after_provenance_expires() {
        let (_dir, paths) = fixture();
        append_fixture(&paths, &row(2024, child()));
        let current = row(2026, agent());
        record(&paths, &current).unwrap();
        assert_eq!(
            load(&paths, current.at)
                .get(&current.kind)
                .and_then(|rows| rows.get(&current.session)),
            Some(&agent())
        );
    }
}
