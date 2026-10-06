//! Zellij [`MuxBackend`](crate::mux::MuxBackend) trait implementation.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::super::mount_proof::{prove_sidebar_mount, sidebar_build_identity};
use super::layout::{TempLayoutFile, render_background_view_layout, render_tab_layout};
use super::pane_topology::{
    PaneTopologyCache, PaneTopologyPane, ZellijPaneId, read_pane_topology_cache,
};
use super::parse::{
    classify_session_not_found, is_no_active_sessions, is_session_not_found, is_transient_empty,
    live_session_name_from_line, parse_client_view, trim_capture,
};
use super::raw_pane::{
    floating_panes_in_anchor_view, is_daemon_host_pane, is_sidebar_pane, sidebar_geometry_off_spec,
    tab_fullscreen_active, tab_view_cols,
};
use super::sidebar::DockOutcome;
use super::{
    HEALTH_PROBE_RETRY_DELAY, RECONCILE_LIST_TIMEOUT, ZellijBackend, env_prefixed, output_error,
};
use crate::disk::paths::RuntimePaths;
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::mux::companion_layout::{GridPane, balance, plan_append};
use crate::mux::tab_name::TabNameIntent;
use crate::mux::{
    BackgroundViewLaunch, BackgroundViewOptions, CachedPaneRoster, ClientFocusOptions, ClientView,
    CommandSpec, CompanionPaneAppend, DaemonView, MuxBackend, MuxErr, PaneCapture, PaneListOptions,
    PaneListing, ReconcileAddOutcome, ReconcilePane, ReconcilePaneRole, Result, ResumeTab,
    ResumeTabShape, ResumeTabUnconfirmed, SessionHealth, SessionLiveness, SessionOptions,
    SidebarLiveness, SidebarPaneOptions, SidebarRecovery, SplitDirection, SplitPaneOptions,
    SplitPlacement, SplitTarget, TabOptions, WidthStep, confirm_resume_tab_shapes,
    ensure_pane_backend, execute_reconcile_plan, group_reconcile_panes, memoized_version,
};
use crate::pane::keys::{BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN, NamedKey, paste_payload};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct RawTab {
    pub(super) name: String,
    #[serde(default)]
    selectable_tiled_panes_count: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawListedPane {
    pub(super) id: u64,
    #[serde(default)]
    pub(super) is_plugin: bool,
    #[serde(default)]
    is_fullscreen: bool,
    #[serde(default)]
    is_held: bool,
    #[serde(default)]
    exited: bool,
    #[serde(default)]
    is_suppressed: bool,
    #[serde(default)]
    is_floating: bool,
    /// Stable Zellij tab id accepted by `new-pane --tab-id`.
    #[serde(default)]
    tab_id: Option<u64>,
    /// Current on-screen tab position. Every supported Zellij emits it; a
    /// listing without it falls back to `tab_id`.
    #[serde(default)]
    tab_position: Option<u64>,
    #[serde(default)]
    tab_name: Option<String>,
    #[serde(default)]
    pane_columns: Option<u64>,
    #[serde(default)]
    pane_x: Option<u64>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    terminal_command: Option<String>,
    /// Live foreground command, falling back to the pane shell, that Zellij
    /// reads from the pane's pty for every listing.
    #[serde(default)]
    pane_command: Option<String>,
    #[serde(default)]
    pane_cwd: Option<String>,
}

impl From<RawListedPane> for PaneTopologyPane {
    fn from(pane: RawListedPane) -> Self {
        Self {
            id: pane.id,
            is_plugin: pane.is_plugin,
            is_fullscreen: pane.is_fullscreen,
            is_held: pane.is_held,
            exited: pane.exited,
            is_suppressed: pane.is_suppressed,
            is_floating: pane.is_floating,
            tab_position: pane.tab_position.or(pane.tab_id).unwrap_or_default(),
            tab_name: pane.tab_name,
            pane_columns: pane.pane_columns,
            pane_x: pane.pane_x,
            title: pane.title,
            pane_command: pane.pane_command.filter(|command| !command.is_empty()),
            pane_cwd: pane.pane_cwd.filter(|cwd| !cwd.is_empty()),
            pane_pid: None,
            terminal_command: pane.terminal_command,
        }
    }
}

fn merge_topology_enrichment(cache: &mut PaneTopologyCache, prior: PaneTopologyCache) {
    let enrichment = prior
        .panes
        .into_iter()
        .map(|pane| {
            (
                pane.native_id(),
                (
                    pane.pane_command,
                    pane.pane_cwd,
                    pane.pane_pid,
                    pane.pane_columns,
                    pane.pane_x,
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    for pane in &mut cache.panes {
        let Some((command, cwd, pid, columns, x)) = enrichment.get(&pane.native_id()) else {
            continue;
        };
        if pane.pane_command.is_none() {
            pane.pane_command.clone_from(command);
        }
        if pane.pane_cwd.is_none() {
            pane.pane_cwd.clone_from(cwd);
        }
        if pane.pane_pid.is_none() {
            pane.pane_pid = *pid;
        }
        if pane.pane_columns.is_none() {
            pane.pane_columns = *columns;
        }
        if pane.pane_x.is_none() {
            pane.pane_x = *x;
        }
    }
}

fn deadline_remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|time| !time.is_zero())
}

fn split_direction(direction: SplitDirection) -> &'static str {
    match direction {
        SplitDirection::Right => "right",
        SplitDirection::Down => "down",
    }
}

fn confirm_tab_action(mut probe: impl FnMut() -> Result<bool>) -> Result<bool> {
    for attempt in 0..super::FOCUS_RESTORE_ATTEMPTS {
        if probe()? {
            return Ok(true);
        }
        if attempt + 1 < super::FOCUS_RESTORE_ATTEMPTS {
            std::thread::sleep(super::FOCUS_RESTORE_RETRY_DELAY);
        }
    }
    Ok(false)
}

fn companion_grid_preserved(
    before: &[GridPane],
    chrome: &[GridPane],
    panes: &[GridPane],
    current_chrome: &[GridPane],
) -> bool {
    current_chrome == chrome
        && before
            .iter()
            .all(|old| panes.iter().any(|pane| pane.pane_id == old.pane_id))
}

struct CompanionStep {
    distance: u64,
    direction: SplitDirection,
    increase: bool,
    pane: PaneId,
}

fn companion_step(
    panes: &[GridPane],
    targets: &[GridPane],
    right: u64,
    bottom: u64,
) -> (u64, Option<CompanionStep>) {
    use SplitDirection::{Down, Right};

    let mut steps = Vec::new();
    for target in targets {
        // balance derives every target from this same pane set.
        let pane = panes
            .iter()
            .find(|pane| pane.pane_id == target.pane_id)
            .expect("balance target belongs to the grid");
        for (direction, edge, desired, outer) in [
            (Right, pane.x + pane.cols, target.x + target.cols, right),
            (Down, pane.y + pane.rows, target.y + target.rows, bottom),
        ] {
            if edge != outer {
                steps.push(CompanionStep {
                    distance: edge.abs_diff(desired),
                    direction,
                    increase: edge < desired,
                    pane: pane.pane_id.clone(),
                });
            }
        }
    }
    let error = steps.iter().map(|step| step.distance).sum();
    (error, steps.into_iter().max_by_key(|step| step.distance))
}

impl ZellijBackend {
    fn supports_no_focus(&self) -> bool {
        self.version()
            .ok()
            .as_deref()
            .and_then(super::parse_version)
            .is_some_and(|version| version >= super::MIN_NO_FOCUS_ZELLIJ_VERSION)
    }

    fn companion_geometry(
        &self,
        session: &str,
        anchor: &PaneId,
        timeout: Duration,
    ) -> Result<Option<(Vec<GridPane>, Vec<GridPane>)>> {
        #[derive(Deserialize)]
        struct Geometry {
            #[serde(flatten)]
            pane: RawListedPane,
            pane_y: Option<u64>,
            pane_rows: Option<u64>,
        }
        let output = self
            .zellij_action(session)
            .args(["list-panes", "--all", "--json"])
            .run_with_timeout(timeout)?;
        let listed: Vec<Geometry> = serde_json::from_slice(&output.stdout)
            .map_err(|err| output_error(format!("parsing companion geometry: {err}")))?;
        let native = ZellijPaneId::try_from(anchor)
            .ok()
            .and_then(ZellijPaneId::terminal_id);
        let Some(tab) = listed
            .iter()
            .find(|item| !item.pane.is_plugin && Some(item.pane.id) == native)
            .and_then(|item| item.pane.tab_id)
        else {
            return Ok(None);
        };
        let mut work = Vec::new();
        let mut chrome = Vec::new();
        for item in listed
            .into_iter()
            .filter(|item| item.pane.tab_id == Some(tab))
        {
            let pane = item.pane;
            if pane.is_fullscreen || pane.is_suppressed {
                return Ok(None);
            }
            let (Some(x), Some(y), Some(cols), Some(rows)) =
                (pane.pane_x, item.pane_y, pane.pane_columns, item.pane_rows)
            else {
                return Ok(None);
            };
            let grid = GridPane {
                pane_id: PaneId::from_parts(
                    MuxName::Zellij,
                    format!(
                        "{}_{id}",
                        if pane.is_plugin { "plugin" } else { "terminal" },
                        id = pane.id
                    ),
                ),
                x,
                y,
                cols,
                rows,
            };
            if pane.is_plugin
                || pane.is_floating
                || pane.title.as_deref() == Some(crate::pane::SIDEBAR_CHROME_TITLE)
            {
                chrome.push(grid);
            } else {
                work.push(grid);
            }
        }
        work.sort_by(|a, b| a.pane_id.as_str().cmp(b.pane_id.as_str()));
        chrome.sort_by(|a, b| a.pane_id.as_str().cmp(b.pane_id.as_str()));
        Ok(Some((work, chrome)))
    }

    fn balance_companion(
        &self,
        session: &str,
        anchor: &PaneId,
        before: &[GridPane],
        chrome: &[GridPane],
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut previous_error = None;
        let mut confirmed_targets = None;
        let mut last_move: Option<CompanionStep> = None;
        let bounds = |panes: &[GridPane]| {
            (
                panes.iter().map(|pane| pane.x).min(),
                panes.iter().map(|pane| pane.y).min(),
                panes.iter().map(|pane| pane.x + pane.cols).max(),
                panes.iter().map(|pane| pane.y + pane.rows).max(),
            )
        };
        let before_bounds = bounds(before);
        while let Some(remaining) = deadline_remaining(deadline) {
            let Some((panes, current_chrome)) =
                self.companion_geometry(session, anchor, remaining)?
            else {
                break;
            };
            if !companion_grid_preserved(before, chrome, &panes, &current_chrome) {
                break;
            }
            if panes == before {
                std::thread::sleep(Duration::from_millis(20).min(remaining));
                continue;
            }
            if panes.len() != before.len() + 1 {
                break;
            }
            if bounds(&panes) != before_bounds {
                break;
            }
            let Some(targets) = balance(&panes, 0) else {
                break;
            };
            if confirmed_targets
                .as_ref()
                .is_some_and(|confirmed| confirmed != &targets)
            {
                break;
            }
            confirmed_targets = Some(targets.clone());
            let (_, _, right, bottom) = before_bounds;
            let (error, step) =
                companion_step(&panes, &targets, right.unwrap_or(0), bottom.unwrap_or(0));
            if let Some(previous) = previous_error
                && error >= previous
            {
                // A native step coarser than the remaining distance overshot:
                // undo it so the grid keeps its closer shape.
                if error > previous
                    && let Some(mut step) = last_move
                {
                    step.increase = !step.increase;
                    self.resize_boundary(session, anchor, &panes, chrome, &step, deadline)?;
                }
                break;
            }
            previous_error = Some(error);
            let Some(step) = step else {
                break;
            };
            if step.distance == 0
                || !self.resize_boundary(session, anchor, &panes, chrome, &step, deadline)?
            {
                break;
            }
            last_move = Some(step);
        }
        Ok(())
    }

    // Returns false once the grid changed under us or the deadline passed.
    fn resize_boundary(
        &self,
        session: &str,
        anchor: &PaneId,
        panes: &[GridPane],
        chrome: &[GridPane],
        step: &CompanionStep,
        deadline: Instant,
    ) -> Result<bool> {
        let Some(moved) = panes
            .iter()
            .find(|candidate| candidate.pane_id == step.pane)
        else {
            return Ok(false);
        };
        let edge = moved.x + moved.cols;
        // A column boundary can span several independently resizable rows.
        // Move every still-unmoved segment before evaluating the grid again:
        // after only one segment the intermediate shape is not a column.
        // Native resizing sometimes moves the entire aligned boundary, so
        // re-read before each following segment instead of applying twice.
        let boundary = if step.direction == SplitDirection::Right {
            panes
                .iter()
                .filter(|pane| pane.x + pane.cols == edge)
                .map(|pane| pane.pane_id.clone())
                .collect::<Vec<_>>()
        } else {
            vec![step.pane.clone()]
        };
        for (index, pane) in boundary.into_iter().enumerate() {
            let Some(remaining) = deadline_remaining(deadline) else {
                return Ok(false);
            };
            if index > 0 {
                let Some((current, current_chrome)) =
                    self.companion_geometry(session, anchor, remaining)?
                else {
                    return Ok(false);
                };
                if current.len() != panes.len()
                    || !companion_grid_preserved(panes, chrome, &current, &current_chrome)
                {
                    return Ok(false);
                }
                let Some(current_pane) = current.iter().find(|candidate| candidate.pane_id == pane)
                else {
                    return Ok(false);
                };
                if current_pane.x + current_pane.cols != edge {
                    continue;
                }
            }
            let Some(remaining) = deadline_remaining(deadline) else {
                return Ok(false);
            };
            self.zellij_action(session)
                .args([
                    "resize",
                    if step.increase {
                        "increase"
                    } else {
                        "decrease"
                    },
                    split_direction(step.direction),
                    "--pane-id",
                    pane.raw(),
                ])
                .run_with_timeout(remaining)?;
        }
        Ok(true)
    }

    fn restore_background_split_focus(
        &self,
        placement: SplitPlacement,
        focus: bool,
        session_name: &str,
        workspace_id: Option<WorkspaceId>,
        target_pane: Option<&PaneId>,
        restore: Option<&PaneId>,
    ) {
        if let Some(restore) = restore {
            if let Some(workspace_id) = workspace_id.as_ref() {
                let _ = self.restore_attached_client_focus(session_name, workspace_id, restore);
            } else {
                let _ = self.focus_pane(restore, Some(session_name));
            }
            return;
        }
        if placement == SplitPlacement::Stacked || focus {
            return;
        }
        let Some(target_pane) = target_pane else {
            return;
        };
        if let Some(workspace_id) = workspace_id
            && let Ok(runtime) = self.runtime_paths_for_workspace(workspace_id)
        {
            let _ = execute_focus_restoration(
                self,
                &runtime,
                session_name,
                target_pane,
                None,
                crate::mux::focus_anchor::FocusDispatchRetries::default(),
            );
        } else {
            let _ = self.focus_pane(target_pane, Some(session_name));
        }
    }

    pub(super) fn tab_id_for_pane(&self, session_name: &str, pane: &PaneId) -> Result<u64> {
        self.tab_id_for_pane_within(session_name, pane, super::super::COMMAND_TIMEOUT)
    }

    fn tab_id_for_pane_within(
        &self,
        session_name: &str,
        pane: &PaneId,
        timeout: Duration,
    ) -> Result<u64> {
        self.tab_for_pane_within(session_name, pane, timeout)
            .map(|(tab_id, _)| tab_id)
    }

    /// The stable id and current name of the tab holding `pane`, from one
    /// listing.
    fn tab_for_pane_within(
        &self,
        session_name: &str,
        pane: &PaneId,
        timeout: Duration,
    ) -> Result<(u64, Option<String>)> {
        let pane_id = ZellijPaneId::try_from(pane)
            .ok()
            .and_then(ZellijPaneId::terminal_id)
            .ok_or_else(|| {
                output_error(format!("target pane `{pane}` has no numeric Zellij id"))
            })?;
        let listed = self.raw_listed_panes(session_name, timeout)?;
        listed
            .into_iter()
            .find(|candidate| !candidate.is_plugin && candidate.id == pane_id)
            .and_then(|candidate| {
                let tab_id = candidate.tab_id.or(candidate.tab_position)?;
                Some((tab_id, candidate.tab_name))
            })
            .ok_or_else(|| {
                output_error(format!(
                    "target pane `{pane}` is absent from session `{session_name}`"
                ))
            })
    }

    pub(super) fn raw_listed_panes(
        &self,
        session_name: &str,
        timeout: Duration,
    ) -> Result<Vec<RawListedPane>> {
        let spec = self
            .zellij_action(session_name)
            .args(["list-panes", "--all", "--json"]);
        let deadline = Instant::now() + timeout;
        let mut remaining = timeout;
        for attempt in 0..super::TRANSIENT_EMPTY_ATTEMPTS {
            if attempt > 0 {
                let Some(wait_budget) = deadline_remaining(deadline) else {
                    break;
                };
                std::thread::sleep(super::TRANSIENT_EMPTY_RETRY_DELAY.min(wait_budget));
                let Some(next_budget) = deadline_remaining(deadline) else {
                    break;
                };
                remaining = next_budget;
            }
            let output = spec.run_with_timeout(remaining).map_err(|err| match err {
                // A rerun runs on what is left of the budget; the caller reads its own bound.
                MuxErr::Timeout { program, args, .. } => MuxErr::Timeout {
                    program,
                    args,
                    seconds: timeout.as_secs(),
                },
                err => err,
            })?;
            if !is_transient_empty(&output.stdout) {
                return serde_json::from_slice(&output.stdout).map_err(|err| {
                    output_error(format!("parsing `list-panes --all --json`: {err}"))
                });
            }
        }
        Err(output_error(
            "`list-panes --all --json` returned no output on every attempt",
        ))
    }

    fn live_session_health(&self, name: &str) -> SessionHealth {
        self.live_session_health_within(name, self.health_probe_timeout())
    }

    fn live_session_health_within(&self, name: &str, budget: Duration) -> SessionHealth {
        let deadline = Instant::now() + budget;
        let mut remaining = budget;
        let last_error = loop {
            match self.raw_listed_panes(name, remaining) {
                Ok(_) => return SessionHealth::Healthy,
                Err(err) => {
                    let Some(wait_budget) = deadline_remaining(deadline) else {
                        break err;
                    };
                    std::thread::sleep(HEALTH_PROBE_RETRY_DELAY.min(wait_budget));
                    let Some(next_budget) = deadline_remaining(deadline) else {
                        break err;
                    };
                    remaining = next_budget;
                }
            }
        };
        tracing::warn!(
            session = %name,
            tags.operation = "zellij.session_health",
            error = &last_error as &dyn std::error::Error,
            "live zellij room did not answer the native health probe",
        );
        SessionHealth::Unresponsive
    }

    pub(super) fn authoritative_pane_listing(
        &self,
        session_name: &str,
        runtime_paths: Option<&RuntimePaths>,
        workspace_id: Option<&WorkspaceId>,
        timeout: Duration,
    ) -> Result<PaneTopologyCache> {
        let observed_at_ms = crate::utils::time::unix_now_ms();
        let listed = self.raw_listed_panes(session_name, timeout)?;
        let mut cache = PaneTopologyCache {
            session_name: session_name.to_owned(),
            produced_at_ms: observed_at_ms,
            writer: None,
            focused_pane: None,
            clients: None,
            panes: listed.into_iter().map(Into::into).collect(),
        };
        let runtime = runtime_paths
            .cloned()
            .or_else(|| workspace_id.and_then(|id| self.runtime_paths_for_authoritative(id)));
        if let Some(runtime) = runtime
            && let Some(prior) = read_pane_topology_cache(&runtime, session_name)
        {
            merge_topology_enrichment(&mut cache, prior);
        }
        for pane in &mut cache.panes {
            if pane
                .pane_command
                .as_deref()
                .is_some_and(crate::pane::command_is_launch_chrome)
            {
                pane.pane_command = None;
            }
        }
        Ok(cache)
    }

    fn runtime_paths_for_authoritative(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Option<crate::disk::paths::RuntimePaths> {
        self.runtime_paths_for_workspace(workspace_id.clone()).ok()
    }

    fn focus_restore_target(
        &self,
        session_name: &str,
        workspace_id: Option<&WorkspaceId>,
    ) -> Option<PaneId> {
        let mut viewed = self
            .client_view(ClientFocusOptions {
                session_name: Some(session_name.to_owned()),
                command_timeout: None,
            })
            .map(|view| view.viewed_panes)
            .ok()?;
        viewed.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        viewed.dedup();
        let [pane] = viewed.as_slice() else {
            return None;
        };
        let panes = match workspace_id {
            Some(workspace_id) => self
                .topology_panes_for_workspace(
                    session_name,
                    workspace_id,
                    Some(crate::utils::time::unix_now_ms()),
                    super::super::COMMAND_TIMEOUT,
                )
                .ok()?,
            None => self
                .raw_listed_panes(session_name, super::super::COMMAND_TIMEOUT)
                .ok()?
                .into_iter()
                .map(PaneTopologyPane::from)
                .collect(),
        };
        panes.iter().find_map(|candidate| {
            (candidate.is_live_terminal() && parse_zellij_raw(pane) == Some(candidate.id))
                .then_some(pane.clone())
        })
    }

    fn restore_attached_client_focus(
        &self,
        session_name: &str,
        workspace_id: &WorkspaceId,
        restore: &PaneId,
    ) -> Result<()> {
        let pane_id = parse_zellij_raw(restore).ok_or_else(|| {
            output_error(format!("focus restore pane `{restore}` has no terminal id"))
        })?;
        let tab_position = self
            .raw_listed_panes(session_name, super::super::COMMAND_TIMEOUT)?
            .into_iter()
            .map(PaneTopologyPane::from)
            .find(|pane| pane.id == pane_id && pane.is_live_terminal())
            .map(|pane| pane.tab_position)
            .ok_or_else(|| {
                output_error(format!("focus restore pane `{restore}` is no longer live"))
            })?;
        let runtime = self.runtime_paths_for_workspace(workspace_id.clone())?;
        execute_focus_restoration(
            self,
            &runtime,
            session_name,
            restore,
            Some(tab_position),
            crate::mux::focus_anchor::FocusDispatchRetries {
                attempts: super::FOCUS_RESTORE_ATTEMPTS,
                delay: super::FOCUS_RESTORE_RETRY_DELAY,
            },
        )
        .map_err(output_error)
    }

    fn move_new_tab_after(&self, session: &str, anchor: &PaneId) -> Result<()> {
        ensure_pane_backend(anchor, MuxName::Zellij)?;
        let panes: Vec<PaneTopologyPane> = self
            .raw_listed_panes(session, RECONCILE_LIST_TIMEOUT)?
            .into_iter()
            .map(Into::into)
            .collect();
        let anchor_id = parse_zellij_raw(anchor)
            .ok_or_else(|| output_error(format!("tab anchor `{anchor}` has no terminal id")))?;
        let anchor_position = panes
            .iter()
            .find(|pane| pane.id == anchor_id && pane.is_live_terminal())
            .map(|pane| pane.tab_position)
            .ok_or_else(|| {
                output_error(format!(
                    "tab anchor `{anchor}` is absent from session `{session}`"
                ))
            })?;
        let tab_count = self.list_tabs(session)?.len() as u64;
        let last_position = tab_count.saturating_sub(1);
        let new_pane_id = panes
            .iter()
            .find(|pane| {
                pane.tab_position == last_position
                    && pane.is_live_terminal()
                    && !is_sidebar_pane(pane)
            })
            .map(|pane| pane.id)
            .ok_or_else(|| output_error("new tab has no live work pane"))?;
        let new_pane = PaneId::from(ZellijPaneId::Terminal(new_pane_id));
        let new_tab_id = self.tab_id_for_pane(session, &new_pane)?;
        let move_count = moves_to_place_after(anchor_position, tab_count);
        for completed in 0..move_count {
            self.zellij_action(session)
                .args(["move-tab", "left", "--tab-id", &new_tab_id.to_string()])
                .run()?;
            let expected_position = last_position - completed - 1;
            let moved = confirm_tab_action(|| {
                Ok(self
                    .raw_listed_panes(session, RECONCILE_LIST_TIMEOUT)?
                    .into_iter()
                    .map(PaneTopologyPane::from)
                    .find(|pane| pane.id == new_pane_id && pane.is_live_terminal())
                    .is_some_and(|pane| pane.tab_position == expected_position))
            })?;
            if !moved {
                return Err(output_error(
                    "new tab did not move to the requested position",
                ));
            }
        }
        Ok(())
    }

    fn run_new_tab_confirmed(&self, session: &str, args: &[String], tab_name: &str) -> Result<()> {
        let tabs = self.list_tabs(session)?;
        let config = crate::config::MachineConfig::load_lenient();
        let theme = &config.theme;
        let (before, before_materialized) = named_tab_counts(&tabs, tab_name, theme);
        let mut created_tab = None;
        for attempt in 0..super::NEW_TAB_ATTEMPTS {
            if attempt > 0 {
                let tabs = self
                    .list_tabs(session)
                    .inspect_err(|_| self.close_unconfirmed_tab(session, created_tab))?;
                let (named, materialized) = named_tab_counts(&tabs, tab_name, theme);
                if named > before {
                    self.wait_for_named_tab_materialized(
                        session,
                        tab_name,
                        before_materialized,
                        materialized,
                        theme,
                    )
                    .inspect_err(|_| self.close_unconfirmed_tab(session, created_tab))?;
                    return Ok(());
                }
            }
            let output = self
                .zellij_action(session)
                .args(args.iter().cloned())
                .run()?;
            created_tab = String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<u64>()
                .ok();
            let deadline = Instant::now() + super::NEW_TAB_CONFIRM_WINDOW;
            loop {
                let tabs = self
                    .list_tabs(session)
                    .inspect_err(|_| self.close_unconfirmed_tab(session, created_tab))?;
                let (named, materialized) = named_tab_counts(&tabs, tab_name, theme);
                if named > before {
                    self.wait_for_named_tab_materialized(
                        session,
                        tab_name,
                        before_materialized,
                        materialized,
                        theme,
                    )
                    .inspect_err(|_| self.close_unconfirmed_tab(session, created_tab))?;
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(super::NEW_TAB_CONFIRM_STEP);
            }
        }
        self.close_unconfirmed_tab(session, created_tab);
        Err(output_error(format!(
            "new-tab '{tab_name}' did not appear after {} attempts",
            super::NEW_TAB_ATTEMPTS
        )))
    }

    fn close_unconfirmed_tab(&self, session: &str, tab: Option<u64>) {
        // Never close a namesake: new-tab returns the stable id it created.
        if let Some(tab) = tab
            && let Err(error) = self
                .zellij_action(session)
                .args(["close-tab", "--tab-id", &tab.to_string()])
                .run()
        {
            tracing::warn!(%session, tab, %error, "could not close unconfirmed tab");
        }
    }

    fn wait_for_named_tab_materialized(
        &self,
        session: &str,
        tab_name: &str,
        before_materialized: usize,
        mut last_count: usize,
        theme: &crate::config::ThemeConfig,
    ) -> Result<()> {
        let deadline = Instant::now() + super::NEW_TAB_MATERIALIZE_WINDOW;
        loop {
            if last_count > before_materialized {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(output_error(format!(
                    "new-tab '{tab_name}' appeared but its layout panes did not materialize; \
                         materialized named tabs stayed at {last_count}"
                )));
            }
            std::thread::sleep(super::NEW_TAB_MATERIALIZE_STEP);
            last_count = named_tab_counts(&self.list_tabs(session)?, tab_name, theme).1;
        }
    }

    pub(super) fn list_tabs(&self, session: &str) -> Result<Vec<RawTab>> {
        for attempt in 0..super::TRANSIENT_EMPTY_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(super::TRANSIENT_EMPTY_RETRY_DELAY);
            }
            let output = self
                .zellij_action(session)
                .args(["list-tabs", "--json", "--panes"])
                .run()
                .map_err(|err| classify_session_not_found(err, session))?;
            if is_session_not_found(&output.stdout) || is_session_not_found(&output.stderr) {
                return Err(MuxErr::SessionNotFound {
                    session: session.to_owned(),
                });
            }
            if is_transient_empty(&output.stdout) {
                continue;
            }
            let tabs = serde_json::from_slice::<Vec<RawTab>>(&output.stdout)
                .map_err(|e| output_error(format!("parsing list-tabs JSON: {e}")))?;
            if tabs.is_empty() {
                continue;
            }
            return Ok(tabs);
        }
        Err(output_error(format!(
            "list-tabs returned no output after {} attempts",
            super::TRANSIENT_EMPTY_ATTEMPTS
        )))
    }
}

/// Each live tab's name and its work panes: the terminal panes still running,
/// besides the sidebar and floating overlays.
pub(super) fn live_tab_shapes(
    panes: &[RawListedPane],
    theme: &crate::config::ThemeConfig,
) -> Vec<ResumeTabShape> {
    let mut tabs: Vec<(u64, ResumeTabShape)> = Vec::new();
    for pane in panes {
        let (Some(tab), Some(name)) = (pane.tab_id.or(pane.tab_position), pane.tab_name.as_deref())
        else {
            continue;
        };
        let work = usize::from(
            !pane.is_plugin
                && !pane.is_floating
                && !pane.exited
                && pane.title.as_deref() != Some(crate::pane::SIDEBAR_CHROME_TITLE),
        );
        match tabs.iter_mut().find(|(id, _)| *id == tab) {
            Some((_, shape)) => shape.panes += work,
            None => tabs.push((
                tab,
                ResumeTabShape {
                    name: crate::theme::strip_status_glyph_suffix(name, theme).to_owned(),
                    panes: work,
                },
            )),
        }
    }
    tabs.into_iter().map(|(_, shape)| shape).collect()
}

fn named_tab_counts(
    tabs: &[RawTab],
    tab_name: &str,
    theme: &crate::config::ThemeConfig,
) -> (usize, usize) {
    tabs.iter()
        .filter(|tab| crate::theme::strip_status_glyph_suffix(&tab.name, theme) == tab_name)
        .fold((0, 0), |(named, materialized), tab| {
            (
                named + 1,
                materialized + usize::from(tab.selectable_tiled_panes_count > 0),
            )
        })
}

impl MuxBackend for ZellijBackend {
    fn name(&self) -> MuxName {
        MuxName::Zellij
    }

    fn ensure_session(&self, _opts: &SessionOptions) -> Result<()> {
        // Zellij creates sessions lazily, and `open_sidebar` owns first birth
        // by rendering the session from a layout (Zellij applies a layout only
        // at session creation). There is nothing to pre-create here.
        Ok(())
    }

    fn attach_command(&self, name: &str, config: &crate::config::MultiplexerConfig) -> CommandSpec {
        self.cmd()
            .args([
                "attach".to_owned(),
                "--create".to_owned(),
                name.to_owned(),
                "options".to_owned(),
            ])
            .args(super::zellij_client_options_args(&config.zellij))
    }

    fn attach_existing_command(&self, name: &str) -> CommandSpec {
        self.cmd().args(["attach", name])
    }

    fn attach_readonly_command(&self, name: &str) -> CommandSpec {
        // Zellij has no read-only attach; broadcast ttyd drops all client input.
        self.attach_existing_command(name)
    }

    fn detach(&self, _name: &str) -> Result<()> {
        // Zellij detaches a client of the caller's own session, whatever room
        // `_name` is: naming another session would detach someone else's.
        let session = self.resolve_session(None)?;
        self.zellij_action(&session).arg("detach").run().map(|_| ())
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        self.delete_session(name)
    }

    fn list_sessions_within(&self, timeout: std::time::Duration) -> Result<Vec<String>> {
        let output = match self.cmd().arg("list-sessions").run_with_timeout(timeout) {
            Ok(output) => output,
            Err(MuxErr::Command { ref stderr, .. }) if is_no_active_sessions(stderr.as_bytes()) => {
                return Ok(Vec::new());
            }
            Err(err) => return Err(err),
        };
        // Output lines look like `name [Created Ns ago]` for live sessions, or
        // `name [Created Ns ago] (EXITED - attach to resurrect)` for stopped
        // sessions. `list_sessions` is the live-session set used by `rimz list`
        // and `rimz reload`, so filter resurrectable corpses out here.
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(live_session_name_from_line)
            .collect())
    }

    fn session_liveness(&self, name: &str) -> Result<SessionLiveness> {
        self.session_state_checked(name)
    }

    fn cached_pane_roster(
        &self,
        session: &str,
        workspace_id: &WorkspaceId,
    ) -> Option<CachedPaneRoster> {
        let runtime = self.runtime_paths_for_authoritative(workspace_id)?;
        let cache = Self::fresh_cached_topology(
            &runtime,
            session,
            crate::utils::time::unix_now_ms(),
            None,
        )?;
        Some(CachedPaneRoster {
            pane_ids: cache
                .panes
                .into_iter()
                .filter(|pane| !pane.is_plugin)
                .map(|pane| PaneId::from(pane.native_id()))
                .collect(),
            observed_at_ms: cache.produced_at_ms,
        })
    }

    fn list_panes(&self, opts: PaneListOptions) -> Result<PaneListing> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let session_name = opts.session_name.unwrap_or_default();
        self.read_topology(
            (!session_name.is_empty()).then_some(session_name.as_str()),
            opts.runtime_paths.as_ref(),
            opts.workspace_id.as_ref(),
            opts.min_topology_produced_at_ms,
            opts.consistency,
            timeout,
        )
        .map(|cache| cache.into_pane_listing(session_name))
    }

    fn client_view(&self, opts: ClientFocusOptions) -> Result<ClientView> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let session = self.resolve_session(opts.session_name.as_deref())?;
        let output = self
            .zellij_action(&session)
            .arg("list-clients")
            .run_with_timeout(timeout)?;
        Ok(parse_client_view(&output.stdout))
    }

    fn split_pane(&self, opts: SplitPaneOptions) -> Result<Option<PaneId>> {
        let target = opts.target;
        // Resolved once, before anything is spawned: the split and both of its
        // focus helpers address this one session.
        let session = self.resolve_session(target.session_name())?;
        // Workspace-scoped focus restoration belongs to a target that names
        // the room; the caller's own session may be another room's.
        let focus_workspace = target
            .session_name()
            .and(opts.env.get(crate::workspace::ENV_WORKSPACE_ID))
            .and_then(|value| value.parse::<WorkspaceId>().ok());
        let target_pane = target.pane_id();
        let mut spec = self.zellij_action(&session).arg("new-pane");
        if let Some(target_pane) = target_pane {
            ensure_pane_backend(target_pane, MuxName::Zellij)?;
            let pane_id = ZellijPaneId::try_from(target_pane)
                .map_err(output_error)?
                .terminal_id()
                .ok_or_else(|| {
                    output_error(format!(
                        "target pane `{target_pane}` is not a terminal pane"
                    ))
                })?;
            spec = spec.env("ZELLIJ_PANE_ID", pane_id.to_string());
        }
        let anchored_stack = opts.placement == SplitPlacement::Stacked && target_pane.is_some();
        let directional_session_pane = matches!(
            (&target, opts.placement),
            (
                SplitTarget::SessionPane { .. },
                SplitPlacement::Directional(_)
            )
        );
        let no_focus = (!opts.focus || directional_session_pane) && self.supports_no_focus();
        let focus_spawned = opts.focus && no_focus;
        let restore = (!opts.focus && !no_focus)
            .then(|| self.focus_restore_target(&session, focus_workspace.as_ref()))
            .flatten();
        match opts.placement {
            SplitPlacement::Stacked => {
                spec = spec.arg("--stacked");
                if anchored_stack && !no_focus {
                    spec = spec.arg("--near-current-pane");
                }
            }
            SplitPlacement::Directional(direction) => {
                spec = spec.args(["--direction", split_direction(direction)]);
            }
        }
        // The CLI pane context anchors a spawn only under `--no-focus`: a
        // focus-taking directional spawn splits the last-active client's
        // focused pane, whatever `ZELLIJ_PANE_ID` and `--tab-id` name. On
        // 0.45+ a directional session-pane split is therefore always spawned
        // unfocused, and a focus-taking one jumps to the pane it made. On 0.44
        // an anchored stack uses `--near-current-pane` and lets
        // `ZELLIJ_PANE_ID` imply the tab; directional spawns silently no-op
        // with that flag and keep resolving a stable tab id, which places a
        // focus-taking one beside the client's focused pane in that tab.
        if let (
            SplitTarget::SessionPane {
                session_name,
                pane_id,
            },
            SplitPlacement::Directional(_),
        ) = (&target, opts.placement)
            && !no_focus
        {
            let tab_id = self.tab_id_for_pane(session_name, pane_id)?;
            spec = spec.args(["--tab-id".to_owned(), tab_id.to_string()]);
        }
        if no_focus {
            spec = spec.arg("--no-focus");
        }
        if opts.close_on_exit {
            spec = spec.arg("--close-on-exit");
        }
        let title = opts.title.or_else(|| {
            opts.command.as_ref().map_or_else(
                || Some(crate::proc::shell_pane_name()),
                |command| super::pane_short_name(command),
            )
        });
        if let Some(title) = title {
            spec = spec.args(["--name".to_owned(), title]);
        }
        if let Some(cwd) = opts.cwd {
            spec = spec.args(["--cwd".to_owned(), cwd]);
        }
        if let Some(command) = opts.command {
            // Zellij's `new-pane` has no env flag, so inject the requested vars
            // through an `env KEY=VALUE …` prefix — the cross-backend match for
            // tmux's native `-e` (see the backend env-injection parity tests).
            let command = env_prefixed(&opts.env, command);
            if let Some((program, args)) = command.split_first() {
                spec = spec
                    .args(["--".to_owned(), program.clone()])
                    .args(args.iter().cloned());
            }
        }
        let spawned = spec.run()?;
        let created = super::raw_pane::parse_new_pane_id(&String::from_utf8_lossy(&spawned.stdout))
            .map(PaneId::from);
        if focus_spawned {
            // The pane is running, so a lost jump never fails the split. The
            // jump is bare: a focus intent naming a pane no renderer has
            // observed yet is invalidated, and a renderer that clears it
            // before dispatch cancels the jump.
            match &created {
                Some(created) => {
                    if let Err(err) = self.focus_pane(created, Some(&session)) {
                        tracing::debug!(error = %err, "split opened; focusing it failed");
                    }
                }
                None => tracing::debug!("split opened; zellij printed no pane id to focus"),
            }
        }
        if !no_focus {
            self.restore_background_split_focus(
                opts.placement,
                opts.focus,
                &session,
                focus_workspace,
                target_pane,
                restore.as_ref(),
            );
        }
        Ok(created)
    }

    fn append_companion_pane(&self, mut opts: SplitPaneOptions) -> Result<CompanionPaneAppend> {
        let SplitTarget::SessionPane {
            session_name,
            pane_id,
        } = &opts.target
        else {
            return Ok(CompanionPaneAppend::Full);
        };
        let session = session_name.clone();
        let anchor = pane_id.clone();
        if !self.supports_no_focus() {
            return Ok(CompanionPaneAppend::Full);
        }
        let Some((panes, chrome)) =
            self.companion_geometry(&session, &anchor, super::super::COMMAND_TIMEOUT)?
        else {
            return Ok(CompanionPaneAppend::Full);
        };
        let Some(split) = plan_append(&panes, 0) else {
            return Ok(CompanionPaneAppend::Full);
        };
        let Some(selected) = panes.iter().find(|pane| pane.pane_id == split.pane_id) else {
            return Ok(CompanionPaneAppend::Full);
        };
        // Framed terminals need room for both content and their outer frames.
        if match split.direction {
            SplitDirection::Down => selected.rows < 10,
            SplitDirection::Right => selected.cols < 12,
        } {
            return Ok(CompanionPaneAppend::Full);
        }
        opts.target = SplitTarget::SessionPane {
            session_name: session.clone(),
            pane_id: split.pane_id,
        };
        opts.placement = SplitPlacement::Directional(split.direction);
        opts.focus = false;
        // The caller fails this durable run on an uncertain spawn result; it
        // never retries the same payload in another tab.
        self.split_pane(opts)?;
        if let Err(err) = self.balance_companion(&session, &anchor, &panes, &chrome) {
            tracing::debug!(error = %err, "companion opened; geometry balancing stopped");
        }
        Ok(CompanionPaneAppend::Opened)
    }

    fn focus_pane(&self, pane: &PaneId, session: Option<&str>) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let target = ZellijPaneId::try_from(pane)
            .map_err(output_error)?
            .action_target();
        // `focus-pane-id <raw>` first ships in Zellij 0.44.1, one reason the
        // floor sits above 0.44.0.
        let session = self.resolve_session(session)?;
        let spec = self.zellij_action(&session).arg("focus-pane-id");
        spec.arg(target).run().map(|_| ())
    }

    fn toggle_fullscreen(&self, pane: &PaneId, session: Option<&str>) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let session =
            session.ok_or_else(|| output_error("fullscreen toggle requires a session name"))?;
        self.broadcast_presence_pipe(session, super::PRESENCE_TOGGLE_FULLSCREEN_PIPE, pane.raw())
    }

    fn sidebar_width_step(
        &self,
        runtime: &RuntimePaths,
        session: &str,
        pane: &PaneId,
        min_observed_at_ms: Option<u64>,
    ) -> Result<WidthStep> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let pane_id = ZellijPaneId::try_from(pane)
            .ok()
            .and_then(ZellijPaneId::terminal_id)
            .ok_or_else(|| {
                output_error(format!("target pane `{pane}` has no numeric topology id"))
            })?;
        let cache = Self::fresh_cached_topology(
            runtime,
            session,
            crate::utils::time::unix_now_ms(),
            min_observed_at_ms,
        )
        .ok_or_else(|| {
            output_error(format!(
                "fresh pane topology is unavailable for session `{session}`"
            ))
        })?;
        let tab_position = cache
            .panes
            .iter()
            .find(|candidate| !candidate.is_plugin && candidate.id == pane_id)
            .map(|candidate| candidate.tab_position)
            .ok_or_else(|| {
                output_error(format!(
                    "target pane `{pane}` is absent from the topology cache"
                ))
            })?;
        let view_cols = tab_view_cols(&cache.panes, tab_position).ok_or_else(|| {
            output_error(format!("tab {tab_position} has no tiled topology width"))
        })?;
        let cols = u16::try_from(crate::mux::width::zellij_resize_step_cols(view_cols))
            .unwrap_or(u16::MAX);
        let stop_step_cols =
            u16::try_from(crate::mux::width::zellij_resize_stop_step_cols(view_cols))
                .unwrap_or(u16::MAX);
        Ok(WidthStep {
            cols,
            stop_step_cols,
            exact: false,
            view_cols: u16::try_from(view_cols).unwrap_or(0),
            fullscreen_active: Some(tab_fullscreen_active(&cache.panes, tab_position)),
        })
    }

    fn nudge_sidebar_width(
        &self,
        session: &str,
        pane: &PaneId,
        current_cols: u16,
        target_cols: u16,
    ) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        if current_cols == target_cols {
            return Ok(());
        }
        let target = ZellijPaneId::try_from(pane)
            .map_err(output_error)?
            .action_target();
        self.resize_sidebar_step(
            session,
            &target,
            if current_cols < target_cols {
                "increase"
            } else {
                "decrease"
            },
        )
    }

    fn record_sidebar_width_default(&self, _session: &str, _cols: u16) -> Result<()> {
        // Zellij birth layouts read the room-runtime override when generated.
        Ok(())
    }

    fn require_pane_in_session(&self, pane: &PaneId, session: &str) -> Result<()> {
        let held = self
            .raw_listed_panes(session, super::super::COMMAND_TIMEOUT)
            .map(|listed| {
                listed
                    .into_iter()
                    .map(PaneTopologyPane::from)
                    .filter(PaneTopologyPane::holds_terminal)
                    .map(|pane| PaneId::from(pane.native_id()))
            });
        crate::mux::require_held_pane(pane, session, held)
    }

    fn capture_pane(
        &self,
        pane: &PaneId,
        session: &str,
        lines: Option<u16>,
        ansi: bool,
    ) -> Result<PaneCapture> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let target = ZellijPaneId::try_from(pane)
            .map_err(output_error)?
            .action_target();
        let mut spec = self.zellij_action(session).arg("dump-screen");
        if ansi {
            spec = spec.arg("-a");
        }
        if lines.is_some() {
            // The `-f`/`--full` flag dumps the entire scrollback. Zellij does
            // not expose a "last N lines" cap at the CLI level, so any non-None
            // request maps onto "include scrollback"; the caller can post-trim.
            spec = spec.arg("-f");
        }
        spec = spec.args(["-p".to_owned(), target]);
        let output = spec.run()?;
        let raw_text = String::from_utf8_lossy(&output.stdout).into_owned();
        let (raw_text, lines) = trim_capture(raw_text, lines);
        Ok(PaneCapture {
            pane_id: pane.clone(),
            raw_text,
            lines,
        })
    }

    fn send_keys(&self, pane: &PaneId, session: &str, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let target = ZellijPaneId::try_from(pane)
            .map_err(output_error)?
            .action_target();
        self.zellij_action(session)
            .args(["write-chars", "--pane-id", &target, "--", text])
            .run()
            .map(|_| ())
    }

    fn send_key(&self, pane: &PaneId, session: &str, key: NamedKey) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let target = ZellijPaneId::try_from(pane)
            .map_err(output_error)?
            .action_target();
        let bytes = key.write_bytes().iter().map(u8::to_string);
        self.zellij_action(session)
            .args(["write", "--pane-id", &target])
            .args(bytes)
            .run()
            .map(|_| ())
    }

    fn paste_text(&self, pane: &PaneId, session: &str, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let payload = paste_payload(text);
        let target = ZellijPaneId::try_from(pane)
            .map_err(output_error)?
            .action_target();
        // Chunk the complete byte stream so small pastes remain one command.
        let bytes = BRACKET_PASTE_OPEN
            .bytes()
            .chain(payload.bytes())
            .chain(BRACKET_PASTE_CLOSE.bytes())
            .collect::<Vec<_>>();
        for chunk in bytes.chunks(super::ZELLIJ_WRITE_CHUNK) {
            if let Err(err) = self
                .zellij_action(session)
                .args(["write", "--pane-id", &target])
                .args(chunk.iter().map(u8::to_string))
                .run()
            {
                let _ = self
                    .zellij_action(session)
                    .args(["write", "--pane-id", &target])
                    .args(BRACKET_PASTE_CLOSE.bytes().map(|byte| byte.to_string()))
                    .run();
                return Err(err);
            }
        }
        Ok(())
    }

    fn open_sidebar(&self, opts: &SidebarPaneOptions, daemon: Option<&DaemonView>) -> Result<()> {
        // Zellij places a left pane only at session birth, so the sidebar is
        // injected only by (re)creating the session from a layout. `daemon`, when
        // present, leads the birth layout (the only way a tab can lead, since
        // Zellij can't reorder tabs after birth):
        //   - Absent: first birth.
        //   - Exited: `attach` would resurrect a stale serialized layout (wrong
        //             geometry, suspended command panes), so delete and rebirth.
        //   - Live + sidebar: healthy only when the caller still trusts a
        //             fresh current-protocol heartbeat. If launch reached this
        //             method after rejecting the heartbeat, the pane may be a
        //             stale renderer with an incompatible snapshot schema.
        //   - Live, no sidebar: the renderer self-closed or crashed (or a launch
        //             was skipped and the session was born by a plain `attach
        //             --create`). A sidebar-less rimz session is non-functional
        //             and cannot gain a left pane in place, so rebirth it.
        match self.session_state(&opts.session_name) {
            SessionLiveness::Absent => self.create_session_with_sidebar(opts, daemon),
            SessionLiveness::Exited => {
                self.delete_session(&opts.session_name)?;
                self.create_session_with_sidebar(opts, daemon)
            }
            SessionLiveness::Live => {
                match self.inspect_session_panes(&opts.session_name, &opts.workspace_id) {
                    Ok(()) => {
                        self.delete_session(&opts.session_name)?;
                        self.create_session_with_sidebar(opts, daemon)
                    }
                    Err(err) => {
                        tracing::warn!(
                            session = %opts.session_name,
                            tags.operation = "zellij.room_inspect",
                            error = &err as &dyn std::error::Error,
                            "live zellij room could not be inspected; leaving it untouched",
                        );
                        Err(err)
                    }
                }
            }
        }
    }

    fn probe_session_health(&self, name: &str) -> Result<SessionHealth> {
        Ok(match self.session_state(name) {
            // Nothing to attach to — a fresh birth will produce a clean room.
            SessionLiveness::Absent => SessionHealth::Healthy,
            // `attach --create` would resurrect a serialized, suspended layout.
            SessionLiveness::Exited => SessionHealth::Stuck,
            SessionLiveness::Live => self.live_session_health(name),
        })
    }

    fn ensure_clean_session(
        &self,
        opts: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
    ) -> Result<SessionHealth> {
        let state = self.session_state(&opts.session_name);
        // A stale topology cache is not evidence that a live room is stuck, but
        // a direct native listing must answer before attach. Its payload is not
        // used as topology truth here.
        if matches!(state, SessionLiveness::Live) {
            return Ok(self.live_session_health(&opts.session_name));
        }
        // Absent → first birth; Exited → delete and rebirth from the layout so
        // the room comes up clean and RUNNING (with serialization off, a rebirth
        // can never resurrect). A rebirth that still fails to talk to Zellij
        // reads as Stuck so the caller runs or reports the reset path.
        let rebirth = || -> Result<()> {
            if !matches!(state, SessionLiveness::Absent) {
                self.delete_session(&opts.session_name)?;
            }
            self.create_session_with_sidebar(opts, daemon)
        };
        match rebirth() {
            Ok(()) => Ok(SessionHealth::Reborn),
            Err(
                err @ (MuxErr::SocketPathTooLong { .. } | MuxErr::SocketPathReportedTooLong { .. }),
            ) => Err(err),
            Err(err) => {
                tracing::warn!(
                    session = %opts.session_name,
                    tags.operation = "zellij.session_rebirth",
                    error = &err as &dyn std::error::Error,
                    "session rebirth failed; a destructive reset is required",
                );
                Ok(SessionHealth::Stuck)
            }
        }
    }

    fn purge_resurrection_cache(&self, name: &str) -> Vec<PathBuf> {
        // `delete-session --force` already drops the serialized session, but a
        // crashed server can leave the cache behind with no live session to
        // delete, so reset removes it directly as well.
        super::session::purge_zellij_session_cache_in(&crate::disk::paths::cache_home(), name)
    }

    fn resurrection_cache_paths(&self, name: &str) -> Vec<PathBuf> {
        super::session::zellij_session_cache_paths_in(&crate::disk::paths::cache_home(), name)
    }

    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> Result<SidebarRecovery> {
        // Zellij docks the sidebar left only at session birth, but a left pane
        // can still be reached in a live session: close a stray sidebar by id,
        // or mount one through a stable tab id before moving it left and sizing
        // it to the tab's live target. This never rebirths the session, so
        // working panes survive.
        let listing = self.topology_listing(
            Some(&opts.session_name),
            None,
            Some(&opts.workspace_id),
            live.topology_floor_ms,
            RECONCILE_LIST_TIMEOUT,
        )?;
        let panes = listing.panes;
        let views = group_reconcile_panes(panes.iter().filter_map(reconcile_pane));
        let plan = super::super::plan_reconcile(&views, live);
        let planned_closes = plan.close_panes();
        let added_views = plan.add_views();
        let replaced_views: HashSet<_> = views
            .iter()
            .filter(|view| !view.sidebar_panes.is_empty() && added_views.contains(&view.view))
            .map(|view| view.view.clone())
            .collect();
        // Kept sidebars (not planned for closing) whose geometry sits off the
        // layout's dock — the residue of a mis-mounted add — converge in place
        // this pass, renderer untouched.
        let width_floor = live.topology_floor_ms;
        let off_spec = off_spec_sidebars(&panes, &planned_closes, width_floor.map(|_| opts.target));
        if plan.is_empty() && off_spec.is_empty() {
            return Ok(SidebarRecovery::default());
        }

        // Structural repair is scoped to the attached client view. Hidden tabs
        // have no RimZ focus state and repair themselves when later viewed.
        let restoration = self
            .client_view(ClientFocusOptions {
                session_name: Some(opts.session_name.clone()),
                command_timeout: Some(RECONCILE_LIST_TIMEOUT),
            })
            .ok()
            .and_then(|view| client_restoration_target(&panes, &view));

        let mut report = SidebarRecovery::default();
        // In-place adds and geometry moves both need an attached client: a
        // detached session's screen thread drops the mount while the spawned
        // serve pair keeps running, so adding there only leaks (the closes
        // above are safe detached). An unanswerable probe reads detached —
        // deferring one run is recoverable, a leaked pair is not. tmux splits
        // fine detached, so the gate is Zellij-internal.
        let detached = (plan.has_adds() || !off_spec.is_empty())
            && !self.session_has_attached_client(&opts.session_name);
        if !detached {
            for (tab_position, raw_id) in &off_spec {
                repair_sidebar_geometry(
                    self,
                    opts,
                    *tab_position,
                    *raw_id,
                    width_floor,
                    &mut report,
                );
            }
        }
        if detached {
            report.deferred += off_spec.len();
        }
        let build = sidebar_build_identity(opts)?;
        let failure = execute_reconcile_plan(
            plan,
            &mut report,
            detached,
            |view| {
                let tab_position = view.parse::<u64>().map_err(|err| {
                    output_error(format!("invalid sidebar tab position `{view}`: {err}"))
                })?;
                let added = self.add_sidebar_to_tab(opts, tab_position, width_floor)?;
                if !prove_sidebar_mount(opts, MuxName::Zellij, &added.pane, &build, || {
                    if let Some(raw_id) = parse_zellij_raw(&added.pane) {
                        self.cleanup_failed_add(opts, raw_id);
                    }
                }) {
                    return Err(output_error(format!(
                        "sidebar {} mounted in tab {tab_position} without a current-build heartbeat",
                        added.pane
                    )));
                }
                Ok(match added.dock {
                    DockOutcome::Docked => ReconcileAddOutcome::Verified,
                    DockOutcome::Misdocked => ReconcileAddOutcome::VerifiedMisdocked,
                })
            },
            |pane| self.close_pane(&opts.session_name, pane),
        );
        if !detached && failure.is_none() && !replaced_views.is_empty() {
            let floor = Some(crate::utils::time::unix_now_ms());
            match self.topology_listing(
                Some(&opts.session_name),
                None,
                Some(&opts.workspace_id),
                floor,
                RECONCILE_LIST_TIMEOUT,
            ) {
                Ok(after) => {
                    for (tab, pane) in off_spec_sidebars(&after.panes, &[], Some(opts.target)) {
                        if replaced_views.contains(&tab.to_string()) {
                            repair_sidebar_geometry(self, opts, tab, pane, floor, &mut report);
                        }
                    }
                }
                Err(err) => tracing::warn!(
                    session = %opts.session_name,
                    tags.operation = "zellij.reconcile.geometry_after_close",
                    error = &err as &dyn std::error::Error,
                    "sidebar replacement geometry unavailable after closing old panes",
                ),
            }
        }
        if detached {
            tracing::info!(
                session = %opts.session_name,
                deferred = report.deferred,
                "sidebar reconcile: no attached client; deferring in-place adds and geometry repairs",
            );
        }
        if let Some(failure) = failure {
            tracing::warn!(
                session = %opts.session_name,
                view = %failure.view,
                tags.operation = "zellij.reconcile.transaction",
                error = &failure.error as &dyn std::error::Error,
                "sidebar repair aborted; leaving remaining views unchanged",
            );
        }
        if let Some(restoration) = restoration {
            restore_client_view(self, opts, restoration);
        }
        Ok(report)
    }

    fn open_background_view(&self, opts: &BackgroundViewOptions) -> Result<BackgroundViewLaunch> {
        let session = &opts.sidebar.session_name;
        // Idempotent on the tab name. The lead position is owned by session birth
        // ([`Self::open_sidebar`] with a `daemon`): `rimz start` births the session
        // with this tab already leading, so the common case is a no-op here. A
        // failed query propagates rather than risk a duplicate launch.
        if self.session_has_named_tab(session, &opts.view.name)? {
            return Ok(BackgroundViewLaunch::AlreadyRunning);
        }
        // Late add: the session was born without the daemon tab (e.g. a host
        // became available after first start) and now carries one or more working
        // tabs. Zellij can't move a tab to the front, so this appended tab does
        // *not* lead — leading is a birth-time property. `--layout` gives the tab
        // its `sidebar | content | hosts…` shape directly (bypassing the tab
        // template, so the sidebar is spelled out). Zellij can drop transient
        // `new-tab` mutations under load, so keep the temp layout alive until
        // the named tab is confirmed. Each pane carries its own `cwd`, so no
        // tab-level `--cwd` is needed.
        let layout = TempLayoutFile::new(render_background_view_layout(opts)?)?;
        let args = [
            "new-tab".to_owned(),
            "--layout".to_owned(),
            layout.path().to_string_lossy().into_owned(),
            "--name".to_owned(),
            opts.view.name.clone(),
        ];
        self.run_new_tab_confirmed(session, &args, &opts.view.name)?;
        drop(layout);
        // `new-tab` focuses the tab it creates. Return focus to the leading tab so
        // the imminent `attach` lands on a working pane, not this freshly-added
        // daemon tab. Best-effort: a focus hiccup never sinks a launch.
        if let Err(err) = self.go_to_lead_tab(session) {
            tracing::warn!(
                session = %session,
                tags.operation = "zellij.focus_tab",
                error = &err as &dyn std::error::Error,
                "could not return focus off the freshly-added daemon tab",
            );
        }
        Ok(BackgroundViewLaunch::Launched)
    }

    fn confirm_resume_tabs(
        &self,
        session: &str,
        tabs: &[ResumeTab],
    ) -> Vec<std::result::Result<(), ResumeTabUnconfirmed>> {
        if tabs.is_empty() {
            return Vec::new();
        }
        let config = crate::config::MachineConfig::load_lenient();
        let planned = tabs
            .iter()
            .map(|tab| ResumeTabShape {
                name: tab.label.clone(),
                panes: tab.pane_count(),
            })
            .collect::<Vec<_>>();
        // Zellij applies a session layout asynchronously, so a tab may list
        // before its panes do.
        let deadline = Instant::now() + super::NEW_TAB_MATERIALIZE_WINDOW;
        let mut last_observed = None;
        while let Some(remaining) = deadline_remaining(deadline) {
            let panes = match self.raw_listed_panes(session, remaining) {
                Ok(panes) => panes,
                Err(MuxErr::Timeout { .. })
                    if last_observed.is_some() && Instant::now() >= deadline =>
                {
                    break;
                }
                Err(err) => {
                    return vec![Err(ResumeTabUnconfirmed::Unlisted(err.to_string())); tabs.len()];
                }
            };
            let outcomes =
                confirm_resume_tab_shapes(&planned, &live_tab_shapes(&panes, &config.theme));
            if outcomes.iter().all(|outcome| outcome.is_ok()) {
                return outcomes;
            }
            last_observed = Some(outcomes);
            let Some(remaining) = deadline_remaining(deadline) else {
                break;
            };
            std::thread::sleep(super::NEW_TAB_MATERIALIZE_STEP.min(remaining));
        }
        last_observed.unwrap_or_else(|| vec![Err(ResumeTabUnconfirmed::Absent); tabs.len()])
    }

    fn open_tab(&self, opts: &TabOptions) -> Result<()> {
        // Before 0.45, new-tab always takes focus. Restore the single client
        // view afterward, or fall back to the lead tab when it cannot resolve.
        let no_focus = !opts.focus && self.supports_no_focus();
        let restore = (!opts.focus && !no_focus)
            .then(|| {
                self.focus_restore_target(
                    &opts.sidebar.session_name,
                    Some(&opts.sidebar.workspace_id),
                )
            })
            .flatten();
        let view_cols = (|| {
            let panes = self
                .topology_panes(&opts.sidebar.session_name, None, RECONCILE_LIST_TIMEOUT)
                .ok()?;
            let tab = panes
                .iter()
                .find(|pane| pane.is_live_terminal())?
                .tab_position;
            tab_view_cols(&panes, tab)
                .and_then(|cols| u16::try_from(cols).ok())
                .filter(|cols| *cols > 0)
        })();
        let runtime = self.runtime_paths_for_workspace(opts.sidebar.workspace_id.clone())?;
        let width = crate::mux::SidebarWidth::from_config(
            &crate::config::MachineConfig::load_lenient().theme,
        );
        let sidebar_percent =
            crate::mux::width_target::resolve(&runtime, width, view_cols).percent();
        let layout = TempLayoutFile::new(render_tab_layout(opts, sidebar_percent)?)?;
        let mut args = vec![
            "new-tab".to_owned(),
            "--layout".to_owned(),
            layout.path().to_string_lossy().into_owned(),
            "--name".to_owned(),
            opts.title.clone(),
        ];
        if no_focus {
            args.push("--no-focus".to_owned());
        }
        self.run_new_tab_confirmed(&opts.sidebar.session_name, &args, &opts.title)?;
        drop(layout);
        if let Some(anchor) = opts.after.as_ref()
            && let Err(err) = self.move_new_tab_after(&opts.sidebar.session_name, anchor)
        {
            tracing::warn!(
                session = %opts.sidebar.session_name,
                pane = %anchor,
                tags.operation = "zellij.move_tab",
                error = &err as &dyn std::error::Error,
                "could not place the new tab after its anchor; leaving it appended",
            );
        }
        if !opts.focus && !no_focus {
            let result = match &restore {
                Some(restore) => self.restore_attached_client_focus(
                    &opts.sidebar.session_name,
                    &opts.sidebar.workspace_id,
                    restore,
                ),
                None => self.go_to_lead_tab(&opts.sidebar.session_name),
            };
            if let Err(err) = result {
                tracing::warn!(
                    session = %opts.sidebar.session_name,
                    tags.operation = "zellij.focus_tab",
                    error = &err as &dyn std::error::Error,
                    "could not return focus after opening an unfocused tab",
                );
            }
        }
        Ok(())
    }

    fn can_open_tab(&self, session: &str) -> bool {
        self.session_can_open_tab(session)
    }

    fn rename_tab(
        &self,
        session: &str,
        anchor: &PaneId,
        name: &str,
        intent: TabNameIntent,
    ) -> Result<()> {
        let (tab_id, current) =
            self.tab_for_pane_within(session, anchor, super::super::TAB_RENAME_TIMEOUT)?;
        if let (Some(observed), Some(current)) = (intent.observed(), current.as_deref())
            && observed != current
        {
            return Ok(());
        }
        self.zellij_action(session)
            .args([
                "rename-tab-by-id".to_owned(),
                tab_id.to_string(),
                name.to_owned(),
            ])
            .run_with_timeout(super::super::TAB_RENAME_TIMEOUT)?;
        if let TabNameIntent::Claim { pane_name } = intent
            && let Err(err) = self
                .zellij_action(session)
                .args(["rename-pane", "--pane-id", anchor.raw(), "--", &pane_name])
                .run_with_timeout(super::super::TAB_RENAME_TIMEOUT)
        {
            tracing::warn!(
                session,
                pane = %anchor,
                tags.operation = "zellij.rename_pane",
                error = &err as &dyn std::error::Error,
                "could not pin the pane's launch name",
            );
        }
        Ok(())
    }

    fn close_pane(&self, session: &str, pane: &PaneId) -> Result<()> {
        ZellijBackend::close_pane(self, session, pane)
    }

    fn close_view_floating_panes(&self, session: &str, anchor: &PaneId) -> Result<Vec<PaneId>> {
        ensure_pane_backend(anchor, MuxName::Zellij)?;
        let panes = self.topology_panes(session, None, super::super::COMMAND_TIMEOUT)?;
        let mut closed = Vec::new();
        for pane_id in floating_panes_in_anchor_view(&panes, anchor) {
            match self.close_pane(session, &pane_id) {
                Ok(()) => closed.push(pane_id),
                Err(err) => tracing::warn!(
                    session,
                    pane = %pane_id,
                    tags.operation = "zellij.close_floating_pane",
                    error = &err as &dyn std::error::Error,
                    "could not close floating pane during sidebar self-close",
                ),
            }
        }
        Ok(closed)
    }

    fn ensure_presence_plugin(&self, opts: &super::super::PresencePluginOptions) -> Result<()> {
        self.ensure_presence_plugin_for(opts)
    }

    fn version(&self) -> Result<String> {
        memoized_version(&self.version, &self.cmd().arg("--version"))
    }
}

