//! Zellij `MuxBackend` implementation.
//!
//! Interactive actions run `zellij action <verb> ...` against the session
//! inferred from the caller's `ZELLIJ_SESSION_NAME` env var. Operations that
//! may run before the user attaches, such as native sidebar launch and wakeup
//! fanout, carry the session name explicitly via `zellij --session <name>`.
//!
//! Caveats live in `docs/internals/multiplexers.md` under
//! "Zellij backend caveats" — namely that raw Zellij pane IDs are
//! integers, scoped per-session, and that the spike does not yet expose
//! tab-level operations beyond what's needed to identify a pane.

use std::env;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    BackgroundViewLaunch, BackgroundViewOptions, CommandSpec, MuxBackend, MuxErr, PaneCapture,
    PaneListOptions, Result, SessionOptions, SidebarPaneOptions, SidebarRecovery, SplitPaneOptions,
    ensure_pane_backend,
};
use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, ViewKind};

/// Minimum Zellij version that ships the pipe-broadcast semantics Rimz uses
/// as a best-effort wakeup optimization.
pub const MIN_ZELLIJ_VERSION: (u32, u32, u32) = (0, 41, 0);

/// Pane name the sidebar layout assigns, and the title Zellij reports back for
/// it. The sole source of truth for both rendering the layout and detecting
/// whether a live session still carries its sidebar.
const SIDEBAR_PANE_NAME: &str = "rimz-sidebar";

/// Zellij's action client occasionally answers `list-panes` with an empty
/// stdout and a success status when the session server is mid-tick — a known
/// race that a short retry clears. Without this, the sidebar's snapshot loop
/// flashes a "could not parse mux output: EOF" alert for a single blip.
const LIST_PANES_ATTEMPTS: u32 = 3;
const LIST_PANES_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Ceiling on how long `create_session_with_sidebar` holds the temp layout file
/// on disk while waiting for Zellij to parse it (Zellij reads `--default-layout`
/// asynchronously, after the create call returns). A *ceiling*, not a fixed
/// wait: a healthy birth materializes the sidebar sub-second and returns at
/// once; this only bounds the pathological case where the layout never parses,
/// where deleting the file early births a sidebar-less session.
const SIDEBAR_LAYOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// Bundle reported by `rimz doctor` when the active backend is Zellij.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZellijCapabilities {
    pub binary_version: String,
    pub parsed_version: Option<(u32, u32, u32)>,
    pub meets_min_version: bool,
}

/// Probe the installed Zellij. Cheap: one `zellij --version` call.
pub fn capabilities() -> Result<ZellijCapabilities> {
    let raw = ZellijBackend::default().version()?;
    let parsed = parse_version(&raw);
    Ok(ZellijCapabilities {
        meets_min_version: parsed.is_some_and(|v| v >= MIN_ZELLIJ_VERSION),
        binary_version: raw,
        parsed_version: parsed,
    })
}

