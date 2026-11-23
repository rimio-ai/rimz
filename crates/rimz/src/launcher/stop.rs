//! The tree stop: end an agent after its live launched children, close each pane a fresh
//! point-of-use read says hosts it, and judge the outcome from durable reads. It prints
//! nothing and exits nothing; a caller renders the [`TreeStop`] it gets back and the
//! backend notices it collects.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::agents::AgentState;
use crate::agents::state::session_row;
use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use crate::sidebar::produce::hosting::{Hosting, HostingRead, read_hosting};
use crate::store::run::RunRecord;
use crate::store::runtime::{AgentLiveness, OWNER_EXIT_BOUND, agent_liveness};
use crate::store::snapshot::SidebarSnapshot;
use crate::{ResolvedWorkspace, Store};

type AgentKey = (AgentKind, AgentSessionId);

/// How long a stop gives the pane of a run it cancelled to leave on its own before closing it.
pub const STOP_BACKSTOP_GRACE: Duration = Duration::from_secs(3);
const SUBAGENT_STOPPED_EVENT: &str = "rimz.subagent-stopped";

/// The room a stop acts in.
#[derive(Clone, Copy)]
pub struct StopRoom<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub store: &'a Store,
    /// The backend a command pinned with `--mux`, if any.
    pub mux: Option<MuxName>,
}

/// What a tree stop did. The default is the skip: the tracker already took
/// this agent, so nothing was stopped and nothing failed.
#[must_use]
#[derive(Debug, Default)]
pub struct TreeStop {
    root_stopped: bool,
    /// The session the stop stopped for the root: its holder when a swap replaced it.
    root: Option<AgentState>,
    child_receipts: Vec<String>,
    failures: Vec<String>,
    /// Each row this tree closed or stopped, under the key it was asked for and the key
    /// it is now; a batch skips them when a later line names one.
    reached: Vec<AgentKey>,
}

impl TreeStop {
    /// Whether the agent itself was stopped: no pane hosts it and its process is gone,
    /// its unknown process has a recorded end, or its never-bound launch is cancelled.
    pub fn root_stopped(&self) -> bool {
        self.root_stopped
    }

    /// The receipt line of each launched child this stop stopped, children first.
    pub fn child_receipts(&self) -> &[String] {
        &self.child_receipts
    }

    /// Each row this tree closed or stopped, under the key it was asked for and the key it is
    /// now.
    pub fn reached(&self) -> &[AgentKey] {
        &self.reached
    }

    /// The session stopped for the root, once [`TreeStop::root_stopped`].
    pub fn root(&self) -> Option<&AgentState> {
        self.root.as_ref()
    }

    /// Everything that failed, joined; each part starts with the handle it
    /// concerns. `None` when nothing failed.
    pub fn error(&self) -> Option<String> {
        (!self.failures.is_empty()).then(|| self.failures.join("; "))
    }

    /// The stop of one agent as a command result.
    pub fn into_result(self, label: &str) -> Result<()> {
        match self.error() {
            None => Ok(()),
            Some(error) if self.root_stopped() => bail!("{label} stopped, but: {error}"),
            Some(error) => bail!("{label} was not stopped: {error}"),
        }
    }
}

pub fn newest_run_for_agent(store: &Store, agent: &AgentState) -> Result<Option<RunRecord>> {
    newest_run_by_ref(store, agent.name.as_deref().unwrap_or(""), Some(agent))
}

pub fn newest_run_by_ref(
    store: &Store,
    reference: &str,
    agent: Option<&AgentState>,
) -> Result<Option<RunRecord>> {
    let mut records = crate::harness::run::list(store.paths())?;
    records.retain(|record| {
        if record.peer.is_some()
            && let Some(agent) = agent
        {
            return record.matches_agent(agent);
        }
        if record.run_id.as_str() == reference || record.agent_name.as_deref() == Some(reference) {
            return true;
        }
        if let Some(agent) = agent {
            return record.kind == agent.kind
                && (record.agent_id.as_ref() == Some(&agent.agent_id)
                    || agent
                        .name
                        .as_deref()
                        .is_some_and(|name| record.agent_name.as_deref() == Some(name)));
        }
        false
    });
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at));
    Ok(records.into_iter().next())
}

/// The newest supervised run recorded for `child`, the run the resume doorway reopens.
pub fn newest_run_for_child<'a>(
    runs: &'a [RunRecord],
    child: &AgentState,
) -> Option<&'a RunRecord> {
    runs.iter()
        .filter(|run| {
            run.kind == child.kind
                && (run.agent_id.as_ref() == Some(&child.agent_id)
                    || run
                        .agent_name
                        .as_ref()
                        .zip(child.name.as_ref())
                        .is_some_and(|(run, child)| run == child))
        })
        .max_by_key(|run| run.started_at)
}

/// What a stop of a parent's subagents did.
pub struct SubagentStops<'a> {
    /// Each child whose owed report could not be settled; nothing stopped it.
    pub unsettled: Vec<(&'a AgentState, crate::store::StoreErr)>,
    /// Every other child with its tree stop, in order.
    pub stops: Vec<(&'a AgentState, TreeStop)>,
}

/// Stop `children`, subagents of one parent: settle the report each one's run owes for the
/// whole fleet first, then stop every child that settled as [`stop_resolved`] does. A child that
/// fails to stop comes back in its [`TreeStop`].
pub fn stop_subagents<'a>(
    room: StopRoom<'_>,
    snapshot: &SidebarSnapshot,
    children: Vec<&'a AgentState>,
    notices: &mut Vec<String>,
) -> Result<SubagentStops<'a>> {
    let runs = crate::harness::run::list(room.store.paths())?;
    let (children, unsettled) =
        prepare_children_for_stop(room.store, &room.workspace.session_name, &runs, children);
    let stops = stop_resolved(room, snapshot, &children, notices)?;
    Ok(SubagentStops {
        unsettled,
        stops: children.into_iter().zip(stops).collect(),
    })
}

