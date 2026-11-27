//! Self waits: arming, listing, and canceling an agent's own shells and watches over the loop
//! scheduler, and the rows `rimz shell`, `rimz watch` and `rimz loop list` show for each
//! wake-up. Each function takes the caller, the room, and the directory it acts in as arguments
//! and returns a value; the CLI handler and the MCP server present it.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use serde::Serialize;

use crate::agents::AgentState;
use crate::config::{CheckOn, MachineConfig, TaskEntry, TaskTarget, WatchSpec};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::harness::ancestry::CallerIdentity;
use crate::harness::schedule::arm::{
    ArmOutcome, DeliveryName, DeliveryPrompt, DeliveryProvenance, DeliverySpec, DeliveryTrigger,
    SubscriptionLifetime, TaskName, arm_delivery, duration_label,
};
use crate::harness::schedule::arming::{self, ArmState};
use crate::harness::schedule::catalog::TaskCatalog;
use crate::harness::schedule::pending::session_deliveries;
use crate::harness::schedule::signal::{stop_watcher, watcher_info};
use crate::harness::schedule::window::{WindowRefusal, window_condition_provider};
use crate::harness::schedule::{
    self, DeliveryOwner, ScheduleErr, TaskKind, Trigger, delivery_owner,
};
use crate::harness::when::{self, CiSource, Verdict, WhenExpr, WindowReadings};
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::snapshot::SidebarSnapshot;
use crate::theme::fmt::checkout_name;
use crate::utils::time::parse_compact_duration;
use crate::{ResolvedWorkspace, Store};

/// Whether a room's sidebar is fresh enough to read forge state, as the sidebar decides it;
/// the caller passes `sidebar::fresh_sidebar_present`, which `harness` cannot reach.
pub type SidebarFresh = fn(&RuntimePaths) -> bool;

/// The room a wait call acts on, as its caller opened it.
pub struct WaitRoom<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub store: &'a Store,
    /// The resolution snapshot the caller was found in.
    pub snapshot: &'a SidebarSnapshot,
    pub sidebar_fresh: SidebarFresh,
}

/// One self wait to arm: a shell or a watch, and its options.
#[derive(Debug)]
pub struct WaitRequest {
    pub label: Option<String>,
    pub trigger: WaitTrigger,
    /// One still-running notice after this long; never a stop.
    pub check_in: Option<Duration>,
}

/// What a self wait waits on.
#[derive(Debug)]
pub enum WaitTrigger {
    /// A command run once through `sh -c`, waking on its exit when the outcome matches `on`.
    Shell { command: String, on: CheckOn },
    /// A condition expression, waking once it reads true.
    Watch { when: String },
}

pub use super::pending::WaitKind;

/// What arming a wait did, as `rimz shell --json` and `rimz watch --json` print it before the
/// pending rows.
#[derive(Serialize)]
pub struct WaitArmed {
    pub name: String,
    /// The trigger as its listing words it.
    #[serde(rename = "trigger")]
    pub description: String,
    /// The handle of the agent the wake comes back to.
    pub target: String,
    /// Set when an identical condition wait already existed and nothing was armed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing: Option<bool>,
    /// The still-running notice a watched trigger sends, if any.
    #[serde(skip)]
    pub check_in: Option<Duration>,
    /// The armed task's entry; none when an identical wait already existed.
    #[serde(skip)]
    pub entry: Option<TaskEntry>,
    /// The condition a `when` wait reads.
    #[serde(skip)]
    pub condition: Option<WhenExpr>,
}

/// Which waits a cancel removes.
pub enum CancelTarget {
    /// One wait, by the name its listing shows.
    Name(TaskName),
    /// Every wait of the kind the caller armed.
    AllWaits,
}

/// What a cancel removed and the caller's rows that remain.
#[derive(Serialize)]
pub struct WaitsCanceled {
    pub canceled: Vec<String>,
    pub pending: Vec<WakeRow>,
}

pub fn runtime_for_root(root: &Path) -> Option<RuntimePaths> {
    let state = StatePaths::for_project_root(root).ok()?;
    RuntimePaths::for_state(&state).ok()
}

/// A room's condition source, read once per list and shared by each row's reading.
pub struct RoomSource {
    pub runtime: Option<RuntimePaths>,
    pub room_open: bool,
    pub source: Option<CiSource>,
}

impl RoomSource {
    /// Read the room at `root`; `sidebar_fresh` says whether its sidebar is fresh enough
    /// to read forge state.
    pub fn read(root: &Path, sidebar_fresh: SidebarFresh) -> Self {
        let runtime = runtime_for_root(root);
        let room_open = runtime.as_ref().is_some_and(sidebar_fresh);
        let source = runtime.as_ref().filter(|_| room_open).map(CiSource::read);
        Self {
            runtime,
            room_open,
            source,
        }
    }
}

