//! Sidebar body membership and stable row ordinals.

use std::collections::{BTreeSet, HashSet};
use std::ops::Range;

use crate::agents::AgentStatus;
use crate::ids::PaneId;
use crate::store::snapshot::{SidebarRow, SidebarSnapshot, SidebarWorktreeGroup, WorktreePrState};

pub(crate) use crate::sidebar::body_filter::BodyFilter;
pub(super) use crate::sidebar::body_filter::BodyLens;

/// Maximum calm rows painted before overflow moves behind `+K more`.
pub const WORKTREE_ROW_CAP: usize = 6;

/// One projected group, indexed into its roster's flat row slice.
#[derive(Clone, Debug)]
pub(super) struct VisibleGroup<'a> {
    source: &'a SidebarWorktreeGroup,
    range: Range<usize>,
    expanded: bool,
    natural_hidden_count: usize,
    hidden_count: usize,
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

/// One body projection shared by render, browse, selection, and order holds.
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
        for group in &snapshot.worktree_groups {
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

    pub(super) fn group_containing(&self, ordinal: usize) -> Option<&VisibleGroup<'a>> {
        self.groups
            .iter()
            .find(|group| group.range.contains(&ordinal))
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

struct GroupProjection<'a> {
    rows: Vec<&'a SidebarRow>,
    natural_hidden_count: usize,
}

fn project_group<'a>(
    group: &'a SidebarWorktreeGroup,
    lens: &BodyLens,
    expanded: bool,
    held: Option<&HashSet<String>>,
    focused_pane: Option<&PaneId>,
) -> GroupProjection<'a> {
    let pr_open = group.pr_state == Some(WorktreePrState::Open);
    let mut projection = project_rows(
        &group.rows,
        group.collapses(),
        lens,
        expanded,
        held,
        pr_open,
        focused_pane,
    );
    let Some(query) = &lens.query else {
        return projection;
    };
    let query = query.to_lowercase();
    let contains = |text: &str| text.to_lowercase().contains(&query);
    if contains(&group.label)
        || group.label_qualifier.as_deref().is_some_and(contains)
        || group.team.as_deref().is_some_and(contains)
        || group
            .pr_number
            .is_some_and(|number| format!("#{number}").contains(&query))
    {
        return projection;
    }
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
    source: &'a [SidebarRow],
    collapses: bool,
    lens: &BodyLens,
    expanded: bool,
    held: Option<&HashSet<String>>,
    pr_open: bool,
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
    // Keep a collapsing roster whole while focus or the order hold anchors any
    // member. Once both clear, every row collapses into the receipt together.
    let revealed = collapses
        && source.iter().any(|row| {
            row_is_focused(row, focused_pane) || held.is_some_and(|ids| ids.contains(&row.id))
        });
    let mut rows = Vec::new();
    let mut natural_visible = 0;
    let mut actual_visible = 0;
    for row in source {
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

        let visible = match lens.filter {
            Some(filter) => filter.matches(row, pr_open),
            None if lens.query.is_some() || expanded => true,
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
    }
}

fn process_is_only_live_member(rows: &[SidebarRow]) -> bool {
    rows.iter().map(row_band).min() == Some(0)
        && rows
            .iter()
            .filter(|row| row_band(row) == 0)
            .all(SidebarRow::is_process)
}

/// Rows surviving the calm-tail cap, including held exemptions.
pub fn capped_visible_rows<'a>(
    rows: &'a [SidebarRow],
    held: Option<&HashSet<String>>,
) -> Vec<&'a SidebarRow> {
    project_rows(rows, false, &BodyLens::default(), false, held, false, None).rows
}

