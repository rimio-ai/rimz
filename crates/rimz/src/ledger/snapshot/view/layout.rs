use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::feed::{AgentState, AgentStatus};
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
    // Unread is the leading inbox band: an unread result deserves one human
    // look before read attention resumes. The final tiebreak is the stable
    // `id` alone — never `name`, which mutates through the session-name → task
    // → prompt label ladder and would reshuffle a bucket on every rename.
    row_tier(left)
        .cmp(&row_tier(right))
        .then_with(|| row_rank(left).cmp(&row_rank(right)))
        .then_with(|| within_bucket(left, right))
        .then_with(|| left.id.cmp(&right.id))
}

/// Tiebreak two rows that share a status bucket (their ranks already tied).
///
/// Attention rows (`waiting`/`failed`/`paused`) sort longest-overdue-first:
/// a blocked or failed agent's `last_activity` is frozen, so this is both stable
/// and the triage order the `␣` "next attention" key promises. Calm rows
/// (`success`, `running`, `idle`) and bare process rows hold a stable spawn
/// order keyed on [`spawn_key`] — set-once and untouched by the activity
/// heartbeat — so a working agent never jumps just because it finished a tool,
/// and new agents append at the bottom of their bucket.
fn within_bucket(left: &SidebarRow, right: &SidebarRow) -> Ordering {
    if is_attention(left.status()) {
        left.last_activity.cmp(&right.last_activity)
    } else {
        cmp_start_asc(spawn_key(left), spawn_key(right))
    }
}

/// The row's durable spawn instant: the pane's process start when the backend
/// reports it (tmux always, Zellij only via the `/proc` agent-pane derivation),
/// else the session's `registered_at`. Both are set-once and immune to the
/// activity heartbeat, so the calm order is stable across refreshes and a
/// renamed session never reorders.
fn spawn_key(row: &SidebarRow) -> Option<Timestamp> {
    pane_start(row).or_else(|| row.as_agent().and_then(|agent| agent.registered_at))
}

fn is_attention(status: Option<AgentStatus>) -> bool {
    status.is_some_and(AgentStatus::is_attention)
}

fn pane_start(row: &SidebarRow) -> Option<Timestamp> {
    row.pane.as_ref().and_then(|pane| pane.pane_process_start)
}

/// Ascending by start time, but a missing start sorts *last* — the opposite of
/// `Option::cmp`, which would float paneless rows (script asks, detached
/// sessions) to the top of their bucket.
pub(super) fn cmp_start_asc(left: Option<Timestamp>, right: Option<Timestamp>) -> Ordering {
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
    // holds. Among project worktrees the macro tiers then order by most-urgent
    // member (a `waiting`-topped group above a `failed`-topped one, above the
    // calm groups), then a stable order keyed on the earliest-spawned member,
    // then label.
    group_is_external(left)
        .cmp(&group_is_external(right))
        .then_with(|| group_tier(left).cmp(&group_tier(right)))
        .then_with(|| cmp_start_asc(group_earliest_spawn(left), group_earliest_spawn(right)))
        .then_with(|| left.label.cmp(&right.label))
}

/// The most-urgent member's *group* tier. `rows` is already sorted by
/// `compare_rows` and the cap never hides attention rows, so `rows.first()` is
/// the true top; an empty group ranks last. Unlike `row_rank`, every fresh calm
/// status collapses to one tier: a calm group's position must not leapfrog a
/// sibling just because its top row flipped success↔running↔idle — calm groups
/// hold the stable earliest-pane order, and only unread or attention reorders.
fn group_tier(group: &SidebarWorktreeGroup) -> (u8, u8) {
    let Some(row) = group.rows.first() else {
        return (u8::MAX, u8::MAX);
    };
    if row.unread {
        return (0, row_rank(row));
    }
    if is_attention(row.status()) {
        return (1, row_rank(row));
    }
    if row.inactive {
        return (4, row_rank(row));
    }
    match row.status() {
        Some(_) => (2, 0),
        None => (3, 0),
    }
}

fn group_is_external(group: &SidebarWorktreeGroup) -> bool {
    group.kind == SidebarWorktreeKind::External
}

/// The group's earliest member [`spawn_key`] — the same durable key the
/// within-bucket calm tiebreak uses, so group order survives a backend that
/// reports no pane starts (Zellij) instead of degrading to the label.
fn group_earliest_spawn(group: &SidebarWorktreeGroup) -> Option<Timestamp> {
    group.rows.iter().filter_map(spawn_key).min()
}

fn row_rank(row: &SidebarRow) -> u8 {
    match row.status() {
        Some(status) => status_rank(status),
        None => 7,
    }
}

/// Primary row ladder: unread inbox rows first, then read attention
/// (`waiting`/`failed`/`paused`), fresh `success`, `running`, fresh `idle`, bare
/// process rows, and finally inactive calm rows. `row_rank` only orders within a
/// tier.
fn row_tier(row: &SidebarRow) -> u8 {
    if row.unread {
        return 0;
    }
    match row.status() {
        Some(AgentStatus::Waiting | AgentStatus::Failed | AgentStatus::Paused) => 1,
        Some(AgentStatus::Success) => {
            if row.inactive {
                6
            } else {
                2
            }
        }
        Some(AgentStatus::Running) => 3,
        Some(AgentStatus::Idle) => {
            if row.inactive {
                6
            } else {
                4
            }
        }
        None => 5,
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
    let multi_branch = multi_branch_paths(
        agents
            .iter()
            .map(|agent| (agent.worktree_path.as_deref(), agent.worktree_branch.as_deref())),
    );
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

/// The agent's durable spawn instant — its pane's process start when known,
/// else `registered_at` — the key the calm within-bucket order uses (see
/// [`spawn_key`] for the row-side equivalent).
fn agent_spawn_key(agent: &AgentState) -> Option<Timestamp> {
    agent
        .pane
        .as_ref()
        .and_then(|pane| pane.pane_process_start)
        .or(agent.registered_at)
}

/// [`compare_rows`] for a bare roster with no unread/inactive state: the status
/// ladder orders the bucket, attention agents tiebreak longest-overdue-first,
/// calm agents by stable spawn order, then the durable `agent_id`.
fn compare_listing_agents(a: &AgentState, b: &AgentState) -> Ordering {
    status_rank(a.status)
        .cmp(&status_rank(b.status))
        .then_with(|| {
            if a.status.is_attention() {
                a.last_activity.cmp(&b.last_activity)
            } else {
                cmp_start_asc(agent_spawn_key(a), agent_spawn_key(b))
            }
        })
        .then_with(|| a.agent_id.as_str().cmp(b.agent_id.as_str()))
}

/// [`compare_groups`] for the roster: the `external` catch-all last, then the
/// most-urgent member, then the earliest-spawned member, then the label.
fn compare_listing_groups(a: &AgentWorktreeGroup, b: &AgentWorktreeGroup) -> Ordering {
    let tier = |group: &AgentWorktreeGroup| match group.agents.first() {
        Some(top) if top.status.is_attention() => (1_u8, status_rank(top.status)),
        Some(_) => (2, 0),
        None => (u8::MAX, u8::MAX),
    };
    let earliest =
        |group: &AgentWorktreeGroup| group.agents.iter().filter_map(|a| agent_spawn_key(a)).min();

    (a.kind == SidebarWorktreeKind::External)
        .cmp(&(b.kind == SidebarWorktreeKind::External))
        .then_with(|| tier(a).cmp(&tier(b)))
        .then_with(|| cmp_start_asc(earliest(a), earliest(b)))
        .then_with(|| a.label.cmp(&b.label))
}
