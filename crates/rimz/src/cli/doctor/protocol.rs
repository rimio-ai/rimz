use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rimz::ledger::event_log;
use rimz::schema::{EVENT_SCHEMA_VERSION, RESOLVER_PROTOCOL_VERSION, SIDEBAR_PROTOCOL_VERSION};
use rimz::{RuntimePaths, StatePaths};

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_protocol_versions(ws: &rimz::ResolvedWorkspace) {
    println!(
        "  protocols     : event {EVENT_SCHEMA_VERSION}; sidebar {SIDEBAR_PROTOCOL_VERSION}; resolver {RESOLVER_PROTOCOL_VERSION}",
    );
    report_event_schema_versions(ws);
    report_heartbeat_protocol_versions(ws);
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_event_schema_versions(ws: &rimz::ResolvedWorkspace) {
    let paths = match StatePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(paths) => paths,
        Err(err) => {
            println!("  protocol warn : event log unavailable ({err})");
            return;
        }
    };
    let events = match event_log::read_all(&paths.events_log) {
        Ok(events) => events,
        // Mid-file corruption — the post-power-cut corpse. Doctor stays
        // read-only; the truncating repair is gc's job.
        Err(err) if err.is_corruption() => {
            println!("  protocol warn : event log needs repair ({err}); run `rimz gc`");
            return;
        }
        Err(err) => {
            println!("  protocol warn : event log unavailable ({err})");
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
        println!(
            "  protocol warn : event log schema {version} seen {count} {noun} (expected {EVENT_SCHEMA_VERSION})",
        );
    }
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_heartbeat_protocol_versions(ws: &rimz::ResolvedWorkspace) {
    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            println!("  protocol warn : heartbeat dir unavailable ({err})");
            return;
        }
    };
    let entries = match fs::read_dir(&runtime.heartbeat_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            println!("  protocol warn : heartbeat dir unavailable ({err})");
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
            Ok(found) => println!(
                "  protocol warn : {kind} heartbeat {name} uses {found} (expected {expected})",
            ),
            Err(err) => {
                println!("  protocol warn : {kind} heartbeat {name} unreadable ({err})");
            }
        }
    }
}

fn heartbeat_kind_and_protocol(name: &str) -> Option<(&'static str, &'static str)> {
    if name.starts_with("sidebar.") && name.ends_with(".json") {
        Some(("sidebar", SIDEBAR_PROTOCOL_VERSION))
    } else if name.starts_with("resolver.") && name.ends_with(".json") {
        Some(("resolver", RESOLVER_PROTOCOL_VERSION))
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