pub struct ConditionReading {
    pub verdict: Verdict,
    /// Each forge term that reads nothing, with its reason (`Verdict::unread_reasons`).
    pub unread: BTreeMap<String, String>,
    /// When each window holding the condition back next resets (`WindowReadings::resets`),
    /// on the windows `verdict` read.
    pub resets: BTreeMap<String, Timestamp>,
}

/// Read `expr` for `entry` under the freshness rule firing applies, against the task's fire
/// stamp when it has one. `room` is the entry's room, read once by the caller.
pub fn read_condition(
    room: &RoomSource,
    entry: &TaskEntry,
    expr: &WhenExpr,
    fired_at: Option<Timestamp>,
) -> ConditionReading {
    let scope = entry.run_dir();
    let source = room.source.as_ref();
    let now = Timestamp::now();
    let windows = WindowReadings::new(room.runtime.as_ref(), now);
    let verdict = when::evaluate(
        expr,
        &scope,
        source,
        entry.provider.as_ref(),
        entry.account.as_ref(),
        &windows,
    )
    .freshen(expr, &scope, source, fired_at, Some(now), true);
    let unread = verdict.unread_reasons(expr, &scope, source, Some(now));
    let resets = windows.resets(
        expr,
        entry.provider.as_ref(),
        entry.account.as_ref(),
        &verdict.readings,
    );
    ConditionReading {
        verdict,
        unread,
        resets,
    }
}

/// Refuse arming `expr` at `scope` in the room at `root` when the room's forge reading already
/// says the checkout has no supported forge: a term on it could never be read. A scope never
/// probed, a detached HEAD, and a closed room are not known to be unreadable, so they arm.
fn refuse_unreadable_forge(
    root: &Path,
    scope: &Path,
    expr: &WhenExpr,
    sidebar_fresh: SidebarFresh,
    field: &str,
) -> Result<()> {
    if !expr.reads_forge() {
        return Ok(());
    }
    let Some(source) = RoomSource::read(root, sidebar_fresh).source else {
        return Ok(());
    };
    let Some(term) = expr.terms().find(|term| {
        term.reads_forge()
            && source.unread_reason(&term.key, scope) == Some(when::UnreadReason::NoForge)
    }) else {
        return Ok(());
    };
    let reason = when::condition::unread_reason(Some(when::UnreadReason::NoForge), None);
    anyhow::bail!(
        "{field}: `{}` cannot be read in {}: {reason}; RimZ reads a git checkout whose `origin` \
         remote is on github.com (through `gh`) or on a host whose name contains gitea, forgejo \
         or codeberg (through `tea`), so wait on something else here",
        term.key,
        crate::theme::fmt::home_relative(&scope.to_string_lossy())
    )
}

/// Where a `when` is armed: the room at `root`, its forge terms read at `scope` (none for a
/// task that reads each worktree).
pub struct WhenPlace<'a> {
    pub root: &'a Path,
    pub scope: Option<&'a Path>,
    pub sidebar_fresh: SidebarFresh,
}

/// The account a `when`'s window terms read: `kind`'s windows on `login`, for the checkout at
/// `root`.
pub struct WindowAccount {
    pub kind: Option<String>,
    pub login: Option<crate::ids::LoginKey>,
    pub root: PathBuf,
    pub runtime: RuntimePaths,
}

/// A `when` as it arms: its expression and the provider whose windows it reads.
pub struct ArmedWhen {
    pub expr: WhenExpr,
    pub provider: Option<AgentKind>,
}

