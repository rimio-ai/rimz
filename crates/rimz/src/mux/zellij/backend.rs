//! Zellij [`MuxBackend`](crate::mux::MuxBackend) trait implementation.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use super::ZellijBackend;
use super::layout::{TempLayoutFile, render_background_view_layout, render_tab_layout};
use super::parse::{
    SessionState, live_session_name_from_line, parse_focused_client_panes, trim_capture,
};
use super::raw_pane::{
    RawPane, SessionCleanliness, SidebarDock, floating_panes_in_anchor_view, is_sidebar_pane,
    own_zellij_pane_id, repairable_nested_work_pane_ids, sidebar_dock_verdict,
    sidebar_geometry_off_spec, tabs_with_sidebars, views_with_sidebars,
};
use super::sidebar::DockOutcome;
use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, ViewKind};
use crate::mux::{
    BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN, BackgroundViewLaunch, BackgroundViewOptions,
    ClientFocusOptions, CommandSpec, DaemonView, MuxBackend, MuxErr, NamedKey, PaneCapture,
    PaneListOptions, PaneListing, Result, SessionHealth, SessionOptions, SidebarLiveness,
    SidebarPaneOptions, SidebarRecovery, SidebarWidth, SplitPaneOptions, TabOptions,
    ensure_pane_backend,
};

