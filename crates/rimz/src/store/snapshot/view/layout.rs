use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agents::{AgentState, AgentStatus};
use crate::ids::PaneId;
use crate::store::snapshot::row::SidebarRow;
use crate::workspace::RootClass;

use super::aggregate::AttentionWindows;
use super::score::{self, GitRung};
use super::{
    SidebarInactive, SidebarSnapshot, SidebarStatusCount, SidebarWorktreeGroup,
    SidebarWorktreeKind, WorktreePrState, WorktreeTrunkSync,
};

/// The branch shared by a group's branched rows, if any. Returns `None` for a
/// group with no branch information, leaving the caller's path-basename seed.
pub(super) fn group_branch_label(rows: &[SidebarRow]) -> Option<String> {
    rows.iter()
        .find_map(|row| row.worktree_branch.as_deref().filter(|b| !b.is_empty()))
        .map(ToOwned::to_owned)
}

#[derive(Clone, Copy)]
pub(super) struct GroupEntry<'a> {
    pub channel: Option<&'a str>,
    pub path: Option<&'a str>,
    pub branch: Option<&'a str>,
}

pub(super) struct GroupIdentity {
    pub kind: SidebarWorktreeKind,
    pub key: String,
    pub label: String,
    pub root_lane: bool,
}

pub(super) struct GroupResolver<'a> {
    project_root: Option<&'a Path>,
    worktree_roots: Vec<PathBuf>,
    worktree_home: Option<&'a Path>,
    root_class: RootClass,
    multi_branch_paths: BTreeSet<String>,
}

impl<'a> GroupResolver<'a> {
    pub(super) fn new<'entry>(
        snapshot: &'a SidebarSnapshot,
        entries: impl IntoIterator<Item = GroupEntry<'entry>>,
    ) -> Self {
        let mut worktree_roots: BTreeSet<PathBuf> =
            snapshot.worktree_roots.iter().cloned().collect();
        let mut branches_per_path: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for entry in entries {
            let (Some(path), Some(branch)) = (
                entry.path.filter(|path| !path.is_empty()),
                entry.branch.filter(|branch| !branch.is_empty()),
            ) else {
                continue;
            };
            worktree_roots.insert(PathBuf::from(path));
            branches_per_path.entry(path).or_default().insert(branch);
        }
        let multi_branch_paths = branches_per_path
            .into_iter()
            .filter(|(_, branches)| branches.len() > 1)
            .map(|(path, _)| path.to_owned())
            .collect();
        Self {
            project_root: snapshot.project_root.as_deref(),
            worktree_roots: worktree_roots.into_iter().collect(),
            worktree_home: snapshot.worktree_home.as_deref(),
            root_class: snapshot.root_class,
            multi_branch_paths,
        }
    }

    pub(super) fn resolve(&self, entry: GroupEntry<'_>) -> GroupIdentity {
        if let Some(channel) = entry.channel.filter(|channel| !channel.is_empty()) {
            return channel_group(channel);
        }
        let branch = entry.branch.filter(|branch| !branch.is_empty());
        let Some(path) = entry.path.filter(|path| !path.is_empty()) else {
            return branch.map_or_else(external_group, branch_group);
        };
        self.resolve_path(path, branch)
    }

    fn resolve_path(&self, path: &str, branch: Option<&str>) -> GroupIdentity {
        let cwd = Path::new(path);
        if self.is_owned_worktree(cwd) {
            return channel_group(&path_basename(cwd));
        }
        let matched = self.deepest_root(cwd);
        if self.groups_per_path(matched) {
            return GroupIdentity {
                root_lane: self.project_root == Some(cwd),
                ..worktree_group(path, cwd, branch, self.multi_branch_paths.contains(path))
            };
        }
        if let Some(root) = matched {
            return self.matched_root_group(root, branch, self.multi_branch_paths.contains(path));
        }
        branch
            .filter(|branch| path_mentions_branch(cwd, branch))
            .map_or_else(external_group, branch_group)
    }

    fn is_owned_worktree(&self, cwd: &Path) -> bool {
        self.worktree_home
            .is_some_and(|home| cwd.starts_with(home) && cwd != home)
            && self.project_root != Some(cwd)
    }

    fn deepest_root(&self, cwd: &Path) -> Option<&Path> {
        self.worktree_roots
            .iter()
            .map(PathBuf::as_path)
            .chain(self.project_root)
            .filter(|root| cwd.starts_with(root))
            .max_by_key(|root| root.components().count())
    }

    fn groups_per_path(&self, matched: Option<&Path>) -> bool {
        match matched {
            Some(root) => self.project_root == Some(root) && self.root_class == RootClass::Repo,
            None => self.project_root.is_none() && self.worktree_roots.is_empty(),
        }
    }

    fn matched_root_group(
        &self,
        root: &Path,
        branch: Option<&str>,
        split_by_branch: bool,
    ) -> GroupIdentity {
        let root_key = root.to_string_lossy().into_owned();
        if self.project_root == Some(root) {
            return GroupIdentity {
                kind: SidebarWorktreeKind::Root,
                key: root_key,
                label: "main".to_owned(),
                root_lane: true,
            };
        }
        worktree_group(&root_key, root, branch, split_by_branch)
    }
}

