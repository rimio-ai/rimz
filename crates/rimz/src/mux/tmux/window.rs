//! tmux window, pane, and tab-layout command helpers.

use std::collections::HashSet;
use std::path::Path;

use crate::ids::{MuxName, PaneId};
use crate::mux::{CommandSpec, MuxErr, Result, SidebarPaneOptions, ensure_pane_backend};

use super::TmuxBackend;
use super::options::sidebar_serve_command;
use super::parse::parse_new_window_ids;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TmuxPaneGeometry {
    pub(super) pane_id: String,
    pub(super) window_id: String,
    pub(super) pane_width: u64,
    pub(super) window_width: u64,
}

/// A tmux window name with its reserved separators neutralized. tmux parses a
/// colon as the `session:window` boundary and a dot as the `window.pane`
/// boundary in a target spec, so `new-window -n` rejects a name carrying
/// either (`invalid window name: run: codex`). Channel labels and run-pane
/// titles are human text that can carry both, so map each to a dash before the
/// name reaches tmux; Zellij tab names have no such constraint, so the mapping
/// stays inside this backend.
pub(super) fn sanitize_window_name(raw: &str) -> String {
    raw.replace([':', '.'], "-")
}

impl TmuxBackend {
    /// Close a single pane by id (`kill-pane -t %N`), terminating its process.
    /// Reconcile uses this to drop a duplicate or unresponsive sidebar pane
    /// without touching the rest of the window.
    pub(super) fn kill_pane(&self, pane: &PaneId) -> Result<()> {
        ensure_pane_backend(pane, MuxName::Tmux)?;
        self.cmd()
            .args([
                "kill-pane".to_owned(),
                "-t".to_owned(),
                pane.raw().to_owned(),
            ])
            .run()
            .map(|_| ())
    }

    /// Split a left sidebar into a specific window in place. When the leftmost
    /// pane already spans the full window height, target that pane so the
    /// sidebar is carved from it alone and the work columns keep their widths.
    /// The `-f` fallback spans the full window edge for split-column left edges,
    /// where tmux can only create a full-height sidebar by redistributing the
    /// remaining width proportionally across the work panes. `-b` places the
    /// pane before/left, `-l <size>` fixes its width, and `-d` keeps the
    /// caller's focus.
    pub(super) fn add_sidebar_to_window(
        &self,
        opts: &SidebarPaneOptions,
        window_id: &str,
    ) -> Result<()> {
        let size = opts.birth_size.cols.to_string();
        let mut args = vec!["split-window".to_owned(), "-d".to_owned(), "-h".to_owned()];
        if let Some(pane) = self.leftmost_full_height_pane(window_id) {
            args.extend([
                "-b".to_owned(),
                "-l".to_owned(),
                size,
                "-t".to_owned(),
                pane,
            ]);
        } else {
            args.extend([
                "-f".to_owned(),
                "-b".to_owned(),
                "-l".to_owned(),
                size,
                "-t".to_owned(),
                window_id.to_owned(),
            ]);
        }
        self.cmd()
            .args(args)
            .args(sidebar_serve_command(opts))
            .run()
            .map(|_| ())
    }

    /// The live column width of `window_id`, when tmux can report it.
    pub(super) fn window_width(&self, window_id: &str) -> Option<u64> {
        let output = self
            .cmd()
            .args(["display-message", "-p", "-t", window_id, "#{window_width}"])
            .run()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }

