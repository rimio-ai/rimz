//! Zellij sidebar birth, in-place recovery, and geometry convergence.

use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

use super::layout::{TempLayoutFile, render_session_layout};
use super::pane_topology::{PaneTopologyCache, PaneTopologyPane, ZellijPaneId};
use super::parse::{parse_client_view, terminal_client_ids};
use super::raw_pane::{
    SidebarDock, is_sidebar_pane, leftmost_live_work_pane, mounted_sidebar_pane,
    nested_work_pane_ids, parse_new_pane_id, repairable_nested_work_pane_ids, sidebar_dock_verdict,
    tab_view_cols, wrong_tab_mounted_sidebar_pane,
};
use super::socket::{socket_headroom_with_xdg_override, stderr_reports_socket_overflow};
use super::{
    MOUNT_POLL_STEP, MOUNT_POLL_TIMEOUT, SIDEBAR_LAYOUT_TIMEOUT, TOPOLOGY_CACHE_POLL_STEP,
    ZellijBackend,
};
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::mux::width::{
    sidebar_width_off_spec, width_step_regressed, zellij_resize_stop_step_cols,
};
use crate::mux::{
    DaemonView, MuxBackend, MuxErr, PaneReadConsistency, PresencePluginOptions, Result,
    SessionLiveness, SidebarPaneOptions, WidthSyncOptions, sidebar_serve_args,
};
use crate::pane::SIDEBAR_CHROME_TITLE;
use crate::sidebar::timing::RECONCILE_LIST_TIMEOUT;
use crate::sidebar::timing::unix_now_ms;

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

fn sidebar_pane(
    panes: &[PaneTopologyPane],
    tab_position: u64,
    raw_id: u64,
) -> Option<&PaneTopologyPane> {
    // Plugin panes share the integer id space (`plugin_1` beside
    // `terminal_1`), so the terminal filter is load-bearing here.
    panes
        .iter()
        .find(|pane| pane.is_terminal() && pane.tab_position == tab_position && pane.id == raw_id)
}