pub(super) fn moves_to_place_after(anchor_position: u64, tab_count: u64) -> u64 {
    tab_count
        .saturating_sub(1)
        .saturating_sub(anchor_position.saturating_add(1))
}

pub(super) fn reconcile_pane(pane: &PaneTopologyPane) -> Option<ReconcilePane> {
    if !pane.is_terminal() {
        return None;
    }
    let role = ReconcilePaneRole::from_evidence(is_sidebar_pane(pane), is_daemon_host_pane(pane));
    Some(ReconcilePane {
        view: pane.tab_position.to_string(),
        pane_id: PaneId::from(pane.native_id()),
        role,
    })
}

pub(super) fn off_spec_sidebars(
    panes: &[PaneTopologyPane],
    closing: &[PaneId],
    width_target: Option<crate::mux::SidebarTarget>,
) -> Vec<(u64, u64)> {
    let closing: HashSet<u64> = closing.iter().filter_map(parse_zellij_raw).collect();
    panes
        .iter()
        .filter(|pane| pane.is_live_terminal() && is_sidebar_pane(pane))
        .filter(|pane| !closing.contains(&pane.id))
        .filter(|pane| !tab_fullscreen_active(panes, pane.tab_position))
        .filter(|pane| sidebar_geometry_off_spec(pane, panes, &closing, width_target))
        .map(|pane| (pane.tab_position, pane.id))
        .collect()
}

