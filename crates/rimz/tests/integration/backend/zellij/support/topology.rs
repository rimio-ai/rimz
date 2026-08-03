use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rimz::ids::WorkspaceId;
use rimz::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};

use super::panes::{ListedPane, PaneSnapshot, list_panes};

impl ListedPane {
    fn topology(&self, fallback_positions: &BTreeMap<u64, u64>) -> PaneTopologyPane {
        PaneTopologyPane {
            id: self.id,
            is_plugin: self.is_plugin,
            is_held: self.is_held,
            exited: self.exited,
            is_suppressed: self.is_suppressed,
            is_floating: self.is_floating,
            tab_position: self
                .tab_position
                .or_else(|| fallback_positions.get(&self.tab_id).copied())
                .unwrap_or(1),
            tab_name: self.tab_name.clone(),
            pane_columns: Some(self.pane_columns),
            pane_x: Some(self.pane_x),
            title: self.title.clone(),
            pane_command: self.pane_command.clone(),
            pane_cwd: self.pane_cwd.clone(),
            pane_pid: None,
            terminal_command: self.terminal_command.clone(),
        }
    }
}

fn write_topology_cache(
    xdg: &Path,
    workspace_id: &WorkspaceId,
    session: &str,
    snapshot: &PaneSnapshot,
) {
    let fallback_positions: BTreeMap<u64, u64> = snapshot
        .tab_ids()
        .into_iter()
        .enumerate()
        .map(|(position, id)| (id, u64::try_from(position).unwrap_or(u64::MAX)))
        .collect();
    let topology = PaneTopologyCache {
        session_name: session.to_owned(),
        produced_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX),
        writer: None,
        focused_pane: None,
        clients: None,
        panes: snapshot
            .panes
            .iter()
            .map(|pane| pane.topology(&fallback_positions))
            .collect(),
    };
    let path = xdg
        .join("rimz")
        .join(workspace_id.as_str())
        .join("pane-topology.json");
    std::fs::create_dir_all(path.parent().expect("topology parent"))
        .expect("create topology parent");
    rimz::store::atomic::write_temp_then_rename_cache_compact(&path, &topology)
        .expect("write topology cache");
}

pub(in crate::backend::zellij) fn write_topology_cache_from_list_panes(
    xdg: &Path,
    workspace_id: &WorkspaceId,
    session: &str,
) {
    let snapshot = super::actions::poll_until(
        super::session::SPAWN_TIMEOUT,
        || list_panes(xdg, session),
        |snapshot| {
            snapshot
                .panes
                .iter()
                .any(|pane| !pane.is_plugin && !pane.is_suppressed && !pane.is_floating)
        },
        &format!("one tiled terminal pane in {session}"),
    );
    write_topology_cache(xdg, workspace_id, session, &snapshot);
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
            if let Ok(snapshot) = list_panes(&xdg, &session) {
                write_topology_cache(&xdg, &workspace_id, &session, &snapshot);
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
    let record = rimz::store::workspace_record::WorkspaceRecord {
        workspace_id: workspace_id.clone(),
        project_root: project_root.to_path_buf(),
        worktree_root: None,
        session_name: session.to_owned(),
        root_class: rimz::workspace::RootClass::Directory,
        rimz_bin: None,
        rimz_build: None,
        updated_at: jiff::Timestamp::now(),
    };
    rimz::store::workspace_record::write(&state, &record).expect("workspace record");
}
