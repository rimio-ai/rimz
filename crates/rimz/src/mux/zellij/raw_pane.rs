//! Zellij topology projection and sidebar classification.

use std::collections::{HashMap, HashSet};

use crate::ids::{MuxName, PaneId};
use crate::mux::width::{live_target_cols, sidebar_width_off_spec, zellij_resize_step_cols};
use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane, TopologyClients};
use crate::mux::{ClientPresence, ClientView, PaneListing, SidebarWidth, ViewSidebars};
use crate::pane::SIDEBAR_CHROME_TITLE;

/// Cleanliness of a live room after a successful pane inspection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SessionCleanliness {
    /// Sidebar and command panes are running.
    Clean,
    /// The sidebar is absent or held at a "Waiting to run" prompt.
    MissingSidebar,
    /// At least one non-sidebar command pane is held at a "Waiting to run" prompt.
    SuspendedCommandPane,
}

/// A live, non-plugin sidebar pane is one Zellij still titles with the shared
/// sidebar chrome title — the same signal `classify_session_panes` trusts.
pub(super) fn is_sidebar_pane(pane: &PaneTopologyPane) -> bool {
    !pane.is_plugin && pane.title.as_deref() == Some(SIDEBAR_CHROME_TITLE)
}

/// Group a pane list into per-tab [`ViewSidebars`] for the reconcile planner:
/// each tab's sidebar panes (as normalized [`PaneId`]s) and whether it holds a
/// user-working terminal pane. Daemon dashboard panes in `rimzd` are not work.
/// First-seen tab order; pane order within a tab preserved.
pub(super) fn views_with_sidebars(panes: &[PaneTopologyPane]) -> Vec<ViewSidebars> {
    let mut views: Vec<ViewSidebars> = Vec::new();
    let mut index: HashMap<u64, usize> = HashMap::new();
    for pane in panes.iter().filter(|pane| pane.is_terminal()) {
        let slot = *index.entry(pane.tab_position).or_insert_with(|| {
            views.push(ViewSidebars {
                view: pane.tab_position.to_string(),
                sidebar_panes: Vec::new(),
                has_working: false,
                has_daemon_host: false,
            });
            views.len() - 1
        });
        if is_sidebar_pane(pane) {
            views[slot].sidebar_panes.push(PaneId::from_parts(
                MuxName::Zellij,
                format!("terminal_{}", pane.id),
            ));
        } else if is_daemon_host_pane(pane) {
            views[slot].has_daemon_host = true;
        } else {
            views[slot].has_working = true;
        }
    }
    views
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
fn is_daemon_host_pane(pane: &PaneTopologyPane) -> bool {
    pane.tab_name.as_deref() == Some(crate::daemon_view::VIEW_NAME)
        || pane
            .spawn_command()
            .is_some_and(crate::daemon_view::command_is_host)
        || pane
            .foreground_command()
            .is_some_and(crate::daemon_view::command_is_host)
}

pub(super) fn classify_session_panes(panes: &[PaneTopologyPane]) -> SessionCleanliness {
    if !has_healthy_sidebar(panes) {
        return SessionCleanliness::MissingSidebar;
    }
    if has_suspended_command_pane(panes) {
        return SessionCleanliness::SuspendedCommandPane;
    }
    SessionCleanliness::Clean
}

pub(super) fn has_healthy_sidebar(panes: &[PaneTopologyPane]) -> bool {
    let mut found = false;
    for pane in panes.iter().filter(|pane| is_sidebar_pane(pane)) {
        found = true;
        if pane.is_held {
            return false;
        }
    }
    found
}

/// Any non-sidebar command pane Zellij is holding at a "Waiting to run" prompt —
/// the fingerprint of a resurrected (serialized) room, where every command pane
/// comes back `start_suspended`. Floating panes count here because a floating
/// agent is a real command pane, even though geometry ignores overlays.
pub(super) fn has_suspended_command_pane(panes: &[PaneTopologyPane]) -> bool {
    panes
        .iter()
        .any(|pane| is_session_health_command_pane(pane) && pane.is_held)
}

fn is_session_health_command_pane(pane: &PaneTopologyPane) -> bool {
    !pane.is_plugin && !pane.is_suppressed && !is_sidebar_pane(pane)
}

pub(super) fn parse_terminal_id(pane_id: &str) -> Option<u64> {
    pane_id.strip_prefix("terminal_")?.parse().ok()
}

pub(super) fn zellij_pane_id(raw: u64) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, format!("terminal_{raw}"))
}

