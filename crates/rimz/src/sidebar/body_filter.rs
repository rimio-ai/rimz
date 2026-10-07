//! Shared room-runtime cockpit lens applied to sidebar body membership.

use std::fs;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::AgentStatus;
use crate::disk::{atomic, paths::RuntimePaths};
use crate::store::snapshot::{SidebarRow, SidebarWorktreeGroup, WorktreePrState};

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

impl BodyFilter {
    pub(crate) fn matches(self, row: &SidebarRow, pr_open: bool) -> bool {
        match self {
            Self::Status(status) => row.status() == Some(status),
            Self::Unread => row.unread,
            Self::OpenPr => pr_open,
        }
    }

    pub(crate) fn total(self, groups: &[SidebarWorktreeGroup]) -> usize {
        match self {
            Self::Status(status) => groups
                .iter()
                .flat_map(|group| &group.status_counts)
                .filter(|count| count.status == status)
                .map(|count| count.count)
                .sum(),
            Self::Unread => groups
                .iter()
                .flat_map(|group| &group.rows)
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
