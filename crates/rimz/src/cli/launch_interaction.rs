//! Terminal decisions and receipts for the shared launcher.

use super::{render, report_unknown_config_keys};
use anyhow::Result;
use rimz::launcher::OpenedLayout as LaunchedLayout;
use rimz::launcher::{self, ClosedCohort, CohortChoice, Interaction, Notice};
use rimz::store::writer::AgentLaunchIdentity;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

pub(super) struct CliInteraction;

impl Interaction for CliInteraction {
    fn cross_repo(&mut self, workspace: &rimz::ResolvedWorkspace) -> Result<bool> {
        super::confirm_cross_repo_worktree_with(
            workspace,
            std::io::stdin().is_terminal(),
            super::confirm,
            |line| writeln!(render::err(), "{line}"),
        )
    }
    fn pr_branch(
        &mut self,
        facts: &rimz::worktree::WorktreeErr,
    ) -> Result<Option<rimz::worktree::PrBranchChoice>> {
        super::resolve_pr_branch_choice(facts)
    }
    fn unmanaged_checkout(&mut self, name: &str, cwd: &Path) -> Result<bool> {
        if !std::io::stdin().is_terminal() {
            return launcher::Unattended.unmanaged_checkout(name, cwd);
        }
        super::confirm(&format!(
            "Worktree `{name}` at {} is not RimZ-managed. Enter it without adopting it or enabling automatic cleanup?",
            cwd.display()
        ))
    }
    fn closed_cohort(&mut self, cohort: ClosedCohort<'_>) -> Result<CohortChoice> {
        use super::worktree_entry::{self, Choice};
        Ok(
            match worktree_entry::offer(&cohort, std::io::stdin().is_terminal())? {
                Choice::Fresh => CohortChoice::Fresh,
                Choice::Remove => CohortChoice::Recreate,
                Choice::Resume => CohortChoice::Resume,
            },
        )
    }
    fn notice(&mut self, notice: Notice<'_>) -> Result<()> {
        match notice {
            Notice::Config(config) => report_unknown_config_keys(config)?,
            Notice::LaunchWarning(warning) => writeln!(render::err(), "{warning}")?,
            Notice::Warning(line) | Notice::Reconcile(line) => writeln!(render::err(), "{line}")?,
            Notice::Trust(notice) => writeln!(render::err(), "{notice}")?,
            Notice::LspIgnored(message) => writeln!(render::err(), "rimz: {message}")?,
            Notice::LspRefused(message) => {
                writeln!(render::err(), "rimz: {message}; agents use grep")?
            }
            Notice::LspWait(wait) => write_wait(wait)?,
            Notice::IsolationClamp { profile, source } => writeln!(
                render::err(),
                "rimz: {profile} {source}, and a subagent never runs looser than its parent"
            )?,
            Notice::PermissionClamp { profile, clamp } => writeln!(
                render::err(),
                "rimz: {profile} runs in {} mode: {} asks for {}, and a subagent never runs looser than its parent",
                clamp.mode,
                clamp.source,
                clamp.requested
            )?,
            Notice::YoloSandbox(profile) => writeln!(
                render::err(),
                "rimz: {profile} runs yolo under scoped isolation; sudo and setuid binaries do not work there; use --isolation host"
            )?,
            Notice::RoomReset { session, report } => {
                render::room::print_automatic_reset(session, report)?
            }
            Notice::Dispatch { target, result } => super::send::report_dispatch(
                rimz::message::DeliveryKind::Boundary,
                target,
                &result.outcomes,
                &result.compacted,
            )?,
            Notice::PickerReceipt {
                kind,
                channel,
                cwd,
                project,
            } => write_picker_receipt(
                &mut render::out(),
                kind,
                launched_lane_label(channel, cwd, project).as_deref(),
            )?,
            Notice::ResumeAnnouncement(plan) => {
                write_resume_announcement(&mut render::out(), plan)?
            }
            Notice::ResumeReceipt {
                plan,
                team,
                channel,
                cwd,
                project,
                identities,
                leader,
            } => write_resume_receipt(
                &mut render::out(),
                plan,
                team,
                launched_lane_label(channel, cwd, project).as_deref(),
                identities,
                leader,
            )?,
        }
        Ok(())
    }
}

