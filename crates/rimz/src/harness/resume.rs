//! Resume-on-rebirth planning: turn the durable agent rollup into the tabs a
//! reborn session re-seeds.
//!
//! When the CLI admits agent recovery for a reborn room — a machine reboot or
//! mux crash — the agents' processes are gone, but the ledger remembers them.
//! This module reads that memory (the audit rollup, which keeps the
//! dead-process agents the runtime projection would expel) and plans one
//! `#channel` tab per worktree, with one resume pane per prior root agent, so
//! the next birth can recover where the user left off instead of empty.
//!
//! Pure over its inputs: the caller supplies the rollup and a worktree-exists
//! predicate, and the flat rebirth planner also supplies the set of cleanly
//! ended sessions, so every filtering rule is unit-tested without a multiplexer
//! or the filesystem. The launcher ([`crate::mux::MuxBackend`]) seeds the
//! resulting [`ResumeTab`]s at birth and stays ignorant of agents and the
//! ledger.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::agents::AgentState;
use crate::agents::find_adapter;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::ledger::runtime::AgentLiveness;
use crate::mux::ResumeTab;

/// The default ceiling on agents auto-resumed into one reborn session, so a
/// long-lived workspace cannot fork-bomb a fleet of agent processes on birth.
/// Anything past it is reported, never silently dropped.
pub const DEFAULT_RESUME_MAX: usize = 128;

/// Why a candidate agent was not resumed — surfaced in the start report so a
/// skipped agent stays visible rather than silently lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeSkipReason {
    /// The agent's kind has no resume CLI ([`crate::agents::AgentAdapter::resume_command`]).
    NoResumeSupport,
    /// Dropped to stay within the resume cap.
    OverCap,
}

/// A candidate that the planner deliberately did not resume, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeSkip {
    pub label: String,
    pub reason: ResumeSkipReason,
}

/// What a reborn session should re-seed, and what it deliberately left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumePlan {
    /// The tabs to seed, ordered by their freshest pane activity (the lead is
    /// the focus target). Panes inside each tab are freshest-first.
    pub tabs: Vec<ResumeTab>,
    /// Candidates not resumed, each with its reason — the start report names them.
    pub skipped: Vec<ResumeSkip>,
    /// Candidates whose worktree disappeared; the caller records these as
    /// durable end traces so they leave the next resume candidate set.
    pub tombstone: Vec<(AgentKind, AgentSessionId)>,
}

/// One cell in an explicit cohort resume spec, reduced to the matching fields
/// the planner needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CohortCell {
    pub kind: AgentKind,
    pub role: Option<String>,
}

/// One agent cell's explicit-resume seed.
#[derive(Clone, Debug, PartialEq)]
pub enum CohortSeed {
    /// Resume a prior provider-native session, replaying its launch identity.
    Resume(Box<AgentState>),
    /// Launch a fresh pane for a cell missing resumable history.
    Fresh,
}

/// Explicit `rimz agents <spec> --resume` plan, parallel to the layout's agent
/// cells.
#[derive(Clone, Debug, PartialEq)]
pub struct CohortResumePlan {
    pub seeds: Vec<CohortSeed>,
    pub cwd: Option<PathBuf>,
    pub channel: Option<String>,
    pub fresh: Vec<String>,
    pub launch_group: Option<String>,
}