/// Prefix `command` with an `env KEY=VALUE …` shim so a freshly split Zellij
/// pane inherits the requested vars; Zellij's `new-pane` has no env flag of its
/// own. An empty env map returns the command unchanged.
fn env_prefixed(env: &BTreeMap<String, String>, command: Vec<String>) -> Vec<String> {
    if env.is_empty() {
        return command;
    }
    let mut wrapped = Vec::with_capacity(command.len() + env.len() + 1);
    wrapped.push("env".to_owned());
    wrapped.extend(env.iter().map(|(key, value)| format!("{key}={value}")));
    wrapped.extend(command);
    wrapped
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
            .args(self.zellij_options_args_probed(&config.zellij))
    }

    fn detach(&self, _name: &str) -> Result<()> {
        self.cmd().args(["action", "detach"]).run().map(|_| ())
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        self.delete_session(name)
    }

    fn list_sessions(&self) -> Result<Vec<String>> {
        let output = self.cmd().arg("list-sessions").run()?;
        // Output lines look like `name [Created Ns ago]` for live sessions, or
        // `name [Created Ns ago] (EXITED - attach to resurrect)` for stopped
        // sessions. `list_sessions` is the live-session set used by `rimz list`
        // and `rimz reload`, so filter resurrectable corpses out here.
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(live_session_name_from_line)
            .collect())
    }

    fn list_panes(&self, opts: PaneListOptions) -> Result<PaneListing> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let raws = self.list_panes_cached_or_cli(
            opts.session_name.as_deref(),
            opts.workspace_id.as_ref(),
            opts.min_topology_produced_at_ms,
            timeout,
        )?;
        let session_name = opts.session_name.unwrap_or_default();
        Ok(raws.into_pane_listing(session_name, |mut p, session_name| {
            if !p.is_live_terminal() {
                return None;
            }
            let command = p.display_command();
            Some(PaneRef {
                pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", p.id)),
                session_name: session_name.to_owned(),
                view_id: Some(format!("tab_{}", p.view_position())),
                view_kind: Some(ViewKind::Tab),
                view_name: p.tab_name.take(),
                is_focused: p.is_focused,
                pane_pid: p.pid(),
                pane_process_start: p.process_start(),
                command,
                spawn_command: p.spawn_command().map(str::to_owned),
                cwd: p.reported_cwd().map(str::to_owned),
                resumed_session_id: None,
                elevated_agent: None,
                first_seen_at_ms: None,
                // Zellij's `list-panes -j` exposes no per-pane "tab is active"
                // or "session attached" signal, so pane visibility is unknown
                // here. `None` makes the renderer's visibility gate fall back
                // to always painting — the deliberate cross-backend floor.
            })
        }))
    }

    fn focused_client_panes(&self, opts: ClientFocusOptions) -> Result<Vec<PaneId>> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let mut spec = self.cmd();
        if let Some(name) = opts.session_name {
            spec = spec.args(["--session".to_owned(), name]);
        }
        let output = spec
            .args(["action", "list-clients"])
            .run_with_timeout(timeout)?;
        Ok(parse_focused_client_panes(&output.stdout))
    }

    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()> {
        let target = opts.target_pane_id;
        if let Some(target) = &target {
            ensure_pane_backend(target, MuxName::Zellij)?;
            // Zellij's CLI opens relative to the current focus and does not
            // expose a target-pane flag for `new-pane`.
        }
        let mut spec = self.cmd().args(["action", "new-pane"]);
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
        spec.run()?;
        // `new-pane` focuses the pane it creates; for the `--bg` path
        // return focus to the splitting pane (best-effort — the pane is open).
        if !opts.focus
            && let Some(target) = &target
        {
            let _ = self.focus_pane(target);
        }
        Ok(())
    }

    fn focus_pane(&self, pane: &PaneId) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        // Zellij 0.41+: `focus-pane-id <raw>`. The earlier `focus-pane-with-id`
        // name was removed; the stub that referenced it never reached a
        // running binary.
        self.cmd()
            .args(["action", "focus-pane-id", pane.raw()])
            .run()
            .map(|_| ())
    }

    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let mut spec = self.cmd().args(["action", "dump-screen"]);
        if ansi {
            spec = spec.arg("-a");
        }
        if lines.is_some() {
            // The `-f`/`--full` flag dumps the entire scrollback. Zellij does
            // not expose a "last N lines" cap at the CLI level, so any non-None
            // request maps onto "include scrollback"; the caller can post-trim.
            spec = spec.arg("-f");
        }
        spec = spec.args(["-p".to_owned(), pane.raw().to_owned()]);
        let output = spec.run()?;
        let raw_text = String::from_utf8_lossy(&output.stdout).into_owned();
        let (raw_text, lines) = trim_capture(raw_text, lines);
        Ok(PaneCapture {
            pane_id: pane.clone(),
            raw_text,
            lines,
        })
    }

    fn send_keys(&self, pane: &PaneId, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        self.cmd()
            .args(["action", "write-chars", "--pane-id", pane.raw(), text])
            .run()
            .map(|_| ())
    }

    fn send_key(&self, pane: &PaneId, key: NamedKey) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        let bytes = key.write_bytes().iter().map(u8::to_string);
        self.cmd()
            .args(["action", "write", "--pane-id", pane.raw()])
            .args(bytes)
            .run()
            .map(|_| ())
    }

    fn paste_text(&self, pane: &PaneId, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Zellij)?;
        // Open marker + text + close marker as one decimal byte list, mirroring
        // the existing `send_key` byte-write mechanism. Raw bytes pass through
        // untouched, so a marker-looking byte inside `text` is never re-parsed.
        let bytes = BRACKET_PASTE_OPEN
            .bytes()
            .chain(text.bytes())
            .chain(BRACKET_PASTE_CLOSE.bytes())
            .map(|byte| byte.to_string());
        self.cmd()
            .args(["action", "write", "--pane-id", pane.raw()])
            .args(bytes)
            .run()
            .map(|_| ())
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
            SessionState::Absent => self.create_session_with_sidebar(opts, daemon),
            SessionState::Exited => {
                self.delete_session(&opts.session_name)?;
                self.create_session_with_sidebar(opts, daemon)
            }
            SessionState::Live => match self.session_cleanliness(&opts.session_name) {
                Ok(SessionCleanliness::Clean) if !opts.replace_existing => Ok(()),
                Ok(_) => {
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
            },
        }
    }

    fn probe_session_health(&self, name: &str) -> Result<SessionHealth> {
        Ok(match self.session_state(name) {
            // Nothing to attach to — a fresh birth will produce a clean room.
            SessionState::Absent => SessionHealth::Healthy,
            // `attach --create` would resurrect a serialized, suspended layout.
            SessionState::Exited => SessionHealth::Stuck,
            SessionState::Live => match self.session_cleanliness(name) {
                Ok(SessionCleanliness::Clean) => SessionHealth::Healthy,
                Ok(
                    SessionCleanliness::MissingSidebar | SessionCleanliness::SuspendedCommandPane,
                )
                | Err(_) => SessionHealth::Stuck,
            },
        })
    }

    fn ensure_clean_session(
        &self,
        opts: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
    ) -> Result<SessionHealth> {
        let state = self.session_state(&opts.session_name);
        // A clean, live room is left untouched — never rebirth working panes.
        if matches!(state, SessionState::Live) {
            match self.session_cleanliness(&opts.session_name) {
                Ok(SessionCleanliness::Clean) => return Ok(SessionHealth::Healthy),
                Ok(
                    SessionCleanliness::MissingSidebar | SessionCleanliness::SuspendedCommandPane,
                ) => {}
                Err(err) => {
                    tracing::warn!(
                        session = %opts.session_name,
                        tags.operation = "zellij.room_inspect",
                        error = &err as &dyn std::error::Error,
                        "live zellij room could not be inspected; reset confirmation is required",
                    );
                    return Ok(SessionHealth::Stuck);
                }
            }
        }
        // Absent → first birth; Exited / inspected Live-but-suspended → delete
        // and rebirth from the layout so the room comes up clean and RUNNING
        // (with serialization off, a rebirth can never resurrect). An
        // uninspectable live room returns Stuck above so the caller offers a
        // reset before any destructive action. A rebirth that still fails to
        // talk to Zellij reads as Stuck so the caller offers a reset.
        let rebirth = || -> Result<()> {
            if !matches!(state, SessionState::Absent) {
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
        super::super::recovery::purge_zellij_session_cache_in(
            &crate::ledger::paths::cache_home(),
            name,
        )
    }

    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> Result<SidebarRecovery> {
        // Zellij docks the sidebar left only at session birth, but a left pane
        // can still be reached in a live session: close a stray sidebar by id,
        // and add one by splitting right, moving it left, and resizing it to the
        // layout width. This never rebirths the session, so the working panes
        // survive.
        let panes = self.list_panes_with_session(Some(&opts.session_name))?;
        let views = views_with_sidebars(&panes);
        let plan = super::super::plan_reconcile(&views, live);
        // Kept sidebars (not planned for closing) whose geometry sits off the
        // layout's dock — the residue of a mis-mounted add — converge in place
        // this pass, renderer untouched.
        let off_spec = off_spec_sidebars(&panes, &plan.close, opts.width);
        if plan.close.is_empty() && plan.add.is_empty() && off_spec.is_empty() {
            return Ok(SidebarRecovery::default());
        }

        // Adding (and closing) a pane shifts focus, so remember each tab's
        // focused (working) pane to restore afterwards, and the user's own
        // invoking pane to return the visible tab to where they ran `rimz reload`.
        let focused_in_tab = focused_work_panes(&panes);

        let mut report = SidebarRecovery::default();
        // Close duplicate / unresponsive sidebar panes first, so a view that lost
        // its only live sidebar reads as missing and gains exactly one fresh one.
        let failed_stale_close_views = close_planned_sidebars(
            self,
            &opts.session_name,
            &plan.close,
            &live.stale_panes,
            &plan.stale_close_views,
            &mut report,
        );
        // In-place adds and geometry moves both need an attached client: a
        // detached session's screen thread drops the mount while the spawned
        // serve pair keeps running, so adding there only leaks (the closes
        // above are safe detached). An unanswerable probe reads detached —
        // deferring one run is recoverable, a leaked pair is not. tmux splits
        // fine detached, so the gate is Zellij-internal.
        let needs_attached = !plan.add.is_empty() || !off_spec.is_empty();
        let attached = !needs_attached || self.session_has_attached_client(&opts.session_name);
        if attached {
            for (tab_id, raw_id) in &off_spec {
                repair_sidebar_geometry(self, opts, *tab_id, *raw_id, &focused_in_tab, &mut report);
            }
        }
        if needs_attached && !attached {
            report.deferred = plan.add.len() + off_spec.len();
            tracing::info!(
                session = %opts.session_name,
                deferred = report.deferred,
                "sidebar reconcile: no attached client; deferring in-place adds and geometry repairs",
            );
        } else {
            add_missing_sidebars(
                self,
                opts,
                &plan.add,
                &plan.restart_add,
                &failed_stale_close_views,
                &focused_in_tab,
                &mut report,
            );
        }
        if let Some(own) = own_zellij_pane_id() {
            let _ = self.focus_terminal(&opts.session_name, own);
        }
        Ok(report)
    }

    fn open_background_view(&self, opts: &BackgroundViewOptions) -> Result<BackgroundViewLaunch> {
        let session = &opts.sidebar.session_name;
        // Idempotent on the tab name. The lead position is owned by session birth
        // ([`Self::open_sidebar`] with a `daemon`): `rimz start` births the session
        // with this tab already leading, so the common case is a no-op here. A
        // failed query propagates rather than risk a duplicate launch.
        if self.session_has_named_tab(session, &opts.name)? {
            return Ok(BackgroundViewLaunch::AlreadyRunning);
        }
        // Late add: the session was born without the daemon tab (e.g. a host
        // became available after first start) and now carries one or more working
        // tabs. Zellij can't move a tab to the front, so this appended tab does
        // *not* lead — leading is a birth-time property. `--layout` gives the tab
        // its `sidebar | hosts…` shape directly (bypassing the tab template, so the
        // sidebar is spelled out); `new-tab` is synchronous (it prints the tab id),
        // so the temp layout can drop once it returns. Each pane carries its own
        // `cwd`, so no tab-level `--cwd` is needed.
        let layout = TempLayoutFile::new(render_background_view_layout(opts)?)?;
        self.zellij_action(session)
            .args([
                "new-tab".to_owned(),
                "--layout".to_owned(),
                layout.path().to_string_lossy().into_owned(),
                "--name".to_owned(),
                opts.name.clone(),
            ])
            .run()?;
        drop(layout);
        // `new-tab` focuses the tab it creates. Return focus to the leading tab so
        // the imminent `attach` lands on a working pane, not this freshly-added
        // daemon tab. Best-effort: a focus hiccup never sinks a launch.
        if let Err(err) = self.go_to_tab(session, 1) {
            tracing::warn!(
                session = %session,
                tags.operation = "zellij.focus_tab",
                error = &err as &dyn std::error::Error,
                "could not return focus off the freshly-added daemon tab",
            );
        }
        Ok(BackgroundViewLaunch::Launched)
    }

    fn open_tab(&self, opts: &TabOptions) -> Result<()> {
        let template_sidebar_cols = self
            .new_tab_template_sidebar_cols(&opts.session_name)
            .ok()
            .flatten();
        let layout = TempLayoutFile::new(render_tab_layout(opts, template_sidebar_cols)?)?;
        self.zellij_action(&opts.session_name)
            .args([
                "new-tab".to_owned(),
                "--layout".to_owned(),
                layout.path().to_string_lossy().into_owned(),
                "--name".to_owned(),
                opts.title.clone(),
            ])
            .run()?;
        drop(layout);
        if !opts.focus
            && let Err(err) = self.go_to_tab(&opts.session_name, 1)
        {
            tracing::warn!(
                session = %opts.session_name,
                tags.operation = "zellij.focus_tab",
                error = &err as &dyn std::error::Error,
                "could not return focus after opening an unfocused tab",
            );
        }
        Ok(())
    }

    fn close_pane(&self, session: &str, pane: &PaneId) -> Result<()> {
        ZellijBackend::close_pane(self, session, pane)
    }

    fn close_view_floating_panes(&self, session: &str, anchor: &PaneId) -> Result<Vec<PaneId>> {
        ensure_pane_backend(anchor, MuxName::Zellij)?;
        let panes = self.list_panes_bounded(Some(session), super::super::COMMAND_TIMEOUT)?;
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
        if let Some(cached) = self.version.get() {
            return Ok(cached.clone());
        }
        let spec = self.cmd().arg("--version");
        let output = spec.to_command().output().map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
                program: spec.program.clone(),
            },
            _ => MuxErr::Io(err),
        })?;
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        // First writer wins on a probe race; both raced probes read one binary.
        Ok(self.version.get_or_init(|| raw).clone())
    }
}