/// The one reading of a `when` every surface arms (`rimz watch`, `rimz loop add`, the `watch`
/// and `schedule` tools): parsed against the room's team stages, refused when it reads a forge
/// the checkout has none of, and its window terms resolved to a provider. `account` runs only
/// when the clauses read a window. Refusals name the clauses as `field`; a window refusal keeps
/// its [`WindowRefusal`] type for a caller to reword.
pub fn read_when(
    clauses: &[String],
    field: &str,
    place: &WhenPlace<'_>,
    account: impl FnOnce() -> Result<WindowAccount>,
) -> Result<ArmedWhen> {
    let stages = when::condition::condition_stages(place.root)?;
    let expr = WhenExpr::parse_with_stages(clauses, Some(&stages))
        .map_err(|error| anyhow!("{field}: {error}"))?;
    if let Some(scope) = place.scope {
        refuse_unreadable_forge(place.root, scope, &expr, place.sidebar_fresh, field)?;
    }
    if expr.window_spans().next().is_none() {
        return Ok(ArmedWhen {
            expr,
            provider: None,
        });
    }
    let account = account()?;
    let provider = window_condition_provider(
        clauses,
        account.kind.as_deref(),
        account.login.as_ref(),
        &account.root,
        &account.runtime,
        Timestamp::now(),
    )?;
    Ok(ArmedWhen { expr, provider })
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WakeState {
    Pending,
    Waiting,
    Watching,
    Starting,
    Lost,
    Disabled,
    Paused,
}

#[derive(Serialize)]
pub struct WakeRow {
    pub name: String,
    #[serde(rename = "type")]
    kind: &'static str,
    pub state: WakeState,
    pub target: String,
    trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dir: Option<String>,
    elapsed: Option<String>,
    elapsed_s: Option<u64>,
    check_in: Option<String>,
    check_in_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    watcher_pid: Option<u32>,
    owner: String,
    parent: Option<String>,
    /// Each unread forge key of a live condition with its reason.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    unread: BTreeMap<String, String>,
    #[serde(skip)]
    pub entry: TaskEntry,
    #[serde(skip)]
    pub arm_state: ArmState,
}

impl WakeRow {
    #[expect(
        clippy::too_many_arguments,
        reason = "each list reads the snapshot, audit rows, and room source once for every row"
    )]
    pub fn new(
        name: &str,
        entry: &TaskEntry,
        arm_state: ArmState,
        watcher_pid: Option<u32>,
        snapshot: Option<&SidebarSnapshot>,
        audit: &[&AgentState],
        room: &RoomSource,
        now: Timestamp,
    ) -> Self {
        // The row keeps its target as the key its holder files it under now,
        // read over `audit`, so an older session target reads as its launch's.
        let mut entry = entry.clone();
        if let Some(target) = &mut entry.wait {
            (target.kind, target.session) =
                schedule::pending::holder_key(audit.iter().copied(), &target.kind, &target.session);
        }
        let entry = &entry;
        let parsed = schedule::parse_trigger(name, entry);
        let delay = entry
            .wait_meta
            .as_ref()
            .and_then(|meta| meta.delay.as_deref());
        let (kind, state, timeout) = match parsed.as_ref().map(|parsed| &parsed.trigger) {
            Ok(Trigger::Watch(spec)) => (
                spec.kind(),
                if watcher_pid.is_some() {
                    WakeState::Watching
                } else if schedule::watch_lost(entry, false, now) {
                    WakeState::Lost
                } else {
                    WakeState::Starting
                },
                entry.timeout.as_deref().and_then(|raw| {
                    parse_compact_duration(raw)
                        .ok()
                        .map(|value| (duration_label(value), value.as_secs()))
                }),
            ),
            Ok(Trigger::Schedule(_)) if delay.is_some() => (
                "timer",
                WakeState::Pending,
                delay.and_then(|raw| {
                    parse_compact_duration(raw)
                        .ok()
                        .map(|value| (raw.to_owned(), value.as_secs()))
                }),
            ),
            Ok(Trigger::Signal { .. }) => ("signal", WakeState::Waiting, None),
            Ok(Trigger::Condition { .. }) => ("condition", WakeState::Waiting, None),
            Ok(Trigger::Schedule(_)) | Err(_) => ("schedule", WakeState::Waiting, None),
        };
        let meta = match parsed.as_ref().map(|parsed| &parsed.trigger) {
            Ok(Trigger::Schedule(_)) if delay.is_some() => entry.wait_meta.as_ref(),
            _ => entry.watch.as_ref().and(entry.wait_meta.as_ref()),
        };
        let dir = entry
            .dir
            .as_deref()
            .map(|dir| crate::theme::fmt::home_relative(&dir.to_string_lossy()));
        let mut trigger = match &parsed {
            Ok(parsed) => match &parsed.trigger {
                Trigger::Watch(spec) => {
                    let subject = entry.label.clone().unwrap_or_else(|| spec.subject());
                    dir.as_ref()
                        .map_or_else(|| subject.clone(), |dir| format!("{subject} · in {dir}"))
                }
                _ => parsed.describe(),
            },
            Err(error) => error.to_string(),
        };
        let state = match arm_state {
            ArmState::Live => state,
            ArmState::Disabled(_) => WakeState::Disabled,
            ArmState::Paused(until) => {
                trigger.push_str(&format!(
                    " · resumes {}",
                    crate::theme::fmt::rel_until(until, now)
                ));
                WakeState::Paused
            }
        };
        let unread = match parsed.as_ref().map(|parsed| &parsed.trigger) {
            Ok(Trigger::Condition { expr, .. }) if state == WakeState::Waiting => {
                let fired_at = room
                    .runtime
                    .as_ref()
                    .and_then(|runtime| schedule::fire::last_stamps(runtime).get(name).copied());
                read_condition(room, entry, expr, fired_at).unread
            }
            _ => BTreeMap::new(),
        };
        let peers = snapshot
            .map(crate::address::addressable_agents)
            .unwrap_or_default();
        let parent = entry.wait.as_ref().and_then(|target| {
            let agent = target_agent(&peers, target)?;
            let parent = crate::address::launched_parent(peers.iter().copied(), agent)?;
            Some(crate::address::agent_handle(parent, &peers, true))
        });
        Self {
            name: name.to_owned(),
            kind,
            state,
            target: entry
                .wait
                .as_ref()
                .map(|target| target.handle.clone())
                .unwrap_or_default(),
            trigger,
            label: entry.label.clone(),
            dir,
            elapsed: meta.map(|meta| crate::theme::fmt::age_short(meta.armed_at, now)),
            elapsed_s: meta.map(|meta| now.duration_since(meta.armed_at).as_secs().max(0) as u64),
            check_in_s: timeout.as_ref().map(|(_, seconds)| *seconds),
            check_in: timeout.map(|(label, _)| label),
            watcher_pid,
            owner: delivery_owner(name, entry).to_string(),
            parent,
            unread,
            entry: entry.clone(),
            arm_state,
        }
    }

    /// The key the row's target files under, as [`Self::new`] resolved it.
    pub fn target_key(&self) -> Option<(AgentKind, AgentSessionId)> {
        let target = self.entry.wait.as_ref()?;
        Some((target.kind.clone(), target.session.clone()))
    }

    pub fn is_self(&self) -> bool {
        self.owner() == DeliveryOwner::SelfWait
    }

    pub fn owner(&self) -> DeliveryOwner<'_> {
        delivery_owner(&self.name, &self.entry)
    }

    pub fn past_check_in(&self) -> bool {
        match self.state {
            WakeState::Watching => self
                .elapsed_s
                .zip(self.check_in_s)
                .is_some_and(|(elapsed, limit)| elapsed >= limit),
            WakeState::Pending
            | WakeState::Waiting
            | WakeState::Starting
            | WakeState::Lost
            | WakeState::Disabled
            | WakeState::Paused => false,
        }
    }

    pub fn priority(&self) -> (u8, Reverse<u64>, &str) {
        (
            match self.state {
                WakeState::Lost => 0,
                WakeState::Watching if self.past_check_in() => 1,
                WakeState::Pending
                | WakeState::Waiting
                | WakeState::Watching
                | WakeState::Starting
                | WakeState::Disabled
                | WakeState::Paused => 2,
            },
            Reverse(self.elapsed_s.unwrap_or(0)),
            &self.name,
        )
    }

    pub fn wait_state(&self, now: &jiff::ZonedDateTime) -> String {
        match self.state {
            WakeState::Watching => {
                let mut state = format!("watching {}", self.elapsed.as_deref().unwrap_or("0s"));
                if let Some(limit) = self.check_in_s {
                    if self.past_check_in() {
                        state.push_str(&format!(
                            " · past check-in at {}",
                            self.check_in.as_deref().unwrap_or("0s")
                        ));
                    } else {
                        state.push_str(&format!(
                            " · check-in in {}",
                            crate::theme::fmt::age_label(
                                limit.saturating_sub(self.elapsed_s.unwrap_or(0))
                            )
                        ));
                    }
                }
                state
            }
            WakeState::Lost => format!(
                "lost · armed {} ago",
                self.elapsed.as_deref().unwrap_or("0s")
            ),
            WakeState::Pending => schedule::parse_trigger(&self.name, &self.entry)
                .ok()
                .and_then(|parsed| parsed.next_after(now))
                .map_or_else(
                    || "pending".into(),
                    |next| {
                        format!(
                            "pending · fires {}",
                            crate::theme::fmt::rel_until(next, now.timestamp())
                        )
                    },
                ),
            WakeState::Paused => match self.arm_state {
                ArmState::Paused(until) => {
                    format!(
                        "paused · resumes {}",
                        crate::theme::fmt::rel_until(until, now.timestamp())
                    )
                }
                _ => "paused".into(),
            },
            WakeState::Waiting if !self.unread.is_empty() => format!(
                "waiting · {}",
                self.unread
                    .iter()
                    .map(|(key, reason)| format!("{key}: {reason}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            WakeState::Waiting => "waiting".into(),
            WakeState::Starting => "starting".into(),
            WakeState::Disabled => "disabled".into(),
        }
    }

    /// A watch's label replaces its command preview; a clock or condition row keeps the
    /// time or expression and adds its label after them.
    pub fn subject(&self, current_root: &Path) -> String {
        let subject = match &self.entry.watch {
            Some(spec) => self.label.clone().unwrap_or_else(|| spec.subject()),
            None => {
                let trigger = schedule::parse_trigger(&self.name, &self.entry)
                    .map_or_else(|error| error.to_string(), |parsed| parsed.describe());
                match &self.label {
                    Some(label) => format!("{trigger} · {label}"),
                    None => trigger,
                }
            }
        };
        format!("{subject}{}", self.worktree_suffix(current_root))
    }

    fn worktree_suffix(&self, current_root: &Path) -> String {
        if self.entry.dir.is_none() || self.entry.run_dir() == current_root {
            return String::new();
        }
        worktree_name(&self.entry, &self.entry.run_dir())
            .map_or_else(String::new, |name| format!(" · in {name}"))
    }
}

/// The row of the agent a wait target names, among one row per agent.
pub fn target_agent<'a>(
    peers: &[&'a AgentState],
    target: &crate::config::TaskTarget,
) -> Option<&'a AgentState> {
    peers
        .iter()
        .copied()
        .find(|agent| schedule::pending::wait_for(&target.kind, &target.session, agent))
}

pub fn worktree_name(entry: &TaskEntry, run_dir: &Path) -> Option<String> {
    if let Some(worktree) = &entry.worktree {
        return Some(worktree.clone());
    }
    (entry.wait.is_some() || entry.dir.is_some()).then(|| {
        entry
            .dir
            .as_ref()
            .map(|_| checkout_name(run_dir))
            .or_else(|| {
                entry.wait.as_ref().and_then(|target| {
                    target
                        .handle
                        .split_once('#')
                        .map(|(_, channel)| channel.to_owned())
                })
            })
            .unwrap_or_else(|| checkout_name(run_dir))
    })
}

/// The calling agent's live row in `snapshot`, or none when the caller is no agent. `action`
/// names what a refusal left undone (`armed`, `canceled`), and `kind` the verb a sandboxed
/// caller is told to run on its own.
pub fn caller_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    caller: Option<&CallerIdentity>,
    kind: WaitKind,
    action: Option<&str>,
) -> Result<Option<&'a AgentState>> {
    let Some(caller) = caller else {
        return Ok(None);
    };
    let agent = crate::harness::ancestry::resolve_launch_caller(&snapshot.agents, caller).map_err(
        |_| {
            let identity = match (&caller.launch_id, &caller.pane_id) {
                (Some(launch_id), _) => format!("launch id `{launch_id}`"),
                (None, Some(pane_id)) => format!("pane `{pane_id}`"),
                (None, None) => "no launch id or pane".into(),
            };
            let unchanged = action
                .map(|action| format!("nothing was {action}: "))
                .unwrap_or_default();
            anyhow!(
                "{unchanged}RimZ could not find the calling agent's live session from this \
                 process (looked for kind `{}`, {identity}). If this ran inside a provider's \
                 command sandbox: {}",
                caller.kind,
                crate::harness::home_write::sandbox_remedy(kind.verb())
            )
        },
    )?;
    if agent.ended_at.is_some() {
        bail!("RimZ identified the calling agent but its live session is unavailable");
    }
    Ok(Some(agent))
}

