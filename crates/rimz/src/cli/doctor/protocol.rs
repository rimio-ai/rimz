use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rimz::sidebar::heartbeat::SIDEBAR_PROTOCOL_VERSION;
use rimz::store::event::EVENT_SCHEMA_VERSION;
use rimz::store::event_log;
use rimz::{RuntimePaths, StatePaths};

use super::model::{self, Protocols};

/// The protocol versions this build speaks, plus any drift found in the
/// workspace's event log and live heartbeats.
pub(super) fn collect_protocols(ws: &rimz::ResolvedWorkspace) -> Protocols {
    let mut warnings = Vec::new();
    collect_event_schema_warnings(ws, &mut warnings);
    collect_heartbeat_warnings(ws, &mut warnings);
    let build_drift = collect_build_drift(ws);
    Protocols {
        event: EVENT_SCHEMA_VERSION,
        sidebar: SIDEBAR_PROTOCOL_VERSION,
        warnings,
        build_drift,
    }
}

fn collect_build_drift(ws: &rimz::ResolvedWorkspace) -> Option<model::BuildDrift> {
    let runtime = RuntimePaths::for_workspace(ws.workspace_id.clone()).ok()?;
    let heartbeats = super::runtime::fresh_sidebar_heartbeats_for_doctor(&runtime).ok()?;
    build_drift(&heartbeats, rimz::build_id::current())
}

fn collect_event_schema_warnings(ws: &rimz::ResolvedWorkspace, warnings: &mut Vec<String>) {
    let paths = match StatePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(paths) => paths,
        Err(err) => {
            warnings.push(format!("event log unavailable ({err})"));
            return;
        }
    };
    let events = match event_log::read_all(&paths.events_log) {
        Ok(events) => events,
        // Mid-file corruption — the post-power-cut corpse. Doctor stays
        // read-only; the truncating repair is gc's job.
        Err(err) if err.is_corruption() => {
            warnings.push(format!("event log needs repair ({err}); run `rimz gc`"));
            return;
        }
        Err(err) => {
            warnings.push(format!("event log unavailable ({err})"));
            return;
        }
    };
    let mut mismatches: BTreeMap<String, usize> = BTreeMap::new();
    for event in events {
        if event.schema_version != EVENT_SCHEMA_VERSION {
            *mismatches.entry(event.schema_version).or_default() += 1;
        }
    }
    for (version, count) in mismatches {
        let noun = if count == 1 { "record" } else { "records" };
        warnings.push(format!(
            "event log schema {version} seen {count} {noun} (expected {EVENT_SCHEMA_VERSION})",
        ));
    }
}

fn collect_heartbeat_warnings(ws: &rimz::ResolvedWorkspace, warnings: &mut Vec<String>) {
    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            warnings.push(format!("heartbeat dir unavailable ({err})"));
            return;
        }
    };
    let entries = match fs::read_dir(&runtime.heartbeat_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            warnings.push(format!("heartbeat dir unavailable ({err})"));
            return;
        }
    };
    let mut checks = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some((kind, expected)) = heartbeat_kind_and_protocol(name) else {
            continue;
        };
        checks.push((name.to_owned(), kind, expected, path));
    }
    checks.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, kind, expected, path) in checks {
        match heartbeat_protocol_version(&path) {
            Ok(found) if found == expected => {}
            Ok(found) => warnings.push(format!(
                "{kind} heartbeat {name} uses {found} (expected {expected})",
            )),
            Err(err) => warnings.push(format!("{kind} heartbeat {name} unreadable ({err})")),
        }
    }
}

fn heartbeat_kind_and_protocol(name: &str) -> Option<(&'static str, &'static str)> {
    if name.starts_with("sidebar.") && name.ends_with(".json") {
        Some(("sidebar", SIDEBAR_PROTOCOL_VERSION))
    } else {
        None
    }
}

fn heartbeat_protocol_version(path: &Path) -> std::result::Result<String, String> {
    let bytes = fs::read(path).map_err(|err| err.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|err| err.to_string())?;
    Ok(value
        .get("protocol_version")
        .and_then(|value| value.as_str())
        .unwrap_or("<missing>")
        .to_owned())
}

