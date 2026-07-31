//! Parent-lifecycle watchdog for pane-backed supervised subagents.
//!
//! Durable parent end stamps are authoritative. Pane presence is a latency
//! signal, so absence only becomes destructive after repeated authoritative
//! mux reads and one final reconfirmation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::agents::AgentState;
use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
use crate::mux::{PaneListOptions, PaneReadConsistency};

const PROBE_INTERVAL: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PANE_GONE_STRIKES: u8 = 3;
const RECONFIRM_DELAY: Duration = Duration::from_millis(500);
#[cfg(any(test, feature = "testkit"))]
const TEST_PROBE_INTERVAL_MS_ENV: &str = "RIMZ_TEST_SUBAGENT_PARENT_PROBE_INTERVAL_MS";
#[cfg(feature = "testkit")]
const TEST_WATCH_ENV: &str = "RIMZ_TEST_SUBAGENT_PARENT_WATCH";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParentProbe {
    Ended,
    Present(u64),
    Absent(u64),
    Unknown,
}

/// Re-resolving watchdog for one pane-backed child.
pub struct ParentWatchdog {
    store: crate::Store,
    child_kind: AgentKind,
    child_launch_id: AgentSessionId,
    child_pane: Option<crate::ids::PaneId>,
    session_name: String,
    workspace_id: WorkspaceId,
    parent_pane: Option<crate::ids::PaneId>,
    next_probe: Instant,
    strikes: u8,
    last_observed_at_ms: Option<u64>,
}

/// Non-blocking parent-death signal observed by the exec supervisor.
pub struct ParentWatch {
    ended: Arc<AtomicBool>,
}

impl ParentWatch {
    pub fn parent_ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }
}

impl ParentWatchdog {
    pub fn new(
        store: crate::Store,
        child_kind: AgentKind,
        child_launch_id: AgentSessionId,
        child_pane: Option<crate::ids::PaneId>,
        session_name: String,
    ) -> Self {
        let parent_pane = store
            .runtime_projection(crate::store::runtime::RuntimeScope::Audit)
            .ok()
            .and_then(|projection| {
                resolve_parent_and_child(&projection.agents, &child_kind, &child_launch_id)
                    .and_then(|(parent, child)| {
                        let parent_pane = parent.pane.as_ref().map(|pane| &pane.pane_id)?;
                        let child_pane = child_pane
                            .as_ref()
                            .or_else(|| child.pane.as_ref().map(|pane| &pane.pane_id));
                        (Some(parent_pane) != child_pane && owner_matches_agent(parent))
                            .then(|| parent_pane.clone())
                    })
            });
        Self {
            workspace_id: store.paths().workspace_id.clone(),
            store,
            child_kind,
            child_launch_id,
            child_pane,
            session_name,
            parent_pane,
            next_probe: Instant::now() + probe_interval(),
            strikes: 0,
            last_observed_at_ms: None,
        }
    }

