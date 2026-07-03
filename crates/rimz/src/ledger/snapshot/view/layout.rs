use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::agents::{AgentState, AgentStatus};
use crate::ledger::snapshot::row::SidebarRow;
use crate::workspace::RootClass;

use super::{SidebarSnapshot, SidebarStatusCount, SidebarWorktreeGroup, SidebarWorktreeKind};

pub(super) const WORKTREE_ROW_CAP: usize = 6;

/// The branch shared by a group's branched rows, if any. Returns `None` for a
/// group with no branch information, leaving the caller's path-basename seed.
pub(super) fn group_branch_label(rows: &[SidebarRow]) -> Option<String> {
    rows.iter()
        .find_map(|row| row.worktree_branch.as_deref().filter(|b| !b.is_empty()))
        .map(ToOwned::to_owned)
}

/// The worktree paths that host more than one branch. A path keyed by branch
/// (rather than by path alone) keeps each branch its own group instead of
/// collapsing two checkouts under one mislabeled header. Shared by the live
/// row fold ([`build_worktree_groups_from_rows`]) and the `rimz agents list`
/// roster ([`group_live_agents_by_worktree`]) so both split identically.
pub(super) fn multi_branch_paths<'a>(
    entries: impl Iterator<Item = (Option<&'a str>, Option<&'a str>)>,
) -> BTreeSet<String> {
    let mut branches_per_path: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (path, branch) in entries {
        if let (Some(path), Some(branch)) = (
            path.filter(|path| !path.is_empty()),
            branch.filter(|branch| !branch.is_empty()),
        ) {
            branches_per_path.entry(path).or_default().insert(branch);
        }
    }
    branches_per_path
        .into_iter()
        .filter(|(_, branches)| branches.len() > 1)
        .map(|(path, _)| path.to_owned())
        .collect()
}

pub(super) fn effective_worktree_roots<'a>(
    worktree_roots: &[PathBuf],
    entries: impl Iterator<Item = (Option<&'a str>, Option<&'a str>)>,
) -> Vec<PathBuf> {
    let mut roots: BTreeSet<PathBuf> = worktree_roots.iter().cloned().collect();
    for (path, branch) in entries {
        let Some(path) = path.filter(|path| !path.is_empty()) else {
            continue;
        };
        if branch.is_some_and(|branch| !branch.is_empty()) {
            roots.insert(PathBuf::from(path));
        }
    }
    roots.into_iter().collect()
}

pub(super) fn worktree_group_key(
    explicit_channel: Option<&str>,
    path: Option<&str>,
    branch: Option<&str>,
    split_by_branch: bool,
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
    root_class: RootClass,
) -> (SidebarWorktreeKind, String, String) {
    if let Some(channel) = explicit_channel.filter(|channel| !channel.is_empty()) {
        return (
            SidebarWorktreeKind::Channel,
            format!("channel:{channel}"),
            channel.to_owned(),
        );
    }
    let branch = branch.filter(|branch| !branch.is_empty());
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        // A cwd belongs to the *deepest* group root that contains it: the room
        // root or any group root — a repo room's `git worktree list` checkouts
        // plus every git-backed row's own resolved toplevel. Keying on the
        // matched root is what folds every pane of one checkout into one pod.
        // Two cases keep per-path pods: a repo room's own checkout (so a nested
        // worktree the enumeration hasn't caught up with never folds into the
        // main pod), and a snapshot with no known root and no known roots. A cwd
        // outside every root (a home shell, `/tmp`, CI) falls through to the
        // `external` catch-all unless its path still carries the reported branch
        // name — the short pre-enumeration window for a real worktree checkout.
        let cwd = Path::new(path);
        let matched = worktree_roots
            .iter()
            .map(PathBuf::as_path)
            .chain(project_root)
            .filter(|root| is_within(root, cwd))
            .max_by_key(|root| root.components().count());
        let per_path = match matched {
            Some(root) => project_root == Some(root) && root_class == RootClass::Repo,
            None => project_root.is_none() && worktree_roots.is_empty(),
        };
        if per_path {
            let label = branch
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| path_basename(cwd));
            // Disambiguate the key by branch only for a path that holds more
            // than one — a newline can appear in neither a path nor a branch, so
            // it is an unambiguous separator. The git projection recovers the
            // bare path from the key's first line, so the split never breaks
            // git reads.
            let key = match branch.filter(|_| split_by_branch) {
                Some(branch) => format!("{path}\n{branch}"),
                None => path.to_owned(),
            };
            return (SidebarWorktreeKind::Worktree, key, label);
        }
        if let Some(root) = matched {
            let root_key = root.to_string_lossy().into_owned();
            // The room root of a non-repo room: one name-only pod for panes at
            // the root and in non-repo subdirs. Branches never split or label
            // it — a non-repo root has no git story to disagree about.
            if project_root == Some(root) {
                let label = path_basename(root);
                return (SidebarWorktreeKind::Root, root_key, label);
            }
            let label = branch
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| path_basename(root));
            let key = match branch.filter(|_| split_by_branch) {
                Some(branch) => format!("{root_key}\n{branch}"),
                None => root_key,
            };
            return (SidebarWorktreeKind::Worktree, key, label);
        }
        if let Some(branch) = branch.filter(|branch| path_mentions_branch(cwd, branch)) {
            return branch_group(branch);
        }
        return (
            SidebarWorktreeKind::External,
            "external".to_owned(),
            "external".to_owned(),
        );
    }
    if let Some(branch) = branch {
        // Branch-only rows have no cwd to anchor to a project root. Keep their
        // label visible until the next pane or workspace observation supplies a
        // path.
        return branch_group(branch);
    }
    // Catch-all: untethered scripts/CI and out-of-project shells. `external`
    // is both the stable grouping key and the header label, so it reads as
    // "outside the project."
    (
        SidebarWorktreeKind::External,
        "external".to_owned(),
        "external".to_owned(),
    )
}

