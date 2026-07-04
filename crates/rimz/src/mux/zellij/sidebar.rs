//! Zellij sidebar birth, in-place recovery, and geometry convergence.

use std::collections::{BTreeSet, HashSet};
use std::num::NonZeroU16;
use std::time::{Duration, Instant};

use super::layout::{TempLayoutFile, render_session_layout};
use super::parse::{
    classify_session_not_found, is_session_not_found, new_tab_template_sidebar_cols,
    parse_focused_terminal_client_ids, strip_ansi,
};
use super::raw_pane::{
    SidebarDock, docked_sidebar_cols, is_sidebar_pane, leftmost_live_work_pane,
    mounted_sidebar_pane, parse_new_pane_id, parse_terminal_id, repairable_nested_work_pane_ids,
    sidebar_dock_verdict, sidebar_width_off_spec,
};
use super::socket::{socket_headroom_with_xdg_override, stderr_reports_socket_overflow};
use super::{
    MOUNT_POLL_STEP, MOUNT_POLL_TIMEOUT, SIDEBAR_LAYOUT_TIMEOUT, STACK_PANES_MIN_ZELLIJ,
    TAB_NAMES_ATTEMPTS, TAB_NAMES_RETRY_DELAY, ZellijBackend, parse_version,
};
use crate::ids::{MuxName, PaneId};
use crate::mux::{DaemonView, MuxBackend, MuxErr, Result, SidebarPaneOptions, sidebar_serve_args};
use crate::pane::SIDEBAR_CHROME_TITLE;
use crate::sidebar::timing::RECONCILE_LIST_TIMEOUT;

