//! tmux [`MuxBackend`](crate::mux::MuxBackend) trait implementation.

use super::TmuxBackend;
use super::options::{sidebar_serve_command, tmux_views_with_sidebars};
use super::parse::{parse_focused_client_panes, parse_new_window_ids, parse_pane_line};
use crate::ids::{MuxName, PaneId};
use crate::mux::{
    BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN, BackgroundViewLaunch, BackgroundViewOptions,
    ClientFocusOptions, CommandSpec, DaemonView, MuxBackend, MuxErr, NamedKey, PaneCapture,
    PaneListOptions, PaneListing, Result, SessionOptions, SidebarLiveness, SidebarPaneOptions,
    SidebarRecovery, SplitPaneOptions, TabOptions, ensure_pane_backend,
};

impl MuxBackend for TmuxBackend {
    fn name(&self) -> MuxName {
        MuxName::Tmux
    }

    fn ensure_session(&self, opts: &SessionOptions) -> Result<()> {
        let pin = crate::workspace::pin_env(&opts.workspace_id, &opts.project_root);
        // `new-session -d` births detached; an already-live room answers
        // `duplicate session` (exit 1), which is the goal state and treated as
        // success below. `-A` is unusable here: on a live session it switches
        // to the attach path, which ignores `-d`/`-e`/`-x`/`-y` and needs a
        // terminal on stdin — `CommandSpec` nulls stdin, so it exits 1 with
        // `open terminal failed` (docs/externals/mux-adapter/tmux-reference.md).
        let mut spec = self.cmd().args([
            "new-session".to_owned(),
            "-d".to_owned(),
            "-s".to_owned(),
            opts.session_name.clone(),
            "-c".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
        ]);
        // The identity pin lands in the session environment at birth (`-e`),
        // so the first window's panes already inherit it — `set-environment`
        // below would only reach panes created after it runs.
        for (key, value) in &pin {
            spec = spec.args(["-e".to_owned(), format!("{key}={value}")]);
        }
        // Birth the detached session at the launching terminal's geometry
        // (instead of tmux's 80×24 default), so a fixed-column sidebar split
        // is already correct before the client attaches. The duplicate path
        // skips creation entirely, so a re-ensure never resizes a live room.
        if let Some((cols, rows)) = opts.detected_size {
            spec = spec.args([
                "-x".to_owned(),
                cols.to_string(),
                "-y".to_owned(),
                rows.to_string(),
            ]);
        }
        match spec.run() {
            Ok(_) => {}
            Err(MuxErr::Command { stderr, .. })
                if stderr.to_ascii_lowercase().contains("duplicate session") => {}
            Err(err) => return Err(err),
        }
        // The duplicate path never saw `-e`, so the pin is re-asserted
        // idempotently: future panes of a pre-pin room inherit it; existing
        // panes keep the env they were born with and their participants fall
        // back to the static ladder.
        for (key, value) in &pin {
            self.cmd()
                .args(["set-environment", "-t", &opts.session_name, key, value])
                .run()?;
        }
        self.apply_room_options(&opts.session_name, &opts.config.tmux)
    }

    fn attach_command(
        &self,
        name: &str,
        _config: &crate::config::MultiplexerConfig,
    ) -> CommandSpec {
        self.cmd().args(["attach", "-t", name])
    }

