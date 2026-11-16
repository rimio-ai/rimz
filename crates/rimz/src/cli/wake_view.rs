//! Shared self-wait lines, room cards, subscription summaries, and condition readings.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use jiff::Timestamp;
use rimz::agents::AgentState;
use rimz::config::TaskEntry;
use rimz::disk::paths::{RuntimePaths, StatePaths};
use rimz::harness::schedule::arm::duration_label;
use rimz::harness::schedule::arming::ArmState;
use rimz::harness::schedule::runner::parse_task_timeout;
use rimz::harness::schedule::{self, DeliveryOwner, TaskActionKind, Trigger, delivery_owner};
use rimz::harness::when::{self, CiSource, Verdict, WhenExpr, WindowReadings};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::snapshot::SidebarSnapshot;
use serde::Serialize;

use super::render as ui;

pub(super) fn runtime_for_root(root: &Path) -> Option<RuntimePaths> {
    let state = StatePaths::for_project_root(root).ok()?;
    RuntimePaths::for_state(&state).ok()
}

/// Whether a room for `root` runs a sidebar fresh enough to read forge state.
pub(super) fn room_open(root: &Path) -> bool {
    runtime_for_root(root)
        .as_ref()
        .is_some_and(rimz::sidebar::fresh_sidebar_present)
}

/// A room's condition source, read once per list and shared by each row's reading.
pub(super) struct RoomSource {
    runtime: Option<RuntimePaths>,
    pub(super) room_open: bool,
    pub(super) source: Option<CiSource>,
}

impl RoomSource {
    pub(super) fn read(root: &Path) -> Self {
        let runtime = runtime_for_root(root);
        let room_open = room_open(root);
        let source = runtime.as_ref().filter(|_| room_open).map(CiSource::read);
        Self {
            runtime,
            room_open,
            source,
        }
    }
}

pub(super) struct ConditionReading {
    pub(super) verdict: Verdict,
    /// Each forge term that reads nothing, with its reason (`Verdict::unread_reasons`).
    pub(super) unread: BTreeMap<String, String>,
}

/// Read `expr` for `entry` under the freshness rule firing applies, against the task's fire
/// stamp when it has one. `room` is the entry's room, read once by the caller.
pub(super) fn read_condition(
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
    ConditionReading { verdict, unread }
}

/// The `now:` state of a condition just armed: its readings while unmet, each unread key with
/// its reason, else `due`, or the start of its hold.
pub(super) fn condition_now(
    entry: &TaskEntry,
    expr: &WhenExpr,
    hold: Option<std::time::Duration>,
) -> String {
    let reading = read_condition(&RoomSource::read(&entry.resolved_root()), entry, expr, None);
    let state =
        schedule::TaskTimingState::unmet(expr, &reading.verdict).unwrap_or_else(|| match hold {
            Some(hold) => schedule::TaskTimingState::Holding {
                elapsed: std::time::Duration::ZERO,
                hold,
            },
            None => schedule::TaskTimingState::Due(Timestamp::now()),
        });
    state
        .condition_label_with(&reading.unread)
        .unwrap_or_default()
}

/// Refuse arming `expr` at `scope` in the room at `root` when the room's forge reading already
/// says the checkout has no supported forge: a term on it could never be read. A scope never
/// probed, a detached HEAD, and a closed room are not known to be unreadable, so they arm.
pub(super) fn refuse_unreadable_forge(root: &Path, scope: &Path, expr: &WhenExpr) -> Result<()> {
    if !expr.reads_forge() || !room_open(root) {
        return Ok(());
    }
    let Some(source) = runtime_for_root(root).as_ref().map(CiSource::read) else {
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
        "--when: `{}` cannot be read in {}: {reason}; RimZ reads a git checkout whose `origin` \
         remote is on github.com (through `gh`) or on a host whose name contains gitea, forgejo \
         or codeberg (through `tea`), so wait on something else here",
        term.key,
        ui::home_relative(&scope.to_string_lossy())
    )
}

