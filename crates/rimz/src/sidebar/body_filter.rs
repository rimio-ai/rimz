//! Shared room-runtime cockpit lens applied to sidebar body membership.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::AgentStatus;
use crate::disk::{atomic, paths::RuntimePaths};
use crate::store::snapshot::{SidebarRow, SidebarUnitKey, SidebarWorktreeGroup, WorktreePrState};

#[cfg(test)]
thread_local! {
    static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_reads() -> usize {
    READS.with(|reads| reads.replace(0))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", content = "status", rename_all = "snake_case")]
pub(crate) enum BodyFilter {
    Status(AgentStatus),
    Unread,
    OpenPr,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BodyLens {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) filter: Option<BodyFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) query: Option<String>,
}

impl BodyLens {
    pub(crate) fn is_empty(&self) -> bool {
        self.filter.is_none() && self.query.is_none()
    }
}

impl From<BodyFilter> for BodyLens {
    fn from(filter: BodyFilter) -> Self {
        Self {
            filter: Some(filter),
            query: None,
        }
    }
}

pub(crate) struct GroupFacts<'a> {
    group: &'a SidebarWorktreeGroup,
    buckets: BTreeMap<SidebarUnitKey<'a>, Vec<AgentStatus>>,
}

impl<'a> GroupFacts<'a> {
    pub(crate) fn new(group: &'a SidebarWorktreeGroup) -> Self {
        let mut buckets: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for row in group.rows.iter() {
            let Some(status) = row.attention_status() else {
                continue;
            };
            let statuses = buckets.entry(group.unit_key(row)).or_default();
            if !statuses.contains(&status) {
                statuses.push(status);
            }
        }
        let is_active = |status: &AgentStatus| {
            matches!(
                status,
                AgentStatus::Waiting
                    | AgentStatus::Failed
                    | AgentStatus::Paused
                    | AgentStatus::Running
            )
        };
        for statuses in buckets.values_mut() {
            if statuses.iter().any(is_active) {
                statuses.retain(is_active);
                continue;
            }
            let rest = [
                AgentStatus::Idle,
                AgentStatus::Sleeping,
                AgentStatus::Success,
            ]
            .into_iter()
            .find(|status| statuses.contains(status));
            statuses.retain(|status| {
                Some(*status) == rest && (*status != AgentStatus::Success || !group.finished)
            });
        }
        Self { group, buckets }
    }
}

impl BodyFilter {
    /// Status lenses show a matching unit whole; other lenses keep row membership.
    pub(crate) fn shown(self, row: &SidebarRow, facts: &GroupFacts<'_>) -> bool {
        match self {
            Self::Status(status) => facts
                .buckets
                .get(&facts.group.unit_key(row))
                .is_some_and(|buckets| buckets.contains(&status)),
            Self::Unread => row.unread,
            Self::OpenPr => facts.group.pr_state == Some(WorktreePrState::Open),
        }
    }

    /// A status match has the bucket's own attention status and belongs to a unit counted there.
    pub(crate) fn matches(self, row: &SidebarRow, facts: &GroupFacts<'_>) -> bool {
        self.shown(row, facts)
            && match self {
                Self::Status(status) => row.attention_status() == Some(status),
                Self::Unread | Self::OpenPr => true,
            }
    }

    /// Full-roster matching units for status lenses, or matching rows for unread and open PRs.
    pub(crate) fn total(self, groups: &[SidebarWorktreeGroup]) -> usize {
        match self {
            Self::Status(_) => groups
                .iter()
                .map(|group| {
                    let facts = GroupFacts::new(group);
                    group
                        .rows
                        .iter()
                        .filter(|row| self.matches(row, &facts))
                        .map(|row| group.unit_key(row))
                        .collect::<BTreeSet<_>>()
                        .len()
                })
                .sum(),
            Self::Unread => groups
                .iter()
                .flat_map(|group| group.rows.iter())
                .filter(|row| row.unread)
                .count(),
            Self::OpenPr => groups
                .iter()
                .filter(|group| group.pr_state == Some(WorktreePrState::Open))
                .map(|group| group.rows.len())
                .sum(),
        }
    }
}

pub(crate) fn load(runtime: &RuntimePaths) -> BodyLens {
    #[cfg(test)]
    READS.with(|reads| reads.set(reads.get() + 1));
    let path = runtime.sidebar_filter_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return BodyLens::default(),
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar body filter unreadable");
            return BodyLens::default();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(lens) => lens,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar body filter invalid");
            BodyLens::default()
        }
    }
}