    /// Every pane's current width and containing-window width in `session`,
    /// collected with one tmux client invocation for sidebar reconciliation.
    pub(super) fn session_pane_geometries(&self, session: &str) -> Result<Vec<TmuxPaneGeometry>> {
        let output = self.session_pane_geometries_command(session).run()?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_tmux_pane_geometry)
            .collect())
    }

    pub(super) fn session_pane_geometries_command(&self, session: &str) -> CommandSpec {
        self.cmd().args([
            "list-panes",
            "-s",
            "-t",
            session,
            "-F",
            "#{pane_id} #{window_id} #{pane_width} #{window_width}",
        ])
    }

    /// Resize a freshly-born tab up to the widest attached client and
    /// re-assert the hook-docked sidebar to its fixed birth width, so agent column
    /// splits land even at full width. Returns whether it resized the window;
    /// the caller must then restore autosizing after placing the splits.
    ///
    /// No-ops when the tab is already at least the widest client's width or
    /// when no sized client is attached.
    pub(super) fn normalize_tab_birth_width(
        &self,
        window_id: &str,
        first_pane: &str,
        sidebar: &SidebarPaneOptions,
    ) -> bool {
        let Some((full_w, full_h)) = self.widest_client_size(&sidebar.session_name) else {
            return false;
        };
        match self.window_width(window_id) {
            Some(width) if width >= full_w => return false,
            _ => {}
        }
        if self
            .cmd()
            .args([
                "resize-window".to_owned(),
                "-t".to_owned(),
                window_id.to_owned(),
                "-x".to_owned(),
                full_w.to_string(),
                "-y".to_owned(),
                full_h.to_string(),
            ])
            .run()
            .is_err()
        {
            return false;
        }

        // The after-new-window hook docks the sidebar with `-b`, which makes
        // it the left-edge pane. Before agent column splits, the only other
        // pane is the first agent; if that is leftmost, no hook ran.
        if let Some(sidebar_pane) = self.leftmost_pane(window_id)
            && sidebar_pane != first_pane
        {
            let sidebar_cols = self
                .after_new_window_hook_cols(&sidebar.session_name)
                .unwrap_or(sidebar.birth_size.cols);
            let _ = self
                .cmd()
                .args([
                    "resize-pane".to_owned(),
                    "-t".to_owned(),
                    sidebar_pane,
                    "-x".to_owned(),
                    sidebar_cols.to_string(),
                ])
                .run();
        }
        true
    }

    /// Undo the `window-size manual` pin that `resize-window` sets in
    /// [`Self::normalize_tab_birth_width`], so the tab tracks client size
    /// again.
    pub(super) fn restore_window_autosize(&self, window_id: &str) {
        let _ = self
            .cmd()
            .args([
                "set-window-option".to_owned(),
                "-u".to_owned(),
                "-t".to_owned(),
                window_id.to_owned(),
                "window-size".to_owned(),
            ])
            .run();
    }

    pub(super) fn remove_sidebar_from_tab(
        &self,
        window_id: &str,
        first_pane: &str,
        rebalance_even: bool,
    ) -> Result<()> {
        if let Some(sidebar_pane) = self.pane_left_of_first_work(window_id, first_pane) {
            self.kill_pane(&PaneId::from_parts(MuxName::Tmux, sidebar_pane))?;
        }
        if !rebalance_even {
            return Ok(());
        }
        self.cmd()
            .args([
                "select-layout".to_owned(),
                "-t".to_owned(),
                window_id.to_owned(),
                "even-horizontal".to_owned(),
            ])
            .run()
            .map(|_| ())
    }

    /// The hook-docked sidebar is born before the first work pane. Use geometry
    /// instead of the sidebar title, which the renderer sets asynchronously.
    fn pane_left_of_first_work(&self, window_id: &str, first_pane: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                window_id,
                "-F",
                "#{pane_id} #{pane_left}",
            ])
            .run()
            .ok()?;
        let mut first_left = None;
        let mut leftmost: Option<(u64, String)> = None;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let (pane_id, left) = line.split_once(' ')?;
            let left: u64 = left.parse().ok()?;
            if pane_id == first_pane {
                first_left = Some(left);
            }
            if leftmost.as_ref().is_none_or(|(min, _)| left < *min) {
                leftmost = Some((left, pane_id.to_owned()));
            }
        }
        let first_left = first_left?;
        let (left, pane_id) = leftmost?;
        (left < first_left).then_some(pane_id)
    }

    /// The `(cols, rows)` of the widest attached client of `session` that
    /// counts toward window sizing. Control-mode `ignore-size` clients are
    /// skipped because they are observers, not display surfaces.
    pub(super) fn widest_client_size(&self, session: &str) -> Option<(u64, u64)> {
        let output = self
            .cmd()
            .args([
                "list-clients",
                "-t",
                session,
                "-F",
                "#{client_width} #{client_height} #{client_flags}",
            ])
            .run()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let width: u64 = fields.next()?.parse().ok()?;
                let height: u64 = fields.next()?.parse().ok()?;
                let flags = fields.next().unwrap_or("");
                (!flags.split(',').any(|flag| flag == "ignore-size")).then_some((width, height))
            })
            .max_by_key(|(width, _)| *width)
    }

    /// The id of the pane at the window's left edge.
    pub(super) fn leftmost_pane(&self, window_id: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                window_id,
                "-F",
                "#{pane_at_left} #{pane_id}",
            ])
            .run()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("1 ").map(|id| id.trim().to_owned()))
    }

    /// The window's leftmost pane when it spans the full window height.
    fn leftmost_full_height_pane(&self, window_id: &str) -> Option<String> {
        let output = self
            .cmd()
            .args([
                "list-panes",
                "-t",
                window_id,
                "-F",
                "#{pane_at_left} #{pane_at_top} #{pane_at_bottom} #{pane_id}",
            ])
            .run()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| {
                let mut fields = line.split_whitespace();
                let at_left = fields.next()?;
                let at_top = fields.next()?;
                let at_bottom = fields.next()?;
                let id = fields.next()?;
                (at_left == "1" && at_top == "1" && at_bottom == "1").then(|| id.to_owned())
            })
    }

    /// Whether `session` already holds a window named `name`. A Rimz background
    /// view is idempotent on its window name, so a relaunch into a session that
    /// already carries it is skipped.
    pub(super) fn session_has_window(&self, session: &str, name: &str) -> Result<bool> {
        let sanitized = sanitize_window_name(name);
        Ok(self
            .window_names(session)?
            .iter()
            .any(|window| window == name || window == &sanitized))
    }

    /// Every window name in `session` — one `list-windows` probe that callers
    /// checking several names share instead of forking per name.
    pub(super) fn window_names(&self, session: &str) -> Result<Vec<String>> {
        let output = self
            .cmd()
            .args(["list-windows", "-t", session, "-F", "#{window_name}"])
            .run()?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_owned())
            .collect())
    }

    /// Force the named window to the session's first slot. tmux tracks the
    /// current window by winlink slot, not stable window id, so swapping into the
    /// base slot can otherwise pull focus to the daemon view. Capture the active
    /// window id before the swap and restore it after; ids survive `swap-window`.
    /// Best-effort: a reorder or focus hiccup never sinks an otherwise-launched
    /// view.
    pub(super) fn lead_window(&self, session: &str, name: &str) {
        let name = sanitize_window_name(name);
        let base = self.base_index();
        let active_window = match self
            .cmd()
            .args(["display-message", "-p", "-t", session, "#{window_id}"])
            .run()
        {
            Ok(output) => {
                let window_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                (!window_id.is_empty()).then_some(window_id)
            }
            Err(err) => {
                tracing::warn!(
                    session = %session,
                    tags.operation = "tmux.capture_active_window",
                    error = &err as &dyn std::error::Error,
                    "could not capture the active window before moving the daemon window",
                );
                None
            }
        };
        if let Err(err) = self
            .cmd()
            .args([
                "swap-window".to_owned(),
                "-d".to_owned(),
                "-s".to_owned(),
                format!("{session}:{name}"),
                "-t".to_owned(),
                format!("{session}:{base}"),
            ])
            .run()
        {
            tracing::warn!(
                session = %session,
                tags.operation = "tmux.move_daemon_window",
                error = &err as &dyn std::error::Error,
                "could not move the daemon window to the front",
            );
            return;
        }
        let Some(active_window) = active_window else {
            return;
        };
        if let Err(err) = self
            .cmd()
            .args(["select-window".to_owned(), "-t".to_owned(), active_window])
            .run()
        {
            tracing::warn!(
                session = %session,
                tags.operation = "tmux.restore_active_window",
                error = &err as &dyn std::error::Error,
                "could not restore focus after moving the daemon window",
            );
        }
    }

    /// Re-seed the reborn session's prior agents, one `#channel` window per
    /// worktree, born `sidebar | agents…` via the `after-new-window` hook.
    /// Idempotent on the window name so a re-run (a heal that re-adds the
    /// sidebar) never doubles a channel window; the freshest channel (the first
    /// in the plan) is selected so attach lands on it, mirroring the Zellij
    /// layout's focus. Best-effort: a failed window is logged and skipped — the
    /// room is still usable.
    pub(super) fn seed_resume_windows(&self, opts: &SidebarPaneOptions) {
        if opts.resume_tabs.is_empty() {
            return;
        }
        // One `list-windows` probe covers every tab's idempotency check. A
        // failed probe means re-seeding cannot be made idempotent, so every
        // channel is left out.
        let existing = match self.window_names(&opts.session_name) {
            Ok(names) => names,
            Err(err) => {
                tracing::warn!(
                    session = %opts.session_name,
                    tags.operation = "tmux.resume.window_probe",
                    error = &err as &dyn std::error::Error,
                    "resume: window probe failed; leaving the agents out",
                );
                return;
            }
        };
        let mut seeded = existing.into_iter().collect::<HashSet<_>>();
        let mut focus_window: Option<String> = None;
        for tab in &opts.resume_tabs {
            let label = sanitize_window_name(&tab.label);
            if !seeded.insert(label.clone()) {
                continue; // already seeded by an earlier birth
            }
            let fallback_shell;
            let first = if let Some(first) = tab
                .layout
                .columns
                .first()
                .and_then(|column| column.panes.first())
            {
                &first.argv
            } else {
                fallback_shell = crate::harness::launch::channel_label_shell_argv(
                    &opts.workspace_id,
                    &opts.project_root,
                    &tab.cwd,
                    &tab.label,
                );
                &fallback_shell
            };
            // `-d` keeps the user on the working window; `-P -F` prints the new
            // window id so we can land focus on the freshest channel without
            // the `session:name` colon ambiguity a label can carry. The agent
            // argv follows directly, run via execvp (no shell), so it needs no
            // quoting.
            let launched = self
                .cmd()
                .args([
                    "new-window".to_owned(),
                    "-d".to_owned(),
                    "-P".to_owned(),
                    "-F".to_owned(),
                    "#{window_id} #{pane_id}".to_owned(),
                    "-t".to_owned(),
                    opts.session_name.clone(),
                    "-n".to_owned(),
                    label.clone(),
                    "-c".to_owned(),
                    tab.cwd.to_string_lossy().into_owned(),
                ])
                .args(first.iter().cloned())
                .run();
            match launched {
                Ok(output) => {
                    let (window_id, first_pane) = match parse_new_window_ids(&output.stdout) {
                        Ok(ids) => ids,
                        Err(err) => {
                            tracing::warn!(
                                session = %opts.session_name,
                                tab = %tab.label,
                                tags.operation = "tmux.resume.launch_window",
                                error = &err as &dyn std::error::Error,
                                "resume: launch window id parse failed; leaving extra panes out",
                            );
                            continue;
                        }
                    };
                    if focus_window.is_none() && !window_id.is_empty() {
                        focus_window = Some(window_id.clone());
                    }
                    // Leave tmux's window layout alone after the hook docks the
                    // sidebar: `select-layout` retiles every pane in the window,
                    // including the managed left sidebar. Additional agents split
                    // the active work area and preserve the sidebar's fixed width.
                    if let Err(err) =
                        self.split_layout_columns(&window_id, &first_pane, &tab.cwd, &tab.layout)
                    {
                        tracing::warn!(
                            session = %opts.session_name,
                            tab = %tab.label,
                            tags.operation = "tmux.resume.split_window",
                            error = &err as &dyn std::error::Error,
                            "resume: launching an agent pane failed; leaving it out",
                        );
                    }
                }
                Err(err) => tracing::warn!(
                    session = %opts.session_name,
                    tab = %tab.label,
                    tags.operation = "tmux.resume.launch_window",
                    error = &err as &dyn std::error::Error,
                    "resume: launching the channel window failed; leaving it out",
                ),
            }
        }
        if let Some(window_id) = focus_window {
            let _ = self
                .cmd()
                .args(["select-window".to_owned(), "-t".to_owned(), window_id])
                .run();
        }
    }

    pub(super) fn split_printed(
        &self,
        direction: &str,
        target: &str,
        size: Option<&str>,
        cwd: &Path,
        argv: &[String],
    ) -> Result<String> {
        self.split_printed_with_reason(
            direction,
            target,
            size,
            cwd,
            argv,
            "split-window did not print a pane id",
        )
    }

    pub(super) fn split_printed_with_reason(
        &self,
        direction: &str,
        target: &str,
        size: Option<&str>,
        cwd: &Path,
        argv: &[String],
        empty_reason: &str,
    ) -> Result<String> {
        let mut args = vec![
            "split-window".to_owned(),
            "-d".to_owned(),
            direction.to_owned(),
            "-P".to_owned(),
            "-F".to_owned(),
            "#{pane_id}".to_owned(),
            "-t".to_owned(),
            target.to_owned(),
        ];
        if let Some(size) = size {
            args.extend(["-l".to_owned(), size.to_owned()]);
        }
        args.extend(["-c".to_owned(), cwd.to_string_lossy().into_owned()]);
        let output = self.cmd().args(args).args(argv.iter().cloned()).run()?;
        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if pane_id.is_empty() {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: empty_reason.to_owned(),
            });
        }
        Ok(pane_id)
    }
}

pub(super) fn parse_tmux_pane_geometry(line: &str) -> Option<TmuxPaneGeometry> {
    let mut fields = line.split_whitespace();
    let geometry = TmuxPaneGeometry {
        pane_id: fields.next()?.to_owned(),
        window_id: fields.next()?.to_owned(),
        pane_width: fields.next()?.parse().ok()?,
        window_width: fields.next()?.parse().ok()?,
    };
    fields.next().is_none().then_some(geometry)
}