/// [`refuse_unreadable_forge`] for a loop task about to be stored: its condition at its own
/// checkout. A task that reads each worktree has no single checkout to refuse on.
pub(super) fn refuse_unreadable_forge_task(entry: &TaskEntry) -> Result<()> {
    let Some(when) = entry.when.as_deref().filter(|_| !entry.each_worktree) else {
        return Ok(());
    };
    let Ok(expr) = WhenExpr::parse(when) else {
        return Ok(());
    };
    refuse_unreadable_forge(&entry.resolved_root(), &entry.run_dir(), &expr)
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum WakeState {
    Pending,
    Waiting,
    Watching,
    Starting,
    Lost,
    Disabled,
    Paused,
}

#[derive(Serialize)]
pub(super) struct WakeRow {
    pub(super) name: String,
    #[serde(rename = "type")]
    kind: &'static str,
    state: WakeState,
    target: String,
    trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
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
    entry: TaskEntry,
    #[serde(skip)]
    arm_state: ArmState,
}

impl WakeRow {
    #[expect(
        clippy::too_many_arguments,
        reason = "each list reads the snapshot, audit rows, and room source once for every row"
    )]
    pub(super) fn new(
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
                    parse_task_timeout(raw)
                        .ok()
                        .map(|value| (duration_label(value), value.as_secs()))
                }),
            ),
            Ok(Trigger::Schedule(_)) if delay.is_some() => (
                "timer",
                WakeState::Pending,
                delay.and_then(|raw| {
                    parse_task_timeout(raw)
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
            .map(|dir| ui::home_relative(&dir.to_string_lossy()));
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
                trigger.push_str(&format!(" · resumes {}", ui::rel_until(until, now)));
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
            .map(rimz::address::addressable_agents)
            .unwrap_or_default();
        let parent = entry.wait.as_ref().and_then(|target| {
            let agent = target_agent(&peers, target)?;
            let parent = rimz::address::launched_parent(peers.iter().copied(), agent)?;
            Some(rimz::address::agent_handle(parent, &peers, true))
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
            elapsed: meta.map(|meta| ui::age_short(meta.armed_at, now)),
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
    pub(super) fn target_key(&self) -> Option<(AgentKind, AgentSessionId)> {
        let target = self.entry.wait.as_ref()?;
        Some((target.kind.clone(), target.session.clone()))
    }

    pub(super) fn is_self(&self) -> bool {
        self.owner() == DeliveryOwner::SelfWait
    }

    pub(super) fn owner(&self) -> DeliveryOwner<'_> {
        delivery_owner(&self.name, &self.entry)
    }

    fn past_check_in(&self) -> bool {
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

    fn priority(&self) -> (u8, Reverse<u64>, &str) {
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

    fn wait_state(&self, now: &jiff::Zoned) -> String {
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
                            ui::age_label(limit.saturating_sub(self.elapsed_s.unwrap_or(0)))
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
                    |next| format!("pending · fires {}", ui::rel_until(next, now.timestamp())),
                ),
            WakeState::Paused => match self.arm_state {
                ArmState::Paused(until) => {
                    format!("paused · resumes {}", ui::rel_until(until, now.timestamp()))
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
    fn subject(&self, current_root: &Path) -> String {
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

pub(super) fn counts<'a>(owners: impl IntoIterator<Item = DeliveryOwner<'a>>) -> String {
    let (mut waits, mut subscriptions) = (0_usize, 0_usize);
    for owner in owners {
        if owner == DeliveryOwner::SelfWait {
            waits += 1;
        } else {
            subscriptions += 1;
        }
    }
    let mut parts = Vec::new();
    if waits > 0 {
        parts.push(wait_count(waits));
    }
    if subscriptions > 0 {
        parts.push(format!(
            "{subscriptions} subscription{}",
            if subscriptions == 1 { "" } else { "s" }
        ));
    }
    if parts.is_empty() {
        return "no waits".into();
    }
    parts.join(" · ")
}

fn wait_count(count: usize) -> String {
    if count == 0 {
        "no waits".into()
    } else {
        format!("{count} wait{}", if count == 1 { "" } else { "s" })
    }
}

pub(super) fn write_self(
    out: &mut impl Write,
    agent: &AgentState,
    snapshot: &SidebarSnapshot,
    rows: &[WakeRow],
    current_root: &Path,
    now: Timestamp,
) -> Result<()> {
    let mut waits = rows.iter().filter(|row| row.is_self()).collect::<Vec<_>>();
    waits.sort_by_key(|row| row.priority());
    let also = subscriptions(rows)
        .into_iter()
        .map(|(owner, members)| {
            let worktrees = members
                .iter()
                .filter(|row| row.entry.dir.is_some() && row.entry.run_dir() != current_root)
                .filter_map(|row| worktree_name(&row.entry, &row.entry.run_dir()))
                .collect::<BTreeSet<_>>();
            let suffix = if worktrees.is_empty() {
                String::new()
            } else {
                format!(
                    " in {}",
                    worktrees.into_iter().collect::<Vec<_>>().join(", ")
                )
            };
            format!("{}{suffix} ({owner})", trigger_names(&members, now))
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let header = format!(
        "{} · {}",
        rimz::address::agent_handle(agent, &rimz::address::addressable_agents(snapshot), true),
        wait_count(waits.len())
    );
    if waits.is_empty() && !also.is_empty() {
        writeln!(out, "{header} · also woken by {also}")?;
        return Ok(());
    }
    writeln!(out, "{header}")?;
    let zoned_now = now.to_zoned(rimz::config::MachineConfig::load_lenient().time_zone());
    write_waits(out, &waits, current_root, &zoned_now, 2)?;
    if !also.is_empty() {
        writeln!(out, "  also woken by {also}")?;
    }
    Ok(())
}

pub(super) fn write_room(
    out: &mut impl Write,
    rows: &[WakeRow],
    snapshot: Option<&SidebarSnapshot>,
    caller: Option<&(AgentKind, AgentSessionId)>,
    current_root: &Path,
    now: Timestamp,
) -> Result<()> {
    let mut cards: Vec<Vec<&WakeRow>> = Vec::new();
    let mut waits = rows.iter().filter(|row| row.is_self()).collect::<Vec<_>>();
    waits.sort_by_key(|row| row.priority());
    for row in waits {
        let group = cards.iter_mut().find(|group| {
            group[0]
                .entry
                .wait
                .as_ref()
                .zip(row.entry.wait.as_ref())
                // Each row's target is already its holder's key (`WakeRow::new`).
                .is_some_and(|(a, b)| schedule::pending::same_target(&[], a, b))
        });
        if let Some(group) = group {
            group.push(row);
        } else {
            cards.push(vec![row]);
        }
    }
    let agents = snapshot
        .map(rimz::address::addressable_agents)
        .unwrap_or_default();
    let mut cards = cards
        .into_iter()
        .map(|card| (card_header(card[0], &agents, caller), card))
        .collect::<Vec<_>>();
    cards.sort_by(|(a_header, a), (b_header, b)| {
        a[0].priority()
            .cmp(&b[0].priority())
            .then_with(|| a_header.cmp(b_header))
    });
    let zoned_now = now.to_zoned(rimz::config::MachineConfig::load_lenient().time_zone());
    for (header, card) in cards {
        writeln!(out, "  {header}")?;
        write_waits(out, &card, current_root, &zoned_now, 4)?;
    }
    for (owner, members) in subscriptions(rows) {
        let targets = members
            .iter()
            .map(|row| {
                let you = row
                    .entry
                    .wait
                    .as_ref()
                    .is_some_and(|target| is_caller(target, &agents, caller));
                action_text(TaskActionKind::Deliver, &row.target, Some(&row.entry), you)
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        let worktrees = members
            .iter()
            .map(|row| row.entry.run_dir())
            .collect::<BTreeSet<_>>()
            .len();
        writeln!(
            out,
            "  {owner} · {} → {targets} in {worktrees} worktree{}",
            trigger_names(&members, now),
            if worktrees == 1 { "" } else { "s" }
        )?;
    }
    Ok(())
}

fn card_header(
    row: &WakeRow,
    peers: &[&AgentState],
    caller: Option<&(AgentKind, AgentSessionId)>,
) -> String {
    let target = row.entry.wait.as_ref();
    let agent = target.and_then(|target| target_agent(peers, target));
    let mut header = if let Some(agent) = agent {
        let mut chain = vec![agent];
        while let Some(parent) = rimz::address::launched_parent(peers.iter().copied(), chain[0]) {
            if chain.iter().any(|ancestor| {
                ancestor.kind == parent.kind && ancestor.agent_id == parent.agent_id
            }) {
                break;
            }
            chain.insert(0, parent);
        }
        let mut labels = chain
            .iter()
            .map(|ancestor| {
                rimz::address::agent_handle(ancestor, peers, !ancestor.is_launched_child())
            })
            .collect::<Vec<_>>();
        if agent.is_launched_child()
            && let Some(name) = &agent.name
        {
            let petname = format!("@{name}");
            if let Some(label) = labels.last_mut()
                && label.as_str() != petname.as_str()
            {
                label.push_str(&format!(" ({petname})"));
            }
        }
        labels.join(" › ")
    } else {
        row.target.clone()
    };
    if target.is_some_and(|target| is_caller(target, peers, caller)) {
        header.push_str(" (you)");
    }
    header
}

/// The row of the agent a wait target names, among one row per agent.
fn target_agent<'a>(
    peers: &[&'a AgentState],
    target: &rimz::config::TaskTarget,
) -> Option<&'a AgentState> {
    peers
        .iter()
        .copied()
        .find(|agent| schedule::pending::wait_for(&target.kind, &target.session, agent))
}

/// Whether a wait target names the calling agent, in whichever session it now runs.
fn is_caller(
    target: &rimz::config::TaskTarget,
    peers: &[&AgentState],
    caller: Option<&(AgentKind, AgentSessionId)>,
) -> bool {
    caller.is_some_and(|(kind, session)| {
        (&target.kind == kind && &target.session == session)
            || peers
                .iter()
                .find(|agent| &agent.kind == kind && &agent.agent_id == session)
                .is_some_and(|agent| {
                    schedule::pending::wait_for(&target.kind, &target.session, agent)
                })
    })
}

fn write_waits(
    out: &mut impl Write,
    rows: &[&WakeRow],
    current_root: &Path,
    now: &jiff::Zoned,
    indent: usize,
) -> Result<()> {
    use ui::status::{StateRole, role};

    let mut table = ui::Table::new(["NAME", "STATE", "TRIGGER"])
        .without_headers()
        .indent(indent);
    for row in rows {
        let style = match row.state {
            WakeState::Lost => role(StateRole::Failed),
            WakeState::Watching if row.past_check_in() => role(StateRole::Waiting),
            WakeState::Pending
            | WakeState::Waiting
            | WakeState::Watching
            | WakeState::Starting
            | WakeState::Disabled
            | WakeState::Paused => ui::palette::body(),
        };
        table.row([
            ui::cell(&row.name),
            ui::cell(row.wait_state(now)).fg(style),
            ui::cell(row.subject(current_root)),
        ]);
    }
    table.render(out)?;
    Ok(())
}

fn subscriptions(rows: &[WakeRow]) -> BTreeMap<String, Vec<&WakeRow>> {
    let mut groups = BTreeMap::<String, Vec<&WakeRow>>::new();
    for row in rows {
        let label = match delivery_owner(&row.name, &row.entry) {
            DeliveryOwner::SelfWait => continue,
            DeliveryOwner::Team(team) => format!("team {team}"),
            DeliveryOwner::Resident(name) | DeliveryOwner::Loop(name) => format!("loop {name}"),
        };
        groups.entry(label).or_default().push(row);
    }
    groups
}

fn trigger_names(rows: &[&WakeRow], now: Timestamp) -> String {
    let signals = signal_names(
        rows.iter()
            .filter_map(|row| row.entry.signal.clone())
            .collect(),
    );
    if !signals.is_empty() {
        return signals;
    }
    rows.iter()
        .map(|row| {
            let mut head = schedule::parse_trigger(&row.name, &row.entry).map_or_else(
                |_| "invalid trigger".into(),
                |parsed| match &parsed.trigger {
                    Trigger::Watch(rimz::config::WatchSpec::Command(_)) => "on command exit".into(),
                    Trigger::Watch(rimz::config::WatchSpec::Pid { pid }) => {
                        format!("on pid {pid} exit")
                    }
                    Trigger::Watch(rimz::config::WatchSpec::Check { .. }) => "on check".into(),
                    Trigger::Watch(rimz::config::WatchSpec::File { file, .. }) => {
                        format!("on file {}", checkout_name(file))
                    }
                    _ => parsed.describe(),
                },
            );
            if let Some(label) = &row.label {
                head.push_str(&format!(" · {label}"));
            }
            if let ArmState::Paused(until) = row.arm_state {
                head.push_str(&format!(
                    " · paused · resumes {}",
                    ui::rel_until(until, now)
                ));
            }
            head
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" / ")
}

fn signal_names(mut signals: Vec<String>) -> String {
    signals.sort();
    signals.dedup();
    let mut families: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for signal in signals {
        let (family, member) = signal.split_once('.').unwrap_or((&signal, ""));
        families
            .entry(family.to_owned())
            .or_default()
            .push(member.to_owned());
    }
    families
        .into_iter()
        .map(|(family, members)| {
            if members.len() > 1 {
                format!("{family}.{{{}}}", members.join(","))
            } else if members[0].is_empty() {
                family
            } else {
                format!("{family}.{}", members[0])
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn worktree_name(entry: &TaskEntry, run_dir: &Path) -> Option<String> {
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

pub(super) fn checkout_name(path: &Path) -> String {
    rimz::theme::fmt::checkout_name(path)
}

pub(super) fn action_text(
    kind: TaskActionKind,
    subject: &str,
    entry: Option<&TaskEntry>,
    you: bool,
) -> String {
    kind.text(subject, entry, you)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No room: these rows read no condition.
    const NO_ROOM: RoomSource = RoomSource {
        runtime: None,
        room_open: false,
        source: None,
    };

    fn pane_shadowed_snapshot() -> SidebarSnapshot {
        let now = Timestamp::from_second(2_000).unwrap();
        let pane_id = rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Tmux, "%1");
        let mut owner = rimz::testkit::agent_state("claude", "owner", now);
        rimz::testkit::set_lane(&mut owner, "lane");
        owner.pane = Some(rimz::pane::PaneRef::from_id(pane_id.clone()));
        let mut shadow = rimz::testkit::agent_state("claude", "cleared", now);
        shadow.channel = owner.channel.clone();
        shadow.pane = owner.pane.clone();
        let mut snapshot = SidebarSnapshot::build_with_agents(
            rimz::ids::WorkspaceId::from_project_root(Path::new("/repo")),
            vec![owner, shadow],
            now,
        );
        let owner = &snapshot.agents[0];
        snapshot.agent_panes = vec![rimz::store::snapshot::PaneAgent {
            checkout: None,
            root_lane: false,
            lane: None,
            kind: owner.kind.clone(),
            kind_ordinal: owner.kind_ordinal,
            name: owner.name.clone(),
            name_explicit: owner.name_explicit,
            profile: owner.profile.clone(),
            role: owner.role.clone(),
            channel: owner.channel.clone(),
            agent_id: Some(owner.agent_id.clone()),
            pane_id,
            pane_pid: None,
            worktree_path: owner.worktree_path.clone(),
            worktree_branch: owner.worktree_branch.clone(),
        }]
        .into();
        snapshot
    }

    fn self_wait_row(snapshot: &SidebarSnapshot, kind: &str, session: &str) -> WakeRow {
        WakeRow::new(
            "wait-owner",
            &TaskEntry {
                root: "/repo".into(),
                signal: Some("deploy.done".into()),
                wait: Some(rimz::config::TaskTarget {
                    kind: AgentKind::new_unchecked(kind),
                    session: session.into(),
                    handle: "@stored#lane".into(),
                }),
                wait_meta: Some(
                    serde_json::from_value(serde_json::json!({
                        "armed_at": snapshot.generated_at,
                    }))
                    .unwrap(),
                ),
                ..Default::default()
            },
            ArmState::Live,
            None,
            Some(snapshot),
            &[],
            &NO_ROOM,
            snapshot.generated_at,
        )
    }

    fn room_text(snapshot: &SidebarSnapshot, row: WakeRow) -> String {
        let mut out = Vec::new();
        write_room(
            &mut out,
            &[row],
            Some(snapshot),
            None,
            Path::new("/repo"),
            snapshot.generated_at,
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn self_header_ignores_a_pane_shadowed_root() {
        let snapshot = pane_shadowed_snapshot();
        let mut out = Vec::new();
        let owner = &snapshot.agents[0];
        write_self(
            &mut out,
            owner,
            &snapshot,
            &[],
            Path::new("/repo"),
            snapshot.generated_at,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "@claude#lane · no waits\n");
    }

    #[test]
    fn room_header_ignores_a_pane_shadowed_root() {
        let snapshot = pane_shadowed_snapshot();
        let rendered = room_text(&snapshot, self_wait_row(&snapshot, "claude", "owner"));
        assert_eq!(rendered.lines().next(), Some("  @claude#lane"));
    }

    #[test]
    fn child_header_and_json_parent_ignore_a_pane_shadowed_root() {
        let mut snapshot = pane_shadowed_snapshot();
        let mut child = rimz::testkit::agent_state("codex", "child", snapshot.generated_at);
        rimz::testkit::set_lane(&mut child, "lane");
        child.profile = Some("reviewer".into());
        child.name = Some("modern-notch".into());
        child.parent_agent_id = Some("owner".into());
        child.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        child.launch_depth = Some(1);
        std::sync::Arc::make_mut(&mut snapshot.agents).push(child);
        let row = self_wait_row(&snapshot, "codex", "child");
        let json = serde_json::to_value(&row).unwrap();
        let rendered = room_text(&snapshot, row);
        assert_eq!(
            (rendered.lines().next(), json["parent"].as_str()),
            (
                Some("  @claude#lane › @reviewer (@modern-notch)"),
                Some("@claude#lane")
            )
        );
    }

    #[test]
    fn shadowed_room_target_uses_its_stored_handle() {
        let snapshot = pane_shadowed_snapshot();
        let rendered = room_text(&snapshot, self_wait_row(&snapshot, "claude", "cleared"));
        assert_eq!(rendered.lines().next(), Some("  @stored#lane"));
    }

    #[test]
    fn an_older_wait_joins_its_holders_card_and_kinds_sharing_a_session_keep_theirs() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut old = rimz::testkit::agent_state("claude", "old-session", now);
        old.launch_id = Some("launch-one".into());
        old.ended_at = Some(now);
        let mut new = rimz::testkit::agent_state("claude", "new-session", now);
        new.launch_id = Some("launch-one".into());
        new.epoch = 1;
        let snapshot = SidebarSnapshot::build_with_agents(
            rimz::ids::WorkspaceId::from_project_root(Path::new("/repo")),
            vec![new.clone()],
            now,
        );
        let audit = [&old, &new];
        let row = |name: &str, kind: &str, session: &str| {
            WakeRow::new(
                name,
                &TaskEntry {
                    root: "/repo".into(),
                    signal: Some("deploy.done".into()),
                    wait: Some(rimz::config::TaskTarget {
                        kind: AgentKind::new_unchecked(kind),
                        session: session.into(),
                        handle: format!("@{kind}-{session}"),
                    }),
                    wait_meta: Some(
                        serde_json::from_value(serde_json::json!({ "armed_at": now })).unwrap(),
                    ),
                    ..Default::default()
                },
                ArmState::Live,
                None,
                Some(&snapshot),
                &audit,
                &NO_ROOM,
                now,
            )
        };
        let rows = [
            row("wait-older", "claude", "old-session"),
            row("wait-launch", "claude", "launch-one"),
            row("wait-plugin-a", "plugin-a", "shared"),
            row("wait-plugin-b", "plugin-b", "shared"),
        ];
        let caller = (new.kind.clone(), new.agent_id.clone());
        let mut out = Vec::new();
        write_room(
            &mut out,
            &rows,
            Some(&snapshot),
            Some(&caller),
            Path::new("/repo"),
            now,
        )
        .unwrap();
        let rendered = String::from_utf8(out).unwrap();
        let cards = rendered
            .lines()
            .map(|line| {
                line.split("  ")
                    .find(|word| !word.is_empty())
                    .unwrap_or(line)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            cards,
            [
                "@claude (you)",
                "wait-launch",
                "wait-older",
                "@plugin-a-shared",
                "wait-plugin-a",
                "@plugin-b-shared",
                "wait-plugin-b"
            ],
            "{rendered}"
        );
    }

    #[test]
    fn subscriptions_pluralize_distinct_worktrees() {
        let now = Timestamp::from_second(1_000).unwrap();
        let mut rows = Vec::new();
        for (index, worktree) in ["topic-1", "topic-2", "topic-2"].into_iter().enumerate() {
            rows.push(WakeRow::new(
                &format!("wake-{index}"),
                &TaskEntry {
                    root: "/repo".into(),
                    dir: Some(format!("/repo/{worktree}").into()),
                    team: Some("recon#lane".parse().unwrap()),
                    signal: Some("ci.failed".into()),
                    wait: Some(rimz::config::TaskTarget {
                        kind: AgentKind::new_unchecked("claude"),
                        session: "session".into(),
                        handle: "@coder".into(),
                    }),
                    ..Default::default()
                },
                ArmState::Live,
                None,
                None,
                &[],
                &NO_ROOM,
                now,
            ));
            let mut out = Vec::new();
            write_room(&mut out, &rows, None, None, Path::new("/repo"), now).unwrap();
            let rendered = String::from_utf8(out).unwrap();
            let count = (index + 1).min(2);
            let suffix = if count == 1 { "worktree" } else { "worktrees" };
            assert_eq!(
                rendered,
                format!("  team recon#lane · ci.failed → @coder in {count} {suffix}\n")
            );
        }
    }
}
