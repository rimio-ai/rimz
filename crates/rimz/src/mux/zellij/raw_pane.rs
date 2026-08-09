//! Zellij topology projection and sidebar classification.

use std::collections::HashSet;

use crate::ids::PaneId;
use crate::mux::width::{sidebar_width_off_spec, zellij_resize_stop_step_cols};
use crate::mux::zellij::pane_topology::{PaneTopologyPane, ZellijPaneId};
use crate::pane::SIDEBAR_CHROME_TITLE;

/// A live, non-plugin sidebar pane is one Zellij still titles with the shared
/// sidebar chrome title.
pub(super) fn is_sidebar_pane(pane: &PaneTopologyPane) -> bool {
    !pane.is_plugin && pane.title.as_deref() == Some(SIDEBAR_CHROME_TITLE)
}

pub(super) fn leftmost_live_work_pane(
    panes: &[PaneTopologyPane],
    tab_position: u64,
) -> Option<u64> {
    panes
        .iter()
        .filter(|pane| {
            pane.tab_position == tab_position && pane.is_live_terminal() && !is_sidebar_pane(pane)
        })
        .min_by_key(|pane| (pane.pane_x.unwrap_or(u64::MAX), pane.id))
        .map(|pane| pane.id)
}

/// A daemon dashboard pane: any pane in the `rimzd` tab, or one whose spawn or
/// foreground command carries a host marker. The spawn command is the
/// authoritative Zellij signal for hosts that re-exec after launch.
pub(super) fn is_daemon_host_pane(pane: &PaneTopologyPane) -> bool {
    pane.tab_name.as_deref() == Some(crate::daemon_view::VIEW_NAME)
        || pane
            .spawn_command()
            .is_some_and(crate::daemon_view::command_is_host)
        || pane
            .foreground_command()
            .is_some_and(crate::daemon_view::command_is_host)
}

pub(super) fn floating_panes_in_anchor_view(
    panes: &[PaneTopologyPane],
    anchor: &PaneId,
) -> Vec<PaneId> {
    let Some(anchor_raw) = ZellijPaneId::try_from(anchor)
        .ok()
        .and_then(ZellijPaneId::terminal_id)
    else {
        return Vec::new();
    };
    let Some(anchor_tab) = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == anchor_raw)
        .map(|pane| pane.tab_position)
    else {
        return Vec::new();
    };
    panes
        .iter()
        .filter(|pane| pane.tab_position == anchor_tab && pane.is_floating && !pane.is_plugin)
        .map(|pane| PaneId::from(pane.native_id()))
        .collect()
}

/// Strict parse of `new-pane` stdout into a pane-id hint: exactly
/// `terminal_<digits>` after trimming, else `None`. Concurrent `zellij action`
/// clients can receive each other's responses, so anything looser would take
/// another command's output (an empty body, a JSON blob) for a pane id.
pub(super) fn parse_new_pane_id(stdout: &str) -> Option<ZellijPaneId> {
    let trimmed = stdout.trim();
    let pane = PaneId::from_parts(crate::ids::MuxName::Zellij, trimmed);
    let id = ZellijPaneId::try_from(&pane).ok()?;
    id.terminal_id().map(ZellijPaneId::Terminal)
}

