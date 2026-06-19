//! tmux window, pane, and tab-layout command helpers.

use crate::ids::{MuxName, PaneId};
use crate::mux::{MuxErr, PaneCmd, Result, SidebarPaneOptions, TabOptions, ensure_pane_backend};

use super::TmuxBackend;
use super::options::sidebar_serve_command;

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

    /// Split a left sidebar into a specific window in place, mirroring the
    /// initial-window split: `-b` (before/left), `-l <size>` (width), `-d`
    /// (keep the caller's focus). The `-t <window_id>` target leaves every other
    /// window untouched. The heal sizes from the live window — `target_cols`
    /// of `#{window_width}` — never from `opts.birth_size`: a reconcile can run
    /// from a terminal (or no terminal) unrelated to the session's clients.
    /// When the width is unreadable, the percentage is the safe fallback.
    pub(super) fn add_sidebar_to_window(
        &self,
        opts: &SidebarPaneOptions,
        window_id: &str,
    ) -> Result<()> {
        let size = match self.window_width(window_id) {
            Some(total) => opts.width.target_cols(total).to_string(),
            None => format!("{}%", opts.width.percent),
        };
        self.cmd()
            .args([
                "split-window".to_owned(),
                "-d".to_owned(),
                "-h".to_owned(),
                "-b".to_owned(),
                "-l".to_owned(),
                size,
                "-t".to_owned(),
                window_id.to_owned(),
            ])
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

    /// Whether `session` already holds a window named `name`. A Rimz background
    /// view is idempotent on its window name, so a relaunch into a session that
    /// already carries it is skipped.
    pub(super) fn session_has_window(&self, session: &str, name: &str) -> Result<bool> {
        Ok(self
            .window_names(session)?
            .iter()
            .any(|window| window == name))
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

    /// Force the named window to the session's first slot. tmux opens the daemon
    /// window last (`-d`, no focus change), so swap it with the base-index window
    /// — `swap-window` always succeeds even when that slot is occupied, and `-d`
    /// keeps the user on their working window, so no focus-return is needed.
    /// Best-effort: a reorder hiccup never sinks an otherwise-launched view.
    pub(super) fn lead_window(&self, session: &str, name: &str) {
        let base = self.base_index();
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
        let mut seeded = existing
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let mut focus_window: Option<String> = None;
        for tab in &opts.resume_tabs {
            if !seeded.insert(tab.label.clone()) {
                continue; // already seeded by an earlier birth
            }
            let Some(first) = tab.panes.first() else {
                tracing::warn!(
                    session = %opts.session_name,
                    tab = %tab.label,
                    tags.operation = "tmux.resume.empty_tab",
                    "resume: channel tab has no panes; leaving it out",
                );
                continue;
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
                    "#{window_id}".to_owned(),
                    "-t".to_owned(),
                    opts.session_name.clone(),
                    "-n".to_owned(),
                    tab.label.clone(),
                    "-c".to_owned(),
                    tab.cwd.to_string_lossy().into_owned(),
                ])
                .args(first.clone())
                .run();
            match launched {
                Ok(output) => {
                    let window_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                    if focus_window.is_none() && !window_id.is_empty() {
                        focus_window = Some(window_id.clone());
                    }
                    for argv in tab.panes.iter().skip(1) {
                        if let Err(err) = self
                            .cmd()
                            .args([
                                "split-window".to_owned(),
                                "-d".to_owned(),
                                "-t".to_owned(),
                                window_id.clone(),
                                "-c".to_owned(),
                                tab.cwd.to_string_lossy().into_owned(),
                            ])
                            .args(argv.clone())
                            .run()
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
                    let _ = self
                        .cmd()
                        .args([
                            "select-layout".to_owned(),
                            "-t".to_owned(),
                            window_id,
                            "tiled".to_owned(),
                        ])
                        .run();
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

    pub(super) fn split_tab_pane(
        &self,
        opts: &TabOptions,
        direction: &str,
        target: &str,
        pane: &PaneCmd,
    ) -> Result<String> {
        let output = self
            .cmd()
            .args([
                "split-window".to_owned(),
                "-d".to_owned(),
                direction.to_owned(),
                "-P".to_owned(),
                "-F".to_owned(),
                "#{pane_id}".to_owned(),
                "-t".to_owned(),
                target.to_owned(),
                "-c".to_owned(),
                opts.cwd.to_string_lossy().into_owned(),
            ])
            .args(pane.argv.clone())
            .run()?;
        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if pane_id.is_empty() {
            return Err(MuxErr::Output {
                program: "tmux".to_owned(),
                reason: "split-window did not print a pane id".to_owned(),
            });
        }
        Ok(pane_id)
    }
}