pub(super) fn admit_lsp(checkout: &Path, machine: &rimz::config::MachineConfig) -> Result<()> {
    launcher::lsp_admission::admit(checkout, machine, &mut CliInteraction)
}
pub(super) trait LayoutReceipt {
    fn write_receipt(
        &self,
        w: &mut impl Write,
        store: &rimz::Store,
        project_root: &Path,
    ) -> Result<()>;
}

impl LayoutReceipt for LaunchedLayout {
    fn write_receipt(
        &self,
        w: &mut impl Write,
        store: &rimz::Store,
        project_root: &Path,
    ) -> Result<()> {
        if self.in_place {
            return Ok(());
        }
        let paths = store.paths();
        let lane_path = self
            .identities
            .first()
            .and_then(|identity| identity.launch.checkout.as_deref())
            .map(Path::new)
            .unwrap_or(&self.cwd);
        let lane_label = launched_lane_label(self.channel.as_deref(), lane_path, project_root);
        if self.pick_session {
            return write_picker_receipt(
                w,
                self.identities[0].kind.as_str(),
                lane_label.as_deref(),
            );
        }
        // The agents are already launched, so an unreadable snapshot only
        // costs the receipt its list handles.
        let snapshot = store.snapshot_cached().ok();
        let handles = receipt_handles(&self.identities, snapshot.as_ref());
        write_launch_receipt(
            w,
            &LaunchReceipt {
                team: self.team.as_ref().map(|(name, team)| (name.as_str(), team)),
                channel: lane_label.as_deref(),
                cwd: &self.cwd,
                identities: &self.identities,
                handles: &handles,
                leader_index: self.leader_index,
                terminal_width: render::terminal_columns(100),
                hints: reported_peer(self.peer.as_ref(), paths).is_none(),
            },
        )?;
        write_peer_receipt(
            w,
            &self.identities,
            self.peer.as_ref(),
            self.channel.as_ref().and(lane_label.as_deref()),
            paths,
        )
    }
}

fn launched_lane_label(channel: Option<&str>, cwd: &Path, project_root: &Path) -> Option<String> {
    if rimz::agents::AgentState::is_root_lane(channel, Some(cwd), Some(project_root)) {
        return Some("main".to_owned());
    }
    channel
        .filter(|channel| !channel.is_empty())
        .map(str::to_owned)
        .or_else(|| cwd.file_name()?.to_str().map(str::to_owned))
}

fn write_picker_receipt(w: &mut impl Write, kind: &str, lane: Option<&str>) -> Result<()> {
    let boundary = if rimz::agents::spec_by_kind(kind)
        .is_some_and(|spec| spec.capabilities.registers_lazily)
    {
        "at the first prompt"
    } else {
        "once a session is picked"
    };
    writeln!(
        w,
        "opened {kind} session picker in #{}; the card binds {boundary}",
        lane.unwrap_or("main")
    )?;
    Ok(())
}

/// The launch-prompt run the peer receipt describes, with the peer's name and
/// response path; `None` when the launch created no such run.
fn reported_peer<'a>(
    run: Option<&'a rimz::store::run::RunRecord>,
    paths: &rimz::StatePaths,
) -> Option<(&'a rimz::store::run::RunRecord, &'a str, PathBuf)> {
    let run = run?;
    let name = run.agent_name.as_deref()?;
    let response = rimz::harness::run::response_path(paths, run)?;
    Some((run, name, response))
}

