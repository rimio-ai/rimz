use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rimz::ledger::event::EVENT_SCHEMA_VERSION;
use rimz::ledger::event_log;
use rimz::sidebar::heartbeat::SIDEBAR_PROTOCOL_VERSION;
use rimz::{RuntimePaths, StatePaths};

use super::model::Protocols;

/// The protocol versions this build speaks, plus any drift found in the
/// workspace's event log and live heartbeats.
pub(super) fn collect_protocols(ws: &rimz::ResolvedWorkspace) -> Protocols {
    let mut warnings = Vec::new();
    collect_event_schema_warnings(ws, &mut warnings);
    collect_heartbeat_warnings(ws, &mut warnings);
    Protocols {
        event: EVENT_SCHEMA_VERSION,
        sidebar: SIDEBAR_PROTOCOL_VERSION,
        warnings,
    }
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