/// The agent arming a wait: an identified caller with a registered session.
pub fn arming_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    caller: Option<&CallerIdentity>,
    kind: WaitKind,
) -> Result<&'a AgentState> {
    let agent = caller_agent(snapshot, caller, kind, Some("armed"))?.with_context(|| {
        format!(
            "arming a {} is only available to an agent RimZ can identify; run this command \
             from an agent pane",
            kind.as_str()
        )
    })?;
    if agent.agent_id.is_provisional() {
        bail!("the calling agent has not registered a real session yet");
    }
    Ok(agent)
}

/// The agent canceling a wait: an identified caller.
pub fn canceling_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    caller: Option<&CallerIdentity>,
    kind: WaitKind,
) -> Result<&'a AgentState> {
    caller_agent(snapshot, caller, kind, Some("canceled"))?.with_context(|| {
        format!(
            "canceling a {} requires an agent RimZ can identify; run this command from an \
             agent pane",
            kind.as_str()
        )
    })
}

/// The waits of `kind` that `agent` armed, or every one of the room when the caller is no
/// agent.
pub fn list(room: &WaitRoom, agent: Option<&AgentState>, kind: WaitKind) -> Result<Vec<WakeRow>> {
    match agent {
        Some(agent) => pending_rows(room, Some(agent), kind)
            .map_err(|error| home_write_hint(error, kind, None)),
        None => room_rows(room, kind),
    }
}

