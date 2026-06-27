//! Sidebar frame-stream observer.
//!
//! Every renderer feeds the observer one compact signature per committed frame.
//! The pure detectors run in-process; the writer thread handles cooldown,
//! elder-only real-world checks, and emission into the typed diagnostics
//! channel ([`crate::diag`]). The durable record vocabulary lives in
//! [`crate::schema::diag`].

mod detect;
mod sig;
pub mod writer;

pub use crate::schema::diag::{AggregateKey, AnomalyKind, FrameStamp, ObserveRole, WatchedField};
pub use detect::Observer;
pub use sig::{
    AggregateSig, EventsSig, FrameSig, GroupSig, OwnViewSig, RosterRowSig, RosterSig, RowSig,
    StatusCountSig, WatchedValues, extract_sig,
};

const EVIDENCE_LIMIT: usize = 32;

#[derive(Clone, Debug)]
pub enum ObserveMsg {
    Anomaly(Box<AnomalyDraft>),
    Roster(RosterSig),
}

#[derive(Clone, Debug)]
pub struct AnomalyDraft {
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
    pub fn from_sig(sig: &FrameSig, kind: AnomalyKind, window_ms: Option<u64>) -> Self {
        Self {
            at_ms: sig.at_ms,
            kind,
            window_ms,
            frame: frame_stamp_from_sig(sig),
            events_recent: sig.events.clone(),
            gate_reject_streak: sig.gate_reject_streak,
            health_failure_streak: sig.health_failure_streak,
            dropped_msgs: 0,
        }
    }

    pub fn from_roster(at_ms: u64, roster: &RosterSig, kind: AnomalyKind) -> Self {
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
        agents: sig.rows.iter().filter(|row| row.is_agent).count(),
        processes: sig.rows.iter().filter(|row| !row.is_agent).count(),
        pulled_rows: Some(sig.pulled_rows),
        pulled_panes_produced_at_ms: sig.pulled_panes_produced_at_ms,
    }
}

fn frame_stamp_from_roster(roster: &RosterSig) -> FrameStamp {
    FrameStamp {
        produced_at_ms: roster.panes_produced_at_ms,
        rows: roster.rows.len(),
        agents: roster.rows.iter().filter(|row| row.is_agent).count(),
        processes: roster.rows.iter().filter(|row| !row.is_agent).count(),
        pulled_rows: None,
        pulled_panes_produced_at_ms: None,
    }
}

fn cap_vec<T>(values: impl IntoIterator<Item = T>) -> Vec<T> {
    values.into_iter().take(EVIDENCE_LIMIT).collect()
}
