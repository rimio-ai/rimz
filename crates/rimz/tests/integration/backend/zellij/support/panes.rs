use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rimz::ids::{MuxName, PaneId, ViewKind, WorkspaceId};
use rimz::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};
use rimz::mux::{
    ClientFocusOptions, MuxBackend, SidebarLiveness, SidebarPaneOptions, SidebarRecovery,
    ZellijBackend,
};
use rimz::pane::PaneRef;

use crate::common::CommandTimeoutExt;

use super::session::*;

pub(in crate::backend::zellij) fn serve_processes_for(session: &str) -> usize {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            std::fs::read(format!("/proc/{pid}/cmdline")).ok()
        })
        .filter(|cmdline| {
            let cmdline = String::from_utf8_lossy(cmdline).replace('\0', " ");
            cmdline.contains(session) && cmdline.contains("sidebar") && cmdline.contains("serve")
        })
        .count()
}

pub(in crate::backend::zellij) fn wait_for_attached_client(xdg: &Path, session: &str) {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let mut last_probes = Vec::new();
    let mut last_error = String::new();
    loop {
        match stable_attached_client_probes(xdg, session) {
            Ok(probes) => {
                let attached = stable_client_present(&probes);
                last_probes = probes;
                if attached {
                    return;
                }
            }
            Err(err) => last_error = err,
        }
        if Instant::now() > deadline {
            panic!(
                "no stable client attached to {session}; last probes: {last_probes:?}; \
                 last error: {last_error}",
            );
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

pub(in crate::backend::zellij) fn stable_attached_client_probes(
    xdg: &Path,
    session: &str,
) -> std::result::Result<Vec<BTreeSet<u32>>, String> {
    let mut probes = vec![focused_terminal_client_ids(xdg, session)?];
    if !stable_client_present(&probes) {
        return Ok(probes);
    }

    let deadline = Instant::now() + CLIENT_ATTACH_CONFIRM_WINDOW;
    while Instant::now() < deadline {
        std::thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(CLIENT_ATTACH_PROBE_STEP),
        );
        probes.push(focused_terminal_client_ids(xdg, session)?);
        if !stable_client_present(&probes) {
            return Ok(probes);
        }
    }
    Ok(probes)
}

pub(in crate::backend::zellij) fn focused_terminal_client_ids(
    xdg: &Path,
    session: &str,
) -> std::result::Result<BTreeSet<u32>, String> {
    let output = scoped_zellij(xdg)
        .args(["--session", session, "action", "list-clients"])
        .bounded_output()
        .map_err(|err| format!("list-clients failed to run: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "list-clients exited with {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(parse_focused_terminal_client_ids_for_test(&output.stdout))
}

pub(in crate::backend::zellij) fn parse_focused_terminal_client_ids_for_test(
    stdout: &[u8],
) -> BTreeSet<u32> {
    let mut clients = BTreeSet::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        let clean = strip_ansi_for_test(line);
        let mut cols = clean.split_whitespace();
        let Some(first) = cols.next() else {
            continue;
        };
        let Some(raw_pane) = cols.next() else {
            continue;
        };
        if first == "CLIENT_ID" || raw_pane == "ZELLIJ_PANE_ID" {
            continue;
        }
        if !raw_pane.starts_with("terminal_") {
            continue;
        }
        if let Ok(client) = first.parse::<u32>() {
            clients.insert(client);
        }
    }
    clients
}

pub(in crate::backend::zellij) fn strip_ansi_for_test(line: &str) -> String {
    let mut clean = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for code in chars.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
        } else {
            clean.push(ch);
        }
    }
    clean
}

pub(in crate::backend::zellij) fn stable_client_present(probes: &[BTreeSet<u32>]) -> bool {
    let Some((first, rest)) = probes.split_first() else {
        return false;
    };
    if first.is_empty() {
        return false;
    }
    let common = rest.iter().fold(first.clone(), |common, ids| {
        common.intersection(ids).copied().collect()
    });
    !common.is_empty()
}

/// The raw `list-panes` JSON object for the session's `rimz-sidebar` pane.
pub(in crate::backend::zellij) fn raw_sidebar_pane(xdg: &Path, session: &str) -> serde_json::Value {
    let panes = expect_list_panes_json(xdg, session);
    panes
        .as_array()
        .expect("pane array")
        .iter()
        .find(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .expect("rimz-sidebar pane")
        .clone()
}

pub(in crate::backend::zellij) fn assert_session_has_bottom_bar(xdg: &Path, session: &str) {
    let panes = expect_list_panes_json(xdg, session);
    let has_bar = panes.as_array().expect("pane array").iter().any(|pane| {
        pane.get("is_plugin").and_then(|v| v.as_bool()) == Some(true)
            && pane
                .get("title")
                .and_then(|v| v.as_str())
                .is_some_and(|title| title.contains("compact-bar"))
    });
    assert!(
        has_bar,
        "session {session} should carry a bottom bar plugin: {panes:?}"
    );
}

pub(in crate::backend::zellij) fn action_until(
    xdg: &Path,
    session: &str,
    args: &[String],
    label: &str,
    mut confirm: impl FnMut() -> std::result::Result<(), String>,
) {
    let mut last_observation = "post-condition was not checked".to_owned();
    for attempt in 0..ACTION_ATTEMPTS {
        if attempt > 0 && confirm().is_ok() {
            return;
        }
        let output = scoped_zellij(xdg)
            .args(["--session", session])
            .args(args.iter().map(String::as_str))
            .bounded_output()
            .unwrap_or_else(|err| panic!("{label} failed to run for {session}: {err}"));
        assert!(
            output.status.success(),
            "{label} failed for {session}: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        let deadline = Instant::now() + ACTION_CONFIRM_WINDOW;
        loop {
            match confirm() {
                Ok(()) => return,
                Err(observation) => last_observation = observation,
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(ACTION_CONFIRM_STEP);
        }
    }
    panic!(
        "{label} did not materialize after {ACTION_ATTEMPTS} attempts in {session}; \
         last observation: {last_observation}"
    );
}

/// Open a second tab the way a user would, from the default tab template.
pub(in crate::backend::zellij) fn open_new_tab(xdg: &Path, session: &str) {
    let before: BTreeSet<u64> = tab_ids(xdg, session).into_iter().collect();
    let args = ["action".to_owned(), "new-tab".to_owned()];
    action_until(xdg, session, &args, "new-tab", || {
        let panes = list_panes_json(xdg, session)?;
        let after = tab_ids_from_panes(&panes);
        let new_tabs: Vec<u64> = after
            .iter()
            .copied()
            .filter(|id| !before.contains(id))
            .collect();
        if new_tabs.is_empty() {
            Err(format!("tabs still {after:?}; before tabs were {before:?}"))
        } else {
            Ok(())
        }
    });
}

/// Parsed `list-panes -j -a` for `session`. Callers that poll keep the last
/// error so deadline failures report the command failure instead of "no panes".
pub(in crate::backend::zellij) fn list_panes_json(
    xdg: &Path,
    session: &str,
) -> std::result::Result<serde_json::Value, String> {
    let mut last_error = "list-panes was not run".to_owned();
    for attempt in 0..LIST_PANES_JSON_ATTEMPTS {
        match scoped_zellij(xdg)
            .args(["--session", session, "action", "list-panes", "-j", "-a"])
            .bounded_output_within(LIST_PANES_JSON_TIMEOUT)
        {
            Ok(output) if output.status.success() => match serde_json::from_slice(&output.stdout) {
                Ok(panes) => return Ok(panes),
                Err(err) => {
                    last_error = format!(
                        "parsing list-panes JSON for {session}: {err}; stdout: {}; stderr: {}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr),
                    );
                }
            },
            Ok(output) => {
                last_error = format!(
                    "list-panes failed for {session} with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr),
                );
            }
            Err(err) => {
                last_error = format!("list-panes failed for {session}: {err}");
            }
        }
        if attempt + 1 < LIST_PANES_JSON_ATTEMPTS {
            std::thread::sleep(LIST_PANES_JSON_RETRY_DELAY);
        }
    }
    Err(last_error)
}

pub(in crate::backend::zellij) fn expect_list_panes_json(
    xdg: &Path,
    session: &str,
) -> serde_json::Value {
    list_panes_json(xdg, session).unwrap_or_else(|err| panic!("{err}"))
}

pub(in crate::backend::zellij) fn pane_refs_from_list_panes_json(
    session: &str,
    panes: &serde_json::Value,
) -> Vec<PaneRef> {
    panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|pane| pane.get("is_plugin").and_then(|value| value.as_bool()) != Some(true))
        .filter(|pane| pane.get("is_held").and_then(|value| value.as_bool()) != Some(true))
        .filter(|pane| pane.get("exited").and_then(|value| value.as_bool()) != Some(true))
        .filter(|pane| pane.get("is_suppressed").and_then(|value| value.as_bool()) != Some(true))
        .filter_map(|pane| pane_ref_from_list_panes_json(session, pane))
        .collect()
}

pub(in crate::backend::zellij) fn write_topology_cache_from_list_panes(
    xdg: &Path,
    workspace_id: &WorkspaceId,
    session: &str,
) {
    let panes = expect_list_panes_json(xdg, session);
    write_topology_cache_from_value(xdg, workspace_id, session, &panes);
}

fn write_topology_cache_from_value(
    xdg: &Path,
    workspace_id: &WorkspaceId,
    session: &str,
    panes: &serde_json::Value,
) {
    let tab_positions = tab_positions_from_list_panes_json(panes);
    let topology_panes: Vec<PaneTopologyPane> = panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|pane| topology_pane_from_list_panes_json(pane, &tab_positions))
        .collect();
    let topology = PaneTopologyCache {
        session_name: session.to_owned(),
        produced_at_ms: now_ms(),
        focused_pane: None,
        panes: topology_panes,
    };
    let path = xdg
        .join("rimz")
        .join(workspace_id.as_str())
        .join("pane-topology.json");
    std::fs::create_dir_all(path.parent().expect("topology parent"))
        .expect("create topology parent");
    rimz::ledger::atomic::write_temp_then_rename_cache_compact(&path, &topology)
        .expect("write topology cache");
}

pub(in crate::backend::zellij) struct TopologyCacheMirror {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for TopologyCacheMirror {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("topology mirror thread");
        }
    }
}

pub(in crate::backend::zellij) fn topology_cache_mirror(
    xdg: &Path,
    workspace_id: &WorkspaceId,
    session: &str,
) -> TopologyCacheMirror {
    let xdg = xdg.to_path_buf();
    let workspace_id = workspace_id.clone();
    let session = session.to_owned();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            if let Ok(panes) = list_panes_json(&xdg, &session) {
                write_topology_cache_from_value(&xdg, &workspace_id, &session, &panes);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    TopologyCacheMirror {
        stop,
        handle: Some(handle),
    }
}

pub(in crate::backend::zellij) fn record_known_workspace_session(
    state_root: &Path,
    workspace_id: &WorkspaceId,
    project_root: &Path,
    session: &str,
) {
    let state = rimz::StatePaths::under(workspace_id.clone(), state_root).expect("state paths");
    state.ensure_dirs().expect("workspace state dirs");
    let record = rimz::WorkspaceRecord {
        workspace_id: workspace_id.clone(),
        project_root: project_root.to_path_buf(),
        session_name: session.to_owned(),
        root_class: rimz::workspace::RootClass::Directory,
        updated_at: jiff::Timestamp::now(),
    };
    rimz::ledger::workspace_record::write(&state, &record).expect("workspace record");
}

fn tab_positions_from_list_panes_json(panes: &serde_json::Value) -> BTreeMap<u64, u64> {
    let mut positions = BTreeMap::new();
    for pane in panes.as_array().map(Vec::as_slice).unwrap_or_default() {
        let Some(tab_id) = pane.get("tab_id").and_then(|value| value.as_u64()) else {
            continue;
        };
        if !positions.contains_key(&tab_id) {
            let position = u64::try_from(positions.len()).ok().unwrap_or(u64::MAX);
            positions.insert(tab_id, position);
        }
    }
    positions
}

fn topology_pane_from_list_panes_json(
    pane: &serde_json::Value,
    tab_positions: &BTreeMap<u64, u64>,
) -> Option<PaneTopologyPane> {
    let tab_position = pane
        .get("tab_position")
        .and_then(|value| value.as_u64())
        .or_else(|| {
            pane.get("tab_id")
                .and_then(|value| value.as_u64())
                .and_then(|tab_id| tab_positions.get(&tab_id).copied())
        })
        .unwrap_or(1);
    Some(PaneTopologyPane {
        id: pane.get("id")?.as_u64()?,
        is_plugin: pane
            .get("is_plugin")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        is_held: pane
            .get("is_held")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        exited: pane
            .get("exited")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        is_suppressed: pane
            .get("is_suppressed")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        is_floating: pane
            .get("is_floating")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        is_focused: pane
            .get("is_focused")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        tab_position,
        tab_name: pane
            .get("tab_name")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        pane_columns: pane.get("pane_columns").and_then(|value| value.as_u64()),
        pane_x: pane.get("pane_x").and_then(|value| value.as_u64()),
        title: pane
            .get("title")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        pane_command: pane
            .get("pane_command")
            .or_else(|| pane.get("command"))
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        terminal_command: pane
            .get("terminal_command")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn pane_ref_from_list_panes_json(session: &str, pane: &serde_json::Value) -> Option<PaneRef> {
    let id = pane.get("id")?.as_u64()?;
    let tab = pane
        .get("tab_position")
        .or_else(|| pane.get("tab_id"))
        .and_then(|value| value.as_u64());
    Some(PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{id}")),
        session_name: session.to_owned(),
        view_id: tab.map(|tab| format!("tab_{tab}")),
        view_kind: Some(ViewKind::Tab),
        view_name: pane
            .get("tab_name")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        is_focused: pane
            .get("is_focused")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        is_floating: pane
            .get("is_floating")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        command: pane
            .get("pane_command")
            .or_else(|| pane.get("command"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        spawn_command: pane
            .get("terminal_command")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        cwd: None,
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    })
}

#[derive(Clone, Copy, Debug)]
pub(in crate::backend::zellij) struct PaneGeometry {
    pub(in crate::backend::zellij) id: u64,
    pub(in crate::backend::zellij) x: u64,
    pub(in crate::backend::zellij) y: u64,
    pub(in crate::backend::zellij) columns: u64,
    pub(in crate::backend::zellij) rows: u64,
}

pub(in crate::backend::zellij) fn pane_geometry(pane: &serde_json::Value) -> Option<PaneGeometry> {
    Some(PaneGeometry {
        id: pane.get("id")?.as_u64()?,
        x: pane.get("pane_x")?.as_u64()?,
        y: pane.get("pane_y")?.as_u64()?,
        columns: pane.get("pane_columns")?.as_u64()?,
        rows: pane.get("pane_rows")?.as_u64()?,
    })
}

pub(in crate::backend::zellij) fn named_work_pane_geometry(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> std::result::Result<Vec<PaneGeometry>, String> {
    let panes = list_panes_json(xdg, session)?;
    let mut work: Vec<PaneGeometry> = panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|pane| pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false))
        .filter(|pane| {
            pane.get("tab_name").and_then(|value| value.as_str()) == Some(tab_name)
                && pane.get("title").and_then(|value| value.as_str()) != Some("rimz-sidebar")
        })
        .filter_map(pane_geometry)
        .collect();
    work.sort_by_key(|pane| pane.x);
    Ok(work)
}

pub(in crate::backend::zellij) fn named_sidebar_pane_geometry(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> std::result::Result<Option<PaneGeometry>, String> {
    let panes = list_panes_json(xdg, session)?;
    Ok(panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|pane| pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false))
        .find(|pane| {
            pane.get("tab_name").and_then(|value| value.as_str()) == Some(tab_name)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .and_then(pane_geometry))
}

pub(in crate::backend::zellij) fn named_compact_bar_pane_geometry(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> std::result::Result<Option<PaneGeometry>, String> {
    let panes = list_panes_json(xdg, session)?;
    Ok(panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(true)
                && pane.get("tab_name").and_then(|value| value.as_str()) == Some(tab_name)
                && pane
                    .get("title")
                    .and_then(|value| value.as_str())
                    .is_some_and(|title| title.contains("compact-bar"))
        })
        .and_then(pane_geometry))
}

pub(in crate::backend::zellij) fn wait_for_named_sidebar_pane(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> Option<PaneGeometry> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match named_sidebar_pane_geometry(xdg, session, tab_name) {
            Ok(sidebar) if sidebar.is_some() => return sidebar,
            Ok(sidebar) if Instant::now() >= deadline => return sidebar,
            Ok(_) => {}
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for sidebar pane in {session}/{tab_name}; \
                         last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(in crate::backend::zellij) fn wait_for_named_compact_bar_pane(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> Option<PaneGeometry> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match named_compact_bar_pane_geometry(xdg, session, tab_name) {
            Ok(bar) if bar.is_some() => return bar,
            Ok(bar) if Instant::now() >= deadline => return bar,
            Ok(_) => {}
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for compact bar pane in {session}/{tab_name}; \
                         last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(in crate::backend::zellij) fn wait_for_named_work_pane_state<F>(
    xdg: &Path,
    session: &str,
    tab_name: &str,
    want: usize,
    mut ready: F,
) -> Vec<PaneGeometry>
where
    F: FnMut(&[PaneGeometry]) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_work = Vec::new();
    loop {
        match named_work_pane_geometry(xdg, session, tab_name) {
            Ok(work) => {
                if work.len() == want && ready(&work) {
                    return work;
                }
                last_work = work;
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for {want} work panes in {session}/{tab_name}; \
                         last panes: {last_work:?}",
                    );
                }
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for {want} work panes in {session}/{tab_name}; \
                         last panes: {last_work:?}; last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(in crate::backend::zellij) fn work_pane_geometry(
    xdg: &Path,
    session: &str,
) -> Vec<PaneGeometry> {
    let panes = expect_list_panes_json(xdg, session);
    let mut work: Vec<PaneGeometry> = panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|pane| pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false))
        .filter(|pane| pane.get("title").and_then(|value| value.as_str()) != Some("rimz-sidebar"))
        .filter(|pane| pane.get("is_held").and_then(|value| value.as_bool()) != Some(true))
        .filter(|pane| pane.get("exited").and_then(|value| value.as_bool()) != Some(true))
        .filter_map(pane_geometry)
        .collect();
    work.sort_by_key(|pane| pane.id);
    work
}

pub(in crate::backend::zellij) fn wait_for_named_work_pane_count(
    xdg: &Path,
    session: &str,
    tab_name: &str,
    want: usize,
) -> Vec<PaneGeometry> {
    wait_for_named_work_pane_state(xdg, session, tab_name, want, |_| true)
}

pub(in crate::backend::zellij) fn spawn_sleep_pane(xdg: &Path, session: &str, cwd: &Path) {
    let before = live_work_pane_count(&expect_list_panes_json(xdg, session));
    let args = [
        "action".to_owned(),
        "new-pane".to_owned(),
        "--cwd".to_owned(),
        cwd.to_string_lossy().into_owned(),
        "--".to_owned(),
        "sleep".to_owned(),
        "600".to_owned(),
    ];
    action_until(xdg, session, &args, "new-pane", || {
        let panes = list_panes_json(xdg, session)?;
        let after = live_work_pane_count(&panes);
        if after > before {
            Ok(())
        } else {
            Err(format!(
                "live work panes still {after}; before was {before}"
            ))
        }
    });
}

pub(in crate::backend::zellij) fn assert_work_panes_reopen_in_survivor_after_closing_first(
    xdg: &Path,
    session: &str,
    tab_name: &str,
    cwd: &Path,
    client_columns: u16,
    client_rows: u16,
) {
    let work = wait_for_named_work_pane_count(xdg, session, tab_name, 2);
    assert_eq!(
        work.len(),
        2,
        "tab should start with two work panes: {work:?}",
    );
    let close = format!("terminal_{}", work[0].id);
    let closed = scoped_zellij(xdg)
        .args([
            "--session",
            session,
            "action",
            "close-pane",
            "--pane-id",
            &close,
        ])
        .bounded_output()
        .expect("close-pane");
    assert!(
        closed.status.success(),
        "close-pane failed: {}",
        String::from_utf8_lossy(&closed.stderr),
    );

    let sidebar_after_close =
        wait_for_named_sidebar_pane(xdg, session, tab_name).expect("work tab keeps its sidebar");
    assert_eq!(
        sidebar_after_close.x, 0,
        "sidebar should stay docked left after close: {sidebar_after_close:?}",
    );
    let expected_work_columns =
        u64::from(client_columns).saturating_sub(sidebar_after_close.columns);
    let survivor = wait_for_named_work_pane_state(xdg, session, tab_name, 1, |work| {
        work[0].columns.abs_diff(expected_work_columns) <= 5
    });
    assert_eq!(
        survivor.len(),
        1,
        "closing one work pane should leave one survivor: {survivor:?}",
    );
    let survivor_diff = survivor[0].columns.abs_diff(expected_work_columns);
    assert!(
        survivor_diff <= 5,
        "surviving work pane should fill the work area after close; expected \
         about {expected_work_columns} cols, got {survivor:?}",
    );
    let focus = scoped_zellij(xdg)
        .args([
            "--session",
            session,
            "action",
            "focus-pane-id",
            &format!("terminal_{}", survivor[0].id),
        ])
        .bounded_output()
        .expect("focus-pane-id");
    assert!(
        focus.status.success(),
        "focus-pane-id failed: {}",
        String::from_utf8_lossy(&focus.stderr),
    );

    let survivor_before_split = survivor[0];
    spawn_sleep_pane(xdg, session, cwd);

    let inside_survivor = |pane: &PaneGeometry| {
        pane.x + 2 >= survivor_before_split.x
            && pane.y + 2 >= survivor_before_split.y
            && pane.x + pane.columns <= survivor_before_split.x + survivor_before_split.columns + 2
            && pane.y + pane.rows <= survivor_before_split.y + survivor_before_split.rows + 2
    };
    let split = wait_for_named_work_pane_state(xdg, session, tab_name, 2, |work| {
        work.iter().all(inside_survivor)
    });
    assert_eq!(
        split.len(),
        2,
        "new terminal should land in the same work tab: {split:?}",
    );
    assert!(
        split.iter().all(inside_survivor),
        "work panes should split inside the reclaimed survivor bounds \
         {survivor_before_split:?}, got {split:?}",
    );
    let sidebar =
        wait_for_named_sidebar_pane(xdg, session, tab_name).expect("work tab keeps its sidebar");
    assert_eq!(sidebar.x, 0, "sidebar should stay docked left: {sidebar:?}");
    assert!(
        (68..=76).contains(&sidebar.columns),
        "sidebar should stay near the 72-column cap: {sidebar:?}",
    );
    let bar = wait_for_named_compact_bar_pane(xdg, session, tab_name)
        .expect("work tab keeps its compact-bar");
    assert_eq!(
        bar.x, 0,
        "compact bar should span from the left edge: {bar:?}"
    );
    assert_eq!(
        bar.columns,
        u64::from(client_columns),
        "compact bar should span the whole tab width: {bar:?}",
    );
    assert_eq!(bar.rows, 1, "compact bar should stay one row tall: {bar:?}");
    assert_eq!(
        bar.y + bar.rows,
        u64::from(client_rows),
        "compact bar should stay docked at the bottom: {bar:?}",
    );
}

pub(in crate::backend::zellij) fn assert_sidebars_not_held(
    xdg: &Path,
    session: &str,
    context: &str,
) {
    let panes = expect_list_panes_json(xdg, session);
    let sidebars: Vec<&serde_json::Value> = panes
        .as_array()
        .expect("pane array")
        .iter()
        .filter(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .collect();
    assert!(
        !sidebars.is_empty(),
        "rimz-sidebar pane missing while checking {context}:\n{panes}",
    );
    for sidebar in sidebars {
        assert_ne!(
            sidebar.get("is_held").and_then(|value| value.as_bool()),
            Some(true),
            "sidebar command pane is waiting for Enter instead of running in {context}:\n{sidebar}",
        );
    }
}

/// Dump just the `new_tab_template` section for readable assertions.
pub(in crate::backend::zellij) fn new_tab_template_dump(xdg: &Path, session: &str) -> String {
    let mut last_observation = "dump-layout was not checked".to_owned();
    for attempt in 0..DUMP_LAYOUT_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(DUMP_LAYOUT_RETRY_DELAY);
        }
        let output = scoped_zellij(xdg)
            .args(["--session", session, "action", "dump-layout"])
            .bounded_output()
            .unwrap_or_else(|err| panic!("dump-layout failed to run for {session}: {err}"));
        let dump = String::from_utf8_lossy(&output.stdout);
        if output.status.success() {
            if let Some(start) = dump.find("new_tab_template") {
                return dump[start..].to_owned();
            }
            last_observation = format!("stdout:\n{dump}");
        } else {
            last_observation = format!(
                "status {}; stderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }
    panic!(
        "dump-layout has no new_tab_template after {DUMP_LAYOUT_ATTEMPTS} attempts in {session}; \
         last observation: {last_observation}",
    );
}

/// Distinct tab ids that currently hold a non-plugin pane.
pub(in crate::backend::zellij) fn tab_ids(xdg: &Path, session: &str) -> Vec<u64> {
    tab_ids_from_panes(&expect_list_panes_json(xdg, session))
}

pub(in crate::backend::zellij) fn tab_ids_from_panes(panes: &serde_json::Value) -> Vec<u64> {
    let mut ids: Vec<u64> = panes
        .as_array()
        .map(|panes| {
            panes
                .iter()
                .filter(|p| p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false))
                .filter_map(|p| p.get("tab_id").and_then(|v| v.as_u64()))
                .collect()
        })
        .unwrap_or_default();
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub(in crate::backend::zellij) fn live_work_pane_count(panes: &serde_json::Value) -> usize {
    panes
        .as_array()
        .map(|panes| {
            panes
                .iter()
                .filter(|pane| {
                    pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                })
                .filter(|pane| {
                    pane.get("title").and_then(|value| value.as_str()) != Some("rimz-sidebar")
                })
                .filter(|pane| pane.get("is_held").and_then(|value| value.as_bool()) != Some(true))
                .filter(|pane| pane.get("exited").and_then(|value| value.as_bool()) != Some(true))
                .count()
        })
        .unwrap_or_default()
}

/// Titles of the non-plugin panes in `tab`.
pub(in crate::backend::zellij) fn nonplugin_titles_in_tab(
    xdg: &Path,
    session: &str,
    tab: u64,
) -> Vec<String> {
    let panes = expect_list_panes_json(xdg, session);
    panes
        .as_array()
        .map(|panes| {
            panes
                .iter()
                .filter(|p| p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false))
                .filter(|p| p.get("tab_id").and_then(|v| v.as_u64()) == Some(tab))
                .filter_map(|p| p.get("title").and_then(|v| v.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Title of the focused non-plugin pane in `tab`, if any.
pub(in crate::backend::zellij) fn focused_nonplugin_title_in_tab(
    xdg: &Path,
    session: &str,
    tab: u64,
) -> Option<String> {
    let panes = expect_list_panes_json(xdg, session);
    panes.as_array()?.iter().find_map(|p| {
        (p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false)
            && p.get("tab_id").and_then(|v| v.as_u64()) == Some(tab)
            && p.get("is_focused").and_then(|v| v.as_bool()) == Some(true))
        .then(|| {
            p.get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned()
        })
    })
}

pub(in crate::backend::zellij) fn wait_for_focused_non_sidebar_title_in_tab(
    xdg: &Path,
    session: &str,
    tab: u64,
) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let focused = focused_nonplugin_title_in_tab(xdg, session, tab);
        if focused
            .as_deref()
            .is_some_and(|title| title != "rimz-sidebar")
            || Instant::now() >= deadline
        {
            return focused;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Raw id of the focused non-plugin pane in `tab`, if any.
pub(in crate::backend::zellij) fn focused_nonplugin_id_in_tab(
    xdg: &Path,
    session: &str,
    tab: u64,
) -> Option<u64> {
    focused_nonplugin_id_in_tab_result(xdg, session, tab).unwrap_or_else(|err| panic!("{err}"))
}

pub(in crate::backend::zellij) fn focused_nonplugin_id_in_tab_result(
    xdg: &Path,
    session: &str,
    tab: u64,
) -> std::result::Result<Option<u64>, String> {
    let panes = list_panes_json(xdg, session)?;
    Ok(panes.as_array().and_then(|panes| {
        panes.iter().find_map(|p| {
            if p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false)
                && p.get("tab_id").and_then(|v| v.as_u64()) == Some(tab)
                && p.get("is_focused").and_then(|v| v.as_bool()) == Some(true)
            {
                p.get("id").and_then(|v| v.as_u64())
            } else {
                None
            }
        })
    }))
}

pub(in crate::backend::zellij) fn wait_for_focused_nonplugin_id_in_tab(
    xdg: &Path,
    session: &str,
    tab: u64,
    want: u64,
) -> Option<u64> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match focused_nonplugin_id_in_tab_result(xdg, session, tab) {
            Ok(focused) => {
                if focused == Some(want) || Instant::now() >= deadline {
                    return focused;
                }
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for focused pane {want} in {session}/tab {tab}; \
                         last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(in crate::backend::zellij) fn focus_nonplugin_pane_until(
    xdg: &Path,
    session: &str,
    tab: u64,
    want: u64,
    context: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let pane_id = format!("terminal_{want}");
    let mut last_focused = None;
    let mut last_error = String::new();
    let run_action = |args: &[&str], last_error: &mut String| match scoped_zellij(xdg)
        .args(["--session", session, "action"])
        .args(args.iter().copied())
        .bounded_output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            *last_error = format!(
                "{} exited with {}; stderr: {}",
                args[0],
                output.status,
                String::from_utf8_lossy(&output.stderr),
            );
        }
        Err(err) => {
            *last_error = format!("{} failed to run: {err}", args[0]);
        }
    };
    let observe_focus = |last_focused: &mut Option<u64>, last_error: &mut String| -> bool {
        match focused_nonplugin_id_in_tab_result(xdg, session, tab) {
            Ok(focused) => {
                *last_focused = focused;
                focused == Some(want)
            }
            Err(err) => {
                *last_error = err;
                false
            }
        }
    };

    loop {
        for _ in 0..5 {
            run_action(&["focus-pane-id", pane_id.as_str()], &mut last_error);
            if observe_focus(&mut last_focused, &mut last_error) {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out focusing {context} ({pane_id}) in {session}/tab {tab}; \
                     last focused: {last_focused:?}; last error: {last_error}",
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        for action in ["focus-previous-pane", "focus-next-pane"] {
            for _ in 0..8 {
                run_action(&[action], &mut last_error);
                if observe_focus(&mut last_focused, &mut last_error) {
                    return;
                }
                if Instant::now() >= deadline {
                    panic!(
                        "timed out focusing {context} ({pane_id}) in {session}/tab {tab}; \
                         last focused: {last_focused:?}; last error: {last_error}",
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

pub(in crate::backend::zellij) fn wait_for_focused_client_pane(
    backend: &ZellijBackend,
    session: &str,
    want: &PaneId,
) -> Vec<PaneId> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let focused = backend
            .client_view(ClientFocusOptions {
                session_name: Some(session.to_owned()),
                ..Default::default()
            })
            .map(|view| view.viewed_panes)
            .expect("client_view");
        if focused.iter().any(|pane| pane == want) || Instant::now() >= deadline {
            return focused;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Poll until at least `want` distinct tabs hold a non-plugin pane, or time out.
pub(in crate::backend::zellij) fn wait_for_tab_count(
    xdg: &Path,
    session: &str,
    want: usize,
) -> Vec<u64> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_ids = Vec::new();
    loop {
        match list_panes_json(xdg, session) {
            Ok(panes) => {
                let ids = tab_ids_from_panes(&panes);
                if ids.len() >= want || Instant::now() >= deadline {
                    return ids;
                }
                last_ids = ids;
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for {want} tabs in {session}; last ids: {last_ids:?}; \
                         last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Name of the first tab that appears after `before`, from the live pane list.
pub(in crate::backend::zellij) fn wait_for_new_tab_name(
    xdg: &Path,
    session: &str,
    before: &[u64],
) -> String {
    let before: BTreeSet<u64> = before.iter().copied().collect();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_new_tabs = BTreeSet::new();
    let mut last_unnamed_nonplugin_tabs = BTreeSet::new();
    loop {
        match list_panes_json(xdg, session) {
            Ok(panes) => {
                last_new_tabs.clear();
                last_unnamed_nonplugin_tabs.clear();
                if let Some(panes) = panes.as_array() {
                    for pane in panes {
                        let Some(tab_id) = pane.get("tab_id").and_then(|value| value.as_u64())
                        else {
                            continue;
                        };
                        if before.contains(&tab_id) {
                            continue;
                        }
                        last_new_tabs.insert(tab_id);
                        if pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false) {
                            if let Some(name) =
                                pane.get("tab_name").and_then(|value| value.as_str())
                            {
                                return name.to_owned();
                            }
                            last_unnamed_nonplugin_tabs.insert(tab_id);
                        }
                    }
                }
                if Instant::now() >= deadline {
                    if !last_unnamed_nonplugin_tabs.is_empty() {
                        panic!(
                            "new tab(s) {last_unnamed_nonplugin_tabs:?} carried unnamed \
                             non-plugin panes after 10s"
                        );
                    }
                    if !last_new_tabs.is_empty() {
                        panic!("new tab(s) {last_new_tabs:?} carried only plugin panes after 10s");
                    }
                    panic!("no new tab appeared after 10s; before tabs were {before:?}");
                }
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for new tab in {session}; last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll `list_panes` until at least `want` panes appear (bounded). Returns the
/// last observation either way so the caller can assert and print it.
pub(in crate::backend::zellij) fn wait_for_pane_count(
    xdg: &Path,
    session: &str,
    want: usize,
) -> Vec<PaneRef> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_panes = Vec::new();
    loop {
        match list_panes_json(xdg, session) {
            Ok(raw) => {
                let panes = pane_refs_from_list_panes_json(session, &raw);
                if panes.len() >= want || Instant::now() >= deadline {
                    return panes;
                }
                last_panes = panes;
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for {want} panes in {session}; last panes: \
                         {last_panes:?}; last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Run `reconcile_sidebars` until it observes the session's live panes.
///
/// Reconcile reads the pane list once and early-returns a no-op
/// `SidebarRecovery::default()` when that read comes back empty. Under heavy CI
/// load Zellij's screen thread can briefly answer `[]` past the backend's
/// bounded empty-retry, so a freshly born session's first reconcile occasionally
/// sees nothing and does nothing. Every reconcile test sets up a view that needs
/// work, so an all-zeros report is that transient-empty race rather than the real
/// outcome — retry it. A no-op pass touches no panes, so re-running is safe; a
/// genuine regression keeps returning the default until the deadline, letting the
/// caller's assertion fire on the real (still wrong) report.
pub(in crate::backend::zellij) fn reconcile_until_observed(
    xdg: &Path,
    opts: &SidebarPaneOptions,
    live: &SidebarLiveness,
) -> SidebarRecovery {
    reconcile_loop(xdg, opts, live, false)
}

/// Run `reconcile_sidebars` until an attached-client test reaches a productive
/// outcome.
///
/// When the caller has already confirmed an attached client, a deferral-only
/// report is the same transient load race as an empty pane read: the client probe
/// missed the screen thread for one pass, and production retries on later
/// attach/reload reconciles. Retry it here so the test models that self-healing
/// loop. A genuine attached-client regression keeps deferring until the deadline,
/// then returns the deferral report for the caller's assertion.
pub(in crate::backend::zellij) fn reconcile_until_converged(
    xdg: &Path,
    opts: &SidebarPaneOptions,
    live: &SidebarLiveness,
) -> SidebarRecovery {
    reconcile_loop(xdg, opts, live, true)
}

pub(in crate::backend::zellij) fn reconcile_loop(
    xdg: &Path,
    opts: &SidebarPaneOptions,
    live: &SidebarLiveness,
    retry_deferral: bool,
) -> SidebarRecovery {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let report = ZellijBackend::with_runtime_dir(xdg)
            .reconcile_sidebars(opts, live)
            .expect("reconcile_sidebars");
        let deferral_only = report.deferred > 0
            && SidebarRecovery {
                deferred: 0,
                ..report
            } == SidebarRecovery::default();
        let transient = report == SidebarRecovery::default() || (retry_deferral && deferral_only);
        if !transient || Instant::now() >= deadline {
            return report;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(in crate::backend::zellij) fn assert_sidebar_is_left_thirty_percent(xdg: &Path, session: &str) {
    let (columns, total_columns) = assert_sidebar_is_left_docked_inner(xdg, session);
    assert!(
        columns * 100 <= total_columns * 35,
        "sidebar should occupy roughly 30% of the tab: {columns}/{total_columns}",
    );
}

pub(in crate::backend::zellij) fn assert_sidebar_is_left_docked(xdg: &Path, session: &str) {
    let _ = assert_sidebar_is_left_docked_inner(xdg, session);
}

fn assert_sidebar_is_left_docked_inner(xdg: &Path, session: &str) -> (u64, u64) {
    let panes = expect_list_panes_json(xdg, session);
    let panes = panes.as_array().expect("pane geometry array");
    let sidebar = panes
        .iter()
        .find(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .expect("rimz-sidebar pane");
    let tab_id = sidebar
        .get("tab_id")
        .and_then(|value| value.as_u64())
        .expect("sidebar tab id");
    let columns = sidebar
        .get("pane_columns")
        .and_then(|value| value.as_u64())
        .expect("sidebar columns");
    let sidebar_id = sidebar
        .get("id")
        .and_then(|value| value.as_u64())
        .expect("sidebar id");
    let total_columns = panes
        .iter()
        .filter(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("tab_id").and_then(|value| value.as_u64()) == Some(tab_id)
        })
        .filter_map(|pane| {
            Some(pane.get("pane_x")?.as_u64()? + pane.get("pane_columns")?.as_u64()?)
        })
        .max()
        .expect("tab width");
    assert_eq!(
        sidebar.get("pane_x").and_then(|value| value.as_u64()),
        Some(0),
        "sidebar should be the left pane",
    );
    for pane in panes.iter().filter(|pane| {
        pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
            && pane.get("tab_id").and_then(|value| value.as_u64()) == Some(tab_id)
            && pane.get("id").and_then(|value| value.as_u64()) != Some(sidebar_id)
    }) {
        let x = pane
            .get("pane_x")
            .and_then(|value| value.as_u64())
            .expect("work pane x");
        assert!(
            x >= columns,
            "work pane intrudes into the sidebar column band: sidebar={sidebar}, pane={pane}",
        );
    }
    (columns, total_columns)
}

/// The `rimz-sidebar` pane's column width per tab, from the live pane listing.
/// Tabs without a sidebar are absent; an unanswerable listing is empty.
pub(in crate::backend::zellij) fn sidebar_columns_by_tab(
    xdg: &Path,
    session: &str,
) -> BTreeMap<u64, u64> {
    let Ok(output) = scoped_zellij(xdg)
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .bounded_output()
    else {
        return BTreeMap::new();
    };
    let Ok(panes) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return BTreeMap::new();
    };
    panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .filter_map(|pane| {
            Some((
                pane.get("tab_id")?.as_u64()?,
                pane.get("pane_columns")?.as_u64()?,
            ))
        })
        .collect()
}

/// Poll until `session` reports one sidebar per entry of `expected`, each
/// inside its tab's column range (ordered by tab id) — attach and tab-open
/// geometry settles asynchronously. `false` on timeout.
pub(in crate::backend::zellij) fn wait_for_sidebar_columns(
    xdg: &Path,
    session: &str,
    expected: &[std::ops::RangeInclusive<u64>],
) -> bool {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        let widths = sidebar_columns_by_tab(xdg, session);
        if widths.len() == expected.len()
            && widths
                .values()
                .zip(expected)
                .all(|(width, range)| range.contains(width))
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}
