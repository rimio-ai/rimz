//! Sidebar body membership and stable row ordinals.

use std::collections::{BTreeSet, HashSet};
use std::ops::Range;

use crate::agents::AgentStatus;
use crate::ids::PaneId;
use crate::{SidebarRow, SidebarSnapshot, SidebarWorktreeGroup};

/// Transient cockpit lens applied only to body membership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BodyFilter {
    Status(AgentStatus),
    Unread,
}

/// Maximum calm rows painted before overflow moves behind `+K more`.
pub const WORKTREE_ROW_CAP: usize = 6;

/// One projected group, indexed into its roster's flat row slice.
#[derive(Clone, Debug)]
pub(crate) struct VisibleGroup<'a> {
    source: &'a SidebarWorktreeGroup,
    range: Range<usize>,
    expanded: bool,
    natural_hidden_count: usize,
    hidden_count: usize,
}

impl<'a> VisibleGroup<'a> {
    pub(crate) fn source(&self) -> &'a SidebarWorktreeGroup {
        self.source
    }

    pub(crate) fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    pub(crate) fn rows<'r>(&self, roster: &'r VisibleRoster<'a>) -> &'r [&'a SidebarRow] {
        &roster.rows[self.range.clone()]
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.range.is_empty()
    }

    pub(crate) fn expanded(&self) -> bool {
        self.expanded
    }

    pub(crate) fn natural_hidden_count(&self) -> usize {
        self.natural_hidden_count
    }

    pub(crate) fn hidden_count(&self) -> usize {
        self.hidden_count
    }
}

/// One body projection shared by render, browse, selection, and order holds.
pub(crate) struct VisibleRoster<'a> {
    rows: Vec<&'a SidebarRow>,
    groups: Vec<VisibleGroup<'a>>,
}

impl<'a> VisibleRoster<'a> {
    pub(crate) fn new(
        snapshot: &'a SidebarSnapshot,
        filter: Option<BodyFilter>,
        expanded_groups: &BTreeSet<String>,
        held: Option<&HashSet<String>>,
    ) -> Self {
        let mut rows = Vec::new();
        let mut groups = Vec::with_capacity(snapshot.worktree_groups.len());
        for group in &snapshot.worktree_groups {
            let expanded = expanded_groups.contains(&group.key);
            let start = rows.len();
            let projection = project_group(group, filter, expanded, held);
            let hidden_count = if filter.is_none() {
                group.rows.len().saturating_sub(projection.rows.len())
            } else {
                0
            };
            rows.extend(projection.rows);
            groups.push(VisibleGroup {
                source: group,
                range: start..rows.len(),
                expanded,
                natural_hidden_count: if filter.is_none() {
                    projection.natural_hidden_count
                } else {
                    0
                },
                hidden_count,
            });
        }
        Self { rows, groups }
    }

    pub(crate) fn baseline(snapshot: &'a SidebarSnapshot) -> Self {
        Self::new(snapshot, None, &BTreeSet::new(), None)
    }

    #[cfg(test)]
    pub(crate) fn single(
        group: &'a SidebarWorktreeGroup,
        filter: Option<BodyFilter>,
        expanded: bool,
        held: Option<&HashSet<String>>,
    ) -> Self {
        let projection = project_group(group, filter, expanded, held);
        let hidden_count = if filter.is_none() {
            group.rows.len().saturating_sub(projection.rows.len())
        } else {
            0
        };
        let len = projection.rows.len();
        Self {
            rows: projection.rows,
            groups: vec![VisibleGroup {
                source: group,
                range: 0..len,
                expanded,
                natural_hidden_count: if filter.is_none() {
                    projection.natural_hidden_count
                } else {
                    0
                },
                hidden_count,
            }],
        }
    }

    pub(crate) fn rows(&self) -> &[&'a SidebarRow] {
        &self.rows
    }

    pub(crate) fn row(&self, ordinal: usize) -> Option<&'a SidebarRow> {
        self.rows.get(ordinal).copied()
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn groups(&self) -> &[VisibleGroup<'a>] {
        &self.groups
    }

    pub(crate) fn ordinal_of_pane(&self, pane_id: &PaneId) -> Option<usize> {
        self.rows.iter().position(|row| {
            row.pane
                .as_ref()
                .is_some_and(|pane| pane.pane_id == *pane_id)
        })
    }

    pub(crate) fn ordinal_of_id(&self, id: &str) -> Option<usize> {
        self.rows.iter().position(|row| row.id == id)
    }

    pub(crate) fn pane_at_ordinal(&self, ordinal: usize) -> Option<PaneId> {
        self.row(ordinal)
            .and_then(|row| row.pane.as_ref())
            .map(|pane| pane.pane_id.clone())
    }

