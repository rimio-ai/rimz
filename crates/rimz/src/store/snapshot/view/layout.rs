use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::agents::{AgentState, AgentStatus};
use crate::store::snapshot::row::SidebarRow;
use crate::workspace::RootClass;

use super::score::{self, GitRung, GroupCalm};
use super::{
    SidebarSnapshot, SidebarStatusCount, SidebarWorktreeGroup, SidebarWorktreeKind,
    WorktreePrState, WorktreeTrunkSync,
};

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

#[derive(Clone, Copy)]
pub(super) struct GroupRoots<'a> {
    pub project_root: Option<&'a Path>,
    pub worktree_roots: &'a [PathBuf],
    pub worktree_home: Option<&'a Path>,
    pub root_class: RootClass,
}

pub(super) fn worktree_group_key(
    explicit_channel: Option<&str>,
    path: Option<&str>,
    branch: Option<&str>,
    split_by_branch: bool,
    roots: GroupRoots<'_>,
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
        let cwd = Path::new(path);
        if roots
            .worktree_home
            .is_some_and(|home| is_within(home, cwd) && cwd != home)
            && roots.project_root != Some(cwd)
        {
            // Mirror `compose_channel`'s worktree-basename fallback so grouping
            // agrees with addressing and `rimz channel list` for unstamped
            // agents launched inside a Rimz-owned worktree.
            let label = path_basename(cwd);
            return (
                SidebarWorktreeKind::Channel,
                format!("channel:{label}"),
                label,
            );
        }
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
        let matched = roots
            .worktree_roots
            .iter()
            .map(PathBuf::as_path)
            .chain(roots.project_root)
            .filter(|root| is_within(root, cwd))
            .max_by_key(|root| root.components().count());
        let per_path = match matched {
            Some(root) => roots.project_root == Some(root) && roots.root_class == RootClass::Repo,
            None => roots.project_root.is_none() && roots.worktree_roots.is_empty(),
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
            if roots.project_root == Some(root) {
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
    group.finished = group_finished(group);
}

pub(super) fn sort_groups_for_presentation(groups: &mut [SidebarWorktreeGroup]) {
    for group in groups.iter_mut() {
        sort_rows(&mut group.rows);
        group.finished = group_finished(group);
    }
    groups.sort_by(compare_groups);
}

pub(super) fn sort_rows(rows: &mut [SidebarRow]) {
    let cohorts = rank_cohort_blocks(rows.iter().map(row_rank_facts));
    rows.sort_by_cached_key(|row| rank_key(row_rank_facts(row), &cohorts));
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
    inactive: bool,
    archived: bool,
    status: Option<AgentStatus>,
    score: u32,
    factor_milli: u32,
    last_activity: jiff::Timestamp,
    pane_ordinal: Option<u64>,
    cohort: Option<&'a str>,
    launch_ordinal: Option<u32>,
    id: &'a str,
}

fn row_rank_facts(row: &SidebarRow) -> RankFacts<'_> {
    RankFacts {
        is_process: row.is_process(),
        inactive: row.inactive,
        archived: row.archived,
        status: row.status(),
        score: row.attention_score,
        factor_milli: score::recovered_time_factor_milli(row.status(), row.attention_score),
        last_activity: row.last_activity,
        pane_ordinal: row_ordinal(row),
        cohort: row.launch_cohort(),
        launch_ordinal: row.launch_ordinal(),
        id: row.id.as_str(),
    }
}

fn agent_rank_facts(
    agent: &AgentState,
    now: jiff::Timestamp,
    inactive_after_secs: u32,
    archive_after_secs: u32,
) -> RankFacts<'_> {
    let age_secs = score::age_secs(now, agent.last_activity);
    let score = score::attention_score(
        Some(agent.status),
        age_secs,
        inactive_after_secs,
        archive_after_secs,
    );
    RankFacts {
        is_process: false,
        inactive: age_secs > inactive_after_secs,
        archived: age_secs > archive_after_secs,
        status: Some(agent.status),
        score,
        factor_milli: score::recovered_time_factor_milli(Some(agent.status), score),
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
    urgency: Reverse<u32>,
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
    urgency: u32,
    tiebreak: BucketTiebreak,
}

#[derive(Clone, Debug)]
struct CohortAccum {
    band: u8,
    pane_ordinal: Option<u64>,
    oldest_attention: Option<jiff::Timestamp>,
    best_actionable: Option<ActionableDriver>,
    oldest_paused: Option<PausedDriver>,
    most_recent_calm: Option<CalmDriver>,
    has_running: bool,
    has_success: bool,
}

#[derive(Clone, Copy, Debug)]
struct ActionableDriver {
    score: u32,
    last_activity: jiff::Timestamp,
}

#[derive(Clone, Copy, Debug)]
struct PausedDriver {
    factor_milli: u32,
    last_activity: jiff::Timestamp,
}

#[derive(Clone, Copy, Debug)]
struct CalmDriver {
    factor_milli: u32,
    last_activity: jiff::Timestamp,
}

fn rank_cohort_blocks<'a>(
    facts: impl Iterator<Item = RankFacts<'a>>,
) -> BTreeMap<String, CohortBlock> {
    let mut blocks = BTreeMap::new();
    for facts in facts {
        let Some(cohort) = facts.cohort else {
            continue;
        };
        let block = blocks.entry(cohort.to_owned()).or_insert(CohortAccum {
            band: u8::MAX,
            pane_ordinal: None,
            oldest_attention: None,
            best_actionable: None,
            oldest_paused: None,
            most_recent_calm: None,
            has_running: false,
            has_success: false,
        });
        block.band = block.band.min(facts_band(&facts));
        if let Some(ordinal) = facts.pane_ordinal {
            block.pane_ordinal = Some(block.pane_ordinal.map_or(ordinal, |min| min.min(ordinal)));
        }
        if is_attention(facts.status) {
            block.oldest_attention = Some(
                block
                    .oldest_attention
                    .map_or(facts.last_activity, |oldest| {
                        oldest.min(facts.last_activity)
                    }),
            );
        }
        match facts.status {
            Some(AgentStatus::Waiting | AgentStatus::Failed) => {
                let candidate = ActionableDriver {
                    score: facts.score,
                    last_activity: facts.last_activity,
                };
                if block.best_actionable.is_none_or(|current| {
                    candidate.score > current.score
                        || (candidate.score == current.score
                            && candidate.last_activity < current.last_activity)
                }) {
                    block.best_actionable = Some(candidate);
                }
            }
            Some(AgentStatus::Paused) => {
                let candidate = PausedDriver {
                    factor_milli: facts.factor_milli,
                    last_activity: facts.last_activity,
                };
                if block
                    .oldest_paused
                    .is_none_or(|current| candidate.last_activity < current.last_activity)
                {
                    block.oldest_paused = Some(candidate);
                }
            }
            Some(AgentStatus::Running) => {
                block.has_running = true;
                block.observe_calm(&facts);
            }
            Some(AgentStatus::Success) => {
                block.has_success = true;
                block.observe_calm(&facts);
            }
            Some(AgentStatus::Idle) | None => block.observe_calm(&facts),
        }
    }
    blocks
        .into_iter()
        .map(|(cohort, block)| (cohort, block.finish()))
        .collect()
}