/// Settle the report each child's newest run owes before any stop: a cancellation would wake
/// a child reporter. The children it settled, and each it could not with why.
pub fn prepare_children_for_stop<'a>(
    store: &Store,
    session_name: &str,
    runs: &[crate::store::run::RunRecord],
    children: Vec<&'a AgentState>,
) -> (
    Vec<&'a AgentState>,
    Vec<(&'a AgentState, crate::store::StoreErr)>,
) {
    let mut prepared = Vec::with_capacity(children.len());
    let mut errors = Vec::new();
    for child in children {
        let Some(run) = newest_run_for_child(runs, child) else {
            prepared.push(child);
            continue;
        };
        match crate::harness::run::report::join_and_settle_digest(
            store,
            session_name,
            &run.run_id,
            None,
            "stopped by parent",
        ) {
            Ok(()) => prepared.push(child),
            Err(err) => errors.push((child, err)),
        }
    }
    (prepared, errors)
}

/// The supervised run a stop of `agent` settles, when it has one.
pub fn stop_run(store: &Store, agent: &AgentState) -> Result<Option<RunRecord>> {
    Ok(newest_run_for_agent(store, agent)?.filter(|run| run.peer.is_none()))
}

/// Stop every tree in `agents`, children before parents, through the point-of-use lookup:
/// the only pane a stop closes is one a read this stop took says hosts the agent, read again
/// before the close whenever the stop did work since that read (cancelled a swap or a run,
/// waited on a departure, thawed the pane, or closed a pane), and each close is followed by
/// a new read. After the last close it waits once, up to the shared
/// owner-exit bound, then sends one start-token-checked SIGTERM to each agent whose pane it
/// closed, or whose run it cancelled, and whose process outlived the wait with no pane
/// hosting it, and waits once more; it never sends SIGKILL.
/// Returns one `TreeStop` per root, in order, judged by a read taken after the waits: an
/// agent is stopped when no pane hosts it and its process is gone, its unknown process
/// has a recorded end, or its never-bound launch is cancelled. A first read that fails
/// stops nothing.
pub fn stop_resolved(
    room: StopRoom<'_>,
    snapshot: &SidebarSnapshot,
    agents: &[&AgentState],
    notices: &mut Vec<String>,
) -> Result<Vec<TreeStop>> {
    let stops = stop_resolved_with(room, snapshot, agents, StopTerms::default(), notices)?;
    // Without `still_due` every stop goes ahead.
    Ok(stops.unwrap_or_default())
}

/// What a caller adds to [`stop_resolved`].
#[derive(Default)]
pub struct StopTerms<'a> {
    /// The run this stop settles: cancelled once the first read succeeds, and the agent it
    /// names then gets the backstop grace to leave before any close, unless the run is kept.
    pub run: Option<&'a RunRecord>,
    /// Re-checked after the first read: `false` stops nothing and returns `None`.
    pub still_due: Option<&'a dyn Fn() -> bool>,
    /// The first read, a hosting read the caller took with no work since, in place of the
    /// stop's own.
    pub read: Option<HostingRead>,
    /// Which tab, if any, the stop closes with the panes it closes. A sidebar card key sets it.
    pub tab: TabClose,
}

/// The tab close of [`StopTerms::tab`].
#[derive(Default)]
pub enum TabClose {
    /// Close panes alone.
    #[default]
    Never,
    /// Close the tab with a pane when the read gating the close finds the pane the last
    /// working pane of its tab and this process runs in no pane of it.
    WhenLast,
    /// Every working pane of one tab is a target: its `panes`, as the caller listed them,
    /// of which `loose` host no agent the stop is given. A pane of them hosting an agent
    /// stays open while the stop ends that agent, cancels and departure wait included; once
    /// every agent has been ended, the stop closes the tab in one command, gated as a pane
    /// close is, and polls each such agent as after a pane close. When the gating read gives
    /// the tab no anchor (`HostingRead::tab_anchor`), or an agent's stop failed before its
    /// close, each such agent's pane and each of `loose` closes on its own instead, and a
    /// failed agent's pane stays.
    Whole {
        panes: Vec<PaneId>,
        loose: Vec<PaneId>,
    },
}

/// [`stop_resolved`] under `terms`; `None` when `still_due` withdrew the stop.
pub fn stop_resolved_with(
    room: StopRoom<'_>,
    snapshot: &SidebarSnapshot,
    agents: &[&AgentState],
    terms: StopTerms<'_>,
    notices: &mut Vec<String>,
) -> Result<Option<Vec<TreeStop>>> {
    let peers = crate::address::addressable_agents(snapshot);
    let read = terms
        .read
        .map_or_else(|| read_hosting(room.workspace, room.store, room.mux), Ok);
    let read = match read {
        Ok(mut read) => {
            notices.extend(read.take_notices());
            read
        }
        Err(mut err) => {
            notices.append(&mut err.notices);
            return Ok(Some(
                agents
                    .iter()
                    .map(|agent| TreeStop {
                        failures: vec![format!(
                            "{}: {err}; nothing was stopped",
                            crate::address::agent_handle(agent, &peers, true)
                        )],
                        ..TreeStop::default()
                    })
                    .collect(),
            ));
        }
    };
    if terms.still_due.is_some_and(|still_due| !still_due()) {
        return Ok(None);
    }
    let run_cancelled = match terms.run.filter(|run| !run.status.is_terminal()) {
        Some(run) => {
            cancel_run(room.store, run)?;
            true
        }
        None => false,
    };
    let stopper = Stopper {
        room,
        notices,
        peers: &peers,
        read,
        fresh: !run_cancelled,
        tab: terms.tab,
        run: terms.run,
        run_cancelled,
        tracker: HashSet::new(),
    };
    stop_with_read(stopper, agents).map(Some)
}

/// Stop the agent the supervised `run` names, settling the run. The run is cancelled first,
/// before any multiplexer read; `Err` means that cancel failed. Its agent is the holder of
/// the run's launch, else the row of its exact session; never one found by name, and never a
/// row that holds another launch now. A run that names no such row is done once cancelled.
/// Otherwise the agent's tree is stopped as a stop of that agent is, with the run's agent awaited as one whose run this stop cancelled: the
/// backstop grace (unless the run is kept) and the owner wait apply even when the run had
/// already ended.
pub fn stop_run_agent(
    room: StopRoom<'_>,
    run: &RunRecord,
    notices: &mut Vec<String>,
) -> Result<TreeStop> {
    stop_run_tree(room, run, None, notices)
}