impl ZellijBackend {
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
        let body = render_session_layout(opts, daemon, &opts.resume_tabs)?;
        let layout = TempLayoutFile::new(body)?;
        let mut option_args = vec![
            "attach".to_owned(),
            "--create-background".to_owned(),
            opts.session_name.clone(),
            "options".to_owned(),
        ];
        option_args.extend(self.zellij_options_args_probed(&opts.config.zellij));
        option_args.extend([
            "--default-cwd".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
            "--default-layout".to_owned(),
            layout.path().to_string_lossy().into_owned(),
        ]);
        let mut spec = self.cmd().args(option_args);
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
        let spawn = || -> Result<bool> {
            let output = spec.clone().cwd(opts.cwd.clone()).output_raw()?;
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

        let created = spawn()?;
        self.ensure_birth_presence_plugin(opts)?;
        if self.wait_for_sidebar_layout(&opts.session_name, &opts.workspace_id) {
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
            self.ensure_birth_presence_plugin(opts)?;
            if self.wait_for_sidebar_layout(&opts.session_name, &opts.workspace_id) {
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

    fn ensure_birth_presence_plugin(&self, opts: &SidebarPaneOptions) -> Result<()> {
        let wasm = super::ensure_presence_plugin_artifact().ok_or_else(|| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: "Zellij presence plugin artifact is unavailable; run `rimz doctor` or use the tmux backend".to_owned(),
        })?;
        let machine_config = crate::config::MachineConfig::load_lenient();
        let presence = PresencePluginOptions {
            session_name: opts.session_name.clone(),
            workspace_id: opts.workspace_id.clone(),
            wasm,
            rimz_bin: self
                .state_paths_for_workspace(opts.workspace_id.clone())?
                .room_bin,
            converge: false,
            focus_key: machine_config.sidebar.focus_key_label().map(str::to_owned),
            zoom_key: machine_config.sidebar.zoom_key_label().map(str::to_owned),
            focus_follows_mouse: opts.config.zellij.focus_follows_mouse,
            mouse_click_through: opts.config.zellij.mouse_click_through,
        };
        let floor_ms = unix_now_ms();
        self.ensure_presence_plugin_for(&presence)?;
        self.retire_proven_presence_plugin_for(
            &presence,
            floor_ms,
            Duration::ZERO,
            TOPOLOGY_CACHE_POLL_STEP,
        );
        Ok(())
    }

    /// Inject a left-docked sidebar into a live tab without a rebirth: mount it
    /// through a stable tab id, discover the pane through topology, converge its
    /// geometry, and verify the full-height dock. A narrow nested-row shape is
    /// repairable by stacking the work panes into the right column; other
    /// persistent mis-docks are kept and reported rather than leaking a
    /// paneless renderer or leaving the tab sidebar-less.
    pub(super) fn add_sidebar_to_tab(
        &self,
        opts: &SidebarPaneOptions,
        tab_position: u64,
        width_floor: Option<u64>,
    ) -> Result<AddedSidebar> {
        let mut last_error = None;
        let mut fallback_misdocked: Option<u64> = None;
        for attempt in 0..ADD_DOCK_ATTEMPTS {
            let before_panes = self
                .read_topology(
                    Some(&opts.session_name),
                    None,
                    Some(&opts.workspace_id),
                    None,
                    PaneReadConsistency::RequireAuthoritative,
                    RECONCILE_LIST_TIMEOUT,
                )?
                .panes;
            let before: HashSet<u64> = before_panes
                .iter()
                .filter(|pane| pane.is_terminal())
                .map(|pane| pane.id)
                .collect();
            let target_pane =
                leftmost_live_work_pane(&before_panes, tab_position).ok_or_else(|| {
                    MuxErr::Output {
                        program: "zellij".to_owned(),
                        reason: format!("tab {tab_position} has no stable work pane to target"),
                    }
                })?;
            let target_pane = PaneId::from(ZellijPaneId::Terminal(target_pane));
            let tab_id = self.tab_id_for_pane(&opts.session_name, &target_pane)?;
            // A `new-pane` failure is remembered, not fatal yet: concurrent
            // action clients can cross-talk responses, so the command can
            // misreport while the pane is still created — discovery gets its
            // window either way.
            let floor_ms = unix_now_ms();
            let (hint, spawn_err) = match self.new_sidebar_pane(opts, tab_id) {
                Ok(hint) => (hint, None),
                Err(err) => (None, Some(err)),
            };
            let Some(mounted) = self.wait_for_mounted_sidebar(
                &opts.session_name,
                tab_position,
                &before,
                hint.as_ref(),
                floor_ms,
                &opts.workspace_id,
            ) else {
                if let Some(raw_id) = fallback_misdocked {
                    return Ok(AddedSidebar {
                        pane: PaneId::from(ZellijPaneId::Terminal(raw_id)),
                        dock: DockOutcome::Misdocked,
                    });
                }
                last_error = Some(spawn_err.unwrap_or_else(|| MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!("new-pane never mounted a sidebar pane in tab {tab_position}"),
                }));
                continue;
            };
            let raw_id = match mounted {
                MountOutcome::Intended(raw_id) => raw_id,
                MountOutcome::WrongTab(raw_id) => {
                    self.cleanup_failed_add(opts, raw_id);
                    return Err(MuxErr::Output {
                        program: "zellij".to_owned(),
                        reason: format!(
                            "new-pane mounted sidebar terminal_{raw_id} outside target tab {tab_position}"
                        ),
                    });
                }
            };
            let pane = PaneId::from(ZellijPaneId::Terminal(raw_id));
            if let Some(previous) = fallback_misdocked.take() {
                self.cleanup_failed_add(opts, previous);
            }
            let floor = self.converge_added_sidebar_geometry(
                opts,
                tab_position,
                raw_id,
                floor_ms,
                width_floor,
            );
            match self.sidebar_dock_outcome(
                &opts.session_name,
                &opts.workspace_id,
                tab_position,
                raw_id,
                floor,
            ) {
                DockOutcome::Docked => {
                    return Ok(AddedSidebar {
                        pane,
                        dock: DockOutcome::Docked,
                    });
                }
                DockOutcome::Misdocked
                    if attempt + 1 < ADD_DOCK_ATTEMPTS
                        && self.misdocked_add_should_retry(opts, tab_position, raw_id, floor) =>
                {
                    fallback_misdocked = Some(raw_id);
                }
                DockOutcome::Misdocked => {
                    let pane_id = ZellijPaneId::Terminal(raw_id).action_target();
                    tracing::warn!(
                        session = %opts.session_name,
                        tab = tab_position,
                        pane = %pane_id,
                        "sidebar add mounted a working pane but could not verify a full-height left dock",
                    );
                    return Ok(AddedSidebar {
                        pane,
                        dock: DockOutcome::Misdocked,
                    });
                }
            }
        }
        Err(last_error.unwrap_or_else(|| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("new-pane never mounted a docked sidebar pane in tab {tab_position}"),
        }))
    }