fn channel_group(channel: &str) -> GroupIdentity {
    GroupIdentity {
        kind: SidebarWorktreeKind::Channel,
        key: format!("channel:{channel}"),
        label: channel.to_owned(),
        root_lane: false,
    }
}

fn worktree_group(
    key_path: &str,
    label_path: &Path,
    branch: Option<&str>,
    split_by_branch: bool,
) -> GroupIdentity {
    GroupIdentity {
        kind: SidebarWorktreeKind::Worktree,
        key: branch.filter(|_| split_by_branch).map_or_else(
            || key_path.to_owned(),
            |branch| format!("{key_path}\n{branch}"),
        ),
        label: branch
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| path_basename(label_path)),
        root_lane: false,
    }
}

fn branch_group(branch: &str) -> GroupIdentity {
    GroupIdentity {
        kind: SidebarWorktreeKind::Worktree,
        key: format!("branch:{branch}"),
        label: branch.to_owned(),
        root_lane: false,
    }
}

fn external_group() -> GroupIdentity {
    GroupIdentity {
        kind: SidebarWorktreeKind::External,
        key: "external".to_owned(),
        label: "external".to_owned(),
        root_lane: false,
    }
}

fn path_mentions_branch(path: &Path, branch: &str) -> bool {
    let branch = branch
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(branch);
    if branch.is_empty() {
        return false;
    }
    let dash = format!("-{branch}");
    let underscore = format!("_{branch}");
    let dot = format!(".{branch}");
    path.components().any(|component| {
        let std::path::Component::Normal(component) = component else {
            return false;
        };
        let Some(component) = component.to_str() else {
            return false;
        };
        component == branch
            || component.ends_with(&dash)
            || component.ends_with(&underscore)
            || component.ends_with(&dot)
    })
}

