//! tmux [`MuxBackend`](crate::mux::MuxBackend) trait implementation.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use super::super::mount_proof::{prove_sidebar_mount, sidebar_build_identity};
use super::TmuxBackend;
use super::layout::{Layout, Refusal};
use super::options::{
    after_new_window_hook_set_cmd, birth_shell_cleanup_hook_set_cmd, birth_split_commands,
    sidebar_serve_command, sidebar_width_option_set_cmd, spawn_command_is_sidebar_serve,
    stack_hook_commands, stack_window_options, window_option_argv,
};
use super::parse::{parse_client_view, parse_floating_pane_ids, parse_pane_line};
use super::window::{SplitAxis, TmuxPaneGeometry, sanitize_window_name};
use crate::ids::{MuxName, PaneId};
use crate::mux::LayoutPanes;
use crate::mux::tab_name::TabNameIntent;
use crate::mux::width::sidebar_width_off_spec;
use crate::mux::{
    BackgroundViewLaunch, BackgroundViewOptions, ClientFocusOptions, ClientView, CommandSpec,
    CompanionAppendErr, CompanionPaneAppend, DaemonView, MuxBackend, MuxErr, PaneCapture,
    PaneListOptions, PaneListing, ReconcileAddOutcome, ReconcilePane, ReconcilePaneRole, Result,
    ResumeTab, ResumeTabShape, ResumeTabUnconfirmed, SessionOptions, SidebarLivenessSource,
    SidebarPaneOptions, SidebarRecovery, SplitDirection, SplitPaneOptions, SplitPlacement,
    SplitTarget, TabOptions, WidthSyncOptions, confirm_resume_tab_shapes, ensure_pane_backend,
    execute_reconcile_plan, group_reconcile_panes, memoized_version,
};
use crate::pane::keys::{BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN, NamedKey, paste_payload};

struct StackLayoutRead {
    raw: String,
    order: Vec<String>,
    members: Vec<String>,
    open: Option<String>,
    has_floating: bool,
}

impl StackLayoutRead {
    fn layout(&self) -> Result<Layout> {
        if self.has_floating {
            return Err(stack_layout_error(Refusal::Floating));
        }
        Layout::parse(&self.raw).ok_or_else(|| stack_layout_error("window layout does not parse"))
    }
}

fn stack_layout_error(reason: impl ToString) -> MuxErr {
    MuxErr::Output {
        program: "tmux".into(),
        reason: reason.to_string(),
    }
}

fn warn_stack_join(target: &str, error: &MuxErr) {
    tracing::warn!(
        pane = target,
        error = error as &dyn std::error::Error,
        tags.operation = "tmux.split_pane.beside",
        "could not split beside the stack; joining it instead"
    );
}

fn server_is_absent(stderr: &str) -> Result<bool> {
    let stderr = stderr.trim();
    if stderr == "no server running" || stderr.starts_with("no server running on ") {
        return Ok(true);
    }
    let Some((socket, reason)) = stderr
        .strip_prefix("error connecting to ")
        .and_then(|text| text.strip_suffix(')'))
        .and_then(|text| text.rsplit_once(" ("))
    else {
        return Ok(false);
    };
    match reason {
        "No such file or directory" => Ok(true),
        "Permission denied" | "Operation not permitted" => Err(MuxErr::SocketConnectDenied {
            mux: MuxName::Tmux,
            socket: socket.into(),
            reason: reason.to_owned(),
        }),
        _ => Ok(false),
    }
}

fn run_session_command(spec: CommandSpec) -> Result<std::process::Output> {
    let output = spec.output_raw_with_timeout(super::super::COMMAND_TIMEOUT)?;
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    // Classify before generic command errors redact the private socket path.
    server_is_absent(&stderr)?;
    Err(spec.command_error(stderr))
}

impl TmuxBackend {
    fn pin_background_window(&self, window: &str) {
        if let Err(err) = self
            .cmd()
            .args(["set-option", "-w", "-t", window, "remain-on-exit", "off"])
            .run()
        {
            tracing::warn!(
                window,
                tags.operation = "tmux.daemon_view.retention",
                error = &err as &dyn std::error::Error,
                "could not pin daemon window to close exited panes",
            );
        }
    }

    fn sweep_dead_background_panes(&self, window: &str) -> Result<bool> {
        let format = concat!(
            "#{pane_id},#{pane_dead},#{s/,/_/g:pane_current_command},",
            "#{s/,/_/g:#{?#{@rimz_title},#{@rimz_title},#{pane_title}}},",
            "#{pane_start_command}",
        );
        let output = self
            .cmd()
            .args(["list-panes", "-t", window, "-F", format])
            .run()?;
        let listing = String::from_utf8_lossy(&output.stdout);
        let mut dead = Vec::new();
        let mut has_live_work = false;
        for line in listing.lines() {
            let fields = line.splitn(5, ',').collect::<Vec<_>>();
            let [pane, exited, command, title, spawn] = fields.as_slice() else {
                return Err(MuxErr::Output {
                    program: "tmux".to_owned(),
                    reason: "invalid daemon pane listing".to_owned(),
                });
            };
            if *exited == "1" {
                dead.push(*pane);
                continue;
            }
            has_live_work |= title.trim() != crate::pane::SIDEBAR_CHROME_TITLE
                && !crate::pane::command_is_sidebar_chrome(command)
                && !spawn_command_is_sidebar_serve(spawn);
        }
        // A sidebar closes when its last sibling vanishes. Rebirth before any
        // sweep when only chrome is live, rather than expose a sidebar-only gap.
        if !has_live_work {
            return Ok(false);
        }
        for pane in dead {
            // Recheck death at the kill and keep the last pane: losing a live
            // sibling during the sweep must not destroy the session's last window.
            self.cmd()
                .args([
                    "if-shell",
                    "-F",
                    "-t",
                    pane,
                    "#{&&:#{pane_dead},#{>:#{window_panes},1}}",
                    &format!("kill-pane -t {pane}"),
                ])
                .run()?;
        }
        Ok(true)
    }