/// [`stop_run_agent`], stopping `agent` in place of the agent the run names when given.
pub fn stop_run_tree(
    room: StopRoom<'_>,
    run: &RunRecord,
    agent: Option<&AgentState>,
    notices: &mut Vec<String>,
) -> Result<TreeStop> {
    cancel_run(room.store, run)?;
    if agent.is_none() && run.agent_launch_id().is_none() && run.agent_id.is_none() {
        return Ok(TreeStop {
            root_stopped: true,
            ..TreeStop::default()
        });
    }
    let read = match read_hosting(room.workspace, room.store, room.mux) {
        Ok(mut read) => {
            notices.extend(read.take_notices());
            read
        }
        Err(mut err) => {
            notices.append(&mut err.notices);
            let label = run.agent_name.as_deref().unwrap_or(run.run_id.as_str());
            let settled = if run.status.is_terminal() {
                "had already ended"
            } else {
                "is canceled"
            };
            return Ok(TreeStop {
                failures: vec![format!(
                    "{label}: the run {settled}, but {err}; no pane was closed"
                )],
                ..TreeStop::default()
            });
        }
    };
    // A launch's holder may be a row that holds another launch now (a fresh launch resumed the
    // session); it is not the run's agent, so that stop is the cancel alone.
    let agent = match (agent, run.agent_launch_id(), &run.agent_id) {
        (Some(agent), ..) => Some(agent.clone()),
        (None, Some(launch), _) => read
            .agent_hosting(&run.kind, launch)
            .holder
            .filter(|row| run.matches_agent(row))
            .cloned(),
        (None, None, Some(session)) => read.agent(&run.kind, session).cloned(),
        (None, None, None) => None,
    };
    let Some(agent) = agent else {
        return Ok(TreeStop {
            root_stopped: true,
            ..TreeStop::default()
        });
    };
    let snapshot = room
        .store
        .snapshot_cached()
        .context("reading agent snapshot")?;
    let peers = crate::address::addressable_agents(&snapshot);
    let stopper = Stopper {
        room,
        notices,
        peers: &peers,
        read,
        fresh: true,
        tab: TabClose::Never,
        run: Some(run),
        run_cancelled: true,
        tracker: HashSet::new(),
    };
    Ok(stop_with_read(stopper, &[&agent])?.remove(0))
}

pub(super) fn cancel_run(store: &Store, run: &RunRecord) -> Result<()> {
    if !run.status.is_terminal() {
        crate::harness::run::cancel_and_wake(store, &run.run_id)?;
    }
    Ok(())
}

/// Append the `Ended` lifecycle event for the card a stop of a subagent run stopped (the
/// `stopped` kind and session), while that card is un-ended and still the run's; a run that
/// is not a subagent's records nothing.
pub fn stamp_stopped_subagent_end(
    store: &Store,
    session_name: &str,
    run: &RunRecord,
    stopped: (&AgentKind, &AgentSessionId),
) -> Result<()> {
    if !run.subagent {
        return Ok(());
    }
    let audit = store
        .runtime_projection(crate::RuntimeScope::Audit)
        .context("reading stopped subagent card")?;
    let Some(child) = audit.agents.iter().find(|agent| {
        (&agent.kind, &agent.agent_id) == stopped
            && agent.ended_at.is_none()
            && run.matches_stopped_agent(agent)
    }) else {
        return Ok(());
    };
    let observation = crate::agents::AgentLifecycleObservation::new(
        Some(child.agent_id.clone()),
        crate::agents::LifecycleSignal::Ended,
    );
    let ended = crate::EventEnvelope::agent_lifecycle(
        run.workspace_id.clone(),
        session_name,
        child.kind.as_str(),
        SUBAGENT_STOPPED_EVENT,
        &observation,
    );
    store
        .append_event(&ended)
        .context("recording stopped subagent end")
}

/// Stop every tree in `agents` through `stopper`, which holds the first read.
fn stop_with_read(mut stopper: Stopper<'_>, agents: &[&AgentState]) -> Result<Vec<TreeStop>> {
    let mut trees = agents
        .iter()
        .map(|agent| {
            let current = stopper
                .read
                .agent(&agent.kind, &agent.agent_id)
                .unwrap_or(agent)
                .clone();
            let mut endings = Vec::new();
            stopper.end_tree(current, None, &mut endings);
            endings
        })
        .collect::<Vec<_>>();
    let tab = stopper.close_whole_tab(trees.iter_mut().flatten().collect());
    stopper.judge(trees.iter_mut().flatten().collect())?;
    tab.map_err(anyhow::Error::msg)?;
    Ok(trees
        .into_iter()
        .map(|endings| stopper.receipt(endings))
        .collect())
}

/// How long a stop waits for an agent's process to leave, and for a closed pane to leave
/// the listing: an exec wrapper holds both through two signal graces after the hangup.
fn stop_owner_exit_bound() -> Duration {
    #[cfg(any(test, feature = "testkit"))]
    if let Some(ms) = std::env::var("RIMZ_TEST_STOP_OWNER_EXIT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    OWNER_EXIT_BOUND
}

/// The pause between reads while a stop waits for a closed pane to leave the listing.
const CLOSE_POLL: Duration = Duration::from_millis(100);
/// How long a stop leaves a pane it closed before closing it again while it stays listed.
const RECLOSE_AFTER: Duration = Duration::from_millis(500);
/// The pause between store reads while a stop waits for processes and recorded ends.
const ENDING_POLL: Duration = Duration::from_millis(50);

/// One agent of a stop: what this stop did to it, and its outcome once judged.
struct Ending {
    /// The key the stop was asked for; schedules retire under it.
    asked: AgentKey,
    /// The session this stop acts on, as last read: after the swap cancellation the agent's
    /// holder unless the asked session still holds it, and after a re-key the row that
    /// adopted its provisional key.
    session: AgentState,
    /// The bare handle a failed root's line starts with; a child's starts with `label`.
    handle: String,
    /// The handle as a receipt names it, with a differing name beside it.
    label: String,
    /// The handle of the agent whose stop reached this one as a launched child.
    parent: Option<String>,
    /// This stop issued a close of its pane, whatever the close returned.
    closed: bool,
    /// This stop cancelled the agent's run; a pane that then left counts as closed for the
    /// owner wait and the SIGTERM.
    cancelled: bool,
    /// The final read still found a pane hosting it.
    still_hosted: bool,
    /// Its pane awaits the tab close of [`TabClose::Whole`].
    awaits_tab: bool,
    /// A reservation no wrapper had bound and no pane hosted at this stop's first read.
    unbound_unhosted: bool,
    close_error: Option<String>,
    run: Option<RunRecord>,
    /// The run the caller passed is this agent's: the stop settles it for this session.
    holds_run: bool,
    /// A fresh launch resumed the session since, so the run's agent is hosted nowhere and
    /// nothing of that launch is this stop's to close, signal, or stamp.
    left_run: bool,
    signaled: bool,
    outcome: Option<std::result::Result<(), String>>,
}

impl Ending {
    /// The key of the session this stop acts on.
    fn key(&self) -> AgentKey {
        (self.session.kind.clone(), self.session.agent_id.clone())
    }

    fn decide(&mut self, outcome: std::result::Result<(), String>) {
        self.outcome = Some(outcome);
    }

    /// Whether this stop is waiting on the agent's live process to leave.
    fn awaits_owner(&self) -> bool {
        self.closed || self.cancelled
    }
}

struct Stopper<'a> {
    room: StopRoom<'a>,
    /// The backend notices of every read this stop takes, in order.
    notices: &'a mut Vec<String>,
    peers: &'a [&'a AgentState],
    /// Every read after the first lists this read's backend.
    read: HostingRead,
    /// No work of this stop (a cancel, a wait, a thaw, a close) sat between `read` and now,
    /// so `read` may gate a close.
    fresh: bool,
    tab: TabClose,
    run: Option<&'a RunRecord>,
    run_cancelled: bool,
    tracker: HashSet<AgentKey>,
}