/// Why explicit cohort resume cannot proceed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CohortResumeErr {
    NothingToResume { spec: String },
    MembersStillLive { labels: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ResumeTabIdentity {
    Channel(String),
    Cwd(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlannedResumeTab {
    identity: ResumeTabIdentity,
    tab: ResumeTab,
}

impl ResumePlan {
    /// Whether there is nothing to seed — the birth is exactly the bare working
    /// room.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

/// Plan the resume seeds for one reborn session from the durable agent rollup.
///
/// `agents` is the audit rollup (dead-process agents intact); `ended` is the
/// `(kind, agent_id)` set the user closed cleanly from
/// [`crate::RuntimeProjection::ended`]; `max` caps the auto-launched panes;
/// `worktree_exists` decides whether a candidate's worktree
/// is still on disk (production passes `|p| p.is_dir()`); `rimz_bin` is the
/// `rimz` executable each pane's wrapper argv names (production passes
/// `std::env::current_exe()`).
///
/// A candidate qualifies when it is a root agent (subagents ride their parent),
/// was bound to a pane in the incarnation that died, still carries a session id
/// and a worktree, and was not cleanly ended. The rollup is workspace-scoped and
/// a `session.rebirth` boundary clears every pane stamp recorded before it, so a
/// surviving (non-`None`) pane stamp means the agent was live in the incarnation
/// the rebirth replaces — exactly the set to bring back. One pane hosts one
/// agent: a relaunch that re-used a pane id collapses to its newest stamp —
/// the same rule the live sidebar binds by (`stamped_agent_for_pane`, in
/// `ledger::snapshot::panes`) — so resume never doubles a pane, while two
/// concurrent agents in one worktree (distinct panes) share one `#channel` tab.
pub fn plan_resume(
    agents: &[AgentState],
    ended: &BTreeSet<(AgentKind, AgentSessionId)>,
    max: usize,
    worktree_exists: impl Fn(&Path) -> bool,
    rimz_bin: &Path,
) -> ResumePlan {
    plan_resume_inner(agents, ended, max, None, worktree_exists, rimz_bin)
}

/// Plan a resume with workspace-root context, so worktree-backed teams converge
/// to the worktree channel while in-place teams keep `<dir>/<team>`.
pub fn plan_resume_with_project(
    agents: &[AgentState],
    ended: &BTreeSet<(AgentKind, AgentSessionId)>,
    max: usize,
    project_root: &Path,
    worktree_exists: impl Fn(&Path) -> bool,
    rimz_bin: &Path,
) -> ResumePlan {
    plan_resume_inner(
        agents,
        ended,
        max,
        Some(project_root),
        worktree_exists,
        rimz_bin,
    )
}

fn plan_resume_inner(
    agents: &[AgentState],
    ended: &BTreeSet<(AgentKind, AgentSessionId)>,
    max: usize,
    project_root: Option<&Path>,
    worktree_exists: impl Fn(&Path) -> bool,
    rimz_bin: &Path,
) -> ResumePlan {
    // Root agents that were bound to a pane in the dead incarnation, still
    // identified, and not cleanly ended. A subagent is paneless and rides its
    // parent, so it is filtered out here and never resumed standalone.
    let mut candidates: Vec<&AgentState> = agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .filter(|agent| {
            agent
                .worktree_path
                .as_deref()
                .is_some_and(|path| !path.is_empty())
        })
        .filter(|agent| agent.pane.is_some())
        .filter(|agent| !ended.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .collect();

    // Most-recently-active first (deterministic on ties), so the newest session
    // wins supersession, the lead pane is the focus target, and the cap keeps
    // the freshest agents.
    candidates.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    });

    let mut seen: HashSet<PaneId> = HashSet::new();
    let mut plan = ResumePlan::default();
    let mut tabs: Vec<PlannedResumeTab> = Vec::new();
    for agent in candidates {
        // `pane` is `Some` and `worktree_path` is `Some(non-empty)` by the
        // filters above. The pane is the unit of identity: an older relaunch
        // that re-used this pane id is superseded by the newest stamp (the
        // candidates are newest-first, so the first one seen for a pane wins),
        // mirroring the live binding's `stamped_agent_for_pane`. Distinct panes
        // — including two same-kind agents in one worktree — each get a seed.
        let pane_id = agent
            .pane
            .as_ref()
            .expect("candidates are filtered to a stamped pane")
            .pane_id
            .clone();
        if !seen.insert(pane_id) {
            continue;
        }
        let worktree = agent.worktree_path.clone().unwrap_or_default();
        let cwd = PathBuf::from(&worktree);
        let channel = agent_room_channel(project_root, agent, &cwd);
        let label = build_label(&agent.kind, channel.as_deref(), &cwd);
        if !worktree_exists(&cwd) {
            plan.tombstone
                .push((agent.kind.clone(), agent.agent_id.clone()));
            continue;
        }
        if !supports_agent_resume(agent) {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::NoResumeSupport,
            });
            continue;
        }
        let seeded = tabs.iter().map(|tab| tab.tab.pane_count()).sum::<usize>();
        if seeded >= max {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::OverCap,
            });
            continue;
        }
        // The pane runs the supervised exec wrapper, not the agent CLI
        // directly: every agent launch funnels through `rimz agents exec`,
        // which replays the durable launch identity, applies trusted
        // `[[agents]]` env and the adapter's launch pins before spawning the
        // resume argv.
        let command = resume_command_with_channel(rimz_bin, agent, channel.as_deref());
        let tab_label = channel_label(channel.as_deref(), &cwd);
        let identity = resume_tab_identity(channel.as_deref(), &cwd);
        if let Some(tab) = tabs.iter_mut().find(|tab| tab.identity == identity) {
            if let Some(column) = tab.tab.layout.columns.first_mut() {
                column.panes.push(crate::mux::PaneCmd { argv: command });
            }
        } else {
            tabs.push(PlannedResumeTab {
                identity,
                tab: ResumeTab::flat(tab_label, cwd, vec![command]),
            });
        }
    }
    disambiguate_resume_tab_labels(&mut tabs);
    plan.tabs = tabs.into_iter().map(|planned| planned.tab).collect();
    plan
}