fn off_spec_sidebars(
    panes: &[RawPane],
    closing: &[PaneId],
    width: SidebarWidth,
) -> Vec<(u64, u64)> {
    let closing: std::collections::HashSet<u64> =
        closing.iter().filter_map(parse_zellij_raw).collect();
    panes
        .iter()
        .filter(|pane| pane.is_live_terminal() && is_sidebar_pane(pane))
        .filter(|pane| !closing.contains(&pane.id))
        .filter(|pane| sidebar_geometry_off_spec(pane, panes, &closing, width))
        .map(|pane| (pane.tab_id, pane.id))
        .collect()
}

fn parse_zellij_raw(pane: &PaneId) -> Option<u64> {
    (pane.mux() == MuxName::Zellij)
        .then(|| pane.raw().strip_prefix("terminal_")?.parse().ok())
        .flatten()
}

fn focused_work_panes(panes: &[RawPane]) -> std::collections::HashMap<u64, u64> {
    let mut focused = std::collections::HashMap::new();
    for pane in panes
        .iter()
        .filter(|pane| pane.is_focused && !pane.is_plugin)
    {
        focused.entry(pane.tab_id).or_insert(pane.id);
    }
    focused
}

fn repair_sidebar_geometry(
    backend: &ZellijBackend,
    opts: &SidebarPaneOptions,
    tab_id: u64,
    raw_id: u64,
    focused_in_tab: &std::collections::HashMap<u64, u64>,
    report: &mut SidebarRecovery,
) {
    let repaired = backend.converge_sidebar_geometry(opts, tab_id, raw_id);
    match backend.sidebar_dock_outcome(&opts.session_name, tab_id, raw_id) {
        DockOutcome::Docked => {
            if repaired {
                report.redocked += 1;
                restore_tab_focus(backend, &opts.session_name, tab_id, focused_in_tab);
            }
        }
        DockOutcome::Misdocked => {
            if repairable_nested_sidebar_remains(backend, &opts.session_name, tab_id, raw_id) {
                rebuild_misdocked_sidebar(backend, opts, tab_id, raw_id, focused_in_tab, report);
            } else {
                report.misdocked += 1;
            }
        }
    }
}