impl Stopper<'_> {
    /// Stop `agent` after its live launched children, pushing each ending in that order.
    fn end_tree(&mut self, agent: AgentState, parent: Option<String>, endings: &mut Vec<Ending>) {
        let key = (agent.kind.clone(), agent.agent_id.clone());
        if self.tracker.contains(&key) {
            return;
        }
        let handle = crate::address::agent_handle(&agent, self.peers, true);
        let label = crate::address::handle_with_name(&handle, agent.name.as_deref());
        let children = crate::address::launched_children(self.read.agents(), &agent)
            .into_iter()
            .filter(|child| child.ended_at.is_none())
            .cloned()
            .collect::<Vec<_>>();
        for child in children {
            self.end_tree(child, Some(label.clone()), endings);
        }
        let ending = self.end(&agent, key.clone(), handle, label, parent);
        self.tracker.insert(key);
        self.tracker.insert(ending.key());
        endings.push(ending);
    }

    fn end(
        &mut self,
        agent: &AgentState,
        key: AgentKey,
        handle: String,
        label: String,
        parent: Option<String>,
    ) -> Ending {
        let mut ending = Ending {
            asked: key,
            session: agent.clone(),
            handle,
            label,
            parent,
            closed: false,
            cancelled: false,
            still_hosted: false,
            awaits_tab: false,
            unbound_unhosted: agent.agent_id.is_provisional()
                && agent.runtime_owner.is_none()
                && self.read.session_hosting(&agent.kind, &agent.agent_id) == Hosting::Nowhere,
            close_error: None,
            run: None,
            holds_run: false,
            left_run: false,
            signaled: false,
            outcome: None,
        };
        if let Err(reason) = self.act(agent, &mut ending) {
            ending.decide(Err(reason));
        }
        ending
    }

    fn act(&mut self, agent: &AgentState, ending: &mut Ending) -> std::result::Result<(), String> {
        let store = self.room.store;
        let workspace = self.room.workspace;
        // A restart or molt still starting would otherwise bind its replacement
        // after the stop; cancel it first.
        let cwd = agent
            .worktree_path
            .as_deref()
            .map_or(workspace.worktree_root.as_path(), Path::new);
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_STOP_BEFORE_SWAP_CANCEL");
        let rows = store.audit_rows().map_err(|err| format!("{err:#}"))?;
        // A replacement the store shows holding the agent may have bound after the read.
        if !closes_asked(rows.iter(), agent) {
            self.fresh = false;
        }
        let mut cancelled_self = false;
        for member in
            crate::agents::state::launch_members(rows.iter(), &agent.kind, agent.launch_key())
                .into_iter()
                .filter(|member| member.agent_id.is_provisional() && member.epoch > 0)
        {
            // Any swap leaves the read stale, its cancel refused or not: a refusal means the
            // replacement bound, perhaps after the read, and holds the agent now.
            self.fresh = false;
            if store
                .cancel_unbound_swap(member, &workspace.session_name, cwd)
                .map_err(|err| format!("{err:#}"))?
                && member.agent_id == agent.agent_id
            {
                cancelled_self = true;
            }
        }
        if cancelled_self {
            ending.decide(Ok(()));
            return Ok(());
        }
        // Read after any swap: a replacement that bound first holds the agent now, and it
        // is what the stop closes.
        self.refresh_stale()?;
        let target = if closes_asked(self.read.agents(), agent) {
            agent.clone()
        } else {
            self.read
                .agent_hosting(&agent.kind, agent.launch_key())
                .holder
                .ok_or("the restarted agent disappeared while stopping it")?
                .clone()
        };
        ending.session = target.clone();
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_STOP_AFTER_TARGET");
        let agent = &target;
        // A run the caller passed is the one this stop settles for the agent it names;
        // `stop_run` matches by name as well, so it could pick another.
        let passed = self.run.filter(|run| run.matches_agent(agent));
        ending.holds_run = passed.is_some();
        ending.cancelled = match passed {
            Some(_) => self.run_cancelled,
            None => {
                ending.run =
                    stop_run(store, agent).map_err(|err| format!("reading its runs: {err:#}"))?;
                match ending.run.as_ref().filter(|run| !run.status.is_terminal()) {
                    Some(run) => {
                        cancel_run(store, run).map_err(|err| format!("{err:#}"))?;
                        self.fresh = false;
                        true
                    }
                    None => false,
                }
            }
        };
        if let Some(run) = passed {
            ending.run = Some(run.clone());
        }
        if ending.cancelled && !ending.run.as_ref().is_some_and(|run| run.keep) {
            self.await_departure(ending)?;
        }
        self.close(ending)
    }

    /// Record a stopped subagent run's end, once its ending is judged stopped.
    fn stamp_end(&self, ending: &Ending) -> std::result::Result<(), String> {
        let Some(run) = ending.run.as_ref().filter(|_| !ending.left_run) else {
            return Ok(());
        };
        stamp_stopped_subagent_end(
            self.room.store,
            &self.room.workspace.session_name,
            run,
            (&ending.session.kind, &ending.session.agent_id),
        )
        .map_err(|err| format!("recording its end failed: {err:#}"))
    }

    /// Where the ending's session is hosted in the current read, following a re-key first;
    /// nowhere once it left the run this stop settles.
    fn hosting(&self, ending: &mut Ending) -> Hosting {
        let row = track(self.read.agents(), &mut ending.session);
        if row.is_some_and(|row| self.leaves_run(ending, row)) {
            ending.left_run = true;
        }
        if ending.left_run {
            return Hosting::Nowhere;
        }
        self.read
            .session_hosting(&ending.session.kind, &ending.session.agent_id)
    }

    /// Whether `row`, the ending's session as read again, no longer holds the run the caller
    /// passed: a fresh launch resumed the session and holds it now.
    fn leaves_run(&self, ending: &Ending, row: &AgentState) -> bool {
        ending.holds_run && self.run.is_some_and(|run| !run.matches_agent(row))
    }

    fn refresh(&mut self) -> std::result::Result<(), String> {
        match read_hosting(self.room.workspace, self.room.store, Some(self.read.mux())) {
            Ok(mut read) => {
                self.notices.extend(read.take_notices());
                self.read = read;
                self.fresh = true;
                Ok(())
            }
            Err(mut err) => {
                self.notices.append(&mut err.notices);
                Err(err.to_string())
            }
        }
    }

    /// Read again unless the current read is fresh.
    fn refresh_stale(&mut self) -> std::result::Result<(), String> {
        if self.fresh {
            return Ok(());
        }
        self.refresh()
    }

    /// A pane whose run this stop cancelled gets the backstop grace to leave on its own; a
    /// kept run's never leaves on its own, so `act` closes it at once.
    fn await_departure(&mut self, ending: &mut Ending) -> std::result::Result<(), String> {
        let deadline = Instant::now() + STOP_BACKSTOP_GRACE;
        while self.hosting(ending) != Hosting::Nowhere && Instant::now() < deadline {
            std::thread::sleep(CLOSE_POLL);
            self.refresh()?;
        }
        self.fresh = false;
        Ok(())
    }

    /// Close whatever pane the current read says hosts the agent, read again first unless
    /// it is fresh, until none does or the owner-exit bound passes after the first close. A
    /// pane still listed is closed again once `RECLOSE_AFTER` has passed since its last close.
    /// Under [`TabClose::WhenLast`], a pane the gating read finds the last working pane of its
    /// tab closes with its tab; under [`TabClose::Whole`], a pane of the tab stays open for
    /// [`Stopper::close_whole_tab`].
    fn close(&mut self, ending: &mut Ending) -> std::result::Result<(), String> {
        let mut deadline = Instant::now() + stop_owner_exit_bound();
        let mut last: Option<(PaneId, Instant)> = None;
        let mut awaited_hold = false;
        loop {
            let Hosting::Pane {
                pane_id,
                session_name,
            } = self.hosting(ending)
            else {
                if last.is_none() && !awaited_hold {
                    awaited_hold = true;
                    self.await_kept_hold(ending)?;
                    deadline = Instant::now() + stop_owner_exit_bound();
                    continue;
                }
                return Ok(());
            };
            let now = Instant::now();
            if now >= deadline {
                return Ok(());
            }
            if last
                .as_ref()
                .is_none_or(|(closed, at)| closed != &pane_id || now >= *at + RECLOSE_AFTER)
            {
                let woken = crate::harness::hibernate::wake_pane(self.room.store, &pane_id)
                    .map_err(|err| format!("{err:#}"))?;
                if matches!(woken, crate::harness::hibernate::Woken::Thawed { .. }) {
                    self.fresh = false;
                }
                if !self.fresh {
                    self.refresh()?;
                }
                if self.hosting(ending)
                    != (Hosting::Pane {
                        pane_id: pane_id.clone(),
                        session_name: session_name.clone(),
                    })
                {
                    continue;
                }
                if let TabClose::Whole { panes, .. } = &self.tab
                    && panes.contains(&pane_id)
                {
                    ending.awaits_tab = true;
                    return Ok(());
                }
                let closed_at = Instant::now();
                if last.is_none() {
                    deadline = closed_at + stop_owner_exit_bound();
                }
                let backend = crate::mux::backend_for(self.read.mux());
                let result = if matches!(self.tab, TabClose::WhenLast)
                    && self.read.closing_empties_tab(
                        &pane_id,
                        crate::mux::own_pane_id(self.read.mux()).as_ref(),
                    ) {
                    backend.close_tab_of_pane(&session_name, &pane_id)
                } else {
                    backend.close_pane(&session_name, &pane_id)
                };
                self.fresh = false;
                ending.closed = true;
                ending.close_error = result.err().map(|err| format!("{err:#}"));
                last = Some((pane_id, closed_at));
            }
            std::thread::sleep(CLOSE_POLL);
            self.refresh()?;
        }
    }

    /// Close the tab of [`TabClose::Whole`] once every agent has been ended, deciding each
    /// ending that awaits it as [`Stopper::close`] would. An agent whose stop failed before
    /// its close keeps its pane, and so the tab: the awaiting agents' panes and the `loose`
    /// panes close on their own instead, as they do when the gating read gives the tab no
    /// anchor. An error is a close that failed with no ending to carry it.
    fn close_whole_tab(&mut self, endings: Vec<&mut Ending>) -> std::result::Result<(), String> {
        let TabClose::Whole { panes, loose } = std::mem::take(&mut self.tab) else {
            return Ok(());
        };
        let failed = endings.iter().any(|ending| {
            !ending.closed
                && !ending.awaits_tab
                && ending.outcome.as_ref().is_some_and(Result::is_err)
        });
        let mut endings = endings
            .into_iter()
            .filter(|ending| ending.awaits_tab)
            .collect::<Vec<_>>();
        let anchor = match self.refresh_stale() {
            Ok(()) => {
                let own = crate::mux::own_pane_id(self.read.mux());
                self.read.tab_anchor(&panes, own.as_ref()).cloned()
            }
            Err(reason) => return decide_all(&mut endings, reason),
        };
        let Some(anchor) = anchor.filter(|_| !failed) else {
            return self.close_each(&loose, &mut endings);
        };
        match self.close_tab_of(&panes, anchor, &mut endings) {
            Err(reason) => decide_all(&mut endings, reason),
            Ok(()) => Ok(()),
        }
    }

    /// Close the tab by `anchor` in one command, then wait as [`Stopper::close`] does until
    /// no pane hosts one of `endings` or the owner-exit bound passes, closing the tab again
    /// once `RECLOSE_AFTER` has passed while one does and a read still gives it an anchor
    /// among `panes`.
    fn close_tab_of(
        &mut self,
        panes: &[PaneId],
        mut anchor: PaneId,
        endings: &mut [&mut Ending],
    ) -> std::result::Result<(), String> {
        let mux = self.read.mux();
        let own = crate::mux::own_pane_id(mux);
        let deadline = Instant::now() + stop_owner_exit_bound();
        loop {
            let result = crate::mux::backend_for(mux)
                .close_tab_of_pane(&self.room.workspace.session_name, &anchor);
            self.fresh = false;
            let closed_at = Instant::now();
            let close_error = result.err().map(|err| format!("{err:#}"));
            if endings.is_empty() {
                return close_error.map_or(Ok(()), Err);
            }
            for ending in endings.iter_mut() {
                ending.closed = true;
                ending.close_error.clone_from(&close_error);
            }
            loop {
                std::thread::sleep(CLOSE_POLL);
                self.refresh()?;
                if endings
                    .iter_mut()
                    .all(|ending| self.hosting(ending) == Hosting::Nowhere)
                    || Instant::now() >= deadline
                {
                    return Ok(());
                }
                if Instant::now() >= closed_at + RECLOSE_AFTER {
                    break;
                }
            }
            let Some(listed) = self.read.tab_anchor(panes, own.as_ref()) else {
                return Ok(());
            };
            anchor = listed.clone();
        }
    }

    /// Close each of `endings` on its own, then each of `loose`.
    fn close_each(
        &mut self,
        loose: &[PaneId],
        endings: &mut [&mut Ending],
    ) -> std::result::Result<(), String> {
        for ending in endings.iter_mut() {
            if let Err(reason) = self.close(ending) {
                ending.decide(Err(reason));
            }
        }
        for pane in loose {
            crate::mux::backend_for(pane.mux())
                .close_pane(&self.room.workspace.session_name, pane)
                .map_err(|err| format!("{err:#}"))?;
        }
        Ok(())
    }

    /// A kept run this stop cancelled can read as hosted nowhere for a moment: its wrapper
    /// ends the provider and then binds the pane again to hold it (`kept_run_stopped` in
    /// `exec/supervisor.rs`). Wait up to the backstop grace for a read that hosts it again.
    fn await_kept_hold(&mut self, ending: &mut Ending) -> std::result::Result<(), String> {
        let kept = ending
            .run
            .as_ref()
            .is_some_and(|run| run.keep && !run.subagent);
        if !(ending.cancelled && kept) {
            return Ok(());
        }
        let deadline = Instant::now() + STOP_BACKSTOP_GRACE;
        while self.hosting(ending) == Hosting::Nowhere
            && !ending.left_run
            && Instant::now() < deadline
        {
            std::thread::sleep(CLOSE_POLL);
            self.refresh()?;
        }
        Ok(())
    }

    /// Wait once for the batch, signal each closed agent whose process outlived it, wait
    /// once more for those, and judge every ending not yet decided from the final reads.
    fn judge(&mut self, mut endings: Vec<&mut Ending>) -> Result<()> {
        endings.retain(|ending| ending.outcome.is_none());
        if endings.is_empty() {
            return Ok(());
        }
        let bound = stop_owner_exit_bound();
        let mut projection = self.await_endings(&mut endings, Instant::now() + bound)?;
        let mut read = self.refresh();
        let mut signaled = false;
        if read.is_ok() {
            for ending in endings.iter_mut().filter(|ending| ending.awaits_owner()) {
                if self.hosting(ending) != Hosting::Nowhere || ending.left_run {
                    continue;
                }
                let Some(owner) = track(self.read.agents(), &mut ending.session)
                    .filter(|row| matches!(agent_liveness(row), AgentLiveness::Live { .. }))
                    .and_then(|row| row.runtime_owner.as_ref())
                    .filter(|owner| owner.process_start.is_some())
                else {
                    continue;
                };
                ending.signaled = crate::child_process::signal_process_term(
                    owner.pid,
                    owner.process_start.as_deref(),
                );
                signaled |= ending.signaled;
            }
        }
        if signaled {
            let mut termed = endings
                .iter_mut()
                .filter(|ending| ending.signaled)
                .map(|ending| &mut **ending)
                .collect::<Vec<_>>();
            projection = self.await_endings(&mut termed, Instant::now() + bound)?;
            read = self.refresh();
        }
        if read.is_ok() {
            for ending in endings.iter_mut().filter(|ending| ending.unbound_unhosted) {
                if self.hosting(ending) != Hosting::Nowhere || ending.left_run {
                    continue;
                }
                let Some(agent) = track(&projection.agents, &mut ending.session) else {
                    continue;
                };
                let cwd = agent
                    .worktree_path
                    .as_deref()
                    .map_or(self.room.workspace.worktree_root.as_path(), Path::new);
                if let Err(err) = self.room.store.cancel_unbound_agent_launch(
                    &ending.session.kind,
                    &ending.session.agent_id,
                    &self.room.workspace.session_name,
                    cwd,
                ) {
                    ending.decide(Err(format!("cancelling its launch failed: {err:#}")));
                }
            }
            read = self.refresh();
            projection = self
                .room
                .store
                .runtime_projection(crate::RuntimeScope::Audit)?;
        }
        for ending in endings {
            if ending.outcome.is_some() {
                continue;
            }
            let outcome = match &read {
                Ok(()) => self
                    .verdict(ending, &projection.agents)
                    .and_then(|()| self.stamp_end(ending)),
                Err(err) => Err(err.clone()),
            };
            ending.decide(outcome);
        }
        Ok(())
    }

    /// Wait until no ending has a process this stop is waiting on (a live one whose pane it
    /// closed, or an unknown one whose end is not recorded), or `deadline` passes.
    fn await_endings(
        &self,
        endings: &mut [&mut Ending],
        deadline: Instant,
    ) -> Result<crate::store::runtime::RuntimeProjection> {
        loop {
            let projection = self
                .room
                .store
                .runtime_projection(crate::RuntimeScope::Audit)?;
            let waiting = endings.iter_mut().any(|ending| {
                let closed = ending.awaits_owner();
                track(&projection.agents, &mut ending.session).is_some_and(|row| {
                    if self.leaves_run(ending, row) {
                        return false;
                    }
                    match agent_liveness(row) {
                        AgentLiveness::Live { .. } => closed,
                        AgentLiveness::Unknown => {
                            !failed_unbound_launch(row)
                                && (row.ended_at.is_none()
                                    || (ending.unbound_unhosted
                                        && row.agent_id.is_provisional()
                                        && row.runtime_owner.is_none()))
                        }
                        AgentLiveness::Dead => false,
                    }
                })
            });
            if !waiting || Instant::now() >= deadline {
                return Ok(projection);
            }
            std::thread::sleep(ENDING_POLL);
        }
    }

    fn verdict(
        &self,
        ending: &mut Ending,
        agents: &[AgentState],
    ) -> std::result::Result<(), String> {
        if let Hosting::Pane { pane_id, .. } = self.hosting(ending) {
            ending.still_hosted = true;
            let close = ending
                .close_error
                .as_ref()
                .map(|err| format!("; closing it failed: {err}"))
                .unwrap_or_default();
            return Err(format!("a pane still hosts it: {pane_id}{close}"));
        }
        // The run's stop was its cancel; the launch that resumed the session is not stopped.
        if ending.left_run {
            return Ok(());
        }
        unhosted_verdict(
            track(agents, &mut ending.session),
            ending.awaits_owner(),
            ending.signaled,
        )
    }

    /// One root's receipt; schedules retire for each agent this stop stopped, or whose
    /// pane it closed and no longer finds.
    fn receipt(&self, endings: Vec<Ending>) -> TreeStop {
        let mut stop = TreeStop::default();
        for ending in endings {
            let key = ending.key();
            // A failed root is paired once, by `TreeStop::into_result`; a child only here.
            let failed = if ending.parent.is_some() {
                &ending.label
            } else {
                &ending.handle
            };
            // `judge` decides every ending before a receipt is built, so an undecided one
            // is impossible; it still never reads as stopped.
            let outcome = ending
                .outcome
                .unwrap_or_else(|| Err("its stop was never judged".to_owned()));
            match (&ending.parent, &outcome) {
                (Some(parent), Ok(())) => stop
                    .child_receipts
                    .push(format!("stopped {} (subagent of {parent})", ending.label)),
                (None, Ok(())) => {
                    stop.root_stopped = true;
                    stop.root = Some(ending.session.clone());
                }
                (_, Err(reason)) => stop.failures.push(format!("{failed}: {reason}")),
            }
            if ending.left_run {
                continue;
            }
            if ending.closed || outcome.is_ok() {
                stop.reached.push(ending.asked.clone());
                stop.reached.push(key.clone());
            }
            if outcome.is_err() && !(ending.closed && !ending.still_hosted) {
                continue;
            }
            // A stop that ends the agent retires the waits armed in any of its
            // sessions, including those a swap's replaced session kept for the
            // holder. One that closed only a replaced session leaves them.
            let rows = match self.room.store.audit_rows() {
                Ok(rows) => rows,
                Err(err) => {
                    stop.failures.push(format!("{failed}: {err:#}"));
                    continue;
                }
            };
            let agent = rows
                .iter()
                .find(|row| (&row.kind, &row.agent_id) == (&key.0, &key.1));
            if agent.is_some_and(|agent| !stop_ends_agent(rows.iter(), agent, &key)) {
                continue;
            }
            let members = agent.map_or_else(Vec::new, |agent| {
                crate::agents::state::launch_members(rows.iter(), &agent.kind, agent.launch_key())
            });
            // A wait armed since targets hold launch ids is pinned to the launch, under
            // the kind its arming session ran as.
            let sessions = std::iter::once((&ending.asked.0, &ending.asked.1))
                .chain(members.iter().flat_map(|member| {
                    [
                        (&member.kind, &member.agent_id),
                        (&member.kind, member.launch_key()),
                    ]
                }))
                .collect::<std::collections::BTreeSet<_>>();
            for (kind, session) in sessions {
                if let Err(err) = crate::harness::schedule::arm::retire_session(
                    &self.room.workspace.project_root,
                    kind,
                    session,
                ) {
                    stop.failures.push(format!("{failed}: {err}"));
                }
            }
        }
        stop
    }
}

