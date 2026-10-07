//! Parent-lifecycle watchdog for pane-backed supervised subagents.
//!
//! End stamps are authoritative only after every parent launch row ends.
//!
//! Pane presence is a latency signal, not authority.
//!
//! Pane loss cancels only after repeated authoritative mux reads and a final reconfirmation.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::agents::{AgentState, LifecycleSignal};
use crate::disk::paths::StatePaths;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::mux::{PaneListOptions, PaneReadConsistency};
use crate::store::event::{EventEnvelope, EventKind};
use crate::store::event_log::LogExtent;
use crate::store::follow::LaunchTail;
use serde::{Deserialize, Serialize};

const PROBE_INTERVAL: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PANE_GONE_STRIKES: u8 = 3;
const RECONFIRM_DELAY: Duration = Duration::from_millis(500);
const RECEIPT_POLL: Duration = Duration::from_secs(1);
#[cfg(any(test, feature = "testkit"))]
const TEST_PROBE_INTERVAL_MS_ENV: &str = "RIMZ_TEST_SUBAGENT_PARENT_PROBE_INTERVAL_MS";
#[cfg(feature = "testkit")]
const TEST_WATCH_ENV: &str = "RIMZ_TEST_SUBAGENT_PARENT_WATCH";

/// The parent launch's members and cursor, captured before startup projection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WatchdogSeed {
    pub child_kind: AgentKind,
    pub child_launch_id: AgentSessionId,
    pub parent_kind: AgentKind,
    pub parent_refs: Vec<AgentSessionId>,
    pub members: BTreeMap<AgentSessionId, bool>,
    pub parent_pane: Option<PaneId>,
    pub child_pane: Option<PaneId>,
    pub session_name: String,
    pub cursor: LogExtent,
}

/// Authoritative answer from the one-shot parent probe.
pub enum ProbeConfirm {
    Ended,
    Alive(Box<WatchdogSeed>),
    Unknown,
}