pub(crate) fn write(runtime: &RuntimePaths, lens: &BodyLens) -> atomic::Result<()> {
    if !lens.is_empty() {
        return atomic::write_temp_then_rename_cache(&runtime.sidebar_filter_path(), lens);
    }
    let path = runtime.sidebar_filter_path();
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(atomic::AtomicErr::Io { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    #[test]
    fn team_units_count_active_buckets_or_one_rest_bucket() {
        use AgentStatus::*;
        for (members, expected) in [
            (vec![Running, Idle], vec![Running]),
            (vec![Waiting, Running], vec![Waiting, Running]),
            (vec![Failed, Paused, Running], vec![Failed, Paused, Running]),
            (vec![Idle, Sleeping, Success], vec![Idle]),
            (vec![Sleeping, Success], vec![Sleeping]),
            (vec![Success, Success], vec![Success]),
        ] {
            let group = team_group(&members);
            for status in [Waiting, Failed, Paused, Running, Idle, Sleeping, Success] {
                assert_eq!(
                    BodyFilter::Status(status).total(std::slice::from_ref(&group)),
                    usize::from(expected.contains(&status)),
                    "{members:?} in {status:?}"
                );
            }
        }
    }

    fn team_group(statuses: &[AgentStatus]) -> SidebarWorktreeGroup {
        use crate::sidebar::test_support::{activity_row, worktree_group};
        let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();
        let path = std::path::Path::new("/repo/lane");
        let rows = statuses
            .iter()
            .enumerate()
            .map(|(index, status)| {
                let mut row = activity_row(true, Some(*status), now, path);
                row.id = format!("member-{index}");
                row.as_agent_mut().unwrap().team = Some("crew".to_owned());
                row
            })
            .collect();
        worktree_group(path, rows)
    }

    #[test]
    fn unit_identity_stays_within_a_group_and_external_rows_are_solo() {
        use AgentStatus::*;
        let group = team_group(&[Running, Running]);
        let mut other = group.clone();
        other.key = "/repo/other".to_owned();
        assert_eq!(
            BodyFilter::Status(Running).total(&[group.clone(), other]),
            2
        );
        let mut external = group.clone();
        external.kind = crate::store::snapshot::SidebarWorktreeKind::External;
        assert_eq!(BodyFilter::Status(Running).total(&[external]), 2);
        let mut rows = group.rows.as_ref().clone();
        let mut solo = rows[0].clone();
        solo.id = "solo".to_owned();
        solo.as_agent_mut().unwrap().team = None;
        rows.push(solo);
        let mixed =
            crate::sidebar::test_support::worktree_group(std::path::Path::new("/repo/lane"), rows);
        assert_eq!(BodyFilter::Status(Running).total(&[mixed]), 2);
    }

    #[test]
    fn success_count_and_lens_exclude_finished_groups_only() {
        let groups = [true, false].map(|finished| {
            let mut group = team_group(&[AgentStatus::Success, AgentStatus::Success]);
            group.finished = finished;
            group
        });
        let success = BodyFilter::Status(AgentStatus::Success);
        assert_eq!(success.total(&groups), 1);
        let idle = BodyFilter::Status(AgentStatus::Idle);
        assert_eq!(idle.total(&groups), 0);
        let mut idle_finished = team_group(&[AgentStatus::Idle, AgentStatus::Success]);
        idle_finished.finished = true;
        assert_eq!(idle.total(&[idle_finished]), 1);
    }

    #[test]
    fn bucket_totals_and_shown_units_use_one_predicate() {
        use AgentStatus::*;
        let mut finished = team_group(&[Success, Success]);
        finished.finished = true;
        let groups = [
            team_group(&[Running, Idle, Success]),
            team_group(&[Waiting, Failed, Paused]),
            team_group(&[Sleeping, Success]),
            finished,
        ];
        let working = BodyFilter::Status(Running);
        let facts = GroupFacts::new(&groups[0]);
        assert!(
            groups[0].rows.iter().all(|row| working.shown(row, &facts)),
            "a matching unit is shown whole"
        );
        for status in [Waiting, Failed, Paused, Running, Idle, Sleeping, Success] {
            let filter = BodyFilter::Status(status);
            let shown = groups.iter().any(|group| {
                let facts = GroupFacts::new(group);
                group.rows.iter().any(|row| filter.shown(row, &facts))
            });
            assert_eq!(filter.total(&groups) == 0, !shown, "{status:?}");
        }
    }

    fn runtime(dir: &std::path::Path) -> RuntimePaths {
        RuntimePaths::under(
            WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace id"),
            dir,
        )
        .expect("runtime paths")
    }

    #[test]
    fn round_trip_missing_and_invalid_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        assert_eq!(load(&runtime), BodyLens::default());

        let filter = BodyLens::from(BodyFilter::Status(AgentStatus::Waiting));
        write(&runtime, &filter).expect("write filter");
        assert_eq!(load(&runtime), filter);

        fs::write(runtime.sidebar_filter_path(), b"not json").expect("garbage file");
        assert_eq!(load(&runtime), BodyLens::default());
    }

    #[test]
    fn clear_removes_a_filter_and_accepts_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        write(&runtime, &BodyLens::from(BodyFilter::Unread)).expect("write filter");

        write(&runtime, &BodyLens::default()).expect("clear filter");
        assert!(!runtime.sidebar_filter_path().exists());
        assert_eq!(load(&runtime), BodyLens::default());
        write(&runtime, &BodyLens::default()).expect("clear missing filter");
    }

    #[test]
    fn lens_file_round_trips_both_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let lens = BodyLens {
            filter: Some(BodyFilter::Status(AgentStatus::Waiting)),
            query: Some("auth".to_owned()),
        };
        write(&runtime, &lens).expect("write lens");
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(runtime.sidebar_filter_path()).expect("lens file"))
                .expect("lens json");
        assert_eq!(
            json,
            serde_json::json!({
                "filter": {"kind": "status", "status": "waiting"},
                "query": "auth"
            })
        );
        assert_eq!(load(&runtime), lens);
    }

    #[test]
    fn query_only_lens_omits_the_pick_and_empty_lens_removes_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let lens = BodyLens {
            query: Some("auth".to_owned()),
            ..Default::default()
        };
        write(&runtime, &lens).expect("write query");
        assert_eq!(load(&runtime), lens);
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(runtime.sidebar_filter_path()).expect("lens file"))
                .expect("lens json");
        assert_eq!(json, serde_json::json!({"query": "auth"}));
        write(&runtime, &BodyLens::default()).expect("clear query");
        assert!(!runtime.sidebar_filter_path().exists());
    }

    #[test]
    fn old_single_filter_file_loads_as_an_empty_lens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        fs::create_dir_all(runtime.sidebar_filter_path().parent().unwrap()).unwrap();
        fs::write(runtime.sidebar_filter_path(), br#"{"kind":"unread"}"#).unwrap();
        assert_eq!(load(&runtime), BodyLens::default());
    }
}