/// Decide each of `endings` failed for `reason`; with none to carry it, `reason` is the error.
fn decide_all(endings: &mut [&mut Ending], reason: String) -> std::result::Result<(), String> {
    if endings.is_empty() {
        return Err(reason);
    }
    for ending in endings.iter_mut() {
        ending.decide(Err(reason.clone()));
    }
    Ok(())
}

/// Whether a stop closes `agent` itself, judged over the rows of a read taken after any swap
/// cancellation: unless a swap replaced it meanwhile (the replacement bound before the
/// cancellation could land, so the cancellation was refused) or it is a starting
/// reservation with no pane. Otherwise it closes the agent's holder.
fn closes_asked<'a>(
    rows: impl IntoIterator<Item = &'a AgentState> + Clone,
    agent: &AgentState,
) -> bool {
    let members = crate::agents::state::launch_members(rows, &agent.kind, agent.launch_key());
    let holders = crate::agents::state::LaunchHolders::new(members.iter().copied());
    let replaced = members.iter().any(|member| {
        member.kind == agent.kind && member.agent_id == agent.agent_id && holders.replaced(member)
    });
    !replaced && (agent.pane.is_some() || !agent.agent_id.is_provisional())
}

/// Whether closing `closed` ends `agent`'s agent: every other row holding it
/// is over already. Waits and watchers armed for the agent go only then; a
/// stop that closed a session a swap replaced leaves them to the holder.
fn stop_ends_agent<'a>(
    rows: impl IntoIterator<Item = &'a AgentState> + Clone,
    agent: &AgentState,
    closed: &AgentKey,
) -> bool {
    let members = crate::agents::state::launch_members(rows, &agent.kind, agent.launch_key());
    crate::agents::state::launch_holder(&members)
        .iter()
        .all(|holder| {
            holder.session_over() || (&holder.kind, &holder.agent_id) == (&closed.0, &closed.1)
        })
}

