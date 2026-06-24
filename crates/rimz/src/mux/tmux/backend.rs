//! tmux [`MuxBackend`](crate::mux::MuxBackend) trait implementation.

use std::collections::{BTreeMap, HashSet};

use super::TmuxBackend;
use super::options::{
    after_new_window_hook_set_cmd, sidebar_serve_command, tmux_views_with_sidebars,
};
use super::parse::{parse_client_view, parse_new_window_ids, parse_pane_line};
use crate::ids::{MuxName, PaneId};
use crate::mux::{
    BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN, BackgroundViewLaunch, BackgroundViewOptions,
    ClientFocusOptions, ClientView, CommandSpec, DaemonView, MuxBackend, MuxErr, NamedKey,
    PaneCapture, PaneListOptions, PaneListing, Result, SessionOptions, SidebarLiveness,
    SidebarPaneOptions, SidebarRecovery, SplitPaneOptions, TabOptions, ensure_pane_backend,
    memoized_version,
};

impl MuxBackend for TmuxBackend {
    fn name(&self) -> MuxName {
        MuxName::Tmux
    }

    fn ensure_session(&self, opts: &SessionOptions) -> Result<()> {
        let mut env = crate::workspace::pin_env(&opts.workspace_id, &opts.project_root);
        // tmux strips COLORTERM and births panes under `tmux-256color`, whose
        // terminfo carries no RGB cap, so apps inside the room — the sidebar and
        // the user's own TUIs — downgrade to 256-color. Restore it for the room
        // when the launching terminal advertises 24-bit color.
        if opts.truecolor {
            env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
        }
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
        // The birth env lands in the session environment at birth (`-e`),
        // so the first window's panes already inherit it — `set-environment`
        // below would only reach panes created after it runs.
        for (key, value) in &env {
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
        // The duplicate path never saw `-e`, so the birth env is re-asserted
        // idempotently: future panes of a pre-stamp room inherit it; existing
        // panes keep the env they were born with and their participants fall
        // back to the static ladder.
        for (key, value) in &env {
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
            source_active: BTreeMap::new(),
            served_from_topology: false,
        })
    }

    fn client_view(&self, opts: ClientFocusOptions) -> Result<ClientView> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let mut spec = self.cmd().args([
            "list-clients",
            "-F",
            "#{pane_id}\t#{client_activity}\t#{client_flags}",
        ]);
        if let Some(session) = opts.session_name {
            spec = spec.args(["-t".to_owned(), session]);
        }
        let output = spec.run_with_timeout(timeout)?;
        Ok(parse_client_view(&output.stdout))
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