    fn split_pane_phased(
        &self,
        opts: SplitPaneOptions,
    ) -> std::result::Result<Option<PaneId>, CompanionAppendErr> {
        let target = match &opts.target {
            SplitTarget::Ambient => None,
            SplitTarget::Pane(pane) | SplitTarget::SessionPane { pane_id: pane, .. } => {
                ensure_pane_backend(pane, MuxName::Tmux)
                    .map_err(CompanionAppendErr::BeforeSpawn)?;
                Some(pane.raw())
            }
            SplitTarget::Session(session) => Some(session.as_str()),
        };
        let mut snapshot = self.cmd().args(["display-message", "-p"]);
        if let Some(target) = target {
            snapshot = snapshot.args(["-t", target]);
        }
        let snapshot = snapshot
            .arg("#{pane_id}|#{@rimz_stack}")
            .run()
            .map_err(CompanionAppendErr::BeforeSpawn)?;
        let snapshot = String::from_utf8_lossy(&snapshot.stdout);
        let (target, mark) = snapshot
            .trim()
            .split_once('|')
            .unwrap_or((snapshot.trim(), ""));
        let mut stack = if !mark.is_empty() {
            Some(mark.to_owned())
        } else {
            (opts.placement == SplitPlacement::Stacked).then(|| target.to_owned())
        };
        let mut members = Vec::new();
        if let Some(stack) = &stack {
            if mark.is_empty() {
                members.push(target.to_owned());
            } else if !matches!(opts.placement, SplitPlacement::Directional(_)) {
                members = self
                    .stack_members(target, stack)
                    .map_err(CompanionAppendErr::BeforeSpawn)?;
            }
        }
        let mut beside = None;
        if let SplitPlacement::Directional(direction) = opts.placement
            && let Some(stack_id) = &stack
        {
            match self.read_stack_layout(target, stack_id) {
                Ok(read) => {
                    members = read.members.clone();
                    if members.len() == 1 {
                        self.batch(&["@rimz_stack", "@rimz_stack_open"].map(|option| {
                            vec![
                                "set-option".into(),
                                "-pu".into(),
                                "-t".into(),
                                target.into(),
                                option.into(),
                            ]
                        }))
                        .map_err(CompanionAppendErr::BeforeSpawn)?;
                        stack = None;
                    } else {
                        match read.layout().and_then(|layout| {
                            layout
                                .check_stack(&read.order, &members, read.open.as_deref(), direction)
                                .map_err(stack_layout_error)
                        }) {
                            Ok(()) => beside = Some((read, direction)),
                            Err(err) => warn_stack_join(target, &err),
                        }
                    }
                }
                Err(err) => {
                    warn_stack_join(target, &err);
                    members = self
                        .stack_members(target, stack_id)
                        .map_err(CompanionAppendErr::BeforeSpawn)?;
                }
            }
        }
        if stack.is_some() && beside.is_none() {
            // Neutralize restore before making room: the split's layout hook
            // must not squeeze the unmarked newcomer before its open decision.
            let mut commands = members
                .iter()
                .map(|pane| {
                    vec![
                        "set-option".into(),
                        "-pu".into(),
                        "-t".into(),
                        pane.clone(),
                        "@rimz_stack_open".into(),
                    ]
                })
                .collect::<Vec<_>>();
            // Unmarked targets and lone survivors may have a foreign vertical ancestor.
            if members.len() > 1 {
                commands.push(vec![
                    "resize-pane".into(),
                    "-t".into(),
                    target.into(),
                    "-y".into(),
                    "100%".into(),
                ]);
            }
            self.batch(&commands)
                .map_err(CompanionAppendErr::BeforeSpawn)?;
        }
        // `-d` keeps focus on the splitting pane; omit it to land in the new
        // pane (the focused launch path).
        let flag = if beside.is_some() {
            "-h"
        } else {
            match opts.placement {
                SplitPlacement::Directional(SplitDirection::Right)
                | SplitPlacement::Rejoin(SplitDirection::Right) => "-h",
                SplitPlacement::Directional(SplitDirection::Down)
                | SplitPlacement::Rejoin(SplitDirection::Down)
                | SplitPlacement::Stacked => "-v",
            }
        };
        // Splits insert into both native orders immediately after their leaf.
        let split_target = if beside.is_some() {
            members.last().map(String::as_str).unwrap_or(target)
        } else {
            target
        };
        let title = opts.title;
        let mut spec = self.cmd().args([
            "split-window",
            flag,
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            split_target,
        ]);
        if !opts.focus {
            spec = spec.arg("-d");
        }
        for (key, value) in &opts.env {
            spec = spec.args(["-e".to_owned(), format!("{key}={value}")]);
        }
        if let Some(cwd) = opts.cwd {
            spec = spec.args(["-c".to_owned(), cwd]);
        }
        if let Some(command) = opts.command {
            spec = spec.args(command);
        }
        let output = spec.run().map_err(CompanionAppendErr::Spawn)?;
        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if pane_id.is_empty() {
            return Err(CompanionAppendErr::Spawn(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "split-window did not print a pane id".to_owned(),
            }));
        }
        let pane = PaneId::from_parts(MuxName::Tmux, &pane_id);
        if opts.close_on_exit
            && let Err(err) = self.close_pane_on_exit(&pane)
        {
            tracing::warn!(
                pane = %pane,
                tags.operation = "tmux.split_pane.retention",
                error = &err as &dyn std::error::Error,
                "could not pin the spawned pane to close on exit",
            );
        }
        if let Some(title) = title {
            self.set_pane_rimz_title(&pane_id, &title);
        }
        if let Some((before, direction)) = &beside
            && let Some(stack_id) = &stack
        {
            match self.apply_stack_beside(split_target, before, stack_id, &pane_id, *direction) {
                Ok(()) => stack = None,
                Err(err) => warn_stack_join(split_target, &err),
            }
        }
        if let Some(stack) = stack {
            let members = if beside.is_some() || !mark.is_empty() {
                self.stack_members(&pane_id, &stack)
            } else {
                Ok(members)
            };
            // The pane already runs its command: an Err here would send the
            // caller to a fallback placement and orphan it.
            let marked = members.and_then(|mut members| {
                let resize_open = members.len() > 1 || flag == "-v";
                members.push(pane_id.clone());
                self.mark_stack(&members, &stack, &pane_id, true, resize_open)
            });
            if let Err(err) = marked {
                tracing::warn!(
                    pane = %pane_id,
                    tags.operation = "tmux.split_pane.mark_stack",
                    error = &err as &dyn std::error::Error,
                    "could not mark the stacked pane",
                );
            }
        }
        Ok(Some(pane))
    }

    pub(super) fn global_mux_context_removal_command(&self) -> CommandSpec {
        let commands = crate::mux::AMBIENT_MUX_ENV
            .into_iter()
            .chain(crate::mux::REMOTE_CLIENT_ENV)
            .map(|key| {
                ["set-environment", "-g", "-r", key]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .collect::<Vec<_>>();
        self.batch_spec(&commands)
    }

    pub(super) fn new_session_command(
        &self,
        opts: &SessionOptions,
        env: &BTreeMap<String, String>,
    ) -> CommandSpec {
        // `new-session -d` births detached; an already-live room answers
        // `duplicate session` (exit 1), which is the goal state and treated as
        // success by `ensure_session`. `-A` is unusable here: on a live session it switches
        // to the attach path, which ignores `-d`/`-e`/`-x`/`-y` and needs a
        // terminal on stdin — `CommandSpec` nulls stdin, so it exits 1 with
        // `open terminal failed` (docs/externals/mux-adapter/tmux-reference.md).
        let mut spec = self.cmd().without_mux_context().args([
            "new-session".to_owned(),
            "-d".to_owned(),
            "-s".to_owned(),
            opts.session_name.clone(),
            "-c".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
        ]);
        for key in crate::mux::REMOTE_CLIENT_ENV {
            spec = spec.env_remove(key);
        }
        // The birth env lands in the session environment at birth (`-e`),
        // so the first window's panes already inherit it — `set-environment`
        // in `ensure_session` would only reach panes created after it runs.
        for (key, value) in env {
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
        spec
    }

    /// Resize exactly-one-sidebar windows from one geometry snapshot, scoped
    /// to the panes structural reconcile kept or mounted.
    fn converge_live_sidebar_geometries(
        &self,
        opts: &WidthSyncOptions,
        geometries: &[TmuxPaneGeometry],
        only: &HashSet<String>,
    ) -> usize {
        let mut by_window: HashMap<&str, Vec<&TmuxPaneGeometry>> = HashMap::new();
        for geometry in geometries {
            by_window
                .entry(&geometry.window_id)
                .or_default()
                .push(geometry);
        }
        let mut resized = 0;
        for window in by_window.into_values() {
            if window.len() < 2 {
                continue;
            }
            let sidebars: Vec<_> = window
                .into_iter()
                .filter(|geometry| geometry.is_sidebar)
                .collect();
            let [geometry] = sidebars.as_slice() else {
                continue;
            };
            if !only.contains(&geometry.pane_id) {
                continue;
            }
            let view_cols = u16::try_from(geometry.window_width).unwrap_or(u16::MAX);
            let target = u64::from(opts.target.cols(Some(view_cols)).get());
            // tmux's absolute resize converges to the exact requested width.
            if !sidebar_width_off_spec(geometry.pane_width, target, 1) {
                continue;
            }
            match self
                .cmd()
                .args([
                    "resize-pane".to_owned(),
                    "-t".to_owned(),
                    geometry.pane_id.clone(),
                    "-x".to_owned(),
                    target.to_string(),
                ])
                .run()
            {
                Ok(_) => resized += 1,
                Err(err) => tracing::warn!(
                    session = %opts.session_name,
                    pane = %geometry.pane_id,
                    tags.operation = "tmux.converge.resize_sidebar",
                    error = &err as &dyn std::error::Error,
                    "sidebar width convergence failed; leaving it for the next pass",
                ),
            }
        }
        resized
    }
}

impl MuxBackend for TmuxBackend {
    fn view_geometry(
        &self,
        _session: &str,
        anchor: &PaneId,
        timeout: Duration,
    ) -> Result<Option<crate::mux::ViewGeometry>> {
        self.read_view_geometry(anchor, timeout)
    }

    fn close_pane_on_exit(&self, pane: &PaneId) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args([
                "set-option",
                "-p",
                "-t",
                pane.raw(),
                "remain-on-exit",
                "off",
            ])
            .run()
            .map(|_| ())
    }

    fn lagging_views(&self, session: &str) -> Result<Option<crate::mux::LaggingViews>> {
        self.read_lagging_views(session)
    }

    fn lagging_view(
        &self,
        session: &str,
        pane: &PaneId,
    ) -> Result<Option<crate::mux::LaggingView>> {
        self.read_lagging_view(session, pane)
    }

    fn fit_views(
        &self,
        views: &[crate::ids::ViewId],
        target: crate::mux::ViewSize,
        sidebar_cols: std::num::NonZeroU16,
    ) -> Result<Vec<crate::mux::ViewFit>> {
        self.fit_hidden_views(views, target, sidebar_cols)
    }

    fn pane_content_size(
        &self,
        pane: &PaneId,
        _session: Option<&str>,
        timeout: Duration,
    ) -> Result<Option<crate::mux::PaneContentSize>> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        let output = match self
            .cmd()
            .args([
                "display-message",
                "-p",
                "-t",
                pane.raw(),
                "#{pane_height} #{pane_width}",
            ])
            .run_with_timeout(timeout)
        {
            Ok(output) => output,
            Err(MuxErr::Command { stderr, .. }) if stderr.contains("can't find pane") => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let mut fields = text.split_whitespace();
        let rows = fields.next().and_then(|value| value.parse::<u16>().ok());
        let cols = fields.next().and_then(|value| value.parse::<u16>().ok());
        let (Some(rows), Some(cols), None) = (rows, cols, fields.next()) else {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: format!("invalid pane content geometry: {text:?}"),
            });
        };
        Ok(Some(crate::mux::PaneContentSize { rows, cols }))
    }

    fn name(&self) -> MuxName {
        MuxName::Tmux
    }

    fn ensure_session(&self, opts: &SessionOptions) -> Result<()> {
        self.ensure_endpoint_ready()?;
        // Repair a pre-fix server before new-session forks its first pane.
        // This command cannot start a server; absent or stale sockets are
        // expected on a true birth, but other repair failures must surface.
        match run_session_command(self.global_mux_context_removal_command()) {
            Ok(_) => {}
            Err(MuxErr::Command { stderr, .. }) if server_is_absent(&stderr)? => {}
            Err(err) => return Err(err),
        }
        // The runtime domain goes in first so the pin and any caller override
        // win on conflict: a pane inherits concrete HOME/XDG values naming the
        // same store and mux endpoint this client resolved.
        let mut env = crate::disk::paths::runtime_domain_env();
        env.extend(crate::workspace::pin_env(
            &opts.workspace_id,
            &opts.project_root,
        ));
        env.extend(opts.extra_env.clone());
        // tmux strips COLORTERM and births panes under `tmux-256color`, whose
        // terminfo carries no RGB cap, so apps inside the room — the sidebar and
        // the user's own TUIs — downgrade to 256-color. Restore it for the room
        // when the launching terminal advertises 24-bit color.
        if opts.truecolor {
            env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
        }
        match self.new_session_command(opts, &env).run() {
            // Only a session this call created proves anything about the
            // server's cwd: a live room's pane may have been `cd`-ed since.
            Ok(_) => self.verify_birth_cwd(&opts.session_name, &opts.cwd)?,
            Err(MuxErr::Command { stderr, .. })
                if stderr.to_ascii_lowercase().contains("duplicate session") => {}
            Err(err) => return Err(err),
        }
        run_session_command(self.global_mux_context_removal_command())?;
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
        self.attach_existing_command(name)
    }

    fn attach_existing_command(&self, name: &str) -> CommandSpec {
        self.cmd().args(["attach", "-t", name])
    }

    fn attach_readonly_command(&self, name: &str) -> CommandSpec {
        // Client flags on `attach -f` arrived in 3.2, below the 3.5 floor.
        self.cmd()
            .args(["attach", "-t", name, "-r", "-f", "ignore-size"])
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
        match run_session_command(self.cmd().args(["kill-session", "-t", name])) {
            Ok(_) => Ok(()),
            Err(MuxErr::Command { stderr, .. })
                if stderr.to_ascii_lowercase().contains("can't find session")
                    || server_is_absent(&stderr)? =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn list_sessions_within(&self, timeout: Duration) -> Result<Vec<String>> {
        // Only a missing server is an empty list. A denied connect says
        // nothing about whether the room is live.
        let spec = self.cmd().args(["list-sessions", "-F", "#{session_name}"]);
        let output = spec.output_raw_with_timeout(timeout)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if server_is_absent(&stderr)? {
                return Ok(Vec::new());
            }
            return Err(spec.command_error(stderr.into_owned()));
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
        let observed_at_ms = crate::utils::time::unix_now_ms();
        let spec = self.list_panes_command(opts.session_name.as_deref());
        let output = spec.run_with_timeout(timeout)?;
        let mut panes = Vec::new();
        let mut views = BTreeMap::new();
        for (pane, naming) in String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_pane_line)
        {
            if let Some(view_id) = &pane.view_id {
                views.entry(view_id.clone()).or_insert(naming);
            }
            panes.push(pane);
        }
        Ok(PaneListing {
            authoritative: true,
            panes,
            views: views.into_iter().collect(),
            observed_at_ms,
            session_focus: None,
            client_view: None,
            client_sample_withheld: false,
        })
    }

    fn client_view(&self, opts: ClientFocusOptions) -> Result<ClientView> {
        let timeout = opts
            .command_timeout
            .unwrap_or(super::super::COMMAND_TIMEOUT);
        let spec = self.client_view_command(opts.session_name.as_deref());
        let output = spec.run_with_timeout(timeout)?;
        Ok(parse_client_view(&output.stdout))
    }

    fn split_pane(&self, opts: SplitPaneOptions) -> Result<Option<PaneId>> {
        self.split_pane_phased(opts).map_err(|err| match err {
            CompanionAppendErr::BeforeSpawn(err) | CompanionAppendErr::Spawn(err) => err,
        })
    }

    fn append_companion_pane(
        &self,
        mut opts: SplitPaneOptions,
    ) -> std::result::Result<CompanionPaneAppend, CompanionAppendErr> {
        let SplitTarget::SessionPane {
            pane_id: anchor, ..
        } = &opts.target
        else {
            return Ok(CompanionPaneAppend::Full);
        };
        let Some(target) = self
            .companion_append_target(anchor)
            .map_err(CompanionAppendErr::BeforeSpawn)?
        else {
            return Ok(CompanionPaneAppend::Full);
        };
        opts.target = SplitTarget::Pane(target);
        opts.placement = SplitPlacement::Stacked;
        opts.focus = false;
        self.split_pane_phased(opts)?;
        Ok(CompanionPaneAppend::Opened)
    }

    fn focus_pane(&self, pane: &PaneId, session: Option<&str>) -> Result<()> {
        // tmux pane ids are server-global, so no session target is needed.
        let _ = session;
        ensure_pane_backend(pane, MuxName::Tmux)?;
        // `select-pane` sets a hidden window's active pane without switching
        // to it. Then `select-window` shows it with the right pane already
        // active, so the presence watch never sees its stale active pane.
        // A pane id resolves as a window target to the window holding it.
        self.batch(&[
            vec![
                "select-pane".to_owned(),
                "-t".to_owned(),
                pane.raw().to_owned(),
            ],
            vec![
                "select-window".to_owned(),
                "-t".to_owned(),
                pane.raw().to_owned(),
            ],
        ])
    }

    fn toggle_fullscreen(&self, pane: &PaneId, session: Option<&str>) -> Result<()> {
        let _ = session;
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args(["resize-pane", "-Z", "-t", pane.raw()])
            .run()
            .map(|_| ())
    }

    fn nudge_sidebar_width(
        &self,
        _session: &str,
        pane: &PaneId,
        _current_cols: u16,
        target_cols: u16,
    ) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args([
                "resize-pane",
                "-t",
                pane.raw(),
                "-x",
                &target_cols.to_string(),
            ])
            .run()
            .map(|_| ())
    }

    fn record_sidebar_width_default(&self, session: &str, cols: u16) -> Result<()> {
        let cols = std::num::NonZeroU16::new(cols).ok_or_else(|| MuxErr::Output {
            program: "tmux".to_owned(),
            reason: "sidebar width must be greater than zero".to_owned(),
        })?;
        self.cmd()
            .args(sidebar_width_option_set_cmd(session, cols))
            .run()
            .map(|_| ())
    }

    fn register_room_key(&self, binding: &super::super::RoomKeyBinding) -> Result<()> {
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

    fn capture_pane(
        &self,
        pane: &PaneId,
        _session: &str,
        lines: Option<u16>,
        ansi: bool,
    ) -> Result<PaneCapture> {
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
        let (text, lines) = crate::mux::capture_tail(text, lines);
        Ok(PaneCapture {
            pane_id: pane.clone(),
            raw_text: text,
            lines,
        })
    }

    fn send_keys(&self, pane: &PaneId, _session: &str, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args(["send-keys", "-l", "-t", pane.raw(), "--", text])
            .run()
            .map(|_| ())
    }

    fn send_key(&self, pane: &PaneId, _session: &str, key: NamedKey) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args(["send-keys", "-t", pane.raw(), key.tmux_name()])
            .run()
            .map(|_| ())
    }

    fn paste_text(&self, pane: &PaneId, _session: &str, text: &str) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        let payload = paste_payload(text);
        static NEXT_BUFFER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let buffer = format!(
            "rimz-paste-{}-{}",
            std::process::id(),
            NEXT_BUFFER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        // Explicit markers bracket even when the application has not enabled
        // paste mode (unlike `-p`); `-r` preserves our CR-normalized bytes.
        let mut paste = vec![
            "paste-buffer".to_owned(),
            "-d".to_owned(),
            "-r".to_owned(),
            "-b".to_owned(),
            buffer.clone(),
            "-t".to_owned(),
            pane.raw().to_owned(),
        ];
        // tmux 3.7 added control-byte sanitization; older releases paste raw.
        if super::parse_version(&self.version()?).is_some_and(|version| version >= (3, 7, 0)) {
            paste.push("-S".to_owned());
        }
        let result = self
            .batch_spec(&[
                vec![
                    "load-buffer".to_owned(),
                    "-b".to_owned(),
                    buffer.clone(),
                    "-".to_owned(),
                ],
                paste,
            ])
            .stdin_bytes(format!("{BRACKET_PASTE_OPEN}{payload}{BRACKET_PASTE_CLOSE}").into_bytes())
            .run()
            .map(|_| ());
        if result.is_err() {
            let _ = self.cmd().args(["delete-buffer", "-b", &buffer]).run();
        }
        result
    }

    fn open_sidebar(&self, opts: &SidebarPaneOptions, _daemon: Option<&DaemonView>) -> Result<()> {
        // tmux can reorder windows freely, so the daemon view leads via
        // `open_background_view` (`swap-window`) rather than a birth layout; the
        // `daemon` hint is Zellij's concern and ignored here.
        // Fresh tmux births repurpose the first pane as the sidebar and split
        // the work shell to the right at final width. Reattach/recovery keeps
        // the long-standing non-destructive split:
        //   tmux split-window -d -h -c '#{session_path}' -l <cols> -b -t <session> 'rimz sidebar serve ...'
        // `-d` keeps focus on the existing pane; `-b` places the new pane
        // before the target so the sidebar sits on the left. Workspace identity
        // is passed directly to the spawned renderer command.
        // The initial split uses the resolved room target. The renderer
        // refreshes the hook's session option as the shared target or view changes.
        let command = sidebar_serve_command(opts);
        // Cross-backend parity (DESIGN.md): a Zellij session's layout doubles
        // as its tab template, so every new tab is born with the same
        // sidebar+terminal split. tmux has no tab template, so we install a
        // session-scoped `after-new-window` hook that replays window options and
        // re-runs the same left split in each new window. `-b -d` keep the
        // sidebar left and focus on the new window's terminal. The split reads
        // an absolute-column session option so syncs can refresh future births
        // without reconstructing the renderer command.
        let birth_cols = opts
            .target
            .cols(opts.detected_view_size.map(|(cols, _)| cols));
        let set_hook = after_new_window_hook_set_cmd(opts);
        let set_width = sidebar_width_option_set_cmd(&opts.session_name, birth_cols);
        if opts.pristine_birth {
            let sidebar_pane = self
                .sole_current_window_pane(&opts.session_name)
                .ok()
                .flatten();
            let window_width = self.window_width(&opts.session_name);
            if let (Some(sidebar_pane), Some(window_width)) = (sidebar_pane, window_width) {
                let mut commands = birth_split_commands(
                    &sidebar_pane,
                    birth_cols,
                    window_width,
                    &opts.cwd,
                    &command,
                );
                commands.push(set_width);
                commands.push(set_hook);
                // One client invocation respawns the pristine pane as sidebar,
                // splits the focused work shell at its final width, and
                // installs the hook for later windows.
                self.batch(&commands)?;
                // The birth work shell draws its first prompt while the session
                // is still detached. If the attaching client lands at a
                // different width, tmux can resize during that draw and leave
                // zsh's PROMPT_EOL_MARK visible. Respawn the work pane once
                // after the first real attach, at the settled client width.
                if let Some(work_pane) = self.birth_work_pane(&opts.session_name, &sidebar_pane)
                    && let Err(err) = self
                        .cmd()
                        .args(birth_shell_cleanup_hook_set_cmd(
                            &opts.session_name,
                            &work_pane,
                        ))
                        .run()
                {
                    tracing::warn!(
                        session = %opts.session_name,
                        tags.operation = "tmux.birth.cleanup_hook",
                        error = &err as &dyn std::error::Error,
                        "installing the birth-shell cleanup hook failed; the first shell may show a stray %",
                    );
                }
                self.seed_resume_windows(opts);
                return Ok(());
            }
            tracing::debug!(
                session = %opts.session_name,
                "tmux pristine birth could not prove single-pane geometry; using non-destructive sidebar split",
            );
        }

        let size = birth_cols.to_string();
        let mut split = vec![
            "split-window".to_owned(),
            "-d".to_owned(),
            "-h".to_owned(),
            "-c".to_owned(),
            "#{session_path}".to_owned(),
            "-l".to_owned(),
            size,
            "-b".to_owned(),
            "-t".to_owned(),
            opts.session_name.clone(),
        ];
        split.extend(command.iter().cloned());

        // One client invocation births the sidebar and installs the hook.
        self.batch(&[split, set_width, set_hook])?;
        // With the `after-new-window` hook installed, re-seed the reborn
        // session's prior agents: each becomes its own window, born
        // `sidebar | agent` as the hook docks the sidebar on its left.
        self.seed_resume_windows(opts);
        Ok(())
    }

    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &dyn SidebarLivenessSource,
    ) -> Result<SidebarRecovery> {
        let view_cols = match self.widest_client_size(&opts.session_name) {
            Some((cols, _)) => Some(cols),
            None => opts.detected_view_size.map(|(cols, rows)| {
                self.normalize_detached_geometry(&opts.session_name, cols, rows);
                u64::from(cols)
            }),
        };
        let mut hook_commands = Vec::new();
        if let Some(view_cols) = view_cols {
            hook_commands.push(sidebar_width_option_set_cmd(
                &opts.session_name,
                opts.target
                    .cols(Some(u16::try_from(view_cols).unwrap_or(u16::MAX))),
            ));
        }
        hook_commands.push(after_new_window_hook_set_cmd(opts));
        if let Err(err) = self.batch(&hook_commands) {
            tracing::warn!(
                session = %opts.session_name,
                tags.operation = "tmux.reconcile.install_hook",
                error = &err as &dyn std::error::Error,
                "sidebar reconcile: re-asserting the after-new-window hook failed",
            );
        }
        // tmux re-adds a sidebar in place with the same left split the initial
        // window got — `-d` keeps the user's focus, `-l <cols>` sets the width —
        // and drops a stray sidebar with `kill-pane -t`; no move/resize/refocus
        // dance and no session teardown is needed. `split-window` mounts fine on
        // a detached session, so tmux never defers an add the way the Zellij
        // backend must (its detached screen thread drops the mount). Surviving panes
        // outside the cross-backend stop band snap to their per-window targets
        // after the close/add phases.
        let panes = self.list_panes(PaneListOptions {
            session_name: Some(opts.session_name.clone()),
            ..Default::default()
        })?;
        let live = live.sample();
        let views = group_reconcile_panes(panes.panes.iter().filter_map(|pane| {
            if pane.session_name != opts.session_name {
                return None;
            }
            let view = pane.view_id.clone()?;
            let role = ReconcilePaneRole::from_evidence(
                pane.is_rimz_sidebar(),
                crate::pane::pane_is_host(pane),
            );
            Some(ReconcilePane {
                view,
                pane_id: pane.pane_id.clone(),
                role,
            })
        }));
        let plan = super::super::plan_reconcile(&views, &live);
        let planned_closes = plan.close_panes();
        let mutated_views = plan.add_views();
        let mut report = SidebarRecovery::default();
        let build = sidebar_build_identity(opts)?;
        let mut mounted = HashSet::new();
        let failure = execute_reconcile_plan(
            plan,
            &mut report,
            false,
            |window, _replacing| {
                let pane = self.add_sidebar_to_window(opts, window)?;
                if prove_sidebar_mount(opts, MuxName::Tmux, &pane, &build, || {
                    let _ = self.kill_pane(&pane);
                }) {
                    mounted.insert(pane.raw().to_owned());
                    Ok(ReconcileAddOutcome::Verified(pane))
                } else {
                    Err(MuxErr::SidebarMountUnproven {
                        pane: format!("pane `{pane}`"),
                        view: format!("window `{window}`"),
                    })
                }
            },
            |pane| self.kill_pane(pane),
        );
        if let Some(failure) = &failure {
            tracing::warn!(
                session = %opts.session_name,
                view = %failure.view,
                tags.operation = "tmux.reconcile.transaction",
                error = &failure.error as &dyn std::error::Error,
                "sidebar repair aborted; leaving remaining views unchanged",
            );
        }
        let survivors: HashSet<String> = views
            .iter()
            .filter(|view| view.sidebar_panes.len() == 1)
            .filter(|view| !mutated_views.contains(&view.view))
            .filter_map(|view| {
                let pane = view.sidebar_panes.first()?;
                (!planned_closes.contains(pane)).then(|| pane.raw().to_owned())
            })
            .chain(mounted)
            .collect();
        if failure.is_none() && view_cols.is_some() && !survivors.is_empty() {
            match self.session_pane_geometries(&opts.session_name) {
                Ok(geometries) => {
                    let sync = WidthSyncOptions {
                        session_name: opts.session_name.clone(),
                        workspace_id: opts.workspace_id.clone(),
                        target: opts.target,
                        max_percent: opts.max_percent,
                    };
                    report.redocked +=
                        self.converge_live_sidebar_geometries(&sync, &geometries, &survivors);
                }
                Err(err) => tracing::warn!(
                    session = %opts.session_name,
                    tags.operation = "tmux.reconcile.list_geometry",
                    error = &err as &dyn std::error::Error,
                    "sidebar reconcile: probing pane geometry failed; leaving widths unchanged",
                ),
            }
        }
        Ok(report)
    }

    fn open_background_view(&self, opts: &BackgroundViewOptions) -> Result<BackgroundViewLaunch> {
        let session = &opts.sidebar.session_name;
        // A failed name lookup propagates rather than risk a duplicate window.
        let existing_window = self.session_window_id(session, &opts.view.name)?;
        if let Some(window) = existing_window.as_deref() {
            self.pin_background_window(window);
            let has_live_work = match self.sweep_dead_background_panes(window) {
                Ok(live) => live,
                Err(err) => {
                    tracing::warn!(
                        window,
                        tags.operation = "tmux.daemon_view.sweep",
                        error = &err as &dyn std::error::Error,
                        "could not sweep exited daemon panes; leaving them for the next start",
                    );
                    true
                }
            };
            if has_live_work {
                self.lead_window(session, &opts.view.name);
                return Ok(BackgroundViewLaunch::AlreadyRunning);
            }
        }
        // `-d` opens the window without pulling the user's focus to it; `-P -F`
        // prints the window and first content pane ids so daemon hosts split
        // beside content, never the sidebar. The session's `after-new-window`
        // hook (installed by `open_sidebar`) docks the global sidebar on its
        // left, so the window is born `sidebar | content`. Daemon hosts, when
        // present, split into a right column sized with the same width verdict
        // as the sidebar. Extra content panes stack inside the middle column.
        // The window pin covers content and runtime panes, including later splits.
        let Some((first_content, rest_content)) = opts.view.content.split_first() else {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "daemon view has no content panes".to_owned(),
            });
        };
        let opened = self.open_named_window(
            session,
            &opts.view.name,
            &first_content.cwd,
            &first_content.argv,
            None,
            &BTreeMap::new(),
        )?;
        self.pin_background_window(&opened.window_id);
        if let Some(window) = existing_window {
            // Birth first and guard the last window at the kill, in case the
            // replacement's command has already exited and closed its window.
            if let Err(err) = self
                .cmd()
                .args([
                    "if-shell",
                    "-F",
                    "-t",
                    &window,
                    "#{>:#{session_windows},1}",
                    &format!("kill-window -t {window}"),
                ])
                .run()
            {
                tracing::warn!(
                    window,
                    tags.operation = "tmux.daemon_view.replace",
                    error = &err as &dyn std::error::Error,
                    "could not remove the dead daemon window after replacement",
                );
            }
        }
        let view_cols = self
            .window_width(&opened.window_id)
            .map(|cols| u16::try_from(cols).unwrap_or(u16::MAX));
        let first_content = opened.first_pane;
        let mut first_daemon_pane = None;
        let runtime = opts
            .view
            .hosts
            .iter()
            .chain(std::iter::once(&opts.view.loop_panel))
            .collect::<Vec<_>>();
        if let Some((first, rest)) = runtime.split_first() {
            let size = opts.sidebar.target.cols(view_cols).to_string();
            let first_daemon = self.split_printed_with_reason(
                "-h",
                &first_content,
                Some(&size),
                &first.cwd,
                &first.argv,
                "split-window did not print a daemon pane id",
                &BTreeMap::new(),
            )?;
            self.append_equal_host_rows(
                &first_daemon,
                runtime.len(),
                rest.iter().copied(),
                "split-window did not print a daemon pane id",
            )?;
            first_daemon_pane = Some(first_daemon);
        }
        self.append_equal_host_rows(
            &first_content,
            opts.view.content.len(),
            rest_content,
            "split-window did not print a content pane id",
        )?;
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

    fn confirm_resume_tabs(
        &self,
        session: &str,
        tabs: &[ResumeTab],
    ) -> Vec<std::result::Result<(), ResumeTabUnconfirmed>> {
        if tabs.is_empty() {
            return Vec::new();
        }
        // Seeding is synchronous on tmux, so one listing is the outcome.
        let listing = match self.list_panes(PaneListOptions {
            session_name: Some(session.to_owned()),
            ..PaneListOptions::default()
        }) {
            Ok(listing) => listing,
            Err(err) => {
                return vec![Err(ResumeTabUnconfirmed::Unlisted(err.to_string())); tabs.len()];
            }
        };
        // A user's `remain-on-exit` keeps the pane of an agent that exited.
        let dead = match self
            .cmd()
            .args([
                "list-panes",
                "-s",
                "-t",
                session,
                "-f",
                "#{pane_dead}",
                "-F",
                "#{pane_id}",
            ])
            .run()
        {
            Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
            Err(err) => {
                return vec![Err(ResumeTabUnconfirmed::Unlisted(err.to_string())); tabs.len()];
            }
        };
        let config = crate::config::MachineConfig::load_lenient();
        let mut windows: Vec<(&str, ResumeTabShape)> = Vec::new();
        for pane in &listing.panes {
            let (Some(window), Some(name)) = (pane.view_id.as_deref(), pane.view_name.as_deref())
            else {
                continue;
            };
            let work = usize::from(
                !pane.is_rimz_sidebar()
                    && !pane.is_floating
                    && !dead.lines().any(|id| id.trim() == pane.pane_id.raw()),
            );
            match windows.iter_mut().find(|(id, _)| *id == window) {
                Some((_, shape)) => shape.panes += work,
                None => windows.push((
                    window,
                    ResumeTabShape {
                        name: crate::theme::strip_status_glyph_suffix(name, &config.theme)
                            .to_owned(),
                        panes: work,
                    },
                )),
            }
        }
        let live = windows
            .into_iter()
            .map(|(_, shape)| shape)
            .collect::<Vec<_>>();
        let planned = tabs
            .iter()
            .map(|tab| ResumeTabShape {
                // The listing format writes a window name's commas as `_`.
                name: sanitize_window_name(&tab.label).replace(',', "_"),
                panes: tab.pane_count(),
            })
            .collect::<Vec<_>>();
        confirm_resume_tab_shapes(&planned, &live)
    }

    fn open_tab(&self, opts: &TabOptions) -> Result<()> {
        let first = opts.panes.leading_pane("tmux")?;
        let opened = self.open_named_window(
            &opts.sidebar.session_name,
            &opts.title,
            &opts.sidebar.cwd,
            &first.argv,
            opts.after.as_ref(),
            &opts.env,
        )?;
        let window_id = opened.window_id;
        let first_pane = opened.first_pane;
        if let Some(name) = &first.name {
            self.set_pane_rimz_title(&first_pane, name);
        }

        // A tab opened from a narrow pane (for example a half-width floating
        // pane) is born at that pane's width, so the hook-docked sidebar and
        // the even column splits below would otherwise be laid out against the
        // narrow birth. Shown later on the full-width client, tmux rescales the
        // window proportionally and inflates the fixed-width sidebar past its
        // cap. Resize the window up to the widest attached client and re-assert
        // the sidebar before the splits so the columns land at full width.
        let normalized = self.normalize_tab_birth_width(&window_id, &first_pane, &opts.sidebar);

        let split_result =
            self.split_layout_columns(&first_pane, &opts.sidebar.cwd, &opts.panes, &opts.env);
        if normalized {
            // `resize-window` pins `window-size=manual`; undo it so the tab
            // tracks client size again like every other tab.
            self.restore_window_autosize(&window_id);
        }
        let founders = split_result?;
        self.cmd()
            .args(Self::window_owner_args(&window_id, &opts.title, &founders))
            .run()?;
        if !opts.dock_sidebar {
            let rebalance_even = opts
                .panes
                .columns
                .iter()
                .all(|column| column.panes.len() == 1);
            self.remove_sidebar_from_tab(&window_id, &first_pane, rebalance_even)?;
        }

        if opts.focus {
            self.cmd()
                .args(["select-window".to_owned(), "-t".to_owned(), window_id])
                .run()?;
        }
        Ok(())
    }

    fn rename_tab(
        &self,
        _session: &str,
        anchor: &PaneId,
        name: &str,
        intent: TabNameIntent,
    ) -> Result<()> {
        let command = match &intent {
            TabNameIntent::Claim { pane_name } => {
                self.claim_window_command(anchor, name, pane_name)?
            }
            TabNameIntent::Rebuild { base, .. } => {
                self.rebuild_window_command(anchor, name, base)?
            }
            TabNameIntent::Status { .. } | TabNameIntent::Rest { .. } => {
                self.rename_window_command(anchor, name)?
            }
            TabNameIntent::Release { .. } => {
                let output = self
                    .window_pane_ids_command(anchor)?
                    .run_with_timeout(super::super::TAB_RENAME_TIMEOUT)?;
                let panes = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                self.release_window_command(anchor, name, &panes)?
            }
        };
        let command = match intent.observed() {
            Some(observed) => self.window_name_guarded_command(anchor, observed, name, &command)?,
            None => command,
        };
        command
            .run_with_timeout(super::super::TAB_RENAME_TIMEOUT)
            .map(|_| ())
    }

    fn close_pane(&self, _session: &str, pane: &PaneId) -> Result<()> {
        self.kill_pane(pane)
    }

    fn close_view_floating_panes(&self, session: &str, anchor: &PaneId) -> Result<Vec<PaneId>> {
        ensure_pane_backend(anchor, MuxName::Tmux)?;
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                anchor.raw(),
                "-F",
                "#{pane_id},#{pane_floating_flag}",
            ])
            .run()?;
        let mut closed = Vec::new();
        for pane in parse_floating_pane_ids(&output.stdout) {
            match self.kill_pane(&pane) {
                Ok(()) => closed.push(pane),
                Err(err) => tracing::warn!(
                    session,
                    pane = %pane,
                    tags.operation = "tmux.close_floating_pane",
                    error = &err as &dyn std::error::Error,
                    "could not close floating pane during sidebar self-close",
                ),
            }
        }
        Ok(closed)
    }

    fn version(&self) -> Result<String> {
        memoized_version(&self.version, &self.cmd().arg("-V"))
    }
}