fn agent_room_channel(
    project_root: Option<&Path>,
    agent: &AgentState,
    cwd: &Path,
) -> Option<String> {
    match project_root {
        Some(project_root) => crate::harness::target::resolve_room_channel(
            project_root,
            cwd,
            agent.team.as_deref(),
            agent.channel.as_deref(),
        ),
        None => agent
            .channel
            .as_deref()
            .filter(|channel| !channel.is_empty())
            .map(ToOwned::to_owned),
    }
}

/// Plan one explicit cohort relaunch from the durable agent rollup.
///
/// `cells` is the launch layout's agent cells in display order. `team` is the
/// named-team spec, when the launch resolved one. The caller supplies liveness
/// and worktree existence so the matching rules stay pure and testable.
pub fn plan_cohort_resume(
    agents: &[AgentState],
    liveness: impl Fn(&AgentState) -> AgentLiveness,
    cells: &[CohortCell],
    team: Option<&str>,
    worktree_exists: impl Fn(&Path) -> bool,
) -> Result<CohortResumePlan, CohortResumeErr> {
    let spec = cohort_spec_label(cells, team);
    let candidates = cohort_candidates(agents, worktree_exists);
    let matches = match (team, cells.len()) {
        (Some(team), _) => match_team_cohort(&candidates, cells, team),
        (None, 1) => match_single_cohort(&candidates, &cells[0]),
        (None, _) => match_inline_cohort(&candidates, cells),
    };

    let matched_any = matches.iter().any(Option::is_some);
    if !matched_any {
        return Err(CohortResumeErr::NothingToResume { spec });
    }

    let live = matches
        .iter()
        .flatten()
        .copied()
        .filter(|agent| matches!(liveness(agent), AgentLiveness::Live { .. }))
        .map(cohort_agent_label)
        .collect::<Vec<_>>();
    if !live.is_empty() {
        return Err(CohortResumeErr::MembersStillLive { labels: live });
    }

    let newest = matches
        .iter()
        .flatten()
        .copied()
        .min_by(cohort_newest_cmp)
        .expect("matched_any guarantees one matched member");
    let cwd = agent_worktree(newest);
    let channel = newest
        .channel
        .as_deref()
        .filter(|channel| !channel.is_empty())
        .map(ToOwned::to_owned);
    let launch_group = newest
        .launch_group
        .as_deref()
        .filter(|group| !group.is_empty())
        .map(ToOwned::to_owned);

    let mut seeds = Vec::with_capacity(cells.len());
    let mut fresh = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        let matched = matches[index];
        let seed = match matched {
            Some(agent) if supports_agent_resume(agent) => {
                CohortSeed::Resume(Box::new(agent.clone()))
            }
            Some(agent) => {
                fresh.push(cohort_fresh_label_for_agent(agent));
                CohortSeed::Fresh
            }
            None => {
                let label = cwd
                    .as_deref()
                    .map(|cwd| build_label(cell.kind.as_str(), channel.as_deref(), cwd))
                    .unwrap_or_else(|| cell.kind.as_str().to_owned());
                fresh.push(label);
                CohortSeed::Fresh
            }
        };
        seeds.push(seed);
    }

    Ok(CohortResumePlan {
        seeds,
        cwd,
        channel,
        fresh,
        launch_group,
    })
}

fn cohort_spec_label(cells: &[CohortCell], team: Option<&str>) -> String {
    if let Some(team) = team {
        return team.to_owned();
    }
    let kinds = cells
        .iter()
        .map(|cell| cell.kind.as_str())
        .collect::<Vec<_>>()
        .join(",");
    if kinds.is_empty() {
        "<empty>".to_owned()
    } else {
        kinds
    }
}

fn cohort_candidates(
    agents: &[AgentState],
    worktree_exists: impl Fn(&Path) -> bool,
) -> Vec<&AgentState> {
    let mut candidates = agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .filter(|agent| agent_worktree(agent).is_some_and(|path| worktree_exists(&path)))
        .collect::<Vec<_>>();
    candidates.sort_by(cohort_newest_cmp);
    candidates
}

