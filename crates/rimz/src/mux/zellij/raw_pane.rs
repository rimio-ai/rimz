//! Raw Zellij pane projection, topology-cache reads, and sidebar classification.

use std::{collections::HashSet, env};

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use super::{SIDEBAR_PANE_NAME, SIDEBAR_RESIZE_TRIGGER_PERCENT};
use crate::ids::{MuxName, PaneId, ViewId, WorkspaceId};
use crate::ledger::paths;
use crate::mux::{PaneListing, SidebarWidth, ViewSidebars};
use crate::schema::pane_topology::{PaneTopologyCache, PaneTopologyPane};

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

/// A live, non-plugin sidebar pane is one Zellij still titles with the layout's
/// [`SIDEBAR_PANE_NAME`] — the same signal `classify_session_panes` trusts.
pub(super) fn is_sidebar_pane(pane: &RawPane) -> bool {
    !pane.is_plugin && pane.title.as_deref() == Some(SIDEBAR_PANE_NAME)
}

/// Group a pane list into per-tab [`ViewSidebars`] for the reconcile planner:
/// each tab's sidebar panes (as normalized [`PaneId`]s) and whether it holds a
/// user-working terminal pane. Managed daemon hosts in `rimzd` are not work.
/// First-seen tab order; pane order within a tab preserved.
pub(super) fn views_with_sidebars(panes: &[RawPane]) -> Vec<ViewSidebars> {
    let mut views: Vec<ViewSidebars> = Vec::new();
    let mut index: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for pane in panes.iter().filter(|pane| pane.is_terminal()) {
        let slot = *index.entry(pane.tab_id).or_insert_with(|| {
            views.push(ViewSidebars {
                view: pane.tab_id.to_string(),
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

pub(super) fn tabs_with_sidebars(panes: &[RawPane]) -> std::collections::HashSet<String> {
    views_with_sidebars(panes)
        .into_iter()
        .filter(|view| !view.sidebar_panes.is_empty())
        .map(|view| view.view)
        .collect()
}

/// A managed daemon-host pane: any pane in the `rimzd` tab, or one whose spawn
/// or foreground command carries a host marker. The spawn command is the
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

/// Any non-sidebar terminal pane Zellij is holding at a "Waiting to run" prompt —
/// the fingerprint of a resurrected (serialized) room, where every command pane
/// comes back `start_suspended`. A clean rebirth has none: every command runs.
pub(super) fn has_suspended_command_pane(panes: &[RawPane]) -> bool {
    panes
        .iter()
        .any(|pane| pane.is_terminal() && !is_sidebar_pane(pane) && pane.is_held)
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
        .map(|pane| pane.tab_id)
    else {
        return Vec::new();
    };
    panes
        .iter()
        .filter(|pane| pane.tab_id == anchor_tab && pane.is_floating && !pane.is_plugin)
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

/// A tab's total width in columns: the extents (`max(pane_x + pane_columns)`)
/// over its terminal panes. A missing `pane_x` reads as `0`, degrading toward
/// the widest single pane rather than the stacked-pane-inflated sum.
pub(super) fn tab_extent_cols(panes: &[RawPane], tab_id: u64) -> u64 {
    panes
        .iter()
        .filter(|pane| pane.is_terminal() && pane.tab_id == tab_id)
        .filter_map(|pane| Some(pane.pane_x.unwrap_or(0) + pane.pane_columns?))
        .max()
        .unwrap_or(0)
}

/// Whether a sidebar `cols` wide in a `total`-wide tab is past the resize
/// trigger: wider than the configured `max_cols` cap, or under the cap but over
/// [`SIDEBAR_RESIZE_TRIGGER_PERCENT`] of the tab. A pane born fixed at the cap
/// is a deliberate width verdict even on a narrow client; anything wider asks
/// the repair path to converge it.
pub(super) fn sidebar_width_off_spec(cols: u64, total: u64, width: SidebarWidth) -> bool {
    if total == 0 {
        return false;
    }
    let cap = width.cap_cols();
    cols > cap || (cols < cap && cols * 100 > total * SIDEBAR_RESIZE_TRIGGER_PERCENT)
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
        other.tab_id == pane.tab_id
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
            pane.tab_id == sidebar.tab_id
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

pub(super) fn stackable_nested_work_pane_ids(
    sidebar: &RawPane,
    panes: &[RawPane],
    excluded: &HashSet<u64>,
) -> Option<Vec<u64>> {
    repairable_nested_work_pane_ids(sidebar, panes, excluded)
}

/// Whether a kept sidebar pane sits off the layout's dock: outside the
/// full-height left column, nested beside a tiled pane that intrudes into its
/// column band, or past the width trigger ([`sidebar_width_off_spec`]). Unknown
/// geometry never reads off-spec.
pub(super) fn sidebar_geometry_off_spec(
    pane: &RawPane,
    panes: &[RawPane],
    excluded: &HashSet<u64>,
    width: SidebarWidth,
) -> bool {
    let Some(verdict) = sidebar_dock_verdict(pane, panes, excluded) else {
        return false;
    };
    matches!(verdict, SidebarDock::SwapReachable | SidebarDock::NestedRow)
        || pane.pane_columns.is_some_and(|cols| {
            sidebar_width_off_spec(cols, tab_extent_cols(panes, pane.tab_id), width)
        })
}

/// The mounted sidebar pane an add produced: a fresh live, sidebar-titled
/// terminal pane in `tab_id` matching the stdout hint, or — the hint being
/// unreliable — the lowest fresh such id absent from the before-set, so a
/// cross-talk duplicate resolves deterministically and reconcile closes the
/// rest. A hinted pane already present before the add is never accepted.
pub(super) fn mounted_sidebar_pane(
    panes: &[RawPane],
    tab_id: u64,
    before: &std::collections::HashSet<u64>,
    hint: Option<u64>,
) -> Option<u64> {
    let ids: Vec<u64> = panes
        .iter()
        .filter(|pane| pane.tab_id == tab_id && pane.is_live_terminal() && is_sidebar_pane(pane))
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

/// Subset of fields `zellij action list-panes -j -a` emits. We deserialize
/// only what we route into `PaneRef`; serde silently ignores everything else.
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
    /// Zellij's internal tab id, used by `...-by-id` action verbs. The
    /// presence-plugin cache cannot observe it, so cache-derived raw panes set
    /// this to the tab position and only flow through read-only projection.
    pub(super) tab_id: u64,
    /// Positional tab index from `list-panes -j`. `PaneRef.view_id` uses this
    /// value so the CLI and presence-cache projections agree.
    #[serde(default)]
    pub(super) tab_position: Option<u64>,
    /// Name of the tab the pane lives in. Routed into [`PaneRef::view_name`];
    /// also how the `rimzd` daemon view is recognised on Zellij, whose pane list
    /// reports no command fields a classifier could read instead.
    #[serde(default)]
    pub(super) tab_name: Option<String>,
    /// Column width of the pane, used by in-place sidebar recovery to resize a
    /// freshly-split sidebar toward the layout's width percentage.
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
    pub(super) command: Option<String>,
    #[serde(default)]
    pub(super) pane_cwd: Option<String>,
    #[serde(default)]
    pub(super) cwd: Option<String>,
    #[serde(default)]
    pub(super) pane_pid: Option<u32>,
    #[serde(default)]
    pub(super) pid: Option<u32>,
    #[serde(default)]
    pub(super) pane_process_start: Option<Value>,
    #[serde(default)]
    pub(super) process_start: Option<Value>,
}

impl RawPane {
    /// A real tiled terminal pane: not plugin chrome, not suppressed, and not a
    /// floating overlay.
    /// The single definition of "counts as a pane" shared by the pane listing,
    /// sidebar recovery, and column math.
    pub(super) fn is_terminal(&self) -> bool {
        !self.is_plugin && !self.is_suppressed && !self.is_floating
    }

    /// A terminal pane hosting a live command. Excludes held/exited corpses so a
    /// dead command never renders a row. Zellij can omit command fields for a
    /// live implicit shell pane, so the projection preserves that as `None`; the
    /// producer's frame rotation repairs raced-null fields from the last good
    /// observation when one exists.
    pub(super) fn is_live_terminal(&self) -> bool {
        self.is_terminal() && !self.is_held && !self.exited
    }

    /// The live foreground command the pane reports. `pane_command` is the
    /// current Zellij pty-enriched field; `command` is the older field name.
    /// Present-but-empty fields fall through rather than masking the next
    /// source.
    pub(super) fn foreground_command(&self) -> Option<&str> {
        [self.pane_command.as_deref(), self.command.as_deref()]
            .into_iter()
            .flatten()
            .find(|value| !value.is_empty())
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
            return Some(SIDEBAR_PANE_NAME.to_owned());
        }
        self.foreground_command().map(str::to_owned)
    }

    /// The cwd the pane reports; `pane_cwd` wins, falling back to `cwd`, with
    /// a present-but-empty field falling through like the command ladder.
    pub(super) fn reported_cwd(&self) -> Option<&str> {
        [self.pane_cwd.as_deref(), self.cwd.as_deref()]
            .into_iter()
            .flatten()
            .find(|value| !value.is_empty())
    }

    pub(super) fn pid(&self) -> Option<u32> {
        self.pane_pid.or(self.pid)
    }

    pub(super) fn process_start(&self) -> Option<Timestamp> {
        self.pane_process_start
            .as_ref()
            .or(self.process_start.as_ref())
            .and_then(timestamp_from_json)
    }

    pub(super) fn view_position(&self) -> u64 {
        self.tab_position.unwrap_or(self.tab_id)
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
            tab_id: pane.tab_position,
            tab_position: Some(pane.tab_position),
            tab_name: pane.tab_name,
            pane_columns: pane.pane_columns,
            pane_x: pane.pane_x,
            title: pane.title,
            terminal_command: pane.terminal_command,
            pane_command: pane.pane_command,
            command: None,
            pane_cwd: None,
            cwd: None,
            pane_pid: None,
            pid: None,
            pane_process_start: None,
            process_start: None,
        }
    }
}

pub(super) fn raw_panes_from_topology(cache: PaneTopologyCache) -> Vec<RawPane> {
    cache.panes.into_iter().map(Into::into).collect()
}

#[derive(Debug)]
pub(super) struct RawPaneListing {
    pub(super) panes: Vec<RawPane>,
    pub(super) observed_at_ms: u64,
    pub(super) source_active: std::collections::BTreeMap<ViewId, PaneId>,
}

impl RawPaneListing {
    pub(super) fn from_cli(panes: Vec<RawPane>, observed_at_ms: u64) -> Self {
        Self {
            panes,
            observed_at_ms,
            source_active: std::collections::BTreeMap::new(),
        }
    }

    pub(super) fn from_topology(cache: PaneTopologyCache) -> Self {
        let observed_at_ms = cache.produced_at_ms;
        let source_active = cache
            .active_panes
            .iter()
            .map(|(tab_position, pane_id)| {
                (
                    ViewId::new_unchecked(format!("tab_{tab_position}")),
                    PaneId::from_parts(MuxName::Zellij, format!("terminal_{pane_id}")),
                )
            })
            .collect();
        Self {
            panes: raw_panes_from_topology(cache),
            observed_at_ms,
            source_active,
        }
    }

    pub(super) fn into_pane_listing(
        self,
        session_name: String,
        project: impl FnMut(RawPane, &str) -> Option<crate::feed::PaneRef>,
    ) -> PaneListing {
        let mut project = project;
        PaneListing {
            panes: self
                .panes
                .into_iter()
                .filter_map(|pane| project(pane, &session_name))
                .collect(),
            observed_at_ms: self.observed_at_ms,
            source_active: self.source_active,
        }
    }
}

pub(super) fn read_fresh_topology_cache(
    session: &str,
    workspace_id: &WorkspaceId,
    min_produced_at_ms: Option<u64>,
) -> Option<RawPaneListing> {
    let runtime = paths::RuntimePaths::for_workspace(workspace_id.clone()).ok()?;
    crate::sidebar::cache::read_fresh_pane_topology_cache(&runtime, session, min_produced_at_ms)
        .map(RawPaneListing::from_topology)
}

pub(super) fn timestamp_from_json(value: &Value) -> Option<Timestamp> {
    if let Some(seconds) = value.as_i64() {
        return Timestamp::from_second(seconds).ok();
    }
    if let Some(raw) = value.as_str() {
        if let Ok(seconds) = raw.parse::<i64>() {
            return Timestamp::from_second(seconds).ok();
        }
        return raw.parse::<Timestamp>().ok();
    }
    None
}

#[cfg(test)]
mod tests;
