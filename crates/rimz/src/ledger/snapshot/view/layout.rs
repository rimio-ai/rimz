use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::feed::{AgentState, AgentStatus, PaneRef};
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

pub(super) fn worktree_group_key(
    path: Option<&str>,
    branch: Option<&str>,
    split_by_branch: bool,
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
    root_class: RootClass,
) -> (SidebarWorktreeKind, String, String) {
    let branch = branch.filter(|branch| !branch.is_empty());
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        // A cwd belongs to the *deepest* group root that contains it: the room
        // root or any enumerated group root — a repo room's worktree checkouts
        // (`git worktree list`, including one parked outside `project_root`)
        // or a directory room's child repos. Keying on the matched root is
        // what folds every pane of one checkout into one pod. Two cases keep
        // per-path pods: a repo room's own checkout (so a nested worktree the
        // enumeration hasn't caught up with never folds into the main pod),
        // and a snapshot with no known root and no enumerated roots. A cwd
        // outside every root (a home shell, `/tmp`, CI) falls through to the
        // `external` catch-all.
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
            // it is an unambiguous separator. `enrich_worktree_groups` recovers
            // the bare path from the key's first line, so the split never
            // breaks git reads.
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
    }
    if let Some(branch) = branch {
        return (
            SidebarWorktreeKind::Worktree,
            format!("branch:{branch}"),
            branch.to_owned(),
        );
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
/// components so `/home/marvinX` is not treated as under `/home/marvin`. This
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
    group.rows.sort_by(compare_rows);
    group.status_counts = status_counts(&group.rows);
    let total = group.rows.len().saturating_add(group.hidden_count);
    let rows = std::mem::take(&mut group.rows);
    group.rows = capped_rows(rows);
    group.hidden_count = total.saturating_sub(group.rows.len());
}

pub(super) fn sort_groups_for_presentation(groups: &mut [SidebarWorktreeGroup]) {
    for group in groups.iter_mut() {
        group.rows.sort_by(compare_rows);
    }
    groups.sort_by(compare_groups);
}

/// Trim a group's idle/process tail to `WORKTREE_ROW_CAP`, always keeping the
/// agent rows whose current or next state needs renderer-side unread tracking
/// plus the focused pane. Inactive success rows still stay visible so a
/// renderer never drops an unread stamp before receipts converge; inactive idle
/// rows rank behind process rows, so they are the first calm rows hidden behind
/// `+K more`.
pub(super) fn capped_rows(rows: Vec<SidebarRow>) -> Vec<SidebarRow> {
    let mut visible = Vec::new();
    for row in rows {
        if row
            .status()
            .is_some_and(|status| status != AgentStatus::Idle)
            || row.pane.as_ref().is_some_and(|pane| pane.is_focused)
            || visible.len() < WORKTREE_ROW_CAP
        {
            visible.push(row);
        }
    }
    visible
}

pub(super) fn compare_rows(left: &SidebarRow, right: &SidebarRow) -> Ordering {
    // Three layers: the unread inbox first, live work over dormant, then the
    // most attention-hungry within each band. The final tiebreak is the stable
    // `id` alone — never `name`, which mutates through the session-name → task
    // → prompt label ladder and would reshuffle a bucket on every rename.
    row_band(left)
        .cmp(&row_band(right))
        .then_with(|| row_rank(left).cmp(&row_rank(right)))
        .then_with(|| within_bucket(left, right))
        .then_with(|| left.id.cmp(&right.id))
}

/// Tiebreak two rows that share a band and rank (their attention rank already
/// tied). Attention rows (`waiting`/`failed`/`paused`) sort longest-overdue-first:
/// a blocked or failed agent's `last_activity` is frozen, so this is both stable
/// and the triage order the `␣` "next attention" key promises. Calm rows
/// (`success`, `running`, `idle`) and bare process rows hold pane creation order
/// ([`pane_creation_ordinal`]) — untouched by the activity heartbeat — so a
/// working agent never jumps just because it finished a tool, and a fresh pane
/// appends at the bottom of its bucket.
fn within_bucket(left: &SidebarRow, right: &SidebarRow) -> Ordering {
    if is_attention(left.status()) {
        left.last_activity.cmp(&right.last_activity)
    } else {
        cmp_start_asc(row_ordinal(left), row_ordinal(right))
    }
}

/// The pane's creation ordinal: the monotonic integer the mux assigns each pane
/// (`zellij:terminal_176` → 176, `tmux:%3` → 3), ascending in birth order. It is
/// the calm tiebreak — one signal both agents in a tab share, and the order the
/// mux itself lays panes out, so the sidebar tracks the pane order until the
/// panes are reordered. It replaces the former `pane_process_start`/`registered_at`
/// spawn key, which read a different clock for each agent (a derived process
/// start for one, hook registration for the other) and inverted co-launched
/// panes whenever the two sources disagreed.
fn pane_creation_ordinal(pane: Option<&PaneRef>) -> Option<u64> {
    let raw = pane?.pane_id.raw();
    let digits_start = raw
        .as_bytes()
        .iter()
        .rposition(|byte| !byte.is_ascii_digit())
        .map_or(0, |last_non_digit| last_non_digit + 1);
    raw.get(digits_start..)
        .filter(|tail| !tail.is_empty())
        .and_then(|tail| tail.parse::<u64>().ok())
}