fn branch_group(branch: &str) -> (SidebarWorktreeKind, String, String) {
    (
        SidebarWorktreeKind::Worktree,
        format!("branch:{branch}"),
        branch.to_owned(),
    )
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

/// True when `path` is `root` itself or nested under it, compared by path
/// components so `/home/userX` is not treated as under `/home/user`. This
/// is a lexical test on the raw cwd the mux reported — no filesystem
/// canonicalization — keeping the reducer pure. Used against both the project
/// root and each enumerated worktree root to decide a cwd's pod.
pub(super) fn is_within(root: &Path, path: &Path) -> bool {
    let mut root_components = root.components();
    let mut path_components = path.components();
    loop {
        match (root_components.next(), path_components.next()) {
            (Some(r), Some(p)) if r == p => continue,
            (Some(_), _) => return false,
            (None, _) => return true,
        }
    }
}

pub(super) fn status_counts(rows: &[SidebarRow]) -> Vec<SidebarStatusCount> {
    [
        AgentStatus::Waiting,
        AgentStatus::Failed,
        AgentStatus::Paused,
        AgentStatus::Success,
        AgentStatus::Running,
        AgentStatus::Idle,
    ]
    .into_iter()
    .filter_map(|status| {
        let count = rows
            .iter()
            .filter(|row| row.status() == Some(status))
            .count();
        (count > 0).then_some(SidebarStatusCount { status, count })
    })
    .collect()
}

pub(super) fn refresh_overlay_group(group: &mut SidebarWorktreeGroup) {
    sort_rows(&mut group.rows);
    group.status_counts = status_counts(&group.rows);
    let total = group.rows.len().saturating_add(group.hidden_count);
    let rows = std::mem::take(&mut group.rows);
    group.rows = capped_rows(rows);
    group.hidden_count = total.saturating_sub(group.rows.len());
}

pub(super) fn sort_groups_for_presentation(groups: &mut [SidebarWorktreeGroup]) {
    for group in groups.iter_mut() {
        sort_rows(&mut group.rows);
    }
    groups.sort_by(compare_groups);
}

pub(super) fn sort_rows(rows: &mut [SidebarRow]) {
    let cohorts = rank_cohort_blocks(rows.iter().map(row_rank_facts));
    rows.sort_by_cached_key(|row| rank_key(row_rank_facts(row), &cohorts));
}

/// Trim a group's idle/process tail to `WORKTREE_ROW_CAP`, always keeping unread
/// rows, non-idle agent rows, and the focused pane. Inactive success rows still
/// stay visible so a renderer never drops an unread stamp before receipts
/// converge; sticky unread idle rows stay visible until the human reads them,
/// and the first live process row stays visible when it is the group's only
/// live member, so capping never turns a live shell's group into an inactive
/// one. Ordinary inactive idle rows are the first calm rows hidden behind `+K
/// more`.
pub(super) fn capped_rows(rows: Vec<SidebarRow>) -> Vec<SidebarRow> {
    let process_is_only_live_member = rows.iter().map(row_band).min() == Some(1)
        && rows
            .iter()
            .filter(|row| row_band(row) == 1)
            .all(SidebarRow::is_process);
    let liveness_process_id = if process_is_only_live_member {
        rows.iter()
            .find(|row| row.is_process() && row_band(row) == 1)
            .map(|row| row.id.clone())
    } else {
        None
    };
    let mut visible = Vec::new();
    for row in rows {
        if row.unread
            || row
                .status()
                .is_some_and(|status| status != AgentStatus::Idle)
            || row.pane.as_ref().is_some_and(|pane| pane.is_focused)
            || liveness_process_id.as_deref() == Some(row.id.as_str())
            || visible.len() < WORKTREE_ROW_CAP
        {
            visible.push(row);
        }
    }
    visible
}

fn row_ordinal(row: &SidebarRow) -> Option<u64> {
    row.pane
        .as_ref()
        .and_then(|pane| pane.pane_id.creation_ordinal())
}

/// Everything the attention ladder reads off one row or roster agent.
#[derive(Clone, Debug)]
struct RankFacts<'a> {
    is_process: bool,
    unread: bool,
    inactive: bool,
    status: Option<AgentStatus>,
    last_activity: jiff::Timestamp,
    pane_ordinal: Option<u64>,
    cohort: Option<&'a str>,
    launch_ordinal: Option<u32>,
    id: &'a str,
}