const ADD_DOCK_ATTEMPTS: u32 = 2;
const DOCK_VERIFY_SETTLE: Duration = Duration::from_millis(100);
const CLIENT_PROBE_SETTLE: Duration = Duration::from_millis(100);
// Birth can land Zellij's layout focus on the sidebar in a detached session
// under load, and a single `focus-pane-id` can lag before it lands. Re-issue
// and re-check a bounded number of times until the work pane holds focus.
const BIRTH_FOCUS_ATTEMPTS: u32 = 8;
const BIRTH_FOCUS_RETRY_DELAY: Duration = Duration::from_millis(100);
// Confirmation must outlast Zellij's background-create bootstrap-client linger.
// A false negative only defers one recoverable add pass; a false positive can
// leak an unmounted sidebar serve pair.
const CLIENT_CONFIRM_WINDOW: Duration = Duration::from_millis(750);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DockOutcome {
    Docked,
    Misdocked,
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
        if self.wait_for_sidebar_layout(&opts.session_name) {
            self.focus_work_pane_if_sidebar_is_focused(&opts.session_name);
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
            if self.wait_for_sidebar_layout(&opts.session_name) {
                self.focus_work_pane_if_sidebar_is_focused(&opts.session_name);
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

    /// Inject a left-docked sidebar into a live tab without a rebirth: split a
    /// pane to the right, discover the mounted pane by listing, converge its
    /// geometry, and verify the full-height dock. A narrow nested-row shape is
    /// repairable by stacking the work panes into the right column; other
    /// persistent mis-docks are kept and reported rather than leaking a
    /// paneless renderer or leaving the tab sidebar-less.
    pub(super) fn add_sidebar_to_tab(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
    ) -> Result<DockOutcome> {
        let mut last_error = None;
        let mut fallback_misdocked: Option<u64> = None;
        for attempt in 0..ADD_DOCK_ATTEMPTS {
            let before: HashSet<u64> = self
                .list_panes_with_session(Some(&opts.session_name))?
                .iter()
                .filter(|pane| pane.is_terminal() && pane.tab_id == tab_id)
                .map(|pane| pane.id)
                .collect();
            self.focus_leftmost_work_pane(&opts.session_name, tab_id);
            // A `new-pane` failure is remembered, not fatal yet: concurrent
            // action clients can cross-talk responses, so the command can
            // misreport while the pane is still created — discovery gets its
            // window either way.
            let (hint, spawn_err) = match self.new_sidebar_pane(opts, tab_id) {
                Ok(hint) => (hint, None),
                Err(err) => (None, Some(err)),
            };
            let Some(raw_id) =
                self.wait_for_mounted_sidebar(&opts.session_name, tab_id, &before, hint.as_deref())
            else {
                if fallback_misdocked.is_some() {
                    return Ok(DockOutcome::Misdocked);
                }
                last_error = Some(spawn_err.unwrap_or_else(|| MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!("new-pane never mounted a sidebar pane in tab {tab_id}"),
                }));
                continue;
            };
            if let Some(previous) = fallback_misdocked.take() {
                self.cleanup_failed_add(opts, previous);
            }
            self.converge_sidebar_geometry(opts, tab_id, raw_id);
            match self.sidebar_dock_outcome(&opts.session_name, tab_id, raw_id) {
                DockOutcome::Docked => return Ok(DockOutcome::Docked),
                DockOutcome::Misdocked
                    if attempt + 1 < ADD_DOCK_ATTEMPTS
                        && self.misdocked_add_should_retry(opts, tab_id, raw_id) =>
                {
                    fallback_misdocked = Some(raw_id);
                }
                DockOutcome::Misdocked => {
                    let pane_id = format!("terminal_{raw_id}");
                    tracing::warn!(
                        session = %opts.session_name,
                        tab = tab_id,
                        pane = %pane_id,
                        "sidebar add mounted a working pane but could not verify a full-height left dock",
                    );
                    return Ok(DockOutcome::Misdocked);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("new-pane never mounted a docked sidebar pane in tab {tab_id}"),
        }))
    }

    /// Bounded poll for the sidebar pane an add just spawned to mount in
    /// `tab_id`. Returns its raw numeric id, or `None` once
    /// [`MOUNT_POLL_TIMEOUT`] elapses — the mount was dropped.
    pub(super) fn wait_for_mounted_sidebar(
        &self,
        session: &str,
        tab_id: u64,
        before: &HashSet<u64>,
        hint: Option<&str>,
    ) -> Option<u64> {
        let hint_raw = hint.and_then(parse_terminal_id);
        let deadline = Instant::now() + MOUNT_POLL_TIMEOUT;
        loop {
            if let Ok(panes) = self.list_panes_with_session(Some(session))
                && let Some(id) = mounted_sidebar_pane(&panes, tab_id, before, hint_raw)
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

    fn focus_leftmost_work_pane(&self, session: &str, tab_id: u64) {
        let Ok(panes) = self.list_panes_with_session(Some(session)) else {
            return;
        };
        let Some(raw_id) = leftmost_live_work_pane(&panes, tab_id) else {
            return;
        };
        let _ = self.focus_terminal(session, raw_id);
    }

    /// Repair birth-time stranded focus: while any tab holds focus on its
    /// sidebar pane, focus that tab's leftmost live work pane instead. Re-checks
    /// and re-issues across a bounded window because Zellij can lag a
    /// `focus-pane-id` before it lands under load. Returns once focus is off the
    /// sidebar everywhere, or when no stranded tab has a work pane to move to.
    fn focus_work_pane_if_sidebar_is_focused(&self, session: &str) {
        for attempt in 0..BIRTH_FOCUS_ATTEMPTS {
            let Ok(panes) = self.list_panes_with_session(Some(session)) else {
                return;
            };
            let stranded_tabs: Vec<u64> = panes
                .iter()
                .filter(|pane| pane.is_live_terminal() && pane.is_focused && is_sidebar_pane(pane))
                .map(|pane| pane.tab_id)
                .collect();
            if stranded_tabs.is_empty() {
                return;
            }
            let mut acted = false;
            for tab_id in stranded_tabs {
                if let Some(raw_id) = leftmost_live_work_pane(&panes, tab_id) {
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
    /// without touching its renderer: a bounded move-left loop (re-listing
    /// between steps — `move-pane left` swaps one position per call) until the
    /// pane reaches the left column or stops progressing, a narrow nested-row
    /// repair that stacks work panes into the right column when the surrounding
    /// layout is safe to rewrite, then a resize back toward the session's fixed
    /// birth width when it is still too wide. Returns whether any repair was issued.
    /// Best-effort:
    /// geometry is cosmetic, so any failure just leaves the pane where it is for
    /// the next pass.
    pub(super) fn converge_sidebar_geometry(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
        raw_id: u64,
    ) -> bool {
        const REDOCK_MAX_STEPS: u32 = 4;
        let pane_raw = format!("terminal_{raw_id}");
        let mut repaired = false;
        let mut last_x = u64::MAX;
        for _ in 0..REDOCK_MAX_STEPS {
            let Ok(panes) = self.list_panes_with_session(Some(&opts.session_name)) else {
                break;
            };
            // Plugin panes share the integer id space (`plugin_1` beside
            // `terminal_1`), so the terminal filter is load-bearing here.
            let Some(pane) = panes
                .iter()
                .find(|pane| pane.is_terminal() && pane.tab_id == tab_id && pane.id == raw_id)
            else {
                break;
            };
            let Some(x) = pane.pane_x else { break };
            if x == 0 || x >= last_x {
                break; // docked, or no progress — stop rather than spin.
            }
            last_x = x;
            if self.dock_left(&opts.session_name, &pane_raw).is_err() {
                break;
            }
            repaired = true;
        }
        if self.stack_nested_work_panes(opts, tab_id, raw_id) {
            repaired = true;
        }
        let target_cols = u64::from(opts.birth_size.cols.get());
        if let Some(cols) = self.sidebar_cols(&opts.session_name, tab_id, raw_id)
            && sidebar_width_off_spec(cols, target_cols)
        {
            self.resize_sidebar_toward(&opts.session_name, tab_id, &pane_raw, target_cols);
            repaired = true;
        }
        repaired
    }

    pub(super) fn sidebar_dock_outcome(
        &self,
        session: &str,
        tab_id: u64,
        raw_id: u64,
    ) -> DockOutcome {
        std::thread::sleep(DOCK_VERIFY_SETTLE);
        let Ok(panes) = self.list_panes_with_session(Some(session)) else {
            return DockOutcome::Docked;
        };
        let Some(pane) = panes
            .iter()
            .find(|pane| pane.is_terminal() && pane.tab_id == tab_id && pane.id == raw_id)
        else {
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
        tab_id: u64,
        raw_id: u64,
    ) -> bool {
        let Ok(panes) = self.list_panes_with_session(Some(&opts.session_name)) else {
            return false;
        };
        let Some(sidebar) = panes
            .iter()
            .find(|pane| pane.is_terminal() && pane.tab_id == tab_id && pane.id == raw_id)
        else {
            return false;
        };
        let excluded = HashSet::new();
        match sidebar_dock_verdict(sidebar, &panes, &excluded) {
            Some(SidebarDock::SwapReachable) => true,
            Some(SidebarDock::NestedRow) => {
                self.stack_panes_supported()
                    && repairable_nested_work_pane_ids(sidebar, &panes, &excluded).is_some()
            }
            Some(SidebarDock::Docked) | None => false,
        }
    }

    /// A nested row has a valid sidebar process at `x=0`, but at least one work
    /// pane also starts inside that column band. On Zellij versions that expose
    /// `stack-panes`, a narrow class of nested rows can be promoted into a
    /// single right-side stack without replacing their processes.
    fn stack_nested_work_panes(&self, opts: &SidebarPaneOptions, tab_id: u64, raw_id: u64) -> bool {
        if !self.stack_panes_supported() {
            return false;
        }
        let Ok(panes) = self.list_panes_with_session(Some(&opts.session_name)) else {
            return false;
        };
        let Some(sidebar) = panes
            .iter()
            .find(|pane| pane.is_terminal() && pane.tab_id == tab_id && pane.id == raw_id)
        else {
            return false;
        };
        let excluded = HashSet::new();
        let Some(work) = repairable_nested_work_pane_ids(sidebar, &panes, &excluded) else {
            return false;
        };
        let mut args = vec!["stack-panes".to_owned(), "--".to_owned()];
        args.extend(work.iter().map(|id| format!("terminal_{id}")));
        match self.zellij_action(&opts.session_name).args(args).run() {
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(
                    session = %opts.session_name,
                    tab = tab_id,
                    pane = raw_id,
                    error = %err,
                    "sidebar geometry repair could not stack work panes into the right column",
                );
                false
            }
        }
    }

    pub(super) fn stack_panes_supported(&self) -> bool {
        self.version()
            .ok()
            .as_deref()
            .and_then(parse_version)
            .is_some_and(|version| version >= STACK_PANES_MIN_ZELLIJ)
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

    /// `new-pane` to the right of the tab's focus, titled and `close_on_exit` to
    /// match the layout, running the same `rimz sidebar serve` command. Returns
    /// the created pane id Zellij prints (e.g. `terminal_58`) — as a *hint*
    /// only: under concurrent action clients the stdout can carry another
    /// client's response or nothing, while the pane is still created.
    pub(super) fn new_sidebar_pane(
        &self,
        opts: &SidebarPaneOptions,
        tab_id: u64,
    ) -> Result<Option<String>> {
        let mut args = vec![
            "new-pane".to_owned(),
            "--direction".to_owned(),
            "right".to_owned(),
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

    /// Shrink the reconcile heal path's freshly-split sidebar (born at ~50% —
    /// `new-pane` has no tiled-size flag) toward the session's fixed birth
    /// width. The resize step is coarse, so the target can fall between two
    /// reachable widths; stop at the first width at or below it so the pane
    /// never finishes above the canonical target. Bounded and best-effort: it
    /// stops at the target, when a step makes no progress (hit a minimum), or
    /// after [`RESIZE_MAX_STEPS`] — never a dead loop. Width is cosmetic, so
    /// any failure just leaves the wider pane.
    pub(super) fn resize_sidebar_toward(
        &self,
        session: &str,
        tab_id: u64,
        pane_id: &str,
        target_cols: u64,
    ) {
        const RESIZE_MAX_STEPS: u32 = 16;
        let Some(target_raw) = parse_terminal_id(pane_id) else {
            return;
        };
        let mut last_cols = u64::MAX;
        for _ in 0..RESIZE_MAX_STEPS {
            let Some(cols) = self.sidebar_cols(session, tab_id, target_raw) else {
                return;
            };
            if cols <= target_cols {
                return;
            }
            if cols >= last_cols {
                return; // no progress (hit a minimum) — stop rather than spin.
            }
            last_cols = cols;
            if self
                .resize_sidebar_step(session, pane_id, "decrease")
                .is_err()
            {
                return;
            }
        }
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

    /// Current column width of `target_raw`. `None` when the pane has vanished
    /// or carries no geometry.
    pub(super) fn sidebar_cols(&self, session: &str, tab_id: u64, target_raw: u64) -> Option<u64> {
        let panes = self.list_panes_with_session(Some(session)).ok()?;
        panes
            .iter()
            .find(|pane| pane.is_terminal() && pane.tab_id == tab_id && pane.id == target_raw)
            .and_then(|pane| pane.pane_columns)
    }

    /// Block until Zellij has materialized the layout's sidebar pane alongside a
    /// second live terminal, so the caller's temp layout file stays on disk
    /// until Zellij has demonstrably parsed it. Returns `true` once that signal
    /// appears, `false` if the [`SIDEBAR_LAYOUT_TIMEOUT`] ceiling elapses first.
    ///
    /// The predicate gates on *our* sidebar chrome pane (a default/fallback
    /// birth carries none) counted with the same `is_live_terminal` filter
    /// `list_panes` applies, so "materialized" here provably implies the
    /// caller's next `list_panes` returns the two panes — no held/exited pane
    /// slips the gate.
    pub(super) fn wait_for_sidebar_layout(&self, session_name: &str) -> bool {
        let deadline = Instant::now() + SIDEBAR_LAYOUT_TIMEOUT;
        while Instant::now() < deadline {
            if let Ok(panes) = self.list_panes_with_session(Some(session_name))
                && panes.iter().any(is_sidebar_pane)
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

    /// The session's fixed sidebar width. `rimz agents <spec>` supplies its own
    /// layout and therefore bypasses the template, so it mirrors the session's
    /// width explicitly: first from the `new_tab_template`, then from live docked
    /// sidebars when Zellij cannot report the template.
    pub(super) fn session_sidebar_cols(&self, session: &str) -> Option<NonZeroU16> {
        match self.new_tab_template_sidebar_cols(session) {
            Ok(Some(cols)) => return Some(cols),
            Ok(None) => tracing::debug!(
                session = %session,
                "dump-layout did not report a new_tab_template sidebar width; falling back to live sidebars",
            ),
            Err(err) => tracing::debug!(
                session = %session,
                error = &err as &dyn std::error::Error,
                "dump-layout failed; falling back to live sidebars",
            ),
        }
        let panes = match self.list_panes_bounded(Some(session), RECONCILE_LIST_TIMEOUT) {
            Ok(panes) => panes,
            Err(err) => {
                tracing::debug!(
                    session = %session,
                    error = &err as &dyn std::error::Error,
                    "list-panes failed while resolving the session sidebar width",
                );
                return None;
            }
        };
        docked_sidebar_cols(&panes)
    }

    /// The fixed sidebar width carried by Zellij's `new_tab_template`.
    pub(super) fn new_tab_template_sidebar_cols(
        &self,
        session: &str,
    ) -> Result<Option<NonZeroU16>> {
        let output = self
            .zellij_action(session)
            .arg("dump-layout")
            .run_with_timeout(RECONCILE_LIST_TIMEOUT)?;
        let layout = String::from_utf8_lossy(&output.stdout);
        Ok(new_tab_template_sidebar_cols(&layout))
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::stable_client_present;

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
}