/// Parse `"zellij 0.41.2"` (and tolerant of leading/trailing whitespace).
/// Returns None when the shape is unexpected so `doctor` can render the
/// raw string verbatim.
fn parse_version(raw: &str) -> Option<(u32, u32, u32)> {
    let trimmed = raw.trim();
    let after_prefix = trimmed.strip_prefix("zellij ").unwrap_or(trimmed);
    let mut parts = after_prefix
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()?
        .split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

#[derive(Clone, Debug, Default)]
pub struct ZellijBackend {
    /// Override for `XDG_RUNTIME_DIR`, where Zellij locates its server socket.
    /// `None` inherits the process env (production); integration tests set a
    /// private tempdir so each test drives its own Zellij server — the
    /// isolation that lets them run in parallel and across worktrees with no
    /// shared lock. Mirrors [`super::TmuxBackend`]'s `with_socket` seam.
    runtime_dir: Option<PathBuf>,
}

impl ZellijBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin every Zellij command this backend runs to `dir` as `XDG_RUNTIME_DIR`,
    /// so a test's server, sessions, and sockets never touch the user's.
    pub fn with_runtime_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            runtime_dir: Some(dir.into()),
        }
    }

    /// Base `CommandSpec` for every Zellij invocation — the single chokepoint.
    /// The program honors `RIMZ_ZELLIJ_BIN` (the binary override the wakeup walk
    /// may point at a test shim; see `tests/fixtures/zellij-trace`), and the
    /// optional `XDG_RUNTIME_DIR` env scopes the server socket. Threading
    /// isolation through one field this way keeps it impossible for a stray
    /// command to escape to the user's default server.
    fn cmd(&self) -> CommandSpec {
        let program = env::var("RIMZ_ZELLIJ_BIN").unwrap_or_else(|_| "zellij".to_owned());
        let mut spec = CommandSpec::new(program);
        if let Some(dir) = &self.runtime_dir {
            spec = spec.env("XDG_RUNTIME_DIR", dir.to_string_lossy().into_owned());
        }
        spec
    }

    fn list_panes_with_session(&self, session: Option<&str>) -> Result<Vec<RawPane>> {
        let mut spec = self.cmd();
        if let Some(name) = session {
            spec = spec.args(["--session".to_owned(), name.to_owned()]);
        }
        spec = spec.args(["action", "list-panes", "-j", "-a"]);
        for attempt in 0..LIST_PANES_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(LIST_PANES_RETRY_DELAY);
            }
            let output = spec.run()?;
            if is_transient_empty(&output.stdout) {
                continue;
            }
            return serde_json::from_slice::<Vec<RawPane>>(&output.stdout).map_err(|e| {
                MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!("parsing list-panes JSON: {e}"),
                }
            });
        }
        Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("list-panes returned no output after {LIST_PANES_ATTEMPTS} attempts"),
        })
    }

    /// Whether `name`'s session currently carries a running `rimz-sidebar`
    /// pane. A held sidebar is still broken: Zellij is waiting for the user to
    /// approve the command, so the renderer is not producing heartbeats and
    /// future tabs inherit the bad launch behavior.
    ///
    /// Best-effort: a failed listing reads as "unhealthy" so the caller heals
    /// rather than trusts a session it cannot inspect.
    fn session_has_healthy_sidebar(&self, name: &str) -> bool {
        self.list_panes_with_session(Some(name))
            .map(|panes| {
                let mut found = false;
                for pane in panes.iter().filter(|pane| is_sidebar_pane(pane)) {
                    found = true;
                    if pane.is_held {
                        return false;
                    }
                }
                found
            })
            .unwrap_or(false)
    }

    /// Classify `name`'s liveness from `zellij list-sessions`. A present session
    /// always lists with exit code 0; the command only fails ("No active zellij
    /// sessions found.", exit 1) when there are none, so any failure here means
    /// the session is absent and a fresh birth should proceed.
    fn session_state(&self, name: &str) -> SessionState {
        let Ok(output) = self.cmd().args(["list-sessions", "--no-formatting"]).run() else {
            return SessionState::Absent;
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| session_state_from_line(line, name))
            .unwrap_or(SessionState::Absent)
    }

    /// Force-delete a session (exited or live) so the next create births a clean
    /// one from the layout rather than resurrecting a stale serialized layout or
    /// attaching to a sidebar-less leftover. `--force` also kills a live session.
    /// A session that vanished between the liveness check and here is already in
    /// the state we want, so "not found" is success.
    fn delete_session(&self, name: &str) -> Result<()> {
        match self.cmd().args(["delete-session", name, "--force"]).run() {
            Ok(_) => Ok(()),
            Err(MuxErr::Command { stderr, .. })
                if stderr.to_ascii_lowercase().contains("not found") =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// Create the background session from a layout that puts the `rimz-sidebar`
    /// pane on the left and focuses the user's terminal on the right. The layout
    /// doubles as the default tab template, so new tabs are born with a sidebar
    /// too. The sidebar pane is `close_on_exit`, so when its own process exits
    /// the pane closes — see the self-close loop in `crates/rimz-sidebar`.
    ///
    /// Zellij parses `--default-layout` asynchronously, after the
    /// `--create-background` client returns, so the temp layout file must
    /// outlive the call. We hold it through a bounded wait for the sidebar pane
    /// to appear, then let it drop.
    fn create_session_with_sidebar(&self, opts: &SidebarPaneOptions) -> Result<()> {
        let layout = TempLayoutFile::new(render_sidebar_layout(opts)?)?;
        let spec = self.cmd().args([
            "attach".to_owned(),
            "--create-background".to_owned(),
            opts.session_name.clone(),
            "options".to_owned(),
            "--default-cwd".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
            "--default-layout".to_owned(),
            layout.path().to_string_lossy().into_owned(),
        ]);
        let mut command = spec.to_command();
        command.current_dir(&opts.cwd);
        let output = command.output().map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
                program: spec.program.clone(),
            },
            _ => MuxErr::Io(err),
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let lower = stderr.to_ascii_lowercase();
            // A racing `rimz` may have created the session first; treat that as
            // success rather than re-injecting.
            if !(lower.contains("already exists")
                || (lower.contains("session") && lower.contains("exists")))
            {
                return Err(MuxErr::Command {
                    program: spec.program,
                    args: spec.args.join(" "),
                    stderr,
                });
            }
        }
        if !self.wait_for_sidebar_layout(&opts.session_name) {
            tracing::warn!(
                session = %opts.session_name,
                "sidebar layout did not materialize within the ceiling; dropping the temp \
                 layout may leave the session sidebar-less — it self-heals on the next open_sidebar",
            );
        }
        drop(layout);
        Ok(())
    }

    /// `zellij --session <name> action <verb> …` — the session-scoped action
    /// form the pane listing and wakeup fanout use, so recovery works whether or
    /// not the caller is attached.
    fn zellij_action(&self, session: &str) -> CommandSpec {
        self.cmd().args([
            "--session".to_owned(),
            session.to_owned(),
            "action".to_owned(),
        ])
    }

    fn focus_terminal(&self, session: &str, raw_id: u64) -> Result<()> {
        self.zellij_action(session)
            .args(["focus-pane-id".to_owned(), format!("terminal_{raw_id}")])
            .run()
            .map(|_| ())
    }

    /// `zellij --session <name> action go-to-tab <index>` (1-based). A
    /// background view's working tab is the session's first, so this returns
    /// focus there after `new-tab` steals it.
    fn go_to_tab(&self, session: &str, index: u32) -> Result<()> {
        self.zellij_action(session)
            .args(["go-to-tab".to_owned(), index.to_string()])
            .run()
            .map(|_| ())
    }

    /// Inject a left-docked sidebar into a live tab without a rebirth: split a
    /// pane to the right, move it left, then resize it toward the layout width.
    fn add_sidebar_to_tab(&self, opts: &SidebarPaneOptions, tab_id: u64) -> Result<()> {
        let new_pane = self.new_sidebar_pane(opts, tab_id)?;
        self.zellij_action(&opts.session_name)
            .args([
                "move-pane".to_owned(),
                "left".to_owned(),
                "--pane-id".to_owned(),
                new_pane.clone(),
            ])
            .run()?;
        self.resize_sidebar_toward(&opts.session_name, tab_id, &new_pane, opts.width_percent);
        Ok(())
    }

    /// `new-pane` to the right of the tab's focus, titled and `close_on_exit` to
    /// match the layout, running the same `rimz sidebar serve` command. Returns
    /// the created pane id Zellij prints (e.g. `terminal_58`).
    fn new_sidebar_pane(&self, opts: &SidebarPaneOptions, tab_id: u64) -> Result<String> {
        let args: Vec<String> = vec![
            "new-pane".to_owned(),
            "--direction".to_owned(),
            "right".to_owned(),
            "--tab-id".to_owned(),
            tab_id.to_string(),
            "--name".to_owned(),
            SIDEBAR_PANE_NAME.to_owned(),
            "--close-on-exit".to_owned(),
            "--cwd".to_owned(),
            opts.cwd.to_string_lossy().into_owned(),
            "--".to_owned(),
            opts.rimz_bin.to_string_lossy().into_owned(),
            "sidebar".to_owned(),
            "serve".to_owned(),
            "--mux".to_owned(),
            "zellij".to_owned(),
            "--workspace-id".to_owned(),
            opts.workspace_id.as_str().to_owned(),
            "--session-name".to_owned(),
            opts.session_name.clone(),
        ];
        let output = self.zellij_action(&opts.session_name).args(args).run()?;
        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if pane_id.is_empty() {
            return Err(MuxErr::Output {
                program: "zellij".to_owned(),
                reason: "new-pane returned no pane id".to_owned(),
            });
        }
        Ok(pane_id)
    }

    /// Shrink a freshly-split sidebar (born at ~50%) toward the layout's width
    /// percentage, landing on the width *closest* to the target. The resize step
    /// is coarse, so the target usually falls between two reachable widths;
    /// stopping at the first width at or below it can overshoot, so when the
    /// prior (above-target) width was closer we step back up one. Bounded and
    /// best-effort: it stops at the target, when a step makes no progress (hit a
    /// minimum), or after [`RESIZE_MAX_STEPS`] — never a dead loop. Width is
    /// cosmetic, so any failure just leaves the wider pane.
    fn resize_sidebar_toward(&self, session: &str, tab_id: u64, pane_id: &str, width_percent: u16) {
        const RESIZE_MAX_STEPS: u32 = 16;
        let Some(target_raw) = parse_terminal_id(pane_id) else {
            return;
        };
        let mut last_cols = u64::MAX;
        for _ in 0..RESIZE_MAX_STEPS {
            let Some((cols, total)) = self.sidebar_and_tab_cols(session, tab_id, target_raw) else {
                return;
            };
            if total == 0 {
                return;
            }
            let target = (total * u64::from(width_percent.clamp(10, 90)) / 100).max(1);
            if cols <= target {
                // Reached/overshot the target. If the previous, above-target
                // width was closer than this one, the last decrease overshot —
                // step back.
                if last_cols != u64::MAX
                    && last_cols.saturating_sub(target) < target.saturating_sub(cols)
                {
                    let _ = self.resize_sidebar_step(session, pane_id, "increase");
                }
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

    fn resize_sidebar_step(&self, session: &str, pane_id: &str, direction: &str) -> Result<()> {
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

    /// Current column width of `target_raw` and the total columns of its tab
    /// (the sum across the tab's panes — exact for the sidebar-plus-terminal row
    /// the recovery produces). `None` when the pane has vanished or carries no
    /// geometry.
    fn sidebar_and_tab_cols(
        &self,
        session: &str,
        tab_id: u64,
        target_raw: u64,
    ) -> Option<(u64, u64)> {
        let panes = self.list_panes_with_session(Some(session)).ok()?;
        let mut total = 0;
        let mut current = None;
        for pane in panes
            .iter()
            .filter(|pane| pane.is_terminal() && pane.tab_id == tab_id)
        {
            let cols = pane.pane_columns?;
            total += cols;
            if pane.id == target_raw {
                current = Some(cols);
            }
        }
        Some((current?, total))
    }

    /// Block until Zellij has materialized the layout's sidebar pane alongside a
    /// second live terminal, so the caller's temp layout file stays on disk
    /// until Zellij has demonstrably parsed it. Returns `true` once that signal
    /// appears, `false` if the [`SIDEBAR_LAYOUT_TIMEOUT`] ceiling elapses first.
    ///
    /// The predicate gates on *our* `rimz-sidebar` pane (a default/fallback
    /// birth carries none) counted with the same `is_live_terminal` filter
    /// `list_panes` applies, so "materialized" here provably implies the
    /// caller's next `list_panes` returns the two panes — no held/exited pane
    /// slips the gate.
    fn wait_for_sidebar_layout(&self, session_name: &str) -> bool {
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

    /// Whether `session` already holds a tab named `tab_name`. `query-tab-names`
    /// prints one tab name per line; a Rimz background view is idempotent on its
    /// name, so a relaunch into a session that already carries it is skipped.
    fn session_has_named_tab(&self, session: &str, tab_name: &str) -> Result<bool> {
        let output = self.zellij_action(session).arg("query-tab-names").run()?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(strip_ansi)
            .any(|line| line.trim() == tab_name))
    }
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

    fn attach_command(&self, name: &str) -> CommandSpec {
        self.cmd().args(["attach", "--create", name])
    }

    fn detach(&self, _name: &str) -> Result<()> {
        self.cmd().args(["action", "detach"]).run().map(|_| ())
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        self.delete_session(name)
    }

    fn list_sessions(&self) -> Result<Vec<String>> {
        let output = self.cmd().arg("list-sessions").run()?;
        // Output lines look like `name [Created Ns ago]`; the bare name
        // appears as a leading whitespace-separated token. Strip ANSI escapes
        // defensively in case `list-sessions` colorizes its output.
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(strip_ansi)
            .map(|line| line.split_whitespace().next().unwrap_or(&line).to_owned())
            .filter(|line| !line.is_empty())
            .collect())
    }

    fn list_panes(&self, opts: PaneListOptions) -> Result<Vec<PaneRef>> {
        let raws = self.list_panes_with_session(opts.session_name.as_deref())?;
        let session_name = opts.session_name.unwrap_or_default();
        Ok(raws
            .into_iter()
            .filter(RawPane::is_live_terminal)
            .map(|mut p| PaneRef {
                pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", p.id)),
                session_name: session_name.clone(),
                view_id: Some(format!("tab_{}", p.tab_id)),
                view_kind: Some(ViewKind::Tab),
                // Zellij `list-panes` carries no per-pane tab name; the
                // remote-control classifier reads the full command line here
                // instead (which Zellij does report).
                view_name: None,
                is_focused: p.is_focused,
                pane_pid: p.pid(),
                pane_process_start: p.process_start(),
                command: p.take_command(),
                cwd: p.take_cwd(),
                // Zellij's `list-panes -j` exposes no per-pane "tab is active"
                // or "session attached" signal, so pane visibility is unknown
                // here. `None` makes the renderer's visibility gate fall back
                // to always painting — the deliberate cross-backend floor.
                view_active: None,
                session_attached: None,
            })
            .collect())
    }

    fn split_pane(&self, opts: SplitPaneOptions) -> Result<()> {
        let mut spec = self.cmd().args(["action", "new-pane"]);
        if let Some(target) = opts.target_pane_id {
            ensure_pane_backend(&target, MuxName::Zellij)?;
            // Zellij's CLI opens relative to the current focus and does not
            // expose a target-pane flag for `new-pane`.
        }
        if let Some(cwd) = opts.cwd {
            spec = spec.args(["--cwd".to_owned(), cwd]);
        }
        if let Some(command) = opts.command
            && let Some((program, args)) = command.split_first()
        {
            spec = spec
                .args(["--".to_owned(), program.clone()])
                .args(args.iter().cloned());
        }
        spec.run().map(|_| ())
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

    fn open_sidebar(&self, opts: &SidebarPaneOptions) -> Result<()> {
        // Zellij places a left pane only at session birth, so the sidebar is
        // injected only by (re)creating the session from a layout:
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
            SessionState::Absent => self.create_session_with_sidebar(opts),
            SessionState::Exited => {
                self.delete_session(&opts.session_name)?;
                self.create_session_with_sidebar(opts)
            }
            SessionState::Live
                if self.session_has_healthy_sidebar(&opts.session_name)
                    && !opts.replace_existing =>
            {
                Ok(())
            }
            SessionState::Live => {
                self.delete_session(&opts.session_name)?;
                self.create_session_with_sidebar(opts)
            }
        }
    }

    fn recover_sidebars(&self, opts: &SidebarPaneOptions) -> Result<SidebarRecovery> {
        // Zellij docks the sidebar left only at session birth, but a left pane
        // can still be reached in a live session: split a new pane to the right,
        // move it left, and resize it to the layout width. This never rebirths
        // the session, so the user's working panes survive.
        let panes = self.list_panes_with_session(Some(&opts.session_name))?;
        let classified: Vec<(String, bool)> = panes
            .iter()
            .filter(|pane| pane.is_terminal())
            .map(|pane| (pane.tab_id.to_string(), is_sidebar_pane(pane)))
            .collect();
        let missing = super::views_missing_sidebar(&classified);
        if missing.is_empty() {
            return Ok(SidebarRecovery::default());
        }

        // The new pane steals focus, so remember each tab's focused (working)
        // pane to restore afterwards, and the user's own invoking pane to return
        // the visible tab to where they ran `rimz reload`.
        let focused_in_tab: std::collections::HashMap<u64, u64> = panes
            .iter()
            .filter(|pane| pane.is_focused && !pane.is_plugin)
            .map(|pane| (pane.tab_id, pane.id))
            .collect();

        let mut report = SidebarRecovery::default();
        for tab in &missing {
            let Ok(tab_id) = tab.parse::<u64>() else {
                report.failed += 1;
                continue;
            };
            match self.add_sidebar_to_tab(opts, tab_id) {
                Ok(()) => {
                    report.recovered += 1;
                    if let Some(work) = focused_in_tab.get(&tab_id) {
                        let _ = self.focus_terminal(&opts.session_name, *work);
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        session = %opts.session_name,
                        tab = tab_id,
                        error = %err,
                        "sidebar recovery: in-place add failed; leaving the tab without a sidebar",
                    );
                    report.failed += 1;
                }
            }
        }
        if let Some(own) = own_zellij_pane_id() {
            let _ = self.focus_terminal(&opts.session_name, own);
        }
        Ok(report)
    }

    fn open_background_view(&self, opts: &BackgroundViewOptions) -> Result<BackgroundViewLaunch> {
        let session = &opts.sidebar.session_name;
        // Idempotent on the tab name: a relaunch into a session already carrying
        // the view launches nothing. A failed query propagates rather than risk a
        // duplicate launch.
        let outcome = if self.session_has_named_tab(session, &opts.name)? {
            BackgroundViewLaunch::AlreadyRunning
        } else {
            // `--layout` gives the tab its `sidebar | host` shape directly,
            // bypassing the session's default tab template (which `--layout`
            // overrides, so the sidebar must be spelled out here). `new-tab` is
            // synchronous (it prints the new tab id), so the temp layout file
            // can drop as soon as the call returns. Each pane carries its own
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
            BackgroundViewLaunch::Launched
        };
        // Return focus to the session's first tab — the working tab — so neither
        // a fresh launch (Zellij `new-tab` focuses the tab it creates) nor a
        // relaunch (the user may be sitting on the host tab) strands the user on
        // the host: the imminent `attach` lands on the shell. Unconditional on
        // purpose: unlike `recover_sidebars` (run from inside the managed session
        // via `rimz reload`), `rimz start` may run from a plain shell or a
        // *different* mux session, so the caller's own pane is not a focus target
        // here — the working tab always is. Best-effort: a focus hiccup never
        // sinks an otherwise-launched view.
        if let Err(err) = self.go_to_tab(session, 1) {
            tracing::warn!(
                session = %session,
                error = %err,
                "could not return focus to the working tab after opening the background view",
            );
        }
        Ok(outcome)
    }

    fn wake_sidebar(&self, session_name: &str, bytes: &[u8]) -> Result<()> {
        // Per-instance socket fanout is the channel of record. The broadcast
        // `zellij pipe` here is a latency optimization on top. Program (and any
        // `RIMZ_ZELLIJ_BIN` test-shim override) and runtime dir both come from
        // `self.cmd()`, the single chokepoint.
        let payload = String::from_utf8_lossy(bytes).to_string();
        self.cmd()
            .args([
                "--session",
                session_name,
                "pipe",
                "--name",
                "rimz::feed",
                "--",
                &payload,
            ])
            .run()
            .map(|_| ())
    }

    fn version(&self) -> Result<String> {
        let spec = self.cmd().arg("--version");
        let output = spec.to_command().output().map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => MuxErr::NotInstalled {
                program: spec.program.clone(),
            },
            _ => MuxErr::Io(err),
        })?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

/// Whether `list-panes` stdout is the transient empty race rather than a real
/// answer. Zellij spells "zero panes" as `[]`, so empty (or whitespace-only)
/// output means the action client raced the session server and is worth a
/// retry — not an EOF parse error.
fn is_transient_empty(stdout: &[u8]) -> bool {
    stdout.iter().all(u8::is_ascii_whitespace)
}

/// Liveness of a Zellij session, as reported by `zellij list-sessions`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionState {
    /// No session by that name.
    Absent,
    /// Running and attachable.
    Live,
    /// Present but exited — `attach` would resurrect a stale serialized layout.
    Exited,
}

/// Parse one `list-sessions` line for `name`. Lines look like
/// `name [Created 6m ago]` (live) or
/// `name [Created 6m ago] (EXITED - attach to resurrect)`. `strip_ansi` guards
/// against a colorized line even though `--no-formatting` should preclude one.
fn session_state_from_line(line: &str, name: &str) -> Option<SessionState> {
    let clean = strip_ansi(line);
    if clean.split_whitespace().next()? != name {
        return None;
    }
    Some(if clean.contains("EXITED") {
        SessionState::Exited
    } else {
        SessionState::Live
    })
}

/// A live, non-plugin sidebar pane is one Zellij still titles with the layout's
/// [`SIDEBAR_PANE_NAME`] — the same signal `session_has_healthy_sidebar` trusts.
fn is_sidebar_pane(pane: &RawPane) -> bool {
    !pane.is_plugin && pane.title.as_deref() == Some(SIDEBAR_PANE_NAME)
}

/// `ZELLIJ_PANE_ID` is the bare integer of the pane the caller runs in. `rimz
/// reload` runs in the user's pane, so refocusing it restores their visible tab.
fn own_zellij_pane_id() -> Option<u64> {
    env::var("ZELLIJ_PANE_ID").ok()?.trim().parse().ok()
}

fn parse_terminal_id(pane_id: &str) -> Option<u64> {
    pane_id.strip_prefix("terminal_")?.parse().ok()
}

/// Subset of fields `zellij action list-panes -j -a` emits. We deserialize
/// only what we route into `PaneRef`; serde silently ignores everything else.
#[derive(Debug, Deserialize)]
struct RawPane {
    id: u64,
    is_plugin: bool,
    #[serde(default)]
    is_held: bool,
    /// Command has exited but Zellij still shows the pane (e.g. hold-on-close).
    /// A dead pane, not a live process — excluded from the pane listing.
    #[serde(default)]
    exited: bool,
    #[serde(default)]
    is_suppressed: bool,
    #[serde(default)]
    is_focused: bool,
    tab_id: u64,
    /// Column width of the pane, used by in-place sidebar recovery to resize a
    /// freshly-split sidebar toward the layout's width percentage.
    #[serde(default)]
    pane_columns: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    pane_command: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    pane_cwd: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    pane_pid: Option<u32>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    pane_process_start: Option<Value>,
    #[serde(default)]
    process_start: Option<Value>,
}

impl RawPane {
    /// A real terminal pane: not a plugin and not suppressed (floating/hidden).
    /// The single definition of "counts as a pane" shared by the pane listing,
    /// sidebar recovery, and column math.
    fn is_terminal(&self) -> bool {
        !self.is_plugin && !self.is_suppressed
    }

    /// A terminal pane hosting a live command. Excludes held/exited corpses so a
    /// dead command never renders a row, and so that — since a live pane always
    /// reports a command — a missing command reads unambiguously as a raced
    /// (degraded) `list-panes` answer the caller can hold the last good list
    /// against rather than flashing an anonymous process row.
    fn is_live_terminal(&self) -> bool {
        self.is_terminal() && !self.is_held && !self.exited
    }

    /// Move the command out of the owned `RawPane` (consumed once, during
    /// `list_panes`) rather than cloning it — `pane_command` wins, falling back
    /// to `command`.
    fn take_command(&mut self) -> Option<String> {
        self.pane_command
            .take()
            .or_else(|| self.command.take())
            .filter(|value| !value.is_empty())
    }

    /// Move the cwd out of the owned `RawPane`; `pane_cwd` wins, falling back
    /// to `cwd`.
    fn take_cwd(&mut self) -> Option<String> {
        self.pane_cwd
            .take()
            .or_else(|| self.cwd.take())
            .filter(|value| !value.is_empty())
    }

    fn pid(&self) -> Option<u32> {
        self.pane_pid.or(self.pid)
    }

    fn process_start(&self) -> Option<Timestamp> {
        self.pane_process_start
            .as_ref()
            .or(self.process_start.as_ref())
            .and_then(timestamp_from_json)
    }
}

fn timestamp_from_json(value: &Value) -> Option<Timestamp> {
    if let Some(seconds) = value.as_i64() {
        return Timestamp::from_second(seconds).ok();
    }
    if let Some(raw) = value.as_str() {
        if let Ok(seconds) = raw.parse::<i64>() {
            return Timestamp::from_second(seconds).ok();
        }
        return raw.parse::<Timestamp>().ok();
    }
    None
}

struct TempLayoutFile {
    path: PathBuf,
}

impl TempLayoutFile {
    fn new(contents: String) -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "rimz-zellij-layout-{}-{}.kdl",
            std::process::id(),
            uuid::Uuid::now_v7().simple(),
        ));
        std::fs::write(&path, contents)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempLayoutFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn render_sidebar_layout(opts: &SidebarPaneOptions) -> Result<String> {
    let rimz_bin = kdl_string(&opts.rimz_bin.to_string_lossy())?;
    let workspace_id = kdl_string(opts.workspace_id.as_str())?;
    let session_name = kdl_string(&opts.session_name)?;
    let size = kdl_string(&format!("{}%", opts.width_percent.clamp(10, 90)))?;
    let pane_name = kdl_string(SIDEBAR_PANE_NAME)?;
    // The whole layout is the `default_tab_template`, so every tab — the first
    // one born with the session and any the user opens later — is identical:
    // the sidebar on the left and a focused terminal on the right. The working
    // cwd comes from the session's `--default-cwd`, so panes need no `cwd`.
    //
    // The terminal is an explicit `pane focus=true`, not Zellij's `children`
    // placeholder. A nested `children` template has version-sensitive behavior:
    // on Zellij 0.44.3 it creates the right terminal but leaves focus stranded
    // on the sidebar in newly-created tabs. Spelling out the terminal makes the
    // product contract explicit and pins focus on the user's working pane.
    //
    // Supplying a `default_tab_template` replaces Zellij's built-in one, which
    // is what carries the bottom bar — so the template re-adds the compact-bar
    // plugin itself, or tabs are born with no tab/status bar at all. The body
    // (sidebar + terminal) is a nested vertical split above that one-row bar.
    // The `plugin` pane must stay multi-line: Zellij's KDL parser rejects the
    // single-line `pane { plugin ... }` form.
    Ok(format!(
        r#"layout {{
    default_tab_template {{
        pane split_direction="vertical" {{
            pane size={size} name={pane_name} {{
                command {rimz_bin}
                args "sidebar" "serve" "--mux" "zellij" "--workspace-id" {workspace_id} "--session-name" {session_name}
                start_suspended false
                close_on_exit true
            }}
            pane focus=true
        }}
        pane size=1 borderless=true {{
            plugin location="zellij:compact-bar"
        }}
    }}
}}
"#,
    ))
}