fn row_rank_facts(row: &SidebarRow) -> RankFacts<'_> {
    RankFacts {
        is_process: row.is_process(),
        unread: row.unread,
        inactive: row.inactive,
        status: row.status(),
        last_activity: row.last_activity,
        pane_ordinal: row_ordinal(row),
        cohort: row.launch_cohort(),
        launch_ordinal: row.launch_ordinal(),
        id: row.id.as_str(),
    }
}

fn agent_rank_facts(agent: &AgentState) -> RankFacts<'_> {
    RankFacts {
        is_process: false,
        unread: false,
        inactive: false,
        status: Some(agent.status),
        last_activity: agent.last_activity,
        pane_ordinal: agent_ordinal(agent),
        cohort: agent_launch_cohort(agent),
        launch_ordinal: agent.launch_ordinal,
        id: agent.agent_id.as_str(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct RankKey {
    block: BlockKey,
    within: WithinKey,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct BlockKey {
    is_process: bool,
    band: u8,
    rank: u8,
    tiebreak: BucketTiebreak,
    identity: String,
    cohort_absent: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct WithinKey {
    launch_ordinal: StartKey<u32>,
    pane_ordinal: StartKey<u64>,
    id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum BucketTiebreak {
    Attention(jiff::Timestamp),
    Calm(StartKey<u64>),
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

#[derive(Clone, Debug)]
struct CohortBlock {
    band: u8,
    pane_ordinal: Option<u64>,
}

fn rank_cohort_blocks<'a>(
    facts: impl Iterator<Item = RankFacts<'a>>,
) -> BTreeMap<String, CohortBlock> {
    let mut blocks = BTreeMap::new();
    for facts in facts {
        let Some(cohort) = facts.cohort else {
            continue;
        };
        let block = blocks.entry(cohort.to_owned()).or_insert(CohortBlock {
            band: u8::MAX,
            pane_ordinal: None,
        });
        block.band = block.band.min(facts_band(&facts));
        if let Some(ordinal) = facts.pane_ordinal {
            block.pane_ordinal = Some(block.pane_ordinal.map_or(ordinal, |min| min.min(ordinal)));
        }
    }
    for block in blocks.values_mut() {
        // An unread member lifts an inactive launch block back into live work,
        // but never into the top unread inbox band; the block itself is still a
        // calm launch cohort.
        block.band = block.band.max(1);
    }
    blocks
}

fn rank_key(facts: RankFacts<'_>, cohorts: &BTreeMap<String, CohortBlock>) -> RankKey {
    let block = if let Some((cohort, cohort_block)) = facts
        .cohort
        .and_then(|cohort| cohorts.get(cohort).map(|block| (cohort, block)))
    {
        BlockKey {
            is_process: false,
            band: cohort_block.band,
            rank: 3,
            tiebreak: BucketTiebreak::Calm(StartKey::from(cohort_block.pane_ordinal)),
            identity: cohort.to_owned(),
            cohort_absent: false,
        }
    } else {
        // Agent cards lead the channel; process rows are the command tail beneath
        // them, whatever either's activity. Within each side the three layers hold:
        // the unread inbox first, live work over dormant, then the most
        // attention-hungry within each band. The final singleton tiebreak is the
        // stable `id` alone — never `name`, which mutates through the
        // session-name -> task -> prompt label ladder and would reshuffle a bucket
        // on every rename.
        BlockKey {
            is_process: facts.is_process,
            band: facts_band(&facts),
            rank: facts_rank(&facts),
            tiebreak: bucket_tiebreak(&facts),
            identity: facts.id.to_owned(),
            cohort_absent: true,
        }
    };
    RankKey {
        block,
        within: WithinKey {
            launch_ordinal: StartKey::from(facts.launch_ordinal),
            pane_ordinal: StartKey::from(facts.pane_ordinal),
            id: facts.id.to_owned(),
        },
    }
}

/// Tiebreak one item inside its band and rank. Attention rows
/// (`waiting`/`failed`/`paused`) sort longest-overdue-first: a blocked or failed
/// agent's `last_activity` is frozen, so this is both stable and the triage order
/// the `space` "next attention" key promises. Calm rows (`success`, `running`,
/// `idle`) and bare process rows hold pane creation order — untouched by the
/// activity heartbeat — so a working agent never jumps just because it finished a
/// tool, and a fresh pane appends at the bottom of its bucket.
fn bucket_tiebreak(facts: &RankFacts<'_>) -> BucketTiebreak {
    if is_attention(facts.status) {
        BucketTiebreak::Attention(facts.last_activity)
    } else {
        BucketTiebreak::Calm(StartKey::from(facts.pane_ordinal))
    }
}

fn facts_rank(facts: &RankFacts<'_>) -> u8 {
    match facts.status {
        Some(status) => status_rank(status),
        None => 7,
    }
}

fn facts_band(facts: &RankFacts<'_>) -> u8 {
    band(facts.unread, facts.inactive)
}

fn is_attention(status: Option<AgentStatus>) -> bool {
    status.is_some_and(AgentStatus::is_attention)
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
    // The `external` catch-all always tails: out-of-project residue never
    // displaces project work, so it sorts below every project group — below even
    // the inactive groups, and regardless of any `waiting`/`failed` member it
    // holds. Among project groups the same three layers apply, read off the
    // most-urgent member: the unread inbox first, then active over inactive, then
    // the attention bucket (every calm status collapsed to one rank so a group
    // never leapfrogs a sibling because its top row flipped success↔running↔idle),
    // then the earliest member's pane-creation order, then label.
    group_sort_key(left.rows.iter().map(row_rank_facts), left.kind, &left.label).cmp(
        &group_sort_key(
            right.rows.iter().map(row_rank_facts),
            right.kind,
            &right.label,
        ),
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct GroupRankKey {
    external: bool,
    band: u8,
    rank: u8,
    earliest_ordinal: StartKey<u64>,
    label: String,
}

fn group_sort_key<'a>(
    facts: impl Iterator<Item = RankFacts<'a>>,
    kind: SidebarWorktreeKind,
    label: &str,
) -> GroupRankKey {
    // The group's band (layers 1 and 2) is read from its liveliest member. Row
    // order seats process rows below agent cards, so group liveness is computed
    // across all rows instead of borrowed from `rows.first()`; an empty group
    // sinks last.
    let mut band = u8::MAX;
    let mut rank = u8::MAX;
    let mut earliest_ordinal: Option<u64> = None;
    for facts in facts {
        let facts_band = facts_band(&facts);
        let facts_rank = group_member_rank(&facts);
        if facts_band < band {
            band = facts_band;
            rank = facts_rank;
        } else if facts_band == band {
            rank = rank.min(facts_rank);
        }
        if let Some(ordinal) = facts.pane_ordinal {
            earliest_ordinal = Some(earliest_ordinal.map_or(ordinal, |min| min.min(ordinal)));
        }
    }
    GroupRankKey {
        external: kind == SidebarWorktreeKind::External,
        band,
        rank,
        // The group's earliest member pane ordinal uses the same creation-order
        // key as the calm tiebreak, so group order tracks the mux's pane layout
        // even when no agent reports a process start (Zellij) instead of
        // degrading to the label.
        earliest_ordinal: StartKey::from(earliest_ordinal),
        label: label.to_owned(),
    }
}

fn group_member_rank(facts: &RankFacts<'_>) -> u8 {
    match facts.status {
        Some(status) if status.is_attention() => status_rank(status),
        Some(_) => 3,
        None => 4,
    }
}

/// The macro band folding layers 1 and 2: the unread inbox first, then live
/// work, then dormant work past the inactive window. Status no longer sets the
/// band — `status_rank` orders within one — so a fresh `idle` agent outranks a
/// stale `waiting` one, and only `unread` or crossing the inactive window moves a
/// row between bands. The `external` partition is group-only ([`compare_groups`]).
fn row_band(row: &SidebarRow) -> u8 {
    band(row.unread, row.inactive)
}

fn band(unread: bool, inactive: bool) -> u8 {
    if unread {
        0
    } else if inactive {
        2
    } else {
        1
    }
}

fn status_rank(status: AgentStatus) -> u8 {
    // Actionable attention (`waiting`/`failed`) leads; `paused` sits just
    // under it — attention-class, but parked with nothing to do but wait, so it
    // ranks below a real failure and above calm. Among the calm states `idle`
    // ranks *last*: a fresh agent registers idle, so idle-at-the-bottom makes a
    // new card append at the bottom of the calm region every time — it never
    // lands above finished or working agents only to drop on its first prompt.
    // Finished work (`success`) reads first — it has a result for you — then
    // live work, then the parked idle tail the per-worktree cap trims first.
    match status {
        AgentStatus::Waiting => 0,
        AgentStatus::Failed => 1,
        AgentStatus::Paused => 2,
        AgentStatus::Success => 3,
        AgentStatus::Running => 4,
        AgentStatus::Idle => 5,
    }
}

/// One worktree's live agents, ranked within the group like the sidebar.
#[derive(Debug)]
pub struct AgentWorktreeGroup<'a> {
    pub label: String,
    pub kind: SidebarWorktreeKind,
    pub agents: Vec<&'a AgentState>,
}

/// Group root agents by worktree and rank them the way the sidebar ranks rows
/// and groups: attention agents first (longest-overdue), calm agents in stable
/// spawn order, the `external` catch-all last. The `rimz agents list` roster
/// reuses this so the CLI and the room agree on order. Uncapped — the listing
/// shows the whole roster, including paneless sessions, unlike the sidebar's
/// per-worktree cap.
pub fn group_live_agents_by_worktree<'a>(
    agents: &[&'a AgentState],
    snapshot: &SidebarSnapshot,
) -> Vec<AgentWorktreeGroup<'a>> {
    let project_root = snapshot.project_root.as_deref();
    // Split a path that hosts more than one branch into per-branch pods, exactly
    // as the sidebar's row fold does, so two checkouts of one path never collapse
    // under a single mislabeled header.
    let multi_branch = multi_branch_paths(agents.iter().map(|agent| {
        (
            agent.worktree_path.as_deref(),
            agent.worktree_branch.as_deref(),
        )
    }));
    let effective_roots = effective_worktree_roots(
        &snapshot.worktree_roots,
        agents.iter().map(|agent| {
            (
                agent.worktree_path.as_deref(),
                agent.worktree_branch.as_deref(),
            )
        }),
    );
    let mut by_key: BTreeMap<String, AgentWorktreeGroup<'a>> = BTreeMap::new();
    for &agent in agents {
        let split_by_branch = agent
            .worktree_path
            .as_deref()
            .is_some_and(|path| multi_branch.contains(path));
        let (kind, key, label) = worktree_group_key(
            agent.channel.as_deref(),
            agent.worktree_path.as_deref(),
            agent.worktree_branch.as_deref(),
            split_by_branch,
            project_root,
            &effective_roots,
            snapshot.root_class,
        );
        by_key
            .entry(key)
            .or_insert_with(|| AgentWorktreeGroup {
                label,
                kind,
                agents: Vec::new(),
            })
            .agents
            .push(agent);
    }
    let mut groups: Vec<AgentWorktreeGroup<'a>> = by_key.into_values().collect();
    for group in &mut groups {
        sort_listing_agents(&mut group.agents);
    }
    groups.sort_by_cached_key(|group| {
        group_sort_key(
            group.agents.iter().copied().map(agent_rank_facts),
            group.kind,
            &group.label,
        )
    });
    groups
}

/// The agent's pane creation ordinal — the calm tiebreak the `rimz agents list`
/// roster shares with the sidebar.
fn agent_ordinal(agent: &AgentState) -> Option<u64> {
    agent
        .pane
        .as_ref()
        .and_then(|pane| pane.pane_id.creation_ordinal())
}

fn sort_listing_agents(agents: &mut [&AgentState]) {
    let cohorts = rank_cohort_blocks(agents.iter().copied().map(agent_rank_facts));
    agents.sort_by_cached_key(|agent| rank_key(agent_rank_facts(agent), &cohorts));
}

fn agent_launch_cohort(agent: &AgentState) -> Option<&str> {
    agent.team.as_deref().or(agent.launch_group.as_deref())
}