fn match_team_cohort<'a>(
    candidates: &[&'a AgentState],
    cells: &[CohortCell],
    team: &str,
) -> Vec<Option<&'a AgentState>> {
    let pool = candidates
        .iter()
        .copied()
        .filter(|agent| agent.team.as_deref() == Some(team))
        .collect::<Vec<_>>();
    let mut claimed = BTreeSet::new();
    let mut matches = Vec::with_capacity(cells.len());
    for cell in cells {
        let agent = if let Some(role) = cell.role.as_deref() {
            pool.iter().copied().find(|agent| {
                !claimed.contains(&agent.agent_id) && agent.role.as_deref() == Some(role)
            })
        } else {
            pool.iter()
                .copied()
                .find(|agent| !claimed.contains(&agent.agent_id) && agent.kind == cell.kind)
        };
        if let Some(agent) = agent {
            claimed.insert(agent.agent_id.clone());
        }
        matches.push(agent);
    }
    matches
}

fn match_single_cohort<'a>(
    candidates: &[&'a AgentState],
    cell: &CohortCell,
) -> Vec<Option<&'a AgentState>> {
    vec![
        candidates
            .iter()
            .copied()
            .find(|agent| agent.kind == cell.kind),
    ]
}

fn match_inline_cohort<'a>(
    candidates: &[&'a AgentState],
    cells: &[CohortCell],
) -> Vec<Option<&'a AgentState>> {
    let mut groups: BTreeMap<&str, Vec<&'a AgentState>> = BTreeMap::new();
    for agent in candidates {
        if let Some(group) = agent
            .launch_group
            .as_deref()
            .filter(|group| !group.is_empty())
        {
            groups.entry(group).or_default().push(*agent);
        }
    }
    let mut groups = groups.into_values().collect::<Vec<_>>();
    groups.sort_by(|a, b| {
        let newest_a = a
            .iter()
            .copied()
            .min_by(cohort_newest_cmp)
            .expect("group is non-empty");
        let newest_b = b
            .iter()
            .copied()
            .min_by(cohort_newest_cmp)
            .expect("group is non-empty");
        cohort_newest_cmp(&newest_a, &newest_b)
    });

    for group in groups {
        let matches = map_inline_group_to_cells(&group, cells);
        if matches.iter().any(Option::is_some) {
            return matches;
        }
    }
    vec![None; cells.len()]
}

fn map_inline_group_to_cells<'a>(
    group: &[&'a AgentState],
    cells: &[CohortCell],
) -> Vec<Option<&'a AgentState>> {
    let mut matches = vec![None; cells.len()];
    let mut claimed = BTreeSet::new();

    for agent in group {
        let Some(ordinal) = agent
            .launch_ordinal
            .and_then(|ordinal| usize::try_from(ordinal).ok())
        else {
            continue;
        };
        if ordinal >= cells.len() || matches[ordinal].is_some() {
            continue;
        }
        matches[ordinal] = Some(*agent);
        claimed.insert(agent.agent_id.clone());
    }

    for agent in group {
        if claimed.contains(&agent.agent_id) {
            continue;
        }
        let Some(index) = cells
            .iter()
            .enumerate()
            .find(|(index, cell)| matches[*index].is_none() && cell.kind == agent.kind)
            .map(|(index, _)| index)
        else {
            continue;
        };
        matches[index] = Some(*agent);
        claimed.insert(agent.agent_id.clone());
    }

    matches
}

fn supports_agent_resume(agent: &AgentState) -> bool {
    // A provisional `launch_...` id only names Rimz's pre-adoption placeholder.
    // Keep the matched cohort cell and relaunch it fresh instead of asking the
    // adapter to resume an id outside the provider session store.
    if agent.agent_id.is_provisional() {
        return false;
    }
    let Some(cwd) = agent_worktree(agent) else {
        return false;
    };
    find_adapter(&agent.kind)
        .is_some_and(|adapter| adapter.resume_command(&agent.agent_id, &cwd).is_some())
}

fn cohort_newest_cmp(a: &&AgentState, b: &&AgentState) -> std::cmp::Ordering {
    b.last_activity
        .cmp(&a.last_activity)
        .then_with(|| a.agent_id.cmp(&b.agent_id))
}