/// The display basename of a group root, falling back to the full path for a
/// root with no final component (`/`).
fn path_basename(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

pub(super) fn status_counts(rows: &[SidebarRow]) -> Vec<SidebarStatusCount> {
    [
        AgentStatus::Waiting,
        AgentStatus::Failed,
        AgentStatus::Paused,
        AgentStatus::Success,
        AgentStatus::Running,
        AgentStatus::Sleeping,
        AgentStatus::Idle,
    ]
    .into_iter()
    .filter_map(|status| {
        let count = rows
            .iter()
            .filter(|row| row.attention_status() == Some(status))
            .count();
        (count > 0).then_some(SidebarStatusCount { status, count })
    })
    .collect()
}

pub(super) fn refresh_overlay_group(
    group: &mut SidebarWorktreeGroup,
    now: jiff::Timestamp,
    windows: AttentionWindows,
) {
    sort_rows(Arc::make_mut(&mut group.rows).as_mut_slice());
    group.status_counts = status_counts(&group.rows);
    group.inactive = group_inactive(group, now, windows);
}

pub(super) fn sort_groups_for_presentation(
    groups: &mut Arc<Vec<SidebarWorktreeGroup>>,
    now: jiff::Timestamp,
    windows: AttentionWindows,
    held: &BTreeMap<String, &HeldRank>,
) {
    for index in 0..groups.len() {
        let group = &groups[index];
        let rows_sorted = group
            .rows
            .iter()
            .is_sorted_by_key(|row| presentation_row_key(row, held));
        let inactive = group_inactive(group, now, windows);
        if rows_sorted && group.inactive == inactive {
            continue;
        }
        let group = &mut Arc::make_mut(groups)[index];
        if !rows_sorted {
            Arc::make_mut(&mut group.rows)
                .sort_by_cached_key(|row| presentation_row_key(row, held));
        }
        group.inactive = inactive;
    }
    sort_presented_groups(groups, held);
}

pub(super) fn sort_presented_groups(
    groups: &mut Arc<Vec<SidebarWorktreeGroup>>,
    held: &BTreeMap<String, &HeldRank>,
) {
    let key = |group: &SidebarWorktreeGroup| presentation_group_key(group, held);
    if !groups.iter().is_sorted_by_key(key) {
        Arc::make_mut(groups).sort_by_cached_key(key);
    }
}

/// A pane's captured presentation rank; only this module interprets its keys.
#[derive(Clone)]
pub(crate) struct HeldRank {
    row: RankKey,
    group: GroupRankKey,
}

pub(super) fn capture_held_rank(
    snapshot: &SidebarSnapshot,
    pane: &PaneId,
    held: &HashMap<PaneId, HeldRank>,
) -> Option<HeldRank> {
    let row = snapshot.row_owning_pane(pane)?;
    let group = snapshot
        .worktree_groups
        .iter()
        .find(|group| group.rows.iter().any(|member| member.id == row.id))?;
    let held = held_rows(snapshot, held);
    Some(HeldRank {
        row: presentation_row_key(row, &held),
        group: presentation_group_key(group, &held),
    })
}

pub(super) fn held_rows<'a>(
    snapshot: &SidebarSnapshot,
    held: &'a HashMap<PaneId, HeldRank>,
) -> BTreeMap<String, &'a HeldRank> {
    held.iter()
        .filter_map(|(pane, rank)| {
            snapshot
                .row_owning_pane(pane)
                .map(|row| (row.id.clone(), rank))
        })
        .collect()
}

fn presentation_row_key(row: &SidebarRow, held: &BTreeMap<String, &HeldRank>) -> RankKey {
    held.get(&row.id)
        .map_or_else(|| rank_key(row_rank_facts(row)), |held| held.row.clone())
}

fn presentation_group_key(
    group: &SidebarWorktreeGroup,
    held: &BTreeMap<String, &HeldRank>,
) -> GroupRankKey {
    if let Some(held) = group.rows.iter().find_map(|row| held.get(&row.id)) {
        return held.group.clone();
    }
    live_group_key(group)
}

pub(super) fn sort_rows_in_order<'a>(
    groups: &mut Arc<Vec<SidebarWorktreeGroup>>,
    order: impl Iterator<Item = &'a str>,
    held: &BTreeMap<String, &HeldRank>,
) {
    let positions: HashMap<_, _> = order.enumerate().map(|(index, id)| (id, index)).collect();
    let key = |row: &SidebarRow| {
        (
            held.get(&row.id)
                .map_or_else(|| row.is_process(), |held| held.row.is_process),
            positions
                .get(row.id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
        )
    };
    for index in 0..groups.len() {
        if groups[index].rows.iter().is_sorted_by_key(key) {
            continue;
        }
        Arc::make_mut(&mut Arc::make_mut(groups)[index].rows).sort_by_key(key);
    }
}

pub(super) fn sort_rows(rows: &mut [SidebarRow]) {
    rows.sort_by_cached_key(|row| rank_key(row_rank_facts(row)));
}

fn row_ordinal(row: &SidebarRow) -> Option<u64> {
    row.pane
        .as_ref()
        .and_then(|pane| pane.pane_id.creation_ordinal())
}

/// The row and group ranking inputs shared by cards and roster agents.
#[derive(Clone, Debug)]
struct RankFacts<'a> {
    is_process: bool,
    inactive: bool,
    archived: bool,
    status: Option<AgentStatus>,
    score: u32,
    attention_at_ms: Option<i64>,
    last_activity: jiff::Timestamp,
    registered_at: Option<jiff::Timestamp>,
    pane_ordinal: Option<u64>,
    launch_ordinal: Option<u32>,
    id: &'a str,
}

