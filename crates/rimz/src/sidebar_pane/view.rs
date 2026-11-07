//! Sidebar body membership and stable row ordinals.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::Range;

use crate::agents::AgentStatus;
use crate::ids::PaneId;
use crate::sidebar::body_filter::GroupFacts;
use crate::store::snapshot::{
    SidebarRow, SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind,
};

pub(crate) use crate::sidebar::body_filter::BodyFilter;
pub(super) use crate::sidebar::body_filter::BodyLens;

/// Maximum calm rows painted before overflow moves behind `+K more`.
pub(crate) const WORKTREE_ROW_CAP: usize = 6;

pub(super) fn card_slots(snapshot: &SidebarSnapshot) -> Vec<&PaneId> {
    let mut panes: Vec<_> = snapshot
        .rows()
        .filter_map(|row| row.pane.as_ref().map(|pane| &pane.pane_id))
        .collect();
    panes.sort_unstable_by_key(|pane| {
        let ordinal = pane.creation_ordinal();
        (ordinal.is_none(), ordinal, pane.as_str())
    });
    panes
}

/// One projected group, indexed into its roster's flat row slice.
#[derive(Clone, Debug)]
pub(super) struct VisibleGroup<'a> {
    source: &'a SidebarWorktreeGroup,
    range: Range<usize>,
    expanded: bool,
    natural_hidden_count: usize,
    hidden_count: usize,
    rowless_workflow: bool,
}

impl<'a> VisibleGroup<'a> {
    pub(super) fn source(&self) -> &'a SidebarWorktreeGroup {
        self.source
    }

    pub(super) fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    pub(super) fn rows<'r>(&self, roster: &'r VisibleRoster<'a>) -> &'r [&'a SidebarRow] {
        &roster.rows[self.range.clone()]
    }

    pub(super) fn is_empty(&self) -> bool {
        self.range.is_empty()
    }

    pub(super) fn shown(&self) -> bool {
        !self.is_empty() || self.hidden_count > 0 || self.rowless_workflow
    }

    pub(super) fn expanded(&self) -> bool {
        self.expanded
    }

    pub(super) fn natural_hidden_count(&self) -> usize {
        self.natural_hidden_count
    }

    pub(super) fn hidden_count(&self) -> usize {
        self.hidden_count
    }
}

/// One body projection shared by render, browse, selection, and layout freezes.
pub(super) struct VisibleRoster<'a> {
    rows: Vec<&'a SidebarRow>,
    groups: Vec<VisibleGroup<'a>>,
}

impl<'a> VisibleRoster<'a> {
    pub(super) fn new(
        snapshot: &'a SidebarSnapshot,
        lens: &BodyLens,
        expanded_groups: &BTreeSet<String>,
        held: Option<&HashSet<String>>,
    ) -> Self {
        let mut rows = Vec::new();
        let mut groups = Vec::with_capacity(snapshot.worktree_groups.len());
        for group in snapshot.worktree_groups.iter() {
            let expanded = expanded_groups.contains(&group.key);
            let start = rows.len();
            let projection =
                project_group(group, lens, expanded, held, snapshot.focused_pane.as_ref());
            let hidden_count = if lens.is_empty() {
                group.rows.len().saturating_sub(projection.rows.len())
            } else {
                0
            };
            rows.extend(projection.rows);
            groups.push(VisibleGroup {
                source: group,
                range: start..rows.len(),
                expanded,
                natural_hidden_count: if lens.is_empty() {
                    projection.natural_hidden_count
                } else {
                    0
                },
                hidden_count,
                rowless_workflow: projection.rowless_workflow,
            });
        }
        Self { rows, groups }
    }

    pub(super) fn baseline(snapshot: &'a SidebarSnapshot) -> Self {
        Self::new(snapshot, &BodyLens::default(), &BTreeSet::new(), None)
    }

    #[cfg(test)]
    pub(crate) fn single(
        group: &'a SidebarWorktreeGroup,
        lens: &BodyLens,
        expanded: bool,
        held: Option<&HashSet<String>>,
        focused_pane: Option<&PaneId>,
    ) -> Self {
        let projection = project_group(group, lens, expanded, held, focused_pane);
        let hidden_count = if lens.is_empty() {
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
                natural_hidden_count: if lens.is_empty() {
                    projection.natural_hidden_count
                } else {
                    0
                },
                hidden_count,
                rowless_workflow: projection.rowless_workflow,
            }],
        }
    }

    pub(super) fn rows(&self) -> &[&'a SidebarRow] {
        &self.rows
    }

    pub(super) fn row(&self, ordinal: usize) -> Option<&'a SidebarRow> {
        self.rows.get(ordinal).copied()
    }

    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn groups(&self) -> &[VisibleGroup<'a>] {
        &self.groups
    }

    pub(super) fn ordinal_of_pane(&self, pane_id: &PaneId) -> Option<usize> {
        self.rows.iter().position(|row| {
            row.pane
                .as_ref()
                .is_some_and(|pane| pane.pane_id == *pane_id)
        })
    }

    pub(super) fn ordinal_of_id(&self, id: &str) -> Option<usize> {
        self.rows.iter().position(|row| row.id == id)
    }

    pub(super) fn pane_at_ordinal(&self, ordinal: usize) -> Option<PaneId> {
        self.row(ordinal)
            .and_then(|row| row.pane.as_ref())
            .map(|pane| pane.pane_id.clone())
    }

    pub(super) fn neighboring_group_head(&self, ordinal: usize, step: isize) -> Option<usize> {
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

pub(super) fn group_can_fold(group: &SidebarWorktreeGroup) -> bool {
    project_group(group, &BodyLens::default(), false, None, None).natural_hidden_count > 0
}

struct GroupProjection<'a> {
    rows: Vec<&'a SidebarRow>,
    natural_hidden_count: usize,
    rowless_workflow: bool,
}