    fn detach(&self, name: &str) -> Result<()> {
        self.cmd()
            .args(["detach-client", "-s", name])
            .run()
            .map(|_| ())
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        // A session that is already gone (or no server at all) is the goal
        // state, so the "can't find session" / "no server" errors are success.
        match self.cmd().args(["kill-session", "-t", name]).run() {
            Ok(_) => Ok(()),
            Err(MuxErr::Command { stderr, .. })
                if {
                    let lower = stderr.to_ascii_lowercase();
                    lower.contains("can't find session")
                        || lower.contains("no server running")
                        || lower.contains("error connecting")
                } =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn list_sessions(&self) -> Result<Vec<String>> {
        // tmux exits 1 with `error connecting to ...` (or `no server
        // running`) on stderr when no server has been started yet. That is
        // an empty list of sessions, not an error condition; the Zellij
        // backend mirrors this shape (exit 0, empty stdout).
        let output = self
            .cmd()
            .args(["list-sessions", "-F", "#{session_name}"])
            .to_command()
            .output()
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
                    program: "tmux".to_owned(),
                },
                _ => MuxErr::Io(err),
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("no server running") || stderr.contains("error connecting") {
                return Ok(Vec::new());
            }
            return Err(MuxErr::Command {
                program: "tmux".to_owned(),
                args: "list-sessions -F #{session_name}".to_owned(),
                stderr: stderr.into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect())
    }

    fn list_panes(&self, opts: PaneListOptions) -> Result<PaneListing> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let observed_at_ms = crate::sidebar::cache::unix_now_ms();
        let mut spec = self.cmd().args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_current_command}\t#{pane_current_path}\t#{pane_pid}\t#{pane_active}\t#{window_name}\t#{pane_title}",
        ]);
        if let Some(session) = opts.session_name {
            spec = spec.args(["-t".to_owned(), session]);
        }
        let output = spec.run_with_timeout(timeout)?;
        let panes = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_pane_line)
            .collect();
        Ok(PaneListing {
            panes,
            observed_at_ms,
            source_active: std::collections::BTreeMap::new(),
        })
    }

    fn focused_client_panes(&self, opts: ClientFocusOptions) -> Result<Vec<PaneId>> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let mut spec = self.cmd().args(["list-clients", "-F", "#{pane_id}"]);
        if let Some(session) = opts.session_name {
            spec = spec.args(["-t".to_owned(), session]);
        }
        let output = spec.run_with_timeout(timeout)?;
        Ok(parse_focused_client_panes(&output.stdout))
    }

    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()> {
        // `-d` keeps focus on the splitting pane; omit it to land in the new
        // pane (the focused launch path).
        let mut spec = self.cmd().args(["split-window", "-h"]);
        if !opts.focus {
            spec = spec.arg("-d");
        }
        for (key, value) in &opts.env {
            spec = spec.args(["-e".to_owned(), format!("{key}={value}")]);
        }
        if let Some(target) = opts.target_pane_id {
            ensure_pane_backend(&target, MuxName::Tmux)?;
            spec = spec.args(["-t".to_owned(), target.raw().to_owned()]);
        }
        if let Some(cwd) = opts.cwd {
            spec = spec.args(["-c".to_owned(), cwd]);
        }
        if let Some(command) = opts.command {
            spec = spec.args(command);
        }
        spec.run().map(|_| ())
    }

    fn focus_pane(&self, pane: &PaneId) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        // `select-pane` activates within its window only — it never switches
        // the session's current window — so a cross-window jump needs
        // `select-window` first. A pane id resolves as a window target to the
        // window holding it, and both verbs ride one batched client call.
        self.batch(&[
            vec![
                "select-window".to_owned(),
                "-t".to_owned(),
                pane.raw().to_owned(),
            ],
            vec![
                "select-pane".to_owned(),
                "-t".to_owned(),
                pane.raw().to_owned(),
            ],
        ])
    }

    fn capture_pane(&self, pane: &PaneId, lines: Option<u16>, ansi: bool) -> Result<PaneCapture> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        let mut spec = self.cmd().args(["capture-pane", "-p", "-t", pane.raw()]);
        if let Some(n) = lines {
            spec = spec.args(["-S".to_owned(), format!("-{n}")]);
        }
        if ansi {
            spec = spec.arg("-e");
        }
        let output = spec.run()?;
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let lines = text.lines().map(ToOwned::to_owned).collect();
        Ok(PaneCapture {
            pane_id: pane.clone(),
            raw_text: text,
            lines,
        })
    }

    fn send_keys(&self, pane: &PaneId, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args(["send-keys", "-l", "-t", pane.raw(), "--", text])
            .run()
            .map(|_| ())
    }

    fn send_key(&self, pane: &PaneId, key: NamedKey) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args(["send-keys", "-t", pane.raw(), key.tmux_name()])
            .run()
            .map(|_| ())
    }

    fn paste_text(&self, pane: &PaneId, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        // Open marker, literal text, close marker — each `-l` so tmux never
        // re-reads the bytes as key names, all in one client invocation.
        let literal = |body: &str| {
            vec![
                "send-keys".to_owned(),
                "-t".to_owned(),
                pane.raw().to_owned(),
                "-l".to_owned(),
                "--".to_owned(),
                body.to_owned(),
            ]
        };
        self.batch(&[
            literal(BRACKET_PASTE_OPEN),
            literal(text),
            literal(BRACKET_PASTE_CLOSE),
        ])
    }

    fn open_sidebar(&self, opts: &SidebarPaneOptions, _daemon: Option<&DaemonView>) -> Result<()> {
        // tmux can reorder windows freely, so the daemon view leads via
        // `open_background_view` (`swap-window`) rather than a birth layout; the
        // `daemon` hint is Zellij's concern and ignored here.
        // Managed sidebar pane per docs/internals/sidebar/multiplexers.md:
        //   tmux split-window -d -h -l <cols> -b -t <session> 'rimz sidebar serve ...'
        // `-d` keeps the spawning client focused on its existing pane;
        // `-b` places the new pane before the target so the sidebar sits
        // on the left. Workspace identity is passed directly to the spawned
        // renderer command.
        // The split sizes from the just-born window: `ensure_session` birthed
        // it at the probed `-x`/`-y` geometry (or an existing room sits at its
        // clients' real geometry), so `target_cols` of the live width is the
        // start verdict in columns. The verdict's percentage spelling is the
        // safe fallback when the width is unreadable.
        let size = match self.window_width(&opts.session_name) {
            Some(total) => opts.width.target_cols(total).to_string(),
            None => format!("{}%", opts.birth_size.percent),
        };
        let command = sidebar_serve_command(opts);
        let mut split = vec![
            "split-window".to_owned(),
            "-d".to_owned(),
            "-h".to_owned(),
            "-l".to_owned(),
            size,
            "-b".to_owned(),
            "-t".to_owned(),
            opts.session_name.clone(),
        ];
        split.extend(command.iter().cloned());

        // Cross-backend parity (DESIGN.md): a Zellij session's layout doubles
        // as its tab template, so every new tab is born with the same
        // sidebar+terminal split. tmux has no tab template, so we install a
        // session-scoped `after-new-window` hook that re-runs the same left
        // split in each new window. `-b -d` keep the sidebar left and focus on
        // the new window's terminal, exactly as the initial window. The hook
        // pins the verdict's fixed columns: a new window instantiates at the
        // attached client's real geometry, and a raw percentage there would
        // re-evaluate against it — exactly how the cap used to vanish.
        let serve = command.join(" ");
        let cols = opts.birth_size.cols;
        let hook = format!("split-window -h -b -d -l {cols} '{serve}'");
        let set_hook = vec![
            "set-hook".to_owned(),
            "-t".to_owned(),
            opts.session_name.clone(),
            "after-new-window".to_owned(),
            hook,
        ];
        // One client invocation births the sidebar and installs the hook.
        self.batch(&[split, set_hook])?;
        // With the `after-new-window` hook installed, re-seed the reborn
        // session's prior agents: each becomes its own window, born
        // `sidebar | agent` as the hook docks the sidebar on its left.
        self.seed_resume_windows(opts);
        Ok(())
    }

    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> Result<SidebarRecovery> {
        // tmux re-adds a sidebar in place with the same left split the initial
        // window got — `-d` keeps the user's focus, `-l <pct>%` sets the width —
        // and drops a stray sidebar with `kill-pane -t`; no move/resize/refocus
        // dance and no session teardown is needed. `split-window` mounts fine on
        // a detached session, so tmux never defers an add the way the Zellij
        // backend must (its detached screen thread drops the mount). Geometry
        // convergence is likewise a deliberate no-op here: `-b` births every
        // sidebar left at the layout width synchronously, so the mis-mounted
        // right/50% shape Zellij repairs cannot occur.
        let panes = self.list_panes(PaneListOptions {
            session_name: Some(opts.session_name.clone()),
            ..Default::default()
        })?;
        let views = tmux_views_with_sidebars(&panes.panes);
        let plan = super::super::plan_reconcile(&views, live);
        let mut report = SidebarRecovery::default();
        let mut failed_stale_close_views = std::collections::HashSet::new();
        for pane in &plan.close {
            match self.kill_pane(pane) {
                Ok(()) => {
                    if live.stale_panes.contains(pane) {
                        report.stale_closed += 1;
                    } else {
                        report.closed += 1;
                    }
                }
                Err(err) => {
                    if let Some(view) = plan.stale_close_views.get(pane) {
                        failed_stale_close_views.insert(view.clone());
                    }
                    tracing::warn!(
                        session = %opts.session_name,
                        pane = %pane.as_str(),
                        tags.operation = "tmux.reconcile.close_stray",
                        error = &err as &dyn std::error::Error,
                        "sidebar reconcile: closing a stray sidebar pane failed; leaving it",
                    );
                }
            }
        }
        for window in &plan.add {
            if plan.restart_add.contains(window) && failed_stale_close_views.contains(window) {
                report.failed += 1;
                continue;
            }
            match self.add_sidebar_to_window(opts, window) {
                Ok(()) => {
                    if plan.restart_add.contains(window) {
                        report.restarted += 1;
                    } else {
                        report.recovered += 1;
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        session = %opts.session_name,
                        window = %window,
                        tags.operation = "tmux.reconcile.add",
                        error = &err as &dyn std::error::Error,
                        "sidebar reconcile: in-place add failed; leaving the window without a sidebar",
                    );
                    report.failed += 1;
                }
            }
        }
        Ok(report)
    }

    fn open_background_view(&self, opts: &BackgroundViewOptions) -> Result<BackgroundViewLaunch> {
        let session = &opts.sidebar.session_name;
        // Idempotent on the window name; a relaunch into a session already
        // carrying the view launches nothing, but still re-asserts its first
        // position. A failed query propagates rather than risk a duplicate window.
        if self.session_has_window(session, &opts.name)? {
            self.lead_window(session, &opts.name);
            return Ok(BackgroundViewLaunch::AlreadyRunning);
        }
        let Some((first, rest)) = opts.hosts.split_first() else {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "background view has no host panes".to_owned(),
            });
        };
        // `-d` opens the window without pulling the user's focus to it; `-P -F`
        // prints the host pane id so extra hosts split beside it, never the
        // sidebar. The session's `after-new-window` hook (installed by
        // `open_sidebar`) docks the global sidebar on its left, so the window is
        // born `sidebar | host0` — the host is always reachable, never a bare
        // trap. Each host closes with its process, so no `remain-on-exit`.
        let output = self
            .cmd()
            .args([
                "new-window".to_owned(),
                "-d".to_owned(),
                "-P".to_owned(),
                "-F".to_owned(),
                "#{pane_id}".to_owned(),
                "-t".to_owned(),
                session.clone(),
                "-n".to_owned(),
                opts.name.clone(),
                "-c".to_owned(),
                first.cwd.to_string_lossy().into_owned(),
            ])
            .args(first.argv.clone())
            .run()?;
        let host0 = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        // Extra hosts (typically just the Codex broker) split beside host0,
        // stacked left-to-right; `-d` keeps host0 the window's active pane.
        for host in rest {
            self.cmd()
                .args([
                    "split-window".to_owned(),
                    "-d".to_owned(),
                    "-h".to_owned(),
                    "-t".to_owned(),
                    host0.clone(),
                    "-c".to_owned(),
                    host.cwd.to_string_lossy().into_owned(),
                ])
                .args(host.argv.clone())
                .run()?;
        }
        self.lead_window(session, &opts.name);
        Ok(BackgroundViewLaunch::Launched)
    }

    fn open_tab(&self, opts: &TabOptions) -> Result<()> {
        let Some((first_column, rest_columns)) = opts.panes.columns.split_first() else {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "tab layout has no columns".to_owned(),
            });
        };
        let Some((first, first_column_rest)) = first_column.split_first() else {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "tab layout has an empty column".to_owned(),
            });
        };
        let output = self
            .cmd()
            .args([
                "new-window".to_owned(),
                "-d".to_owned(),
                "-P".to_owned(),
                "-F".to_owned(),
                "#{window_id}\t#{pane_id}".to_owned(),
                "-t".to_owned(),
                opts.session_name.clone(),
                "-n".to_owned(),
                opts.title.clone(),
                "-c".to_owned(),
                opts.cwd.to_string_lossy().into_owned(),
            ])
            .args(first.argv.clone())
            .run()?;
        let (window_id, first_pane) = parse_new_window_ids(&output.stdout)?;

        let mut column_anchors = vec![first_pane.clone()];
        let mut previous_in_column = first_pane;
        for pane in first_column_rest {
            previous_in_column = self.split_tab_pane(opts, "-v", &previous_in_column, pane)?;
        }
        for column in rest_columns {
            let Some((top, rows)) = column.split_first() else {
                return Err(MuxErr::Output {
                    program: "tmux".to_owned(),
                    reason: "tab layout has an empty column".to_owned(),
                });
            };
            let target = column_anchors
                .last()
                .cloned()
                .unwrap_or_else(|| window_id.clone());
            let new_column = self.split_tab_pane(opts, "-h", &target, top)?;
            column_anchors.push(new_column.clone());
            let mut previous = new_column;
            for row in rows {
                previous = self.split_tab_pane(opts, "-v", &previous, row)?;
            }
        }
        if opts.focus {
            self.cmd()
                .args(["select-window".to_owned(), "-t".to_owned(), window_id])
                .run()?;
        }
        Ok(())
    }

    fn close_pane(&self, _session: &str, pane: &PaneId) -> Result<()> {
        self.kill_pane(pane)
    }

    fn wake_sidebar(&self, _session_name: &str, _bytes: &[u8]) -> Result<()> {
        // tmux has no pipe equivalent; the sidebar wakeup socket is the
        // only channel. Socket fanout lives above this trait in the ledger
        // module.
        Ok(())
    }

    fn version(&self) -> Result<String> {
        if let Some(cached) = self.version.get() {
            return Ok(cached.clone());
        }
        let output =
            self.cmd()
                .arg("-V")
                .to_command()
                .output()
                .map_err(|err| match err.kind() {
                    std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
                        program: "tmux".to_owned(),
                    },
                    _ => MuxErr::Io(err),
                })?;
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        // First writer wins on a probe race; both raced probes read one binary.
        Ok(self.version.get_or_init(|| raw).clone())
    }
}
