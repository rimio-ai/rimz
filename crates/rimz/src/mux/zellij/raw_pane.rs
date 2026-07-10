//! Raw Zellij pane projection, topology-cache reads, and sidebar classification.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env,
    num::NonZeroU16,
};

use serde::Deserialize;

use crate::ids::{MuxName, PaneId};
use crate::mux::width::sidebar_width_off_spec;
use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane, TopologyClients};
use crate::mux::{ClientPresence, ClientView, PaneListing, ViewSidebars};
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
pub(super) fn is_sidebar_pane(pane: &RawPane) -> bool {
    !pane.is_plugin && pane.title.as_deref() == Some(SIDEBAR_CHROME_TITLE)
}

/// Group a pane list into per-tab [`ViewSidebars`] for the reconcile planner:
/// each tab's sidebar panes (as normalized [`PaneId`]s) and whether it holds a
/// user-working terminal pane. Daemon dashboard panes in `rimzd` are not work.
/// First-seen tab order; pane order within a tab preserved.
pub(super) fn views_with_sidebars(panes: &[RawPane]) -> Vec<ViewSidebars> {
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

pub(super) fn tabs_with_sidebars(panes: &[RawPane]) -> HashSet<String> {
    views_with_sidebars(panes)
        .into_iter()
        .filter(|view| !view.sidebar_panes.is_empty())
        .map(|view| view.view)
        .collect()
}

pub(super) fn docked_sidebar_cols(panes: &[RawPane]) -> Option<NonZeroU16> {
    let excluded = HashSet::new();
    let mut widths: BTreeMap<NonZeroU16, (usize, u64)> = BTreeMap::new();
    for pane in panes.iter().filter(|pane| {
        pane.is_live_terminal()
            && is_sidebar_pane(pane)
            && sidebar_dock_verdict(pane, panes, &excluded) == Some(SidebarDock::Docked)
    }) {
        let Some(cols) = pane
            .pane_columns
            .and_then(|cols| u16::try_from(cols).ok())
            .and_then(NonZeroU16::new)
        else {
            continue;
        };
        let (count, first_tab) = widths.entry(cols).or_insert((0, pane.tab_position));
        *count += 1;
        *first_tab = (*first_tab).min(pane.tab_position);
    }
    widths
        .into_iter()
        .fold(None, |best, (cols, (count, first_tab))| match best {
            Some((best_cols, best_count, best_first_tab))
                if best_count > count || (best_count == count && best_first_tab <= first_tab) =>
            {
                Some((best_cols, best_count, best_first_tab))
            }
            _ => Some((cols, count, first_tab)),
        })
        .map(|(cols, _, _)| cols)
}

pub(super) fn leftmost_live_work_pane(panes: &[RawPane], tab_position: u64) -> Option<u64> {
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
fn is_daemon_host_pane(pane: &RawPane) -> bool {
    pane.tab_name.as_deref() == Some(crate::remote_control::VIEW_NAME)
        || pane
            .spawn_command()
            .is_some_and(crate::remote_control::command_is_host)
        || pane
            .foreground_command()
            .is_some_and(crate::remote_control::command_is_host)
}

pub(super) fn classify_session_panes(panes: &[RawPane]) -> SessionCleanliness {
    if !has_healthy_sidebar(panes) {
        return SessionCleanliness::MissingSidebar;
    }
    if has_suspended_command_pane(panes) {
        return SessionCleanliness::SuspendedCommandPane;
    }
    SessionCleanliness::Clean
}

pub(super) fn has_healthy_sidebar(panes: &[RawPane]) -> bool {
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
pub(super) fn has_suspended_command_pane(panes: &[RawPane]) -> bool {
    panes
        .iter()
        .any(|pane| is_session_health_command_pane(pane) && pane.is_held)
}

fn is_session_health_command_pane(pane: &RawPane) -> bool {
    !pane.is_plugin && !pane.is_suppressed && !is_sidebar_pane(pane)
}

/// `ZELLIJ_PANE_ID` is the bare integer of the pane the caller runs in. `rimz
/// reload` runs in the user's pane, so refocusing it restores their visible tab.
pub(super) fn own_zellij_pane_id() -> Option<u64> {
    env::var("ZELLIJ_PANE_ID").ok()?.trim().parse().ok()
}

pub(super) fn parse_terminal_id(pane_id: &str) -> Option<u64> {
    pane_id.strip_prefix("terminal_")?.parse().ok()
}

pub(super) fn zellij_pane_id(raw: u64) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, format!("terminal_{raw}"))
}

pub(super) fn floating_panes_in_anchor_view(panes: &[RawPane], anchor: &PaneId) -> Vec<PaneId> {
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
pub(super) fn tab_view_cols(panes: &[RawPane], tab_position: u64) -> Option<u64> {
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
    pane: &RawPane,
    panes: &[RawPane],
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

/// Pane ids in the narrow nested-sidebar shape that Rimz can safely repair:
/// one right-side work column plus one or more live work panes intruding from
/// `x=0`. Newer Zellij can stack these panes in place; older supported Zellij
/// can close and verified-readd the sidebar. A real multi-column work layout is
/// left untouched and reported as mis-docked.
pub(super) fn repairable_nested_work_pane_ids(
    sidebar: &RawPane,
    panes: &[RawPane],
    excluded: &HashSet<u64>,
) -> Option<Vec<u64>> {
    let sidebar_cols = sidebar.pane_columns?;
    if sidebar_dock_verdict(sidebar, panes, excluded) != Some(SidebarDock::NestedRow) {
        return None;
    }

    let mut work: Vec<&RawPane> = panes
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
/// column band, or wider than the session's fixed birth width. Unknown
/// geometry never reads off-spec.
pub(super) fn sidebar_geometry_off_spec(
    pane: &RawPane,
    panes: &[RawPane],
    excluded: &HashSet<u64>,
    canonical_cols: u64,
) -> bool {
    let Some(verdict) = sidebar_dock_verdict(pane, panes, excluded) else {
        return false;
    };
    matches!(verdict, SidebarDock::SwapReachable | SidebarDock::NestedRow)
        || pane.pane_columns.is_some_and(|cols| {
            tab_view_cols(panes, pane.tab_position)
                .is_some_and(|view_cols| sidebar_width_off_spec(cols, canonical_cols, view_cols))
        })
}

/// The mounted sidebar pane an add produced: a fresh live, sidebar-titled
/// terminal pane in `tab_position` matching the stdout hint, or — the hint being
/// unreliable — the lowest fresh such id absent from the before-set, so a
/// cross-talk duplicate resolves deterministically and reconcile closes the
/// rest. A hinted pane already present before the add is never accepted.
pub(super) fn mounted_sidebar_pane(
    panes: &[RawPane],
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

/// Pane fields published by the presence plugin topology cache.
#[derive(Debug, Deserialize)]
pub(super) struct RawPane {
    pub(super) id: u64,
    pub(super) is_plugin: bool,
    #[serde(default)]
    pub(super) is_held: bool,
    /// Command has exited but Zellij still shows the pane (e.g. hold-on-close).
    /// A dead pane, not a live process — excluded from the pane listing.
    #[serde(default)]
    pub(super) exited: bool,
    #[serde(default)]
    pub(super) is_suppressed: bool,
    #[serde(default)]
    pub(super) is_floating: bool,
    #[serde(default)]
    pub(super) is_focused: bool,
    /// Positional tab index from the plugin manifest. Zellij's internal tab ids
    /// are intentionally absent from product runtime.
    #[serde(alias = "tab_id")]
    pub(super) tab_position: u64,
    /// Name of the tab the pane lives in. Routed into [`PaneRef::view_name`];
    /// also how the `rimzd` daemon view is recognised on Zellij, whose pane list
    /// reports no command fields a classifier could read instead.
    #[serde(default)]
    pub(super) tab_name: Option<String>,
    /// Column width of the pane, used by in-place sidebar recovery to resize a
    /// freshly-split sidebar toward the session's fixed birth width.
    #[serde(default)]
    pub(super) pane_columns: Option<u64>,
    /// Column offset of the pane's left edge — `0` is the left column. Drives
    /// the tab-width extents math and the off-spec redock in sidebar recovery.
    #[serde(default)]
    pub(super) pane_x: Option<u64>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) terminal_command: Option<String>,
    #[serde(default)]
    pub(super) pane_command: Option<String>,
    #[serde(default)]
    pub(super) pane_cwd: Option<String>,
}

impl RawPane {
    /// A tiled terminal pane for geometry and sidebar reconcile: not plugin
    /// chrome, not suppressed, and not a floating overlay. Held and exited panes
    /// still occupy layout cells until Zellij closes them.
    pub(super) fn is_terminal(&self) -> bool {
        !self.is_plugin && !self.is_suppressed && !self.is_floating
    }

    /// A live terminal pane that belongs in the listing feed. Floating panes are
    /// included here because agent discovery and process presence follow visible
    /// terminals, while geometry and sidebar reconcile keep using
    /// [`Self::is_terminal`] to exclude overlays from column math.
    pub(super) fn is_listed_pane(&self) -> bool {
        !self.is_plugin && !self.is_suppressed && !self.is_held && !self.exited
    }

    /// A live tiled terminal pane. Excludes held/exited corpses so a dead command
    /// never drives sidebar recovery or geometry. Zellij can omit command fields
    /// for a live implicit shell pane, so the projection preserves that as
    /// `None`; the producer's frame rotation repairs raced-null fields from the
    /// last good observation when one exists.
    pub(super) fn is_live_terminal(&self) -> bool {
        self.is_listed_pane() && !self.is_floating
    }

    /// The live foreground command the presence plugin last observed.
    pub(super) fn foreground_command(&self) -> Option<&str> {
        self.pane_command
            .as_deref()
            .filter(|value| !value.is_empty())
    }

    /// The launch command Zellij was given when the pane was spawned.
    pub(super) fn spawn_command(&self) -> Option<&str> {
        self.terminal_command
            .as_deref()
            .filter(|command| !command.is_empty())
    }

    /// The display command the pane's `PaneRef` carries. A title-identified
    /// sidebar wins: Zellij can omit command fields for the layout pane, and it
    /// must still be filtered as chrome rather than rendered as an anonymous
    /// process row. Otherwise the foreground command decides.
    pub(super) fn display_command(&self) -> Option<String> {
        if is_sidebar_pane(self) {
            return Some(SIDEBAR_CHROME_TITLE.to_owned());
        }
        self.foreground_command().map(str::to_owned)
    }

    pub(super) fn view_position(&self) -> u64 {
        self.tab_position
    }
}

impl From<PaneTopologyPane> for RawPane {
    fn from(pane: PaneTopologyPane) -> Self {
        Self {
            id: pane.id,
            is_plugin: pane.is_plugin,
            is_held: pane.is_held,
            exited: pane.exited,
            is_suppressed: pane.is_suppressed,
            is_floating: pane.is_floating,
            is_focused: pane.is_focused,
            tab_position: pane.tab_position,
            tab_name: pane.tab_name,
            pane_columns: pane.pane_columns,
            pane_x: pane.pane_x,
            title: pane.title,
            terminal_command: pane.terminal_command,
            pane_command: pane.pane_command,
            pane_cwd: pane.pane_cwd,
        }
    }
}

#[cfg(test)]
pub(super) fn raw_panes_from_topology(cache: PaneTopologyCache) -> Vec<RawPane> {
    cache.panes.into_iter().map(Into::into).collect()
}

#[derive(Debug)]
pub(super) struct RawPaneListing {
    pub(super) panes: Vec<RawPane>,
    pub(super) observed_at_ms: u64,
    pub(super) authoritative_focus: Option<PaneId>,
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
            panes: panes.into_iter().map(Into::into).collect(),
            observed_at_ms: produced_at_ms,
            authoritative_focus: focused_pane.map(zellij_pane_id),
            client_view: clients.map(client_view_from_topology),
        }
    }

    pub(super) fn into_pane_listing(
        self,
        session_name: String,
        project: impl FnMut(RawPane, &str) -> Option<crate::pane::PaneRef>,
    ) -> PaneListing {
        let mut project = project;
        PaneListing {
            panes: self
                .panes
                .into_iter()
                .filter_map(|pane| project(pane, &session_name))
                .collect(),
            observed_at_ms: self.observed_at_ms,
            authoritative_focus: self.authoritative_focus,
            client_view: self.client_view,
        }
    }
}

fn client_view_from_topology(clients: TopologyClients) -> ClientView {
    ClientView {
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