fn agent_worktree(agent: &AgentState) -> Option<PathBuf> {
    agent
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn cohort_agent_label(agent: &AgentState) -> String {
    match agent.name.as_deref().filter(|name| !name.is_empty()) {
        Some(name) => format!("{}:{} ({})", agent.kind.as_str(), name, agent.agent_id),
        None => format!("{}:{}", agent.kind.as_str(), agent.agent_id),
    }
}

fn cohort_fresh_label_for_agent(agent: &AgentState) -> String {
    let cwd = agent_worktree(agent).unwrap_or_default();
    build_label(agent.kind.as_str(), agent.channel.as_deref(), &cwd)
}

fn resume_tab_identity(channel: Option<&str>, cwd: &Path) -> ResumeTabIdentity {
    match channel.filter(|channel| !channel.is_empty()) {
        Some(channel) => ResumeTabIdentity::Channel(channel.to_owned()),
        None => ResumeTabIdentity::Cwd(cwd.to_path_buf()),
    }
}

fn disambiguate_resume_tab_labels(tabs: &mut [PlannedResumeTab]) {
    let mut label_counts = BTreeMap::new();
    for planned in tabs.iter() {
        *label_counts.entry(planned.tab.label.clone()).or_insert(0) += 1;
    }
    let relabel: BTreeSet<usize> = tabs
        .iter()
        .enumerate()
        .filter(|(_, planned)| label_counts[&planned.tab.label] > 1)
        .filter(|(_, planned)| matches!(planned.identity, ResumeTabIdentity::Cwd(_)))
        .map(|(index, _)| index)
        .collect();
    if relabel.is_empty() {
        return;
    }

    let mut used: HashSet<String> = tabs
        .iter()
        .enumerate()
        .filter(|(index, _)| !relabel.contains(index))
        .map(|(_, planned)| planned.tab.label.clone())
        .collect();
    for index in relabel {
        let base = parent_prefixed_label(&tabs[index].tab.cwd)
            .unwrap_or_else(|| tabs[index].tab.label.clone());
        tabs[index].tab.label = unique_label(&base, &mut used);
    }
}

fn parent_prefixed_label(cwd: &Path) -> Option<String> {
    let child = cwd.file_name()?.to_string_lossy();
    let parent = cwd.parent()?.file_name()?.to_string_lossy();
    Some(format!("#{parent}/{child}"))
}

fn unique_label(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_owned()) {
        return base.to_owned();
    }
    for ordinal in 2.. {
        let candidate = format!("{base}-{ordinal}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("unbounded ordinal search always yields a fresh label")
}

/// Wrapper argv for resuming one prior provider-native session with its durable
/// Rimz launch identity.
pub fn resume_command(rimz_bin: &Path, agent: &AgentState) -> Vec<String> {
    resume_command_with_channel(rimz_bin, agent, agent.channel.as_deref())
}

pub fn resume_command_with_channel(
    rimz_bin: &Path,
    agent: &AgentState,
    channel: Option<&str>,
) -> Vec<String> {
    let channel = agent
        .channel
        .as_deref()
        .filter(|channel| !channel.is_empty())
        .or_else(|| channel.filter(|channel| !channel.is_empty()));
    crate::harness::launch::exec_argv(
        rimz_bin,
        &crate::harness::launch::ExecInvocation {
            kind: agent.kind.as_str(),
            action: crate::harness::launch::ExecAction::Resume {
                session_id: agent.agent_id.as_str(),
            },
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: true,
            exit_on_run_completion: false,
            identity: crate::harness::launch::ExecIdentity {
                name: agent.name.as_deref(),
                profile: agent.profile.as_deref(),
                role: agent.role.as_deref(),
                team: agent.team.as_deref(),
                launch_group: agent.launch_group.as_deref(),
                launch_ordinal: agent.launch_ordinal,
                channel,
                // Resume did not replay model/effort before the wrapper grammar
                // moved here; keep argv behavior stable.
                model: None,
                effort: None,
                ..crate::harness::launch::ExecIdentity::default()
            },
        },
    )
}

/// A short, view-safe label for a resumed agent: `kind:<channel>`, falling back
/// to the worktree directory name, then `kind:agent`. Used in skip reports and
/// legacy per-agent tab title fallbacks.
pub fn build_label(kind: &str, channel: Option<&str>, worktree: &Path) -> String {
    format!("{kind}:{}", channel_short(channel, worktree))
}

/// A short, view-safe channel name: explicit channel, then worktree directory,
/// then `agent`.
pub fn channel_short(channel: Option<&str>, worktree: &Path) -> String {
    channel
        .filter(|channel| !channel.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            worktree
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "agent".to_owned())
}

/// A channel tab label from the worktree directory name, matching live
/// worktree-launch tabs. A main-repo non-worktree agent falls back to
/// `#<repo-name>` rather than the live `kind:repo` title because resume groups by
/// cwd.
pub fn channel_label(channel: Option<&str>, worktree: &Path) -> String {
    format!("#{}", channel_short(channel, worktree))
}

#[cfg(test)]
mod tests;