impl TmuxBackend {
    fn stack_members(&self, target: &str, stack: &str) -> Result<Vec<String>> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                target,
                "-F",
                "#{pane_id}|#{@rimz_stack}",
            ])
            .run()?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (id, mark) = line.split_once('|')?;
                (mark == stack).then(|| id.to_owned())
            })
            .collect())
    }

    fn read_stack_layout(&self, target: &str, stack: &str) -> Result<StackLayoutRead> {
        let output = self
            .batch_spec(&[
                vec![
                    "display-message".into(),
                    "-p".into(),
                    "-t".into(),
                    target.into(),
                    "#{window_layout}".into(),
                ],
                vec![
                    "list-panes".into(),
                    "-t".into(),
                    target.into(),
                    "-F".into(),
                    "#{pane_id}|#{pane_floating_flag}|#{@rimz_stack}|#{@rimz_stack_open}".into(),
                ],
            ])
            .run()?;
        let output = String::from_utf8_lossy(&output.stdout);
        let mut lines = output.lines();
        let raw = lines
            .next()
            .ok_or_else(|| stack_layout_error("window layout is missing"))?
            .to_owned();
        let mut order = Vec::new();
        let mut members = Vec::new();
        let mut open = None;
        let mut has_floating = false;
        for line in lines {
            let fields = line.split('|').collect::<Vec<_>>();
            let [id, floating, mark, expanded] = fields.as_slice() else {
                return Err(stack_layout_error("invalid pane-list snapshot"));
            };
            has_floating |= !floating.is_empty() && *floating != "0";
            order.push((*id).to_owned());
            if *mark == stack {
                members.push((*id).to_owned());
                if *expanded == "1" && open.replace((*id).to_owned()).is_some() {
                    return Err(stack_layout_error("stack has multiple open members"));
                }
            }
        }
        Ok(StackLayoutRead {
            raw,
            order,
            members,
            open,
            has_floating,
        })
    }

    fn apply_stack_beside(
        &self,
        target: &str,
        before: &StackLayoutRead,
        stack: &str,
        newcomer: &str,
        direction: SplitDirection,
    ) -> Result<()> {
        let after = self.read_stack_layout(target, stack)?;
        if after.members != before.members {
            return Err(stack_layout_error(
                "stack membership changed during the split",
            ));
        }
        let after_layout = after.layout()?;
        after_layout
            .check_split_order(&after.order, &before.order, target, newcomer)
            .map_err(stack_layout_error)?;
        let before_layout = before.layout()?;
        after_layout
            .check_split_geometry(&before_layout, target, newcomer)
            .map_err(stack_layout_error)?;
        let layout = before_layout
            .beside_stack(
                &before.order,
                &before.members,
                before.open.as_deref(),
                newcomer,
                direction,
            )
            .map_err(stack_layout_error)?
            .legacy();
        let applied = "rimz-stack-layout-applied";
        let guard = "@rimz_stack_layout_guard";
        // Legacy select-layout rebinds by order, not the ids in its string.
        // Check the observed post-split tree in the same queue as the rewrite.
        let unchanged = format!("#{{==:#{{window_layout}},#{{{guard}}}}}");
        let apply = format!(
            "select-layout -t {target} '{layout}' ; display-message -p -t {target} '{applied}'"
        );
        let output = self
            .batch_spec(&[
                vec![
                    "set-option".into(),
                    "-w".into(),
                    "-t".into(),
                    target.into(),
                    guard.into(),
                    after.raw,
                ],
                vec![
                    "if-shell".into(),
                    "-F".into(),
                    "-t".into(),
                    target.into(),
                    unchanged,
                    apply,
                ],
                vec![
                    "set-option".into(),
                    "-wu".into(),
                    "-t".into(),
                    target.into(),
                    guard.into(),
                ],
            ])
            .run();
        if output.is_err() {
            let _ = self
                .cmd()
                .args(["set-option", "-wu", "-t", target, guard])
                .run();
        }
        let output = output?;
        if !String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == applied)
        {
            return Err(stack_layout_error(
                "window layout changed before the guarded apply",
            ));
        }
        Ok(())
    }

    /// Birth a tab's layout inside `first_pane`: every column first, as one
    /// sized `-h` run while each target is still full height, then each
    /// column's rows as a sized `-v` run inside it. Splitting rows first would
    /// leave the next column nested inside column 0's top row, inheriting its
    /// height; sizing both runs in cells leaves tiled rectangles even at
    /// creation. Stacked columns then collapse around their open member.
    pub(super) fn split_layout_columns(
        &self,
        first_pane: &str,
        cwd: &Path,
        panes: &LayoutPanes,
        env: &BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        let Some((first_column, rest_columns)) = panes.columns.split_first() else {
            return Ok(vec![first_pane.to_owned()]);
        };
        // Split every column open before the first tmux command runs, so an
        // empty column errors out of an untouched window.
        let mut column_rows = vec![first_column.split_leading("tmux")?.1];
        let mut column_tops = Vec::with_capacity(rest_columns.len());
        for column in rest_columns {
            let (top, rows) = column.split_leading("tmux")?;
            column_tops.push(top);
            column_rows.push(rows);
        }
        let mut anchors = vec![first_pane.to_owned()];
        anchors.extend(self.split_even_run(
            SplitAxis::Columns,
            first_pane,
            cwd,
            &column_tops,
            env,
        )?);
        let mut layout_order = Vec::new();
        for ((anchor, rows), column) in anchors.iter().zip(column_rows).zip(&panes.columns) {
            let rows = rows.iter().collect::<Vec<_>>();
            let start = layout_order.len();
            layout_order.push(anchor.clone());
            layout_order.extend(self.split_even_run(SplitAxis::Rows, anchor, cwd, &rows, env)?);
            if column.stacked {
                let members = &layout_order[start..];
                let open = panes
                    .focus_position()
                    .checked_sub(start)
                    .and_then(|index| members.get(index))
                    .unwrap_or(members.last().unwrap_or(anchor));
                self.mark_stack(members, anchor, open, false, true)?;
            }
        }
        // Every split ran with `-d`, so the leading pane is already active.
        let focus_position = panes.focus_position();
        if focus_position > 0 {
            self.cmd()
                .args(["select-pane", "-t", &layout_order[focus_position]])
                .run()?;
        }
        Ok(layout_order)
    }

    fn mark_stack(
        &self,
        members: &[String],
        stack: &str,
        open: &str,
        keep_focus: bool,
        resize_open: bool,
    ) -> Result<()> {
        let mut commands = stack_hook_commands();
        for pane in members {
            commands.push(vec![
                "set-option".into(),
                "-p".into(),
                "-t".into(),
                pane.clone(),
                "@rimz_stack".into(),
                stack.into(),
            ]);
            commands.push(vec![
                "set-option".into(),
                "-pu".into(),
                "-t".into(),
                pane.clone(),
                "@rimz_stack_open".into(),
            ]);
        }
        let open = if keep_focus {
            let focused = members
                .iter()
                .map(|pane| {
                    [
                        "#{?#{&&:#{pane_active},#{==:#{pane_id},",
                        pane,
                        "}},#{pane_id},}",
                    ]
                    .concat()
                })
                .collect::<String>();
            let focused = format!("#{{P:{focused}}}");
            format!("#{{?{focused},{focused},{open}}}")
        } else {
            open.to_owned()
        };
        let mut open_command = format!("set-option -p -t {open} @rimz_stack_open 1");
        if resize_open {
            open_command.push_str(&format!(
                " ; if-shell -F '#{{!=:#{{window_zoomed_flag}},1}}' {{ resize-pane -t {open} -y #{{window_height}} }}"
            ));
        }
        let borders = stack_window_options(&Default::default(), true)
            .into_iter()
            .filter(|(key, _)| {
                matches!(
                    *key,
                    "pane-border-status" | "pane-border-format" | "@rimz_stack_borders_owned"
                )
            })
            .map(|(key, value)| {
                window_option_argv(key, value.as_deref(), Some(&members[0]))
                    .into_iter()
                    .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join(" ; ");
        // Apply borders before expansion: tmux <3.8 can subtract a status row from a one-row cell.
        commands.push(vec![
            "if-shell".into(),
            "-F".into(),
            "-t".into(),
            members[0].clone(),
            "#{@rimz_stack_borders}".into(),
            borders,
        ]);
        commands.push(vec![
            "run-shell".into(),
            "-C".into(),
            "-t".into(),
            members[0].clone(),
            open_command,
        ]);
        self.batch(&commands)
    }

    fn sole_current_window_pane(&self, session: &str) -> Result<Option<String>> {
        let output = self
            .cmd()
            .args(["list-panes", "-t", session, "-F", "#{pane_id}"])
            .run()?;
        let panes: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        Ok(match panes.as_slice() {
            [pane] => Some(pane.clone()),
            _ => None,
        })
    }

    /// The pristine birth work pane is the one initial-window pane that is not
    /// the sidebar. Called immediately after `birth_split_commands`, before
    /// resume windows can move focus or add panes.
    fn birth_work_pane(&self, session: &str, sidebar_pane: &str) -> Option<String> {
        let output = self
            .cmd()
            .args(["list-panes", "-t", session, "-F", "#{pane_id}"])
            .run()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .find(|id| !id.is_empty() && *id != sidebar_pane)
            .map(ToOwned::to_owned)
    }

    pub(super) fn list_panes_command(&self, session_name: Option<&str>) -> CommandSpec {
        let format = "#{s/,/_/g:session_name},#{window_id},#{pane_id},#{s/,/_/g:pane_current_command},#{s/,/_/g:pane_current_path},#{pane_pid},#{s/,/_/g:window_name},#{s/,/_/g:#{?#{@rimz_title},#{@rimz_title},#{pane_title}}},#{pane_floating_flag},#{s/,/_/g:pane_start_command},#{automatic-rename},#{s/,/_/g:@rimz_tab_base},#{@rimz_tab_founders}";
        match session_name {
            Some(session) => self
                .cmd()
                .args(["list-panes", "-s", "-t", session, "-F", format]),
            None => self.cmd().args(["list-panes", "-a", "-F", format]),
        }
    }

    pub(super) fn client_view_command(&self, session_name: Option<&str>) -> CommandSpec {
        let format = "#{client_name}|#{pane_id}|#{client_activity}|#{client_flags}";
        let mut spec = self.cmd().args(["list-clients", "-F", format]);
        if let Some(session) = session_name {
            spec = spec.args(["-t", session]);
        }
        spec
    }
}
