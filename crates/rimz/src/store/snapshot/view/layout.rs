use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agents::{AgentState, AgentStatus};
use crate::ids::{Lane, LaneHome, LaneName, PaneId};
use crate::store::snapshot::row::SidebarRow;
use crate::workspace::RootClass;

use super::aggregate::AttentionWindows;
use super::score::{self, GitRung};
use super::{
    SidebarInactive, SidebarPlace, SidebarSnapshot, SidebarStatusCount, SidebarWorktreeGroup,
    SidebarWorktreeKind, WorktreePrState, WorktreeTrunkSync,
};

/// The branch the first of a group's rows reported, if any; the header shows it
/// beside the lane label until the checkout's live branch replaces it. Only a
/// checkout has a branch: a root pod, the catch-all and a lane with no place
/// ignore one a stray row reported.
pub(super) fn group_branch_label<'b>(
    kind: SidebarWorktreeKind,
    place: Option<&SidebarPlace>,
    branches: impl IntoIterator<Item = Option<&'b str>>,
) -> Option<String> {
    let checkout = matches!(
        kind,
        SidebarWorktreeKind::Worktree | SidebarWorktreeKind::Channel
    ) && place.is_some();
    checkout
        .then(|| {
            branches
                .into_iter()
                .find_map(|branch| branch.filter(|branch| !branch.is_empty()))
        })
        .flatten()
        .map(ToOwned::to_owned)
}

/// The branch a group header draws beside its lane label: only one that differs
/// from the label.
pub(super) fn shown_branch<'b>(branch: Option<&'b str>, label: &str) -> Option<&'b str> {
    branch.filter(|branch| *branch != label)
}

#[derive(Clone, Copy)]
pub(super) struct GroupEntry<'a> {
    /// The row's stored lane, which alone decides its group.
    pub lane: Option<&'a Lane>,
    /// The `channel` stamp of a row an older binary wrote before rows stored
    /// their lane. Link 3 deletes it.
    pub stamp: Option<&'a str>,
    pub path: Option<&'a str>,
    pub branch: Option<&'a str>,
}

pub(super) struct GroupIdentity {
    pub kind: SidebarWorktreeKind,
    pub key: String,
    pub label: String,
    pub root_lane: bool,
    pub place: Option<SidebarPlace>,
}

pub(super) struct GroupResolver<'a> {
    project_root: Option<&'a Path>,
    worktree_roots: Vec<PathBuf>,
    listed_roots: &'a [PathBuf],
    worktree_channels: &'a BTreeMap<PathBuf, String>,
    channel_places: BTreeMap<&'a str, &'a Path>,
    root_class: RootClass,
}