fn row_ordinal(row: &SidebarRow) -> Option<u64> {
    pane_creation_ordinal(row.pane.as_ref())
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
    group_is_external(left)
        .cmp(&group_is_external(right))
        .then_with(|| group_band(left).cmp(&group_band(right)))
        .then_with(|| group_rank(left).cmp(&group_rank(right)))
        .then_with(|| cmp_start_asc(group_earliest_ordinal(left), group_earliest_ordinal(right)))
        .then_with(|| left.label.cmp(&right.label))
}

/// The group's band (layers 1 and 2), read off its most-urgent member. `rows` is
/// already sorted by `compare_rows` and the cap never hides the top row, so
/// `rows.first()` is the true top; an empty group sinks last.
fn group_band(group: &SidebarWorktreeGroup) -> u8 {
    match group.rows.first() {
        Some(row) => row_band(row),
        None => u8::MAX,
    }
}

/// The group's rank within its band: an attention top row leads by its bucket
/// (`waiting`/`failed`/`paused`); every calm status collapses to one rank so a
/// calm group holds its place through its members' success↔running↔idle churn; a
/// process-only group ranks just under calm agent groups; an empty group sinks
/// last.
fn group_rank(group: &SidebarWorktreeGroup) -> u8 {
    let Some(row) = group.rows.first() else {
        return u8::MAX;
    };
    match row.status() {
        Some(status) if status.is_attention() => status_rank(status),
        Some(_) => 3,
        None => 4,
    }
}

fn group_is_external(group: &SidebarWorktreeGroup) -> bool {
    group.kind == SidebarWorktreeKind::External
}

/// The group's earliest member pane ordinal — the same creation-order key the
/// within-bucket calm tiebreak uses, so group order tracks the mux's pane layout
/// even when no agent reports a process start (Zellij) instead of degrading to
/// the label.
fn group_earliest_ordinal(group: &SidebarWorktreeGroup) -> Option<u64> {
    group.rows.iter().filter_map(row_ordinal).min()
}

fn row_rank(row: &SidebarRow) -> u8 {
    match row.status() {
        Some(status) => status_rank(status),
        None => 7,
    }
}

/// The macro band folding layers 1 and 2: the unread inbox first, then live
/// work, then dormant work past the inactive window. Status no longer sets the
/// band — `row_rank` orders within one — so a fresh `idle` agent outranks a stale
/// `waiting` one, and only `unread` or crossing the inactive window moves a row
/// between bands. The `external` partition is group-only ([`compare_groups`]).
fn row_band(row: &SidebarRow) -> u8 {
    if row.unread {
        0
    } else if row.inactive {
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
    let mut by_key: BTreeMap<String, AgentWorktreeGroup<'a>> = BTreeMap::new();
    for &agent in agents {
        let split_by_branch = agent
            .worktree_path
            .as_deref()
            .is_some_and(|path| multi_branch.contains(path));
        let (kind, key, label) = worktree_group_key(
            agent.worktree_path.as_deref(),
            agent.worktree_branch.as_deref(),
            split_by_branch,
            project_root,
            &snapshot.worktree_roots,
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
        group.agents.sort_by(|a, b| compare_listing_agents(a, b));
    }
    groups.sort_by(compare_listing_groups);
    groups
}

/// The agent's pane creation ordinal — the calm tiebreak the `rimz agents list`
/// roster shares with the sidebar ([`pane_creation_ordinal`]).
fn agent_ordinal(agent: &AgentState) -> Option<u64> {
    pane_creation_ordinal(agent.pane.as_ref())
}

/// [`compare_rows`] for a bare roster with no unread/inactive state: the status
/// ladder orders the bucket, attention agents tiebreak longest-overdue-first,
/// calm agents by pane creation order, then the durable `agent_id`.
fn compare_listing_agents(a: &AgentState, b: &AgentState) -> Ordering {
    status_rank(a.status)
        .cmp(&status_rank(b.status))
        .then_with(|| {
            if a.status.is_attention() {
                a.last_activity.cmp(&b.last_activity)
            } else {
                cmp_start_asc(agent_ordinal(a), agent_ordinal(b))
            }
        })
        .then_with(|| a.agent_id.as_str().cmp(b.agent_id.as_str()))
}

/// [`compare_groups`] for the roster: the `external` catch-all last, then the
/// most-urgent member, then the earliest member's pane creation order, then the
/// label.
fn compare_listing_groups(a: &AgentWorktreeGroup, b: &AgentWorktreeGroup) -> Ordering {
    let tier = |group: &AgentWorktreeGroup| match group.agents.first() {
        Some(top) if top.status.is_attention() => (1_u8, status_rank(top.status)),
        Some(_) => (2, 0),
        None => (u8::MAX, u8::MAX),
    };
    let earliest =
        |group: &AgentWorktreeGroup| group.agents.iter().filter_map(|a| agent_ordinal(a)).min();

    (a.kind == SidebarWorktreeKind::External)
        .cmp(&(b.kind == SidebarWorktreeKind::External))
        .then_with(|| tier(a).cmp(&tier(b)))
        .then_with(|| cmp_start_asc(earliest(a), earliest(b)))
        .then_with(|| a.label.cmp(&b.label))
}