/// Arm the wait `request` describes for `agent`, which the wake comes back to. `published`
/// reads the room's published snapshot, which a wait without a check-in consults for the
/// agent's cache keepalive.
pub fn arm(
    room: &WaitRoom,
    agent: &AgentState,
    request: WaitRequest,
    published: impl FnOnce() -> Result<SidebarSnapshot>,
) -> Result<WaitArmed> {
    request.validate()?;
    let request_kind = request.kind();
    let workspace = room.workspace;
    let check_in = match request.check_in {
        Some(check_in) => Some(check_in),
        None => {
            let config = MachineConfig::load_lenient();
            let published = published()?;
            let ping_agent = published
                .agents
                .iter()
                .find(|row| row.kind == agent.kind && row.agent_id == agent.agent_id)
                .unwrap_or(agent);
            (!(config.harness.cache_keepalive
                && config.harness.ping_lifetime(ping_agent).is_some()))
            .then_some(Duration::from_secs(30 * 60))
        }
    };
    let target = TaskTarget {
        kind: agent.kind.clone(),
        session: agent.launch_key().clone(),
        handle: crate::address::agent_handle(
            agent,
            &crate::address::addressable_agents(room.snapshot),
            true,
        ),
    };
    let mut condition = None;
    let (trigger, description) = match request.trigger {
        WaitTrigger::Watch { when } => {
            let clauses = vec![when];
            let place = WhenPlace {
                root: &workspace.project_root,
                scope: Some(&workspace.worktree_root),
                sidebar_fresh: room.sidebar_fresh,
            };
            let armed = read_when(&clauses, "when", &place, || {
                let runtime = RuntimePaths::for_project_root(&workspace.project_root)?;
                let login = crate::harness::task_login::task_login(
                    &crate::store::writer::LaunchLogin::RoomDefault,
                    agent.kind.as_str(),
                    &runtime,
                )?
                .map(|login| login.key());
                Ok(WindowAccount {
                    kind: Some(agent.kind.as_str().to_owned()),
                    login,
                    root: workspace.worktree_root.clone(),
                    runtime,
                })
            })
            .map_err(|error| match error.downcast::<WindowRefusal>() {
                Ok(refusal) => window_refusal(refusal),
                Err(error) => error,
            })?;
            let description = format!("when {}", armed.expr);
            condition = Some(armed.expr.clone());
            (
                DeliveryTrigger::Condition {
                    expr: armed.expr,
                    hold: None,
                    lifetime: SubscriptionLifetime::Once,
                    check_in,
                },
                description,
            )
        }
        WaitTrigger::Shell { command, on } => {
            let spec = WatchSpec::Command(command);
            let description = spec.describe();
            (
                DeliveryTrigger::Watch {
                    spec,
                    on,
                    timeout: check_in,
                },
                description,
            )
        }
    };
    let outcome = arm_delivery(
        workspace,
        DeliverySpec {
            name: DeliveryName::Mint,
            label: request.label,
            target: target.clone(),
            trigger,
            prompt: DeliveryPrompt::None,
            provenance: DeliveryProvenance::SelfWait {
                reader: agent.name.clone(),
            },
            deadline: None,
            max_strikes: None,
        },
    )
    .map_err(|error| home_write_hint(error.into(), request_kind, Some("armed")))?;
    let (name, entry) = match outcome {
        ArmOutcome::Armed { name, task } => (name, Some(task.entry().clone())),
        ArmOutcome::AlreadySubscribed { name } => (name, None),
    };
    Ok(WaitArmed {
        name,
        description,
        target: target.handle,
        existing: entry.is_none().then_some(true),
        check_in,
        entry,
        condition,
    })
}