fn row_rank_facts(row: &SidebarRow) -> RankFacts<'_> {
    let status = row.attention_status();
    RankFacts {
        is_process: row.is_process(),
        inactive: row.inactive,
        archived: row.archived,
        status,
        score: row.attention_score,
        attention_at_ms: row.as_agent().and_then(|agent| agent.attention_at_ms),
        last_activity: row.last_activity,
        registered_at: row.as_agent().and_then(|agent| agent.registered_at),
        pane_ordinal: row_ordinal(row),
        launch_ordinal: row.launch_ordinal(),
        id: row.id.as_str(),
    }
}

fn agent_rank_facts(
    agent: &AgentState,
    now: jiff::Timestamp,
    windows: AttentionWindows,
    attention_at_ms: Option<i64>,
) -> RankFacts<'_> {
    let status = if agent.status == AgentStatus::Waiting {
        agent.effective_status(now)
    } else {
        agent.status
    };
    let age_secs = score::age_secs(now, agent.last_activity);
    let (inactive, archived, score) =
        score::attention_facts(Some(status), age_secs, windows, false);
    RankFacts {
        is_process: false,
        inactive,
        archived,
        status: Some(status),
        score,
        attention_at_ms,
        last_activity: agent.last_activity,
        registered_at: agent.registered_at,
        pane_ordinal: agent_ordinal(agent),
        launch_ordinal: agent.launch_ordinal,
        id: agent.agent_id.as_str(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct RankKey {
    is_process: bool,
    calm: bool,
    band: u8,
    stamp: StartKey<Reverse<i64>>,
    launch_ordinal: StartKey<u32>,
    pane_ordinal: StartKey<u64>,
    id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StartKey<T> {
    Present(T),
    Missing,
}

impl<T> From<Option<T>> for StartKey<T> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(value) => Self::Present(value),
            None => Self::Missing,
        }
    }
}

impl<T: Ord> Ord for StartKey<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        let left = match self {
            Self::Present(value) => Some(value),
            Self::Missing => None,
        };
        let right = match other {
            Self::Present(value) => Some(value),
            Self::Missing => None,
        };
        cmp_start_asc(left, right)
    }
}

impl<T: Ord> PartialOrd for StartKey<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Each card ranks alone: newest top-zone stamp, then calm band and newest
/// stamp. Process rows retain pane-creation order at the tail.
fn rank_key(facts: RankFacts<'_>) -> RankKey {
    let top = facts.status.is_some_and(AgentStatus::is_actionable);
    RankKey {
        is_process: facts.is_process,
        calm: !top,
        band: if top || facts.is_process {
            0
        } else {
            band(facts.inactive, facts.archived)
        },
        stamp: StartKey::from(if facts.is_process {
            None
        } else {
            facts
                .attention_at_ms
                .or_else(|| facts.registered_at.map(|birth| birth.as_millisecond()))
                .map(Reverse)
        }),
        launch_ordinal: StartKey::from(facts.launch_ordinal.filter(|_| !facts.is_process)),
        pane_ordinal: StartKey::from(facts.pane_ordinal),
        id: facts.id.to_owned(),
    }
}