impl<'a> GroupResolver<'a> {
    pub(super) fn new<'entry>(
        snapshot: &'a SidebarSnapshot,
        entries: impl IntoIterator<Item = GroupEntry<'entry>>,
    ) -> Self {
        // A row that reported a branch names a checkout at its path, which
        // places the shells working there.
        let mut worktree_roots: BTreeSet<PathBuf> =
            snapshot.worktree_roots.iter().cloned().collect();
        worktree_roots.extend(
            entries
                .into_iter()
                .filter(|entry| entry.branch.is_some_and(|branch| !branch.is_empty()))
                .filter_map(|entry| entry.path.filter(|path| !path.is_empty()))
                .map(PathBuf::from),
        );
        let mut channel_places = BTreeMap::new();
        for (path, channel) in &snapshot.worktree_channels {
            if snapshot.worktree_roots.contains(path) {
                channel_places
                    .entry(channel.as_str())
                    .or_insert(path.as_path());
            }
        }
        Self {
            project_root: snapshot.project_root.as_deref(),
            worktree_roots: worktree_roots.into_iter().collect(),
            listed_roots: &snapshot.worktree_roots,
            worktree_channels: &snapshot.worktree_channels,
            channel_places,
            root_class: snapshot.root_class,
        }
    }

    /// A row with a lane groups by that lane alone; any other row by its path,
    /// in the own lane of the deepest known root holding it.
    pub(super) fn resolve(&self, entry: GroupEntry<'_>) -> GroupIdentity {
        if let Some(lane) = entry.lane {
            return self.lane_group(lane);
        }
        if let Some(stamp) = entry.stamp.filter(|stamp| !stamp.is_empty()) {
            return self.channel_group(stamp);
        }
        let Some(path) = entry.path.filter(|path| !path.is_empty()) else {
            return entry
                .branch
                .filter(|branch| !branch.is_empty())
                .map_or_else(external_group, branch_group);
        };
        self.resolve_path(path)
    }

    /// Trunk's own lane keeps its path group, and a foreign checkout's own lane
    /// is the group of its home path, listed or not; a worktree's own lane and
    /// every named lane are channels homed at the lane's checkout.
    fn lane_group(&self, lane: &Lane) -> GroupIdentity {
        match (lane.name(), lane.home()) {
            (None, LaneHome::Trunk) => return self.resolve_path(lane.path()),
            (None, LaneHome::Foreign) => return self.matched_root_group(Path::new(lane.path())),
            _ => {}
        }
        let label = lane.label();
        GroupIdentity {
            kind: SidebarWorktreeKind::Channel,
            key: format!("channel:{label}"),
            label,
            root_lane: false,
            place: Some(SidebarPlace {
                path: PathBuf::from(lane.path()),
                foreign: lane.home() == LaneHome::Foreign,
            }),
        }
    }

    fn resolve_path(&self, path: &str) -> GroupIdentity {
        let Some(root) = self.deepest_root(Path::new(path)) else {
            return external_group();
        };
        self.matched_root_group(root)
    }

    fn deepest_root(&self, cwd: &Path) -> Option<&Path> {
        self.worktree_roots
            .iter()
            .map(PathBuf::as_path)
            .chain(self.project_root)
            .filter(|root| cwd.starts_with(root))
            .max_by_key(|root| root.components().count())
    }

    fn channel_group(&self, channel: &str) -> GroupIdentity {
        GroupIdentity {
            kind: SidebarWorktreeKind::Channel,
            key: format!("channel:{channel}"),
            label: channel.to_owned(),
            root_lane: false,
            place: self.channel_places.get(channel).map(|path| SidebarPlace {
                path: path.to_path_buf(),
                foreign: false,
            }),
        }
    }

    fn matched_root_group(&self, root: &Path) -> GroupIdentity {
        let root_key = root.to_string_lossy().into_owned();
        let root_lane = self.project_root == Some(root);
        let place = SidebarPlace {
            path: root.to_path_buf(),
            foreign: self.project_root.is_some_and(|project| {
                !root.starts_with(project) && !self.listed_roots.iter().any(|listed| listed == root)
            }),
        };
        if root_lane && self.root_class != RootClass::Repo {
            return GroupIdentity {
                kind: SidebarWorktreeKind::Root,
                key: root_key,
                label: LaneName::TRUNK_LABEL.to_owned(),
                root_lane: true,
                place: Some(place),
            };
        }
        if !root_lane
            && self.listed_roots.iter().any(|listed| listed == root)
            && let Some(channel) = self.worktree_channels.get(root)
        {
            return self.channel_group(channel);
        }
        GroupIdentity {
            kind: SidebarWorktreeKind::Worktree,
            key: root_key,
            label: if root_lane {
                LaneName::TRUNK_LABEL.to_owned()
            } else {
                path_basename(root)
            },
            root_lane,
            place: Some(place),
        }
    }
}

fn branch_group(branch: &str) -> GroupIdentity {
    GroupIdentity {
        kind: SidebarWorktreeKind::Worktree,
        key: format!("branch:{branch}"),
        label: branch.to_owned(),
        root_lane: false,
        place: None,
    }
}