fn parse_zellij_raw(pane: &PaneId) -> Option<u64> {
    ZellijPaneId::try_from(pane)
        .ok()
        .and_then(ZellijPaneId::terminal_id)
}

fn repair_sidebar_geometry(
    backend: &ZellijBackend,
    opts: &SidebarPaneOptions,
    tab_position: u64,
    raw_id: u64,
    width_floor: Option<u64>,
    report: &mut SidebarRecovery,
) {
    let floor = backend.converge_sidebar_geometry(opts, tab_position, raw_id, width_floor);
    match backend.sidebar_dock_outcome(
        &opts.session_name,
        &opts.workspace_id,
        tab_position,
        raw_id,
        floor,
    ) {
        DockOutcome::Docked => {
            if floor.is_some() {
                report.redocked += 1;
            }
        }
        DockOutcome::Misdocked => {
            report.misdocked += 1;
        }
    }
}

fn client_restoration_target(
    panes: &[PaneTopologyPane],
    view: &ClientView,
) -> Option<(u64, Option<u64>)> {
    let mut viewed = view
        .viewed_panes
        .iter()
        .filter_map(parse_zellij_raw)
        .collect::<Vec<_>>();
    viewed.sort_unstable();
    viewed.dedup();
    let [viewed] = viewed.as_slice() else {
        return None;
    };
    let pane = panes
        .iter()
        .find(|pane| pane.id == *viewed && pane.is_live_terminal())?;
    Some((
        pane.tab_position,
        (!is_sidebar_pane(pane)).then_some(pane.id),
    ))
}