fn repairable_nested_sidebar_remains(
    backend: &ZellijBackend,
    session_name: &str,
    tab_id: u64,
    raw_id: u64,
) -> bool {
    let Ok(panes) = backend.list_panes_with_session(Some(session_name)) else {
        return false;
    };
    let Some(sidebar) = panes
        .iter()
        .find(|pane| pane.is_terminal() && pane.tab_id == tab_id && pane.id == raw_id)
    else {
        return false;
    };
    let excluded = std::collections::HashSet::new();
    sidebar_dock_verdict(sidebar, &panes, &excluded) == Some(SidebarDock::NestedRow)
        && repairable_nested_work_pane_ids(sidebar, &panes, &excluded).is_some()
}

fn rebuild_misdocked_sidebar(
    backend: &ZellijBackend,
    opts: &SidebarPaneOptions,
    tab_id: u64,
    raw_id: u64,
    focused_in_tab: &std::collections::HashMap<u64, u64>,
    report: &mut SidebarRecovery,
) {
    let pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{raw_id}"));
    if let Err(err) = backend.close_pane(&opts.session_name, &pane) {
        tracing::warn!(
            session = %opts.session_name,
            tab = tab_id,
            pane = %pane.as_str(),
            tags.operation = "zellij.reconcile.close",
            error = &err as &dyn std::error::Error,
            "sidebar reconcile: closing a nested sidebar for rebuild failed; leaving it",
        );
        report.failed += 1;
        return;
    }
    match backend.add_sidebar_to_tab(opts, tab_id) {
        Ok(DockOutcome::Docked) => {
            report.redocked += 1;
            restore_tab_focus(backend, &opts.session_name, tab_id, focused_in_tab);
        }
        Ok(DockOutcome::Misdocked) => {
            report.misdocked += 1;
            restore_tab_focus(backend, &opts.session_name, tab_id, focused_in_tab);
        }
        Err(err) => {
            tracing::warn!(
                session = %opts.session_name,
                tab = tab_id,
                tags.operation = "zellij.reconcile.rebuild",
                error = &err as &dyn std::error::Error,
                "sidebar reconcile: rebuilding a nested sidebar failed",
            );
            report.failed += 1;
        }
    }
}