    pub(crate) fn group_containing(&self, ordinal: usize) -> Option<&VisibleGroup<'a>> {
        self.groups
            .iter()
            .find(|group| group.range.contains(&ordinal))
    }

    pub(crate) fn neighboring_group_head(&self, ordinal: usize, step: isize) -> Option<usize> {
        let visible = self
            .groups
            .iter()
            .filter(|group| !group.is_empty())
            .collect::<Vec<_>>();
        let current = visible
            .iter()
            .position(|group| group.range.contains(&ordinal))?;
        let target = if step < 0 {
            current.checked_sub(1)?
        } else {
            (current + 1 < visible.len()).then_some(current + 1)?
        };
        Some(visible[target].range.start)
    }
}

struct GroupProjection<'a> {
    rows: Vec<&'a SidebarRow>,
    natural_hidden_count: usize,
}

fn project_group<'a>(
    group: &'a SidebarWorktreeGroup,
    filter: Option<BodyFilter>,
    expanded: bool,
    held: Option<&HashSet<String>>,
) -> GroupProjection<'a> {
    project_rows(&group.rows, group.finished, filter, expanded, held)
}

fn project_rows<'a>(
    source: &'a [SidebarRow],
    finished: bool,
    filter: Option<BodyFilter>,
    expanded: bool,
    held: Option<&HashSet<String>>,
) -> GroupProjection<'a> {
    let process_is_only_live_member = process_is_only_live_member(source);
    let liveness_process_id = process_is_only_live_member
        .then(|| {
            source
                .iter()
                .find(|row| row.is_process() && row_band(row) == 0)
                .map(|row| row.id.as_str())
        })
        .flatten();
    // Keep a finished roster whole while focus or the order hold anchors any
    // member. Once both clear, every row collapses into the receipt together.
    let revealed = finished
        && source.iter().any(|row| {
            row.pane.as_ref().is_some_and(|pane| pane.is_focused)
                || held.is_some_and(|ids| ids.contains(&row.id))
        });
    let mut rows = Vec::new();
    let mut natural_visible = 0;
    let mut actual_visible = 0;
    for row in source {
        let essential = row.unread
            || row
                .status()
                .is_some_and(|status| status != AgentStatus::Idle)
            || row.pane.as_ref().is_some_and(|pane| pane.is_focused)
            || liveness_process_id == Some(row.id.as_str());
        let natural = if finished {
            revealed
        } else {
            essential || natural_visible < WORKTREE_ROW_CAP
        };
        natural_visible += usize::from(natural);

        let visible = match filter {
            Some(filter) => row_passes_filter(row, Some(filter)),
            None if expanded => true,
            None if finished => revealed,
            None => {
                essential
                    || held.is_some_and(|ids| ids.contains(&row.id))
                    || actual_visible < WORKTREE_ROW_CAP
            }
        };
        if visible {
            rows.push(row);
            actual_visible += 1;
        }
    }
    GroupProjection {
        rows,
        natural_hidden_count: source.len().saturating_sub(natural_visible),
    }
}

fn process_is_only_live_member(rows: &[SidebarRow]) -> bool {
    rows.iter().map(row_band).min() == Some(0)
        && rows
            .iter()
            .filter(|row| row_band(row) == 0)
            .all(SidebarRow::is_process)
}

/// One body filter predicate shared by projection and cockpit behavior.
pub(crate) fn row_passes_filter(row: &SidebarRow, filter: Option<BodyFilter>) -> bool {
    match filter {
        None => true,
        Some(BodyFilter::Status(status)) => row.status() == Some(status),
        Some(BodyFilter::Unread) => row.unread,
    }
}

/// Rows surviving the calm-tail cap, including held exemptions.
pub fn capped_visible_rows<'a>(
    rows: &'a [SidebarRow],
    held: Option<&HashSet<String>>,
) -> Vec<&'a SidebarRow> {
    project_rows(rows, false, None, false, held).rows
}