/// Ascending by key, but a missing key sorts *last* — the opposite of
/// `Option::cmp`, which would float keyless rows (paneless script asks, detached
/// sessions, undated subagents) to the top of their bucket.
pub(super) fn cmp_start_asc<T: Ord>(left: Option<T>, right: Option<T>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub(super) fn compare_groups(
    left: &SidebarWorktreeGroup,
    right: &SidebarWorktreeGroup,
) -> Ordering {
    live_group_key(left).cmp(&live_group_key(right))
}

fn live_group_key(group: &SidebarWorktreeGroup) -> GroupRankKey {
    // External stays last; inactive groups follow active project work. Only
    // actionable groups use urgency. Member groups lead rowless runs within each
    // tier; rowless runs rank by node entry rather than agent clocks.
    group_sort_key(
        group.rows.iter().map(row_rank_facts),
        group.kind,
        group.inactive.is_some(),
        group.root_lane,
        &group.label,
        group.workflow.as_ref().map(|run| run.entered_at),
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct GroupRankKey {
    external: bool,
    inactive: bool,
    tier: u8,
    urgency: Reverse<u32>,
    rowless: bool,
    newest: StartKey<Reverse<jiff::Timestamp>>,
    ordinal: StartKey<Reverse<u64>>,
    label: String,
}

fn group_sort_key<'a>(
    facts: impl Iterator<Item = RankFacts<'a>>,
    kind: SidebarWorktreeKind,
    inactive: bool,
    root_lane: bool,
    label: &str,
    run_entered_at: Option<jiff::Timestamp>,
) -> GroupRankKey {
    let mut urgency = 0;
    let mut newest_registration = None;
    let mut agent_activity = None;
    let mut process_activity = None;
    let mut highest_ordinal = None;
    let mut actionable = false;
    let mut rowless = true;
    for facts in facts {
        rowless = false;
        actionable |= facts.status.is_some_and(AgentStatus::is_actionable);
        urgency = urgency.max(group_member_urgency(&facts));
        if facts.is_process {
            process_activity = process_activity.max(Some(facts.last_activity));
        } else {
            newest_registration = newest_registration.max(facts.registered_at);
            agent_activity = agent_activity.max(Some(facts.last_activity));
        }
        highest_ordinal = highest_ordinal.max(facts.pane_ordinal);
    }
    GroupRankKey {
        external: kind == SidebarWorktreeKind::External,
        inactive,
        tier: if actionable {
            0
        } else if root_lane {
            1
        } else {
            2
        },
        urgency: Reverse(if actionable { urgency } else { 0 }),
        rowless,
        newest: StartKey::from(
            if rowless {
                run_entered_at
            } else if inactive {
                agent_activity.or(process_activity)
            } else {
                newest_registration
            }
            .map(Reverse),
        ),
        ordinal: StartKey::from(
            highest_ordinal
                .filter(|_| newest_registration.is_none())
                .map(Reverse),
        ),
        label: label.to_owned(),
    }
}

fn group_git_rung(group: &SidebarWorktreeGroup) -> GitRung {
    let pr_finished = matches!(
        group.pr_state,
        Some(WorktreePrState::Merged | WorktreePrState::Closed)
    );
    let finished = pr_finished || group.trunk_sync == Some(WorktreeTrunkSync::Merged);
    score::git_rung(group.clean, finished)
}

fn group_inactive(
    group: &SidebarWorktreeGroup,
    now: jiff::Timestamp,
    windows: AttentionWindows,
) -> Option<SidebarInactive> {
    if group.root_lane
        || group.kind == SidebarWorktreeKind::External
        || group
            .rows
            .iter()
            .any(|row| member_blocks_inactivity(row.attention_status()))
    {
        return None;
    }
    let landed = group_git_rung(group) == GitRung::Done;
    if !landed && (group.workflow.is_some() || group.rows.iter().any(|row| row.unread)) {
        return None;
    }
    let quiet = group
        .rows
        .iter()
        .filter_map(|row| {
            row.as_agent()
                .map(|agent| agent.engaged_at.unwrap_or(row.last_activity))
        })
        .max()
        .or_else(|| group.rows.iter().map(|row| row.last_activity).max())
        .max(group.workflow.as_ref().map(|run| run.entered_at))?;
    let window = if landed {
        windows.inactive_after_secs
    } else {
        windows.archive_after_secs
    };
    (now.duration_since(quiet).as_secs() > i64::from(window)).then_some(if landed {
        SidebarInactive::Landed
    } else {
        SidebarInactive::Unlanded
    })
}

fn member_blocks_inactivity(status: Option<AgentStatus>) -> bool {
    status.is_some_and(|status| status == AgentStatus::Running || status.is_attention())
}

fn group_member_urgency(facts: &RankFacts<'_>) -> u32 {
    if facts.status.is_some_and(AgentStatus::is_actionable) {
        facts.score
    } else {
        0
    }
}

/// Macro bands: hot work, warm work past the inactive window, and archived work
/// past the archive window. Read state never changes the band. Actionable rows
/// stay hot while alive; calm and parked rows cool with age. The `external` partition is group-only
/// ([`compare_groups`]).
fn band(inactive: bool, archived: bool) -> u8 {
    if archived {
        2
    } else if inactive {
        1
    } else {
        0
    }
}

/// One worktree's live agents, ranked within the group like the sidebar.
#[derive(Debug)]
pub struct AgentWorktreeGroup<'a> {
    pub key: String,
    pub label: String,
    pub kind: SidebarWorktreeKind,
    pub agents: Vec<&'a AgentState>,
}

impl AgentWorktreeGroup<'_> {
    pub fn team(&self) -> Option<&str> {
        super::cohort_team(self.agents.iter().map(|agent| agent.team.as_deref()))
    }
}