pub(super) fn floating_panes_in_anchor_view(
    panes: &[PaneTopologyPane],
    anchor: &PaneId,
) -> Vec<PaneId> {
    let Some(anchor_raw) = parse_terminal_id(anchor.raw()) else {
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
        .map(|pane| zellij_pane_id(pane.id))
        .collect()
}

/// Strict parse of `new-pane` stdout into a pane-id hint: exactly
/// `terminal_<digits>` after trimming, else `None`. Concurrent `zellij action`
/// clients can receive each other's responses, so anything looser would take
/// another command's output (an empty body, a JSON blob) for a pane id.
pub(super) fn parse_new_pane_id(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    parse_terminal_id(trimmed)?;
    Some(trimmed.to_owned())
}

/// The tiled width of `tab_position`, derived from the rightmost pane extent.
/// Floating, suppressed, and plugin panes do not define the tab's view width.
pub(super) fn tab_view_cols(panes: &[PaneTopologyPane], tab_position: u64) -> Option<u64> {
    panes
        .iter()
        .filter(|pane| pane.tab_position == tab_position && pane.is_terminal())
        .filter_map(|pane| pane.pane_x?.checked_add(pane.pane_columns?))
        .max()
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
/// column band, or outside the tolerated band around its live per-view target.
/// Unknown geometry never reads off-spec.
pub(super) fn sidebar_geometry_off_spec(
    pane: &PaneTopologyPane,
    panes: &[PaneTopologyPane],
    excluded: &HashSet<u64>,
    width: SidebarWidth,
    width_override: Option<std::num::NonZeroU16>,
) -> bool {
    let Some(verdict) = sidebar_dock_verdict(pane, panes, excluded) else {
        return false;
    };
    matches!(verdict, SidebarDock::SwapReachable | SidebarDock::NestedRow)
        || pane.pane_columns.is_some_and(|cols| {
            tab_view_cols(panes, pane.tab_position).is_some_and(|view_cols| {
                sidebar_width_off_spec(
                    cols,
                    live_target_cols(width, width_override, view_cols),
                    zellij_resize_step_cols(view_cols),
                )
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

#[derive(Debug)]
pub(super) struct RawPaneListing {
    pub(super) panes: Vec<PaneTopologyPane>,
    pub(super) observed_at_ms: u64,
    pub(super) session_focus: Option<PaneId>,
    pub(super) client_view: Option<ClientView>,
}

impl RawPaneListing {
    pub(super) fn from_topology(cache: PaneTopologyCache) -> Self {
        let PaneTopologyCache {
            produced_at_ms,
            focused_pane,
            clients,
            panes,
            ..
        } = cache;
        Self {
            panes,
            observed_at_ms: produced_at_ms,
            session_focus: focused_pane.map(zellij_pane_id),
            client_view: clients.map(client_view_from_topology),
        }
    }

    pub(super) fn into_pane_listing(
        self,
        session_name: String,
        project: impl FnMut(PaneTopologyPane, &str) -> Option<crate::pane::PaneRef>,
    ) -> PaneListing {
        let mut project = project;
        PaneListing {
            panes: self
                .panes
                .into_iter()
                .filter_map(|pane| project(pane, &session_name))
                .collect(),
            observed_at_ms: self.observed_at_ms,
            session_focus: self.session_focus,
            client_view: self.client_view,
        }
    }
}

fn client_view_from_topology(clients: TopologyClients) -> ClientView {
    ClientView {
        clients: clients
            .views
            .into_iter()
            .map(|view| crate::mux::ClientPaneView {
                client_id: crate::mux::MuxClientId::Zellij(view.client_id),
                pane_id: match view.pane_id {
                    super::pane_topology::TopologyClientPane::Terminal(id) => zellij_pane_id(id),
                    super::pane_topology::TopologyClientPane::Plugin(id) => {
                        PaneId::from_parts(MuxName::Zellij, format!("plugin_{id}"))
                    }
                },
            })
            .collect(),
        viewed_panes: clients
            .viewed_panes
            .into_iter()
            .map(zellij_pane_id)
            .collect(),
        presence: ClientPresence {
            human_clients: clients.human_clients as usize,
            last_input_ms: None,
        },
    }
}

#[cfg(test)]
mod tests;
