//! Zellij sidebar birth, in-place recovery, and geometry convergence.

use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

use super::layout::{TempLayoutFile, render_session_layout};
use super::parse::{
    SessionState, classify_session_not_found, is_session_not_found,
    parse_focused_terminal_client_ids, strip_ansi,
};
use super::raw_pane::{
    RawPane, RawPaneListing, SidebarDock, is_sidebar_pane, leftmost_live_work_pane,
    mounted_sidebar_pane, parse_new_pane_id, parse_terminal_id, repairable_nested_work_pane_ids,
    sidebar_dock_verdict, tab_view_cols,
};
use super::socket::{socket_headroom_with_xdg_override, stderr_reports_socket_overflow};
use super::{
    MOUNT_POLL_STEP, MOUNT_POLL_TIMEOUT, SIDEBAR_LAYOUT_TIMEOUT, TAB_NAMES_ATTEMPTS,
    TAB_NAMES_RETRY_DELAY, TOPOLOGY_CACHE_POLL_STEP, ZellijBackend,
};
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::mux::width::{live_target_cols, sidebar_width_off_spec, zellij_resize_step_cols};
use crate::mux::{
    DaemonView, MuxErr, PresencePluginOptions, Result, SidebarPaneOptions, WidthSyncOptions,
    sidebar_serve_args,
};
use crate::pane::SIDEBAR_CHROME_TITLE;
use crate::sidebar::timing::RECONCILE_LIST_TIMEOUT;
use crate::sidebar::timing::unix_now_ms;

const ADD_DOCK_ATTEMPTS: u32 = 2;
const DOCK_VERIFY_SETTLE: Duration = Duration::from_millis(100);
const CLIENT_PROBE_SETTLE: Duration = Duration::from_millis(100);
// Birth can land Zellij's layout focus on the sidebar in a detached session
// under load, and a single `focus-pane-id` can lag before it lands. Re-issue
// and re-check a bounded number of times until the work pane holds focus.
const BIRTH_FOCUS_ATTEMPTS: u32 = 30;
const BIRTH_FOCUS_RETRY_DELAY: Duration = Duration::from_millis(100);
const BIRTH_FOCUS_CLEAN_SAMPLES: u32 = 3;
// Confirmation must outlast Zellij's background-create bootstrap-client linger.
// A false negative only defers one recoverable add pass; a false positive can
// leak an unmounted sidebar serve pair.
const CLIENT_CONFIRM_WINDOW: Duration = Duration::from_millis(750);
const STACK_REPAIR_SETTLE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DockOutcome {
    Docked,
    Misdocked,
}

