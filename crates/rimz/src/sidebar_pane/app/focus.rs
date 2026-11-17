//! Detached mux focus execution and its wall-clock I/O retry pacing.

use super::focus_push::FocusPeers;
use super::input::Wakeup;
use crate::ids::{AgentKind, AgentSessionId, FocusNonce, PaneId};
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

/// The agent a card focus is aimed at, as the reducing snapshot showed it in the pane:
/// the card's `(kind, session id)`, or a child entry's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CardAgent {
    pub(super) kind: AgentKind,
    pub(super) session: AgentSessionId,
}

impl CardAgent {
    /// The agent `row` shows in `pane`: the card's own agent when `pane` is the card's, else
    /// the child entry's; none for a process card.
    pub(super) fn in_row(row: &crate::store::snapshot::SidebarRow, pane: &PaneId) -> Option<Self> {
        if row.pane.as_ref().is_some_and(|own| own.pane_id == *pane) {
            return row.is_agent().then(|| Self {
                kind: AgentKind::new_unchecked(row.name.clone()),
                session: AgentSessionId::from(row.id.clone()),
            });
        }
        row.sub_agents()
            .iter()
            .find(|child| child.pane.as_ref() == Some(pane))
            .map(|child| Self {
                kind: child.kind.clone(),
                session: AgentSessionId::from(child.id.clone()),
            })
    }
}

/// Why a focus ended without the mux focusing its pane.
#[derive(Debug, thiserror::Error)]
pub(super) enum FocusFailure {
    #[error(transparent)]
    Action(#[from] FocusActionError),
    /// The hosting lookup says the card's agent is not in the focus's pane.
    #[error("{0}")]
    Refused(String),
}

/// Where the hosting lookup puts a card's agent now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CardHosting {
    /// No stored agent answers to the card's key: the card stands for its pane.
    Unheld,
    Pane(PaneId),
    Nowhere,
}