fn close_planned_sidebars(
    backend: &ZellijBackend,
    session_name: &str,
    close: &[PaneId],
    stale_panes: &std::collections::HashSet<PaneId>,
    stale_close_views: &std::collections::HashMap<PaneId, String>,
    report: &mut SidebarRecovery,
) -> std::collections::HashSet<String> {
    let mut failed_stale_close_views = std::collections::HashSet::new();
    for pane in close {
        match backend.close_pane(session_name, pane) {
            Ok(()) => {
                if stale_panes.contains(pane) {
                    report.stale_closed += 1;
                } else {
                    report.closed += 1;
                }
            }
            Err(err) => {
                if let Some(view) = stale_close_views.get(pane) {
                    failed_stale_close_views.insert(view.clone());
                }
                tracing::warn!(
                    session = %session_name,
                    pane = %pane.as_str(),
                    tags.operation = "zellij.reconcile.close_stray",
                    error = &err as &dyn std::error::Error,
                    "sidebar reconcile: closing a stray sidebar pane failed; leaving it",
                );
            }
        }
    }
    failed_stale_close_views
}

fn add_missing_sidebars(
    backend: &ZellijBackend,
    opts: &SidebarPaneOptions,
    add: &[String],
    restart_add: &std::collections::HashSet<String>,
    failed_stale_close_views: &std::collections::HashSet<String>,
    focused_in_tab: &std::collections::HashMap<u64, u64>,
    report: &mut SidebarRecovery,
) {
    let mut tabs_with_sidebar = existing_sidebar_tabs(backend, &opts.session_name, add);
    for tab in add {
        let Ok(tab_id) = tab.parse::<u64>() else {
            report.failed += 1;
            continue;
        };
        let Some(occupied_tabs) = tabs_with_sidebar.as_mut() else {
            report.failed += 1;
            continue;
        };
        if restart_add.contains(tab) && failed_stale_close_views.contains(tab) {
            report.failed += 1;
            continue;
        }
        if occupied_tabs.contains(tab) {
            warn_sidebar_add_skipped(&opts.session_name, tab_id);
            report.failed += 1;
            continue;
        }
        add_sidebar_to_tab(
            backend,
            opts,
            tab_id,
            restart_add.contains(tab),
            focused_in_tab,
            occupied_tabs,
            report,
        );
    }
}