    fn focus_pane(&self, pane: &PaneId, session: Option<&str>) -> Result<()> {
        // tmux pane ids are server-global, so no session target is needed.
        let _ = session;
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

    fn register_focus_key(&self, binding: &super::super::FocusKeyBinding) -> Result<()> {
        // Root keytable (`-n`): the chord fires from any pane in the server, and
        // `run-shell -b` runs the focus command off the server so the pane never
        // blocks. The binding is server-global, so the command resolves the
        // pressing session at keypress (`#{session_name}`) instead of baking one
        // room in — a later room registering the same key stays correct for the
        // first. Re-registering is idempotent: every room writes the same bind.
        self.cmd()
            .args([
                "bind-key".to_owned(),
                "-n".to_owned(),
                binding.chord.to_tmux(),
                "run-shell".to_owned(),
                "-b".to_owned(),
                binding.tmux_run_shell_command(),
            ])
            .run()
            .map(|_| ())
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
        // session-scoped `after-new-window` hook that replays window options and
        // re-runs the same left split in each new window. `-b -d` keep the
        // sidebar left and focus on the new window's terminal, exactly as the
        // initial window. The hook pins the verdict's fixed columns: a new
        // window instantiates at the attached client's real geometry, and a raw
        // percentage there would re-evaluate against it — exactly how the cap
        // used to vanish.
        let set_hook = after_new_window_hook_set_cmd(opts);
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
        if let Err(err) = self.cmd().args(after_new_window_hook_set_cmd(opts)).run() {
            tracing::warn!(
                session = %opts.session_name,
                tags.operation = "tmux.reconcile.install_hook",
                error = &err as &dyn std::error::Error,
                "sidebar reconcile: re-asserting the after-new-window hook failed",
            );
        }
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
        let mut failed_stale_close_views = HashSet::new();
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
        if self.session_has_window(session, &opts.view.name)? {
            self.lead_window(session, &opts.view.name);
            return Ok(BackgroundViewLaunch::AlreadyRunning);
        }
        // `-d` opens the window without pulling the user's focus to it; `-P -F`
        // prints the window and first content pane ids so daemon hosts split
        // beside content, never the sidebar. The session's `after-new-window`
        // hook (installed by `open_sidebar`) docks the global sidebar on its
        // left, so the window is born `sidebar | content`. Daemon hosts, when
        // present, split into a right column sized with the same width verdict
        // as the sidebar. Extra content panes stack inside the middle column.
        // Each process exits with its pane, so no `remain-on-exit`.
        let Some((first_content, rest_content)) = opts.view.content.split_first() else {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "daemon view has no content panes".to_owned(),
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
                session.clone(),
                "-n".to_owned(),
                opts.view.name.clone(),
                "-c".to_owned(),
                first_content.cwd.to_string_lossy().into_owned(),
            ])
            .args(first_content.argv.clone())
            .run()?;
        let (window_id, first_content) = parse_new_window_ids(&output.stdout)?;
        let mut first_daemon_pane = None;
        if let Some((first, rest)) = opts.view.hosts.split_first() {
            let mut split = vec![
                "split-window".to_owned(),
                "-d".to_owned(),
                "-h".to_owned(),
                "-P".to_owned(),
                "-F".to_owned(),
                "#{pane_id}".to_owned(),
                "-t".to_owned(),
                first_content.clone(),
            ];
            if let Some(total) = self.window_width(&window_id) {
                split.extend([
                    "-l".to_owned(),
                    opts.sidebar.width.target_cols(total).to_string(),
                ]);
            }
            split.extend(["-c".to_owned(), first.cwd.to_string_lossy().into_owned()]);
            let output = self.cmd().args(split).args(first.argv.clone()).run()?;
            let first_daemon = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if first_daemon.is_empty() {
                return Err(MuxErr::Output {
                    program: "tmux".to_owned(),
                    reason: "split-window did not print a daemon pane id".to_owned(),
                });
            }
            let mut previous = first_daemon.clone();
            for host in rest {
                let output = self
                    .cmd()
                    .args([
                        "split-window".to_owned(),
                        "-d".to_owned(),
                        "-v".to_owned(),
                        "-P".to_owned(),
                        "-F".to_owned(),
                        "#{pane_id}".to_owned(),
                        "-t".to_owned(),
                        previous,
                        "-c".to_owned(),
                        host.cwd.to_string_lossy().into_owned(),
                    ])
                    .args(host.argv.clone())
                    .run()?;
                previous = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if previous.is_empty() {
                    return Err(MuxErr::Output {
                        program: "tmux".to_owned(),
                        reason: "split-window did not print a daemon pane id".to_owned(),
                    });
                }
            }
            first_daemon_pane = Some(first_daemon);
        }
        let mut previous_content = first_content.clone();
        for content in rest_content {
            let output = self
                .cmd()
                .args([
                    "split-window".to_owned(),
                    "-d".to_owned(),
                    "-v".to_owned(),
                    "-P".to_owned(),
                    "-F".to_owned(),
                    "#{pane_id}".to_owned(),
                    "-t".to_owned(),
                    previous_content,
                    "-c".to_owned(),
                    content.cwd.to_string_lossy().into_owned(),
                ])
                .args(content.argv.clone())
                .run()?;
            previous_content = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if previous_content.is_empty() {
                return Err(MuxErr::Output {
                    program: "tmux".to_owned(),
                    reason: "split-window did not print a content pane id".to_owned(),
                });
            }
        }
        if let Some(first_daemon) = first_daemon_pane {
            if let Err(err) = self
                .cmd()
                .args(["select-pane".to_owned(), "-t".to_owned(), first_daemon])
                .run()
            {
                tracing::warn!(
                    session = %session,
                    view = %opts.view.name,
                    tags.operation = "tmux.daemon_view.focus",
                    error = &err as &dyn std::error::Error,
                    "could not focus the first daemon pane",
                );
            }
        } else if !rest_content.is_empty()
            && let Err(err) = self
                .cmd()
                .args(["select-pane".to_owned(), "-t".to_owned(), first_content])
                .run()
        {
            tracing::warn!(
                session = %session,
                view = %opts.view.name,
                tags.operation = "tmux.daemon_view.focus",
                error = &err as &dyn std::error::Error,
                "could not focus the first content pane",
            );
        }
        self.lead_window(session, &opts.view.name);
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

        // A tab opened from a narrow pane (for example a half-width floating
        // pane) is born at that pane's width, so the hook-docked sidebar and
        // the even column splits below would otherwise be laid out against the
        // narrow birth. Shown later on the full-width client, tmux rescales the
        // window proportionally and inflates the fixed-width sidebar past its
        // cap. Resize the window up to the widest attached client and re-assert
        // the sidebar before the splits so the columns land at full width.
        let normalized = self.normalize_tab_birth_width(&window_id, &first_pane, &opts.sidebar);

        let split_result = (|| {
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
            Ok(())
        })();
        if normalized {
            // `resize-window` pins `window-size=manual`; undo it so the tab
            // tracks client size again like every other tab.
            self.restore_window_autosize(&window_id);
        }
        split_result?;

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

    fn version(&self) -> Result<String> {
        memoized_version(&self.version, &self.cmd().arg("-V"))
    }
}
