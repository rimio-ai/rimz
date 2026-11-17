//! Latest-wins per-session check-in from a provider mod RimZ installs.
//!
//! RimZ's Claude mod reports through `rimz mod feed`, and every report upserts
//! one record per `(kind, session id)` under the runtime `mod_checkin/` dir: the
//! mod's version, the provider's, and when it last reported. So a session whose
//! start report was missed still checks in at its next one. `rimz doctor` reads
//! the records to flag a live session the mod never reached. Like the context
//! sidecars this is display-only latency, never truth: the mod reports, the
//! settings hooks stay the lifecycle record ("Durability first",
//! `docs/internals/store.md`).

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::atomic;
use crate::disk::paths::RuntimePaths;
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::sidecar;

/// One session's check-in, filed under the `(kind, session id)` it names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModCheckinRecord {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub mod_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_version: Option<String>,
    pub reported_at: Timestamp,
}

impl sidecar::SidecarRecord for ModCheckinRecord {
    const FILE_PREFIX: &'static str = "mod";

    fn kind(&self) -> &str {
        self.kind.as_str()
    }

    fn agent_id(&self) -> &str {
        self.agent_id.as_str()
    }
}

/// Upsert one session's check-in, stamped now.
pub fn record(
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
    mod_version: &str,
    provider_version: Option<&str>,
) -> Result<(), atomic::AtomicErr> {
    let fresh = || ModCheckinRecord {
        kind: AgentKind::new_unchecked(kind),
        agent_id: session_id.into(),
        mod_version: mod_version.to_owned(),
        provider_version: provider_version.map(str::to_owned),
        reported_at: Timestamp::now(),
    };
    sidecar::update(
        runtime,
        &runtime.mod_checkin_dir,
        kind,
        session_id,
        fresh,
        |record, _| {
            *record = fresh();
            true
        },
    )
    .map(|_| ())
}

/// Every session's check-in. An unreadable or malformed file is skipped.
pub fn read_all(runtime: &RuntimePaths) -> Vec<ModCheckinRecord> {
    sidecar::read_all(&runtime.mod_checkin_dir, &sidecar::ParseCache::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    #[test]
    fn each_report_upserts_the_sessions_one_record() {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(std::path::Path::new("/tmp/checkin-test"));
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        assert!(read_all(&runtime).is_empty());

        record(&runtime, "claude", "sess-1", "1.0.0", None).unwrap();
        record(&runtime, "claude", "sess-1", "1.0.1", Some("2.1.326")).unwrap();
        record(&runtime, "claude", "sess-2", "1.0.0", None).unwrap();

        let mut all = read_all(&runtime);
        all.sort_by(|left, right| left.agent_id.as_str().cmp(right.agent_id.as_str()));
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].agent_id, "sess-1");
        assert_eq!(all[0].mod_version, "1.0.1");
        assert_eq!(all[0].provider_version.as_deref(), Some("2.1.326"));
        assert_eq!(all[1].agent_id, "sess-2");
        assert_eq!(all[1].provider_version, None);
    }
}
