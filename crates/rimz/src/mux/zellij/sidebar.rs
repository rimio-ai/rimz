//! Zellij sidebar birth, in-place recovery, and geometry convergence.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use super::layout::{TempLayoutFile, render_session_layout};
use super::pane_topology::{PaneTopologyCache, PaneTopologyPane, ZellijPaneId};
use super::parse::terminal_client_ids;
use super::raw_pane::{
    SidebarDock, is_sidebar_pane, leftmost_live_work_pane, mounted_sidebar_pane,
    nested_work_pane_ids, parse_new_pane_id, repairable_nested_work_pane_ids, sidebar_dock_verdict,
    tab_view_cols, wrong_tab_mounted_sidebar_pane,
};
use super::socket::{socket_headroom_with_xdg_override, stderr_reports_socket_overflow};
use super::{
    MOUNT_POLL_STEP, MOUNT_POLL_TIMEOUT, RECONCILE_LIST_TIMEOUT, SIDEBAR_LAYOUT_TIMEOUT,
    TOPOLOGY_CACHE_POLL_STEP, ZellijBackend,
};
use crate::ids::{MuxClientId, MuxName, PaneId, WorkspaceId};
use crate::mux::width::{
    sidebar_width_off_spec, width_step_regressed, zellij_resize_stop_step_cols,
};
use crate::mux::{
    CommandSpec, DaemonView, MuxBackend, MuxErr, PaneReadConsistency, PresencePluginOptions,
    Result, SessionLiveness, SidebarPaneOptions, WidthSyncOptions, sidebar_serve_args,
};
use crate::pane::SIDEBAR_CHROME_TITLE;
use crate::utils::time::unix_now_ms;

