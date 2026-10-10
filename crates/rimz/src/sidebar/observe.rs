//! Sidebar frame-stream observer.
//!
//! The worker shares one room signature per publication; renderers observe only
//! their own committed view. The writer detects room-common anomalies once and runs the room-wide
//! real-world checks and emission into the typed diagnostics channel
//! ([`crate::diag`]), which owns the one rate limit. The durable record
//! vocabulary lives in [`crate::diag::record`].

mod detect;
mod sig;
pub(crate) mod writer;

use crate::diag::record::{AnomalyKind, FrameStamp, WatchedField};
use crate::ids::SidebarInstanceId;
use crate::sidebar::event_store::EventStore;
use crate::store::snapshot::SidebarSnapshot;
pub(crate) use detect::{Observer, OwnObserver};
#[cfg(test)]
pub(crate) use sig::take_extractions;
use sig::{EventsSig, RosterSig};
pub(crate) use sig::{FrameSig, OwnFrameSig, PulledFrameSig, extract_sig};
use std::sync::{Arc, Mutex, mpsc::SyncSender};

const EVIDENCE_LIMIT: usize = 32;

#[derive(Clone, Debug)]
pub(crate) enum ObserveMsg {
    #[cfg(test)]
    Anomaly(Box<AnomalyDraft>),
    #[cfg(test)]
    Roster(RosterSig),
    RoomFrame {
        sig: Arc<FrameSig>,
        renderer: SidebarInstanceId,
        dropped_msgs: u32,
    },
    OwnAnomaly {
        draft: Box<AnomalyDraft>,
        renderer: SidebarInstanceId,
    },
}

pub(crate) struct RoomObserver {
    tx: SyncSender<ObserveMsg>,
    pub(crate) events: Arc<Mutex<EventStore>>,
    dropped_msgs: u32,
}

impl RoomObserver {
    pub(crate) fn new(tx: SyncSender<ObserveMsg>) -> Self {
        Self {
            tx,
            events: Arc::default(),
            dropped_msgs: 0,
        }
    }

    pub(crate) fn extract(
        &mut self,
        snapshot: &SidebarSnapshot,
        renderer: SidebarInstanceId,
    ) -> Arc<FrameSig> {
        let now_ms = crate::utils::time::unix_now_ms();
        let events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sig = extract_sig(
            snapshot,
            &PulledFrameSig::from_snapshot(snapshot),
            &events,
            0,
            0,
            now_ms,
        );
        sig.own_view = None;
        let sig = Arc::new(sig);
        let dropped_msgs = std::mem::take(&mut self.dropped_msgs);
        if self
            .tx
            .try_send(ObserveMsg::RoomFrame {
                sig: sig.clone(),
                renderer,
                dropped_msgs,
            })
            .is_err()
        {
            self.dropped_msgs = dropped_msgs.saturating_add(1);
        }
        sig
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AnomalyDraft {
    pub at_ms: u64,
    pub kind: AnomalyKind,
    pub window_ms: Option<u64>,
    pub frame: FrameStamp,
    pub events_recent: EventsSig,
    pub gate_reject_streak: u32,
    pub health_failure_streak: u32,
    pub dropped_msgs: u32,
}

impl AnomalyDraft {
    fn from_sig(sig: &FrameSig, kind: AnomalyKind, window_ms: Option<u64>) -> Self {
        Self::from_sig_at_frame(sig, kind, window_ms, frame_stamp_from_sig(sig))
    }

    /// A draft stamped with the frame that caused the anomaly rather than the
    /// frame that revealed it. Detectors that fire on a later frame than the
    /// fault (a presence flap fires when the row returns) name the causal frame
    /// here, so the `produced_at_ms` join reaches the producer records for the
    /// same episode and every renderer's copy of one fault carries one stamp.
    fn from_sig_at_frame(
        sig: &FrameSig,
        kind: AnomalyKind,
        window_ms: Option<u64>,
        frame: FrameStamp,
    ) -> Self {
        Self {
            at_ms: sig.at_ms,
            kind,
            window_ms,
            frame,
            events_recent: sig.events.clone(),
            gate_reject_streak: sig.gate_reject_streak,
            health_failure_streak: sig.health_failure_streak,
            dropped_msgs: 0,
        }
    }

    fn from_roster(at_ms: u64, roster: &RosterSig, kind: AnomalyKind) -> Self {
        Self {
            at_ms,
            kind,
            window_ms: None,
            frame: frame_stamp_from_roster(roster),
            events_recent: EventsSig::default(),
            gate_reject_streak: 0,
            health_failure_streak: 0,
            dropped_msgs: 0,
        }
    }
}

fn frame_stamp_from_sig(sig: &FrameSig) -> FrameStamp {
    FrameStamp {
        produced_at_ms: sig.panes_produced_at_ms,
        rows: sig.rows.len(),
        agents: sig
            .rows
            .iter()
            .filter(|row| row.agent_kind.is_some())
            .count(),
        processes: sig
            .rows
            .iter()
            .filter(|row| row.agent_kind.is_none())
            .count(),
        pulled_rows: Some(sig.pulled_rows),
        pulled_panes_produced_at_ms: sig.pulled_panes_produced_at_ms,
    }
}

fn frame_stamp_from_roster(roster: &RosterSig) -> FrameStamp {
    FrameStamp {
        produced_at_ms: roster.panes_produced_at_ms,
        rows: roster.rows.len(),
        agents: roster
            .rows
            .iter()
            .filter(|row| row.agent_kind.is_some())
            .count(),
        processes: roster
            .rows
            .iter()
            .filter(|row| row.agent_kind.is_none())
            .count(),
        pulled_rows: None,
        pulled_panes_produced_at_ms: None,
    }
}

fn cap_vec<T>(values: impl IntoIterator<Item = T>) -> Vec<T> {
    values.into_iter().take(EVIDENCE_LIMIT).collect()
}