fn existing_sidebar_tabs(
    backend: &ZellijBackend,
    session_name: &str,
    add: &[String],
) -> Option<std::collections::HashSet<String>> {
    if add.is_empty() {
        return Some(std::collections::HashSet::new());
    }
    match backend.list_panes_with_session(Some(session_name)) {
        Ok(panes) => Some(tabs_with_sidebars(&panes)),
        Err(err) => {
            tracing::warn!(
                session = %session_name,
                tags.operation = "zellij.reconcile.verify",
                error = &err as &dyn std::error::Error,
                "sidebar reconcile: cannot verify sidebar absence before add; skipping adds",
            );
            None
        }
    }
}

fn add_sidebar_to_tab(
    backend: &ZellijBackend,
    opts: &SidebarPaneOptions,
    tab_id: u64,
    restart: bool,
    focused_in_tab: &std::collections::HashMap<u64, u64>,
    occupied_tabs: &mut std::collections::HashSet<String>,
    report: &mut SidebarRecovery,
) {
    match backend.add_sidebar_to_tab(opts, tab_id) {
        Ok(outcome) => {
            if restart {
                report.restarted += 1;
            } else {
                report.recovered += 1;
            }
            if outcome == DockOutcome::Misdocked {
                report.misdocked += 1;
            }
            occupied_tabs.insert(tab_id.to_string());
            restore_tab_focus(backend, &opts.session_name, tab_id, focused_in_tab);
        }
        Err(err) => {
            tracing::warn!(
                session = %opts.session_name,
                tab = tab_id,
                tags.operation = "zellij.reconcile.add",
                error = &err as &dyn std::error::Error,
                "sidebar reconcile: in-place add failed; leaving the tab without a sidebar",
            );
            report.failed += 1;
        }
    }
}

fn restore_tab_focus(
    backend: &ZellijBackend,
    session_name: &str,
    tab_id: u64,
    focused_in_tab: &std::collections::HashMap<u64, u64>,
) {
    const ATTEMPTS: u32 = 5;
    const CYCLE_STEPS: u32 = 8;
    const RETRY_DELAY: Duration = Duration::from_millis(50);

    let Some(work) = focused_in_tab.get(&tab_id).copied() else {
        return;
    };
    for attempt in 0..ATTEMPTS {
        let _ = backend.focus_terminal(session_name, work);
        if tab_focus_is(backend, session_name, tab_id, work) {
            return;
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(RETRY_DELAY);
        }
    }
    for action in ["focus-previous-pane", "focus-next-pane"] {
        for _ in 0..CYCLE_STEPS {
            let _ = backend.zellij_action(session_name).arg(action).run();
            std::thread::sleep(RETRY_DELAY);
            if tab_focus_is(backend, session_name, tab_id, work) {
                return;
            }
        }
    }
}

fn tab_focus_is(backend: &ZellijBackend, session_name: &str, tab_id: u64, raw_id: u64) -> bool {
    backend
        .list_panes_with_session(Some(session_name))
        .is_ok_and(|panes| {
            panes.iter().any(|pane| {
                pane.is_terminal() && pane.tab_id == tab_id && pane.id == raw_id && pane.is_focused
            })
        })
}

fn warn_sidebar_add_skipped(session_name: &str, tab_id: u64) {
    tracing::warn!(
        session = %session_name,
        tab = tab_id,
        tags.operation = "zellij.reconcile.add_skipped",
        "sidebar reconcile: add skipped because the tab still has a sidebar",
    );
}