fn restore_client_view(
    backend: &ZellijBackend,
    opts: &SidebarPaneOptions,
    restoration: (u64, Option<u64>),
) {
    let Ok(panes) = backend.topology_panes_for_workspace(
        &opts.session_name,
        &opts.workspace_id,
        None,
        RECONCILE_LIST_TIMEOUT,
    ) else {
        return;
    };
    let (tab_position, preferred) = restoration;
    let work = preferred
        .filter(|id| {
            panes.iter().any(|pane| {
                pane.id == *id
                    && pane.tab_position == tab_position
                    && pane.is_live_terminal()
                    && !is_sidebar_pane(pane)
            })
        })
        .or_else(|| super::raw_pane::leftmost_live_work_pane(&panes, tab_position));
    let Some(work) = work else {
        return;
    };
    let Ok(runtime) = backend.runtime_paths_for_workspace(opts.workspace_id.clone()) else {
        return;
    };
    let pane = PaneId::from(ZellijPaneId::Terminal(work));
    let _ = execute_focus_restoration(
        backend,
        &runtime,
        &opts.session_name,
        &pane,
        Some(tab_position),
        crate::mux::focus_anchor::FocusDispatchRetries {
            attempts: super::FOCUS_RESTORE_ATTEMPTS,
            delay: super::FOCUS_RESTORE_RETRY_DELAY,
        },
    );
}

pub(super) fn execute_focus_restoration(
    backend: &ZellijBackend,
    runtime: &crate::disk::paths::RuntimePaths,
    session_name: &str,
    pane: &PaneId,
    tab_position: Option<u64>,
    retries: crate::mux::focus_anchor::FocusDispatchRetries,
) -> std::result::Result<(), crate::mux::focus_anchor::FocusActionError> {
    let nonce = crate::mux::focus_anchor::request_action(
        backend,
        runtime,
        session_name,
        crate::mux::focus_anchor::FocusActionRequest {
            pane_id: pane.clone(),
            origin: crate::mux::focus_anchor::FocusOrigin::User,
            repair_generation: None,
            expected_pre_action: None,
            offset: 0,
            order: None,
        },
    )?;
    if let Some(tab_position) = tab_position {
        let _ = backend.go_to_tab_position(session_name, tab_position);
    }
    if crate::mux::focus_anchor::dispatch_action(
        backend,
        runtime,
        session_name,
        pane,
        nonce,
        retries,
    )? {
        Ok(())
    } else {
        Err(crate::mux::focus_anchor::FocusActionError::Superseded)
    }
}
