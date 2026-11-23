//! Shared self-wait lines, room cards, subscription summaries, and condition readings.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use rimz::agents::AgentState;
use rimz::config::TaskEntry;
use rimz::harness::schedule::arming::ArmState;
pub(super) use rimz::harness::schedule::wait::{
    ConditionReading, RoomSource, WakeRow, read_condition, runtime_for_root, worktree_name,
};
use rimz::harness::schedule::wait::{WakeState, target_agent};
use rimz::harness::schedule::{self, DeliveryOwner, TaskActionKind, Trigger, delivery_owner};
use rimz::harness::when::WhenExpr;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::snapshot::SidebarSnapshot;

use super::render as ui;

/// Whether a room for `root` runs a sidebar fresh enough to read forge state.
pub(super) fn room_open(root: &Path) -> bool {
    runtime_for_root(root)
        .as_ref()
        .is_some_and(rimz::sidebar::fresh_sidebar_present)
}

/// The condition source of the room at `root`, read once per list.
pub(super) fn room_source(root: &Path) -> RoomSource {
    RoomSource::read(root, rimz::sidebar::fresh_sidebar_present)
}

/// A window reset as a reading line shows it, in `zone`.
pub(super) fn reset_minute(at: Timestamp, zone: &TimeZone) -> String {
    at.to_zoned(zone.clone())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

/// Each window reset as a reading line shows it, in `zone`.
pub(super) fn reset_minutes(
    resets: &BTreeMap<String, Timestamp>,
    zone: &TimeZone,
) -> BTreeMap<String, String> {
    resets
        .iter()
        .map(|(key, at)| (key.clone(), reset_minute(*at, zone)))
        .collect()
}

/// An unmet condition `state` with each reading `resets` names followed by its reset, as the
/// `now:` line and `loop watch` show it; any other state as given.
pub(super) fn with_resets(
    state: schedule::TaskTimingState,
    resets: &BTreeMap<String, String>,
) -> schedule::TaskTimingState {
    let annotate = |readings: Vec<(String, Option<String>)>| {
        readings
            .into_iter()
            .map(|(key, value)| {
                let value = value.map(|value| match resets.get(&key) {
                    Some(at) => format!("{value} (resets {at})"),
                    None => value,
                });
                (key, value)
            })
            .collect()
    };
    match state {
        schedule::TaskTimingState::Waiting { readings } => schedule::TaskTimingState::Waiting {
            readings: annotate(readings),
        },
        schedule::TaskTimingState::Unknown { readings } => schedule::TaskTimingState::Unknown {
            readings: annotate(readings),
        },
        state => state,
    }
}

/// The `now:` state of a condition just armed: its readings while unmet, each unread key with
/// its reason and each window holding it back with its reset, else `due`, or the start of its
/// hold.
pub(super) fn condition_now(
    entry: &TaskEntry,
    expr: &WhenExpr,
    hold: Option<std::time::Duration>,
) -> String {
    let reading = read_condition(&room_source(&entry.resolved_root()), entry, expr, None);
    let state = match schedule::TaskTimingState::unmet(expr, &reading.verdict) {
        Some(unmet) => with_resets(
            unmet,
            &reset_minutes(
                &reading.resets,
                &rimz::config::MachineConfig::load_lenient().time_zone(),
            ),
        ),
        None => match hold {
            Some(hold) => schedule::TaskTimingState::Holding {
                elapsed: std::time::Duration::ZERO,
                hold,
            },
            None => schedule::TaskTimingState::Due(Timestamp::now()),
        },
    };
    state
        .condition_label_with(&reading.unread)
        .unwrap_or_default()
}

/// Refuse arming `expr` at `scope` in the room at `root` when the room's forge reading already
/// says the checkout has no supported forge (`wait::refuse_unreadable_forge`).
pub(super) fn refuse_unreadable_forge(root: &Path, scope: &Path, expr: &WhenExpr) -> Result<()> {
    rimz::harness::schedule::wait::refuse_unreadable_forge(
        root,
        scope,
        expr,
        rimz::sidebar::fresh_sidebar_present,
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
        rimz::address::named_handle(agent, &rimz::address::addressable_agents(snapshot), true),
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
        chain
            .iter()
            .map(|ancestor| {
                let handle =
                    rimz::address::agent_handle(ancestor, peers, !ancestor.is_launched_child());
                rimz::address::handle_with_name(&handle, ancestor.name.as_deref())
            })
            .collect::<Vec<_>>()
            .join(" › ")
    } else {
        row.target.clone()
    };
    if target.is_some_and(|target| is_caller(target, peers, caller)) {
        header.push_str(" (you)");
    }
    header
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
    fn headers_pair_each_differing_name() {
        let mut snapshot = pane_shadowed_snapshot();
        std::sync::Arc::make_mut(&mut snapshot.agents)[0].name = Some("round-binder".into());
        let mut out = Vec::new();
        write_self(
            &mut out,
            &snapshot.agents[0],
            &snapshot,
            &[],
            Path::new("/repo"),
            snapshot.generated_at,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "@claude#lane (@round-binder) · no waits\n"
        );
        let mut child = rimz::testkit::agent_state("codex", "child", snapshot.generated_at);
        rimz::testkit::set_lane(&mut child, "lane");
        child.profile = Some("reviewer".into());
        child.name = Some("modern-notch".into());
        child.parent_agent_id = Some("owner".into());
        child.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        child.launch_depth = Some(1);
        std::sync::Arc::make_mut(&mut snapshot.agents).push(child);
        let rendered = room_text(&snapshot, self_wait_row(&snapshot, "codex", "child"));
        assert_eq!(
            rendered.lines().next(),
            Some("  @claude#lane (@round-binder) › @reviewer (@modern-notch)")
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