/// With no hosting pane, only a known-dead owner, a recorded end, or a cancelled
/// reservation proves the stop. A missing row (including an ambiguous re-key) proves none.
fn unhosted_verdict(
    agent: Option<&AgentState>,
    closed: bool,
    signaled: bool,
) -> std::result::Result<(), String> {
    let unconfirmed = || {
        Err(
            "RimZ could not confirm its process is gone; the stop succeeds once the agent's \
             end is recorded"
                .to_owned(),
        )
    };
    let Some(agent) = agent else {
        return unconfirmed();
    };
    match agent_liveness(agent) {
        AgentLiveness::Dead => Ok(()),
        AgentLiveness::Live { pid } if signaled => Err(format!(
            "its process {pid} is still running after RimZ sent it SIGTERM"
        )),
        AgentLiveness::Live { pid } if closed => Err(format!(
            "its process {pid} is still running after its pane closed"
        )),
        AgentLiveness::Live { pid } => Err(format!(
            "RimZ found no pane for it in this room and did not signal its process {pid}, \
             which is still running"
        )),
        AgentLiveness::Unknown if agent.ended_at.is_some() => Ok(()),
        AgentLiveness::Unknown if failed_unbound_launch(agent) => Ok(()),
        AgentLiveness::Unknown => unconfirmed(),
    }
}