fn project_group<'a>(
    group: &'a SidebarWorktreeGroup,
    lens: &BodyLens,
    expanded: bool,
    held: Option<&HashSet<String>>,
    focused_pane: Option<&PaneId>,
) -> GroupProjection<'a> {
    let facts = lens.filter.map(|_| GroupFacts::new(group));
    let owner = group.workflow_owner();
    let mut rows = group.rows.iter().collect::<Vec<_>>();
    if group.kind != SidebarWorktreeKind::External {
        let pane_ordinal = |row: &SidebarRow| {
            row.pane
                .as_ref()
                .and_then(|pane| pane.pane_id.creation_ordinal())
        };
        let mut cohorts: BTreeMap<_, Vec<_>> = BTreeMap::new();
        let mut solo = Vec::new();
        for row in rows {
            if let Some(cohort) = row.launch_cohort() {
                cohorts.entry(cohort).or_default().push(row);
            } else {
                solo.push(row);
            }
        }
        let staged = group.pipeline.as_ref().and(group.team.as_deref());
        let mut cohorts = cohorts.into_iter().collect::<Vec<_>>();
        cohorts.sort_by_key(|(cohort, members)| {
            let first = members.iter().filter_map(|row| pane_ordinal(row)).min();
            (Some(*cohort) != staged, first.is_none(), first, *cohort)
        });
        rows = Vec::with_capacity(group.rows.len());
        for (_, mut members) in cohorts {
            members.sort_by_key(|&row| {
                let ordinal = row.as_agent().and_then(|agent| agent.launch_ordinal);
                let pane = pane_ordinal(row);
                (
                    ordinal.is_none(),
                    ordinal,
                    pane.is_none(),
                    pane,
                    row.id.as_str(),
                )
            });
            rows.extend(members);
        }
        rows.extend(solo);
    }
    if let Some(index) = rows
        .iter()
        .position(|row| owner.is_some_and(|owner| owner.id == row.id))
    {
        rows[..=index].rotate_right(1);
    }
    let mut projection = project_rows(
        &rows,
        group.collapses(),
        lens.query.is_some(),
        expanded,
        held,
        lens.filter.zip(facts.as_ref()),
        focused_pane,
    );
    projection.rowless_workflow = group.rows.is_empty()
        && group.workflow.is_some()
        && lens.filter.is_none_or(|filter| filter.shows_group(group));
    let Some(query) = &lens.query else {
        return projection;
    };
    let query = query.to_lowercase();
    let contains = |text: &str| text.to_lowercase().contains(&query);
    if contains(&group.label)
        || group.label_qualifier.as_deref().is_some_and(contains)
        || group.team.as_deref().is_some_and(contains)
        || group
            .workflow
            .as_ref()
            .is_some_and(|run| contains(&run.workflow))
        || group
            .pr_number
            .is_some_and(|number| format!("#{number}").contains(&query))
    {
        return projection;
    }
    projection.rowless_workflow = false;
    projection.rows.retain(|row| {
        contains(&row.name)
            || row.worktree_branch.as_deref().is_some_and(contains)
            || row.team().is_some_and(contains)
            || row
                .as_agent()
                .and_then(|agent| agent.handle.as_deref())
                .is_some_and(|handle| contains(&format!("@{handle}")))
    });
    projection
}

fn project_rows<'a>(
    source: &[&'a SidebarRow],
    collapses: bool,
    querying: bool,
    expanded: bool,
    held: Option<&HashSet<String>>,
    filter: Option<(BodyFilter, &GroupFacts<'_>)>,
    focused_pane: Option<&PaneId>,
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
    // Keep a collapsing roster whole while focus or the layout freeze anchors any
    // member. Without a user expansion, every row collapses into the receipt
    // together once both clear.
    let revealed = collapses
        && source.iter().any(|row| {
            row_is_focused(row, focused_pane) || held.is_some_and(|ids| ids.contains(&row.id))
        });
    let mut rows = Vec::new();
    let mut natural_visible = 0;
    let mut actual_visible = 0;
    for row in source.iter().copied() {
        let essential = row.unread
            || row
                .status()
                .is_some_and(|status| status != AgentStatus::Idle)
            || row_is_focused(row, focused_pane)
            || liveness_process_id == Some(row.id.as_str());
        let natural = if collapses {
            revealed
        } else {
            essential || natural_visible < WORKTREE_ROW_CAP
        };
        natural_visible += usize::from(natural);

        let visible = match filter {
            Some((filter, facts)) => filter.shown(row, facts),
            None if querying || expanded => true,
            None if collapses => revealed,
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
        rowless_workflow: false,
    }
}

fn process_is_only_live_member(rows: &[&SidebarRow]) -> bool {
    rows.iter().copied().map(row_band).min() == Some(0)
        && rows
            .iter()
            .copied()
            .filter(|row| row_band(row) == 0)
            .all(SidebarRow::is_process)
}

/// Rows surviving the calm-tail cap, including held exemptions.
#[cfg(any(test, feature = "testkit"))]
pub(crate) fn capped_visible_rows<'a>(
    rows: &'a [SidebarRow],
    held: Option<&HashSet<String>>,
) -> Vec<&'a SidebarRow> {
    project_rows(
        &rows.iter().collect::<Vec<_>>(),
        false,
        false,
        false,
        held,
        None,
        None,
    )
    .rows
}

fn row_is_focused(row: &SidebarRow, focused_pane: Option<&PaneId>) -> bool {
    row.pane_ids().any(|pane| Some(pane) == focused_pane)
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
mod tests;