fn row_is_focused(row: &SidebarRow, focused_pane: Option<&PaneId>) -> bool {
    row.pane
        .as_ref()
        .is_some_and(|pane| Some(&pane.pane_id) == focused_pane)
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
    use crate::store::snapshot::{AgentCard, RowCard, SidebarWorktreeKind};
    use jiff::Timestamp;

    #[test]
    fn roster_projects_cap_expansion_filter_hold_and_control_counts_once() {
        let group = group(idle_rows(9));
        let snapshot = snapshot(vec![group]);

        let collapsed = VisibleRoster::new(&snapshot, &BodyLens::default(), &BTreeSet::new(), None);
        assert_eq!(
            ids(&collapsed),
            ["idle-0", "idle-1", "idle-2", "idle-3", "idle-4", "idle-5"]
        );
        assert_eq!(collapsed.groups()[0].natural_hidden_count(), 3);
        assert_eq!(collapsed.groups()[0].hidden_count(), 3);

        let expanded_keys = BTreeSet::from(["group-0".to_owned()]);
        let expanded = VisibleRoster::new(&snapshot, &BodyLens::default(), &expanded_keys, None);
        assert_eq!(expanded.len(), 9);
        assert!(expanded.groups()[0].expanded());
        assert_eq!(expanded.groups()[0].natural_hidden_count(), 3);
        assert_eq!(expanded.groups()[0].hidden_count(), 0);

        let filtered = VisibleRoster::new(
            &snapshot,
            &BodyLens::from(BodyFilter::Status(AgentStatus::Idle)),
            &BTreeSet::new(),
            None,
        );
        assert_eq!(filtered.len(), 9, "filters expose every matching row");

        let held_ids = HashSet::from(["idle-8".to_owned()]);
        let held = VisibleRoster::new(
            &snapshot,
            &BodyLens::default(),
            &BTreeSet::new(),
            Some(&held_ids),
        );
        assert_eq!(held.len(), 7);
        assert_eq!(held.groups()[0].natural_hidden_count(), 3);
        assert_eq!(held.groups()[0].hidden_count(), 2);
        assert!(ids(&held).contains(&"idle-8"));
    }

    #[test]
    fn roster_keeps_attention_focus_unread_and_only_live_process_beyond_cap() {
        let mut rows = idle_rows(9);
        rows[6].unread = true;
        rows[8].card = RowCard::Agent(Box::new(AgentCard {
            status: AgentStatus::Failed,
            ..AgentCard::default()
        }));
        let mut attention_snapshot = snapshot(vec![group(rows)]);
        attention_snapshot.focused_pane = attention_snapshot.worktree_groups[0].rows[7]
            .pane
            .as_ref()
            .map(|pane| pane.pane_id.clone());
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

        snapshot.focused_pane = snapshot.worktree_groups[0].rows[1]
            .pane
            .as_ref()
            .map(|pane| pane.pane_id.clone());
        let focused = VisibleRoster::baseline(&snapshot);
        assert_eq!(ids(&focused), ["done-unread", "done-focused", "active"]);
        assert_eq!(focused.ordinal_of_id("active"), Some(2));
        assert_eq!(focused.neighboring_group_head(0, 1), Some(2));

        let filtered = VisibleRoster::new(
            &snapshot,
            &BodyLens::from(BodyFilter::Status(AgentStatus::Success)),
            &BTreeSet::new(),
            None,
        );
        assert_eq!(ids(&filtered), ["done-unread", "done-focused"]);
    }

    #[test]
    fn roster_keeps_lone_finished_row_visible() {
        let mut finished = group(vec![agent_row("done", AgentStatus::Success)]);
        finished.finished = true;
        let snapshot = snapshot(vec![finished]);

        let roster = VisibleRoster::baseline(&snapshot);

        assert_eq!(ids(&roster), ["done"]);
        assert_eq!(roster.groups()[0].hidden_count(), 0);
        assert_eq!(roster.groups()[0].natural_hidden_count(), 0);
    }

    #[test]
    fn open_pr_filter_keeps_every_row_in_open_pr_groups() {
        let mut open = group(idle_rows(8));
        open.key = "open".to_owned();
        open.rows.push(process_row("open-shell"));
        open.pr_state = Some(WorktreePrState::Open);
        open.pr_number = Some(91);

        let mut closed = group(vec![agent_row("closed", AgentStatus::Failed)]);
        closed.key = "closed".to_owned();
        closed.pr_state = Some(WorktreePrState::Closed);
        let snapshot = snapshot(vec![open, closed]);

        let filtered = VisibleRoster::new(
            &snapshot,
            &BodyLens::from(BodyFilter::OpenPr),
            &BTreeSet::new(),
            None,
        );

        assert_eq!(filtered.len(), 9, "the PR lens bypasses the calm row cap");
        assert!(ids(&filtered).contains(&"open-shell"));
        assert!(!ids(&filtered).contains(&"closed"));
    }

    #[test]
    fn query_matches_row_name_branch_and_handle_case_insensitively() {
        let mut rows = idle_rows(9);
        rows[0].name = "Auth helper".to_owned();
        rows[7].worktree_branch = Some("feature/AUTH".to_owned());
        if let RowCard::Agent(card) = &mut rows[8].card {
            card.handle = Some("AuthScout".to_owned());
        }
        rows.push(process_row("unrelated"));
        let mut unrelated = group(vec![agent_row("other", AgentStatus::Running)]);
        unrelated.key = "other".to_owned();
        let snapshot = snapshot(vec![group(rows), unrelated]);
        let lens = BodyLens {
            query: Some("aUtH".to_owned()),
            ..Default::default()
        };
        let roster = VisibleRoster::new(&snapshot, &lens, &BTreeSet::new(), None);
        assert_eq!(ids(&roster), ["idle-0", "idle-7", "idle-8"]);
        assert!(roster.groups()[1].is_empty());
        assert_eq!(roster.groups()[0].hidden_count(), 0);
        let lens = BodyLens {
            query: Some("@authscout".to_owned()),
            ..Default::default()
        };
        let roster = VisibleRoster::new(&snapshot, &lens, &BTreeSet::new(), None);
        assert_eq!(ids(&roster), ["idle-8"]);
    }

    #[test]
    fn query_matches_row_team_in_a_mixed_team_group() {
        let mut rows = idle_rows(3);
        for (row, team) in rows.iter_mut().zip(["forge", "docs", "forge"]) {
            row.as_agent_mut().unwrap().team = Some(team.to_owned());
        }
        let snapshot = snapshot(vec![group(rows)]);
        let lens = BodyLens {
            query: Some("forge".to_owned()),
            ..Default::default()
        };
        let roster = VisibleRoster::new(&snapshot, &lens, &BTreeSet::new(), None);
        assert_eq!(ids(&roster), ["idle-0", "idle-2"]);
    }

    #[test]
    fn group_query_matches_keep_all_rows_and_lift_the_fold_cap() {
        let mut matching = group(idle_rows(8));
        matching.label = "feature/Auth".to_owned();
        matching.label_qualifier = Some("RepoIdentity".to_owned());
        matching.team = Some("Forge".to_owned());
        matching.pr_number = Some(123);
        let snapshot = snapshot(vec![matching]);
        for query in ["AUTH", "identity", "forge", "#123", "123"] {
            let lens = BodyLens {
                query: Some(query.to_owned()),
                ..Default::default()
            };
            let roster = VisibleRoster::new(&snapshot, &lens, &BTreeSet::new(), None);
            assert_eq!(roster.len(), 8, "group query: {query}");
            assert_eq!(roster.groups()[0].hidden_count(), 0);
            assert_eq!(roster.groups()[0].natural_hidden_count(), 0);
        }
    }

    #[test]
    fn query_composes_with_status_even_when_the_group_matches() {
        let mut matching = group(vec![
            agent_row("running", AgentStatus::Running),
            agent_row("idle", AgentStatus::Idle),
        ]);
        matching.label = "Auth".to_owned();
        let mut unrelated = group(vec![agent_row("other", AgentStatus::Running)]);
        unrelated.key = "other".to_owned();
        let snapshot = snapshot(vec![matching, unrelated]);
        let lens = BodyLens {
            filter: Some(BodyFilter::Status(AgentStatus::Running)),
            query: Some("auth".to_owned()),
        };
        let roster = VisibleRoster::new(&snapshot, &lens, &BTreeSet::new(), None);
        assert_eq!(ids(&roster), ["running"]);
    }

    fn snapshot(groups: Vec<SidebarWorktreeGroup>) -> SidebarSnapshot {
        let workspace = WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap();
        let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
        snapshot.worktree_groups = groups;
        snapshot
    }

    fn group(rows: Vec<SidebarRow>) -> SidebarWorktreeGroup {
        SidebarWorktreeGroup {
            pr_stack: Default::default(),
            key: "group-0".to_owned(),
            label: "main".to_owned(),
            label_qualifier: None,
            kind: SidebarWorktreeKind::Worktree,
            team: None,
            cohort_effort: None,
            pipeline: None,
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
            pr_queue: None,
            ci: None,
            pr_number: None,
            pr_url: None,
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
        row.card = RowCard::Process(crate::store::snapshot::ProcessCard::default());
        row
    }

    fn ids<'a>(roster: &'a VisibleRoster<'_>) -> Vec<&'a str> {
        roster.rows().iter().map(|row| row.id.as_str()).collect()
    }
}