fn failed_unbound_launch(agent: &AgentState) -> bool {
    agent.agent_id.is_provisional()
        && agent.runtime_owner.is_none()
        && agent.status == crate::agents::AgentStatus::Failed
}

/// The row `session` is now in `agents`; `session` follows it when a re-key moved it.
fn track<'a>(agents: &'a [AgentState], session: &mut AgentState) -> Option<&'a AgentState> {
    let row = session_row(agents, session)?;
    if row.agent_id != session.agent_id {
        *session = row.clone();
    }
    Some(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentStatus, PermissionMode};

    /// A stop that read the old session while its swap's replacement bound
    /// closes the holder instead, and only a stop of the holder ends the agent.
    #[test]
    fn a_stop_racing_a_bound_swap_targets_the_replacement() {
        let pane = |raw: &str| {
            crate::pane::PaneRef::from_id(crate::ids::PaneId::from_parts(
                crate::ids::MuxName::Tmux,
                raw,
            ))
        };
        let mut old = AgentState::stub("claude", "sess-old", crate::agents::AgentStatus::Idle);
        old.launch_id = Some("launch_agent".into());
        old.pane = Some(pane("%1"));
        let mut replacement = old.clone();
        replacement.agent_id = "launch_swap".into();
        replacement.epoch = 1;
        replacement.pane = Some(pane("%2"));
        let key = |row: &AgentState| (row.kind.clone(), row.agent_id.clone());

        let unbound = [old.clone(), replacement.clone()];
        assert!(
            closes_asked(&unbound, &old),
            "a reservation that never stood leaves the old session the agent"
        );
        assert!(stop_ends_agent(&unbound, &old, &key(&old)));

        replacement.launch_bound = true;
        let bound = [old.clone(), replacement.clone()];
        assert!(!closes_asked(&bound, &old));
        assert!(
            !stop_ends_agent(&bound, &old, &key(&old)),
            "closing the replaced session leaves the holder's waits"
        );
        assert!(stop_ends_agent(&bound, &old, &key(&replacement)));
    }

    #[test]
    fn a_missing_final_row_is_not_evidence_that_the_agent_stopped() {
        assert_eq!(
            unhosted_verdict(None, true, false),
            Err(
                "RimZ could not confirm its process is gone; the stop succeeds once the agent's \
                 end is recorded"
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_stop_never_settles_a_peer_run() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = crate::WorkspaceId::from_project_root(dir.path());
        let store = crate::Store::open(
            crate::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap(),
            crate::RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap(),
        )
        .unwrap();
        let mut peer = AgentState::stub("codex", "peer", crate::agents::AgentStatus::Idle);
        peer.name = Some("peer".into());
        peer.launch_id = Some("peer-launch".into());
        for status in [
            crate::store::run::RunStatus::Running,
            crate::store::run::RunStatus::Completed,
        ] {
            let mut record = crate::store::run::RunRecord::new(
                workspace_id.clone(),
                peer.kind.clone(),
                crate::agents::PermissionMode::Auto,
                "task".into(),
                dir.path().into(),
            );
            record.peer = Some(crate::store::run::PeerRun {
                launch_id: "peer-launch".into(),
                opened_by: Vec::new(),
            });
            record.status = status;
            crate::harness::run::create(store.paths(), &record).unwrap();
            assert!(stop_run(&store, &peer).unwrap().is_none());
            assert_eq!(
                crate::harness::run::load(store.paths(), &record.run_id).unwrap(),
                record
            );
        }
    }

    #[test]
    fn peer_lookup_requires_identity_when_agent_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let paths = crate::disk::paths::StatePaths::under(id.clone(), dir.path()).unwrap();
        let runtime = crate::disk::paths::RuntimePaths::under(id.clone(), dir.path()).unwrap();
        let store = crate::Store::open(paths, runtime).unwrap();
        let mut agent = AgentState::stub("codex", "session", AgentStatus::Idle);
        agent.name = Some("peer".into());
        agent.launch_id = Some("launch".into());
        let mut record = RunRecord::new(
            id,
            agent.kind.clone(),
            PermissionMode::Auto,
            "task".into(),
            dir.path().into(),
        );
        record.agent_name = agent.name.clone();
        let mut value = serde_json::to_value(record).unwrap();
        value["peer"] = serde_json::json!({"launch_id": "old-launch"});
        let record: RunRecord = serde_json::from_value(value.clone()).unwrap();
        crate::harness::run::create(store.paths(), &record).unwrap();
        assert!(
            newest_run_by_ref(&store, "peer", Some(&agent))
                .unwrap()
                .is_none()
        );
        assert!(
            newest_run_by_ref(&store, record.run_id.as_str(), None)
                .unwrap()
                .is_some()
        );
        value["peer"]["launch_id"] = serde_json::json!("launch");
        let mut record: RunRecord = serde_json::from_value(value).unwrap();
        record.agent_name = Some("old-name".into());
        crate::harness::run::create(store.paths(), &record).unwrap();
        assert_eq!(
            newest_run_for_agent(&store, &agent)
                .unwrap()
                .unwrap()
                .run_id,
            record.run_id
        );
        // A molt to another kind keeps the agent's launch, and its peer turn
        // with it.
        agent.kind = crate::ids::AgentKind::new_unchecked("claude");
        assert_eq!(
            newest_run_by_ref(&store, "old-name", Some(&agent))
                .unwrap()
                .map(|run| run.run_id),
            Some(record.run_id)
        );
    }

    #[test]
    fn an_unnamed_agent_matches_a_run_only_by_its_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let paths = crate::disk::paths::StatePaths::under(id.clone(), dir.path()).unwrap();
        let runtime = crate::disk::paths::RuntimePaths::under(id.clone(), dir.path()).unwrap();
        let store = crate::Store::open(paths, runtime).unwrap();
        let agent = AgentState::stub("claude", "session", AgentStatus::Idle);
        let mut record = RunRecord::new(
            id,
            agent.kind.clone(),
            PermissionMode::Auto,
            "task".into(),
            dir.path().into(),
        );
        record.agent_id = Some("other-session".into());
        crate::harness::run::create(store.paths(), &record).unwrap();
        assert!(newest_run_for_agent(&store, &agent).unwrap().is_none());
        record.agent_id = Some(agent.agent_id.clone());
        crate::harness::run::create(store.paths(), &record).unwrap();
        assert_eq!(
            newest_run_for_agent(&store, &agent)
                .unwrap()
                .map(|run| run.run_id),
            Some(record.run_id)
        );
    }
}