/// Cancel `target` among `agent`'s rows of `kind`, stopping each shell's command, and list
/// what remains.
pub fn cancel(
    room: &WaitRoom,
    agent: &AgentState,
    kind: WaitKind,
    target: CancelTarget,
) -> Result<WaitsCanceled> {
    let pending = pending_rows(room, Some(agent), kind)?;
    let names = match target {
        CancelTarget::AllWaits => pending
            .into_iter()
            .filter(WakeRow::is_self)
            .map(|row| row.name)
            .collect::<Vec<_>>(),
        CancelTarget::Name(name) => {
            let name = name.to_string();
            if !pending.iter().any(|row| row.name == name) {
                bail!(
                    "no pending {} named `{name}`; see `{} list`",
                    kind.as_str(),
                    kind.verb()
                );
            }
            vec![name]
        }
    };
    let root = &room.workspace.project_root;
    let catalog = TaskCatalog::load(Some(root))?;
    for name in &names {
        let shell = catalog
            .visible()
            .get(name.as_str())
            .filter(|_| kind == WaitKind::Shell)
            .map(|task| canceled_shell(name, task));
        catalog
            .remove(name)
            .map_err(|error| home_write_hint(error, kind, Some("canceled")))?;
        if let Some(record) = shell {
            super::run_log::record_stopped(root, &record, Some(super::TaskKind::WakeUp));
        }
        stop_watcher(room.store.runtime_paths(), name)?;
    }
    Ok(WaitsCanceled {
        canceled: names,
        pending: pending_rows(room, Some(agent), kind)?,
    })
}

/// The history record of canceling shell `name`, appended once its row is gone: under its task
/// record, bound now for a shell that never fired so `rimz loop logs` finds it once the removal
/// retires it, and naming the agent it woke, by which a `join` reads the cancel as its end.
fn canceled_shell(name: &str, task: &super::catalog::LoadedTask) -> super::run_log::LoopRunRecord {
    let binding = task.binding(name);
    let mut record = super::run_log::LoopRunRecord::new(
        name,
        super::run_log::LoopRunResult::Canceled,
        super::run_log::LoopRunMode::Manual,
        0,
    );
    record.task_id = super::task_record_store::mutate(|records| records.bind(&binding).0)
        .inspect_err(
            |err| tracing::warn!(task = name, error = %err, "cancel: binding the task record"),
        )
        .ok();
    record.wakes = task.entry().wait.clone();
    record
}

/// What a `join` reads and writes of one shell: its row, its claim, and its history.
pub struct ShellJoin {
    paths: StatePaths,
    root: PathBuf,
    name: String,
    armed_at: Timestamp,
    target: TaskTarget,
    /// The history log's length and modification time at the last read that found no outcome.
    unchanged: Option<(u64, std::time::SystemTime)>,
}

