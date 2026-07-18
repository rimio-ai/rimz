use std::path::Path;
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, PaneId};
use rimz::mux::{ClientFocusOptions, MuxBackend, ZellijBackend};

use crate::common::CommandTimeoutExt;

use super::panes::list_panes;
use super::session::{SPAWN_TIMEOUT, scoped_zellij};

fn viewed_panes(backend: &ZellijBackend, session: &str) -> Result<Vec<PaneId>, String> {
    backend
        .client_view(ClientFocusOptions {
            session_name: Some(session.to_owned()),
            ..Default::default()
        })
        .map(|view| view.viewed_panes)
        .map_err(|err| err.to_string())
}

pub(in crate::backend::zellij) fn wait_for_attached_client(xdg: &Path, session: &str) {
    wait_for_client_view_count(xdg, session, 1);
}

pub(in crate::backend::zellij) fn wait_for_client_view_count(
    xdg: &Path,
    session: &str,
    want: usize,
) -> Vec<PaneId> {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let mut consecutive_matches = 0;
    let mut last_view = Vec::new();
    let mut last_error = String::new();
    let backend = ZellijBackend::with_runtime_dir(xdg);
    loop {
        match viewed_panes(&backend, session) {
            Ok(view) if view.len() == want => {
                last_view = view;
                last_error.clear();
                consecutive_matches += 1;
                if consecutive_matches == 2 {
                    return last_view;
                }
            }
            Ok(view) => {
                last_view = view;
                last_error.clear();
                consecutive_matches = 0;
            }
            Err(err) => {
                last_error = err;
                consecutive_matches = 0;
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "client view for {session} did not stabilize at {want} panes; last view: {last_view:?}; last error: {last_error}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(in crate::backend::zellij) fn wait_for_focused_nonplugin_id_in_tab(
    xdg: &Path,
    session: &str,
    tab: u64,
    want: u64,
) -> Option<u64> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_error = String::new();
    loop {
        match list_panes(xdg, session) {
            Ok(snapshot) => {
                let focused = snapshot.focused_terminal_in_tab(tab).map(|pane| pane.id);
                if focused == Some(want) || Instant::now() >= deadline {
                    return focused;
                }
            }
            Err(err) => last_error = err,
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for focused pane {want} in {session}/tab {tab}; last list-panes error: {last_error}"
            );
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
    let pane_id = PaneId::from_parts(MuxName::Zellij, format!("terminal_{want}"));
    let backend = ZellijBackend::with_runtime_dir(xdg);
    let mut last_focused = None;
    let mut last_error = String::new();
    let mut attempts = 0;
    loop {
        if let Err(err) = backend.focus_pane(&pane_id, Some(session)) {
            last_error = err.to_string();
        }
        match list_panes(xdg, session) {
            Ok(snapshot) => {
                last_focused = snapshot.focused_terminal_in_tab(tab).map(|pane| pane.id);
                if last_focused == Some(want) {
                    return;
                }
            }
            Err(err) => last_error = err,
        }
        attempts += 1;
        // Zellij can acknowledge a direct focus action without applying it under load;
        // bounded rotation supplies a second public action path before the deadline.
        if attempts % 5 == 0 {
            match scoped_zellij(xdg)
                .args(["--session", session, "action", "focus-next-pane"])
                .bounded_output()
            {
                Ok(output) if output.status.success() => {}
                Ok(output) => last_error = String::from_utf8_lossy(&output.stderr).into_owned(),
                Err(err) => last_error = err.to_string(),
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out focusing {context} ({pane_id}) in {session}/tab {tab}; last focused: {last_focused:?}; last error: {last_error}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(in crate::backend::zellij) fn wait_for_focused_client_pane(
    backend: &ZellijBackend,
    session: &str,
    want: &PaneId,
) -> Vec<PaneId> {
    super::actions::poll_until(
        Duration::from_secs(5),
        || viewed_panes(backend, session),
        |focused| focused.contains(want),
        &format!("client focus on {want}"),
    )
}