impl CohortAccum {
    fn observe_calm(&mut self, facts: &RankFacts<'_>) {
        let candidate = CalmDriver {
            factor_milli: facts.factor_milli,
            last_activity: facts.last_activity,
        };
        if self
            .most_recent_calm
            .is_none_or(|current| candidate.last_activity > current.last_activity)
        {
            self.most_recent_calm = Some(candidate);
        }
    }

    fn finish(self) -> CohortBlock {
        // A launch block's band is the minimum member time band; read state
        // never lifts it.
        let band = self.band;
        if let Some(driver) = self.best_actionable {
            return CohortBlock {
                band,
                urgency: driver.score,
                tiebreak: BucketTiebreak::Attention(
                    self.oldest_attention.unwrap_or(driver.last_activity),
                ),
            };
        }
        if let Some(driver) = self.oldest_paused {
            return CohortBlock {
                band,
                urgency: score::score_from_weight_and_factor(
                    AgentStatus::Paused,
                    driver.factor_milli,
                ),
                tiebreak: BucketTiebreak::Attention(
                    self.oldest_attention.unwrap_or(driver.last_activity),
                ),
            };
        }
        let status = if self.has_running {
            AgentStatus::Running
        } else if self.has_success {
            AgentStatus::Success
        } else {
            AgentStatus::Idle
        };
        CohortBlock {
            band,
            urgency: score::score_from_weight_and_factor(
                status,
                self.most_recent_calm
                    .map_or(1_000, |driver| driver.factor_milli),
            ),
            tiebreak: BucketTiebreak::Calm(StartKey::from(self.pane_ordinal)),
        }
    }
}