fn external_group() -> GroupIdentity {
    GroupIdentity {
        kind: SidebarWorktreeKind::External,
        key: "external".to_owned(),
        label: "external".to_owned(),
        root_lane: false,
        place: None,
    }
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
) {
    let positions: HashMap<_, _> = order.enumerate().map(|(index, id)| (id, index)).collect();
    let key = |row: &SidebarRow| {
        positions
            .get(row.id.as_str())
            .copied()
            .unwrap_or(usize::MAX)
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
    /// `None` for a process row, so every agent precedes one at an equal stamp.
    launch_ordinal: Option<Option<u32>>,
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
        attention_at_ms: row
            .attention_at_ms
            .or_else(|| row.is_process().then(|| row.last_activity.as_millisecond())),
        last_activity: row.last_activity,
        registered_at: row.as_agent().and_then(|agent| agent.registered_at),
        pane_ordinal: row_ordinal(row),
        launch_ordinal: row.as_agent().map(|agent| agent.launch_ordinal),
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
    let (inactive, archived, score) = score::attention_facts(Some(status), age_secs, windows);
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
        launch_ordinal: Some(agent.launch_ordinal),
        id: agent.agent_id.as_str(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct RankKey {
    calm: bool,
    band: u8,
    stamp: StartKey<Reverse<i64>>,
    launch_ordinal: StartKey<StartKey<u32>>,
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

/// Asks and failures lead by newest stamp; calm cards and process rows share
/// the remaining bands, newest stamp first within each.
fn rank_key(facts: RankFacts<'_>) -> RankKey {
    let top = facts.status.is_some_and(AgentStatus::is_actionable);
    RankKey {
        calm: !top,
        band: if top {
            0
        } else {
            band(facts.inactive, facts.archived)
        },
        stamp: StartKey::from(
            facts
                .attention_at_ms
                .or_else(|| facts.registered_at.map(|birth| birth.as_millisecond()))
                .map(Reverse),
        ),
        launch_ordinal: StartKey::from(facts.launch_ordinal.map(StartKey::from)),
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
    // Active project work leads external, then inactive groups. Only
    // actionable groups use urgency. Unlanded groups lead landed ones after
    // urgency; members lead rowless groups within each slice. Rowless runs
    // rank by node entry rather than agent clocks, and a rowless group with no
    // run (one an open pull request holds) follows them by label.
    group_sort_key(
        group.rows.iter().map(row_rank_facts),
        group.kind,
        group.inactive.is_some(),
        group_landed(group),
        group.root_lane,
        &group.label,
        group.workflow.as_ref().map(|run| run.entered_at),
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct GroupRankKey {
    partition: GroupPartition,
    tier: u8,
    urgency: Reverse<u32>,
    landed: bool,
    rowless: bool,
    newest: StartKey<Reverse<jiff::Timestamp>>,
    ordinal: StartKey<Reverse<u64>>,
    label: String,
}

/// The presentation partitions in column order, shared with frozen layouts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum GroupPartition {
    Active,
    External,
    Inactive,
}

pub(crate) fn group_partition(kind: SidebarWorktreeKind, inactive: bool) -> GroupPartition {
    if inactive {
        GroupPartition::Inactive
    } else if kind == SidebarWorktreeKind::External {
        GroupPartition::External
    } else {
        GroupPartition::Active
    }
}

fn group_sort_key<'a>(
    facts: impl Iterator<Item = RankFacts<'a>>,
    kind: SidebarWorktreeKind,
    inactive: bool,
    landed: bool,
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
        partition: group_partition(kind, inactive),
        tier: if actionable {
            0
        } else if root_lane {
            1
        } else {
            2
        },
        urgency: Reverse(if actionable { urgency } else { 0 }),
        landed: !inactive && landed,
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

fn group_landed(group: &SidebarWorktreeGroup) -> bool {
    let pr_finished = matches!(
        group.pr_state,
        Some(WorktreePrState::Merged | WorktreePrState::Closed)
    );
    let finished = pr_finished || group.trunk_sync == Some(WorktreeTrunkSync::Merged);
    score::git_rung(group.clean, finished) == GitRung::Done
}

fn group_inactive(
    group: &SidebarWorktreeGroup,
    now: jiff::Timestamp,
    windows: AttentionWindows,
) -> Option<SidebarInactive> {
    if group.root_lane
        || group.kind == SidebarWorktreeKind::External
        || group.stands_without_rows()
        || group
            .rows
            .iter()
            .any(|row| member_blocks_inactivity(row.attention_status()))
    {
        return None;
    }
    let landed = group_landed(group);
    if !landed && group.rows.iter().any(|row| row.unread) {
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
        .or_else(|| group.rows.iter().map(|row| row.last_activity).max())?;
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
/// stay hot while alive; calm cards and process rows cool with age. The
/// `external` partition is group-only ([`compare_groups`]).
fn band(inactive: bool, archived: bool) -> u8 {
    if archived {
        2
    } else if inactive {
        1
    } else {
        0
    }
}

/// One worktree's live agents in snapshot attention order, without the
/// renderer's cohort blocks or workflow-owner section.
#[derive(Debug)]
pub struct AgentWorktreeGroup<'a> {
    pub key: String,
    pub label: String,
    pub kind: SidebarWorktreeKind,
    pub place: Option<SidebarPlace>,
    /// The published group's live branch, else the one its agents reported.
    pub branch: Option<String>,
    pub agents: Vec<&'a AgentState>,
}

impl AgentWorktreeGroup<'_> {
    pub fn team(&self) -> Option<&str> {
        super::cohort_team(self.agents.iter().map(|agent| agent.team.as_deref()))
    }

    /// The branch the header draws beside the lane label, as the sidebar's does.
    pub fn header_branch(&self) -> Option<&str> {
        shown_branch(self.branch.as_deref(), &self.label)
    }
}

/// Group root agents by worktree: actionable groups first, then the root lane,
/// then other groups, with `external` last. After urgency, unlanded groups lead
/// landed ones, using git facts from the published snapshot's matching group key.
/// Within each slice, newest agent registration leads. The roster has no
/// inactive partition, cohort blocks or workflow-owner section.
/// Within groups, actionable agents lead, newest attention stamp first; calm
/// agents follow by band and newest stamp. Stamps come from the snapshot's
/// enriched rows, falling back to registration. The roster uses effective agent
/// statuses, which can differ from read-side projections such as child lifting.
pub fn group_live_agents_by_worktree<'a>(
    agents: &[&'a AgentState],
    snapshot: &SidebarSnapshot,
) -> Vec<AgentWorktreeGroup<'a>> {
    let windows = AttentionWindows::from_config(&snapshot.attention);
    let landed_keys: BTreeSet<_> = snapshot
        .worktree_groups
        .iter()
        .filter(|group| group_landed(group))
        .map(|group| group.key.as_str())
        .collect();
    let stamps: BTreeMap<_, _> = snapshot
        .rows()
        .filter(|row| row.is_agent())
        .filter_map(|row| Some(((row.name.as_str(), row.id.as_str()), row.attention_at_ms?)))
        .collect();
    let laneless: Vec<_> = agents.iter().map(|agent| laneless_stamp(agent)).collect();
    let resolver = GroupResolver::new(
        snapshot,
        agents
            .iter()
            .zip(&laneless)
            .map(|(agent, stamp)| agent_entry(agent, stamp.as_ref())),
    );
    let mut by_key: BTreeMap<String, (bool, AgentWorktreeGroup<'a>)> = BTreeMap::new();
    for (&agent, stamp) in agents.iter().zip(&laneless) {
        let identity = resolver.resolve(agent_entry(agent, stamp.as_ref()));
        by_key
            .entry(identity.key.clone())
            .or_insert_with(|| {
                (
                    identity.root_lane,
                    AgentWorktreeGroup {
                        key: identity.key,
                        label: identity.label,
                        kind: identity.kind,
                        place: identity.place,
                        branch: None,
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
        group.branch = snapshot
            .worktree_groups
            .iter()
            .find(|published| published.key == group.key)
            .and_then(|published| published.branch.clone())
            .or_else(|| {
                group_branch_label(
                    group.kind,
                    group.place.as_ref(),
                    group
                        .agents
                        .iter()
                        .map(|agent| agent.worktree_branch.as_deref()),
                )
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
            landed_keys.contains(group.key.as_str()),
            *root_lane,
            &group.label,
            None,
        )
    });
    groups.into_iter().map(|(_, group)| group).collect()
}

/// What the listing groups an agent by, as the sidebar groups its row.
fn agent_entry<'e>(agent: &'e AgentState, stamp: Option<&'e LaneName>) -> GroupEntry<'e> {
    GroupEntry {
        lane: agent.lane.as_ref(),
        stamp: agent.stamp_label().or(stamp.map(LaneName::as_str)),
        path: agent.lane_path(),
        branch: agent.worktree_branch.as_deref(),
    }
}

/// The stamped name of a lane-less row an older binary wrote, as its sidebar row
/// carries it. Link 3 deletes it.
fn laneless_stamp(agent: &AgentState) -> Option<LaneName> {
    if agent.lane.is_some() || agent.own_lane_path().is_some() {
        return None;
    }
    agent.lane_name()
}

/// Pane creation ordinal, the roster's final ordinal tiebreak, as in the sidebar.
fn agent_ordinal(agent: &AgentState) -> Option<u64> {
    agent
        .pane
        .as_ref()
        .and_then(|pane| pane.pane_id.creation_ordinal())
}
