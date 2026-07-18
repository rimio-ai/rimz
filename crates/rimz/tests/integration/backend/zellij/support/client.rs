use std::path::Path;
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, PaneId};
use rimz::mux::{ClientFocusOptions, MuxBackend, ZellijBackend};

use super::session::SPAWN_TIMEOUT;

pub(in crate::backend::zellij) fn client_viewed_panes(
    backend: &ZellijBackend,
    session: &str,
) -> Result<Vec<PaneId>, String> {
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
        match client_viewed_panes(&backend, session) {
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

pub(in crate::backend::zellij) fn focus_attached_client_pane_until(
    xdg: &Path,
    session: &str,
    want: u64,
    context: &str,
    mut focus_next: impl FnMut(),
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let backend = ZellijBackend::with_runtime_dir(xdg);
    let pane_id = PaneId::from_parts(MuxName::Zellij, format!("terminal_{want}"));
    let mut last_view = Vec::new();
    let mut last_error = String::new();
    let mut attempts = 0;
    loop {
        match client_viewed_panes(&backend, session) {
            Ok(viewed) => {
                if viewed.contains(&pane_id) {
                    return;
                }
                last_view = viewed;
            }
            Err(err) => last_error = err.to_string(),
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out focusing {context} ({pane_id}) through the attached client in {session}; attempts: {attempts}; last client view: {last_view:?}; last error: {last_error}"
            );
        }
        focus_next();
        attempts += 1;
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(in crate::backend::zellij) fn assert_client_input_reaches_pane(
    xdg: &Path,
    session: &str,
    want: u64,
    context: &str,
    mut send_line: impl FnMut(&str),
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let pane_id = PaneId::from_parts(MuxName::Zellij, format!("terminal_{want}"));
    let backend = ZellijBackend::with_runtime_dir(xdg);
    let marker = format!("rimz-routed-{}", uuid::Uuid::now_v7().simple());
    send_line(&marker);
    let mut last_capture = String::new();
    let mut last_error = String::new();
    loop {
        match backend.capture_pane(&pane_id, None, false) {
            Ok(capture) => {
                if capture.raw_text.contains(&marker) {
                    return;
                }
                last_capture = capture.raw_text;
            }
            Err(err) => last_error = err.to_string(),
        }
        if Instant::now() >= deadline {
            panic!(
                "attached input did not reach {context} ({pane_id}) in {session}; marker: {marker}; last capture: {last_capture:?}; last error: {last_error}"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(in crate::backend::zellij) fn wait_for_focused_client_pane(
    backend: &ZellijBackend,
    session: &str,
    want: &PaneId,
) -> Vec<PaneId> {
    super::actions::poll_until(
        Duration::from_secs(10),
        || client_viewed_panes(backend, session),
        |focused| focused.contains(want),
        &format!("client focus on {want}"),
    )
}