const ADD_DOCK_ATTEMPTS: u32 = 2;
const DOCK_VERIFY_SETTLE: Duration = Duration::from_millis(100);
const CLIENT_PROBE_SETTLE: Duration = Duration::from_millis(100);
// A single `focus-pane-id` can lag during session birth. Re-issue and confirm
// against the attached client view within a bounded window.
// Confirmation must outlast Zellij's background-create bootstrap-client linger.
// A false negative only defers one recoverable add pass; a false positive can
// leak an unmounted sidebar serve pair.
const CLIENT_CONFIRM_WINDOW: Duration = Duration::from_millis(750);
const GEOMETRY_REPAIR_SETTLE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DockOutcome {
    Docked,
    Misdocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AddedSidebar {
    pub(super) pane: PaneId,
    pub(super) dock: DockOutcome,
}

enum MountOutcome {
    Intended(u64),
    WrongTab(u64),
}

fn sidebar_pane(panes: &[PaneTopologyPane], tab_id: u64, raw_id: u64) -> Option<&PaneTopologyPane> {
    // Plugin panes share the integer id space (`plugin_1` beside
    // `terminal_1`), so the terminal filter is load-bearing here.
    panes
        .iter()
        .find(|pane| pane.is_terminal() && pane.stable_tab_id == Some(tab_id) && pane.id == raw_id)
}

impl ZellijBackend {
    pub(super) fn background_session_command(
        &self,
        opts: &SidebarPaneOptions,
        layout: &Path,
    ) -> CommandSpec {
        let option_args = vec![
            "attach".to_owned(),
            "--create-background".to_owned(),
            opts.session_name.clone(),
            "options".to_owned(),
            "--default-cwd".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
            "--default-layout".to_owned(),
            layout.to_string_lossy().into_owned(),
        ];
        let mut spec = self.cmd().without_mux_context().args(option_args);
        for key in crate::mux::REMOTE_CLIENT_ENV {
            spec = spec.env_remove(key);
        }
        // The identity pin rides the spawning client's environment: the
        // per-session server is forked from this command, and every pane is
        // forked from the server, so panes — and the agents and in-pane hook
        // children inside them — inherit the room's workspace transitively.
        // A daemon-routed hook child (Codex's per-user app-server) inherits
        // the daemon's env instead and recovers the pin from the in-pane
        // agent process (`resolve_daemon_participant_with_pin_recovery`). Zellij has
        // no post-birth `set-environment`, so birth is the one stamping
        // point; a pre-pin server keeps its env and its participants fall
        // back to the static ladder.
        for (key, value) in crate::workspace::pin_env(&opts.workspace_id, &opts.project_root) {
            spec = spec.env(key, value);
        }
        for (key, value) in &opts.extra_env {
            spec = spec.env(key, value);
        }
        // Zellij fixes each pane's TERM at server birth from this process's
        // environment and never re-asserts it. A non-PTY birth carries no TERM,
        // so ncurses apps in the room see the compiled default `unknown` and
        // shell line editing breaks. Stamp the xterm.js-compatible baseline
        // when the birth env has none; a real terminal's TERM rides through.
        if let Some(term) = birth_term(std::env::var("TERM").ok().as_deref()) {
            spec = spec.env("TERM", term);
        }
        spec
    }

    /// Create the background session from a layout that puts the sidebar chrome
    /// pane on the left and focuses the user's terminal on the right. The
    /// layout carries the new-tab template, so new tabs are born with a sidebar
    /// too. The sidebar pane is `close_on_exit`, so when its own process exits
    /// the pane closes — see the self-close loop in `sidebar_pane::app`.
    ///
    /// Zellij parses `--default-layout` asynchronously, after the
    /// `--create-background` client returns, so the temp layout file must
    /// outlive the call. We hold it through a bounded wait for the sidebar pane
    /// to appear, then let it drop.
    pub(super) fn create_session_with_sidebar(
        &self,
        opts: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
    ) -> Result<()> {
        // A daemon view leads only if it is born first, and resumed agents only
        // come back as command panes the birth layout spells out: Zellij can't
        // reorder tabs or add command panes after birth. The same birth layout
        // handles a plain room as `None, &[]`, so every session carries the same
        // new-tab template and fixed sidebar/compact-bar tree shape.
        let presence = self.birth_presence_options(opts)?;
        self.seed_presence_permissions(&presence);
        // Allocate the path first so resolution observes the exact birth spec.
        let layout = TempLayoutFile::new(String::new())?;
        let spec = self.background_session_command(opts, layout.path());
        let mut plugins = super::background_plugins::resolve(&spec, &opts.cwd);
        if let Some(plugins) = &mut plugins {
            plugins
                .nodes_mut()
                .push(super::presence::presence_plugin_entry(&presence));
        }
        let body = render_session_layout(opts, daemon, &opts.resume_tabs, plugins.as_ref())?;
        std::fs::write(layout.path(), body)?;
        let spawn = || -> Result<bool> {
            let output = spec
                .clone()
                .cwd(opts.cwd.clone())
                .output_raw_with_timeout(self.birth_timeout())?;
            if output.status.success() {
                return Ok(true);
            }

            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let lower = stderr.to_ascii_lowercase();
            // A racing `rimz` may have created the session first; treat that as
            // success rather than re-injecting.
            if lower.contains("already exists")
                || (lower.contains("session") && lower.contains("exists"))
            {
                return Ok(false);
            }
            if stderr_reports_socket_overflow(&stderr) {
                let headroom = socket_headroom_with_xdg_override(
                    &opts.session_name,
                    self.runtime_dir.as_deref(),
                );
                if headroom.len < headroom.limit {
                    return Err(MuxErr::SocketPathReportedTooLong { stderr });
                }
                return Err(MuxErr::SocketPathTooLong {
                    path: headroom.path,
                    len: headroom.len,
                    limit: headroom.limit,
                });
            }

            Err(MuxErr::Command {
                program: spec.program.clone(),
                args: spec.args.join(" "),
                stderr,
            })
        };

        let record_birth_owners = || -> Result<()> {
            let runtime = self.runtime_paths_for_workspace(opts.workspace_id.clone())?;
            super::tab_owner::clear(&runtime)?;
            self.record_layout_tab_owners(opts, daemon)
        };

        let created = spawn()?;
        if created {
            record_birth_owners()?;
        }
        self.ensure_birth_presence_plugin(&presence)?;
        if self.wait_for_sidebar_layout(&opts.session_name) {
            self.finalize_birth_focus(&opts.session_name, &opts.workspace_id);
            drop(layout);
            return Ok(());
        }

        if created {
            tracing::warn!(
                session = %opts.session_name,
                "sidebar layout did not materialize within the ceiling; retrying session birth \
                 before dropping the temp layout",
            );
            self.delete_session(&opts.session_name)?;
            spawn()?;
            record_birth_owners()?;
            self.ensure_birth_presence_plugin(&presence)?;
            if self.wait_for_sidebar_layout(&opts.session_name) {
                self.finalize_birth_focus(&opts.session_name, &opts.workspace_id);
                drop(layout);
                return Ok(());
            }
        }

        if !created {
            tracing::warn!(
                session = %opts.session_name,
                "sidebar layout did not materialize within the ceiling; dropping the temp \
                 layout may leave the session sidebar-less — it self-heals on the next open_sidebar",
            );
        } else {
            tracing::warn!(
                session = %opts.session_name,
                "sidebar layout still did not materialize after retry; dropping the temp \
                 layout may leave the session sidebar-less — it self-heals on the next open_sidebar",
            );
        }
        drop(layout);
        Ok(())
    }

    fn birth_presence_options(&self, opts: &SidebarPaneOptions) -> Result<PresencePluginOptions> {
        let wasm = super::ensure_presence_plugin_artifact().ok_or_else(|| super::output_error("Zellij presence plugin artifact is unavailable; run `rimz doctor` or use the tmux backend"))?;
        let machine_config = crate::config::MachineConfig::load_lenient();
        Ok(PresencePluginOptions::from_config(
            &opts.session_name,
            &opts.workspace_id,
            wasm,
            self.state_paths_for_workspace(opts.workspace_id.clone())?
                .room_bin,
            &machine_config.sidebar,
            &opts.config.zellij,
        ))
    }

    fn ensure_birth_presence_plugin(&self, presence: &PresencePluginOptions) -> Result<()> {
        let floor_ms = unix_now_ms();
        self.ensure_presence_plugin_for(presence)?;
        self.retire_proven_presence_plugin_for(
            presence,
            floor_ms,
            Duration::ZERO,
            TOPOLOGY_CACHE_POLL_STEP,
        );
        Ok(())
    }

    /// Mount a sidebar in a stable tab. A replacement can split a docked old
    /// sidebar, leaving its geometry alone until the executor closes the old
    /// panes. Plain adds retain their native placement and bounded dock repair.
    pub(super) fn add_sidebar_to_tab(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
        replacing: &[PaneId],
        viewed_tabs: &HashSet<u64>,
    ) -> Result<AddedSidebar> {
        let mut last_error = None;
        let mut fallback_misdocked: Option<u64> = None;
        for attempt in 0..ADD_DOCK_ATTEMPTS {
            let before_panes = match self.structural_geometry_listing(
                &opts.session_name,
                &opts.workspace_id,
                viewed_tabs,
            ) {
                Ok(listing) => listing.panes,
                Err(err) => {
                    if let Some(raw_id) = fallback_misdocked {
                        self.cleanup_failed_add(opts, raw_id);
                    }
                    return Err(err);
                }
            };
            let before: HashSet<u64> = before_panes
                .iter()
                .filter(|pane| !pane.is_plugin)
                .map(|pane| pane.id)
                .collect();
            let tab_position = before_panes
                .iter()
                .find(|pane| pane.stable_tab_id == Some(tab_id))
                .map(|pane| pane.tab_position)
                .ok_or_else(|| super::output_error(format!("tab {tab_id} is no longer listed")))?;
            if leftmost_live_work_pane(&before_panes, tab_position).is_none() {
                return Err(super::output_error(format!(
                    "tab {tab_id} has no stable work pane to target"
                )));
            }
            let replaced: HashSet<u64> = replacing
                .iter()
                .filter_map(|pane| ZellijPaneId::try_from(pane).ok()?.terminal_id())
                .collect();
            let anchor = if !replaced.is_empty() && self.supports_no_focus() {
                replacing.iter().find_map(|pane| {
                    let id = ZellijPaneId::try_from(pane).ok()?.terminal_id()?;
                    let candidate = sidebar_pane(&before_panes, tab_id, id)?;
                    let excluded = replaced
                        .iter()
                        .copied()
                        .filter(|id| *id != candidate.id)
                        .collect();
                    (is_sidebar_pane(candidate)
                        && candidate.pane_columns.is_some_and(|cols| cols >= 10)
                        && sidebar_dock_verdict(candidate, &before_panes, &excluded)
                            == Some(SidebarDock::Docked))
                    .then_some(candidate)
                })
            } else {
                None
            };
            // Action clients can cross-talk responses: discover the mount even
            // when the command misreports failure, and never close a hint alone.
            let (hint, spawn_err) =
                match self.new_sidebar_pane(opts, tab_id, anchor.map(|pane| pane.id)) {
                    Ok(hint) => (hint, None),
                    Err(err) => (None, Some(err)),
                };
            let mounted = match self.wait_for_mounted_sidebar(
                opts,
                tab_id,
                &before,
                hint.as_ref(),
                anchor,
                viewed_tabs,
            ) {
                Ok(Some(mounted)) => mounted,
                result => {
                    let cleanup = self.cleanup_missed_add(opts, tab_id, &before, hint, viewed_tabs);
                    if let Err(err) = result.map(|_| ()).and(cleanup) {
                        if let Some(raw_id) = fallback_misdocked {
                            self.cleanup_failed_add(opts, raw_id);
                        }
                        return Err(err);
                    }
                    if anchor.is_some() {
                        return Err(super::output_error(format!(
                            "anchored sidebar split never mounted in tab {tab_id}; \
                             floating panes being shown can prevent a tiled split"
                        )));
                    }
                    if let Some(raw_id) = fallback_misdocked {
                        return Ok(AddedSidebar {
                            pane: PaneId::from(ZellijPaneId::Terminal(raw_id)),
                            dock: DockOutcome::Misdocked,
                        });
                    }
                    last_error = Some(spawn_err.unwrap_or_else(|| {
                        super::output_error(format!(
                            "new-pane never mounted a sidebar pane in tab {tab_id}"
                        ))
                    }));
                    continue;
                }
            };
            let raw_id = match mounted {
                MountOutcome::Intended(raw_id) => raw_id,
                MountOutcome::WrongTab(raw_id) => {
                    self.cleanup_failed_add(opts, raw_id);
                    if let Some(previous) = fallback_misdocked {
                        self.cleanup_failed_add(opts, previous);
                    }
                    return Err(super::output_error(format!(
                        "new-pane mounted sidebar terminal_{raw_id} outside target tab {tab_id}"
                    )));
                }
            };
            let pane = PaneId::from(ZellijPaneId::Terminal(raw_id));
            if let Some(previous) = fallback_misdocked.take() {
                self.cleanup_failed_add(opts, previous);
            }
            if anchor.is_some() {
                // Its old neighbour still owns the left edge. Verify geometry
                // only after the executor closes that neighbour.
                return Ok(AddedSidebar {
                    pane,
                    dock: DockOutcome::Misdocked,
                });
            }
            let geometry: Result<_> = (|| {
                self.converge_sidebar_geometry_with(opts, tab_id, raw_id, true, viewed_tabs)?;
                let dock = self.sidebar_dock_outcome(
                    &opts.session_name,
                    &opts.workspace_id,
                    tab_id,
                    raw_id,
                    viewed_tabs,
                )?;
                let retry = dock == DockOutcome::Misdocked
                    && attempt + 1 < ADD_DOCK_ATTEMPTS
                    && self.misdocked_add_should_retry(opts, tab_id, raw_id, viewed_tabs)?;
                Ok((dock, retry))
            })();
            let (dock, retry) = match geometry {
                Ok(outcome) => outcome,
                Err(err) => {
                    self.cleanup_failed_add(opts, raw_id);
                    return Err(err);
                }
            };
            if retry {
                fallback_misdocked = Some(raw_id);
                continue;
            }
            if dock == DockOutcome::Misdocked {
                tracing::warn!(
                    session = %opts.session_name,
                    tab = tab_id,
                    pane = %pane,
                    "sidebar add mounted a working pane but could not verify a full-height left dock",
                );
            }
            return Ok(AddedSidebar { pane, dock });
        }
        Err(last_error.unwrap_or_else(|| {
            super::output_error(format!(
                "new-pane never mounted a docked sidebar pane in tab {tab_id}"
            ))
        }))
    }

    /// Poll native geometry for a newly mounted sidebar in the stable target
    /// tab, and inside the old sidebar's column band for an anchored split.
    fn wait_for_mounted_sidebar(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
        before: &HashSet<u64>,
        hint: Option<&ZellijPaneId>,
        anchor: Option<&PaneTopologyPane>,
        viewed_tabs: &HashSet<u64>,
    ) -> Result<Option<MountOutcome>> {
        let hint_raw = hint.copied().and_then(ZellijPaneId::terminal_id);
        let deadline = Instant::now() + MOUNT_POLL_TIMEOUT;
        loop {
            let panes = self
                .structural_geometry_listing(&opts.session_name, &opts.workspace_id, viewed_tabs)?
                .panes;
            let tab_position = panes
                .iter()
                .find(|pane| pane.stable_tab_id == Some(tab_id))
                .map(|pane| pane.tab_position)
                .ok_or_else(|| super::output_error(format!("tab {tab_id} is no longer listed")))?;
            if let Some(id) = mounted_sidebar_pane(&panes, tab_position, before, hint_raw) {
                let in_anchor = anchor.is_none_or(|anchor| {
                    sidebar_pane(&panes, tab_id, id)
                        .and_then(|pane| pane.pane_x)
                        .zip(anchor.pane_x.zip(anchor.pane_columns))
                        .is_some_and(|(x, (left, cols))| x > left && x < left.saturating_add(cols))
                });
                if in_anchor {
                    return Ok(Some(MountOutcome::Intended(id)));
                }
            }
            if let Some(id) = wrong_tab_mounted_sidebar_pane(&panes, tab_position, before, hint_raw)
            {
                return Ok(Some(MountOutcome::WrongTab(id)));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(MOUNT_POLL_STEP);
        }
    }

    fn cleanup_missed_add(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
        before: &HashSet<u64>,
        hint: Option<ZellijPaneId>,
        viewed_tabs: &HashSet<u64>,
    ) -> Result<()> {
        let listing =
            self.structural_geometry_listing(&opts.session_name, &opts.workspace_id, viewed_tabs);
        if let Ok(listing) = &listing {
            for pane in &listing.panes {
                if pane.stable_tab_id == Some(tab_id)
                    && is_sidebar_pane(pane)
                    && !before.contains(&pane.id)
                {
                    self.cleanup_failed_add(opts, pane.id);
                }
            }
        }
        // A targeted split can leave its PTY unplaced. Its hint licenses only
        // a serve-pair kill, not a pane close; an existing hint is cross-talk.
        if let Some(id) = hint
            .and_then(ZellijPaneId::terminal_id)
            .filter(|id| !before.contains(id))
        {
            super::super::recovery::kill_sidebar_serve_for_pane(
                opts.workspace_id.as_str(),
                &opts.session_name,
                &PaneId::from(ZellijPaneId::Terminal(id)),
                MuxName::Zellij,
            );
        }
        listing.map(|_| ())
    }

    /// Move a pane to its tab's left column — the dock position the layout
    /// gives a sidebar at birth.
    pub(super) fn dock_left(&self, session: &str, pane_id: &str) -> Result<()> {
        self.zellij_action(session)
            .args([
                "move-pane".to_owned(),
                "left".to_owned(),
                "--pane-id".to_owned(),
                pane_id.to_owned(),
            ])
            .run()
            .map(|_| ())
    }

    /// Read native geometry without a cached-roster fallback. Width judgment
    /// uses exactly the live client-view snapshot's stable tabs, never a latch
    /// carried over from the presence cache.
    pub(super) fn structural_geometry_listing(
        &self,
        session: &str,
        workspace_id: &WorkspaceId,
        viewed_tabs: &HashSet<u64>,
    ) -> Result<PaneTopologyCache> {
        let (mut listing, _) = self.read_topology(
            Some(session),
            None,
            Some(workspace_id),
            None,
            PaneReadConsistency::RequireAuthoritative,
            RECONCILE_LIST_TIMEOUT,
        )?;
        for pane in &mut listing.panes {
            if !pane.is_plugin && pane.stable_tab_id.is_none() {
                return Err(super::output_error(format!(
                    "terminal_{} has no stable tab id in the live pane list",
                    pane.id
                )));
            }
            pane.tab_viewed = pane
                .stable_tab_id
                .is_some_and(|id| viewed_tabs.contains(&id));
        }
        Ok(listing)
    }

    /// Seed the attached client's initial tab with its deterministic work pane.
    /// Hidden tabs have no focus state and need no birth-time normalization.
    fn finalize_birth_focus(&self, session: &str, workspace_id: &WorkspaceId) {
        let Ok(view) = self.client_view(crate::mux::ClientFocusOptions {
            session_name: Some(session.to_owned()),
            command_timeout: Some(RECONCILE_LIST_TIMEOUT),
        }) else {
            return;
        };
        let [viewed] = view.viewed_panes.as_slice() else {
            return;
        };
        let Some(viewed) = ZellijPaneId::try_from(viewed)
            .ok()
            .and_then(ZellijPaneId::terminal_id)
        else {
            return;
        };
        let Ok(panes) = self.raw_listed_panes(session, RECONCILE_LIST_TIMEOUT) else {
            return;
        };
        let panes = panes
            .into_iter()
            .map(PaneTopologyPane::from)
            .collect::<Vec<_>>();
        let Some(tab_position) = panes
            .iter()
            .find(|pane| pane.id == viewed && pane.is_live_terminal())
            .map(|pane| pane.tab_position)
        else {
            return;
        };
        let Some(work) = leftmost_live_work_pane(&panes, tab_position) else {
            return;
        };
        let Ok(runtime) = self.runtime_paths_for_workspace(workspace_id.clone()) else {
            return;
        };
        let _ = super::backend::execute_focus_restoration(
            self,
            &runtime,
            session,
            &PaneId::from(ZellijPaneId::Terminal(work)),
            None,
            crate::mux::focus_anchor::FocusDispatchRetries {
                attempts: super::FOCUS_RESTORE_ATTEMPTS,
                delay: super::FOCUS_RESTORE_RETRY_DELAY,
            },
        );
    }

    /// Repair a kept sidebar in place. Moves must reduce live x, and the tiled
    /// pane count bounds them. A narrow nested row can be stacked into the right
    /// column. Width is judged only after a verified dock, on a viewed tab.
    /// Returns whether any repair action succeeded; live read failures propagate.
    pub(super) fn converge_sidebar_geometry(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
        raw_id: u64,
        viewed_tabs: &HashSet<u64>,
    ) -> Result<bool> {
        self.converge_sidebar_geometry_with(opts, tab_id, raw_id, false, viewed_tabs)
    }

    fn converge_sidebar_geometry_with(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
        raw_id: u64,
        stack_multicolumn_work: bool,
        viewed_tabs: &HashSet<u64>,
    ) -> Result<bool> {
        let pane_raw = ZellijPaneId::Terminal(raw_id).action_target();
        let mut changed = false;
        let mut listing =
            self.structural_geometry_listing(&opts.session_name, &opts.workspace_id, viewed_tabs)?;
        let Some(mut x) = sidebar_pane(&listing.panes, tab_id, raw_id).and_then(|pane| pane.pane_x)
        else {
            return Ok(false);
        };
        let mut swaps_remaining = listing
            .panes
            .iter()
            .filter(|pane| pane.stable_tab_id == Some(tab_id) && pane.is_terminal())
            .count()
            .saturating_sub(1);
        'moves: while x > 0 && swaps_remaining > 0 {
            if self.dock_left(&opts.session_name, &pane_raw).is_err() {
                break;
            }
            changed = true;
            swaps_remaining -= 1;
            let deadline = Instant::now() + GEOMETRY_REPAIR_SETTLE;
            loop {
                let next = self.structural_geometry_listing(
                    &opts.session_name,
                    &opts.workspace_id,
                    viewed_tabs,
                )?;
                let Some(next_x) =
                    sidebar_pane(&next.panes, tab_id, raw_id).and_then(|pane| pane.pane_x)
                else {
                    break 'moves;
                };
                if next_x < x {
                    listing = next;
                    x = next_x;
                    break;
                }
                if Instant::now() >= deadline {
                    break 'moves;
                }
                std::thread::sleep(MOUNT_POLL_STEP);
            }
        }

        let excluded = HashSet::new();
        let verdict = sidebar_pane(&listing.panes, tab_id, raw_id)
            .and_then(|pane| sidebar_dock_verdict(pane, &listing.panes, &excluded));
        if verdict == Some(SidebarDock::NestedRow)
            && self.stack_nested_work_panes(
                opts,
                tab_id,
                raw_id,
                viewed_tabs,
                stack_multicolumn_work,
            )?
        {
            changed = true;
            let deadline = Instant::now() + GEOMETRY_REPAIR_SETTLE;
            loop {
                let next = self.structural_geometry_listing(
                    &opts.session_name,
                    &opts.workspace_id,
                    viewed_tabs,
                )?;
                let docked = sidebar_pane(&next.panes, tab_id, raw_id)
                    .and_then(|pane| sidebar_dock_verdict(pane, &next.panes, &excluded))
                    == Some(SidebarDock::Docked);
                listing = next;
                if docked {
                    break;
                }
                if Instant::now() >= deadline {
                    return Ok(changed);
                }
                std::thread::sleep(MOUNT_POLL_STEP);
            }
        }
        let verdict = sidebar_pane(&listing.panes, tab_id, raw_id)
            .and_then(|pane| sidebar_dock_verdict(pane, &listing.panes, &excluded));
        if verdict != Some(SidebarDock::Docked) {
            return Ok(changed);
        }
        let sync = WidthSyncOptions {
            session_name: opts.session_name.clone(),
            workspace_id: opts.workspace_id.clone(),
            target: opts.target,
        };
        changed |= self.converge_sidebar_width(&sync, tab_id, raw_id, viewed_tabs)?;
        Ok(changed)
    }

    /// Converge one sidebar to its current viewed tab's width target.
    /// Returns whether a resize action succeeded.
    pub(super) fn converge_sidebar_width(
        &self,
        opts: &WidthSyncOptions,
        tab_id: u64,
        raw_id: u64,
        viewed_tabs: &HashSet<u64>,
    ) -> Result<bool> {
        let listing =
            self.structural_geometry_listing(&opts.session_name, &opts.workspace_id, viewed_tabs)?;
        self.converge_sidebar_widths_stepwise(
            opts,
            tab_id,
            raw_id,
            Some(&listing.panes),
            viewed_tabs,
        )
    }

    pub(super) fn sidebar_dock_outcome(
        &self,
        session: &str,
        workspace_id: &WorkspaceId,
        tab_id: u64,
        raw_id: u64,
        viewed_tabs: &HashSet<u64>,
    ) -> Result<DockOutcome> {
        std::thread::sleep(DOCK_VERIFY_SETTLE);
        let listing = self.structural_geometry_listing(session, workspace_id, viewed_tabs)?;
        let verdict = sidebar_pane(&listing.panes, tab_id, raw_id)
            .and_then(|pane| sidebar_dock_verdict(pane, &listing.panes, &HashSet::new()));
        Ok(if verdict == Some(SidebarDock::Docked) {
            DockOutcome::Docked
        } else {
            DockOutcome::Misdocked
        })
    }

    fn misdocked_add_should_retry(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
        raw_id: u64,
        viewed_tabs: &HashSet<u64>,
    ) -> Result<bool> {
        let listing =
            self.structural_geometry_listing(&opts.session_name, &opts.workspace_id, viewed_tabs)?;
        let panes = listing.panes;
        let Some(sidebar) = sidebar_pane(&panes, tab_id, raw_id) else {
            return Ok(false);
        };
        Ok(
            match sidebar_dock_verdict(sidebar, &panes, &HashSet::new()) {
                Some(SidebarDock::SwapReachable) => true,
                Some(SidebarDock::NestedRow) => {
                    repairable_nested_work_pane_ids(sidebar, &panes).is_some()
                }
                Some(SidebarDock::Docked) | None => false,
            },
        )
    }

    /// Existing user layouts stack only the narrow one-column repair shape.
    /// An add can stack every work pane because it introduced the nested shape.
    fn stack_nested_work_panes(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
        raw_id: u64,
        viewed_tabs: &HashSet<u64>,
        allow_multicolumn: bool,
    ) -> Result<bool> {
        let deadline = Instant::now() + GEOMETRY_REPAIR_SETTLE;
        let work = loop {
            let listing = self.structural_geometry_listing(
                &opts.session_name,
                &opts.workspace_id,
                viewed_tabs,
            )?;
            let panes = listing.panes;
            let Some(sidebar) = sidebar_pane(&panes, tab_id, raw_id) else {
                return Ok(false);
            };
            let work = if allow_multicolumn {
                nested_work_pane_ids(sidebar, &panes)
            } else {
                repairable_nested_work_pane_ids(sidebar, &panes)
            };
            if let Some(work) = work {
                break work;
            }
            if sidebar_dock_verdict(sidebar, &panes, &HashSet::new()) == Some(SidebarDock::Docked)
                || Instant::now() >= deadline
            {
                return Ok(false);
            }
            std::thread::sleep(MOUNT_POLL_STEP);
        };
        let mut args = vec!["stack-panes".to_owned(), "--".to_owned()];
        args.extend(
            work.iter()
                .map(|id| ZellijPaneId::Terminal(*id).action_target()),
        );
        match self.zellij_action(&opts.session_name).args(args).run() {
            Ok(_) => Ok(true),
            Err(err) => {
                tracing::warn!(
                    session = %opts.session_name,
                    tab = tab_id,
                    pane = raw_id,
                    error = %err,
                    "sidebar geometry repair could not stack work panes into the right column",
                );
                Ok(false)
            }
        }
    }

    /// Undo a failed add for a pane that a fresh listing already proved is a
    /// newly-created sidebar in the target tab: best-effort close it, then kill
    /// the spawned serve pair attributed to that pane. A stdout-only
    /// `new-pane` hint never reaches this path.
    pub(super) fn cleanup_failed_add(&self, opts: &SidebarPaneOptions, raw_id: u64) {
        let pane = PaneId::from(ZellijPaneId::Terminal(raw_id));
        let _ = self.close_pane(&opts.session_name, &pane);
        let killed = super::super::recovery::kill_sidebar_serve_for_pane(
            opts.workspace_id.as_str(),
            &opts.session_name,
            &pane,
            MuxName::Zellij,
        );
        if killed > 0 {
            tracing::debug!(
                session = %opts.session_name,
                pane = %pane.as_str(),
                killed,
                "sidebar add cleanup: reaped the unmounted serve pair",
            );
        }
    }

    /// Whether `session` has a persistent attached client. A real
    /// attach holds one client id across the confirmation window. Zellij's
    /// transient bootstrap/action clients churn or drain within it, so an
    /// unstable or empty roster reads detached and the add defers.
    pub(super) fn session_has_attached_client(&self, session: &str) -> bool {
        Self::confirm_attached_client(|| {
            self.client_view(crate::mux::ClientFocusOptions {
                session_name: Some(session.to_owned()),
                ..Default::default()
            })
            .map(|view| {
                view.clients
                    .iter()
                    .filter_map(|client| match &client.client_id {
                        MuxClientId::Zellij(id) => Some(*id),
                        MuxClientId::Tmux(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
        })
    }

    fn confirm_attached_client(probe: impl Fn() -> BTreeSet<u32>) -> bool {
        let mut probes = vec![probe()];
        if !stable_client_present(&probes) {
            return false;
        }
        let deadline = Instant::now() + CLIENT_CONFIRM_WINDOW;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(remaining.min(CLIENT_PROBE_SETTLE));
            probes.push(probe());
            if !stable_client_present(&probes) {
                return false;
            }
        }
        stable_client_present(&probes)
    }

    pub(super) fn focused_terminal_client_ids(&self, session: &str) -> BTreeSet<u32> {
        self.client_view(crate::mux::ClientFocusOptions {
            session_name: Some(session.to_owned()),
            ..Default::default()
        })
        .map(|view| terminal_client_ids(&view))
        .unwrap_or_default()
    }

    /// Spawn the layout's sidebar command. A replacement splits its old docked
    /// anchor unfocused; a plain add leaves placement unspecified because 0.44.3
    /// directional placement with `--tab-id` can leave its PTY unmounted.
    /// Returns the created pane id Zellij prints (for example, `terminal_58`) as
    /// a hint only: concurrent action clients can cross-talk.
    pub(super) fn new_sidebar_pane(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
        anchor: Option<u64>,
    ) -> Result<Option<ZellijPaneId>> {
        let mut spec = self.zellij_action(&opts.session_name);
        let mut args = vec!["new-pane".to_owned()];
        if let Some(anchor) = anchor {
            spec = spec.env("ZELLIJ_PANE_ID", anchor.to_string());
            args.extend([
                "--direction".to_owned(),
                "right".to_owned(),
                "--no-focus".to_owned(),
            ]);
        } else {
            args.extend(["--tab-id".to_owned(), tab_id.to_string()]);
        }
        args.extend([
            "--name".to_owned(),
            SIDEBAR_CHROME_TITLE.to_owned(),
            "--borderless".to_owned(),
            "true".to_owned(),
            "--close-on-exit".to_owned(),
            "--cwd".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
            "--".to_owned(),
        ]);
        let mut command = vec![opts.rimz_bin.to_string_lossy().into_owned()];
        command.extend(sidebar_serve_args(MuxName::Zellij, opts));
        args.extend(command);
        let output = spec.args(args).run()?;
        Ok(parse_new_pane_id(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Converge one sidebar with live geometry feedback between resize steps.
    /// Returns whether a resize succeeded.
    pub(super) fn converge_sidebar_widths_stepwise(
        &self,
        opts: &WidthSyncOptions,
        tab_id: u64,
        raw_id: u64,
        initial: Option<&[PaneTopologyPane]>,
        viewed_tabs: &HashSet<u64>,
    ) -> Result<bool> {
        const RESIZE_MAX_STEPS: u32 = 64;
        const TRANSIENT_MAX_RETRIES: u8 = 2;
        let mut initial = initial;
        let mut last_cols: Option<u64> = None;
        let mut last_step_grow = None;
        let mut no_progress_retry = false;
        let mut transient_retries = 0;
        let mut reverse_spent = false;
        let mut resized = false;

        for _ in 0..RESIZE_MAX_STEPS {
            if reverse_spent {
                break;
            }
            let owned_panes;
            let panes = if let Some(panes) = initial.take() {
                panes
            } else {
                owned_panes = self
                    .structural_geometry_listing(
                        &opts.session_name,
                        &opts.workspace_id,
                        viewed_tabs,
                    )?
                    .panes;
                &owned_panes
            };
            let Some(sidebar) = sidebar_pane(panes, tab_id, raw_id) else {
                break;
            };
            let Some((cols, view_cols)) = sidebar
                .pane_columns
                .zip(tab_view_cols(panes, sidebar.tab_position))
            else {
                break;
            };
            let target_cols = u64::from(
                opts.target
                    .cols(Some(u16::try_from(view_cols).unwrap_or(u16::MAX)))
                    .get(),
            );
            if !sidebar_width_off_spec(cols, target_cols, zellij_resize_stop_step_cols(view_cols)) {
                break;
            }
            // Defensive only: reverse once if a future backend delta exceeds
            // the ceiling stop estimate.
            let regressed = last_cols
                .is_some_and(|last_cols| width_step_regressed(last_cols, cols, target_cols));
            let grow = cols < target_cols;
            let no_progress =
                last_cols
                    .zip(last_step_grow)
                    .is_some_and(|(last_cols, last_step_grow)| {
                        if last_step_grow {
                            cols <= last_cols
                        } else {
                            cols >= last_cols
                        }
                    });
            if no_progress {
                // Allow one settle and a second native read before treating
                // an unchanged width as pinned at a backend minimum.
                if !no_progress_retry {
                    no_progress_retry = true;
                    std::thread::sleep(MOUNT_POLL_STEP);
                    continue;
                }
                break;
            }
            no_progress_retry = false;
            last_cols = Some(cols);
            last_step_grow = Some(grow);

            if self
                .resize_sidebar_step(
                    &opts.session_name,
                    &ZellijPaneId::Terminal(raw_id).action_target(),
                    if grow { "increase" } else { "decrease" },
                )
                .is_err()
            {
                if transient_retries < TRANSIENT_MAX_RETRIES {
                    transient_retries += 1;
                    // A failed CLI may still have reached the server. Read
                    // native geometry before choosing the next step.
                    last_cols = None;
                    last_step_grow = None;
                    continue;
                }
                break;
            }
            resized = true;
            reverse_spent = regressed;
        }
        Ok(resized)
    }

    pub(super) fn resize_sidebar_step(
        &self,
        session: &str,
        pane_id: &str,
        direction: &str,
    ) -> Result<()> {
        self.zellij_action(session)
            .args([
                "resize".to_owned(),
                direction.to_owned(),
                "right".to_owned(),
                "--pane-id".to_owned(),
                pane_id.to_owned(),
            ])
            .run()
            .map(|_| ())
    }

    /// Block until Zellij has materialized the layout's sidebar pane alongside a
    /// second live terminal, so the caller's temp layout file stays on disk
    /// until Zellij has demonstrably parsed it. Returns `true` once that signal
    /// appears, `false` if the [`SIDEBAR_LAYOUT_TIMEOUT`] ceiling elapses first.
    /// Reads the server's pane list, independent of the presence-plugin cache.
    ///
    /// The predicate gates on *our* sidebar chrome pane (a default/fallback
    /// birth carries none) counted with the same `is_live_terminal` filter the
    /// roster applies, so "materialized" means the server listed the two live
    /// panes — no held/exited pane slips the gate.
    pub(super) fn wait_for_sidebar_layout(&self, session_name: &str) -> bool {
        let deadline = Instant::now() + SIDEBAR_LAYOUT_TIMEOUT;
        while Instant::now() < deadline {
            if self.session_state(session_name) != SessionLiveness::Live {
                return false;
            }
            if let Ok(panes) = self
                .raw_listed_panes(session_name, RECONCILE_LIST_TIMEOUT)
                .map(|panes| {
                    panes
                        .into_iter()
                        .map(PaneTopologyPane::from)
                        .collect::<Vec<_>>()
                })
                && panes.iter().any(is_sidebar_pane)
                && panes.iter().filter(|pane| pane.is_live_terminal()).count() >= 2
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Whether `session` already holds a tab named `tab_name`. A RimZ background
    /// view is idempotent on its name, so a relaunch into a session carrying it
    /// is skipped.
    pub(super) fn session_has_named_tab(&self, session: &str, tab_name: &str) -> Result<bool> {
        let config = crate::config::MachineConfig::load_lenient();
        let theme = &config.theme;
        Ok(self
            .list_tabs(session)?
            .iter()
            .any(|tab| crate::theme::strip_status_glyph_suffix(&tab.name, theme) == tab_name))
    }
}

fn stable_client_present(probes: &[BTreeSet<u32>]) -> bool {
    let Some((first, rest)) = probes.split_first() else {
        return false;
    };
    if first.is_empty() {
        return false;
    }
    let common = rest.iter().fold(first.clone(), |common, ids| {
        common.intersection(ids).copied().collect()
    });
    !common.is_empty()
}

fn birth_term(current: Option<&str>) -> Option<&'static str> {
    match current {
        Some(term) if !term.is_empty() => None,
        _ => Some("xterm-256color"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{birth_term, stable_client_present};

    #[test]
    fn stable_client_present_requires_one_id_across_every_probe() {
        assert!(!stable_client_present(&[]), "no probes is detached");
        assert!(
            !stable_client_present(&[BTreeSet::new()]),
            "empty first probe is detached"
        );
        assert!(
            stable_client_present(&[BTreeSet::from([7])]),
            "one non-empty probe seeds a candidate"
        );
        assert!(
            stable_client_present(&[
                BTreeSet::from([1, 7]),
                BTreeSet::from([7, 8]),
                BTreeSet::from([7]),
            ]),
            "shared client id survives churn"
        );
        assert!(
            !stable_client_present(&[
                BTreeSet::from([1]),
                BTreeSet::from([2]),
                BTreeSet::from([3]),
            ]),
            "churn without a stable id is detached"
        );
        assert!(
            !stable_client_present(&[BTreeSet::from([1]), BTreeSet::new()]),
            "drained roster is detached"
        );
    }

    #[test]
    fn birth_term_fills_only_a_missing_value() {
        assert_eq!(birth_term(None), Some("xterm-256color"));
        assert_eq!(birth_term(Some("")), Some("xterm-256color"));
        assert_eq!(birth_term(Some("xterm-ghostty")), None);
        assert_eq!(birth_term(Some("alacritty")), None);
    }
}