impl ShellJoin {
    /// The shell `name` in the room at `root`; `None` once its row is gone.
    pub fn open(root: &Path, name: &str) -> Result<Option<Self>> {
        let catalog = TaskCatalog::load(Some(root))?;
        let Some((armed_at, target)) = catalog.visible().get(name).and_then(|task| {
            let entry = task.entry();
            Some((entry.wait_meta.as_ref()?.armed_at, entry.wait.clone()?))
        }) else {
            return Ok(None);
        };
        Ok(Some(Self {
            paths: StatePaths::for_project_root(root)?,
            root: root.to_owned(),
            name: name.to_owned(),
            armed_at,
            target,
            unchanged: None,
        }))
    }

    /// The agent the shell's outcome goes to.
    pub fn target(&self) -> &TaskTarget {
        &self.target
    }

    /// Claim the shell's outcome for a join until `until`, or drop the claim; `false` once
    /// its row is gone.
    pub fn claim(&self, until: Option<Timestamp>) -> Result<bool> {
        Ok(crate::harness::schedule::instances::claim_join(
            &self.paths,
            &self.name,
            until,
        )?)
    }

    /// Whether the shell's row is still there.
    pub fn present(&self) -> bool {
        crate::harness::schedule::instances::load_from(&self.paths.root)
            .0
            .contains_key(&self.name)
    }

    /// The history record of how this arming ended, once one is written. The history is read
    /// again only once the log has changed since a read that found none.
    pub fn ended(&mut self) -> Option<crate::harness::schedule::run_log::LoopRunRecord> {
        let stamp = crate::harness::schedule::run_log::log_stamp();
        if stamp.is_some() && stamp == self.unchanged {
            return None;
        }
        let ended = last_outcome_since(&self.name, &self.root, self.armed_at);
        if ended.is_none() {
            self.unchanged = stamp;
        }
        ended
    }

    /// The newest record that ended an arming of shell `name` in the room at `root`, read once
    /// its row is gone.
    pub fn last_outcome(
        root: &Path,
        name: &str,
    ) -> Option<crate::harness::schedule::run_log::LoopRunRecord> {
        last_outcome_since(name, root, Timestamp::MIN)
    }
}

/// The newest record of row `name` at `root` since `since` that ended an arming.
fn last_outcome_since(
    name: &str,
    root: &Path,
    since: Timestamp,
) -> Option<crate::harness::schedule::run_log::LoopRunRecord> {
    crate::harness::schedule::run_log::named_records_since(name, root, since)
        .into_iter()
        .rev()
        .find(crate::harness::schedule::run_log::LoopRunRecord::ends_arming)
}

/// The rows of the waits of `kind` that wake `agent`.
pub fn pending_rows(
    room: &WaitRoom,
    agent: Option<&AgentState>,
    kind: WaitKind,
) -> Result<Vec<WakeRow>> {
    let root = &room.workspace.project_root;
    let catalog = TaskCatalog::load(Some(root))?;
    let arming = arming::load()?;
    let now = Timestamp::now();
    let audit = room.store.audit_rows()?;
    let rows = audit.iter().collect::<Vec<_>>();
    let source = RoomSource::read(root, room.sidebar_fresh);
    session_deliveries(&catalog, root, agent, &rows)
        .filter(|(_, task)| WaitKind::of(task.entry()) == kind)
        .map(|(name, task)| {
            let entry = task.entry();
            let parsed = task.trigger().as_ref().map_err(Clone::clone)?;
            let duration = match &parsed.trigger {
                Trigger::Watch(_) => entry.timeout.as_deref(),
                Trigger::Schedule(_) => entry
                    .wait_meta
                    .as_ref()
                    .and_then(|meta| meta.delay.as_deref()),
                Trigger::Signal { .. } | Trigger::Condition { .. } => None,
            };
            if let Some(duration) = duration {
                parse_compact_duration(duration).map_err(anyhow::Error::msg)?;
            }
            let state = ArmState::resolve(arming.get(&task.key(name)), task.source(), now);
            let pid = if entry.watch.is_some() {
                watcher_info(room.store.runtime_paths(), name)?.map(|info| info.pid)
            } else {
                None
            };
            Ok(WakeRow::new(
                name,
                entry,
                state,
                pid,
                Some(room.snapshot),
                &rows,
                &source,
                now,
            ))
        })
        .collect()
}

fn room_rows(room: &WaitRoom, kind: WaitKind) -> Result<Vec<WakeRow>> {
    let root = &room.workspace.project_root;
    let catalog = TaskCatalog::load_room(root)?;
    let arming = arming::load()?;
    let now = Timestamp::now();
    let audit = room.store.audit_rows()?;
    let rows = audit.iter().collect::<Vec<_>>();
    let source = RoomSource::read(root, room.sidebar_fresh);
    Ok(catalog
        .visible()
        .iter()
        .filter(|(_, task)| {
            task.kind() == TaskKind::WakeUp
                && task.entry().resolved_root() == *root
                && WaitKind::of(task.entry()) == kind
        })
        .map(|(name, task)| {
            let entry = task.entry();
            let state = ArmState::resolve(arming.get(&task.key(name)), task.source(), now);
            let pid = entry
                .watch
                .as_ref()
                .and_then(|_| {
                    watcher_info(room.store.runtime_paths(), name)
                        .ok()
                        .flatten()
                })
                .map(|info| info.pid);
            WakeRow::new(
                name,
                entry,
                state,
                pid,
                Some(room.snapshot),
                &rows,
                &source,
                now,
            )
        })
        .collect())
}