/// Resolve a parent launch without doing any I/O.
pub fn seed(
    agents: &[AgentState],
    child_kind: AgentKind,
    child_launch_id: AgentSessionId,
    child_pane: Option<PaneId>,
    session_name: String,
    cursor: LogExtent,
) -> Option<WatchdogSeed> {
    let (parent, child) = resolve_parent_and_child(agents, &child_kind, &child_launch_id)?;
    let mut parent_refs = vec![
        parent
            .launch_id
            .as_ref()
            .unwrap_or(&parent.agent_id)
            .clone(),
    ];
    if let Some(alias) = &child.parent_agent_id
        && !parent_refs.contains(alias)
    {
        parent_refs.push(alias.clone());
    }
    let members = agents
        .iter()
        .filter(|agent| {
            agent.kind == parent.kind
                && !agent.is_provider_subagent()
                && (parent_refs.contains(&agent.agent_id)
                    || agent
                        .launch_id
                        .as_ref()
                        .is_some_and(|id| parent_refs.contains(id)))
        })
        .map(|agent| (agent.agent_id.clone(), agent.ended_at.is_some()))
        .collect();
    let child_pane = child_pane.or_else(|| child.pane.as_ref().map(|pane| pane.pane_id.clone()));
    let parent_pane = parent
        .pane
        .as_ref()
        .map(|pane| &pane.pane_id)
        .filter(|pane| Some(*pane) != child_pane.as_ref() && owner_matches_agent(parent))
        .cloned();
    Some(WatchdogSeed {
        parent_kind: parent.kind.clone(),
        parent_refs,
        members,
        parent_pane,
        child_kind,
        child_launch_id,
        child_pane,
        session_name,
        cursor,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParentProbe {
    Ended,
    Present(u64),
    Absent(u64),
    Unknown,
}

type ConfirmParent = dyn Fn(&WatchdogSeed, bool) -> ProbeConfirm + Send;

/// Fold-free watchdog for one pane-backed child.
pub struct ParentWatchdog {
    seed: WatchdogSeed,
    tail: LaunchTail,
    paths: StatePaths,
    confirm: Box<ConfirmParent>,
    changed: Arc<AtomicBool>,
    receipt_waiting: Arc<AtomicBool>,
    next_probe: Instant,
    next_tail: Instant,
    tail_uncertain: bool,
    strikes: u8,
    last_observed_at_ms: Option<u64>,
}

/// Non-blocking parent-death and receipt-trigger signals for the supervisor.
pub struct ParentWatch {
    ended: Arc<AtomicBool>,
    changed: Arc<AtomicBool>,
    receipt_waiting: Arc<AtomicBool>,
    worker: thread::Thread,
}

impl ParentWatch {
    /// Wake the tail reader while a terminal child awaits its parent's receipt.
    pub fn set_receipt_waiting(&self, waiting: bool) {
        if self.receipt_waiting.swap(waiting, Ordering::AcqRel) != waiting {
            self.worker.unpark();
        }
    }

    pub fn parent_ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }
    pub fn take_parent_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }
}

impl ParentWatchdog {
    /// Resume the captured cursor, delegating every death decision to `confirm`.
    pub fn from_seed(
        seed: WatchdogSeed,
        paths: StatePaths,
        confirm: impl Fn(&WatchdogSeed, bool) -> ProbeConfirm + Send + 'static,
    ) -> Self {
        let tail_uncertain = seed.parent_refs.is_empty();
        let next_probe = Instant::now()
            + if tail_uncertain {
                Duration::ZERO
            } else {
                probe_interval()
            };
        Self {
            tail: LaunchTail::from_cursor(paths.clone(), seed.cursor),
            paths,
            seed,
            confirm: Box::new(confirm),
            changed: Arc::new(AtomicBool::new(false)),
            receipt_waiting: Arc::new(AtomicBool::new(false)),
            next_probe,
            next_tail: next_probe,
            tail_uncertain,
            strikes: 0,
            last_observed_at_ms: None,
        }
    }

    pub fn start(mut self) -> ParentWatch {
        let ended = Arc::new(AtomicBool::new(false));
        let signal = ended.clone();
        let changed = self.changed.clone();
        let receipt_waiting = self.receipt_waiting.clone();
        let worker = thread::spawn(move || {
            let mut was_waiting = false;
            loop {
                let now = Instant::now();
                let waiting = self.receipt_waiting.load(Ordering::Acquire);
                if waiting && !was_waiting {
                    self.next_tail = now;
                } else if !waiting {
                    self.next_tail = self.next_probe;
                }
                was_waiting = waiting;
                if self.probe_if_due(now) {
                    signal.store(true, Ordering::Release);
                    break;
                }
                let deadline = self.next_probe.min(self.next_tail);
                thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
            }
        });
        ParentWatch {
            ended,
            changed,
            receipt_waiting,
            worker: worker.thread().clone(),
        }
    }

    fn probe_if_due(&mut self, now: Instant) -> bool {
        if now < self.next_tail && now < self.next_probe {
            return false;
        }
        #[cfg(feature = "testkit")]
        if std::env::var(TEST_WATCH_ENV).ok().as_deref() == Some("disabled") {
            self.next_probe = now + probe_interval();
            self.next_tail = self.next_probe;
            return false;
        }
        let seed = &mut self.seed;
        let changed = &self.changed;
        match self.tail.poll(|event| {
            if observe_frame(seed, event) {
                changed.store(true, Ordering::Release);
            }
        }) {
            Ok(warnings) => {
                self.tail_uncertain |= !warnings.is_empty();
                for warning in warnings {
                    tracing::debug!(%warning, "parent watchdog event tail");
                }
            }
            Err(error) => {
                tracing::debug!(%error, "could not read parent watchdog event tail");
                self.tail_uncertain = true;
            }
        }
        self.next_tail = if self.receipt_waiting.load(Ordering::Acquire) {
            now + RECEIPT_POLL
        } else {
            now + probe_interval()
        };
        if now < self.next_probe {
            return false;
        }
        self.next_probe = now + probe_interval();
        let ended = !self.seed.members.is_empty() && self.seed.members.values().all(|ended| *ended);
        let probe = if ended || self.tail_uncertain {
            ParentProbe::Ended
        } else {
            pane_probe(&self.seed, &self.paths.workspace_id)
        };
        if !self.observe(probe) {
            return false;
        }
        // Only pane-strike nominations wait for reconfirmation.
        if probe != ParentProbe::Ended {
            thread::sleep(RECONFIRM_DELAY);
        }
        self.next_probe = self.next_probe.max(Instant::now() + probe_interval());
        match (self.confirm)(&self.seed, probe != ParentProbe::Ended) {
            ProbeConfirm::Ended => true,
            ProbeConfirm::Alive(seed) => {
                self.tail = LaunchTail::from_cursor(self.paths.clone(), seed.cursor);
                self.seed = *seed;
                self.tail_uncertain = false;
                self.strikes = 0;
                self.last_observed_at_ms = None;
                false
            }
            ProbeConfirm::Unknown => false,
        }
    }

    fn observe(&mut self, probe: ParentProbe) -> bool {
        match probe {
            ParentProbe::Ended => return true,
            ParentProbe::Present(observed_at_ms) => {
                if self.last_observed_at_ms != Some(observed_at_ms) {
                    self.strikes = 0;
                    self.last_observed_at_ms = Some(observed_at_ms);
                }
            }
            ParentProbe::Absent(observed_at_ms) => {
                if self.last_observed_at_ms != Some(observed_at_ms) {
                    self.strikes = self.strikes.saturating_add(1);
                    self.last_observed_at_ms = Some(observed_at_ms);
                }
            }
            ParentProbe::Unknown => {}
        }
        self.strikes >= PANE_GONE_STRIKES
    }
}

fn observe_frame(seed: &mut WatchdogSeed, event: EventEnvelope) -> bool {
    if event.source != seed.parent_kind.as_str() {
        return false;
    }
    match event.kind() {
        EventKind::AgentLaunch(payload)
            if seed.parent_refs.contains(&payload.agent_id)
                || payload
                    .launch_id
                    .as_ref()
                    .is_some_and(|id| seed.parent_refs.contains(id)) =>
        {
            seed.members.insert(payload.agent_id, false);
        }
        EventKind::AgentAttach(payload)
            if seed.members.contains_key(&payload.agent_id)
                || seed.parent_refs.contains(&payload.agent_id)
                || payload
                    .launch_id
                    .as_ref()
                    .is_some_and(|id| seed.parent_refs.contains(id)) =>
        {
            seed.members.entry(payload.agent_id).or_insert(false);
            if Some(&payload.pane_id) != seed.child_pane.as_ref() {
                seed.parent_pane = Some(payload.pane_id);
            }
        }
        EventKind::AgentLifecycle(payload) => {
            let Some(ended) = payload
                .observation
                .agent_id
                .as_ref()
                .and_then(|id| seed.members.get_mut(id))
            else {
                return false;
            };
            *ended = matches!(payload.observation.signal, LifecycleSignal::Ended);
            return true;
        }
        _ => {}
    }
    false
}

fn pane_probe(seed: &WatchdogSeed, workspace_id: &WorkspaceId) -> ParentProbe {
    let Some(parent_pane) = &seed.parent_pane else {
        return ParentProbe::Unknown;
    };
    let backend = crate::mux::backend_for(parent_pane.mux());
    if let Some(roster) = backend.cached_pane_roster(&seed.session_name, workspace_id)
        && roster.pane_ids.contains(parent_pane)
    {
        return ParentProbe::Present(roster.observed_at_ms);
    }
    match backend.list_panes(PaneListOptions {
        session_name: Some(seed.session_name.clone()),
        workspace_id: Some(workspace_id.clone()),
        consistency: PaneReadConsistency::RequireAuthoritative,
        command_timeout: Some(PROBE_TIMEOUT),
        ..PaneListOptions::default()
    }) {
        Ok(listing)
            if listing
                .panes
                .iter()
                .any(|pane| pane.pane_id == *parent_pane) =>
        {
            ParentProbe::Present(listing.observed_at_ms)
        }
        Ok(listing) => ParentProbe::Absent(listing.observed_at_ms),
        Err(error) => {
            tracing::debug!(%error, "subagent parent watchdog authoritative pane probe failed");
            ParentProbe::Unknown
        }
    }
}

fn owner_matches_agent(agent: &AgentState) -> bool {
    agent
        .runtime_owner
        .as_ref()
        .is_none_or(|owner| owner.subject_id == agent.agent_id.as_str())
}

fn resolve_parent_and_child<'a>(
    agents: &'a [AgentState],
    child_kind: &AgentKind,
    child_launch_id: &AgentSessionId,
) -> Option<(&'a AgentState, &'a AgentState)> {
    let child = crate::address::launch_row(agents, child_kind, child_launch_id)?;
    let parent = crate::address::launched_parent(agents, child)?;
    Some((parent, child))
}

fn probe_interval() -> Duration {
    #[cfg(any(test, feature = "testkit"))]
    if let Some(ms) = std::env::var(TEST_PROBE_INTERVAL_MS_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        return Duration::from_millis(ms.max(1));
    }
    PROBE_INTERVAL
}

#[cfg(test)]
mod tests;