fn sidebar_pane(panes: &[RawPane], tab_position: u64, raw_id: u64) -> Option<&RawPane> {
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
        // agent process (`resolve_participant_with_pin_recovery`). Zellij has
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
            self.focus_work_pane_if_sidebar_is_focused(&opts.session_name, &opts.workspace_id);
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
                self.focus_work_pane_if_sidebar_is_focused(&opts.session_name, &opts.workspace_id);
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
            rimz_bin: opts.rimz_bin.clone(),
            converge: false,
            seed_permissions: machine_config.web.enabled,
            focus_key: machine_config.sidebar.focus_key_label().map(str::to_owned),
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

    /// Inject a left-docked sidebar into a live tab without a rebirth: split a
    /// pane to the right, discover the mounted pane through topology, converge its
    /// geometry, and verify the full-height dock. A narrow nested-row shape is
    /// repairable by stacking the work panes into the right column; other
    /// persistent mis-docks are kept and reported rather than leaking a
    /// paneless renderer or leaving the tab sidebar-less.
    pub(super) fn add_sidebar_to_tab(
        &self,
        opts: &SidebarPaneOptions,
        tab_position: u64,
    ) -> Result<DockOutcome> {
        let mut last_error = None;
        let mut fallback_misdocked: Option<u64> = None;
        for attempt in 0..ADD_DOCK_ATTEMPTS {
            let before: HashSet<u64> = self
                .topology_panes_for_workspace(
                    &opts.session_name,
                    &opts.workspace_id,
                    None,
                    RECONCILE_LIST_TIMEOUT,
                )?
                .iter()
                .filter(|pane| pane.is_terminal() && pane.tab_position == tab_position)
                .map(|pane| pane.id)
                .collect();
            // Focus is only a placement hint for ordinary `new-pane`; mounted
            // geometry and the bounded redock below own correctness.
            self.focus_leftmost_work_pane(&opts.session_name, &opts.workspace_id, tab_position);
            // A `new-pane` failure is remembered, not fatal yet: concurrent
            // action clients can cross-talk responses, so the command can
            // misreport while the pane is still created — discovery gets its
            // window either way.
            let floor_ms = unix_now_ms();
            let (hint, spawn_err) = match self.new_sidebar_pane(opts, tab_position) {
                Ok(hint) => (hint, None),
                Err(err) => (None, Some(err)),
            };
            let Some(raw_id) = self.wait_for_mounted_sidebar(
                &opts.session_name,
                tab_position,
                &before,
                hint.as_deref(),
                floor_ms,
                &opts.workspace_id,
            ) else {
                if fallback_misdocked.is_some() {
                    return Ok(DockOutcome::Misdocked);
                }
                last_error = Some(spawn_err.unwrap_or_else(|| MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!("new-pane never mounted a sidebar pane in tab {tab_position}"),
                }));
                continue;
            };
            if let Some(previous) = fallback_misdocked.take() {
                self.cleanup_failed_add(opts, previous);
            }
            let floor = self.converge_sidebar_geometry(opts, tab_position, raw_id);
            match self.sidebar_dock_outcome(
                &opts.session_name,
                &opts.workspace_id,
                tab_position,
                raw_id,
                floor,
            ) {
                DockOutcome::Docked => return Ok(DockOutcome::Docked),
                DockOutcome::Misdocked
                    if attempt + 1 < ADD_DOCK_ATTEMPTS
                        && self.misdocked_add_should_retry(opts, tab_position, raw_id, floor) =>
                {
                    fallback_misdocked = Some(raw_id);
                }
                DockOutcome::Misdocked => {
                    let pane_id = format!("terminal_{raw_id}");
                    tracing::warn!(
                        session = %opts.session_name,
                        tab = tab_position,
                        pane = %pane_id,
                        "sidebar add mounted a working pane but could not verify a full-height left dock",
                    );
                    return Ok(DockOutcome::Misdocked);
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
    pub(super) fn wait_for_mounted_sidebar(
        &self,
        session: &str,
        tab_position: u64,
        before: &HashSet<u64>,
        hint: Option<&str>,
        floor_ms: u64,
        workspace_id: &WorkspaceId,
    ) -> Option<u64> {
        let hint_raw = hint.and_then(parse_terminal_id);
        let deadline = Instant::now() + MOUNT_POLL_TIMEOUT;
        loop {
            if let Ok(panes) = self.topology_panes_for_workspace(
                session,
                workspace_id,
                Some(floor_ms),
                RECONCILE_LIST_TIMEOUT,
            ) && let Some(id) = mounted_sidebar_pane(&panes, tab_position, before, hint_raw)
            {
                return Some(id);
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

    fn focus_leftmost_work_pane(
        &self,
        session: &str,
        workspace_id: &WorkspaceId,
        tab_position: u64,
    ) {
        let _ = self.go_to_tab_position(session, tab_position);
        let Ok(panes) =
            self.topology_panes_for_workspace(session, workspace_id, None, RECONCILE_LIST_TIMEOUT)
        else {
            return;
        };
        let Some(raw_id) = leftmost_live_work_pane(&panes, tab_position) else {
            return;
        };
        let _ = self.focus_terminal(session, raw_id);
    }

    /// Read completed structural geometry directly from Zellij, using a fresh
    /// presence publication only when the server listing is unavailable.
    pub(super) fn structural_geometry_listing(
        &self,
        session: &str,
        workspace_id: &WorkspaceId,
        min_topology_produced_at_ms: Option<u64>,
    ) -> Result<RawPaneListing> {
        self.authoritative_pane_listing(session, None, Some(workspace_id), RECONCILE_LIST_TIMEOUT)
            .or_else(|_| {
                self.topology_listing(
                    Some(session),
                    None,
                    Some(workspace_id),
                    min_topology_produced_at_ms,
                    RECONCILE_LIST_TIMEOUT,
                )
            })
    }

    /// Repair birth-time stranded focus: while any tab holds focus on its
    /// sidebar pane, focus that tab's leftmost live work pane instead. Re-checks
    /// and re-issues across a bounded window because Zellij can lag a
    /// `focus-pane-id` before it lands under load. Returns once focus is off the
    /// sidebar everywhere across a few stable samples, or when no stranded tab
    /// has a work pane to move to.
    fn focus_work_pane_if_sidebar_is_focused(&self, session: &str, workspace_id: &WorkspaceId) {
        let mut clean_samples = 0;
        for attempt in 0..BIRTH_FOCUS_ATTEMPTS {
            let Ok(panes) = self.topology_panes_for_workspace(
                session,
                workspace_id,
                None,
                RECONCILE_LIST_TIMEOUT,
            ) else {
                return;
            };
            let stranded_tabs: Vec<u64> = panes
                .iter()
                .filter(|pane| pane.is_live_terminal() && pane.is_focused && is_sidebar_pane(pane))
                .map(|pane| pane.tab_position)
                .collect();
            if stranded_tabs.is_empty() {
                clean_samples += 1;
                if clean_samples >= BIRTH_FOCUS_CLEAN_SAMPLES {
                    return;
                }
                std::thread::sleep(BIRTH_FOCUS_RETRY_DELAY);
                continue;
            }
            clean_samples = 0;
            let mut acted = false;
            for tab_position in stranded_tabs {
                if let Some(raw_id) = leftmost_live_work_pane(&panes, tab_position) {
                    let _ = self.focus_terminal(session, raw_id);
                    acted = true;
                }
            }
            if !acted {
                return;
            }
            if attempt + 1 < BIRTH_FOCUS_ATTEMPTS {
                std::thread::sleep(BIRTH_FOCUS_RETRY_DELAY);
            }
        }
    }

    /// Converge one kept sidebar pane onto the layout's dock, in place and
    /// without touching its renderer. Each targeted `move-pane left` crosses
    /// one adjacent pane and must strictly reduce authoritative `pane_x`; the
    /// tab's tiled-pane count bounds the loop. A narrow nested row can then be
    /// stacked into the right column. Width convergence starts only after the
    /// same current geometry proves a full-height left dock. Returns the latest
    /// successful repair-action timestamp. Best-effort: any failure leaves the
    /// pane for the next pass.
    pub(super) fn converge_sidebar_geometry(
        &self,
        opts: &SidebarPaneOptions,
        tab_position: u64,
        raw_id: u64,
    ) -> Option<u64> {
        let pane_raw = format!("terminal_{raw_id}");
        let mut floor = None;
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
        while x > 0 && swaps_remaining > 0 {
            let action_floor = unix_now_ms();
            if self.dock_left(&opts.session_name, &pane_raw).is_err() {
                break;
            }
            floor = Some(action_floor);
            swaps_remaining -= 1;
            let Ok(next) = self.structural_geometry_listing(
                &opts.session_name,
                &opts.workspace_id,
                Some(action_floor),
            ) else {
                break;
            };
            let Some(next_x) =
                sidebar_pane(&next.panes, tab_position, raw_id).and_then(|pane| pane.pane_x)
            else {
                break;
            };
            listing = next;
            if next_x >= x {
                break;
            }
            x = next_x;
        }

        let excluded = HashSet::new();
        let verdict = sidebar_pane(&listing.panes, tab_position, raw_id)
            .and_then(|pane| sidebar_dock_verdict(pane, &listing.panes, &excluded));
        if verdict == Some(SidebarDock::NestedRow)
            && let Some(action_ms) = self.stack_nested_work_panes(opts, tab_position, raw_id, floor)
        {
            floor = Some(action_ms);
            let Ok(next) =
                self.structural_geometry_listing(&opts.session_name, &opts.workspace_id, floor)
            else {
                return floor;
            };
            listing = next;
        }
        let verdict = sidebar_pane(&listing.panes, tab_position, raw_id)
            .and_then(|pane| sidebar_dock_verdict(pane, &listing.panes, &excluded));
        if verdict != Some(SidebarDock::Docked) {
            return floor;
        }
        let sync = WidthSyncOptions {
            session_name: opts.session_name.clone(),
            workspace_id: opts.workspace_id.clone(),
            width: opts.width,
            width_override: opts.width_override,
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
        let listing = self
            .authoritative_pane_listing(
                &opts.session_name,
                None,
                Some(&opts.workspace_id),
                RECONCILE_LIST_TIMEOUT,
            )
            .or_else(|_| {
                self.topology_listing(
                    Some(&opts.session_name),
                    None,
                    Some(&opts.workspace_id),
                    floor,
                    RECONCILE_LIST_TIMEOUT,
                )
            });
        let Ok(listing) = listing else {
            return (floor, false);
        };
        let (width_floor, resized) = self.converge_sidebar_widths_stepwise(
            opts,
            tab_position,
            raw_id,
            Some((&listing.panes, listing.observed_at_ms)),
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
    /// pane also starts inside that column band. On Zellij versions that expose
    /// `stack-panes`, a narrow class of nested rows can be promoted into a
    /// single right-side stack without replacing their processes.
    fn stack_nested_work_panes(
        &self,
        opts: &SidebarPaneOptions,
        tab_position: u64,
        raw_id: u64,
        min_topology_produced_at_ms: Option<u64>,
    ) -> Option<u64> {
        let deadline = Instant::now() + STACK_REPAIR_SETTLE;
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
            if let Some(work) = repairable_nested_work_pane_ids(sidebar, &panes, &excluded) {
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
        args.extend(work.iter().map(|id| format!("terminal_{id}")));
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
        let pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{raw_id}"));
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

    fn focused_terminal_client_ids(&self, session: &str) -> BTreeSet<u32> {
        self.zellij_action(session)
            .arg("list-clients")
            .run()
            .map(|output| parse_focused_terminal_client_ids(&output.stdout))
            .unwrap_or_default()
    }

    /// `new-pane` to the right of the tab's placement-hint focus, titled and
    /// `close_on_exit` to match the layout, running the same `rimz sidebar
    /// serve` command. Returns the created pane id Zellij prints (for example,
    /// `terminal_58`) as a hint only: concurrent action clients can cross-talk.
    pub(super) fn new_sidebar_pane(
        &self,
        opts: &SidebarPaneOptions,
        tab_position: u64,
    ) -> Result<Option<String>> {
        self.go_to_tab_position(&opts.session_name, tab_position)?;
        let mut args = vec![
            "new-pane".to_owned(),
            "--direction".to_owned(),
            "right".to_owned(),
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
        initial: Option<(&[RawPane], u64)>,
    ) -> (Option<u64>, bool) {
        const RESIZE_MAX_STEPS: u32 = 64;
        const TRANSIENT_MAX_RETRIES: u8 = 2;
        let mut floor = initial.as_ref().map(|(_, observed_at_ms)| *observed_at_ms);
        let mut initial = initial;
        let mut listing_retries = 0;
        let mut require_fresh_topology = false;
        let mut last_cols = None;
        let mut last_step_grow = None;
        let mut no_progress_retry = false;
        let mut transient_retries = 0;
        let mut resized = false;

        for _ in 0..RESIZE_MAX_STEPS {
            let owned_panes;
            let panes = if let Some((panes, _)) = initial.take() {
                panes
            } else {
                let topology = || {
                    self.topology_listing(
                        Some(&opts.session_name),
                        None,
                        Some(&opts.workspace_id),
                        floor,
                        RECONCILE_LIST_TIMEOUT,
                    )
                };
                let listing = if require_fresh_topology {
                    require_fresh_topology = false;
                    topology()
                } else {
                    self.authoritative_pane_listing(
                        &opts.session_name,
                        None,
                        Some(&opts.workspace_id),
                        RECONCILE_LIST_TIMEOUT,
                    )
                    .or_else(|_| topology())
                };
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
            let target_cols = live_target_cols(opts.width, opts.width_override, view_cols);
            if !sidebar_width_off_spec(cols, target_cols, zellij_resize_step_cols(view_cols)) {
                break;
            }
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
                    &format!("terminal_{raw_id}"),
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
            if self.session_state(session_name) != SessionState::Live {
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

    /// The session's tab names, in tab order. `query-tab-names` prints one name
    /// per line; the ANSI banner newer Zellij ships is stripped.
    pub(super) fn tab_names(&self, session: &str) -> Result<Vec<String>> {
        for attempt in 0..TAB_NAMES_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(TAB_NAMES_RETRY_DELAY);
            }
            let output = self
                .zellij_action(session)
                .arg("query-tab-names")
                .run()
                .map_err(|err| classify_session_not_found(err, session))?;
            if is_session_not_found(&output.stdout) || is_session_not_found(&output.stderr) {
                return Err(MuxErr::SessionNotFound {
                    session: session.to_owned(),
                });
            }
            let names: Vec<String> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(strip_ansi)
                .map(|line| line.trim().to_owned())
                .filter(|line| !line.is_empty())
                .collect();
            if !names.is_empty() {
                return Ok(names);
            }
        }
        Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("query-tab-names returned no tabs after {TAB_NAMES_ATTEMPTS} attempts"),
        })
    }

    /// Whether `session` already holds a tab named `tab_name`. A Rimz background
    /// view is idempotent on its name, so a relaunch into a session that already
    /// carries it is skipped.
    pub(super) fn session_has_named_tab(&self, session: &str, tab_name: &str) -> Result<bool> {
        Ok(self.tab_names(session)?.iter().any(|name| name == tab_name))
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