fn rank_key(facts: RankFacts<'_>, cohorts: &BTreeMap<String, CohortBlock>) -> RankKey {
    let block = if let Some((cohort, cohort_block)) = facts
        .cohort
        .and_then(|cohort| cohorts.get(cohort).map(|block| (cohort, block)))
    {
        BlockKey {
            is_process: false,
            band: cohort_block.band,
            urgency: Reverse(cohort_block.urgency),
            tiebreak: cohort_block.tiebreak.clone(),
            identity: cohort.to_owned(),
            cohort_absent: false,
        }
    } else {
        // Agent cards lead the channel; process rows are the command tail beneath
        // them, whatever either's activity. Within each side the bands hold:
        // hot work, warm work, then archive; the fixed-point urgency score
        // orders within a band. The final singleton tiebreak is the stable `id`
        // alone — never `name`, which mutates through the session-name -> task
        // -> prompt label ladder and would reshuffle a bucket on every rename.
        BlockKey {
            is_process: facts.is_process,
            band: facts_band(&facts),
            urgency: Reverse(facts.score),
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

fn facts_band(facts: &RankFacts<'_>) -> u8 {
    band(facts.inactive, facts.archived)
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
    // archived groups, and regardless of any `waiting`/`failed` member it holds.
    // Among project groups the same macro bands apply, read off the liveliest
    // member: hot work, warm work, archive. Urgency decides attention inside the
    // winning band, then calm activity orders working, successful, idle, and
    // process-only groups before git, pane creation, and label settle ties.
    group_sort_key(
        left.rows.iter().map(row_rank_facts),
        left.kind,
        group_git_rung(left),
        &left.label,
    )
    .cmp(&group_sort_key(
        right.rows.iter().map(row_rank_facts),
        right.kind,
        group_git_rung(right),
        &right.label,
    ))
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct GroupRankKey {
    external: bool,
    band: u8,
    urgency: Reverse<u32>,
    calm: GroupCalm,
    git: GitRung,
    earliest_ordinal: StartKey<u64>,
    label: String,
}

fn group_sort_key<'a>(
    facts: impl Iterator<Item = RankFacts<'a>>,
    kind: SidebarWorktreeKind,
    git: GitRung,
    label: &str,
) -> GroupRankKey {
    // The group's band is read from its liveliest member. Row order seats
    // process rows below agent cards, so group liveness is computed across all
    // rows instead of borrowed from `rows.first()`; an empty group sinks last.
    let mut band = u8::MAX;
    let mut urgency = 0;
    let mut calm = GroupCalm::Processes;
    let mut earliest_ordinal: Option<u64> = None;
    let mut has_rows = false;
    let mut finish_blocked = false;
    for facts in facts {
        has_rows = true;
        finish_blocked |= member_blocks_finish(facts.status);
        let facts_band = facts_band(&facts);
        if facts_band < band {
            band = facts_band;
            urgency = group_member_urgency(&facts);
            calm = group_member_calm(facts.status);
        } else if facts_band == band {
            urgency = urgency.max(group_member_urgency(&facts));
            calm = calm.min(group_member_calm(facts.status));
        }
        if let Some(ordinal) = facts.pane_ordinal {
            earliest_ordinal = Some(earliest_ordinal.map_or(ordinal, |min| min.min(ordinal)));
        }
    }
    if has_rows && git == GitRung::Done && !finish_blocked {
        band = 2;
    }
    GroupRankKey {
        external: kind == SidebarWorktreeKind::External,
        band,
        urgency: Reverse(urgency),
        calm,
        git,
        // The group's earliest member pane ordinal uses the same creation-order
        // key as the calm tiebreak, so group order tracks the mux's pane layout
        // even when no agent reports a process start (Zellij) instead of
        // degrading to the label.
        earliest_ordinal: StartKey::from(earliest_ordinal),
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

pub(super) fn group_finished(group: &SidebarWorktreeGroup) -> bool {
    !group.rows.is_empty()
        && group_git_rung(group) == GitRung::Done
        && !group
            .rows
            .iter()
            .any(|row| member_blocks_finish(row.status()))
}

fn member_blocks_finish(status: Option<AgentStatus>) -> bool {
    status.is_some_and(|status| status == AgentStatus::Running || status.is_attention())
}

fn group_member_calm(status: Option<AgentStatus>) -> GroupCalm {
    match status {
        Some(AgentStatus::Success) => GroupCalm::Finished,
        Some(AgentStatus::Idle) => GroupCalm::Idle,
        None => GroupCalm::Processes,
        Some(_) => GroupCalm::Working,
    }
}

fn group_member_urgency(facts: &RankFacts<'_>) -> u32 {
    if is_attention(facts.status) {
        facts.score
    } else {
        0
    }
}

/// Macro bands: hot work, warm work past the inactive window, and archived work
/// past the archive window. Read state never changes the band. Status no longer
/// sets the band — score orders within one — so a fresh `idle` agent outranks a
/// warm or archived `waiting` one. The `external` partition is group-only
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
    pub label: String,
    pub kind: SidebarWorktreeKind,
    pub agents: Vec<&'a AgentState>,
}

/// Group root agents by worktree and rank them the way the sidebar ranks rows
/// and groups: attention agents first (longest-overdue), calm agents in stable
/// spawn order, the `external` catch-all last. The `rimz agents list` roster
/// reuses this so the CLI and the room agree on order.
pub fn group_live_agents_by_worktree<'a>(
    agents: &[&'a AgentState],
    snapshot: &SidebarSnapshot,
) -> Vec<AgentWorktreeGroup<'a>> {
    let project_root = snapshot.project_root.as_deref();
    let (inactive_after_secs, archive_after_secs) = rank_windows(snapshot);
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
    let roots = GroupRoots {
        project_root,
        worktree_roots: &effective_roots,
        worktree_home: snapshot.worktree_home.as_deref(),
        root_class: snapshot.root_class,
    };
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
            roots,
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
        sort_listing_agents(
            &mut group.agents,
            snapshot.now,
            inactive_after_secs,
            archive_after_secs,
        );
    }
    groups.sort_by_cached_key(|group| {
        group_sort_key(
            group.agents.iter().copied().map(|agent| {
                agent_rank_facts(agent, snapshot.now, inactive_after_secs, archive_after_secs)
            }),
            group.kind,
            GitRung::Unknown,
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

fn sort_listing_agents(
    agents: &mut [&AgentState],
    now: jiff::Timestamp,
    inactive_after_secs: u32,
    archive_after_secs: u32,
) {
    let cohorts = rank_cohort_blocks(
        agents
            .iter()
            .copied()
            .map(|agent| agent_rank_facts(agent, now, inactive_after_secs, archive_after_secs)),
    );
    agents.sort_by_cached_key(|agent| {
        rank_key(
            agent_rank_facts(agent, now, inactive_after_secs, archive_after_secs),
            &cohorts,
        )
    });
}

fn agent_launch_cohort(agent: &AgentState) -> Option<&str> {
    agent.team.as_deref().or(agent.launch_group.as_deref())
}

fn rank_windows(snapshot: &SidebarSnapshot) -> (u32, u32) {
    let inactive_after_secs = snapshot.attention.inactive_after_secs.get();
    let archive_after_secs = snapshot
        .attention
        .archive_after_secs
        .get()
        .max(inactive_after_secs.saturating_add(1));
    (inactive_after_secs, archive_after_secs)
}
