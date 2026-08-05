//! Durable two-phase intent for RimZ-initiated attached-client focus actions.
//!
//! The request supplies a short presentation overlay before mux dispatch.
//! Command acceptance and native client observations confirm, supersede, or
//! fence that overlay independently.

use std::collections::HashSet;
use std::fs;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::debug;
use uuid::Uuid;

use crate::ids::PaneId;
use crate::mux::{ClientFocusOptions, ClientPaneView, MuxBackend};
use crate::sidebar::timing::FOCUS_ANCHOR_FRESH;
use crate::store::{RuntimePaths, atomic};

const FOCUS_ANCHOR_VERSION: &str = "rimz.focus-anchor.v3";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FocusNonce(Uuid);

impl FocusNonce {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for FocusNonce {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for FocusNonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusOrigin {
    User,
    AutomaticRepair,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusIntentState {
    Requested,
    Applied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusAnchor {
    pub nonce: FocusNonce,
    pub session_name: String,
    pub pane_id: PaneId,
    pub origin: FocusOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_generation: Option<u64>,
    pub issued_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at_ms: Option<u64>,
    pub state: FocusIntentState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_action: Vec<ClientPaneView>,
    pub offset: usize,
    #[serde(default)]
    pub order: Option<FrozenOrder>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FocusPresentation {
    Target(Box<FocusAnchor>),
    Fence,
}

/// Compact pulled truth retained by the renderer while no realtime overlay
/// needs the full snapshot. Focus resolution reads only pane membership,
/// session/client observations, and the pane observation stamp.
#[derive(Clone, Debug)]
pub(crate) struct FocusObservation {
    pub(crate) panes_observed_at_ms: Option<u64>,
    pub(crate) pane_session_name: Option<String>,
    pub(crate) pane_ids: Vec<PaneId>,
    pub(crate) presence_known: bool,
    pub(crate) client_views: Vec<ClientPaneView>,
}

impl FocusObservation {
    pub(crate) fn from_snapshot(snapshot: &crate::store::snapshot::SidebarSnapshot) -> Self {
        Self {
            panes_observed_at_ms: snapshot.panes_observed_at_ms,
            pane_session_name: snapshot.pane_session_name.clone(),
            pane_ids: snapshot
                .worktree_groups
                .iter()
                .flat_map(|group| &group.rows)
                .filter_map(|row| row.pane.as_ref().map(|pane| pane.pane_id.clone()))
                .collect(),
            presence_known: snapshot.presence.is_some(),
            client_views: snapshot.client_views.clone(),
        }
    }
}

pub struct FocusActionRequest<'a> {
    pub pane_id: PaneId,
    pub origin: FocusOrigin,
    pub repair_generation: Option<u64>,
    pub expected_pre_action: Option<&'a [ClientPaneView]>,
    pub offset: usize,
    pub order: Option<FrozenOrder>,
}

#[derive(Clone, Copy, Debug)]
pub struct FocusDispatchRetries {
    pub attempts: u32,
    pub delay: Duration,
}

impl Default for FocusDispatchRetries {
    fn default() -> Self {
        Self {
            attempts: 1,
            delay: Duration::ZERO,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusObservationOutcome {
    Present,
    Fence,
    Confirmed,
    Superseded,
    Invalidated,
}

#[derive(Debug, thiserror::Error)]
pub enum FocusActionError {
    #[error("sampling attached clients before focus: {0}")]
    ClientSample(#[source] crate::mux::MuxErr),
    #[error("attached-client focus changed before repair dispatch")]
    PreObservationChanged,
    #[error("serializing focus action intent: {0}")]
    Lock(#[from] crate::store::lock::LockErr),
    #[error("writing focus action intent: {0}")]
    Store(#[from] atomic::AtomicErr),
    #[error("dispatching focus action: {0}")]
    Dispatch(#[source] crate::mux::MuxErr),
    #[error("focus action was superseded before dispatch")]
    Superseded,
}

/// A snapshot of painted row/group order and the rows visible in that frame.
///
/// Group keys and row ids preserve presentation order. `visible` names the row
/// ids the renderer painted, so a peer renderer can keep cap exemptions stable
/// while it adopts a shared hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenOrder {
    pub(crate) groups: Vec<String>,
    pub(crate) rows: Vec<FrozenRow>,
    pub(crate) visible: HashSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenRow {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pane: Option<String>,
}

pub fn store(runtime: &RuntimePaths, anchor: &FocusAnchor) -> atomic::Result<()> {
    let path = runtime.focus_anchor_path();
    let file = FocusAnchorFile {
        v: FOCUS_ANCHOR_VERSION.to_owned(),
        anchor: anchor.clone(),
    };
    atomic::write_temp_then_rename_cache(&path, &file)
}

pub fn load(runtime: &RuntimePaths) -> Option<FocusAnchor> {
    let path = runtime.focus_anchor_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar focus anchor unreadable");
            return None;
        }
    };
    let file: FocusAnchorFile = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar focus anchor invalid");
            return None;
        }
    };
    if file.v != FOCUS_ANCHOR_VERSION {
        debug!(
            path = %path.display(),
            version = file.v,
            "sidebar focus anchor version ignored",
        );
        return None;
    }
    Some(file.anchor)
}

pub fn is_fresh(stamp_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(stamp_ms) <= FOCUS_ANCHOR_FRESH.as_millis() as u64
}

pub fn request_action(
    backend: &dyn MuxBackend,
    runtime: &RuntimePaths,
    session_name: &str,
    request: FocusActionRequest<'_>,
) -> Result<FocusNonce, FocusActionError> {
    request_action_with_client_sample(runtime, session_name, request, || {
        backend
            .client_view(ClientFocusOptions {
                session_name: Some(session_name.to_owned()),
                command_timeout: None,
            })
            .map(|view| view.clients)
            .map_err(FocusActionError::ClientSample)
    })
}

fn request_action_with_client_sample(
    runtime: &RuntimePaths,
    session_name: &str,
    request: FocusActionRequest<'_>,
    sample: impl FnOnce() -> Result<Vec<ClientPaneView>, FocusActionError>,
) -> Result<FocusNonce, FocusActionError> {
    let FocusActionRequest {
        pane_id,
        origin,
        repair_generation,
        expected_pre_action,
        offset,
        order,
    } = request;
    let _guard = crate::store::lock::WorkspaceLock::acquire(&runtime.focus_anchor_lock())?;
    let mut pre_action = sample()?;
    normalize_views(&mut pre_action);
    if let Some(expected) = expected_pre_action {
        let mut expected = expected.to_vec();
        normalize_views(&mut expected);
        if pre_action != expected {
            return Err(FocusActionError::PreObservationChanged);
        }
    }
    let nonce = FocusNonce::new();
    let event_pane_id = pane_id.clone();
    let anchor = FocusAnchor {
        nonce,
        session_name: session_name.to_owned(),
        pane_id,
        origin,
        repair_generation,
        issued_at_ms: crate::sidebar::timing::unix_now_ms(),
        applied_at_ms: None,
        state: FocusIntentState::Requested,
        pre_action,
        offset,
        order,
    };
    store(runtime, &anchor)?;
    drop(_guard);
    if let Err(err) = crate::sidebar::wakeup::broadcast(
        runtime,
        Some(session_name),
        crate::sidebar::events::SidebarEvent::FocusIntent {
            pane_id: event_pane_id.clone(),
            nonce,
        },
    ) {
        debug!(pane = %event_pane_id, error = %err, "focus intent broadcast failed");
    }
    Ok(nonce)
}

pub fn dispatch_action(
    backend: &dyn MuxBackend,
    runtime: &RuntimePaths,
    session_name: &str,
    pane_id: &PaneId,
    nonce: FocusNonce,
    retries: FocusDispatchRetries,
) -> Result<bool, FocusActionError> {
    let _guard = crate::store::lock::WorkspaceLock::acquire(&runtime.focus_anchor_lock())?;
    let Some(mut anchor) = load(runtime).filter(|anchor| anchor.nonce == nonce) else {
        return Ok(false);
    };
    let mut accepted = false;
    let mut last_error = None;
    for attempt in 0..retries.attempts.max(1) {
        if attempt > 0 {
            std::thread::sleep(retries.delay);
        }
        match backend.focus_pane(pane_id, Some(session_name)) {
            Ok(()) => accepted = true,
            Err(err) => last_error = Some(err),
        }
    }
    if !accepted {
        if let Err(clear_err) = fs::remove_file(runtime.focus_anchor_path())
            && clear_err.kind() != std::io::ErrorKind::NotFound
        {
            debug!(error = %clear_err, "failed focus intent cleanup after dispatch error");
        }
        // `attempts.max(1)` guarantees an all-error run retains its final error.
        return Err(FocusActionError::Dispatch(
            last_error.expect("focus retry loop ran at least once"),
        ));
    }
    let applied_at_ms = crate::sidebar::timing::unix_now_ms();
    anchor.state = FocusIntentState::Applied;
    anchor.applied_at_ms = Some(applied_at_ms);
    store(runtime, &anchor)?;
    Ok(true)
}

pub fn execute_action(
    backend: &dyn MuxBackend,
    runtime: &RuntimePaths,
    session_name: &str,
    pane_id: PaneId,
    origin: FocusOrigin,
    expected_pre_action: Option<&[ClientPaneView]>,
    retries: FocusDispatchRetries,
) -> Result<FocusNonce, FocusActionError> {
    let nonce = request_action(
        backend,
        runtime,
        session_name,
        FocusActionRequest {
            pane_id: pane_id.clone(),
            origin,
            repair_generation: None,
            expected_pre_action,
            offset: 0,
            order: None,
        },
    )?;
    if dispatch_action(backend, runtime, session_name, &pane_id, nonce, retries)? {
        Ok(nonce)
    } else {
        Err(FocusActionError::Superseded)
    }
}

pub(crate) fn observation_outcome_from(
    anchor: &FocusAnchor,
    observation: &FocusObservation,
    now_ms: u64,
) -> FocusObservationOutcome {
    if observation.pane_session_name.as_deref() != Some(anchor.session_name.as_str())
        || !observation.pane_ids.contains(&anchor.pane_id)
    {
        return FocusObservationOutcome::Invalidated;
    }
    if anchor.state == FocusIntentState::Requested {
        return if is_fresh(anchor.issued_at_ms, now_ms) {
            FocusObservationOutcome::Present
        } else {
            FocusObservationOutcome::Invalidated
        };
    }
    let Some(applied_at_ms) = anchor.applied_at_ms else {
        return FocusObservationOutcome::Invalidated;
    };
    if !observation.presence_known {
        return if is_fresh(applied_at_ms, now_ms) {
            FocusObservationOutcome::Present
        } else {
            FocusObservationOutcome::Fence
        };
    }
    let mut observed = observation.client_views.clone();
    normalize_views(&mut observed);
    if observed.is_empty() {
        return FocusObservationOutcome::Invalidated;
    }
    let expected_ids = anchor
        .pre_action
        .iter()
        .map(|view| &view.client_id)
        .collect::<HashSet<_>>();
    let observed_ids = observed
        .iter()
        .map(|view| &view.client_id)
        .collect::<HashSet<_>>();
    if expected_ids != observed_ids {
        return FocusObservationOutcome::Invalidated;
    }
    if observed.iter().all(|view| view.pane_id == anchor.pane_id) {
        return FocusObservationOutcome::Confirmed;
    }
    let mut expected = anchor.pre_action.clone();
    normalize_views(&mut expected);
    if observed != expected {
        return FocusObservationOutcome::Superseded;
    }
    if is_fresh(applied_at_ms, now_ms) {
        FocusObservationOutcome::Present
    } else {
        FocusObservationOutcome::Fence
    }
}

#[cfg(test)]
fn observation_outcome(
    anchor: &FocusAnchor,
    snapshot: &crate::store::snapshot::SidebarSnapshot,
    now_ms: u64,
) -> FocusObservationOutcome {
    observation_outcome_from(anchor, &FocusObservation::from_snapshot(snapshot), now_ms)
}

pub fn clear_matching(runtime: &RuntimePaths, nonce: FocusNonce) -> bool {
    let Ok(_guard) = crate::store::lock::WorkspaceLock::acquire(&runtime.focus_anchor_lock())
    else {
        return false;
    };
    if load(runtime).is_none_or(|anchor| anchor.nonce != nonce) {
        return false;
    }
    match fs::remove_file(runtime.focus_anchor_path()) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            debug!(error = %err, "focus intent clear failed");
            false
        }
    }
}

fn normalize_views(views: &mut Vec<ClientPaneView>) {
    views.sort();
    views.dedup();
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct FocusAnchorFile {
    v: String,
    #[serde(flatten)]
    anchor: FocusAnchor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, SidebarInstanceId, WorkspaceId};
    use jiff::Timestamp;
    use std::os::unix::net::UnixDatagram;
    use std::path::Path;
    use tempfile::TempDir;

    fn runtime() -> (TempDir, RuntimePaths) {
        let dir = TempDir::new().expect("tempdir");
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime");
        (dir, runtime)
    }

    fn anchor(stamp_ms: u64) -> FocusAnchor {
        FocusAnchor {
            nonce: FocusNonce::new(),
            session_name: "rimz-test".to_owned(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            origin: FocusOrigin::User,
            repair_generation: None,
            issued_at_ms: stamp_ms,
            applied_at_ms: Some(stamp_ms),
            state: FocusIntentState::Applied,
            pre_action: Vec::new(),
            offset: 7,
            order: None,
        }
    }

    fn view(client_id: u32, pane_id: &PaneId) -> ClientPaneView {
        ClientPaneView {
            client_id: crate::mux::MuxClientId::Zellij(client_id),
            pane_id: pane_id.clone(),
        }
    }

    fn observed_snapshot(
        session_name: &str,
        live_panes: &[PaneId],
        client_views: Vec<ClientPaneView>,
    ) -> crate::store::snapshot::SidebarSnapshot {
        let workspace = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace");
        let mut snapshot = crate::store::snapshot::SidebarSnapshot::build_with_agents(
            workspace,
            Vec::new(),
            Timestamp::now(),
        );
        let rows = live_panes
            .iter()
            .map(|pane_id| {
                let mut pane = crate::sidebar::test_support::pane("terminal_1", "zsh", "/tmp");
                pane.pane_id = pane_id.clone();
                pane.session_name = session_name.to_owned();
                let mut row = crate::sidebar::test_support::activity_row(
                    false,
                    None,
                    Timestamp::now(),
                    Path::new("/tmp"),
                );
                row.pane = Some(pane);
                row
            })
            .collect();
        snapshot.worktree_groups = vec![crate::sidebar::test_support::worktree_group(
            Path::new("/tmp"),
            rows,
        )];
        snapshot.pane_session_name = Some(session_name.to_owned());
        snapshot.presence = Some(if client_views.is_empty() {
            crate::store::snapshot::SidebarPresence::Detached
        } else {
            crate::store::snapshot::SidebarPresence::Active
        });
        snapshot.client_views = client_views;
        snapshot
    }

    #[test]
    fn stores_and_loads_anchor() {
        let (_dir, runtime) = runtime();
        let mut anchor = anchor(1_000);
        anchor.order = Some(FrozenOrder {
            groups: vec!["main".to_owned()],
            rows: vec![
                FrozenRow {
                    id: "row-1".to_owned(),
                    pane: Some("tmux:%1".to_owned()),
                },
                FrozenRow {
                    id: "row-2".to_owned(),
                    pane: Some("tmux:%2".to_owned()),
                },
            ],
            visible: HashSet::from(["row-2".to_owned()]),
        });

        store(&runtime, &anchor).expect("store anchor");

        assert_eq!(load(&runtime), Some(anchor));
    }

    #[test]
    fn missing_order_loads_scroll_only_anchor() {
        let (_dir, runtime) = runtime();
        let anchor = anchor(1_000);
        let mut file = serde_json::to_value(FocusAnchorFile {
            v: FOCUS_ANCHOR_VERSION.to_owned(),
            anchor: anchor.clone(),
        })
        .expect("anchor json");
        file.as_object_mut().expect("file object").remove("order");
        atomic::write_temp_then_rename_cache(&runtime.focus_anchor_path(), &file)
            .expect("write anchor");

        assert_eq!(load(&runtime), Some(anchor));
    }

    #[test]
    fn missing_anchor_loads_none() {
        let (_dir, runtime) = runtime();

        assert_eq!(load(&runtime), None);
    }

    #[test]
    fn wrong_version_loads_none() {
        let (_dir, runtime) = runtime();
        let file = FocusAnchorFile {
            v: "rimz.focus-anchor.v0".to_owned(),
            anchor: anchor(1_000),
        };
        atomic::write_temp_then_rename_cache(&runtime.focus_anchor_path(), &file)
            .expect("write anchor");

        assert_eq!(load(&runtime), None);
    }

    #[test]
    fn freshness_includes_ttl_boundary() {
        let ttl_ms = FOCUS_ANCHOR_FRESH.as_millis() as u64;

        assert!(is_fresh(1_000, 1_000 + ttl_ms));
        assert!(!is_fresh(1_000, 1_000 + ttl_ms + 1));
    }

    #[test]
    fn request_broadcasts_focus_intent_before_dispatch() {
        let (_dir, runtime) = runtime();
        runtime.ensure_dirs().expect("runtime dirs");
        let instance = SidebarInstanceId::new();
        let socket_path = runtime.sock_dir.join("focus-intent-test.sock");
        let socket = UnixDatagram::bind(&socket_path).expect("bind wakeup socket");
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("set socket timeout");
        crate::sidebar::write_heartbeat(
            &runtime,
            runtime.workspace_id.clone(),
            &instance,
            MuxName::Tmux,
            "rimz-test",
            &socket_path,
            None,
        )
        .expect("write heartbeat");
        let pane_id = PaneId::from_parts(MuxName::Tmux, "%1");

        let nonce = request_action_with_client_sample(
            &runtime,
            "rimz-test",
            FocusActionRequest {
                pane_id: pane_id.clone(),
                origin: FocusOrigin::User,
                repair_generation: None,
                expected_pre_action: None,
                offset: 7,
                order: None,
            },
            || Ok(Vec::new()),
        )
        .expect("request focus");

        let mut payload = [0_u8; 1024];
        let received = socket.recv(&mut payload).expect("receive focus intent");
        let envelope: crate::sidebar::events::SidebarEventEnvelope =
            serde_json::from_slice(&payload[..received]).expect("decode focus intent");
        assert_eq!(
            envelope.event,
            crate::sidebar::events::SidebarEvent::FocusIntent { pane_id, nonce }
        );
        assert_eq!(
            load(&runtime).expect("stored anchor").state,
            FocusIntentState::Requested
        );
    }

    #[test]
    fn action_intent_observations_confirm_supersede_and_fence() {
        let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let prior = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let other = PaneId::from_parts(MuxName::Zellij, "terminal_3");
        let mut intent = anchor(1_000);
        intent.pane_id = target.clone();
        intent.pre_action = vec![view(7, &prior)];
        let ttl_ms = FOCUS_ANCHOR_FRESH.as_millis() as u64;

        let unchanged = observed_snapshot(
            "rimz-test",
            &[prior.clone(), target.clone(), other.clone()],
            vec![view(7, &prior)],
        );
        assert_eq!(
            observation_outcome(&intent, &unchanged, 1_000 + ttl_ms),
            FocusObservationOutcome::Present,
        );
        assert_eq!(
            observation_outcome(&intent, &unchanged, 1_000 + ttl_ms + 1),
            FocusObservationOutcome::Fence,
        );

        let confirmed = observed_snapshot(
            "rimz-test",
            &[prior.clone(), target.clone()],
            vec![view(7, &target)],
        );
        assert_eq!(
            observation_outcome(&intent, &confirmed, 1_001),
            FocusObservationOutcome::Confirmed,
        );

        let superseded = observed_snapshot(
            "rimz-test",
            &[prior, target.clone(), other.clone()],
            vec![view(7, &other)],
        );
        assert_eq!(
            observation_outcome(&intent, &superseded, 1_001),
            FocusObservationOutcome::Superseded,
        );

        intent.state = FocusIntentState::Requested;
        intent.applied_at_ms = None;
        assert_eq!(
            observation_outcome(&intent, &confirmed, 1_001),
            FocusObservationOutcome::Present,
        );
        assert_eq!(
            observation_outcome(&intent, &confirmed, 1_000 + ttl_ms + 1),
            FocusObservationOutcome::Invalidated,
        );
    }

    #[test]
    fn action_intent_invalidates_on_scope_or_liveness_change() {
        let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let prior = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let mut intent = anchor(1_000);
        intent.pane_id = target.clone();
        intent.pre_action = vec![view(7, &prior)];

        for snapshot in [
            observed_snapshot(
                "replacement",
                std::slice::from_ref(&target),
                vec![view(7, &target)],
            ),
            observed_snapshot(
                "rimz-test",
                std::slice::from_ref(&prior),
                vec![view(7, &prior)],
            ),
            observed_snapshot("rimz-test", &[prior.clone(), target.clone()], Vec::new()),
            observed_snapshot(
                "rimz-test",
                &[prior, target.clone()],
                vec![view(8, &target)],
            ),
        ] {
            assert_eq!(
                observation_outcome(&intent, &snapshot, 1_001),
                FocusObservationOutcome::Invalidated,
            );
        }
    }
}
