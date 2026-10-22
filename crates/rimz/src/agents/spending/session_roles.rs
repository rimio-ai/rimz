//! Durable, home-wide session provenance for spending attribution.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::atomic::{self, AtomicErr};
use crate::disk::parse_cache::ParseCache;
use crate::disk::paths::RuntimePaths;
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
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Placement<'a> {
    Solo { profile: &'a str },
    Team { team: &'a str, role: &'a str },
    Subagent { profile: &'a str },
}

impl SessionRole {
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
            Self::Subagent { profile } => Placement::Subagent { profile },
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
    roles: RoleMap,
    recorded_since_secs: Option<u64>,
    unfinished_tail: bool,
}

thread_local! {
    // One slot for each retained year; a different root or year replaces the slot.
    static SEGMENTS: [ParseCache<Segment>; 2] = const { [ParseCache::new(), ParseCache::new()] };
}

#[must_use = "durability barrier; check the result"]
pub fn record(paths: &RuntimePaths, record: &SessionRoleRecord) -> Result<(), AtomicErr> {
    let year = record.at.to_zoned(jiff::tz::TimeZone::UTC).year();
    let path = paths.shared_session_roles(year);
    let current = read_segment(&path, 1);
    if current
        .roles
        .get(&record.kind)
        .is_some_and(|rows| rows.contains_key(&record.session))
    {
        return Ok(());
    }
    let previous = read_segment(&paths.shared_session_roles(year - 1), 0);
    let mut record = record.clone();
    if let Some(role) = previous
        .roles
        .get(&record.kind)
        .and_then(|rows| rows.get(&record.session))
    {
        record.role = role.clone();
    }
    let created = !path.exists();
    // The append syncs the segment's own entry; the directory's entry needs its parent synced.
    if let Some(directory) = path.parent()
        && !directory.exists()
    {
        std::fs::create_dir_all(directory).map_err(|source| AtomicErr::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        if let Some(providers) = directory.parent() {
            atomic::sync_dir(providers)?;
        }
    }
    let mut bytes = Vec::new();
    if current.unfinished_tail {
        bytes.push(b'\n');
    }
    serde_json::to_writer(&mut bytes, &record)?;
    bytes.push(b'\n');
    atomic::append_record_bytes(&path, &bytes)?;
    atomic::sync_file_data(&path)?;
    if created {
        prune_segments(&path, year)?;
    }
    Ok(())
}

pub(super) fn load(cursor: &Path, now_secs: u64) -> RoleIndex {
    let Some(at) = i64::try_from(now_secs)
        .ok()
        .and_then(|second| Timestamp::from_second(second).ok())
    else {
        return RoleIndex::default();
    };
    let year = at.to_zoned(jiff::tz::TimeZone::UTC).year();
    let mut roles = RoleMap::new();
    let mut recorded_since_secs = None;
    for (slot, year) in [year - 1, year].into_iter().enumerate() {
        let segment = read_segment(&RuntimePaths::session_roles_for_cursor(cursor, year), slot);
        recorded_since_secs = recorded_since_secs
            .into_iter()
            .chain(segment.recorded_since_secs)
            .min();
        for (kind, sessions) in &segment.roles {
            let entries = roles.entry(kind.clone()).or_default();
            for (session, role) in sessions {
                entries
                    .entry(session.clone())
                    .or_insert_with(|| role.clone());
            }
        }
    }
    RoleIndex {
        roles,
        recorded_since_secs,
        ..Default::default()
    }
}

pub(super) fn load_history(cursor: &Path, now_secs: u64) -> RoleIndex {
    RoleIndex {
        effort: super::session_effort::load(cursor, now_secs),
        ..load(cursor, now_secs)
    }
}

fn read_segment(path: &Path, slot: usize) -> Arc<Segment> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Arc::default();
    };
    let Ok(modified) = meta.modified() else {
        return Arc::default();
    };
    SEGMENTS.with(|slots| {
        let memo = &slots[slot];
        if let Some(value) = memo.get(path, modified, meta.len()) {
            return value;
        }
        let Ok(bytes) = std::fs::read(path) else {
            return Arc::default();
        };
        let mut roles = RoleMap::new();
        let mut recorded_since_secs = None;
        for line in bytes.split(|byte| *byte == b'\n') {
            let Ok(record) = serde_json::from_slice::<SessionRoleRecord>(line) else {
                continue;
            };
            recorded_since_secs = recorded_since_secs
                .into_iter()
                .chain(u64::try_from(record.at.as_second()).ok())
                .min();
            roles
                .entry(record.kind)
                .or_default()
                .entry(record.session)
                .or_insert(record.role);
        }
        let segment = Arc::new(Segment {
            roles,
            recorded_since_secs,
            unfinished_tail: bytes.last().is_some_and(|byte| *byte != b'\n'),
        });
        memo.store(path, modified, meta.len(), Arc::clone(&segment));
        segment
    })
}

fn prune_segments(path: &Path, year: i16) -> Result<(), AtomicErr> {
    let Some(directory) = path.parent() else {
        return Ok(());
    };
    let io = |source| AtomicErr::Io {
        path: directory.to_path_buf(),
        source,
    };
    for entry in std::fs::read_dir(directory).map_err(io)? {
        let entry = entry.map_err(io)?;
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "jsonl")
            || !entry.file_type().map_err(io)?.is_file()
            || !path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<i16>().ok())
                .is_some_and(|old| old < year - 1)
        {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(AtomicErr::Io { path, source }),
        }
    }
    atomic::sync_dir(directory)
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
        ] {
            let role: SessionRole = serde_json::from_value(row.clone()).unwrap();
            assert_eq!(serde_json::to_value(role).unwrap(), row);
        }
    }

    fn load(paths: &RuntimePaths, at: Timestamp) -> RoleMap {
        super::load(&paths.shared_spending_cursor_path(), at.as_second() as u64).roles
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
        let loaded = super::load(&paths.shared_spending_cursor_path(), now.as_second() as u64);
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
    fn new_year_carries_forward_and_prunes_only_older_segments() {
        let (_dir, paths) = fixture();
        append_fixture(&paths, &row(2024, agent()));
        append_fixture(&paths, &row(2025, child()));
        let current = row(2026, agent());
        record(&paths, &current).unwrap();
        assert!(!paths.shared_session_roles(2024).exists());
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
