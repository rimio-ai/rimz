//! Typed live pane topology published by the sidebar producer.
//!
//! The mux seam remains a flat [`PaneRef`](crate::pane::PaneRef) list because
//! non-sidebar callers route by pane. The sidebar producer lifts that list into
//! tabs/windows, keeps process state as one record, and publishes the topology
//! as cache-class `snapshot.json`. The frame admits every rendered sidebar
//! card; ledger, sidecars, and realtime events only enrich cards whose pane is
//! present here.

use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::diag::record::DiagEvent;
use crate::ids::{AgentKind, AgentSessionId, PaneId, ViewId, ViewKind};
use crate::ledger::snapshot::{PresenceSample, SidebarOwnView};
use crate::pane::{ElevatedAgent, PaneRef};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneFrame {
    pub produced_at_ms: u64,
    /// When the pane source observed the topology. A zero value can appear
    /// only when an old build's frame is read once across a reload; cache reads
    /// normalize it to `produced_at_ms`.
    #[serde(default)]
    pub observed_at_ms: u64,
    /// Build id of the producer that assembled this frame
    /// ([`crate::build_id`]); absent when the running image is unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub session_name: String,
    pub tabs: Vec<TabFrame>,
    /// Panes retained from the prior published frame because the latest pane
    /// source omitted them while process liveness still proved them alive.
    /// Empty on healthy frames.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried_panes: Vec<CarriedPane>,
    /// Panes attached clients are currently viewing, one per client. Assembly
    /// uses this as focus-register input before publishing it for snapshot
    /// enrichment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub viewed_panes: Vec<PaneId>,
    /// Session-global latest focused pane resolved from client views, prior
    /// frame state, and backend raw focus marks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane: Option<PaneId>,
    /// Producer-sampled session presence. Absent on fallback paths that could
    /// not read the per-client mux state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<PresenceSample>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedPane {
    pub pane_id: PaneId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ticks: Option<u64>,
    pub carried_since_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabFrame {
    pub view_id: ViewId,
    pub kind: ViewKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub panes: Vec<PaneState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneState {
    pub pane_id: PaneId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_at_ms: Option<u64>,
    /// Non-advancing TTL anchor for a hosted-agent stamp restored from the
    /// prior frame after a transient process scan miss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_carry_since_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_floating: bool,
    pub current: PaneProcess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PaneProcess>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<u32>,
    #[serde(default, skip_serializing_if = "PaneMetrics::is_empty")]
    pub metrics: PaneMetrics,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneProcess {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_agent_kind: Option<AgentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_agent_process_start: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_session_id: Option<AgentSessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevated_agent: Option<ElevatedAgent>,
}

/// Producer-sampled resource figures for one pane's process tree —
/// display-only, written by the metrics cadence and projected onto process
/// rows. The CPU/memory/IO figures publish together once two same-tenant
/// `/proc` samples complete them, never as a partial set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneMetrics {
    /// The sampler's stuck verdict only — `Some(Stuck)` when `/proc` reported a
    /// zombie or repeated uninterruptible sleep, else `None`. Idle-vs-busy is
    /// never carried here; the fold classifies it from the pane's program
    /// (`ledger::snapshot::process`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_state: Option<crate::ProcessState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_kb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_bps: Option<u64>,
}

impl PaneMetrics {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl PaneFrame {
    pub fn to_pane_refs(&self) -> Vec<PaneRef> {
        self.tabs
            .iter()
            .flat_map(|tab| {
                tab.panes
                    .iter()
                    .map(move |pane| self.pane_ref_for_state(tab, pane))
            })
            .collect()
    }

    pub fn pane_metrics(&self) -> impl Iterator<Item = (PaneId, PaneMetrics)> + '_ {
        self.pane_states()
            .map(|pane| (pane.pane_id.clone(), pane.metrics))
    }

    pub fn pane_states(&self) -> impl Iterator<Item = &PaneState> {
        self.tabs.iter().flat_map(|tab| tab.panes.iter())
    }

    pub fn pane_states_mut(&mut self) -> impl Iterator<Item = &mut PaneState> {
        self.tabs.iter_mut().flat_map(|tab| tab.panes.iter_mut())
    }

    pub fn rotate_against_prior(&mut self, prior: &PaneFrame) {
        let prior_by_pane: HashMap<PaneId, &PaneState> = prior
            .pane_states()
            .map(|pane| (pane.pane_id.clone(), pane))
            .collect();
        for pane in self.pane_states_mut() {
            if let Some(prior) = prior_by_pane.get(&pane.pane_id) {
                pane.rotate_on_process_change(prior);
            }
        }
    }

    fn pane_ref_for_state(&self, tab: &TabFrame, pane: &PaneState) -> PaneRef {
        PaneRef {
            pane_id: pane.pane_id.clone(),
            session_name: self.session_name.clone(),
            view_id: Some(tab.view_id.to_string()),
            view_kind: Some(tab.kind),
            view_name: tab.name.clone(),
            is_focused: self.focused_pane.as_ref() == Some(&pane.pane_id)
                || self.viewed_panes.contains(&pane.pane_id),
            is_floating: pane.is_floating,
            command: pane.current.command.clone(),
            spawn_command: pane.current.spawn_command.clone(),
            cwd: pane.current.cwd.clone(),
            pane_pid: pane.current.pid,
            pane_process_start: pane.current.started_at,
            hosted_agent_kind: pane.current.hosted_agent_kind.clone(),
            hosted_agent_process_start: pane.current.hosted_agent_process_start,
            resumed_session_id: pane.current.resumed_session_id.clone(),
            elevated_agent: pane.current.elevated_agent.clone(),
            first_seen_at_ms: pane.first_seen_at_ms,
        }
    }
}

impl PaneState {
    /// Join this fresh pane state to the prior frame's state for the same pane
    /// id. A changed spawn command, pid, or process start is a new tenant: the
    /// prior current process rotates to `previous` and the fresh record stands
    /// clean. A stable identity repairs raced-null mux fields (`spawn_command`,
    /// `cwd`, `started_at`) from the prior read and carries `previous` along.
    /// Foreground command repair is narrower: idle commands and agent hosts may
    /// be restored freely, but active commands require a known same pane-root
    /// pid. Zellij's fresh list-panes path usually has no pid here, so an
    /// exited foreground task does not keep rendering as busy when the shell
    /// returns and the mux reports no fresh command for a tick. tmux's pid is
    /// the stable pane root, so this still preserves tmux raced-null repair.
    ///
    /// `current.pid` is never backfilled here: on Zellij the pid is a
    /// metrics-layer derivation, and only that layer's `starttime` pid-reuse
    /// guard may restore it ([`super::produce`]'s metrics module) — a rotation
    /// carry would republish a stale binding without ever revalidating it.
    pub fn rotate_on_process_change(&mut self, prior: &PaneState) {
        let spawn_changed = match (
            self.current.spawn_command.as_deref(),
            prior.current.spawn_command.as_deref(),
        ) {
            (Some(fresh), Some(previous)) => fresh != previous,
            _ => false,
        };
        let pid_changed = match (self.current.pid, prior.current.pid) {
            (Some(fresh), Some(previous)) => fresh != previous,
            _ => false,
        };
        let start_changed = match (self.current.started_at, prior.current.started_at) {
            (Some(fresh), Some(previous)) => fresh != previous,
            _ => false,
        };
        if spawn_changed || pid_changed || start_changed {
            self.previous = Some(prior.current.clone());
            return;
        }

        self.first_seen_at_ms = prior.first_seen_at_ms;
        self.previous = prior.previous.clone();
        if self.current.command.is_none() {
            let prior_is_idle = prior
                .current
                .command
                .as_deref()
                .is_none_or(|command| !crate::ledger::snapshot::process_is_active(command));
            let same_known_pid = matches!(
                (self.current.pid, prior.current.pid),
                (Some(fresh), Some(previous)) if fresh == previous
            );
            if prior_is_idle || same_known_pid {
                self.current.command = prior.current.command.clone();
            }
        }
        if self.current.spawn_command.is_none() {
            self.current.spawn_command = prior.current.spawn_command.clone();
        }
        if self.current.cwd.is_none() {
            self.current.cwd = prior.current.cwd.clone();
        }
        if self.current.started_at.is_none() {
            self.current.started_at = prior.current.started_at;
        }
        if self.current.resumed_session_id.is_none() {
            self.current.resumed_session_id = prior.current.resumed_session_id.clone();
        }
        if self.current.pid.is_some() && self.current.elevated_agent.is_none() {
            self.current.elevated_agent = prior.current.elevated_agent.clone();
        }
    }
}

// The constructor lives here, beside the frame it consumes, rather than with
// the `SidebarOwnView` type in `ledger/snapshot` — the ledger read path stays
// free of sidebar imports and only the sidebar fold derives an own-view.
impl SidebarOwnView {
    pub fn from_frame(own: &PaneId, frame: &PaneFrame) -> Option<Self> {
        let tab = frame
            .tabs
            .iter()
            .find(|tab| tab.panes.iter().any(|pane| pane.pane_id == *own))?;
        let siblings = tab
            .panes
            .iter()
            .filter(|pane| pane.pane_id != *own)
            .collect::<Vec<_>>();
        let non_sidebar_siblings = siblings
            .iter()
            .copied()
            .filter(|pane| !pane_is_sidebar_chrome(pane))
            .collect::<Vec<_>>();
        let working_pane_ids = non_sidebar_siblings
            .iter()
            .map(|pane| pane.pane_id.clone())
            .collect::<Vec<_>>();
        let own_view_is_daemon = !non_sidebar_siblings.is_empty()
            && non_sidebar_siblings.iter().all(|pane| {
                crate::remote_control::pane_is_host(&frame.pane_ref_for_state(tab, pane))
            });
        Some(Self {
            sibling_count: siblings.len(),
            working_pane_ids,
            own_view_is_daemon,
        })
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub struct FrameInputs<'a> {
    pub panes: Vec<PaneRef>,
    pub produced_at_ms: u64,
    pub observed_at_ms: u64,
    pub session_name: String,
    pub client_viewed: &'a [PaneId],
    pub prior: Option<&'a PaneFrame>,
}

pub fn assemble_frame(
    panes: Vec<PaneRef>,
    produced_at_ms: u64,
    session_name: impl Into<String>,
) -> PaneFrame {
    assemble_frame_with_diagnostics(panes, produced_at_ms, session_name).0
}

pub fn assemble_frame_with_diagnostics(
    panes: Vec<PaneRef>,
    produced_at_ms: u64,
    session_name: impl Into<String>,
) -> (PaneFrame, Vec<DiagEvent>) {
    assemble_frame_from_inputs(FrameInputs {
        panes,
        produced_at_ms,
        observed_at_ms: produced_at_ms,
        session_name: session_name.into(),
        client_viewed: &[],
        prior: None,
    })
}

pub fn assemble_frame_from_inputs(inputs: FrameInputs<'_>) -> (PaneFrame, Vec<DiagEvent>) {
    let FrameInputs {
        panes,
        produced_at_ms,
        observed_at_ms,
        session_name,
        client_viewed,
        prior,
    } = inputs;
    let mut tabs: BTreeMap<ViewId, TabFrame> = BTreeMap::new();
    let mut raw_focused = Vec::new();
    let mut seen_panes = HashSet::new();
    let mut diagnostics = Vec::new();
    for pane in panes {
        if !seen_panes.insert(pane.pane_id.clone()) {
            diagnostics.push(DiagEvent::DuplicatePaneId {
                pane_id: pane.pane_id,
            });
            continue;
        }
        let view_id = pane
            .view_id
            .clone()
            .map(ViewId::new_unchecked)
            .unwrap_or_else(|| ViewId::new_unchecked(format!("pane:{}", pane.pane_id)));
        let kind = pane
            .view_kind
            .unwrap_or_else(|| crate::mux::view_kind(pane.pane_id.mux()));
        let tab = tabs.entry(view_id.clone()).or_insert_with(|| TabFrame {
            view_id,
            kind,
            name: pane.view_name.clone(),
            panes: Vec::new(),
        });
        if tab.name.is_none() {
            tab.name = pane.view_name.clone();
        }
        if pane.is_focused {
            raw_focused.push(pane.pane_id.clone());
        }
        let resumed_session_id = pane.resumed_session_id.or_else(|| {
            pane.command
                .as_deref()
                .and_then(crate::agents::codex::codex_resumed_session_id_from_cmdline)
        });
        tab.panes.push(PaneState {
            pane_id: pane.pane_id,
            first_seen_at_ms: pane.first_seen_at_ms,
            hosted_carry_since_ms: None,
            is_floating: pane.is_floating,
            current: PaneProcess {
                pid: pane.pane_pid,
                command: pane.command,
                spawn_command: pane.spawn_command,
                cwd: pane.cwd,
                started_at: pane.pane_process_start,
                hosted_agent_kind: pane.hosted_agent_kind,
                hosted_agent_process_start: pane.hosted_agent_process_start,
                resumed_session_id,
                elevated_agent: pane.elevated_agent,
            },
            previous: None,
            children: Vec::new(),
            metrics: PaneMetrics::default(),
        });
    }
    let live = tabs
        .values()
        .flat_map(|tab| tab.panes.iter().map(|pane| pane.pane_id.clone()))
        .collect::<HashSet<_>>();
    let focused_pane = resolve_session_focus(
        prior.and_then(|frame| frame.focused_pane.as_ref()),
        client_viewed,
        &raw_focused,
        &live,
    );
    (
        PaneFrame {
            produced_at_ms,
            observed_at_ms,
            build: crate::build_id::current().map(str::to_owned),
            session_name,
            tabs: tabs.into_values().collect(),
            carried_panes: Vec::new(),
            viewed_panes: client_viewed.to_vec(),
            focused_pane,
            presence: None,
        },
        diagnostics,
    )
}

pub(crate) fn resolve_session_focus(
    prior: Option<&PaneId>,
    client_viewed: &[PaneId],
    raw_focused: &[PaneId],
    live: &HashSet<PaneId>,
) -> Option<PaneId> {
    let live_viewed = client_viewed
        .iter()
        .filter(|pane| live.contains(*pane))
        .collect::<Vec<_>>();
    match live_viewed.as_slice() {
        [pane] => return Some((*pane).clone()),
        panes if panes.len() > 1 => {
            if let Some(prior) = prior.filter(|prior| client_viewed.contains(prior)) {
                return Some(prior.clone());
            }
            return panes.first().map(|pane| (*pane).clone());
        }
        _ => {}
    }

    if let Some(prior) = prior.filter(|prior| live.contains(prior)) {
        return Some(prior.clone());
    }
    let live_raw = raw_focused
        .iter()
        .filter(|pane| live.contains(*pane))
        .collect::<Vec<_>>();
    match live_raw.as_slice() {
        [pane] => Some((*pane).clone()),
        _ => None,
    }
}

fn pane_is_sidebar_chrome(pane: &PaneState) -> bool {
    pane.current
        .command
        .as_deref()
        .is_some_and(crate::ledger::snapshot::command_is_sidebar_chrome)
}

#[cfg(test)]
mod tests;