fn build_drift(
    heartbeats: &[rimz::sidebar::heartbeat::SidebarHeartbeat],
    own: Option<&str>,
) -> Option<model::BuildDrift> {
    let mut by_build: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    for heartbeat in heartbeats {
        let Some(build) = heartbeat.build.clone() else {
            continue;
        };
        let entry = by_build.entry(build).or_default();
        entry.0 += 1;
        if let Some(pane_id) = heartbeat.pane_id.as_ref() {
            entry.1.push(pane_id.to_string());
        }
    }
    if let Some(own) = own {
        by_build.entry(own.to_owned()).or_default();
    }
    if by_build.len() <= 1 {
        return None;
    }

    let writers = by_build
        .into_iter()
        .map(|(build, (sidebar_count, mut pane_ids))| {
            pane_ids.sort();
            pane_ids.dedup();
            model::BuildWriter {
                is_running: own == Some(build.as_str()),
                build,
                sidebar_count,
                pane_ids,
            }
        })
        .collect();
    Some(model::BuildDrift { writers })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rimz::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};
    use rimz::sidebar::heartbeat::SidebarHeartbeat;

    use super::*;

    fn heartbeat(build: Option<&str>, pane: Option<&str>) -> SidebarHeartbeat {
        let mut heartbeat = SidebarHeartbeat::new(
            WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace id"),
            SidebarInstanceId::new(),
            MuxName::Tmux,
            "rimz-test",
            PathBuf::from("/tmp/sidebar.sock"),
            pane.map(|pane| PaneId::from_parts(MuxName::Tmux, pane)),
        );
        heartbeat.build = build.map(ToOwned::to_owned);
        heartbeat
    }

    fn drift(heartbeats: &[SidebarHeartbeat], own: Option<&str>) -> Option<model::BuildDrift> {
        build_drift(heartbeats, own)
    }

    #[test]
    fn build_drift_absent_without_live_foreign_builds() {
        assert!(drift(&[], Some("aaa")).is_none());
        assert!(drift(&[heartbeat(Some("aaa"), Some("%1"))], Some("aaa")).is_none());
        assert!(drift(&[heartbeat(Some("aaa"), Some("%1"))], None).is_none());
        assert!(
            drift(
                &[
                    heartbeat(Some("aaa"), Some("%1")),
                    heartbeat(None, Some("%2")),
                ],
                Some("aaa")
            )
            .is_none()
        );
    }

    #[test]
    fn build_drift_includes_running_binary_with_no_live_sidebar() {
        let drift =
            drift(&[heartbeat(Some("bbb"), Some("%2"))], Some("aaa")).expect("foreign build drift");

        assert_eq!(drift.writers.len(), 2);
        assert_eq!(drift.writers[0].build, "aaa");
        assert!(drift.writers[0].is_running);
        assert_eq!(drift.writers[0].sidebar_count, 0);
        assert!(drift.writers[0].pane_ids.is_empty());
        assert_eq!(drift.writers[1].build, "bbb");
        assert!(!drift.writers[1].is_running);
        assert_eq!(drift.writers[1].sidebar_count, 1);
        assert_eq!(drift.writers[1].pane_ids, ["tmux:%2"]);
    }

    #[test]
    fn build_drift_groups_live_writers_by_sorted_build_id() {
        let drift = drift(
            &[
                heartbeat(Some("bbb"), Some("%3")),
                heartbeat(Some("aaa"), Some("%1")),
                heartbeat(Some("bbb"), Some("%2")),
            ],
            Some("bbb"),
        )
        .expect("mixed live builds");

        assert_eq!(drift.writers.len(), 2);
        assert_eq!(drift.writers[0].build, "aaa");
        assert!(!drift.writers[0].is_running);
        assert_eq!(drift.writers[0].sidebar_count, 1);
        assert_eq!(drift.writers[0].pane_ids, ["tmux:%1"]);
        assert_eq!(drift.writers[1].build, "bbb");
        assert!(drift.writers[1].is_running);
        assert_eq!(drift.writers[1].sidebar_count, 2);
        assert_eq!(drift.writers[1].pane_ids, ["tmux:%2", "tmux:%3"]);
    }

    #[test]
    fn build_drift_detects_mixed_live_builds_without_running_build_id() {
        let drift = drift(
            &[
                heartbeat(Some("bbb"), Some("%2")),
                heartbeat(Some("aaa"), Some("%1")),
            ],
            None,
        )
        .expect("mixed live builds");

        assert_eq!(
            drift
                .writers
                .iter()
                .map(|writer| (writer.build.as_str(), writer.is_running))
                .collect::<Vec<_>>(),
            [("aaa", false), ("bbb", false)]
        );
    }

    #[test]
    fn build_drift_ignores_heartbeats_without_build_id() {
        let drift = drift(
            &[
                heartbeat(None, Some("%1")),
                heartbeat(Some("bbb"), Some("%2")),
            ],
            Some("aaa"),
        )
        .expect("foreign build drift");

        assert_eq!(drift.writers.len(), 2);
        assert_eq!(drift.writers[0].build, "aaa");
        assert_eq!(drift.writers[0].sidebar_count, 0);
        assert_eq!(drift.writers[1].build, "bbb");
        assert_eq!(drift.writers[1].sidebar_count, 1);
        assert_eq!(drift.writers[1].pane_ids, ["tmux:%2"]);
    }
}