fn kdl_string(value: &str) -> Result<String> {
    serde_json::to_string(value).map_err(|err| MuxErr::Output {
        program: "zellij".to_owned(),
        reason: format!("escaping layout string: {err}"),
    })
}

/// A tab layout born `sidebar | host`: the global sidebar docked on the left,
/// the view's host command on the focused right pane, and the compact-bar below
/// — mirroring the session's working-tab template ([`render_sidebar_layout`]).
/// Supplying this as `new-tab --layout` overrides that template, so the sidebar
/// is spelled out here rather than inherited. The sidebar runs from its own
/// worktree cwd and the host from `host_cwd` (the project root), so each pane
/// carries an explicit `cwd`. The host closes with its process
/// (`close_on_exit true`): an exit means the host is gone.
fn render_background_view_layout(opts: &BackgroundViewOptions) -> Result<String> {
    let (host_program, host_args) = opts.host.split_first().ok_or_else(|| MuxErr::Output {
        program: "zellij".to_owned(),
        reason: "background view has no host command".to_owned(),
    })?;
    let sidebar = &opts.sidebar;
    let rimz_bin = kdl_string(&sidebar.rimz_bin.to_string_lossy())?;
    let workspace_id = kdl_string(sidebar.workspace_id.as_str())?;
    let session_name = kdl_string(&sidebar.session_name)?;
    let size = kdl_string(&format!("{}%", sidebar.width_percent.clamp(10, 90)))?;
    let pane_name = kdl_string(SIDEBAR_PANE_NAME)?;
    let sidebar_cwd = kdl_string(&sidebar.cwd.to_string_lossy())?;
    let host_cwd = kdl_string(&opts.host_cwd.to_string_lossy())?;
    let host_program = kdl_string(host_program)?;
    let host_args_line = if host_args.is_empty() {
        String::new()
    } else {
        let rendered = host_args
            .iter()
            .map(|arg| kdl_string(arg))
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        format!("\n            args {rendered}")
    };
    // The body (sidebar + host) is a nested vertical split above a one-row
    // compact-bar. As in `render_sidebar_layout`, supplying our own layout drops
    // Zellij's built-in bar, so the template re-adds it or the tab is born with
    // no tab/status bar at all. The `plugin` pane must stay multi-line: Zellij's
    // KDL parser rejects the single-line `pane { plugin ... }` form.
    Ok(format!(
        r#"layout {{
    pane split_direction="vertical" {{
        pane size={size} name={pane_name} cwd={sidebar_cwd} {{
            command {rimz_bin}
            args "sidebar" "serve" "--mux" "zellij" "--workspace-id" {workspace_id} "--session-name" {session_name}
            start_suspended false
            close_on_exit true
        }}
        pane focus=true cwd={host_cwd} {{
            command {host_program}{host_args_line}
            start_suspended false
            close_on_exit true
        }}
    }}
    pane size=1 borderless=true {{
        plugin location="zellij:compact-bar"
    }}
}}
"#,
    ))
}

