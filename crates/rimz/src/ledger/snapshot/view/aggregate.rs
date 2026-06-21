use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::agents::AgentState;
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

/// The per-machine attention timing windows the row fold reads, bundled so the
/// fold entry point stays under the argument cap and the two knobs travel
/// together from the config that owns them.
#[derive(Clone, Copy)]
pub(super) struct AttentionWindows {
    /// Silent-`running` → actionable `!` projection window (`stalled_after_secs`).
    pub stalled_after_secs: u32,
    /// No-activity → inactive-sink window (`inactive_after_secs`).
    pub inactive_after_secs: u32,
}

pub(super) fn build_worktree_groups_from_rows(
    mut rows: Vec<SidebarRow>,
    agents: &[AgentState],
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
    root_class: RootClass,
    now: Timestamp,
    windows: AttentionWindows,
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
    status::project_display_status(&mut rows, agents, now, windows.stalled_after_secs);
    stamp_inactive(&mut rows, now, windows.inactive_after_secs);

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
            // Prefer a branch label over the path-basename seed for worktree
            // pods. Root pods keep the room name; external keeps its catch-all
            // label even if a stray branch rode the row.
            let label = if kind == SidebarWorktreeKind::Worktree {
                group_branch_label(&rows).unwrap_or(label)
            } else {
                label
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
                landed: None,
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(compare_groups);
    groups
}

/// Stamp the inactive sink: a row with no activity past `inactive_after_secs`
/// drops into the inactive partition, beneath every live row, whatever its
/// status — a stale `waiting` ask sinks the same as a stale `idle`, then leads
/// the inactive band by its attention rank. Durable `unread` still outranks the
/// sink. The boundary is strict (`>`), so the configured window is the last
/// live second.
fn stamp_inactive(rows: &mut [SidebarRow], now: Timestamp, inactive_after_secs: u32) {
    for row in rows {
        row.inactive =
            now.duration_since(row.last_activity).as_secs() > i64::from(inactive_after_secs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::snapshot::row::{ProcessCard, RowCard};

    fn now() -> Timestamp {
        Timestamp::from_second(1_750_000_000).expect("fixed test instant is valid")
    }

    /// A status-less row whose only activity was `age_secs` before `now` — all
    /// `stamp_inactive` reads is `last_activity`, so the card kind is irrelevant.
    fn row_aged(age_secs: i64) -> SidebarRow {
        SidebarRow {
            id: "r".into(),
            name: "r".into(),
            pane: None,
            worktree_path: None,
            worktree_branch: None,
            unread: false,
            inactive: false,
            last_activity: now() - std::time::Duration::from_secs(age_secs as u64),
            card: RowCard::Process(ProcessCard::default()),
        }
    }

    #[test]
    fn inactive_window_is_threshold_driven_and_strict() {
        // The same row sinks or stays live by the configured window alone.
        let mut rows = vec![row_aged(1_800)];
        stamp_inactive(&mut rows, now(), 3_600);
        assert!(
            !rows[0].inactive,
            "30m of silence is live under a one-hour window"
        );
        stamp_inactive(&mut rows, now(), 1_200);
        assert!(
            rows[0].inactive,
            "the same row sinks under a twenty-minute window"
        );

        // The boundary is strict: the configured window is the last live second.
        let mut exact = vec![row_aged(3_600)];
        stamp_inactive(&mut exact, now(), 3_600);
        assert!(!exact[0].inactive, "exactly at the window is still live");
        let mut past = vec![row_aged(3_601)];
        stamp_inactive(&mut past, now(), 3_600);
        assert!(past[0].inactive, "one second past the window sinks");
    }
}