fn row_band(row: &SidebarRow) -> u8 {
    if row.archived {
        2
    } else if row.inactive {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;
    use crate::sidebar::test_support::pane;
    use crate::{AgentCard, RowCard, SidebarWorktreeKind};
    use jiff::Timestamp;

    #[test]
    fn roster_projects_cap_expansion_filter_hold_and_control_counts_once() {
        let group = group(idle_rows(9));
        let snapshot = snapshot(vec![group]);

        let collapsed = VisibleRoster::new(&snapshot, None, &BTreeSet::new(), None);
        assert_eq!(
            ids(&collapsed),
            ["idle-0", "idle-1", "idle-2", "idle-3", "idle-4", "idle-5"]
        );
        assert_eq!(collapsed.groups()[0].natural_hidden_count(), 3);
        assert_eq!(collapsed.groups()[0].hidden_count(), 3);

        let expanded_keys = BTreeSet::from(["group-0".to_owned()]);
        let expanded = VisibleRoster::new(&snapshot, None, &expanded_keys, None);
        assert_eq!(expanded.len(), 9);
        assert!(expanded.groups()[0].expanded());
        assert_eq!(expanded.groups()[0].natural_hidden_count(), 3);
        assert_eq!(expanded.groups()[0].hidden_count(), 0);

        let filtered = VisibleRoster::new(
            &snapshot,
            Some(BodyFilter::Status(AgentStatus::Idle)),
            &BTreeSet::new(),
            None,
        );
        assert_eq!(filtered.len(), 9, "filters expose every matching row");

        let held_ids = HashSet::from(["idle-8".to_owned()]);
        let held = VisibleRoster::new(&snapshot, None, &BTreeSet::new(), Some(&held_ids));
        assert_eq!(held.len(), 7);
        assert_eq!(held.groups()[0].natural_hidden_count(), 3);
        assert_eq!(held.groups()[0].hidden_count(), 2);
        assert!(ids(&held).contains(&"idle-8"));
    }

    #[test]
    fn roster_keeps_attention_focus_unread_and_only_live_process_beyond_cap() {
        let mut rows = idle_rows(9);
        rows[6].unread = true;
        rows[7].pane.as_mut().unwrap().is_focused = true;
        rows[8].card = RowCard::Agent(Box::new(AgentCard {
            status: AgentStatus::Failed,
            ..AgentCard::default()
        }));
        let attention_snapshot = snapshot(vec![group(rows)]);
        let roster = VisibleRoster::baseline(&attention_snapshot);
        assert!(ids(&roster).contains(&"idle-6"));
        assert!(ids(&roster).contains(&"idle-7"));
        assert!(ids(&roster).contains(&"idle-8"));

        let mut rows = idle_rows(7);
        for row in &mut rows {
            row.inactive = true;
        }
        rows.push(process_row("shell"));
        let process_snapshot = snapshot(vec![group(rows)]);
        assert!(ids(&VisibleRoster::baseline(&process_snapshot)).contains(&"shell"));
    }

    #[test]
    fn roster_finishes_groups_and_preserves_flat_group_ordinals() {
        let mut finished = group(vec![
            agent_row("done-unread", AgentStatus::Success),
            agent_row("done-focused", AgentStatus::Success),
        ]);
        finished.key = "finished".to_owned();
        finished.finished = true;
        finished.rows[0].unread = true;

        let mut active = group(vec![agent_row("active", AgentStatus::Running)]);
        active.key = "active".to_owned();
        let mut snapshot = snapshot(vec![finished, active]);
        let collapsed = VisibleRoster::baseline(&snapshot);
        assert_eq!(ids(&collapsed), ["active"]);
        assert_eq!(collapsed.groups()[0].hidden_count(), 2);
        assert_eq!(collapsed.groups()[1].range(), 0..1);

        snapshot.worktree_groups[0].rows[1]
            .pane
            .as_mut()
            .unwrap()
            .is_focused = true;
        let focused = VisibleRoster::baseline(&snapshot);
        assert_eq!(ids(&focused), ["done-unread", "done-focused", "active"]);
        assert_eq!(focused.ordinal_of_id("active"), Some(2));
        assert_eq!(focused.neighboring_group_head(0, 1), Some(2));

        let filtered = VisibleRoster::new(
            &snapshot,
            Some(BodyFilter::Status(AgentStatus::Success)),
            &BTreeSet::new(),
            None,
        );
        assert_eq!(ids(&filtered), ["done-unread", "done-focused"]);
    }

    fn snapshot(groups: Vec<SidebarWorktreeGroup>) -> SidebarSnapshot {
        let workspace = WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap();
        let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
        snapshot.worktree_groups = groups;
        snapshot
    }

    fn group(rows: Vec<SidebarRow>) -> SidebarWorktreeGroup {
        SidebarWorktreeGroup {
            key: "group-0".to_owned(),
            label: "main".to_owned(),
            kind: SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            worktree_backed: false,
            finished: false,
            clean: None,
            landed: None,
            trunk_sync: None,
            pr_state: None,
            pr_number: None,
        }
    }

    fn idle_rows(count: usize) -> Vec<SidebarRow> {
        (0..count)
            .map(|index| agent_row(&format!("idle-{index}"), AgentStatus::Idle))
            .collect()
    }

    fn agent_row(id: &str, status: AgentStatus) -> SidebarRow {
        SidebarRow {
            id: id.to_owned(),
            name: "codex".to_owned(),
            pane: Some(pane(&format!("%{id}"), "codex", "/repo/main")),
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            channel: None,
            unread: false,
            inactive: false,
            archived: false,
            attention_score: 0,
            last_activity: Timestamp::now(),
            card: RowCard::Agent(Box::new(AgentCard {
                status,
                ..AgentCard::default()
            })),
        }
    }

    fn process_row(id: &str) -> SidebarRow {
        let mut row = agent_row(id, AgentStatus::Idle);
        row.name = "zsh".to_owned();
        row.card = RowCard::Process(crate::ProcessCard::default());
        row
    }

    fn ids<'a>(roster: &'a VisibleRoster<'_>) -> Vec<&'a str> {
        roster.rows().iter().map(|row| row.id.as_str()).collect()
    }
}