/// Defensive ANSI strip for `list-sessions` output. Zellij ships a colored
/// banner in newer versions; the parser only cares about the bare name.
///
/// Handles the CSI subset (`ESC [ params final`) Zellij emits. The
/// introducer `[` lives at 0x5b which overlaps the final-byte range
/// (0x40..=0x7e), so we must consume the introducer first and only then
/// scan for the final byte.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some('[') = chars.next() {
                for ch in chars.by_ref() {
                    if matches!(ch, '\x40'..='\x7e') {
                        break;
                    }
                }
            }
            // Non-CSI escape (single byte after ESC) or end-of-string: nothing
            // to skip; the next iteration resumes the scan.
        } else {
            out.push(c);
        }
    }
    out
}

fn trim_capture(raw_text: String, max_lines: Option<u16>) -> (String, Vec<String>) {
    let mut lines: Vec<String> = raw_text.lines().map(str::to_owned).collect();
    if let Some(max_lines) = max_lines {
        let keep = max_lines as usize;
        if keep == 0 {
            lines.clear();
        } else if lines.len() > keep {
            lines = lines.split_off(lines.len() - keep);
        }
    }

    let mut trimmed = lines.join("\n");
    if raw_text.ends_with('\n') && !trimmed.is_empty() {
        trimmed.push('\n');
    }
    (trimmed, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parser_accepts_three_dot_form() {
        assert_eq!(parse_version("zellij 0.41.2"), Some((0, 41, 2)));
        assert_eq!(parse_version("  zellij 1.2.3  \n"), Some((1, 2, 3)));
        assert_eq!(parse_version("zellij 0.44"), Some((0, 44, 0)));
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn min_version_threshold_holds() {
        assert!((0, 41, 0) >= MIN_ZELLIJ_VERSION);
        assert!((0, 44, 3) >= MIN_ZELLIJ_VERSION);
        assert!((0, 40, 9) < MIN_ZELLIJ_VERSION);
    }

    #[test]
    fn raw_pane_deserializes_minimal_shape() {
        let json = r#"[
          {"id": 0, "is_plugin": false, "is_suppressed": false, "is_focused": true, "tab_id": 0},
          {"id": 2, "is_plugin": true,  "is_suppressed": false, "tab_id": 0}
        ]"#;
        let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(!parsed[0].is_plugin);
        assert!(parsed[0].is_focused);
        assert!(parsed[1].is_plugin);
        assert!(!parsed[1].is_focused);
    }

    #[test]
    fn live_terminal_excludes_plugin_suppressed_and_dead_panes() {
        let json = r#"[
          {"id": 0, "is_plugin": false, "is_suppressed": false, "tab_id": 0},
          {"id": 1, "is_plugin": true,  "is_suppressed": false, "tab_id": 0},
          {"id": 2, "is_plugin": false, "is_suppressed": true,  "tab_id": 0},
          {"id": 3, "is_plugin": false, "is_suppressed": false, "is_held": true, "tab_id": 0},
          {"id": 4, "is_plugin": false, "is_suppressed": false, "exited": true, "tab_id": 0}
        ]"#;
        let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
        let live: Vec<u64> = parsed
            .iter()
            .filter(|p| p.is_live_terminal())
            .map(|p| p.id)
            .collect();
        // Only the plain live terminal pane survives; plugin, suppressed, held,
        // and exited panes are all dropped.
        assert_eq!(live, vec![0]);
    }

    #[test]
    fn transient_empty_detects_blank_list_panes_output() {
        assert!(is_transient_empty(b""));
        assert!(is_transient_empty(b"  \n\t"));
        // A real, parseable answer — even an empty pane set — is not transient.
        assert!(!is_transient_empty(b"[]"));
        assert!(!is_transient_empty(b"[{\"id\":0}]"));
    }

    #[test]
    fn ansi_strip_drops_color_codes() {
        let stripped = strip_ansi("\x1b[32mfoo\x1b[0m bar");
        assert_eq!(stripped, "foo bar");
    }

    #[test]
    fn capture_trim_keeps_last_requested_lines() {
        let (raw, lines) = trim_capture("a\nb\nc\nd\n".to_owned(), Some(2));
        assert_eq!(lines, vec!["c", "d"]);
        assert_eq!(raw, "c\nd\n");
    }

    #[test]
    fn session_state_classifies_list_sessions_lines() {
        assert_eq!(
            session_state_from_line("rimz-query-engine [Created 6m ago]", "rimz-query-engine"),
            Some(SessionState::Live),
        );
        assert_eq!(
            session_state_from_line(
                "rimz-query-engine [Created 6m ago] (EXITED - attach to resurrect)",
                "rimz-query-engine",
            ),
            Some(SessionState::Exited),
        );
        // A colorized line (no `--no-formatting`) still parses via `strip_ansi`.
        assert_eq!(
            session_state_from_line(
                "\x1b[32;1mrimz-query-engine\x1b[m [Created ago] (\x1b[31;1mEXITED\x1b[m - resurrect)",
                "rimz-query-engine",
            ),
            Some(SessionState::Exited),
        );
        // A different session's line is not a match.
        assert_eq!(
            session_state_from_line("other [Created 6m ago]", "rimz-query-engine"),
            None,
        );
    }

    #[test]
    fn sidebar_layout_carries_a_bottom_bar() {
        use crate::ids::WorkspaceId;
        let opts = SidebarPaneOptions {
            session_name: "rimz-bar".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bar")),
            cwd: PathBuf::from("/tmp/rimz-bar"),
            width_percent: 30,
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
        };
        let layout = render_sidebar_layout(&opts).expect("render layout");
        assert!(
            layout.contains("compact-bar"),
            "the sidebar layout overrides Zellij's default tab template, so it must \
             re-add a bottom bar plugin or the tab/status bar vanishes:\n{layout}",
        );
    }

    #[test]
    fn sidebar_layout_focuses_an_explicit_terminal_in_every_tab() {
        use crate::ids::WorkspaceId;
        let opts = SidebarPaneOptions {
            session_name: "rimz-focus".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-focus")),
            cwd: PathBuf::from("/tmp/rimz-focus"),
            width_percent: 30,
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
        };
        let layout = render_sidebar_layout(&opts).expect("render layout");
        // The template must spell out the focused terminal instead of relying
        // on a nested `children` placeholder: every template-born tab needs a
        // right pane with focus, never a bare or focused sidebar.
        assert!(
            layout.contains("pane focus=true"),
            "the layout must focus an explicit terminal pane:\n{layout}",
        );
        assert!(
            !layout.contains("children"),
            "the layout must not depend on `children`: placeholder semantics \
             can misplace focus or omit the right terminal in template-born tabs:\n{layout}",
        );
        // One self-contained template, no separate `tab` node, so the initial
        // tab and every later one are born identically.
        assert!(
            !layout.contains("tab "),
            "every tab comes from the template; no explicit `tab` node:\n{layout}",
        );
    }

    fn background_view_opts(host: Vec<String>) -> BackgroundViewOptions {
        use crate::ids::WorkspaceId;
        BackgroundViewOptions {
            name: "rimz-rc".to_owned(),
            host,
            host_cwd: PathBuf::from("/proj/root"),
            sidebar: SidebarPaneOptions {
                session_name: "rimz-bg".to_owned(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/proj/root")),
                cwd: PathBuf::from("/proj/worktree"),
                width_percent: 30,
                rimz_bin: PathBuf::from("/usr/bin/rimz"),
                replace_existing: false,
            },
        }
    }

    #[test]
    fn background_view_layout_runs_the_host_beside_the_sidebar() {
        let layout = render_background_view_layout(&background_view_opts(vec![
            "claude".to_owned(),
            "remote-control".to_owned(),
            "--spawn".to_owned(),
            "worktree".to_owned(),
        ]))
        .expect("render background view layout");
        // The host is the focused right pane, born unsuspended, and closes with
        // its process — an exit means the host is gone.
        assert!(layout.contains(r#"command "claude""#), "{layout}");
        assert!(
            layout.contains(r#"args "remote-control" "--spawn" "worktree""#),
            "{layout}",
        );
        assert!(layout.contains("pane focus=true"), "{layout}");
        assert!(layout.contains("start_suspended false"), "{layout}");
        assert!(layout.contains("close_on_exit true"), "{layout}");
        // The global sidebar is docked on the left, running the renderer.
        assert!(layout.contains(r#"name="rimz-sidebar""#), "{layout}");
        assert!(layout.contains(r#""sidebar" "serve""#), "{layout}");
        // A bottom bar, mirroring the working-tab template.
        assert!(layout.contains("compact-bar"), "{layout}");
        // Each pane carries its own cwd: the sidebar from the worktree, the host
        // from the project root.
        assert!(layout.contains(r#"cwd="/proj/worktree""#), "{layout}");
        assert!(layout.contains(r#"cwd="/proj/root""#), "{layout}");
    }

    #[test]
    fn background_view_layout_rejects_an_empty_host() {
        assert!(render_background_view_layout(&background_view_opts(vec![])).is_err());
    }

    #[test]
    fn sidebar_layout_starts_the_sidebar_without_a_run_prompt() {
        use crate::ids::WorkspaceId;
        let opts = SidebarPaneOptions {
            session_name: "rimz-run".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
            cwd: PathBuf::from("/tmp/rimz-run"),
            width_percent: 30,
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
        };
        let layout = render_sidebar_layout(&opts).expect("render layout");
        assert!(
            layout.contains("start_suspended false"),
            "Zellij command panes default to a run prompt unless the layout \
             starts them explicitly:\n{layout}",
        );
        assert!(
            !layout.contains("start_suspended true"),
            "the sidebar pane must never be born suspended:\n{layout}",
        );
    }
}
