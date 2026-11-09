//! The host's duplicate-sidebar heal. A produced frame that keeps showing two
//! sidebars in one view for [`FRESH_PANE_GRACE`] starts one close-only pass
//! off the fetch thread; the pass claims exactly the panes attached to this
//! host and never adds, replaces, or closes a view's last sidebar.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::RuntimePaths;
use crate::ids::{MuxName, ViewId, WorkspaceId};
use crate::mux::{SidebarLiveness, SidebarLivenessSource};
use crate::sidebar::FRESH_PANE_GRACE;
use crate::sidebar::frame::PaneFrame;

use super::Subscribers;

/// Dwell and rate state for the heal, kept by the fetch worker.
#[derive(Default)]
pub(super) struct HealGate {
    /// Each view's duplicate, by the `observed_at_ms` of the first frame that
    /// showed it.
    first_seen_ms: HashMap<ViewId, u64>,
    pass: Arc<Mutex<PassState>>,
}

#[derive(Default)]
struct PassState {
    running: bool,
    finished_at: Option<Instant>,
}

impl HealGate {
    /// Whether `frame` starts a pass at `now`, marking it running when it
    /// does. Pure over the frame: no subprocess and no file read.
    pub(super) fn start_if_due(&mut self, frame: &PaneFrame, now: Instant) -> Option<PassDone> {
        let duplicates: HashSet<&ViewId> = frame.duplicate_sidebar_views().collect();
        self.first_seen_ms
            .retain(|view, _| duplicates.contains(view));
        for view in duplicates {
            self.first_seen_ms
                .entry(view.clone())
                .or_insert(frame.observed_at_ms);
        }
        let grace_ms = FRESH_PANE_GRACE.as_millis() as u64;
        let dwelled = self
            .first_seen_ms
            .values()
            .any(|first| frame.observed_at_ms.saturating_sub(*first) >= grace_ms);
        if !dwelled {
            return None;
        }
        let mut pass = self
            .pass
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let resting = pass
            .finished_at
            .is_some_and(|at| now.saturating_duration_since(at) < FRESH_PANE_GRACE);
        if pass.running || resting {
            return None;
        }
        pass.running = true;
        Some(PassDone(Arc::clone(&self.pass)))
    }
}

/// Marks the pass finished when dropped, whatever its outcome, so the next one
/// waits out another [`FRESH_PANE_GRACE`].
pub(super) struct PassDone(Arc<Mutex<PassState>>);

impl Drop for PassDone {
    fn drop(&mut self) {
        let mut pass = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pass.running = false;
        pass.finished_at = Some(Instant::now());
    }
}

/// Everything one heal pass needs on its own thread.
pub(super) struct HealPass {
    pub(super) mux: MuxName,
    pub(super) session_name: String,
    pub(super) workspace_id: WorkspaceId,
    pub(super) runtime: RuntimePaths,
    pub(super) subscribers: Subscribers,
    pub(super) diag: crate::diag::DiagSink,
    pub(super) done: PassDone,
}

impl HealPass {
    pub(super) fn spawn(self) {
        std::thread::spawn(move || self.run());
    }

    fn run(self) {
        let backend = crate::mux::backend_for(self.mux);
        let claims = AttachedPanes(&self.subscribers);
        let result = crate::sidebar::close_duplicate_sidebars(
            &*backend,
            &self.runtime,
            &self.session_name,
            &self.workspace_id,
            &claims,
            || self.diag.clone(),
        );
        match result {
            Ok(Some(report)) => tracing::debug!(
                session = %self.session_name,
                closed = report.closed,
                failed = report.failed,
                "sidebar heal: closed duplicate sidebars",
            ),
            Ok(None) => tracing::debug!(
                session = %self.session_name,
                "sidebar heal: reconcile lock busy; skipping",
            ),
            Err(err) => tracing::warn!(
                session = %self.session_name,
                tags.operation = "sidebar.heal",
                error = &err as &dyn std::error::Error,
                "sidebar heal pass failed",
            ),
        }
        drop(self.done);
    }
}

/// Claims read at sample time: every pane attached to this host.
struct AttachedPanes<'a>(&'a Subscribers);

impl SidebarLivenessSource for AttachedPanes<'_> {
    fn sample(&self) -> SidebarLiveness {
        let subscribers = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        SidebarLiveness {
            claimed_panes: subscribers
                .values()
                .map(|subscriber| subscriber.own_pane.clone())
                .collect(),
            ..SidebarLiveness::default()
        }
    }
}