/// The tiled width of `tab_position`, derived from the rightmost pane extent.
/// Floating, suppressed, and plugin panes do not define the tab's view width.
/// A single pane cannot prove the viewport: the sidebar is materialized first,
/// so that shape is a mid-layout snapshot whose extent is only the sidebar's.
pub(super) fn tab_view_cols(panes: &[PaneTopologyPane], tab_position: u64) -> Option<u64> {
    let mut extents = panes
        .iter()
        .filter(|pane| pane.tab_position == tab_position && pane.is_terminal())
        .filter_map(|pane| pane.pane_x?.checked_add(pane.pane_columns?));
    let first = extents.next()?;
    let second = extents.next()?;
    Some(extents.fold(first.max(second), u64::max))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SidebarDock {
    /// The sidebar occupies the tab's left column band: every other tiled
    /// terminal begins at or to the right of the sidebar's width.
    Docked,
    /// The sidebar is to the right of the left column; `move-pane left` can
    /// still reach the dock in a bounded swap loop.
    SwapReachable,
    /// The sidebar starts at `x=0`, but another tiled terminal also occupies
    /// the sidebar's column band. Zellij's left swaps cannot restructure that
    /// nested row into a full-height column.
    NestedRow,
}

/// Classify whether a sidebar pane is a full-height left dock. Zellij reports
/// only column geometry here, so the test is a band invariant: when a sidebar
/// is a real left column of width `W`, every other tiled terminal in that tab
/// starts at `x >= W`. A pane at `x < W` proves a nested-row layout. Panes
/// already planned for close are ignored so a duplicate sidebar cannot fake a
/// nested verdict. Missing sidebar geometry stays unknown and never triggers
/// repair from this predicate.
pub(super) fn sidebar_dock_verdict(
    pane: &PaneTopologyPane,
    panes: &[PaneTopologyPane],
    excluded: &HashSet<u64>,
) -> Option<SidebarDock> {
    let x = pane.pane_x?;
    let cols = pane.pane_columns?;
    if x != 0 {
        return Some(SidebarDock::SwapReachable);
    }
    let intrudes = panes.iter().any(|other| {
        other.tab_position == pane.tab_position
            && other.id != pane.id
            && !excluded.contains(&other.id)
            && other.is_terminal()
            && other.pane_x.is_some_and(|other_x| other_x < cols)
    });
    Some(if intrudes {
        SidebarDock::NestedRow
    } else {
        SidebarDock::Docked
    })
}

/// Every live work pane in a nested sidebar shape, ordered from right to left
/// for `stack-panes`. A newly added sidebar can stack this full set to recover
/// from Zellij mounting it into one row; repair of an existing user layout uses
/// [`repairable_nested_work_pane_ids`] and keeps multi-column layouts intact.
pub(super) fn nested_work_pane_ids(
    sidebar: &PaneTopologyPane,
    panes: &[PaneTopologyPane],
    excluded: &HashSet<u64>,
) -> Option<Vec<u64>> {
    if sidebar_dock_verdict(sidebar, panes, excluded) != Some(SidebarDock::NestedRow) {
        return None;
    }
    let mut work: Vec<_> = panes
        .iter()
        .filter(|pane| {
            pane.tab_position == sidebar.tab_position
                && pane.is_live_terminal()
                && !is_sidebar_pane(pane)
                && !excluded.contains(&pane.id)
        })
        .collect();
    if work.len() < 2 || work.iter().any(|pane| pane.pane_x.is_none()) {
        return None;
    }
    work.sort_by_key(|pane| (std::cmp::Reverse(pane.pane_x.unwrap_or(0)), pane.id));
    Some(work.into_iter().map(|pane| pane.id).collect())
}

/// Pane ids in the narrow nested-sidebar shape that RimZ can safely repair:
/// one right-side work column plus one or more live work panes intruding from
/// `x=0`. Newer Zellij can stack these panes in place; older supported Zellij
/// can close and verified-readd the sidebar. A real multi-column work layout is
/// left untouched and reported as mis-docked.
pub(super) fn repairable_nested_work_pane_ids(
    sidebar: &PaneTopologyPane,
    panes: &[PaneTopologyPane],
    excluded: &HashSet<u64>,
) -> Option<Vec<u64>> {
    let sidebar_cols = sidebar.pane_columns?;
    if sidebar_dock_verdict(sidebar, panes, excluded) != Some(SidebarDock::NestedRow) {
        return None;
    }

    let mut work: Vec<&PaneTopologyPane> = panes
        .iter()
        .filter(|pane| {
            pane.tab_position == sidebar.tab_position
                && pane.is_live_terminal()
                && !is_sidebar_pane(pane)
                && !excluded.contains(&pane.id)
        })
        .collect();
    if work.len() < 2 {
        return None;
    }

    let mut has_live_intruder = false;
    let mut right_column_x = None;
    for pane in &work {
        let x = pane.pane_x?;
        if x < sidebar_cols {
            if x != 0 {
                return None;
            }
            has_live_intruder = true;
        } else {
            match right_column_x {
                Some(right_x) if right_x != x => return None,
                Some(_) => {}
                None => right_column_x = Some(x),
            }
        }
    }
    if !has_live_intruder || right_column_x.is_none() {
        return None;
    }

    work.sort_by_key(|pane| (std::cmp::Reverse(pane.pane_x.unwrap_or(0)), pane.id));
    Some(work.into_iter().map(|pane| pane.id).collect())
}

/// Whether a kept sidebar pane sits off the layout's dock: outside the
/// full-height left column, nested beside a tiled pane that intrudes into its
/// column band, or — when `width_target` is proven — outside the upward stop
/// band for its live per-view target. Unknown geometry never reads off-spec.
pub(super) fn sidebar_geometry_off_spec(
    pane: &PaneTopologyPane,
    panes: &[PaneTopologyPane],
    excluded: &HashSet<u64>,
    width_target: Option<crate::mux::SidebarTarget>,
) -> bool {
    let Some(verdict) = sidebar_dock_verdict(pane, panes, excluded) else {
        return false;
    };
    matches!(verdict, SidebarDock::SwapReachable | SidebarDock::NestedRow)
        || width_target.is_some_and(|target| {
            pane.pane_columns.is_some_and(|cols| {
                tab_view_cols(panes, pane.tab_position).is_some_and(|view_cols| {
                    let target_cols = target
                        .cols(Some(u16::try_from(view_cols).unwrap_or(u16::MAX)))
                        .get();
                    sidebar_width_off_spec(
                        cols,
                        u64::from(target_cols),
                        zellij_resize_stop_step_cols(view_cols),
                    )
                })
            })
        })
}

/// The mounted sidebar pane an add produced: a fresh live, sidebar-titled
/// terminal pane in `tab_position` matching the stdout hint, or — the hint being
/// unreliable — the lowest fresh such id absent from the before-set, so a
/// cross-talk duplicate resolves deterministically and reconcile closes the
/// rest. A hinted pane already present before the add is never accepted.
pub(super) fn mounted_sidebar_pane(
    panes: &[PaneTopologyPane],
    tab_position: u64,
    before: &HashSet<u64>,
    hint: Option<u64>,
) -> Option<u64> {
    let ids: Vec<u64> = panes
        .iter()
        .filter(|pane| {
            pane.tab_position == tab_position && pane.is_live_terminal() && is_sidebar_pane(pane)
        })
        .map(|pane| pane.id)
        .collect();
    if let Some(raw) = hint
        && !before.contains(&raw)
        && ids.contains(&raw)
    {
        return Some(raw);
    }
    ids.into_iter().filter(|id| !before.contains(id)).min()
}

/// A newly-mounted sidebar outside `tab_position`. Prefer the stdout hint when
/// it identifies a candidate; otherwise accept exactly one new candidate so a
/// missing or cross-talked hint still lets repair clean up the wrong-tab mount.
pub(super) fn wrong_tab_mounted_sidebar_pane(
    panes: &[PaneTopologyPane],
    tab_position: u64,
    before: &HashSet<u64>,
    hint: Option<u64>,
) -> Option<u64> {
    let candidates: Vec<u64> = panes
        .iter()
        .filter(|pane| {
            pane.tab_position != tab_position
                && pane.is_live_terminal()
                && is_sidebar_pane(pane)
                && !before.contains(&pane.id)
        })
        .map(|pane| pane.id)
        .collect();
    hint.filter(|hint| candidates.contains(hint)).or_else(|| {
        (candidates.len() == 1)
            .then(|| candidates.first().copied())
            .flatten()
    })
}

#[cfg(test)]
mod tests;
