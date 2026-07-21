use std::collections::HashSet;
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

/// How long a typed marker has to surface in the target pane's buffer.
const INPUT_DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// Cadence for re-reading the client view and retyping the marker. Requiring a
/// second delivery after this interval proves that routing settled rather than
/// merely crossing the target during the relayout.
const MARKER_RETYPE_INTERVAL: Duration = Duration::from_millis(500);
/// Capture depth for the marker search. A `stack-panes` repair leaves every
/// unfocused stack member one row tall, which pushes an already-echoed marker
/// out of the viewport, so search the scrollback rather than the visible grid.
const MARKER_CAPTURE_LINES: u16 = 200;

/// Assert that what the attached client types lands in `want`.
///
/// Routed input and target capture are the decision channel. Zellij can route a
/// `focus-pane-id` action before `list-clients` publishes a causally matching
/// view, so the client view is advisory: it keeps test text away from sidebar
/// panes and supplies failure context, but it does not gate on naming `want`.
/// Two captured markers separated by [`MARKER_RETYPE_INTERVAL`] prove that the
/// client settled on the target instead of crossing it during the relayout.
pub(in crate::backend::zellij) fn assert_client_input_reaches_pane(
    xdg: &Path,
    session: &str,
    want: u64,
    context: &str,
    mut send_line: impl FnMut(&str),
) {
    let pane_id = PaneId::from_parts(MuxName::Zellij, format!("terminal_{want}"));
    let backend = ZellijBackend::with_runtime_dir(xdg);
    let work_panes: HashSet<_> = super::panes::PaneSnapshot::expect(xdg, session)
        .panes
        .iter()
        .filter(|pane| pane.is_live_terminal() && !pane.is_sidebar())
        .map(|pane| pane.pane_ref(session).pane_id)
        .collect();

    let deadline = Instant::now() + INPUT_DELIVERY_TIMEOUT;
    let marker = format!("rimz-routed-{}", uuid::Uuid::now_v7().simple());
    let markers = [marker.clone(), format!("{marker}-confirmed")];
    let mut marker_index = 0;
    let mut next_sample_at = Instant::now();
    let mut last_capture = String::new();
    let mut last_view = Vec::new();
    let mut last_capture_error = String::new();
    let mut last_view_error = String::new();
    loop {
        if Instant::now() >= next_sample_at {
            next_sample_at = Instant::now() + MARKER_RETYPE_INTERVAL;
            match client_viewed_panes(&backend, session) {
                Ok(viewed) => {
                    if viewed.iter().any(|pane| work_panes.contains(pane)) {
                        send_line(&markers[marker_index]);
                    }
                    last_view = viewed;
                    last_view_error.clear();
                }
                Err(err) => last_view_error = err,
            }
        }
        match backend.capture_pane(&pane_id, Some(MARKER_CAPTURE_LINES), false) {
            Ok(capture) => {
                if capture.raw_text.contains(&markers[marker_index]) {
                    if marker_index + 1 == markers.len() {
                        return;
                    }
                    marker_index += 1;
                    next_sample_at = Instant::now() + MARKER_RETYPE_INTERVAL;
                }
                last_capture = capture.raw_text;
                last_capture_error.clear();
            }
            Err(err) => last_capture_error = err.to_string(),
        }
        if Instant::now() >= deadline {
            panic!(
                "attached input did not settle on {context} ({pane_id}) in {session}; pending marker: {}; last client view: {last_view:?}; last capture: {last_capture:?}; last view error: {last_view_error}; last capture error: {last_capture_error}",
                markers[marker_index],
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
