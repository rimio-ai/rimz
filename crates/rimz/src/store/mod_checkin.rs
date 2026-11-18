//! Latest-wins per-session check-in from a provider mod RimZ installs.
//!
//! RimZ's Claude mod reports through `rimz mod feed`, and every report upserts
//! one record per `(kind, session id)` under the runtime `mod_checkin/` dir: the
//! mod's version, the provider's, when it first and last reported, and when it
//! last sent each signal it owes (readings, a turn report). So a session whose
//! start report was missed still checks in at its next one. `rimz doctor` reads
//! the records to flag a live session the mod never reached, and one whose mod
//! never sent a signal across a turn it finished. Like the context
//! sidecars this is display-only latency, never truth: the mod reports, the
//! settings hooks stay the lifecycle record ("Durability first",
//! `docs/internals/store.md`).

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::context::ModReport;
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
    /// The session's first report; absent in a record written before it was
    /// kept, where [`ModCheckinRecord::checked_in_at`] reads `reported_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_reported_at: Option<Timestamp>,
    /// The last report whose usage carried a context window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readings_at: Option<Timestamp>,
    /// The last tool-call or turn-end report whose transcript the scan read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_report_at: Option<Timestamp>,
}

impl ModCheckinRecord {
    /// When the session first checked in.
    pub fn checked_in_at(&self) -> Timestamp {
        self.first_reported_at.unwrap_or(self.reported_at)
    }
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

/// Upsert one session's check-in from `report`, stamped now. The first report
/// time and a signal's stamp survive a report that does not carry it.
pub fn record(
    runtime: &RuntimePaths,
    kind: &str,
    report: &ModReport,
) -> Result<(), atomic::AtomicErr> {
    let now = Timestamp::now();
    let fresh = || ModCheckinRecord {
        kind: AgentKind::new_unchecked(kind),
        agent_id: report.session_id.as_str().into(),
        mod_version: report.mod_version.clone(),
        provider_version: report.provider_version.clone(),
        reported_at: now,
        first_reported_at: Some(now),
        readings_at: None,
        turn_report_at: None,
    };
    sidecar::update(
        runtime,
        &runtime.mod_checkin_dir,
        kind,
        &report.session_id,
        fresh,
        |record, _| {
            *record = ModCheckinRecord {
                first_reported_at: Some(record.checked_in_at()),
                readings_at: report.readings.then_some(now).or(record.readings_at),
                turn_report_at: report.turn_report.then_some(now).or(record.turn_report_at),
                ..fresh()
            };
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

    fn report(session: &str, version: &str, provider: Option<&str>) -> ModReport {
        ModReport {
            session_id: session.into(),
            mod_version: version.into(),
            provider_version: provider.map(str::to_owned),
            subagents: Vec::new(),
            context: crate::agents::AgentContext::new("claude", Timestamp::now()),
            readings: false,
            turn_report: false,
        }
    }

    #[test]
    fn each_report_upserts_the_sessions_one_record() {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(std::path::Path::new("/tmp/checkin-test"));
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        assert!(read_all(&runtime).is_empty());

        record(&runtime, "claude", &report("sess-1", "1.0.0", None)).unwrap();
        let first = read_all(&runtime).remove(0);
        let readings = ModReport {
            readings: true,
            ..report("sess-1", "1.1.0", Some("2.1.326"))
        };
        record(&runtime, "claude", &readings).unwrap();
        let turn = ModReport {
            turn_report: true,
            ..report("sess-1", "1.1.0", Some("2.1.326"))
        };
        record(&runtime, "claude", &turn).unwrap();
        record(&runtime, "claude", &report("sess-2", "1.0.0", None)).unwrap();

        let mut all = read_all(&runtime);
        all.sort_by(|left, right| left.agent_id.as_str().cmp(right.agent_id.as_str()));
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].agent_id, "sess-1");
        assert_eq!(all[0].mod_version, "1.1.0");
        assert_eq!(all[0].provider_version.as_deref(), Some("2.1.326"));
        assert_eq!(
            all[0].checked_in_at(),
            first.reported_at,
            "the first report stays"
        );
        let readings_at = all[0]
            .readings_at
            .expect("a later report keeps the readings stamp");
        let turn_report_at = all[0].turn_report_at.unwrap();
        assert!(first.reported_at <= readings_at && readings_at <= turn_report_at);
        assert_eq!(all[0].reported_at, turn_report_at);
        assert_eq!(all[1].agent_id, "sess-2");
        assert_eq!(all[1].provider_version, None);
        assert_eq!((all[1].readings_at, all[1].turn_report_at), (None, None));
    }

    #[test]
    fn a_record_written_before_the_signal_stamps_still_reads() {
        let record: ModCheckinRecord = serde_json::from_str(
            r#"{"kind":"claude","agent_id":"s","mod_version":"1.0.0","reported_at":"2026-11-17T01:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(record.checked_in_at(), record.reported_at);
        assert_eq!((record.readings_at, record.turn_report_at), (None, None));
    }
}