impl WaitRequest {
    /// Refuse a request whose command or expression is blank, or whose check-in is out of range.
    pub fn validate(&self) -> Result<()> {
        match &self.trigger {
            WaitTrigger::Shell { command, .. } if command.trim().is_empty() => {
                bail!("a shell needs a command")
            }
            WaitTrigger::Watch { when } if when.trim().is_empty() => {
                bail!("a watch needs an expression")
            }
            WaitTrigger::Shell { .. } | WaitTrigger::Watch { .. } => {}
        }
        if self.check_in.is_some_and(|duration| duration.is_zero()) {
            bail!("--check-in must be greater than zero");
        }
        if self
            .check_in
            .is_some_and(|duration| duration >= Duration::from_secs(24 * 60 * 60))
        {
            bail!("--check-in must be less than 24h");
        }
        Ok(())
    }

    /// Whether the request arms a shell or a watch.
    pub fn kind(&self) -> WaitKind {
        match self.trigger {
            WaitTrigger::Shell { .. } => WaitKind::Shell,
            WaitTrigger::Watch { .. } => WaitKind::Watch,
        }
    }
}

/// `at` refusals worded for the field: a schedule has no name before it arms.
pub fn at_refusal(error: ScheduleErr, field: &str) -> anyhow::Error {
    match error {
        ScheduleErr::BadOnceAt { value, .. } => anyhow!(
            "{field} `{value}` is not a time; use `HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM`"
        ),
        ScheduleErr::PastOnceAt { value, .. } => {
            anyhow!("{field} `{value}` is in the past; choose a later time")
        }
        other => other.into(),
    }
}

/// Window refusals worded for a wait: the loop wording speaks of a task's run.
fn window_refusal(error: WindowRefusal) -> anyhow::Error {
    match error {
        WindowRefusal::ManagedAccount { kind } => anyhow!(
            "{kind} chooses its account per launch, so the room has no {kind} account whose \
             windows a wait can read; window.* terms do not support {kind}"
        ),
        other => other.into(),
    }
}

fn home_write_hint(error: anyhow::Error, kind: WaitKind, action: Option<&str>) -> anyhow::Error {
    crate::harness::home_write::hint(error, kind.verb(), action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_managed_account_refusal_names_the_wait_flags_not_a_task_run() {
        let message = window_refusal(WindowRefusal::ManagedAccount {
            kind: "qwen".into(),
        })
        .to_string();
        assert_eq!(
            message,
            "qwen chooses its account per launch, so the room has no qwen account whose windows \
             a wait can read; window.* terms do not support qwen"
        );
        for wrong in ["before the run", "window triggers", "task"] {
            assert!(!message.contains(wrong), "{message}");
        }
    }

    /// A join's poll reads the history again only once the log's length or modification time
    /// moved since a read that found no outcome.
    #[test]
    fn a_join_rereads_the_history_only_once_the_log_changes() {
        if super::super::test_homes::reexec_with_private_homes(
            "harness::schedule::wait::tests::a_join_rereads_the_history_only_once_the_log_changes",
        ) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let target = TaskTarget {
            kind: AgentKind::new_unchecked("claude"),
            session: "session".into(),
            handle: "@coder".to_owned(),
        };
        let mut canceled = super::super::run_log::LoopRunRecord::new(
            "shell-b",
            super::super::run_log::LoopRunResult::Canceled,
            super::super::run_log::LoopRunMode::Manual,
            0,
        );
        canceled.wakes = Some(target.clone());
        let mut join = ShellJoin {
            paths: StatePaths::for_project_root(root.path()).unwrap(),
            root: root.path().to_owned(),
            name: "shell-a".to_owned(),
            armed_at: canceled.at,
            target,
            unchanged: None,
        };
        super::super::run_log::record_stopped(root.path(), &canceled, Some(TaskKind::WakeUp));
        assert!(join.ended().is_none(), "another shell's outcome");
        // The same bytes but for the name, so the log keeps its length and modification time.
        let log = crate::disk::paths::logs_dir().join("loop-runs.log.jsonl");
        let modified = std::fs::metadata(&log).unwrap().modified().unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        std::fs::write(&log, text.replace("shell-b", "shell-a")).unwrap();
        let touch = |at| {
            std::fs::File::options()
                .write(true)
                .open(&log)
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        touch(modified);
        assert!(join.ended().is_none(), "an unchanged log is read again");
        touch(modified + Duration::from_secs(1));
        assert_eq!(
            join.ended().map(|record| record.task),
            Some("shell-a".to_owned())
        );
    }
}