fn write_peer_receipt(
    w: &mut impl Write,
    identities: &[AgentLaunchIdentity],
    run: Option<&rimz::store::run::RunRecord>,
    channel: Option<&str>,
    paths: &rimz::StatePaths,
) -> Result<()> {
    if let Some((run, name, response)) = reported_peer(run, paths) {
        let response = response.display();
        let detached = run.report_to == rimz::store::run::ReportTo::Nobody;
        let handle = channel.map_or_else(
            || format!("@{name}"),
            |channel| format!("@{name}#{channel}"),
        );
        if let Some(team) = run.team.as_ref() {
            let instance = &team.instance;
            if detached {
                writeln!(
                    w,
                    "@{name} leads {instance}, detached: no TEAM_REPORT will reach you."
                )?;
                writeln!(
                    w,
                    "The leader's final response lands at {response}. To block until Done: rimz teams wait {instance}"
                )?;
            } else {
                writeln!(
                    w,
                    "@{name} leads {instance}. Keep working or end your turn: one TEAM_REPORT reaches you when its board reaches Done; stop it after: rimz teams stop {instance}"
                )?;
                writeln!(
                    w,
                    "The leader's final response lands at {response}. To block instead: rimz teams wait {instance}"
                )?;
            }
        } else if detached {
            writeln!(
                w,
                "Running detached in its own pane: no AGENT_REPORT will reach you for this launch turn."
            )?;
            writeln!(
                w,
                "Its response lands at {response} when this turn settles. To read it: rimz agents wait {handle}"
            )?;
            writeln!(
                w,
                "It stays open after this turn. A message of yours reports as usual only if it lands after this turn settles. To follow up: rimz message {handle} '<text>'"
            )?;
        } else {
            writeln!(
                w,
                "Running in its own pane. Keep working or end your turn: one AGENT_REPORT reaches you once every agent you launched has settled, and another after each turn a message of yours opens."
            )?;
            writeln!(
                w,
                "Its response lands at {response} when this turn settles. To block instead: rimz agents wait {handle}"
            )?;
            writeln!(
                w,
                "It stays open after this turn. To follow up: rimz message {handle} '<text>'"
            )?;
        }
    }
    // A team reports through its prompted leader alone, so only that seat's
    // missing hooks cost the launcher its report.
    for identity in identities.iter().filter(|identity| {
        identity.launch.launched_by.is_some()
            && (identity.launch.team.is_none() || identity.prompt.is_some())
    }) {
        if let Some(adapter) = rimz::agents::find_definition(identity.kind.as_str())
            && !rimz::harness::run::peer_can_report(adapter)
        {
            writeln!(
                w,
                "@{} will not report back: {} does not provide the turn hooks needed for reports.",
                identity.name, identity.kind
            )?;
        }
    }
    Ok(())
}

struct LaunchReceipt<'a> {
    team: Option<(&'a str, &'a rimz::config::Team)>,
    channel: Option<&'a str>,
    cwd: &'a Path,
    identities: &'a [AgentLaunchIdentity],
    /// Each identity's `rimz agents list` handle, without the `@`.
    handles: &'a [String],
    leader_index: Option<usize>,
    terminal_width: usize,
    /// Off when the peer receipt prints, since it names the same two commands.
    hints: bool,
}

