//! Shared self-wait lines, room cards, and subscription summaries.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use jiff::Timestamp;
use rimz::agents::AgentState;
use rimz::config::TaskEntry;
use rimz::harness::schedule::arm::duration_label;
use rimz::harness::schedule::arming::ArmState;
use rimz::harness::schedule::runner::parse_task_timeout;
use rimz::harness::schedule::{self, DeliveryOwner, TaskActionKind, Trigger, delivery_owner};
use rimz::ids::{AgentKind, AgentSessionId};
use serde::Serialize;

use super::render as ui;

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
    #[serde(skip)]
    entry: TaskEntry,
    #[serde(skip)]
    arm_state: ArmState,
}

impl WakeRow {
    pub(super) fn new(
        name: &str,
        entry: &TaskEntry,
        arm_state: ArmState,
        watcher_pid: Option<u32>,
        agents: &[AgentState],
        now: Timestamp,
    ) -> Self {
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
        let peers = peers(agents);
        let parent = entry.wait.as_ref().and_then(|target| {
            let agent = peers
                .iter()
                .find(|agent| agent.kind == target.kind && agent.agent_id == target.session)?;
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
            entry: entry.clone(),
            arm_state,
        }
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
            WakeState::Waiting => "waiting".into(),
            WakeState::Starting => "starting".into(),
            WakeState::Disabled => "disabled".into(),
        }
    }

    fn subject(&self, current_root: &Path) -> String {
        let subject = self
            .entry
            .watch
            .as_ref()
            .map(|spec| self.label.clone().unwrap_or_else(|| spec.subject()))
            .unwrap_or_else(|| {
                schedule::parse_trigger(&self.name, &self.entry)
                    .map_or_else(|error| error.to_string(), |parsed| parsed.describe())
            });
        format!("{subject}{}", self.worktree_suffix(current_root))
    }

    fn worktree_suffix(&self, current_root: &Path) -> String {
        if self.entry.dir.is_none() || self.entry.run_dir() == current_root {
            return String::new();
        }
        worktree_name(&self.entry).map_or_else(String::new, |name| format!(" · in {name}"))
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

fn peers(agents: &[AgentState]) -> Vec<&AgentState> {
    agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .collect()
}

pub(super) fn write_self(
    out: &mut impl Write,
    agent: &AgentState,
    agents: &[AgentState],
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
                .filter_map(|row| worktree_name(&row.entry))
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
        rimz::address::agent_handle(agent, &peers(agents), true),
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
    agents: &[AgentState],
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
                .is_some_and(|(a, b)| a.kind == b.kind && a.session == b.session)
        });
        if let Some(group) = group {
            group.push(row);
        } else {
            cards.push(vec![row]);
        }
    }
    let mut cards = cards
        .into_iter()
        .map(|card| (card_header(card[0], agents, caller), card))
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
        let targets =
            members
                .iter()
                .map(|row| {
                    let you = row.entry.wait.as_ref().zip(caller).is_some_and(
                        |(target, (kind, session))| {
                            &target.kind == kind && &target.session == session
                        },
                    );
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
    agents: &[AgentState],
    caller: Option<&(AgentKind, AgentSessionId)>,
) -> String {
    let peers = peers(agents);
    let target = row.entry.wait.as_ref();
    let agent = target.and_then(|target| {
        peers
            .iter()
            .copied()
            .find(|agent| agent.kind == target.kind && agent.agent_id == target.session)
    });
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
                rimz::address::agent_handle(ancestor, &peers, !ancestor.is_launched_child())
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
    if target
        .zip(caller)
        .is_some_and(|(target, (kind, session))| &target.kind == kind && &target.session == session)
    {
        header.push_str(" (you)");
    }
    header
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

pub(super) fn worktree_name(entry: &TaskEntry) -> Option<String> {
    if let Some(name) = &entry.worktree {
        return Some(name.clone());
    }
    (entry.wait.is_some() || entry.dir.is_some()).then(|| {
        entry
            .dir
            .as_ref()
            .map(|_| checkout_name(&entry.run_dir()))
            .or_else(|| {
                entry.wait.as_ref().and_then(|target| {
                    target
                        .handle
                        .split_once('#')
                        .map(|(_, channel)| channel.to_owned())
                })
            })
            .unwrap_or_else(|| checkout_name(&entry.resolved_root()))
    })
}

pub(super) fn checkout_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

pub(super) fn action_text(
    kind: TaskActionKind,
    subject: &str,
    entry: Option<&TaskEntry>,
    you: bool,
) -> String {
    let mut action = match kind {
        TaskActionKind::Deliver => subject.split('#').next().unwrap_or(subject).to_owned(),
        TaskActionKind::Spawn => format!("start {subject}"),
        TaskActionKind::Workflow => format!("start workflow {subject}"),
        TaskActionKind::CheckOnly => "run check".into(),
    };
    if let Some(entry) = entry {
        let guard = entry.guard();
        let stages = guard
            .shell
            .map(|_| "check".to_owned())
            .into_iter()
            .chain(
                guard
                    .judges
                    .iter()
                    .map(|judge| format!("judge `{}`", judge.agent)),
            )
            .collect::<Vec<_>>();
        if !stages.is_empty() && kind.has_effect() {
            let guard = stages.join(", then ");
            action = if kind.is_spawn() {
                format!("{guard}, then start {subject}")
            } else {
                format!("{action} · {guard}")
            };
        }
    }
    if you {
        action.push_str(" (you)");
    }
    if let Some(account) = entry.and_then(|entry| entry.account.as_ref()) {
        action.push_str(&format!(" · account {account}"));
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

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
                &[],
                now,
            ));
            let mut out = Vec::new();
            write_room(&mut out, &rows, &[], None, Path::new("/repo"), now).unwrap();
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
