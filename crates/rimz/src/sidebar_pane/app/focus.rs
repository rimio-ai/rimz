//! Detached mux focus execution and its wall-clock I/O retry pacing.

use super::focus_push::FocusPeers;
use super::input::Wakeup;
use crate::ids::{FocusNonce, PaneId};
use crate::mux::focus_anchor::FocusActionError;
use crate::sidebar::timing::FOCUS_REPAIR_RETRY_STEP;
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};
use tracing::debug;

pub(super) struct FocusRepairDispatch {
    pub(super) generation: u64,
    pub(super) deadline_ms: Option<u64>,
}

pub(super) struct FocusRequestOutcome {
    pub(super) requested: Option<std::result::Result<FocusNonce, FocusActionError>>,
    pub(super) observed: Vec<crate::pane::ClientPaneView>,
    pub(super) attempts: u32,
}

pub(super) fn request_focus_until(
    deadline_ms: Option<u64>,
    expected: &[crate::pane::ClientPaneView],
    mut request: impl FnMut() -> std::result::Result<FocusNonce, FocusActionError>,
    mut now_ms: impl FnMut() -> u64,
    mut sleep: impl FnMut(Duration),
) -> FocusRequestOutcome {
    let mut outcome = FocusRequestOutcome {
        requested: None,
        observed: Vec::new(),
        attempts: 0,
    };
    while deadline_ms.is_none_or(|deadline| now_ms() < deadline) {
        outcome.attempts += 1;
        let requested = request();
        match &requested {
            Err(FocusActionError::PreObservationChanged { observed }) => {
                outcome.observed = observed.clone();
            }
            Ok(_) | Err(FocusActionError::Store(_)) => {
                outcome.observed = expected.to_vec();
                outcome.observed.sort();
                outcome.observed.dedup();
            }
            _ => {}
        }
        let retry = matches!(
            &requested,
            Err(FocusActionError::PreObservationChanged { .. })
        );
        outcome.requested = Some(requested);
        if !retry {
            return outcome;
        }
        let Some(deadline) = deadline_ms else {
            return outcome;
        };
        let remaining_ms = deadline.saturating_sub(now_ms());
        if remaining_ms == 0 {
            return outcome;
        }
        sleep(FOCUS_REPAIR_RETRY_STEP.min(Duration::from_millis(remaining_ms)));
    }
    outcome
}