/// Group root agents by worktree and rank them the way the sidebar ranks rows
/// and groups: actionable groups first, then the root lane, then newest agent
/// registration first, with `external` last. The roster has no inactive partition.
/// Within groups, actionable agents lead, newest attention stamp first; calm
/// agents follow by band and newest stamp. Stamps come from the snapshot's
/// enriched rows, falling back to registration. The roster uses effective agent
/// statuses, which can differ from read-side projections such as child lifting.
pub fn group_live_agents_by_worktree<'a>(
    agents: &[&'a AgentState],
    snapshot: &SidebarSnapshot,
) -> Vec<AgentWorktreeGroup<'a>> {
    let windows = AttentionWindows::from_config(&snapshot.attention);
    let stamps: BTreeMap<_, _> = snapshot
        .rows()
        .filter_map(|row| {
            Some((
                (row.name.as_str(), row.id.as_str()),
                row.as_agent()?.attention_at_ms?,
            ))
        })
        .collect();
    let resolver = GroupResolver::new(
        snapshot,
        agents.iter().map(|agent| GroupEntry {
            channel: agent.channel.as_deref(),
            path: agent.lane_path(),
            branch: agent.worktree_branch.as_deref(),
        }),
    );
    let mut by_key: BTreeMap<String, (bool, AgentWorktreeGroup<'a>)> = BTreeMap::new();
    for &agent in agents {
        let identity = resolver.resolve(GroupEntry {
            channel: agent.channel.as_deref(),
            path: agent.lane_path(),
            branch: agent.worktree_branch.as_deref(),
        });
        by_key
            .entry(identity.key.clone())
            .or_insert_with(|| {
                (
                    identity.root_lane,
                    AgentWorktreeGroup {
                        key: identity.key,
                        label: identity.label,
                        kind: identity.kind,
                        agents: Vec::new(),
                    },
                )
            })
            .1
            .agents
            .push(agent);
    }
    let mut groups: Vec<_> = by_key.into_values().collect();
    for (_, group) in &mut groups {
        group.agents.sort_by_cached_key(|agent| {
            rank_key(agent_rank_facts(
                agent,
                snapshot.now,
                windows,
                stamps
                    .get(&(agent.kind.as_str(), agent.agent_id.as_str()))
                    .copied(),
            ))
        });
    }
    groups.sort_by_cached_key(|(root_lane, group)| {
        group_sort_key(
            group
                .agents
                .iter()
                .copied()
                .map(|agent| agent_rank_facts(agent, snapshot.now, windows, None)),
            group.kind,
            false,
            *root_lane,
            &group.label,
            None,
        )
    });
    groups.into_iter().map(|(_, group)| group).collect()
}

/// Pane creation ordinal, the roster's final ordinal tiebreak, as in the sidebar.
fn agent_ordinal(agent: &AgentState) -> Option<u64> {
    agent
        .pane
        .as_ref()
        .and_then(|pane| pane.pane_id.creation_ordinal())
}