fn write_launch_receipt(w: &mut impl Write, receipt: &LaunchReceipt<'_>) -> Result<()> {
    let team = receipt.team.map(|(name, _)| name);
    let channel = receipt.channel;
    let identities = receipt.identities;
    let members = identities
        .iter()
        .enumerate()
        .map(|(index, identity)| {
            let handle = receipt
                .handles
                .get(index)
                .map_or_else(|| launch_identity_handle(identity), String::as_str);
            (identity, handle)
        })
        .collect::<Vec<_>>();
    let subject = match (team, members.as_slice()) {
        (Some(team), _) => team.to_owned(),
        (None, [(identity, handle)]) => format!("@{}", receipt_label(handle, identity)),
        (None, members) => format!("{} agents", members.len()),
    };
    let lane = channel.map_or_else(String::new, |channel| {
        if team.is_some() {
            format!(" in worktree #{channel}")
        } else {
            format!(" in #{channel}")
        }
    });
    let cwd = rimz::utils::path::normalize_path_lexical(receipt.cwd);
    if team.is_some() {
        writeln!(w, "launched {subject}{lane}")?;
        writeln!(w, "  path      {}", cwd.display())?;
        writeln!(
            w,
            "  board     {}",
            rimz::harness::scratch::board_relative_path()
        )?;
    } else {
        writeln!(w, "launched {subject}{lane} ({})", cwd.display())?;
    }
    let leader = receipt.leader_index.and_then(|index| members.get(index));
    if let Some((identity, handle)) = leader
        && let Some(prompt) = identity.prompt.as_deref()
    {
        let prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        let prompt = prompt.replace('\\', "\\\\").replace('"', "\\\"");
        let line = format!("  prompt    → @{handle}  \"{prompt}\"");
        writeln!(
            w,
            "{}",
            render::clip_to_width(&line, receipt.terminal_width)
        )?;
    }
    writeln!(w)?;

    let rows = members
        .iter()
        .enumerate()
        .map(|(index, (identity, handle))| render::RosterRow {
            handle: receipt_label(handle, identity),
            kind: identity.kind.as_str().to_owned(),
            model: identity.launch.model.clone(),
            tier: identity.launch.tier.as_deref().cloned(),
            leader: team.is_some() && receipt.leader_index == Some(index),
        })
        .collect();
    let signals = receipt
        .team
        .into_iter()
        .flat_map(|(_, team)| &team.roles)
        .flat_map(|role| {
            role.signals.iter().map(|binding| render::RosterSignal {
                signal: binding.signal.clone(),
                matches: binding.matches.clone(),
                role: role.role.clone(),
            })
        })
        .collect();
    render::Roster::new(rows)
        .signals(signals)
        .signal_width(receipt.terminal_width)
        .indent(2)
        .render(w)?;
    if team.is_some() {
        return Ok(());
    }
    writeln!(w)?;
    if !receipt.hints {
        return Ok(());
    }
    write_launch_hints(
        w,
        team,
        channel,
        members.first().map(|(_, handle)| *handle),
        leader.map(|(_, handle)| *handle),
    )
}

/// The handle `rimz agents list` gives each launched agent among the resolver's
/// addressable agents; an identity with no row yet keeps its role, else its pet name.
fn receipt_handles(
    identities: &[AgentLaunchIdentity],
    snapshot: Option<&rimz::store::snapshot::SidebarSnapshot>,
) -> Vec<String> {
    let agents = snapshot
        .map(rimz::address::addressable_agents)
        .unwrap_or_default();
    identities
        .iter()
        .map(|identity| {
            agents
                .iter()
                .find(|agent| agent.agent_id == identity.agent_id && agent.kind == identity.kind)
                .map(|agent| rimz::address::agent_handle(agent, &agents, false))
                .and_then(|handle| handle.strip_prefix('@').map(str::to_owned))
                .unwrap_or_else(|| launch_identity_handle(identity).to_owned())
        })
        .collect()
}

/// A receipt handle with the pet name beside it when the two differ.
fn receipt_label(handle: &str, identity: &AgentLaunchIdentity) -> String {
    if handle == identity.name {
        handle.to_owned()
    } else {
        format!("{handle} ({})", identity.name)
    }
}

fn launch_identity_handle(identity: &AgentLaunchIdentity) -> &str {
    identity.launch.role.as_deref().unwrap_or(&identity.name)
}

fn resume_hint_handle<'a>(
    plan: &'a rimz::harness::plan::CohortResumePlan,
    fresh_identities: &'a [AgentLaunchIdentity],
) -> Option<&'a str> {
    match plan.seeds.first()? {
        rimz::harness::plan::CohortSeed::Resume(agent) => agent
            .role
            .as_deref()
            .or(agent.name.as_deref())
            .or(agent.profile.as_deref())
            .or(Some(agent.kind.as_str())),
        rimz::harness::plan::CohortSeed::Fresh => {
            fresh_identities.first().map(launch_identity_handle)
        }
    }
}