/// Judge a focus of `pane` aimed at `agent` by `ask`, one hosting read, before its dispatch.
/// `None` lets the dispatch go on, and an unkeyed focus (`q`, a process card, a repair)
/// takes no read. `Some(Ok(false))` ends it superseded, when the click stopped being current
/// during the read or its intent (nonce `nonce`) is no longer its own. `Some(Err(_))`
/// refuses it, once that intent is cleared.
pub(super) fn vet_card_focus(
    agent: Option<&CardAgent>,
    pane: &PaneId,
    nonce: FocusNonce,
    runtime: &crate::RuntimePaths,
    peers: Option<&FocusPeers>,
    ask: impl FnOnce(&CardAgent) -> Result<CardHosting, String>,
) -> Option<Result<bool, FocusFailure>> {
    let agent = agent?;
    if let Some(peers) = peers {
        peers.trace_stage("hosting_read");
    }
    let answer = ask(agent);
    if peers.is_some_and(|peers| !peers.is_current()) {
        if let Some(peers) = peers {
            peers.trace_stage("read_superseded");
        }
        return Some(Ok(false));
    }
    let reason = match answer {
        Ok(CardHosting::Unheld) => return None,
        Ok(CardHosting::Pane(hosted)) if hosted == *pane => return None,
        Ok(CardHosting::Pane(hosted)) => format!("the agent is in pane {hosted} now"),
        Ok(CardHosting::Nowhere) => "no pane hosts the agent now".to_owned(),
        Err(error) => error,
    };
    if !crate::mux::focus_anchor::clear_matching(runtime, nonce) {
        return Some(Ok(false));
    }
    Some(Err(FocusFailure::Refused(reason)))
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
/// equivalent), which must never block the loop. The pane is focused directly, with no
/// `rimz pane focus` child; a card focus (`agent` set) first takes one hosting read
/// after its last wait, outside the click-ordered gate and the focus-anchor lock, and is
/// refused unless the read puts the agent in the pane or answers no stored agent.
/// The shared intent helper takes a fresh attached-client sample at request
/// time and serializes concurrent focus actions. An automatic
/// Zellij repair re-samples a mismatching fence until its strand deadline.
/// Errors return to the attachment for selection rollback and a recovered
/// health notice. A superseded dispatch releases its input without a failure alert.
/// A cross-tab user request pushes the stored anchor to the host attachments that own
/// its target in the host's fold and waits for the destination's frame until click + 500 ms
/// before mux dispatch; request work and any stale tmux fit spend that same budget, without
/// resetting it. The source arms its click-ordered lease before the input paint. Attachments
/// offered no push go on with room work and user input; the destination consumes the
/// priority push first. Checkpoints fence older
/// requests before store, after fitting, and under the focus-anchor lock before
/// each mux dispatch attempt. The lease is
/// released before error rollback, on return, and on unwinding. Same-tab
/// requests keep a lease without pushing or waiting; automatic repairs have none. The durable
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
    input: Option<FocusNonce>,
    agent: Option<CardAgent>,
) {
    let session_name = session_name.to_owned();
    let input_at_ms = input_at_ms(clock, clicked, input);
    let requested_at_ms = crate::utils::time::unix_now_ms();
    clock.focus(|| {
        std::thread::spawn(move || {
            crate::harness::hibernate::spawn_wake(&runtime, &pane_id, requested_at_ms);
            let backend = crate::mux::backend_for(pane_id.mux());
            let repair_generation = repair.as_ref().map(|repair| repair.generation);
            let request = || request_focus_until(
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
                            input_at_ms,
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
            let requested = match &peers {
                Some(peers) => peers.request(request),
                None => Some(request()),
            };
            let Some(FocusRequestOutcome { requested, observed, attempts }) = requested else {
                wake_superseded_input(wake.as_ref(), input);
                return;
            };
            let dispatch = |nonce| {
                let dispatch = || {
                    crate::mux::focus_anchor::dispatch_action_if_current(
                        backend.as_ref(),
                        &runtime,
                        &session_name,
                        &pane_id,
                        nonce,
                        Default::default(),
                        || peers.as_ref().is_none_or(FocusPeers::is_current),
                    )
                    .map_err(FocusFailure::from)
                };
                let vet = || {
                    vet_card_focus(agent.as_ref(), &pane_id, nonce, &runtime, peers.as_ref(), |agent| {
                        ask_hosting(&runtime, &session_name, &pane_id, agent)
                    })
                };
                if let Some(peers) = &peers
                    && let Some(anchor) =
                        crate::mux::focus_anchor::load(&runtime).filter(|anchor| anchor.nonce == nonce)
                {
                    peers.dispatch(anchor, clicked, vet, dispatch).unwrap_or(Ok(false))
                } else if let Some(peers) = &peers {
                    // Another intent replaced this one: the dispatch under the anchor lock
                    // finds it gone and ends superseded, so no read is due.
                    peers.trace_stage("anchor_changed");
                    let result = peers.request(dispatch).unwrap_or(Ok(false));
                    peers.release();
                    result
                } else {
                    vet().unwrap_or_else(dispatch)
                }
            };
            let outcome = finish_focus_request(
                &pane_id,
                wake.as_ref(),
                requested,
                peers.as_ref(),
                input,
                dispatch,
            );
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

/// One hosting read of the room for `agent`, the agent a focus of `pane` is aimed at: the
/// room's existing store is opened for this read alone, and dropped with it.
fn ask_hosting(
    runtime: &crate::RuntimePaths,
    session_name: &str,
    pane: &PaneId,
    agent: &CardAgent,
) -> Result<CardHosting, String> {
    let store = crate::StatePaths::for_workspace(runtime.workspace_id.clone())
        .ok()
        .and_then(|state| crate::Store::open_existing(state, runtime.clone()))
        .ok_or_else(|| "the room's store could not be opened".to_owned())?;
    let read = crate::sidebar::produce::hosting::read_hosting_in(
        &runtime.workspace_id,
        session_name,
        &store,
        Some(pane.mux()),
    )
    .map_err(|error| error.to_string())?;
    let answer = read.agent_hosting_of_session(&agent.kind, &agent.session);
    Ok(match (answer.holder, answer.hosting) {
        (None, _) => CardHosting::Unheld,
        (Some(_), crate::sidebar::produce::hosting::Hosting::Pane { pane_id, .. }) => {
            CardHosting::Pane(pane_id)
        }
        (Some(_), crate::sidebar::produce::hosting::Hosting::Nowhere) => CardHosting::Nowhere,
    })
}

/// The wall-clock instant of the keypress or click behind a user-input focus
/// request, which pending-pick release compares with; repairs carry none.
pub(super) fn input_at_ms(
    clock: &super::clock::LoopClock,
    clicked: Instant,
    input: Option<FocusNonce>,
) -> Option<u64> {
    input.map(|_| clock.timestamp_at(clicked).as_millisecond().max(0) as u64)
}

pub(super) fn finish_focus_request(
    pane: &PaneId,
    wake: Option<&SyncSender<Wakeup>>,
    requested: Option<std::result::Result<FocusNonce, FocusActionError>>,
    peers: Option<&FocusPeers>,
    input: Option<FocusNonce>,
    dispatch: impl FnOnce(FocusNonce) -> std::result::Result<bool, FocusFailure>,
) -> (Option<FocusNonce>, &'static str, Option<String>) {
    let (nonce, dispatched) = match requested {
        Some(Ok(nonce)) => (Some(nonce), dispatch(nonce)),
        Some(Err(error)) => (None, Err(error.into())),
        None => {
            if let Some(peers) = peers {
                peers.release();
            }
            return (
                None,
                "failed",
                Some("strand TTL expired before focus request".into()),
            );
        }
    };
    if let Some(peers) = peers {
        peers.release();
        if nonce.is_none() {
            peers.trace_stage("request_failed");
        }
    }
    match dispatched {
        Ok(true) => {
            if let (Some(wake), Some(input), Some(nonce)) = (wake, input, nonce) {
                let _ = wake.send(Wakeup::FocusDispatched { input, nonce });
            }
            (nonce, "accepted_unconfirmed", None)
        }
        Ok(false) => {
            wake_superseded_input(wake, input);
            (
                nonce,
                "failed",
                Some("superseded before focus dispatch".into()),
            )
        }
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

fn wake_superseded_input(wake: Option<&SyncSender<Wakeup>>, input: Option<FocusNonce>) {
    if let (Some(wake), Some(input)) = (wake, input) {
        let _ = wake.send(Wakeup::FocusSuperseded { input });
    }
}
