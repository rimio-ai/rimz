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

/// How long the client view has to name the target pane. A repair restores
/// focus by dispatching `focus-pane-id` through its own Zellij client and
/// returns without waiting for the attached client to catch up, so the view
/// settles some time after the reconcile call does.
const FOCUS_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a typed marker has to surface in the target pane's buffer.
const INPUT_DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// Cadence for re-reading the client view and retyping the marker. Each read
/// spawns a `zellij action list-clients` client of its own, so sample on a
/// human-scale beat rather than on every capture poll.
const MARKER_RETYPE_INTERVAL: Duration = Duration::from_millis(500);
/// Capture depth for the marker search. A `stack-panes` repair leaves every
/// unfocused stack member one row tall, which pushes an already-echoed marker
/// out of the viewport, so search the scrollback rather than the visible grid.
const MARKER_CAPTURE_LINES: u16 = 200;

/// Assert that what the attached client types lands in `want`.
///
/// Delivery is a two-stage property, and each stage settles on the mux's own
/// clock: the client view has to name the pane, and a keystroke typed into the
/// PTY has to reach that pane's buffer. Wait for the view first so a marker is
/// only ever typed at a pane the client is actually focused on, then retype it
/// while that holds — a single keystroke racing a relayout is delivered to
/// whichever pane the repair left focused, and is unrecoverable once lost.
pub(in crate::backend::zellij) fn assert_client_input_reaches_pane(
    xdg: &Path,
    session: &str,
    want: u64,
    context: &str,
    mut send_line: impl FnMut(&str),
) {
    let pane_id = PaneId::from_parts(MuxName::Zellij, format!("terminal_{want}"));
    let backend = ZellijBackend::with_runtime_dir(xdg);
    super::actions::poll_until(
        FOCUS_SETTLE_TIMEOUT,
        || client_viewed_panes(&backend, session),
        |viewed| viewed.contains(&pane_id),
        &format!("the client view in {session} to settle on {context} ({pane_id})"),
    );

    let deadline = Instant::now() + INPUT_DELIVERY_TIMEOUT;
    let marker = format!("rimz-routed-{}", uuid::Uuid::now_v7().simple());
    send_line(&marker);
    let mut sampled_at = Instant::now();
    let mut last_capture = String::new();
    let mut last_view = Vec::new();
    let mut last_error = String::new();
    loop {
        match backend.capture_pane(&pane_id, Some(MARKER_CAPTURE_LINES), false) {
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
                "attached input did not reach {context} ({pane_id}) in {session}; marker: {marker}; last client view: {last_view:?}; last capture: {last_capture:?}; last error: {last_error}"
            );
        }
        if sampled_at.elapsed() >= MARKER_RETYPE_INTERVAL {
            sampled_at = Instant::now();
            match client_viewed_panes(&backend, session) {
                Ok(viewed) => {
                    if viewed.contains(&pane_id) {
                        send_line(&marker);
                    }
                    last_view = viewed;
                }
                Err(err) => last_error = err,
            }
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