fn write_launch_hints(
    w: &mut impl Write,
    team: Option<&str>,
    channel: Option<&str>,
    fallback_handle: Option<&str>,
    leader: Option<&str>,
) -> Result<()> {
    if let (Some(team), Some(channel)) = (team, channel) {
        writeln!(w, "Check: rimz teams show {team}#{channel}")?;
    }
    if let Some(handle) = leader.or(fallback_handle) {
        let lane = channel.map_or_else(String::new, |channel| format!("#{channel}"));
        writeln!(w, "Reach: rimz message @{handle}{lane} '<text>'")?;
        if team.is_none() {
            writeln!(w, "Wait:  rimz agents wait @{handle}{lane}")?;
        }
    }
    if let (Some(team), Some(channel)) = (team, channel) {
        writeln!(
            w,
            "Wait:  rimz loop add team-idle --wait @me --signal team.idle --match instance={team}#{channel} --once"
        )?;
    }
    Ok(())
}

fn write_resume_announcement(
    w: &mut impl Write,
    plan: &rimz::harness::plan::CohortResumePlan,
) -> Result<()> {
    write!(w, "resuming {} in {}", plan.spec, plan.lane_label)?;
    if plan.matching_lanes > 1 {
        write!(
            w,
            " (newest of {}; others: run rimz agents resume from the project root)",
            plan.matching_lanes
        )?;
    }
    writeln!(w)?;
    Ok(())
}

fn write_resume_receipt(
    w: &mut impl Write,
    plan: &rimz::harness::plan::CohortResumePlan,
    team: Option<&str>,
    channel: Option<&str>,
    fresh_identities: &[AgentLaunchIdentity],
    leader: Option<&str>,
) -> Result<()> {
    report_cohort_resume(w, plan)?;
    writeln!(w)?;
    write_launch_hints(
        w,
        team,
        channel,
        resume_hint_handle(plan, fresh_identities),
        leader,
    )
}

fn report_cohort_resume(
    w: &mut impl Write,
    plan: &rimz::harness::plan::CohortResumePlan,
) -> Result<()> {
    let mut fresh = plan.fresh.iter();
    for seed in &plan.seeds {
        match seed {
            rimz::harness::plan::CohortSeed::Resume(agent) => {
                let name = agent.name.as_deref().unwrap_or("unnamed");
                writeln!(
                    w,
                    "resumed {}:{} ({})",
                    agent.kind.as_str(),
                    name,
                    agent.agent_id
                )?;
            }
            rimz::harness::plan::CohortSeed::Fresh => {
                let label = fresh.next().map_or("agent", String::as_str);
                writeln!(w, "started fresh {label}")?;
            }
        }
    }
    Ok(())
}

pub(super) fn write_wait(wait: &rimz::lsp::admission::Wait) -> Result<()> {
    writeln!(
        super::render::err(),
        "rimz: waiting to start language server {}: needs {}, {}; position {} in the queue; giving up in {}",
        wait.shortfall.server,
        rimz::utils::size::decimal_bytes(wait.shortfall.estimate_bytes),
        wait.shortfall.headroom(),
        wait.position,
        rimz::utils::time::format_duration_compact(wait.remaining)
    )?;
    Ok(())
}
pub(super) fn preflight_hooks(
    health: Result<(), rimz::agents::BirthLoginErr>,
    unattended: impl FnOnce() -> Option<launcher::trust::Unattended>,
    fix_command: impl FnOnce() -> String,
    noticed: &mut std::collections::HashSet<rimz::ids::LoginKey>,
    out: &mut impl Write,
) -> Result<()> {
    if let Some(notice) =
        launcher::trust::preflight_hooks(health, unattended, fix_command, noticed)?
    {
        writeln!(out, "{notice}")?;
    }
    Ok(())
}

pub(super) fn preflight(
    kind: &str,
    cwd: &Path,
    trust: Option<rimz::agents::FolderTrust>,
    unattended: Option<launcher::trust::Unattended>,
) -> Result<()> {
    if let Some(notice) = launcher::trust::preflight(kind, cwd, trust, unattended)? {
        CliInteraction.notice(Notice::Trust(&notice))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