    pub fn start(mut self) -> ParentWatch {
        let ended = Arc::new(AtomicBool::new(false));
        let signal = ended.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(self.next_probe.saturating_duration_since(Instant::now()));
                if self.probe_if_due(Instant::now()) {
                    signal.store(true, Ordering::Release);
                    break;
                }
            }
        });
        ParentWatch { ended }
    }

    /// Return true only when the parent has durably ended or pane absence has
    /// survived the debounce and a fresh authoritative confirmation.
    fn probe_if_due(&mut self, now: Instant) -> bool {
        if now < self.next_probe {
            return false;
        }
        self.next_probe = now + probe_interval();
        #[cfg(feature = "testkit")]
        if std::env::var(TEST_WATCH_ENV).ok().as_deref() == Some("disabled") {
            return false;
        }

        let probe = self.probe();
        if probe == ParentProbe::Ended {
            return true;
        }
        if !self.observe(probe) {
            return false;
        }

        thread::sleep(RECONFIRM_DELAY);
        matches!(self.probe(), ParentProbe::Ended | ParentProbe::Absent(_))
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

    fn probe(&mut self) -> ParentProbe {
        let projection = match self
            .store
            .runtime_projection(crate::store::runtime::RuntimeScope::Audit)
        {
            Ok(projection) => projection,
            Err(err) => {
                tracing::debug!(
                    child = %self.child_launch_id,
                    error = &err as &dyn std::error::Error,
                    "subagent parent watchdog could not read the runtime projection",
                );
                return ParentProbe::Unknown;
            }
        };
        let Some((parent, child)) =
            resolve_parent_and_child(&projection.agents, &self.child_kind, &self.child_launch_id)
        else {
            return ParentProbe::Unknown;
        };
        if parent.ended_at.is_some() {
            return ParentProbe::Ended;
        }
        let child_pane = self
            .child_pane
            .as_ref()
            .or_else(|| child.pane.as_ref().map(|pane| &pane.pane_id));
        if let Some(candidate) = parent.pane.as_ref().map(|pane| &pane.pane_id)
            && Some(candidate) != child_pane
            && owner_matches_agent(parent)
            && (self.parent_pane.is_none() || self.strikes == 0)
        {
            self.parent_pane = Some(candidate.clone());
        }
        let Some(parent_pane) = self.parent_pane.as_ref() else {
            return ParentProbe::Unknown;
        };
        let backend = crate::mux::backend_for(parent_pane.mux());
        if let Some(roster) = backend.cached_pane_roster(&self.session_name, &self.workspace_id)
            && roster.pane_ids.contains(parent_pane)
        {
            return ParentProbe::Present(roster.observed_at_ms);
        }
        match backend.list_panes(PaneListOptions {
            session_name: Some(self.session_name.clone()),
            workspace_id: Some(self.workspace_id.clone()),
            consistency: PaneReadConsistency::RequireAuthoritative,
            command_timeout: Some(PROBE_TIMEOUT),
            ..PaneListOptions::default()
        }) {
            Ok(listing)
                if listing
                    .panes
                    .iter()
                    .any(|item| item.pane_id == *parent_pane) =>
            {
                ParentProbe::Present(listing.observed_at_ms)
            }
            Ok(listing) => ParentProbe::Absent(listing.observed_at_ms),
            Err(err) => {
                tracing::debug!(
                    child = %self.child_launch_id,
                    parent = %parent.agent_id,
                    pane = %parent_pane,
                    error = &err as &dyn std::error::Error,
                    "subagent parent watchdog authoritative pane probe failed",
                );
                ParentProbe::Unknown
            }
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
    let child = agents.iter().find(|agent| {
        &agent.kind == child_kind
            && (agent.launch_id.as_ref() == Some(child_launch_id)
                || &agent.agent_id == child_launch_id)
    })?;
    let parent_id = child.parent_agent_id.as_ref()?;
    let parent = agents.iter().find(|agent| {
        (&agent.agent_id == parent_id || agent.launch_id.as_ref() == Some(parent_id))
            && child
                .parent_agent_kind
                .as_ref()
                .is_none_or(|kind| &agent.kind == kind)
    })?;
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
mod tests {
    use super::*;

    fn watchdog() -> ParentWatchdog {
        let workspace_id =
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/parent-watch"));
        let state = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(workspace_id.clone(), state.path()).unwrap();
        let runtime = crate::RuntimePaths::under(workspace_id, runtime.path()).unwrap();
        let store = crate::Store::open(paths, runtime).unwrap();
        ParentWatchdog::new(
            store,
            AgentKind::new_unchecked("codex"),
            AgentSessionId::from("child"),
            None,
            "session".to_owned(),
        )
    }

    #[test]
    fn authoritative_absence_requires_three_distinct_observations() {
        let mut watch = watchdog();

        assert!(!watch.observe(ParentProbe::Absent(1)));
        assert!(!watch.observe(ParentProbe::Absent(1)));
        assert!(!watch.observe(ParentProbe::Absent(2)));
        assert!(watch.observe(ParentProbe::Absent(3)));
    }

    #[test]
    fn presence_resets_absence_strikes_and_durable_end_is_immediate() {
        let mut watch = watchdog();

        assert!(!watch.observe(ParentProbe::Absent(1)));
        assert!(!watch.observe(ParentProbe::Absent(2)));
        assert!(!watch.observe(ParentProbe::Present(3)));
        assert!(!watch.observe(ParentProbe::Absent(4)));
        assert!(watch.observe(ParentProbe::Ended));
    }

    #[test]
    fn adopted_child_and_parent_rows_resolve_by_stable_launch_identity() {
        let mut parent =
            crate::testkit::agent_state("codex", "parent-provider-session", jiff::Timestamp::now());
        parent.launch_id = Some(AgentSessionId::from("launch-parent"));
        let mut child =
            crate::testkit::agent_state("codex", "child-provider-session", jiff::Timestamp::now());
        child.launch_id = Some(AgentSessionId::from("launch-child"));
        child.parent_agent_id = Some(AgentSessionId::from("launch-parent"));
        child.parent_agent_kind = Some(parent.kind.clone());

        let agents = [parent.clone(), child.clone()];
        let resolved = resolve_parent_and_child(
            &agents,
            &child.kind,
            child.launch_id.as_ref().expect("child launch id"),
        )
        .expect("resolve adopted rows");

        assert_eq!(resolved.0.agent_id, parent.agent_id);
        assert_eq!(resolved.1.agent_id, child.agent_id);
    }
}