    /// Bounded poll for the sidebar pane an add just spawned to mount in
    /// `tab_position`. Returns its raw numeric id, or `None` once
    /// [`MOUNT_POLL_TIMEOUT`] elapses — the mount was dropped.
    fn wait_for_mounted_sidebar(
        &self,
        session: &str,
        tab_position: u64,
        before: &HashSet<u64>,
        hint: Option<&ZellijPaneId>,
        floor_ms: u64,
        workspace_id: &WorkspaceId,
    ) -> Option<MountOutcome> {
        let hint_raw = hint.copied().and_then(ZellijPaneId::terminal_id);
        let deadline = Instant::now() + MOUNT_POLL_TIMEOUT;
        loop {
            if let Ok(panes) = self.topology_panes_for_workspace(
                session,
                workspace_id,
                Some(floor_ms),
                RECONCILE_LIST_TIMEOUT,
            ) {
                if let Some(id) = mounted_sidebar_pane(&panes, tab_position, before, hint_raw) {
                    return Some(MountOutcome::Intended(id));
                }
                if let Some(id) =
                    wrong_tab_mounted_sidebar_pane(&panes, tab_position, before, hint_raw)
                {
                    return Some(MountOutcome::WrongTab(id));
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(MOUNT_POLL_STEP);
        }
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

    /// Read completed structural geometry directly from Zellij, using a fresh
    /// presence publication only when the server listing is unavailable.
    pub(super) fn structural_geometry_listing(
        &self,
        session: &str,
        workspace_id: &WorkspaceId,
        min_topology_produced_at_ms: Option<u64>,
    ) -> Result<PaneTopologyCache> {
        self.read_topology(
            Some(session),
            None,
            Some(workspace_id),
            min_topology_produced_at_ms,
            PaneReadConsistency::PreferAuthoritative,
            RECONCILE_LIST_TIMEOUT,
        )
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
        let Ok(panes) =
            self.topology_panes_for_workspace(session, workspace_id, None, RECONCILE_LIST_TIMEOUT)
        else {
            return;
        };
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
        let _ = crate::sidebar::focus_anchor::execute_action(
            self,
            &runtime,
            session,
            PaneId::from(ZellijPaneId::Terminal(work)),
            crate::sidebar::focus_anchor::FocusOrigin::User,
            None,
            crate::sidebar::focus_anchor::FocusDispatchRetries {
                attempts: super::FOCUS_RESTORE_ATTEMPTS,
                delay: super::FOCUS_RESTORE_RETRY_DELAY,
            },
        );
    }

    /// Converge one kept sidebar pane onto the layout's dock, in place and
    /// without touching its renderer. Each targeted `move-pane left` crosses
    /// one adjacent pane and must strictly reduce authoritative `pane_x`; the
    /// tab's tiled-pane count bounds the loop. A narrow nested row can then be
    /// stacked into the right column. Width convergence starts only after the
    /// same current geometry proves a full-height left dock and `width_floor`
    /// carries the caller's temporal viewport proof. Returns the latest
    /// successful repair-action timestamp. Best-effort: any failure leaves the
    /// pane for the next pass.
    pub(super) fn converge_sidebar_geometry(
        &self,
        opts: &SidebarPaneOptions,
        tab_position: u64,
        raw_id: u64,
        width_floor: Option<u64>,
    ) -> Option<u64> {
        self.converge_sidebar_geometry_with(
            opts,
            tab_position,
            raw_id,
            false,
            width_floor,
            width_floor.is_some(),
        )
    }

    fn converge_added_sidebar_geometry(
        &self,
        opts: &SidebarPaneOptions,
        tab_position: u64,
        raw_id: u64,
        structural_floor: u64,
        width_floor: Option<u64>,
    ) -> Option<u64> {
        self.converge_sidebar_geometry_with(
            opts,
            tab_position,
            raw_id,
            true,
            Some(structural_floor.max(width_floor.unwrap_or_default())),
            width_floor.is_some(),
        )
    }

    fn converge_sidebar_geometry_with(
        &self,
        opts: &SidebarPaneOptions,
        tab_position: u64,
        raw_id: u64,
        stack_multicolumn_work: bool,
        initial_floor: Option<u64>,
        repair_width: bool,
    ) -> Option<u64> {
        let pane_raw = ZellijPaneId::Terminal(raw_id).action_target();
        let mut floor = initial_floor;
        let Ok(mut listing) =
            self.structural_geometry_listing(&opts.session_name, &opts.workspace_id, floor)
        else {
            return floor;
        };
        let Some(mut x) =
            sidebar_pane(&listing.panes, tab_position, raw_id).and_then(|pane| pane.pane_x)
        else {
            return floor;
        };
        let mut swaps_remaining = listing
            .panes
            .iter()
            .filter(|pane| pane.tab_position == tab_position && pane.is_terminal())
            .count()
            .saturating_sub(1);
        'moves: while x > 0 && swaps_remaining > 0 {
            let action_floor = unix_now_ms();
            if self.dock_left(&opts.session_name, &pane_raw).is_err() {
                break;
            }
            floor = Some(action_floor);
            swaps_remaining -= 1;
            let deadline = Instant::now() + GEOMETRY_REPAIR_SETTLE;
            loop {
                if let Ok(next) = self.structural_geometry_listing(
                    &opts.session_name,
                    &opts.workspace_id,
                    Some(action_floor),
                ) {
                    let Some(next_x) = sidebar_pane(&next.panes, tab_position, raw_id)
                        .and_then(|pane| pane.pane_x)
                    else {
                        break 'moves;
                    };
                    if next_x < x {
                        listing = next;
                        x = next_x;
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    break 'moves;
                }
                std::thread::sleep(MOUNT_POLL_STEP);
            }
        }

        let excluded = HashSet::new();
        let verdict = sidebar_pane(&listing.panes, tab_position, raw_id)
            .and_then(|pane| sidebar_dock_verdict(pane, &listing.panes, &excluded));
        if verdict == Some(SidebarDock::NestedRow)
            && let Some(action_ms) = self.stack_nested_work_panes(
                opts,
                tab_position,
                raw_id,
                floor,
                stack_multicolumn_work,
            )
        {
            floor = Some(action_ms);
            let deadline = Instant::now() + GEOMETRY_REPAIR_SETTLE;
            loop {
                if let Ok(next) =
                    self.structural_geometry_listing(&opts.session_name, &opts.workspace_id, floor)
                {
                    let docked = sidebar_pane(&next.panes, tab_position, raw_id)
                        .and_then(|pane| sidebar_dock_verdict(pane, &next.panes, &excluded))
                        == Some(SidebarDock::Docked);
                    listing = next;
                    if docked {
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    return floor;
                }
                std::thread::sleep(MOUNT_POLL_STEP);
            }
        }
        let verdict = sidebar_pane(&listing.panes, tab_position, raw_id)
            .and_then(|pane| sidebar_dock_verdict(pane, &listing.panes, &excluded));
        if verdict != Some(SidebarDock::Docked) {
            return floor;
        }
        if !repair_width {
            return floor;
        }
        let sync = WidthSyncOptions {
            session_name: opts.session_name.clone(),
            workspace_id: opts.workspace_id.clone(),
            target: opts.target,
        };
        let (width_floor, _) = self.converge_sidebar_width(&sync, tab_position, raw_id, floor);
        width_floor
    }

    /// Converge one sidebar to the target computed from its current tab width.
    /// Returns the latest topology floor and whether at least one resize action
    /// succeeded. Structural reconcile owns this listing-based repair path.
    pub(super) fn converge_sidebar_width(
        &self,
        opts: &WidthSyncOptions,
        tab_position: u64,
        raw_id: u64,
        floor: Option<u64>,
    ) -> (Option<u64>, bool) {
        let listing = self.read_topology(
            Some(&opts.session_name),
            None,
            Some(&opts.workspace_id),
            floor,
            PaneReadConsistency::PreferAuthoritative,
            RECONCILE_LIST_TIMEOUT,
        );
        let Ok(listing) = listing else {
            return (floor, false);
        };
        let (width_floor, resized) = self.converge_sidebar_widths_stepwise(
            opts,
            tab_position,
            raw_id,
            Some((&listing.panes, listing.produced_at_ms)),
        );
        (width_floor.or(floor), resized)
    }

    pub(super) fn sidebar_dock_outcome(
        &self,
        session: &str,
        workspace_id: &WorkspaceId,
        tab_position: u64,
        raw_id: u64,
        min_topology_produced_at_ms: Option<u64>,
    ) -> DockOutcome {
        std::thread::sleep(DOCK_VERIFY_SETTLE);
        let Ok(listing) =
            self.structural_geometry_listing(session, workspace_id, min_topology_produced_at_ms)
        else {
            return DockOutcome::Docked;
        };
        let panes = listing.panes;
        let Some(pane) = sidebar_pane(&panes, tab_position, raw_id) else {
            return DockOutcome::Misdocked;
        };
        let excluded = HashSet::new();
        match sidebar_dock_verdict(pane, &panes, &excluded) {
            Some(SidebarDock::SwapReachable | SidebarDock::NestedRow) => DockOutcome::Misdocked,
            Some(SidebarDock::Docked) | None => DockOutcome::Docked,
        }
    }

    fn misdocked_add_should_retry(
        &self,
        opts: &SidebarPaneOptions,
        tab_position: u64,
        raw_id: u64,
        min_topology_produced_at_ms: Option<u64>,
    ) -> bool {
        let Ok(listing) = self.structural_geometry_listing(
            &opts.session_name,
            &opts.workspace_id,
            min_topology_produced_at_ms,
        ) else {
            return false;
        };
        let panes = listing.panes;
        let Some(sidebar) = sidebar_pane(&panes, tab_position, raw_id) else {
            return false;
        };
        let excluded = HashSet::new();
        match sidebar_dock_verdict(sidebar, &panes, &excluded) {
            Some(SidebarDock::SwapReachable) => true,
            Some(SidebarDock::NestedRow) => {
                repairable_nested_work_pane_ids(sidebar, &panes, &excluded).is_some()
            }
            Some(SidebarDock::Docked) | None => false,
        }
    }

    /// A nested row has a valid sidebar process at `x=0`, but at least one work
    /// pane also starts inside that column band. Existing user layouts stack
    /// only the narrow one-column repair shape; a sidebar created by this add
    /// transaction can stack every work pane because the transaction itself
    /// introduced the nested shape. Both paths preserve every process.
    fn stack_nested_work_panes(
        &self,
        opts: &SidebarPaneOptions,
        tab_position: u64,
        raw_id: u64,
        min_topology_produced_at_ms: Option<u64>,
        allow_multicolumn: bool,
    ) -> Option<u64> {
        let deadline = Instant::now() + GEOMETRY_REPAIR_SETTLE;
        let work = loop {
            let Ok(listing) = self.structural_geometry_listing(
                &opts.session_name,
                &opts.workspace_id,
                min_topology_produced_at_ms,
            ) else {
                return None;
            };
            let panes = listing.panes;
            let sidebar = sidebar_pane(&panes, tab_position, raw_id)?;
            let excluded = HashSet::new();
            let work = if allow_multicolumn {
                nested_work_pane_ids(sidebar, &panes, &excluded)
            } else {
                repairable_nested_work_pane_ids(sidebar, &panes, &excluded)
            };
            if let Some(work) = work {
                break work;
            }
            if sidebar_dock_verdict(sidebar, &panes, &excluded) == Some(SidebarDock::Docked)
                || Instant::now() >= deadline
            {
                return None;
            }
            std::thread::sleep(MOUNT_POLL_STEP);
        };
        let mut args = vec!["stack-panes".to_owned(), "--".to_owned()];
        args.extend(
            work.iter()
                .map(|id| ZellijPaneId::Terminal(*id).action_target()),
        );
        let action_floor = unix_now_ms();
        match self.zellij_action(&opts.session_name).args(args).run() {
            Ok(_) => Some(action_floor),
            Err(err) => {
                tracing::warn!(
                    session = %opts.session_name,
                    tab = tab_position,
                    pane = raw_id,
                    error = %err,
                    "sidebar geometry repair could not stack work panes into the right column",
                );
                None
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

    /// Whether `session` has a persistent attached terminal client. A real
    /// attach holds one client id across the confirmation window. Zellij's
    /// transient bootstrap/action clients churn or drain within it, so an
    /// unstable or empty roster reads detached and the add defers.
    pub(super) fn session_has_attached_client(&self, session: &str) -> bool {
        let mut probes = vec![self.focused_terminal_client_ids(session)];
        if !stable_client_present(&probes) {
            return false;
        }
        let deadline = Instant::now() + CLIENT_CONFIRM_WINDOW;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(remaining.min(CLIENT_PROBE_SETTLE));
            probes.push(self.focused_terminal_client_ids(session));
            if !stable_client_present(&probes) {
                return false;
            }
        }
        stable_client_present(&probes)
    }

    pub(super) fn focused_terminal_client_ids(&self, session: &str) -> BTreeSet<u32> {
        self.zellij_action(session)
            .arg("list-clients")
            .run()
            .map(|output| terminal_client_ids(&parse_client_view(&output.stdout)))
            .unwrap_or_default()
    }

    /// `new-pane` in a stable target tab, titled and `close_on_exit` to match
    /// the layout, running the same `rimz sidebar serve` command. Placement is
    /// deliberately unspecified: Zellij 0.44.3 directional placement with
    /// `--tab-id` can allocate a terminal without mounting it when the action
    /// client's active pane is absent. The geometry pass docks the mounted pane.
    /// Returns the created pane id Zellij prints (for example, `terminal_58`) as
    /// a hint only: concurrent action clients can cross-talk.
    pub(super) fn new_sidebar_pane(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
    ) -> Result<Option<ZellijPaneId>> {
        let mut args = vec![
            "new-pane".to_owned(),
            "--tab-id".to_owned(),
            tab_id.to_string(),
            "--name".to_owned(),
            SIDEBAR_CHROME_TITLE.to_owned(),
            "--borderless".to_owned(),
            "true".to_owned(),
            "--close-on-exit".to_owned(),
            "--cwd".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
            "--".to_owned(),
        ];
        let mut command = vec![opts.rimz_bin.to_string_lossy().into_owned()];
        command.extend(sidebar_serve_args(MuxName::Zellij, opts));
        args.extend(command);
        let output = self.zellij_action(&opts.session_name).args(args).run()?;
        Ok(parse_new_pane_id(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Converge one sidebar with geometry feedback between native resize steps.
    /// Returns the latest topology floor and whether a resize succeeded.
    pub(super) fn converge_sidebar_widths_stepwise(
        &self,
        opts: &WidthSyncOptions,
        tab_position: u64,
        raw_id: u64,
        initial: Option<(&[PaneTopologyPane], u64)>,
    ) -> (Option<u64>, bool) {
        const RESIZE_MAX_STEPS: u32 = 64;
        const TRANSIENT_MAX_RETRIES: u8 = 2;
        let mut floor = initial.as_ref().map(|(_, observed_at_ms)| *observed_at_ms);
        let mut initial = initial;
        let mut listing_retries = 0;
        let mut require_fresh_topology = false;
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
            let panes = if let Some((panes, _)) = initial.take() {
                panes
            } else {
                let consistency = if require_fresh_topology {
                    require_fresh_topology = false;
                    PaneReadConsistency::Cached
                } else {
                    PaneReadConsistency::PreferAuthoritative
                };
                let listing = self.read_topology(
                    Some(&opts.session_name),
                    None,
                    Some(&opts.workspace_id),
                    floor,
                    consistency,
                    RECONCILE_LIST_TIMEOUT,
                );
                let Ok(listing) = listing else {
                    if listing_retries < TRANSIENT_MAX_RETRIES {
                        listing_retries += 1;
                        continue;
                    }
                    break;
                };
                owned_panes = listing.panes;
                &owned_panes
            };

            let cols = panes.iter().find_map(|pane| {
                (pane.is_terminal() && pane.tab_position == tab_position && pane.id == raw_id)
                    .then_some(pane.pane_columns)
                    .flatten()
            });
            let Some((cols, view_cols)) = cols.zip(tab_view_cols(panes, tab_position)) else {
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
            // Defensive only: the ceiling stop estimate bounds Zellij's real
            // lattice, but reverse once if a future backend delta exceeds it.
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
                // A cache produced after action start but before Zellij applies
                // the resize can repeat once; require one newer read before
                // treating the pane as pinned at a backend minimum.
                if !no_progress_retry {
                    no_progress_retry = true;
                    floor = Some(unix_now_ms());
                    // An authoritative listing can merge background geometry
                    // from a newly stamped but pre-action cache. Confirm a
                    // repeated width against topology after this floor.
                    require_fresh_topology = true;
                    continue;
                }
                break;
            }
            no_progress_retry = false;
            last_cols = Some(cols);
            last_step_grow = Some(grow);

            let action_floor = unix_now_ms();
            floor = Some(action_floor);
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
                    // The failed CLI may still have reached the server. Read
                    // post-action geometry before deciding which step remains.
                    last_cols = None;
                    last_step_grow = None;
                    continue;
                }
                break;
            }
            resized = true;
            reverse_spent = regressed;
        }

        (floor, resized)
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
    ///
    /// The predicate gates on *our* sidebar chrome pane (a default/fallback
    /// birth carries none) counted with the same `is_live_terminal` filter the
    /// roster applies, so "materialized" here provably implies the caller's next
    /// pane roster returns the two panes — no held/exited pane slips the gate.
    pub(super) fn wait_for_sidebar_layout(
        &self,
        session_name: &str,
        workspace_id: &WorkspaceId,
    ) -> bool {
        let deadline = Instant::now() + SIDEBAR_LAYOUT_TIMEOUT;
        while Instant::now() < deadline {
            if self.session_state(session_name) != SessionLiveness::Live {
                return false;
            }
            if let Ok(panes) = self.topology_panes_for_workspace(
                session_name,
                workspace_id,
                None,
                RECONCILE_LIST_TIMEOUT,
            ) && panes.iter().any(is_sidebar_pane)
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