/// Focus the pane on a detached thread so the keypress/click returns instantly:
/// `focus_pane` forks the mux client (`zellij action focus-pane-id` / the tmux
/// equivalent), which must never block the loop. The snapshot-bound pane is
/// focused directly — no `rimz pane focus` child and no per-click pane roster.
/// The shared intent helper takes a fresh attached-client sample at request
/// time and serializes concurrent focus actions. An automatic
/// Zellij repair re-samples a mismatching fence until its strand deadline.
/// Errors return to the attachment for selection rollback and a recovered
/// health notice. A superseded dispatch stays diagnostic only.
/// A cross-tab user request pushes the stored anchor to host peers
/// and waits for the destination's frame until click + 500 ms before mux dispatch;
/// request work and any stale tmux fit spend that same budget, without resetting it.
/// The dispatch nonce check still fences superseded requests. The durable
/// request also wakes renderers outside the push path;
/// command acceptance applies it, and later native observations confirm or
/// supersede it.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_pane_focus(
    clock: &super::clock::LoopClock,
    clicked: Instant,
    pane_id: PaneId,
    session_name: &str,
    runtime: crate::disk::paths::RuntimePaths,
    origin: crate::mux::focus_anchor::FocusOrigin,
    expected_pre_action: Option<Vec<crate::pane::ClientPaneView>>,
    presentation: (
        Option<crate::mux::focus_anchor::FocusPlacement>,
        Option<crate::mux::focus_anchor::FrozenOrder>,
    ),
    repair: Option<FocusRepairDispatch>,
    peers: Option<FocusPeers>,
    wake: Option<SyncSender<Wakeup>>,
) {
    let session_name = session_name.to_owned();
    let requested_at_ms = crate::utils::time::unix_now_ms();
    clock.focus(|| {
        std::thread::spawn(move || {
            crate::harness::hibernate::spawn_wake(&runtime, &pane_id, requested_at_ms);
            let backend = crate::mux::backend_for(pane_id.mux());
            let repair_generation = repair.as_ref().map(|repair| repair.generation);
            let FocusRequestOutcome {
                requested,
                observed,
                attempts,
            } = request_focus_until(
                repair.as_ref().and_then(|repair| repair.deadline_ms),
                expected_pre_action.as_deref().unwrap_or_default(),
                || {
                    crate::mux::focus_anchor::request_action(
                        backend.as_ref(),
                        &runtime,
                        &session_name,
                        crate::mux::focus_anchor::FocusActionRequest {
                            pane_id: pane_id.clone(),
                            origin,
                            repair_generation,
                            expected_pre_action: expected_pre_action.as_deref(),
                            placement: presentation.0.clone(),
                            order: presentation.1.clone(),
                        },
                    )
                },
                crate::utils::time::unix_now_ms,
                std::thread::sleep,
            );
            let outcome = finish_focus_request(&pane_id, wake.as_ref(), requested, |nonce| {
                let dispatch = || {
                    crate::mux::focus_anchor::dispatch_action(
                        backend.as_ref(),
                        &runtime,
                        &session_name,
                        &pane_id,
                        nonce,
                        Default::default(),
                    )
                };
                if let Some(peers) = &peers
                    && let Some(anchor) =
                        crate::mux::focus_anchor::load(&runtime).filter(|anchor| anchor.nonce == nonce)
                {
                    peers.dispatch(anchor, clicked, dispatch)
                } else {
                    dispatch()
                }
            });
            if let Some(generation) = repair_generation {
                use crate::diag::focus_repair::{FocusRepairOutcome, FocusRepairRecord};
                crate::diag::focus_repair::spawn_append(
                    &runtime,
                    &FocusRepairRecord {
                        at: jiff::Timestamp::now(),
                        nonce: outcome.0.map(|nonce| nonce.to_string()),
                        workspace_id: runtime.workspace_id.clone(),
                        session_name: session_name.clone(),
                        generation,
                        evidence: expected_pre_action.clone().unwrap_or_default(),
                        observed: observed.clone(),
                        attempts,
                        target: pane_id.clone(),
                        outcome: if outcome.1 == "accepted_unconfirmed" {
                            FocusRepairOutcome::AcceptedUnconfirmed
                        } else {
                            FocusRepairOutcome::Failed
                        },
                        error: outcome.2.clone(),
                    },
                );
            }
            if let Some(error) = outcome.2 {
                debug!(pane = %pane_id, error, expected = ?expected_pre_action, observed = ?observed, attempts, "sidebar pane focus failed");
            }
        });
    });
}

pub(super) fn finish_focus_request(
    pane: &PaneId,
    wake: Option<&SyncSender<Wakeup>>,
    requested: Option<std::result::Result<FocusNonce, FocusActionError>>,
    dispatch: impl FnOnce(FocusNonce) -> std::result::Result<bool, FocusActionError>,
) -> (Option<FocusNonce>, &'static str, Option<String>) {
    let (nonce, dispatched) = match requested {
        Some(Ok(nonce)) => (Some(nonce), dispatch(nonce)),
        Some(Err(error)) => (None, Err(error)),
        None => {
            return (
                None,
                "failed",
                Some("strand TTL expired before focus request".into()),
            );
        }
    };
    match dispatched {
        Ok(true) => (nonce, "accepted_unconfirmed", None),
        Ok(false) => (
            nonce,
            "failed",
            Some("superseded before focus dispatch".into()),
        ),
        Err(error) => {
            let error = error.to_string();
            if let Some(wake) = wake {
                let _ = wake.send(Wakeup::FocusFailed {
                    pane: pane.clone(),
                    error: error.clone(),
                });
            }
            (nonce, "failed", Some(error))
        }
    }
}
