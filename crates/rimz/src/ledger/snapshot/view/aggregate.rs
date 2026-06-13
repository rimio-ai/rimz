use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::feed::{ATTENTION_AGE_CEILING_SECS, AgentState, AgentStatus};
use crate::ledger::snapshot::row::SidebarRow;
use crate::workspace::RootClass;

use super::layout::{
    capped_rows, compare_groups, compare_rows, group_branch_label, multi_branch_paths,
    status_counts, worktree_group_key,
};
use super::{SidebarWorktreeGroup, SidebarWorktreeKind};

mod status;
mod subagents;

#[cfg(not(test))]
use subagents::attach_sub_agents;
#[cfg(test)]
pub(in crate::ledger::snapshot) use subagents::{attach_sub_agents, sub_agent_from_state};

pub(super) fn build_worktree_groups_from_rows(
    mut rows: Vec<SidebarRow>,
    agents: &[AgentState],
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
    root_class: RootClass,
    now: Timestamp,
    stalled_after_secs: u32,
) -> Vec<SidebarWorktreeGroup> {
    // Nest each subagent under its parent root row before grouping. This is the
    // one chokepoint every live (`rows_from_panes`) card flows through, so
    // nesting behaves identically for process, agent, and attention rows.
    attach_sub_agents(&mut rows, agents, now);
    // A delegating parent's work is its children's, so their activity advances
    // the parent row's displayed clock before the stall check reads it.
    subagents::fold_child_activity_onto_parents(&mut rows);
    // Project the displayed status now that each row knows its subagents and
    // the full agent set is in hand.
    status::project_display_status(&mut rows, agents, now, stalled_after_secs);
    stamp_inactive(&mut rows, now);

    let multi_branch = multi_branch_paths(
        rows.iter()
            .map(|row| (row.worktree_path.as_deref(), row.worktree_branch.as_deref())),
    );

    let mut by_group: BTreeMap<String, (String, SidebarWorktreeKind, Vec<SidebarRow>)> =
        BTreeMap::new();
    for row in rows {
        let split_by_branch = row
            .worktree_path
            .as_deref()
            .is_some_and(|path| multi_branch.contains(path));
        let (kind, key, label) = worktree_group_key(
            row.worktree_path.as_deref(),
            row.worktree_branch.as_deref(),
            split_by_branch,
            project_root,
            worktree_roots,
            root_class,
        );
        by_group
            .entry(key)
            .and_modify(|(_, _, rows)| rows.push(row.clone()))
            .or_insert_with(|| (label, kind, vec![row]));
    }

    let mut groups = by_group
        .into_iter()
        .map(|(key, (label, kind, mut rows))| {
            rows.sort_by(compare_rows);
            // Prefer a branch label over the path-basename seed unless this is
            // a root pod, whose room name must stay stable.
            let label = if kind == SidebarWorktreeKind::Root {
                label
            } else {
                group_branch_label(&rows).unwrap_or(label)
            };
            let status_counts = status_counts(&rows);
            let total = rows.len();
            rows = capped_rows(rows);
            SidebarWorktreeGroup {
                key,
                label,
                kind,
                status_counts,
                hidden_count: total.saturating_sub(rows.len()),
                rows,
                diff_added: None,
                diff_removed: None,
                commits_ahead: None,
                commits_behind: None,
                trunk: None,
                clean: None,
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(compare_groups);
    groups
}

fn stamp_inactive(rows: &mut [SidebarRow], now: Timestamp) {
    for row in rows {
        row.inactive = matches!(row.status(), Some(AgentStatus::Success | AgentStatus::Idle))
            && now.duration_since(row.last_activity).as_secs() > ATTENTION_AGE_CEILING_SECS;
    }
}
