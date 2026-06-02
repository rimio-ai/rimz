//! Runtime loop for the native sidebar process.

use std::collections::HashSet;
use std::io;
use std::io::Read;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal;
use rimz::ids::PaneId;
use rimz::ledger::paths::PathErr;
use rimz::mux::PaneListOptions;
use rimz::{MuxName, RuntimePaths, SidebarInstanceId, SidebarSnapshot, WorkspaceId};
use tracing::{debug, info, warn};

use crate::render::{self, Alert, UiState};

mod input;
use input::{
    KeyAction, SELF_CLOSE_WAKEUP, SNAPSHOT_WAKEUP, Wakeup, encode_key, encode_mouse,
    wait_for_wakeup,
};

#[derive(Clone, Debug)]
pub struct ServeConfig {
    pub workspace_id: WorkspaceId,
    pub mux: MuxName,
    pub session_name: String,
    pub instance_id: SidebarInstanceId,
    pub tick_seconds: u64,
    pub rimz_bin: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum SidebarAppErr {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Paths(#[from] PathErr),
    #[error("running `{program}`: {source}")]
    CommandIo {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("snapshot command failed: {stderr}")]
    SnapshotCommand { stderr: String },
    #[error("heartbeat write failed: {0}")]
    Heartbeat(String),
}

pub type Result<T> = std::result::Result<T, SidebarAppErr>;

pub fn serve(config: ServeConfig) -> Result<()> {
    let runtime = RuntimePaths::for_workspace(config.workspace_id.clone())?;
    runtime.ensure_dirs()?;
    let socket_path = sidebar_socket_path(&runtime, &config.instance_id);
    let socket = bind_socket(&socket_path)?;
    let _socket_cleanup = RuntimeFileGuard {
        path: socket_path.clone(),
    };
    // Drop the heartbeat on exit too — including the self-close below. A
    // lingering heartbeat stays mtime-fresh for `SIDEBAR_HEARTBEAT_TTL`, during
    // which `rimz`'s freshness gate would skip relaunch and let a plain
    // `attach` rebirth the session with no sidebar.
    let _heartbeat_cleanup = RuntimeFileGuard {
        path: runtime.sidebar_heartbeat_path(&config.instance_id),
    };
    let tick = tick_for(config.tick_seconds);

    // Redraw the instant the pane is resized — most importantly when a user
    // attaches to a background session and Zellij sizes the pane for the first
    // time. The watcher nudges this loop through the same wakeup socket the
    // ledger uses, so a resize is just another wakeup; without it the first
    // usable frame waits for the next `tick`, reading as a blank sidebar.
    let _input_mode = InputModeGuard::enable()?;
    spawn_event_waker(socket_path.clone());
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut last_snapshot: Option<SidebarSnapshot> = None;
    let mut current = placeholder_snapshot(config.workspace_id.clone());
    let mut health = Health::default();
    let mut gate = GateState::default();
    let mut self_close = SelfCloseState::default();
    let mut session_exit = SessionExitState::default();
    let mut ui = UiState::default();
    let mut reexec_to: Option<PathBuf> = None;
    // Monotonic base for the animation frame. Deriving the phase from elapsed
    // wall-clock (rather than a per-tick counter) keeps the spin continuous
    // across re-fetches and ledger deltas, so no redraw path can stall it.
    let anim_start = Instant::now();

    // The snapshot subprocess (workspace resolve + `list-panes` + git) and the
    // heartbeat write run on a background worker, so animation and input never
    // block on them. The worker posts `SNAPSHOT_WAKEUP` when a result is ready;
    // `in_flight`/`pending_refetch` coalesce requests so a ledger-delta storm or
    // a slow fetch can never queue more than one extra run.
    let (request_tx, request_rx) = std::sync::mpsc::channel::<FetchRequest>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<FetchOutcome>();
    spawn_fetch_worker(
        config.clone(),
        runtime.clone(),
        socket_path.clone(),
        request_rx,
        result_tx,
    );
    let mut in_flight = false;
    let mut pending_refetch: Option<FetchRequest> = None;

    // Resize is the earliest signal that a sibling pane closed and Zellij/tmux
    // gave the sidebar the freed space. Probe only the live pane list on a
    // second worker: it feeds the existing self-close latch without running the
    // snapshot path's ledger/git/account enrichments.
    let (self_close_probe_tx, self_close_probe_rx) =
        std::sync::mpsc::channel::<SelfCloseProbeRequest>();
    let (self_close_result_tx, self_close_result_rx) =
        std::sync::mpsc::channel::<SelfCloseProbeOutcome>();
    spawn_self_close_probe_worker(
        config.clone(),
        socket_path.clone(),
        self_close_probe_rx,
        self_close_result_tx,
    );
    let mut self_close_probe_in_flight = false;
    let mut pending_self_close_probe: Option<Duration> = None;

    // First frame synchronously: nothing animates yet, so there is no loop to
    // stall, and it avoids a placeholder flash before the worker's first result.
    let mut fetched_at = Instant::now();
    // First frame: `prev_good` is the empty placeholder, so the gate always
    // accepts and there is no loop yet to fire a self-heal refetch — ignore
    // `rejected` here.
    let mut should_exit = apply_fetch_outcome(
        &config,
        run_fetch(
            &config,
            &runtime,
            &socket_path,
            FetchRequest::default(),
            true,
        ),
        &mut last_snapshot,
        &mut current,
        &mut health,
        &mut gate,
        &mut self_close,
        &mut session_exit,
        &mut ui,
        &mut terminal,
        anim_start,
    )?
    .should_exit;
    if !should_exit && current.own_view.as_ref().map(|view| view.sibling_count) == Some(0) {
        request_self_close_probe(
            &self_close_probe_tx,
            &mut self_close_probe_in_flight,
            &mut pending_self_close_probe,
            STARTUP_SELF_CLOSE_RECHECK_AFTER,
        );
    }

    // One event loop. It blocks only in `recv`; the spinner advances on the
    // animation tick, input is applied in place, and a finished background fetch
    // arrives as `Wakeup::Snapshot` to be folded in — so no path forks a
    // subprocess on the render thread, and a busy fetch never freezes the spin
    // or swallows a keypress.
    while !should_exit {
        let animating = render::has_live_animation(&current);
        let timeout = if animating {
            ANIMATION_FRAME.min(tick)
        } else {
            tick
        };
        socket.set_read_timeout(Some(timeout))?;
        match wait_for_wakeup(&socket)? {
            // A background fetch finished. Take the most recent result (drop any
            // older queued ones), fold it, then fire the deferred refetch a
            // ledger delta asked for while this one was in flight.
            Wakeup::Snapshot => {
                in_flight = false;
                let mut latest = None;
                while let Ok(outcome) = result_rx.try_recv() {
                    latest = Some(outcome);
                }
                let mut rejected = false;
                if let Some(outcome) = latest {
                    fetched_at = Instant::now();
                    let applied = apply_fetch_outcome(
                        &config,
                        outcome,
                        &mut last_snapshot,
                        &mut current,
                        &mut health,
                        &mut gate,
                        &mut self_close,
                        &mut session_exit,
                        &mut ui,
                        &mut terminal,
                        anim_start,
                    )?;
                    should_exit = applied.should_exit;
                    rejected = applied.rejected;
                }
                if !should_exit && let Some(request) = pending_refetch.take() {
                    request_fetch(
                        &request_tx,
                        &mut in_flight,
                        &mut pending_refetch,
                        request,
                        false,
                    );
                }
                // A held transient regression: ask for one more read so the
                // last-known-good cache heals to the next good frame. Single-
                // flight bounds this to one extra run; once the escape hatch
                // opens, the fetch is accepted and `rejected` clears, so this
                // never spins.
                if !should_exit && rejected {
                    request_fetch(
                        &request_tx,
                        &mut in_flight,
                        &mut pending_refetch,
                        FetchRequest::default(),
                        false,
                    );
                }
            }
            Wakeup::SelfCloseProbe => {
                self_close_probe_in_flight = false;
                while let Ok(outcome) = self_close_result_rx.try_recv() {
                    if apply_self_close_probe_outcome(&config, outcome, &mut self_close) {
                        should_exit = true;
                        break;
                    }
                }
                if !should_exit && let Some(delay) = pending_self_close_probe.take() {
                    request_self_close_probe(
                        &self_close_probe_tx,
                        &mut self_close_probe_in_flight,
                        &mut pending_self_close_probe,
                        delay,
                    );
                }
            }
            // The poll timeout drives two decoupled layers. Render: while a row
            // animates (a running agent, a resolver, or an active process),
            // advance the spin frame on the cached snapshot — pure in-process
            // redraw, never gated on fetch state, so the spin stays smooth at
            // `ANIMATION_FRAME` regardless of fetch latency. Data: a latency-tolerant backstop refetch, fired only when
            // nothing has refreshed data for a full `tick`. Ledger deltas (which
            // include the statusline `$`/token push) are the primary data
            // channel; this backstop only catches pane/git drift that fires no
            // delta. `request_fetch` is a no-op while a fetch is in flight, so the
            // backstop can neither double-fire nor stall.
            Wakeup::Tick => {
                // Advance the spin whenever there is motion to show — a pure
                // in-process redraw on the cached snapshot, decoupled from fetch
                // state so it stays smooth at `ANIMATION_FRAME` regardless of
                // fetch latency.
                if animating {
                    ui.animation_phase = wall_clock_phase(anim_start);
                    render::draw_to_terminal_with_ui(
                        &mut terminal,
                        &current,
                        health.alert.as_ref(),
                        &mut ui,
                    )?;
                }
                if fetched_at.elapsed() >= tick {
                    request_fetch(
                        &request_tx,
                        &mut in_flight,
                        &mut pending_refetch,
                        FetchRequest::default(),
                        false,
                    );
                }
            }
            // A ledger delta means new committed data: refetch, forcing one more
            // run if a fetch is already in flight so the delta is never lost.
            // Pane-sensitive lifecycle deltas bypass the pane cache so a
            // just-started or just-ended agent is not pinned to the next TTL.
            // A burst of deltas collapses to a single fetch via `in_flight`.
            Wakeup::Ledger { fresh_panes } => {
                request_fetch(
                    &request_tx,
                    &mut in_flight,
                    &mut pending_refetch,
                    if fresh_panes {
                        FetchRequest::fresh_panes()
                    } else {
                        FetchRequest::default()
                    },
                    true,
                );
            }
            Wakeup::Resize => {
                apply_input(
                    Wakeup::Resize,
                    &mut ui,
                    &mut health,
                    &mut terminal,
                    &current,
                )?;
                request_self_close_probe(
                    &self_close_probe_tx,
                    &mut self_close_probe_in_flight,
                    &mut pending_self_close_probe,
                    Duration::ZERO,
                );
                // A resize is the mux telling us topology changed: a split
                // opened/closed, or the sidebar got space back. Pull a fresh
                // pane list immediately and require a cache produced after this
                // signal; otherwise a just-closed/just-opened agent can linger
                // until the pane-cache TTL or the next data tick.
                request_fetch(
                    &request_tx,
                    &mut in_flight,
                    &mut pending_refetch,
                    FetchRequest::fresh_panes(),
                    true,
                );
            }
            Wakeup::Reload => match reload_action() {
                ReloadAction::Reexec(target) => {
                    debug!(
                        session = %config.session_name,
                        target = %target.display(),
                        "reload: on-disk binary differs; re-execing the renderer in place",
                    );
                    reexec_to = Some(target);
                    break;
                }
                // The binary on disk is byte-identical to the one we run — a
                // reload that installed no new renderer, or a reproducible
                // rebuild. Skip the re-exec churn, but still honour the reload
                // intent with an immediate producing refetch so `r` always pulls
                // live data and un-sticks a tab whose producer stalled.
                ReloadAction::AlreadyCurrent => {
                    debug!(
                        session = %config.session_name,
                        "reload: binary unchanged; refetching in place without re-exec",
                    );
                    request_fetch(
                        &request_tx,
                        &mut in_flight,
                        &mut pending_refetch,
                        FetchRequest::fresh_panes(),
                        true,
                    );
                }
                // A reload that cannot find its replacement (a partial or
                // in-flight install) must never make the sidebar vanish — keep
                // serving the current build and refetch as above.
                ReloadAction::Missing => {
                    warn!(
                        session = %config.session_name,
                        "reload requested but no renderer binary is on disk; refetching in place",
                    );
                    request_fetch(
                        &request_tx,
                        &mut in_flight,
                        &mut pending_refetch,
                        FetchRequest::fresh_panes(),
                        true,
                    );
                }
            },
            wakeup => {
                apply_input(wakeup, &mut ui, &mut health, &mut terminal, &current)?;
            }
        }
    }
    if let Some(target) = reexec_to {
        // Restore the terminal and release this instance's runtime files before
        // replacing the process image — `exec` never returns, so their RAII
        // Drop would otherwise be skipped and leak a stale socket + heartbeat.
        drop(_input_mode);
        drop(_socket_cleanup);
        drop(_heartbeat_cleanup);
        return Err(reexec_self(&target));
    }
    Ok(())
}

/// Replace this process with a fresh invocation of `exe` and our own argv.
/// After `rimz reload`, the renderer's binary on disk has been updated in
/// place; re-execing the resolved path loads the new code without touching the
/// pane or session. Only returns on failure — success replaces the image.
fn reexec_self(exe: &Path) -> SidebarAppErr {
    use std::os::unix::process::CommandExt;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let source = Command::new(exe).args(&args).exec();
    SidebarAppErr::CommandIo {
        program: exe.display().to_string(),
        source,
    }
}

/// What a reload (`rimz reload` or the `r` keypress) does this tick: re-exec
/// onto a changed on-disk binary, skip the re-exec when it is byte-identical to
/// the running image, or keep the current build when nothing is on disk to load.
enum ReloadAction {
    /// The on-disk binary differs from the running image — load it in place.
    Reexec(PathBuf),
    /// The on-disk binary is byte-identical to the running image — skip the
    /// re-exec churn and refetch in place instead.
    AlreadyCurrent,
    /// No binary resolves on disk (a partial or in-flight install) — keep
    /// serving the current build and refetch.
    Missing,
}

/// Decide the reload action from the resolved target and whether it matches the
/// running image. Pure, so the branching is unit-tested directly. An unknown
/// match (the running image's bytes were unreadable) re-execs, preserving the
/// always-load-the-on-disk-build behavior.
fn decide_reload(target: Option<PathBuf>, running_matches: Option<bool>) -> ReloadAction {
    match (target, running_matches) {
        (None, _) => ReloadAction::Missing,
        (Some(_), Some(true)) => ReloadAction::AlreadyCurrent,
        (Some(target), Some(false) | None) => ReloadAction::Reexec(target),
    }
}

/// Resolve this reload into an action: find the binary to load, then compare it
/// to the running image so an unchanged build skips the re-exec.
fn reload_action() -> ReloadAction {
    let target = reexec_target();
    let running_matches = target.as_deref().and_then(running_image_matches);
    decide_reload(target, running_matches)
}

/// Resolve the on-disk binary to re-exec for a reload, or `None` when none can
/// be found — in which case the caller keeps serving the current build instead
/// of vanishing.
fn reexec_target() -> Option<PathBuf> {
    resolve_reexec_target(std::env::current_exe().ok()?)
}

/// Pick the live binary behind a `current_exe()` reading.
///
/// A fresh `cargo install` replaces our binary via atomic rename, which unlinks
/// the inode the running process still holds. The kernel then annotates
/// `/proc/self/exe` (what `current_exe()` reads) with a trailing " (deleted)",
/// so the raw path no longer resolves on disk. The replacement now lives at the
/// un-annotated path — exactly the build `rimz reload` means to pick up — so we
/// strip that marker and prefer whichever path is a real file. `None` (neither
/// path exists, e.g. a partial install) tells the caller to keep the old build.
fn resolve_reexec_target(exe: PathBuf) -> Option<PathBuf> {
    if exe.is_file() {
        return Some(exe);
    }
    strip_deleted_suffix(&exe).filter(|path| path.is_file())
}

/// Resolve the `rimz` binary that drives `sidebar snapshot` this tick.
///
/// `cached` is the path captured at launch — the sibling `rimz` beside this
/// renderer, or `RIMZ_BIN`. A long-lived sidebar can outlive it: removing the
/// dev worktree it was built in deletes that binary out from under the still
/// running renderer, and every snapshot fork then fails with ENOENT, degrading
/// the sidebar with no way back (a reload cannot rescue it either, since the
/// renderer binary in that worktree is gone too). Keep the cached path while it
/// is a real file; once it vanishes, fall back to the installed `rimz` on `PATH`
/// so the sidebar heals itself instead of degrading until it is killed.
fn resolve_snapshot_bin(cached: &Path) -> PathBuf {
    if cached.is_file() {
        return cached.to_path_buf();
    }
    // A bare name; `Command::new` resolves it against `PATH`.
    PathBuf::from(format!("rimz{}", std::env::consts::EXE_SUFFIX))
}

/// Strip the kernel's " (deleted)" annotation from a `/proc/self/exe` path.
/// `None` when the path carries no such suffix.
fn strip_deleted_suffix(path: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    const DELETED_SUFFIX: &[u8] = b" (deleted)";
    let stripped = path.as_os_str().as_bytes().strip_suffix(DELETED_SUFFIX)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(stripped)))
}

/// Whether the binary at `target` is byte-identical to the image this process
/// is currently running. `None` when the running image's bytes can't be read —
/// no `/proc/self/exe` (non-Linux) or an IO race — in which case the caller
/// re-execs unconditionally, preserving the always-load-the-on-disk-build
/// behavior.
fn running_image_matches(target: &Path) -> Option<bool> {
    let running = running_image_path()?;
    same_file_contents(&running, target).ok()
}

/// Path that reads back the bytes of the image this process is executing. Linux
/// exposes it as `/proc/self/exe`, which resolves to the running inode even
/// after an atomic-rename install has unlinked it from its original path — so a
/// post-install renderer can still read the build it is running.
#[cfg(target_os = "linux")]
fn running_image_path() -> Option<PathBuf> {
    Some(PathBuf::from("/proc/self/exe"))
}

/// No `/proc` to read the running image from, so reload always re-execs.
#[cfg(not(target_os = "linux"))]
fn running_image_path() -> Option<PathBuf> {
    None
}

/// Whether two files hold byte-identical content. A size mismatch is an
/// immediate `false`; otherwise both streams are read in lockstep chunks and
/// the compare early-exits on the first difference, so no whole binary is ever
/// buffered.
fn same_file_contents(a: &Path, b: &Path) -> io::Result<bool> {
    if std::fs::metadata(a)?.len() != std::fs::metadata(b)?.len() {
        return Ok(false);
    }
    let mut reader_a = io::BufReader::new(std::fs::File::open(a)?);
    let mut reader_b = io::BufReader::new(std::fs::File::open(b)?);
    let mut buf_a = [0u8; 8192];
    let mut buf_b = [0u8; 8192];
    loop {
        let read_a = fill(&mut reader_a, &mut buf_a)?;
        let read_b = fill(&mut reader_b, &mut buf_b)?;
        if read_a != read_b {
            // Equal lengths were confirmed above; a differing fill here means a
            // concurrent truncate — treat as not-identical.
            return Ok(false);
        }
        if read_a == 0 {
            return Ok(true);
        }
        if buf_a[..read_a] != buf_b[..read_b] {
            return Ok(false);
        }
    }
}

/// Read up to `buf.len()` bytes, looping past short reads and `Interrupted`
/// until the buffer is full or EOF. Returns how many bytes were read.
fn fill(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// How long the refresh loop may stay continuously degraded before the renderer
/// gives up and exits. Generous so a transient mux hiccup or the sub-second gap
/// while `cargo install` swaps `rimz` never closes a healthy sidebar; short
/// enough that a genuinely broken renderer (missing `RIMZ_BIN`, deleted ledger,
/// an old build whose heartbeat subcommand was removed) heals on the next
/// reload/attach instead of lingering for minutes.
const GIVE_UP_AFTER_DEGRADED: Duration = Duration::from_secs(30);

/// Whether the refresh loop has been *continuously* degraded past
/// [`GIVE_UP_AFTER_DEGRADED`]. Keys off the sticky health alert: `since` is
/// pinned to the start of the current failure episode and any successful fetch
/// clears the active state (the alert lingers only as a dim recovered notice),
/// so this fires only on an unbroken run of failures, never after a recovery.
fn degraded_too_long(health: &Health, now: Timestamp) -> bool {
    health
        .alert
        .as_ref()
        .filter(|alert| alert.is_active())
        .is_some_and(|alert| {
            now.duration_since(alert.since).as_secs() >= GIVE_UP_AFTER_DEGRADED.as_secs() as i64
        })
}

/// Decide whether the sidebar should exit so its own pane closes. The sidebar
/// shares a tab/view with the user's working pane(s); when the last of them
/// exits, the sidebar is alone and has no reason to stay.
///
/// Startup gets one empty observation before close: during session birth the
/// sidebar can run before Zellij materializes the terminal sibling, but a tab
/// born permanently sidebar-only must still clean itself up.
///
/// `sibling_count` is `None` when the count could not be determined (the
/// snapshot carries no `own_view` — no mux pane env var, so no
/// `--exclude-pane-id`, or our own pane was missing from the live list); in
/// that case we never close.
fn self_close_decision(state: &mut SelfCloseState, sibling_count: Option<usize>) -> bool {
    state.should_close(sibling_count)
}

#[derive(Debug, Default)]
struct SelfCloseState {
    seen_sibling: bool,
    empty_startup_observations: u8,
}

impl SelfCloseState {
    fn should_close(&mut self, sibling_count: Option<usize>) -> bool {
        match sibling_count {
            Some(0) if self.seen_sibling => true,
            Some(0) => {
                self.empty_startup_observations = self.empty_startup_observations.saturating_add(1);
                self.empty_startup_observations >= EMPTY_STARTUP_OBSERVATIONS_BEFORE_CLOSE
            }
            Some(_) => {
                self.seen_sibling = true;
                self.empty_startup_observations = 0;
                false
            }
            None => false,
        }
    }
}

const EMPTY_STARTUP_OBSERVATIONS_BEFORE_CLOSE: u8 = 2;
const STARTUP_SELF_CLOSE_RECHECK_AFTER: Duration = Duration::from_secs(1);

/// Decide whether the daemon-view sidebar should detach the client because the
/// `rimzd` daemon tab is the only tab left in the session. Mirrors
/// [`SelfCloseState`], but it detaches the client (the session keeps running)
/// rather than exiting, fires once, and only after a working view has been seen.
///
/// `only_daemon` is the snapshot's `only_daemon_view_remains`, passed only when
/// this renderer's own view *is* the daemon view (the caller gates on
/// `SidebarOwnView::own_view_is_daemon`); otherwise `None`, which never detaches.
#[derive(Debug, Default)]
struct SessionExitState {
    /// Latched once a non-daemon (working) view has ever been seen. Until then,
    /// "only the daemon view remains" is session birth (the `rimzd` tab is born
    /// first), not teardown, so it must never detach.
    seen_other_view: bool,
    /// Latched after a detach has been requested once, so a slow client teardown
    /// spanning the next few ticks does not spawn redundant detaches.
    fired: bool,
}

impl SessionExitState {
    fn should_detach(&mut self, only_daemon: Option<bool>) -> bool {
        match only_daemon {
            // A working view still exists → latch it; never detach while the
            // user has work open.
            Some(false) => {
                self.seen_other_view = true;
                false
            }
            // Only the daemon view remains, a working view has come and gone, and
            // we have not detached yet → the room emptied: detach, once.
            Some(true) if self.seen_other_view && !self.fired => {
                self.fired = true;
                true
            }
            // Already fired, or session birth (no working view seen yet): hold.
            Some(true) => false,
            // Not in the daemon view, or unknown: never our call.
            None => false,
        }
    }
}

/// This process's normalized pane id, read from the multiplexer's per-pane env
/// var. Zellij exposes a bare integer in `ZELLIJ_PANE_ID` (normalized as
/// `terminal_<id>`); tmux exposes the full raw id in `TMUX_PANE`.
fn own_pane_id(mux: MuxName) -> Option<PaneId> {
    let raw = match mux {
        MuxName::Zellij => format!("terminal_{}", std::env::var("ZELLIJ_PANE_ID").ok()?),
        MuxName::Tmux => std::env::var("TMUX_PANE").ok()?,
    };
    Some(PaneId::from_parts(mux, raw))
}

/// A single transient fetch hiccup must not flash a scary banner: the loop
/// already holds the last good frame, so absorb the first failures silently
/// and only raise an alert once a failure persists this many consecutive
/// fetches. Sustained failures still surface promptly (~one tick apart).
const ALERT_AFTER_FAILURES: u32 = 2;

/// Debounced, sticky health of the refresh loop. `failure_streak` counts
/// consecutive failed fetches so a lone blip never alarms; `alert` is the
/// bottom-of-sidebar notice, which survives recovery (marked recovered) until
/// the user dismisses it.
#[derive(Clone, Debug, Default)]
pub struct Health {
    pub failure_streak: u32,
    pub alert: Option<Alert>,
}

/// Sticky state for the last-known-good commit gate, kept beside [`Health`] but
/// deliberately orthogonal to it: `Health` tracks a *failed fetch*, this tracks
/// a fetch that *succeeded but regressed transiently* and was held. `Gate`
/// never feeds `failure_streak`/`degraded_too_long`, so a sub-second binding
/// glitch neither flashes the degraded banner nor counts toward self-close.
/// `reject_streak` and `rejecting_since` bound how long a regression may be held
/// before the escape hatch releases it (see [`escape_hatch_open`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GateState {
    pub reject_streak: u32,
    pub rejecting_since: Option<Timestamp>,
}

/// Consecutive holds before the escape hatch accepts a regression anyway. Each
/// reject fires one immediate self-heal refetch. The rollup is now read fresh
/// from the atomic `latest.json` each fold (it only ever reflects committed
/// events), so a multi-frame transient agent-drop no longer occurs — the gate
/// needs to absorb only a single slipped frame. Two holds confirm a *genuine*
/// exit (its shell pane survives) and demote it promptly, while a true one-frame
/// flicker recovers on the first reject's refetch and is never accepted.
const ACCEPT_REGRESSION_AFTER_REJECTS: u32 = 2;

/// Hard wall-clock ceiling on a hold episode — the load-bearing hatch, since a
/// slow poll cadence could otherwise stretch the count out. One second caps a
/// genuine exit on the producer tab (whose reject-refetches each pay a
/// `list-panes` round-trip) while staying above a single such round-trip, and
/// well under [`GIVE_UP_AFTER_DEGRADED`].
const ACCEPT_REGRESSION_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitDecision {
    /// Replace the cache with the incoming snapshot.
    Accept,
    /// Keep the prior good snapshot; the incoming one is a transient regression.
    KeepPrior,
}

/// Decide whether `incoming` may replace the last-known-good `prev`. Pure: the
/// clock and the streak arrive as arguments so the escape hatch is
/// deterministic in tests. A regression is held only while the *panel set is
/// unchanged* and a pane that `prev` rendered as an agent (or remote-control)
/// host now renders as a bare process — exactly the phantom-`process` flicker.
/// Persistence, not the rollup's `agents` list, distinguishes a transient drop
/// (recovers next read) from a genuine exit (persists until the hatch opens),
/// because the root-cause race is the agent momentarily *leaving* that list.
fn gate_commit(
    prev: &SidebarSnapshot,
    incoming: &SidebarSnapshot,
    gate: &GateState,
    now: Timestamp,
) -> CommitDecision {
    if pane_id_set(prev) != pane_id_set(incoming) {
        // The room genuinely changed (a pane opened or closed); never hold.
        return CommitDecision::Accept;
    }
    if !demotes_agentish_to_process(prev, incoming) {
        return CommitDecision::Accept;
    }
    if escape_hatch_open(gate, now) {
        return CommitDecision::Accept;
    }
    CommitDecision::KeepPrior
}

/// The set of live pane ids a snapshot renders a row for.
fn pane_id_set(snapshot: &SidebarSnapshot) -> HashSet<&PaneId> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter_map(|row| row.pane.as_ref().map(|pane| &pane.pane_id))
        .collect()
}

/// True when some pane that `prev` rendered as an agent is a bare process row in
/// `incoming` — the Agent→Process demotion the gate protects against.
fn demotes_agentish_to_process(prev: &SidebarSnapshot, incoming: &SidebarSnapshot) -> bool {
    let agentish: HashSet<&PaneId> = prev
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.row_kind == rimz::SidebarRowKind::Agent)
        .filter_map(|row| row.pane.as_ref().map(|pane| &pane.pane_id))
        .collect();
    incoming
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.row_kind == rimz::SidebarRowKind::Process)
        .filter_map(|row| row.pane.as_ref().map(|pane| &pane.pane_id))
        .any(|pane_id| agentish.contains(pane_id))
}

/// Whether a hold episode has run long enough — by count or wall-clock — to
/// accept the regression and stop holding. Mirrors [`degraded_too_long`]'s
/// "never freeze forever" rule for the gate.
fn escape_hatch_open(gate: &GateState, now: Timestamp) -> bool {
    gate.reject_streak >= ACCEPT_REGRESSION_AFTER_REJECTS
        || gate.rejecting_since.is_some_and(|since| {
            now.duration_since(since).as_secs() >= ACCEPT_REGRESSION_AFTER.as_secs() as i64
        })
}

/// Overlay the last-known-good gate on a freshly computed [`RenderState`].
///
/// A *failed* fetch already fell back to the prior snapshot inside
/// [`compute_next_state`], so it is never gated here. A *successful* fetch that
/// [`gate_commit`] judges a transient regression is held: the prior good frame
/// becomes both the rendered snapshot and the next-tick baseline
/// (`last_snapshot`), so the cache never advances onto bad data and the next
/// comparison is still against the last good frame. Returns the possibly-held
/// state, the next gate state, and whether this fetch was rejected (the loop
/// fires one self-heal refetch on a reject).
fn apply_gate(
    mut state: RenderState,
    fetch_was_ok: bool,
    prev_good: &SidebarSnapshot,
    gate: &GateState,
    now: Timestamp,
) -> (RenderState, GateState, bool) {
    if fetch_was_ok
        && gate_commit(prev_good, &state.snapshot, gate, now) == CommitDecision::KeepPrior
    {
        state.snapshot = prev_good.clone();
        state.last_snapshot = Some(prev_good.clone());
        let next = GateState {
            reject_streak: gate.reject_streak.saturating_add(1),
            rejecting_since: gate.rejecting_since.or(Some(now)),
        };
        (state, next, true)
    } else {
        (state, GateState::default(), false)
    }
}

/// Decide what to render next given the latest heartbeat + snapshot outcomes.
/// Pure data, no I/O — extracted so the loop's recovery rules are testable.
pub fn compute_next_state(
    workspace_id: &WorkspaceId,
    heartbeat_failure: Option<String>,
    snapshot: std::result::Result<SidebarSnapshot, String>,
    previous_snapshot: Option<SidebarSnapshot>,
    previous_health: &Health,
) -> RenderState {
    let (last_snapshot, snapshot_failure) = match snapshot {
        Ok(snapshot) => (Some(snapshot), None),
        Err(reason) => (previous_snapshot, Some(reason)),
    };

    // A failed snapshot is the headline; a heartbeat-only failure still keeps
    // the fresh snapshot but reports its own reason.
    let failure = snapshot_failure
        .map(|reason| format!("snapshot failed: {reason}"))
        .or_else(|| heartbeat_failure.map(|reason| format!("heartbeat failed: {reason}")));

    let health = next_health(previous_health, failure);

    let snapshot_to_render = last_snapshot
        .clone()
        .unwrap_or_else(|| placeholder_snapshot(workspace_id.clone()));

    RenderState {
        snapshot: snapshot_to_render,
        health,
        last_snapshot,
    }
}

/// Fold the latest fetch outcome into the debounced, sticky health.
///
/// - A failure bumps the streak and, once it crosses [`ALERT_AFTER_FAILURES`],
///   arms (or refreshes) an active alert, preserving `since` so "for Ns" grows
///   monotonically across an episode.
/// - A success resets the streak and marks any active alert recovered, leaving
///   it pinned to the bottom until the user dismisses it.
fn next_health(previous: &Health, failure: Option<String>) -> Health {
    match failure {
        Some(reason) => {
            let failure_streak = previous.failure_streak.saturating_add(1);
            let alert = if failure_streak >= ALERT_AFTER_FAILURES {
                let since = previous
                    .alert
                    .as_ref()
                    .filter(|alert| alert.is_active())
                    .map(|alert| alert.since)
                    .unwrap_or_else(Timestamp::now);
                Some(Alert {
                    reason,
                    since,
                    recovered_at: None,
                })
            } else {
                // Below the threshold: absorb the blip, but keep any lingering
                // recovered alert from a previous episode.
                previous.alert.clone()
            };
            Health {
                failure_streak,
                alert,
            }
        }
        None => {
            let alert = previous.alert.clone().map(|mut alert| {
                if alert.is_active() {
                    alert.recovered_at = Some(Timestamp::now());
                }
                alert
            });
            Health {
                failure_streak: 0,
                alert,
            }
        }
    }
}

/// Animation tick: how often an animated row advances a spin frame — a running
/// agent's head, a resolver, or an active process spinning on real work. Pure
/// in-process redraw from the cached snapshot — it never forks a fetch — so the
/// spin layer is decoupled from the data layer and stays smooth regardless of
/// fetch latency. Clamped against the data tick so a slow `tick_seconds` never
/// stutters, and only used while [`render::has_live_animation`] reports
/// something to move.
const ANIMATION_FRAME: Duration = Duration::from_millis(100);

/// The animation frame index for `now`, derived from elapsed wall-clock since
/// the serve loop's monotonic base. Every redraw path sets the phase from this,
/// so the spin advances on real time and survives re-fetches and ledger deltas
/// without a per-tick counter that a break-and-refetch could reset.
fn wall_clock_phase(start: Instant) -> u64 {
    (start.elapsed().as_millis() / ANIMATION_FRAME.as_millis()) as u64
}

fn tick_for(seconds: u64) -> Duration {
    Duration::from_secs(seconds.max(1))
}

fn placeholder_snapshot(workspace_id: WorkspaceId) -> SidebarSnapshot {
    let display_name = workspace_id.as_str().to_owned();
    SidebarSnapshot {
        workspace_id,
        display_name,
        generated_at: Timestamp::now(),
        worktree_groups: Vec::new(),
        needs_attention: Vec::new(),
        resolver_working: Vec::new(),
        agents: Vec::new(),
        agent_hooks_ready: false,
        codex_hooks_ready: false,
        own_view: None,
        only_daemon_view_remains: false,
        project_root: None,
        worktree_roots: Vec::new(),
        sidebar: rimz::config::SidebarConfig::default(),
        providers: Vec::new(),
        today_cost_usd: None,
    }
}

/// Bundle returned by [`compute_next_state`]; the loop applies it verbatim.
#[derive(Clone, Debug)]
pub struct RenderState {
    pub snapshot: SidebarSnapshot,
    pub health: Health,
    pub last_snapshot: Option<SidebarSnapshot>,
}

/// Fork `rimz sidebar snapshot` for the producer: it resolves the workspace,
/// runs `list-panes` and git, and publishes the shared cache the consumers read.
/// Off the render loop (fetch worker thread), so the round-trip never stalls
/// animation. Consumers do not call this — they read the published cache in
/// process via [`rimz::sidebar::snapshot::read_published_snapshot`].
fn fetch_snapshot_for(
    rimz_bin: &Path,
    workspace_id: &WorkspaceId,
    mux: Option<MuxName>,
    session_name: Option<&str>,
    exclude_pane_id: Option<PaneId>,
    min_pane_cache_ms: Option<u64>,
) -> Result<SidebarSnapshot> {
    let mut command = Command::new(rimz_bin);
    command
        .args(["sidebar", "snapshot", "--workspace-id"])
        .arg(workspace_id.as_str());
    if let Some(mux) = mux {
        command.args(["--mux", mux.as_str()]);
    }
    if let Some(session_name) = session_name {
        command.args(["--session-name", session_name]);
    }
    if let Some(pane_id) = exclude_pane_id {
        command.args(["--exclude-pane-id", pane_id.as_str()]);
    }
    if let Some(min_pane_cache_ms) = min_pane_cache_ms {
        command
            .arg("--min-pane-cache-ms")
            .arg(min_pane_cache_ms.to_string());
    }
    command.arg("--json");
    let output = command
        .output()
        .map_err(|source| SidebarAppErr::CommandIo {
            program: rimz_bin.display().to_string(),
            source,
        })?;
    if !output.status.success() {
        return Err(SidebarAppErr::SnapshotCommand {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// One refresh cycle's result: the liveness-write error (if any) and the
/// snapshot fetch, kept separate so [`compute_next_state`] can tell a
/// heartbeat-only failure apart from a snapshot failure.
struct FetchOutcome {
    heartbeat_err: Option<String>,
    snapshot: std::result::Result<SidebarSnapshot, String>,
}

/// Write the heartbeat and fetch the snapshot. Runs on the fetch worker thread
/// (and once inline for the first frame), keeping the producer's `list-panes` +
/// git round-trip off the render/input loop so animation never stalls on it.
///
/// One producer per workspace, one renderer per tab. The eldest live instance
/// is the producer: it forks `rimz sidebar snapshot` (`list-panes`/git) and
/// publishes the shared cache. Every younger instance is a consumer — it reads
/// that published frame **in process** ([`rimz::sidebar::snapshot::read_published_snapshot`]),
/// folding only its own-pane exclusion, so it never forks a subprocess, never
/// runs `list-panes`/git, and never exits — a per-tab renderer stays alive and
/// paints. The mux/git round-trip is paid once per workspace; a consumer with no
/// published frame yet reports a soft miss so the gate holds its last good frame.
fn run_fetch(
    config: &ServeConfig,
    runtime: &RuntimePaths,
    socket_path: &Path,
    request: FetchRequest,
    write_heartbeat_now: bool,
) -> FetchOutcome {
    let heartbeat_err = if write_heartbeat_now {
        write_heartbeat(config, runtime, socket_path)
            .err()
            .map(|err| err.to_string())
    } else {
        None
    };
    let is_producer = !rimz::sidebar::elder_sidebar_present(runtime, &config.instance_id);
    let exclude = own_pane_id(config.mux);
    // Take the producer path — fork the real `list-panes`/git snapshot — when we
    // are the elected producer, when the user forced a reload (`r`), or when the
    // producer's published frame has gone stale. The last is the consumer
    // self-heal: rather than hold a stalled producer's last frame forever (the
    // freeze), a consumer produces its own current frame; `cached_base_or_produce`
    // single-flights, so a fleet self-healing at once still elects one producer.
    let produce = is_producer
        || request.force_produce
        || rimz::sidebar::snapshot::published_frame_is_stale(
            runtime,
            &config.session_name,
            rimz::sidebar::snapshot::unix_now_ms(),
        );
    let snapshot = if produce {
        fetch_snapshot_for(
            &resolve_snapshot_bin(&config.rimz_bin),
            &config.workspace_id,
            Some(config.mux),
            Some(&config.session_name),
            exclude,
            request.min_pane_cache_ms,
        )
        .map_err(|err| err.to_string())
    } else {
        // Consumer: fold the producer's coalesced panes with the event-fresh
        // rollup read in process from `latest.json` (read-only — no ledger-writer
        // import), so a status change or a new agent in an existing pane repaints
        // within one wakeup without forking `list-panes`.
        match rimz::ledger::paths::StatePaths::for_workspace(config.workspace_id.clone()) {
            Ok(state) => rimz::sidebar::snapshot::read_published_snapshot(
                &state,
                runtime,
                &config.session_name,
                exclude.as_ref(),
            )
            .ok_or_else(|| "waiting for the producer's first published snapshot".to_owned()),
            Err(err) => Err(err.to_string()),
        }
    };
    FetchOutcome {
        heartbeat_err,
        snapshot,
    }
}

/// One request to the fetch worker. `force_produce` makes the run take the
/// producer path (real `list-panes`/git) regardless of election. When it is
/// paired with `min_pane_cache_ms`, the producer ignores a pane cache older
/// than the signal that asked for fresh topology.
#[derive(Clone, Copy, Debug, Default)]
struct FetchRequest {
    force_produce: bool,
    min_pane_cache_ms: Option<u64>,
}

impl FetchRequest {
    fn fresh_panes() -> Self {
        Self {
            force_produce: true,
            min_pane_cache_ms: Some(rimz::sidebar::snapshot::unix_now_ms()),
        }
    }

    fn merge(&mut self, other: Self) {
        self.force_produce |= other.force_produce;
        self.min_pane_cache_ms = match (self.min_pane_cache_ms, other.min_pane_cache_ms) {
            (Some(current), Some(next)) => Some(current.max(next)),
            (Some(current), None) => Some(current),
            (None, Some(next)) => Some(next),
            (None, None) => None,
        };
    }
}

/// Write this renderer's heartbeat at most this often. The heartbeat TTL is 5s;
/// 2s keeps two missed writes of slack while avoiding an atomic file write for
/// every ledger-delta fetch in a busy fleet.
const HEARTBEAT_WRITE_INTERVAL: Duration = Duration::from_secs(2);

/// Spawn the background fetch worker. It blocks for a request, coalesces any
/// that piled up (a ledger-delta storm collapses to one fetch), runs one
/// [`run_fetch`], hands the result back over `result_tx`, and pokes the loop's
/// wakeup socket so it folds the result without polling. The thread ends when
/// the loop drops `request_tx`.
fn spawn_fetch_worker(
    config: ServeConfig,
    runtime: RuntimePaths,
    socket_path: PathBuf,
    request_rx: std::sync::mpsc::Receiver<FetchRequest>,
    result_tx: std::sync::mpsc::Sender<FetchOutcome>,
) {
    std::thread::spawn(move || {
        let waker = UnixDatagram::unbound().ok();
        let mut last_heartbeat: Option<Instant> = None;
        while let Ok(first) = request_rx.recv() {
            // Coalesce any requests that piled up into one run, keeping the
            // strongest intent and the newest pane-freshness floor.
            let mut request = first;
            while let Ok(extra) = request_rx.try_recv() {
                request.merge(extra);
            }
            let write_heartbeat_now = heartbeat_write_due(last_heartbeat);
            if write_heartbeat_now {
                last_heartbeat = Some(Instant::now());
            }
            let outcome = run_fetch(
                &config,
                &runtime,
                &socket_path,
                request,
                write_heartbeat_now,
            );
            if result_tx.send(outcome).is_err() {
                return;
            }
            if let Some(waker) = &waker {
                let _ = waker.send_to(SNAPSHOT_WAKEUP, &socket_path);
            }
        }
    });
}

fn heartbeat_write_due(last_heartbeat: Option<Instant>) -> bool {
    last_heartbeat.is_none_or(|last| last.elapsed() >= HEARTBEAT_WRITE_INTERVAL)
}

fn spawn_self_close_probe_worker(
    config: ServeConfig,
    socket_path: PathBuf,
    request_rx: std::sync::mpsc::Receiver<SelfCloseProbeRequest>,
    result_tx: std::sync::mpsc::Sender<SelfCloseProbeOutcome>,
) {
    std::thread::spawn(move || {
        let waker = UnixDatagram::unbound().ok();
        while let Ok(first) = request_rx.recv() {
            let mut delay = first.delay;
            while let Ok(extra) = request_rx.try_recv() {
                delay = delay.min(extra.delay);
            }
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            let outcome = run_self_close_probe(&config);
            if result_tx.send(outcome).is_err() {
                return;
            }
            if let Some(waker) = &waker {
                let _ = waker.send_to(SELF_CLOSE_WAKEUP, &socket_path);
            }
        }
    });
}

fn run_self_close_probe(config: &ServeConfig) -> SelfCloseProbeOutcome {
    let Some(own) = own_pane_id(config.mux) else {
        return SelfCloseProbeOutcome {
            sibling_count: None,
            error: None,
        };
    };
    match rimz::mux::backend_for(config.mux).list_panes(PaneListOptions {
        session_name: Some(config.session_name.clone()),
    }) {
        Ok(panes) => SelfCloseProbeOutcome {
            // This probe reads only `sibling_count`; the focus timestamp is
            // irrelevant here, so stamp it now.
            sibling_count: rimz::SidebarOwnView::from_panes(&own, &panes, Timestamp::now())
                .map(|view| view.sibling_count),
            error: None,
        },
        Err(err) => SelfCloseProbeOutcome {
            sibling_count: None,
            error: Some(err.to_string()),
        },
    }
}

/// Ask the fetch worker for a fresh snapshot. `in_flight` collapses redundant
/// requests while one is already running; `force_after` (set by a ledger delta,
/// i.e. new committed data) guarantees one more fetch once the in-flight one
/// returns, so a delta that races an in-flight fetch is never lost.
/// `request` carries the strongest freshness requirement currently known.
fn request_fetch(
    request_tx: &std::sync::mpsc::Sender<FetchRequest>,
    in_flight: &mut bool,
    pending_refetch: &mut Option<FetchRequest>,
    request: FetchRequest,
    force_after: bool,
) {
    if !*in_flight {
        if request_tx.send(request).is_ok() {
            *in_flight = true;
        }
    } else if force_after {
        match pending_refetch {
            Some(pending) => pending.merge(request),
            None => *pending_refetch = Some(request),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SelfCloseProbeRequest {
    delay: Duration,
}

#[derive(Debug, PartialEq, Eq)]
struct SelfCloseProbeOutcome {
    sibling_count: Option<usize>,
    error: Option<String>,
}

/// Ask the lightweight self-close worker for a live sibling count. While a
/// probe is already running, keep the shortest pending delay so an immediate
/// resize probe wins over the startup grace recheck.
fn request_self_close_probe(
    request_tx: &std::sync::mpsc::Sender<SelfCloseProbeRequest>,
    in_flight: &mut bool,
    pending_delay: &mut Option<Duration>,
    delay: Duration,
) {
    if !*in_flight {
        if request_tx.send(SelfCloseProbeRequest { delay }).is_ok() {
            *in_flight = true;
        }
        return;
    }
    *pending_delay = Some(pending_delay.map_or(delay, |pending| pending.min(delay)));
}

/// Fold a fast probe result into the same latch the snapshot path uses. The
/// probe is best-effort metadata: failures never degrade the rendered frame
/// because the normal snapshot backstop still owns recovery.
fn apply_self_close_probe_outcome(
    config: &ServeConfig,
    outcome: SelfCloseProbeOutcome,
    self_close: &mut SelfCloseState,
) -> bool {
    if let Some(error) = outcome.error {
        debug!(
            session = %config.session_name,
            error = %error,
            "self-close pane probe failed",
        );
        return false;
    }
    if self_close_decision(self_close, outcome.sibling_count) {
        debug!(
            session = %config.session_name,
            "sidebar tab emptied; exiting after resize probe",
        );
        return true;
    }
    false
}

/// What [`apply_fetch_outcome`] reports back to the loop: whether to exit, and
/// whether this fetch was held as a transient regression (the loop fires one
/// self-heal refetch so the cache reaches the next good frame).
struct ApplyOutcome {
    should_exit: bool,
    rejected: bool,
}

/// Fold one fetch outcome into the render state: gate it against the
/// last-known-good frame, update health, snapshot, and selection, draw the
/// frame, and report whether the loop should exit — give up after sustained
/// degradation, or self-close once the tab has emptied. Shared by the first
/// synchronous frame and every background-fetch result so the recovery rules
/// live in one place.
#[allow(clippy::too_many_arguments)]
fn apply_fetch_outcome(
    config: &ServeConfig,
    outcome: FetchOutcome,
    last_snapshot: &mut Option<SidebarSnapshot>,
    current: &mut SidebarSnapshot,
    health: &mut Health,
    gate: &mut GateState,
    self_close: &mut SelfCloseState,
    session_exit: &mut SessionExitState,
    ui: &mut UiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    anim_start: Instant,
) -> Result<ApplyOutcome> {
    // The gate compares the incoming snapshot against the last frame we actually
    // committed; `current` still holds it until we overwrite it below.
    let fetch_was_ok = outcome.snapshot.is_ok();
    let prev_good = current.clone();
    let computed = compute_next_state(
        &config.workspace_id,
        outcome.heartbeat_err,
        outcome.snapshot,
        last_snapshot.take(),
        health,
    );
    let (state, next_gate, rejected) =
        apply_gate(computed, fetch_was_ok, &prev_good, gate, Timestamp::now());
    *gate = next_gate;
    if let Some(alert) = state
        .health
        .alert
        .as_ref()
        .filter(|alert| alert.is_active())
    {
        warn!(reason = %alert.reason, "sidebar refresh degraded");
    }
    *last_snapshot = state.last_snapshot;
    *health = state.health;
    *current = state.snapshot;
    // Reconcile the highlight before drawing: re-anchor the identity-keyed
    // selection to its row (so a status-churn reorder never slides it onto a
    // neighbour) and apply the edge-triggered external-focus mirror. Running it
    // before the draw means an external focus move paints this frame, not next.
    let external_focus = current
        .own_view
        .as_ref()
        .filter(|view| !view.own_is_focused)
        .and_then(|view| {
            view.focused_pane_id
                .clone()
                .map(|pane| (pane, view.focused_observed_at))
        });
    reconcile_selection(ui, current, external_focus);
    ui.animation_phase = wall_clock_phase(anim_start);
    render::draw_to_terminal_with_ui(terminal, current, health.alert.as_ref(), ui)?;

    // A renderer degraded this long is non-functional and, with a now-stale
    // heartbeat, unreachable by `rimz reload` — so it gives up rather than
    // lingering as a zombie showing a frozen frame. Exiting closes its
    // `close_on_exit` pane; reload/attach recovery then rebuilds a current
    // sidebar against the live panes.
    if degraded_too_long(health, Timestamp::now()) {
        warn!(
            session = %config.session_name,
            reason = health.alert.as_ref().map(|alert| alert.reason.as_str()),
            "sidebar degraded too long; exiting so the pane closes and reload/attach can rebuild it",
        );
        return Ok(ApplyOutcome {
            should_exit: true,
            rejected,
        });
    }

    // Own-view (sibling count) rides in on the snapshot — the CLI computes it
    // from the same pane list it already enumerated. Resize events have their
    // own metadata-only fast probe; this snapshot path stays the durable
    // backstop. The focus-driven selection reconcile already ran before the
    // draw above.
    if self_close_decision(
        self_close,
        current.own_view.as_ref().map(|view| view.sibling_count),
    ) {
        debug!(
            session = %config.session_name,
            "sidebar tab emptied; exiting so the pane closes itself",
        );
        return Ok(ApplyOutcome {
            should_exit: true,
            rejected,
        });
    }
    // The daemon-view sidebar detaches the client once the daemon tab is the
    // only tab left — gated on our own view being the daemon view, then latched
    // so it fires once after a working view has come and gone. Unlike
    // self-close it does not exit: the daemon pane keeps running in the
    // background session and resurrects on reattach.
    let only_daemon = current.own_view.as_ref().and_then(|view| {
        view.own_view_is_daemon
            .then_some(current.only_daemon_view_remains)
    });
    if session_exit.should_detach(only_daemon) {
        info!(
            session = %config.session_name,
            "only the daemon view remains; detaching the client",
        );
        request_detach(config);
    }
    Ok(ApplyOutcome {
        should_exit: false,
        rejected,
    })
}

/// Detach the attached client from the session, best-effort, by shelling out to
/// `rimz pane detach` (the same cached binary the snapshot fork uses, so no mux
/// command knowledge leaks into the sidebar). The daemon-view sidebar calls this
/// once the daemon tab is the only tab left; the background session and its
/// daemons keep running and resurrect on the next attach. A failure is logged,
/// never fatal — the session stays attached and the next tick retries.
fn request_detach(config: &ServeConfig) {
    let bin = resolve_snapshot_bin(&config.rimz_bin);
    match Command::new(&bin)
        .args(["pane", "detach", "--mux"])
        .arg(config.mux.as_str())
        .args(["--session-name", &config.session_name])
        .status()
    {
        Ok(status) if status.success() => {
            debug!(session = %config.session_name, "client detach requested");
        }
        Ok(status) => warn!(
            session = %config.session_name,
            code = ?status.code(),
            "client detach exited non-zero",
        ),
        Err(err) => warn!(
            session = %config.session_name,
            error = %err,
            "client detach spawn failed",
        ),
    }
}

/// Refresh this instance's liveness heartbeat. Written in-process — no `rimz
/// sidebar heartbeat` fork per tick — through the shared liveness helper, which
/// keeps the JSON shape and atomic write identical to what the ledger wakeup
/// fanout and launch freshness gate expect.
fn write_heartbeat(config: &ServeConfig, runtime: &RuntimePaths, socket_path: &Path) -> Result<()> {
    rimz::sidebar::write_heartbeat(
        runtime,
        config.workspace_id.clone(),
        &config.instance_id,
        config.mux,
        &config.session_name,
        socket_path,
        own_pane_id(config.mux),
    )
    .map_err(|err| SidebarAppErr::Heartbeat(err.to_string()))
}

fn sidebar_socket_path(runtime: &RuntimePaths, instance_id: &SidebarInstanceId) -> PathBuf {
    // Use the short (12-hex) id, not the full `sb_<32 hex>`: the bound path must
    // fit the 108-byte AF_UNIX budget, same as the per-request feed socket. The
    // heartbeat carries this path verbatim, so senders stay in sync.
    runtime
        .sock_dir
        .join(format!("sidebar.{}.sock", instance_id.short()))
}

fn bind_socket(path: &Path) -> io::Result<UnixDatagram> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    UnixDatagram::bind(path)
}

/// How long the resize watcher blocks per poll. A resize event wakes it
/// immediately regardless; this only bounds how often it loops while idle.
const RESIZE_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Watch the terminal for resize and key events and wake the serve loop. Runs
/// on its own thread for the life of the process; it self-wakes by sending to
/// `wake_path` (the loop's bound wakeup socket), which keeps redraw and input
/// on one path. Stops quietly if the event source or socket goes away.
fn spawn_event_waker(wake_path: PathBuf) {
    std::thread::spawn(move || {
        let waker = match UnixDatagram::unbound() {
            Ok(socket) => socket,
            Err(err) => {
                warn!(error = %err, "event waker disabled; input waits for the tick");
                return;
            }
        };
        loop {
            match event::poll(RESIZE_POLL_INTERVAL) {
                Ok(true) => match event::read() {
                    Ok(Event::Resize(_, _)) => {
                        if waker.send_to(b"resize", &wake_path).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if let Some(encoded) = encode_key(key.code)
                            && waker.send_to(encoded.as_bytes(), &wake_path).is_err()
                        {
                            return;
                        }
                    }
                    Ok(Event::Mouse(mouse)) => {
                        if let Some(encoded) = encode_mouse(mouse.kind, mouse.column, mouse.row)
                            && waker.send_to(encoded.as_bytes(), &wake_path).is_err()
                        {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        warn!(error = %err, "event waker stopping: event read failed");
                        return;
                    }
                },
                Ok(false) => {}
                Err(err) => {
                    warn!(error = %err, "event waker stopping: event poll failed");
                    return;
                }
            }
        }
    });
}

/// Apply an input wakeup (key/mouse/resize) to the local UI in place. Input
/// never changes ledger data, so it redraws the *current* snapshot and may jump
/// focus, but it never re-runs the snapshot burst — that per-keystroke refetch
/// was the input lag.
fn apply_input(
    wakeup: Wakeup,
    ui: &mut UiState,
    health: &mut Health,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snapshot: &SidebarSnapshot,
) -> Result<()> {
    let outcome = handle_wakeup(wakeup, ui, snapshot);
    if outcome.dismiss {
        health.alert = None;
    }
    if outcome.redraw {
        render::draw_to_terminal_with_ui(terminal, snapshot, health.alert.as_ref(), ui)?;
    }
    if let Some(index) = outcome.focus_index {
        // The handler already pinned `selected_pane`; fire the async jump. The
        // fold's edge-triggered mirror (`reconcile_selection`) holds the
        // highlight on the clicked pane through the briefly-stale focus, so no
        // optimistic-focus guard is needed.
        focus_selected_row(snapshot, index);
    }
    Ok(())
}

fn handle_wakeup(wakeup: Wakeup, ui: &mut UiState, snapshot: &SidebarSnapshot) -> InputOutcome {
    match wakeup {
        Wakeup::Key(action) => handle_key(action, ui, snapshot),
        Wakeup::MouseClick { column, row } => handle_mouse_click(column, row, ui, snapshot),
        Wakeup::Resize => InputOutcome::redraw(),
        // The serve loop intercepts these before dispatching here: a tick or a
        // ledger delta is the re-fetch trigger, worker completions are folded,
        // and a reload re-execs.
        Wakeup::Tick
        | Wakeup::Ledger { .. }
        | Wakeup::Reload
        | Wakeup::Snapshot
        | Wakeup::SelfCloseProbe => InputOutcome::default(),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InputOutcome {
    redraw: bool,
    focus_index: Option<usize>,
    dismiss: bool,
}

impl InputOutcome {
    fn redraw() -> Self {
        Self {
            redraw: true,
            focus_index: None,
            dismiss: false,
        }
    }

    fn focus(index: usize) -> Self {
        Self {
            redraw: true,
            focus_index: Some(index),
            dismiss: false,
        }
    }

    fn dismiss() -> Self {
        Self {
            redraw: true,
            focus_index: None,
            dismiss: true,
        }
    }
}

fn handle_key(action: KeyAction, ui: &mut UiState, snapshot: &SidebarSnapshot) -> InputOutcome {
    match action {
        KeyAction::Up => {
            if ui.selected_index > 0 {
                select_row(ui, snapshot, ui.selected_index - 1);
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Down => {
            let len = visible_row_count(snapshot);
            if ui.selected_index + 1 < len {
                select_row(ui, snapshot, ui.selected_index + 1);
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Enter => {
            // Local selection + jump on the current row: re-stamp so the
            // highlight holds on it through the briefly-stale post-jump focus,
            // identical to a click.
            select_row(ui, snapshot, ui.selected_index);
            InputOutcome::focus(ui.selected_index)
        }
        KeyAction::Space => {
            if let Some(index) = next_attention_index(snapshot, ui.selected_index) {
                select_row(ui, snapshot, index);
                return InputOutcome::focus(index);
            }
            InputOutcome::default()
        }
        KeyAction::Help => {
            ui.help_visible = !ui.help_visible;
            InputOutcome::redraw()
        }
        KeyAction::Dismiss => InputOutcome::dismiss(),
        KeyAction::Digit(digit) => {
            let index = usize::from(digit.saturating_sub(1));
            if index < visible_row_count(snapshot) {
                select_row(ui, snapshot, index);
                return InputOutcome::focus(index);
            }
            InputOutcome::default()
        }
    }
}

fn handle_mouse_click(
    _column: u16,
    row: u16,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
) -> InputOutcome {
    if let Some(index) = row_index_at_screen_position(ui, row) {
        select_row(ui, snapshot, index);
        return InputOutcome::focus(index);
    }
    InputOutcome::default()
}

/// Point the highlight at a visible row by index — the identity-keyed selection
/// (`selected_pane`) plus its derived render index. Every local selection action
/// (click, `↵`, digit, `␣`, arrow navigation) routes through here, so each one
/// stamps `local_selection` with the chosen pane and the current instant: the
/// newest local pick contests the last valid external focus by timestamp in
/// `reconcile_selection`, and the highlight is always anchored to a pane, never
/// a bare position.
fn select_row(ui: &mut UiState, snapshot: &SidebarSnapshot, index: usize) {
    ui.selected_index = index;
    ui.selected_pane = visible_rows(snapshot)
        .nth(index)
        .and_then(|row| row.pane.as_ref())
        .map(|pane| pane.pane_id.clone());
    if let Some(pane) = ui.selected_pane.clone() {
        ui.local_selection = Some((pane, Timestamp::now()));
    }
}

fn clamp_selection(ui: &mut UiState, snapshot: &SidebarSnapshot) {
    let len = visible_row_count(snapshot);
    if len == 0 {
        ui.selected_index = 0;
    } else if ui.selected_index >= len {
        ui.selected_index = len - 1;
    }
}

/// Reconcile the highlight after folding a new snapshot by contesting two
/// timestamped values — the last local selection and the last *valid* external
/// focus — and letting the newer one win. Keyed on pane identity, never
/// position.
///
/// `external` is the snapshot's focus report `(pane, observed_at)`, already
/// filtered to a non-sidebar focus (`!own_is_focused` with a `Some` pane). It is
/// folded into `external_focus` only when it is a **genuine new external move**:
///
/// 1. **Row guard.** The pane must be an agent row in this snapshot. A focus on
///    a non-row helper pane (`claude rc`, `codex app-server`) — or no report at
///    all (sidebar-self / undiscoverable focus) — is inert and leaves
///    `external_focus` untouched, so a fresh local selection still wins during
///    the click-through focus window.
/// 2. **Identity guard.** The pane must differ from the one `external_focus` was
///    last trusted on (the pane we jumped *from*); an equal report is
///    steady-state or a lagging re-report, not a move. A `None`/cold-start
///    `external_focus` adopts the first valid report.
/// 3. **Monotonic guard.** Its `observed_at` must be newer than the stored
///    sample, rejecting a reordered older one.
///
/// The winner is the value with the newer `Timestamp`; the lone present value
/// wins if only one exists; with neither, the current `selected_pane` holds.
/// Finally a `local_selection`/`external_focus` whose pane has left the snapshot
/// is dropped, and `anchor_selection` re-derives `selected_index` by identity.
fn reconcile_selection(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    external: Option<(PaneId, Timestamp)>,
) {
    if let Some((pane, observed_at)) = external
        && row_index_of_pane(snapshot, &pane).is_some()
    {
        let genuine_move = match &ui.external_focus {
            Some((prev_pane, prev_at)) => pane != *prev_pane && observed_at > *prev_at,
            None => true,
        };
        if genuine_move {
            ui.external_focus = Some((pane, observed_at));
        }
    }

    if let Some(pane) = newer_selection(ui.local_selection.as_ref(), ui.external_focus.as_ref()) {
        ui.selected_pane = Some(pane);
    }

    drop_absent_selection(&mut ui.local_selection, snapshot);
    drop_absent_selection(&mut ui.external_focus, snapshot);
    anchor_selection(ui, snapshot);
}

/// The pane of the timestamped value that was set most recently; the lone
/// present value if only one exists; `None` when neither is set.
fn newer_selection(
    local: Option<&(PaneId, Timestamp)>,
    external: Option<&(PaneId, Timestamp)>,
) -> Option<PaneId> {
    match (local, external) {
        (Some((lp, lt)), Some((ep, et))) => Some(if et > lt { ep.clone() } else { lp.clone() }),
        (Some((lp, _)), None) => Some(lp.clone()),
        (None, Some((ep, _))) => Some(ep.clone()),
        (None, None) => None,
    }
}

/// Clear a timestamped selection whose pane has left the snapshot, so the other
/// value can win the next contest instead of being shadowed by a dangling pick.
fn drop_absent_selection(slot: &mut Option<(PaneId, Timestamp)>, snapshot: &SidebarSnapshot) {
    if let Some((pane, _)) = slot
        && row_index_of_pane(snapshot, pane).is_none()
    {
        *slot = None;
    }
}

/// Re-derive `selected_index` from the identity-keyed `selected_pane`. When the
/// selected pane has left the room its row is gone, so drop the dangling
/// identity and clamp the index — the next external focus edge re-seats it.
fn anchor_selection(ui: &mut UiState, snapshot: &SidebarSnapshot) {
    if let Some(pane) = ui.selected_pane.clone() {
        if let Some(index) = row_index_of_pane(snapshot, &pane) {
            ui.selected_index = index;
            return;
        }
        ui.selected_pane = None;
    }
    clamp_selection(ui, snapshot);
}

/// The visible-row index backing `pane_id`, in `visible_rows` order.
fn row_index_of_pane(snapshot: &SidebarSnapshot, pane_id: &PaneId) -> Option<usize> {
    visible_rows(snapshot).position(|row| {
        row.pane
            .as_ref()
            .is_some_and(|pane| pane.pane_id == *pane_id)
    })
}

fn row_index_at_screen_position(ui: &UiState, row: u16) -> Option<usize> {
    // Borderless: the body fills the frame from row 0 (no border to skip) and a
    // row's lane spine occupies column 0, so a click anywhere on a line — spine
    // included — maps straight onto the hit-test entry built alongside it.
    ui.line_map.get(usize::from(row)).copied().flatten()
}

fn visible_row_count(snapshot: &SidebarSnapshot) -> usize {
    snapshot
        .worktree_groups
        .iter()
        .map(|group| group.rows.len())
        .sum()
}

fn visible_rows(snapshot: &SidebarSnapshot) -> impl Iterator<Item = &rimz::SidebarRow> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
}

fn next_attention_index(snapshot: &SidebarSnapshot, selected: usize) -> Option<usize> {
    let rows = visible_rows(snapshot).collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    let start = selected.saturating_add(1);
    (0..rows.len()).find_map(|offset| {
        let index = (start + offset) % rows.len();
        matches!(
            rows[index].status,
            Some(rimz::feed::AgentStatus::Waiting | rimz::feed::AgentStatus::Failed)
        )
        .then_some(index)
    })
}

/// Focus the pane backing the selected row. A no-op when the row has no pane.
fn focus_selected_row(snapshot: &SidebarSnapshot, selected: usize) {
    let Some(pane) = visible_rows(snapshot)
        .nth(selected)
        .and_then(|row| row.pane.as_ref())
    else {
        return;
    };
    // Jump off the render thread: `focus_pane` still forks the mux client
    // (`zellij action focus-pane-id` / the tmux equivalent), which must never
    // block the loop. The highlight is already redrawn, so the jump is
    // fire-and-forget. Focus the pane bound in the snapshot directly — no
    // `rimz pane focus` child, no per-click `list-panes` re-validation. A pane
    // recycled in the sub-second window since the snapshot self-corrects on the
    // next refresh.
    spawn_pane_focus(pane.pane_id.clone());
}

/// Focus the pane on a detached thread so the keypress/click returns instantly.
/// Errors are logged, not surfaced — a missed jump is a retriable annoyance,
/// never a reason to block the UI.
fn spawn_pane_focus(pane_id: PaneId) {
    std::thread::spawn(move || {
        let backend = rimz::mux::backend_for(pane_id.mux());
        if let Err(err) = backend.focus_pane(&pane_id) {
            warn!(pane = %pane_id, error = %err, "sidebar pane focus failed");
        }
    });
}

struct InputModeGuard;

impl InputModeGuard {
    fn enable() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        if let Err(err) = execute!(io::stdout(), EnableMouseCapture) {
            let _ = terminal::disable_raw_mode();
            return Err(err);
        }
        Ok(Self)
    }
}

impl Drop for InputModeGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        let _ = terminal::disable_raw_mode();
    }
}

/// Removes a per-instance runtime file (wakeup socket, heartbeat) when the
/// sidebar exits, so a later `rimz` launch sees an honest "no sidebar here" and
/// rebirths one rather than trusting a stale artifact.
struct RuntimeFileGuard {
    path: PathBuf,
}

impl Drop for RuntimeFileGuard {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                warn!(path = %self.path.display(), error = %err, "sidebar runtime file cleanup failed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::feed::PaneRef;

    fn workspace() -> WorkspaceId {
        WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap()
    }

    fn snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
        placeholder_snapshot(ws.clone())
    }

    fn pane(raw: &str, view: &str, focused: bool) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some(view.to_owned()),
            view_kind: Some(rimz::ids::ViewKind::Tab),
            view_name: None,
            is_focused: focused,
            client_focused: focused,
            command: Some("zsh".to_owned()),
            cwd: Some("/repo/main".to_owned()),
            pane_pid: None,
            pane_process_start: None,
        }
    }

    fn snapshot_with_panes(ws: &WorkspaceId, panes: Vec<PaneRef>) -> SidebarSnapshot {
        let mut snapshot = snapshot(ws);
        snapshot.worktree_groups = vec![rimz::SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: panes
                .into_iter()
                .map(|pane| rimz::SidebarRow {
                    row_kind: rimz::SidebarRowKind::Process,
                    id: pane.pane_id.to_string(),
                    name: pane.command.clone().unwrap_or_else(|| "process".to_owned()),
                    status: None,
                    permission_posture: None,
                    pane: Some(pane),
                    request_id: None,
                    surface: None,
                    task: None,
                    prompt: None,
                    model: None,
                    effort: None,
                    context_pct: None,
                    total_tokens: None,
                    todo_done: None,
                    todo_total: None,
                    context: None,
                    worktree_path: Some("/repo/main".to_owned()),
                    worktree_branch: Some("main".to_owned()),
                    last_activity: Timestamp::now(),
                    resolver: None,
                    options: Vec::new(),
                    sub_agents: Vec::new(),
                    process_active: false,
                    command_detail: None,
                    compacting: false,
                    spending: None,
                })
                .collect(),
            hidden_count: 0,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
        }];
        snapshot
    }

    fn agent_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
        let mut snapshot = snapshot(ws);
        let row = rimz::SidebarRow {
            row_kind: rimz::SidebarRowKind::Agent,
            id: "agent-1".to_owned(),
            name: "claude".to_owned(),
            status: Some(rimz::feed::AgentStatus::Idle),
            permission_posture: Some(rimz::feed::PermissionPosture::Default),
            pane: Some(pane("terminal_9", "tab_0", false)),
            request_id: None,
            surface: None,
            task: Some("inspect auth".to_owned()),
            prompt: None,
            model: Some("Opus".to_owned()),
            effort: None,
            context_pct: None,
            total_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            last_activity: Timestamp::now(),
            resolver: None,
            options: Vec::new(),
            sub_agents: Vec::new(),
            process_active: false,
            command_detail: None,
            compacting: false,
            spending: None,
        };
        snapshot.worktree_groups = vec![rimz::SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![rimz::SidebarStatusCount {
                status: rimz::feed::AgentStatus::Idle,
                count: 1,
            }],
            rows: vec![row],
            hidden_count: 0,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
        }];
        snapshot
    }

    /// A group whose first row is a multi-line agent card (model, effort, and
    /// context% set so it carries identity + description + gauge, and selecting
    /// it reveals its deeper budget-bar and stats lines), followed by a
    /// single-line process row, with a non-zero hidden count so a `+K more` line
    /// renders. The fixture for the whole-block clickability regression guard.
    fn clickable_block_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
        let mut snapshot = snapshot(ws);
        let agent = rimz::SidebarRow {
            row_kind: rimz::SidebarRowKind::Agent,
            id: "agent-1".to_owned(),
            name: "claude".to_owned(),
            status: Some(rimz::feed::AgentStatus::Running),
            permission_posture: Some(rimz::feed::PermissionPosture::Auto),
            pane: Some(pane("terminal_9", "tab_0", false)),
            request_id: None,
            surface: None,
            task: Some("inspect auth".to_owned()),
            prompt: None,
            model: Some("Opus".to_owned()),
            effort: Some("high".to_owned()),
            context_pct: Some(38),
            total_tokens: Some(12_400),
            todo_done: Some(3),
            todo_total: Some(5),
            context: None,
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            last_activity: Timestamp::now(),
            resolver: None,
            options: Vec::new(),
            sub_agents: Vec::new(),
            process_active: false,
            command_detail: None,
            compacting: false,
            spending: None,
        };
        let process = rimz::SidebarRow {
            row_kind: rimz::SidebarRowKind::Process,
            id: "terminal_10".to_owned(),
            name: "zsh".to_owned(),
            status: None,
            permission_posture: None,
            pane: Some(pane("terminal_10", "tab_0", false)),
            request_id: None,
            surface: None,
            task: None,
            prompt: None,
            model: None,
            effort: None,
            context_pct: None,
            total_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            last_activity: Timestamp::now(),
            resolver: None,
            options: Vec::new(),
            sub_agents: Vec::new(),
            process_active: false,
            command_detail: None,
            compacting: false,
            spending: None,
        };
        snapshot.worktree_groups = vec![rimz::SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![rimz::SidebarStatusCount {
                status: rimz::feed::AgentStatus::Running,
                count: 1,
            }],
            rows: vec![agent, process],
            hidden_count: 2,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
        }];
        snapshot
    }

    /// Health seeded with a live alert, as if a failure already crossed the
    /// debounce threshold — the starting point for recovery/sticky tests.
    fn degraded_health(reason: &str) -> Health {
        Health {
            failure_streak: ALERT_AFTER_FAILURES,
            alert: Some(Alert::active(reason)),
        }
    }

    #[test]
    fn first_ok_fetch_clears_status_and_records_snapshot() {
        let ws = workspace();
        let snap = snapshot(&ws);
        let state = compute_next_state(&ws, None, Ok(snap.clone()), None, &Health::default());
        assert!(state.health.alert.is_none());
        assert_eq!(state.health.failure_streak, 0);
        assert!(state.last_snapshot.is_some());
        assert_eq!(state.snapshot.workspace_id, ws);
    }

    #[test]
    fn single_failure_is_absorbed_without_an_alert() {
        // One flaky tick must not flash a banner: the streak climbs but no
        // alert arms yet, and the last good frame is reused.
        let ws = workspace();
        let previous = snapshot(&ws);
        let state = compute_next_state(
            &ws,
            None,
            Err("ledger not found".to_owned()),
            Some(previous.clone()),
            &Health::default(),
        );
        assert!(state.health.alert.is_none(), "one blip must not alarm");
        assert_eq!(state.health.failure_streak, 1);
        assert!(state.last_snapshot.is_some());
        assert_eq!(state.snapshot.workspace_id, previous.workspace_id);
    }

    #[test]
    fn sustained_failure_raises_active_alert_after_threshold() {
        let ws = workspace();
        let previous = snapshot(&ws);
        let first = compute_next_state(
            &ws,
            None,
            Err("ledger not found".to_owned()),
            Some(previous.clone()),
            &Health::default(),
        );
        let second = compute_next_state(
            &ws,
            None,
            Err("ledger not found".to_owned()),
            first.last_snapshot,
            &first.health,
        );
        let alert = second.health.alert.expect("a sustained failure alerts");
        assert!(alert.is_active());
        assert!(alert.reason.contains("snapshot failed"));
        assert!(alert.reason.contains("ledger not found"));
        assert!(second.last_snapshot.is_some());
    }

    #[test]
    fn sustained_failure_without_previous_snapshot_uses_placeholder() {
        let ws = workspace();
        let err = || Err::<SidebarSnapshot, String>("ledger not found".to_owned());
        let first = compute_next_state(&ws, None, err(), None, &Health::default());
        let second = compute_next_state(&ws, None, err(), None, &first.health);
        assert!(second.health.alert.is_some_and(|alert| alert.is_active()));
        assert!(second.last_snapshot.is_none());
        assert_eq!(second.snapshot.workspace_id, ws);
        assert!(second.snapshot.needs_attention.is_empty());
    }

    #[test]
    fn sustained_heartbeat_failure_alerts_but_keeps_fresh_snapshot() {
        let ws = workspace();
        let snap = snapshot(&ws);
        let first = compute_next_state(
            &ws,
            Some("hb failed".to_owned()),
            Ok(snap.clone()),
            None,
            &Health::default(),
        );
        let second = compute_next_state(
            &ws,
            Some("hb failed".to_owned()),
            Ok(snap.clone()),
            first.last_snapshot,
            &first.health,
        );
        let alert = second
            .health
            .alert
            .expect("sustained heartbeat failure alerts");
        assert!(alert.reason.contains("heartbeat failed"));
        // Heartbeat failing does not invalidate a fresh snapshot.
        assert!(second.last_snapshot.is_some());
    }

    #[test]
    fn active_alert_since_stays_pinned_across_the_episode() {
        let ws = workspace();
        let armed = degraded_health("snapshot failed: first");
        let first_since = armed.alert.as_ref().unwrap().since;
        let next = compute_next_state(
            &ws,
            None,
            Err("second".to_owned()),
            Some(snapshot(&ws)),
            &armed,
        );
        let alert = next.health.alert.expect("still degraded");
        assert_eq!(alert.since, first_since, "since must remain pinned");
        assert!(alert.reason.contains("second"));
    }

    #[test]
    fn recovery_marks_alert_recovered_and_keeps_it_sticky() {
        // Recovery does not erase the alert: it lingers, recovered, until the
        // user dismisses it.
        let ws = workspace();
        let armed = degraded_health("snapshot failed: x");
        let recovered = compute_next_state(&ws, None, Ok(snapshot(&ws)), None, &armed);
        let alert = recovered.health.alert.expect("recovered alert lingers");
        assert!(!alert.is_active());
        assert!(alert.recovered_at.is_some());
        assert_eq!(recovered.health.failure_streak, 0);
    }

    /// Health seeded with an alert whose episode started at `since`. `recovered`
    /// flips it to the sticky-but-inactive (last fetch succeeded) state.
    fn degraded_since(since: Timestamp, recovered: bool) -> Health {
        Health {
            failure_streak: ALERT_AFTER_FAILURES,
            alert: Some(Alert {
                reason: "snapshot failed: boom".to_owned(),
                since,
                recovered_at: recovered.then_some(since),
            }),
        }
    }

    #[test]
    fn gives_up_after_sustained_degradation() {
        let base = 1_700_000_000;
        let since = Timestamp::from_second(base).unwrap();
        let now = Timestamp::from_second(base + GIVE_UP_AFTER_DEGRADED.as_secs() as i64).unwrap();
        assert!(degraded_too_long(&degraded_since(since, false), now));
    }

    #[test]
    fn holds_while_degradation_is_still_brief() {
        // A few seconds of failure must not close the sidebar — that is a hiccup
        // or the sub-second gap while `cargo install` swaps the binary.
        let base = 1_700_000_000;
        let since = Timestamp::from_second(base).unwrap();
        let now = Timestamp::from_second(base + 5).unwrap();
        assert!(!degraded_too_long(&degraded_since(since, false), now));
    }

    #[test]
    fn never_gives_up_once_recovered() {
        // A recovered (sticky but inactive) alert means the latest fetch
        // succeeded: the renderer is healthy and must not exit, however old the
        // past episode is.
        let base = 1_700_000_000;
        let since = Timestamp::from_second(base).unwrap();
        let now = Timestamp::from_second(base + 1_000).unwrap();
        assert!(!degraded_too_long(&degraded_since(since, true), now));
    }

    #[test]
    fn never_gives_up_without_an_alert() {
        let now = Timestamp::from_second(1_700_000_000).unwrap();
        assert!(!degraded_too_long(&Health::default(), now));
    }

    #[test]
    fn strip_deleted_suffix_removes_only_the_kernel_annotation() {
        assert_eq!(
            strip_deleted_suffix(Path::new("/usr/bin/rimz-sidebar (deleted)")),
            Some(PathBuf::from("/usr/bin/rimz-sidebar"))
        );
        // A path the kernel did not annotate is left alone.
        assert_eq!(
            strip_deleted_suffix(Path::new("/usr/bin/rimz-sidebar")),
            None
        );
        // " (deleted)" only counts as a trailing suffix, never mid-path.
        assert_eq!(
            strip_deleted_suffix(Path::new("/opt/my (deleted)/rimz-sidebar")),
            None
        );
    }

    #[test]
    fn reexec_target_resolves_the_replacement_after_an_install() {
        // Post-`cargo install`: the inode behind our `current_exe()` was
        // unlinked, so it reads "<path> (deleted)" while the freshly-installed
        // binary now sits at the un-annotated path — that is what we re-exec.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("rimz-sidebar");
        std::fs::write(&real, b"x").unwrap();
        let deleted = PathBuf::from(format!("{} (deleted)", real.display()));
        assert!(!deleted.is_file(), "the annotated path must not exist");
        assert_eq!(resolve_reexec_target(deleted), Some(real.clone()));
        // The ordinary, not-replaced case uses the live path as-is.
        assert_eq!(resolve_reexec_target(real.clone()), Some(real));
    }

    #[test]
    fn reexec_target_is_none_when_nothing_exists_on_disk() {
        // A partial or in-flight install: neither the annotated nor the
        // stripped path is a file, so the loop keeps serving the current build
        // rather than re-execing into nothing and vanishing.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("rimz-sidebar");
        let deleted = PathBuf::from(format!("{} (deleted)", missing.display()));
        assert_eq!(resolve_reexec_target(deleted), None);
        assert_eq!(resolve_reexec_target(missing), None);
    }

    #[test]
    fn decide_reload_reexecs_only_when_the_on_disk_binary_differs() {
        let target = PathBuf::from("/some/rimz-sidebar");
        // Byte-identical to what we run: skip the re-exec churn.
        assert!(matches!(
            decide_reload(Some(target.clone()), Some(true)),
            ReloadAction::AlreadyCurrent
        ));
        // Content differs: re-exec onto the freshly-installed build.
        assert!(matches!(
            decide_reload(Some(target.clone()), Some(false)),
            ReloadAction::Reexec(t) if t == target
        ));
        // Running image unreadable (non-Linux / IO race): re-exec, preserving
        // the always-load-the-on-disk-build behavior.
        assert!(matches!(
            decide_reload(Some(target.clone()), None),
            ReloadAction::Reexec(t) if t == target
        ));
        // No binary on disk: keep the current build regardless of the compare.
        assert!(matches!(decide_reload(None, None), ReloadAction::Missing));
        assert!(matches!(
            decide_reload(None, Some(true)),
            ReloadAction::Missing
        ));
    }

    #[test]
    fn same_file_contents_detects_byte_equality() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        let identical = dir.path().join("identical");
        let same_len_differs = dir.path().join("same_len_differs");
        let shorter = dir.path().join("shorter");
        std::fs::write(&original, b"freshly-installed build").unwrap();
        std::fs::write(&identical, b"freshly-installed build").unwrap();
        std::fs::write(&same_len_differs, b"freshly-installed BUILD").unwrap();
        std::fs::write(&shorter, b"shorter").unwrap();
        assert!(same_file_contents(&original, &identical).unwrap());
        assert!(!same_file_contents(&original, &same_len_differs).unwrap());
        assert!(!same_file_contents(&original, &shorter).unwrap());
    }

    #[test]
    fn snapshot_bin_uses_the_cached_path_while_it_exists() {
        // The sibling `rimz` captured at launch is still on disk — drive the
        // snapshot with exactly that build, so a dev worktree's changes apply.
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("rimz");
        std::fs::write(&cached, b"x").unwrap();
        assert_eq!(resolve_snapshot_bin(&cached), cached);
    }

    #[test]
    fn snapshot_bin_falls_back_to_path_when_the_cached_binary_vanished() {
        // The dev worktree this sidebar launched from was removed, deleting the
        // sibling `rimz` it cached. Recover via the installed binary on `PATH`
        // rather than forking a path that no longer exists every tick.
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("rimz");
        assert!(!gone.is_file(), "the cached path must not exist");
        assert_eq!(resolve_snapshot_bin(&gone), PathBuf::from("rimz"));
    }

    #[test]
    fn tick_for_honours_above_two_seconds() {
        assert_eq!(tick_for(5), Duration::from_secs(5));
    }

    #[test]
    fn tick_for_clamps_zero_to_one() {
        assert_eq!(tick_for(0), Duration::from_secs(1));
    }

    #[test]
    fn heartbeat_write_due_on_first_or_aged_write_only() {
        assert!(heartbeat_write_due(None));
        assert!(!heartbeat_write_due(Some(Instant::now())));
        assert!(heartbeat_write_due(Some(
            Instant::now() - HEARTBEAT_WRITE_INTERVAL
        )));
    }

    #[test]
    fn fetch_request_sends_immediately_when_idle() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut in_flight = false;
        let mut pending = None;
        let request = FetchRequest::fresh_panes();

        request_fetch(&tx, &mut in_flight, &mut pending, request, true);

        assert!(in_flight);
        assert!(rx.try_recv().unwrap().force_produce);
        assert!(pending.is_none());
    }

    #[test]
    fn fetch_request_preserves_forced_pane_refresh_while_in_flight() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut in_flight = true;
        let mut pending = Some(FetchRequest::default());
        let request = FetchRequest::fresh_panes();
        let min_pane_cache_ms = request.min_pane_cache_ms;

        request_fetch(&tx, &mut in_flight, &mut pending, request, true);

        let pending = pending.expect("pending refetch");
        assert!(pending.force_produce);
        assert_eq!(pending.min_pane_cache_ms, min_pane_cache_ms);
    }

    #[test]
    fn self_close_probe_request_sends_when_idle() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut in_flight = false;
        let mut pending = None;

        request_self_close_probe(&tx, &mut in_flight, &mut pending, Duration::ZERO);

        assert!(in_flight);
        assert_eq!(
            rx.try_recv().unwrap(),
            SelfCloseProbeRequest {
                delay: Duration::ZERO
            }
        );
        assert_eq!(pending, None);
    }

    #[test]
    fn self_close_probe_request_coalesces_to_shortest_pending_delay() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut in_flight = true;
        let mut pending = Some(Duration::from_secs(2));

        request_self_close_probe(&tx, &mut in_flight, &mut pending, Duration::from_millis(50));

        assert!(in_flight);
        assert_eq!(pending, Some(Duration::from_millis(50)));
    }

    #[test]
    fn self_close_probe_outcome_uses_the_existing_latch() {
        let config = ServeConfig {
            workspace_id: workspace(),
            mux: MuxName::Zellij,
            session_name: "rimz-test".to_owned(),
            instance_id: SidebarInstanceId::new(),
            tick_seconds: 2,
            rimz_bin: PathBuf::from("rimz"),
        };
        let mut state = SelfCloseState::default();

        assert!(!apply_self_close_probe_outcome(
            &config,
            SelfCloseProbeOutcome {
                sibling_count: Some(1),
                error: None,
            },
            &mut state,
        ));
        assert!(state.seen_sibling);
        assert!(apply_self_close_probe_outcome(
            &config,
            SelfCloseProbeOutcome {
                sibling_count: Some(0),
                error: None,
            },
            &mut state,
        ));
    }

    #[test]
    fn self_close_waits_for_a_sibling_before_ever_closing() {
        let mut state = SelfCloseState::default();
        // Startup: no sibling yet (terminal pane not materialized). Give Zellij
        // one observation to finish materializing the sibling.
        assert!(!self_close_decision(&mut state, Some(0)));
        assert!(!state.seen_sibling);
    }

    #[test]
    fn self_close_fires_when_a_sibling_never_appears() {
        let mut state = SelfCloseState::default();
        assert!(!self_close_decision(&mut state, Some(0)));
        assert!(self_close_decision(&mut state, Some(0)));
    }

    #[test]
    fn self_close_latches_then_fires_when_alone() {
        let mut state = SelfCloseState::default();
        assert!(!self_close_decision(&mut state, Some(1)));
        assert!(state.seen_sibling, "seeing a sibling must latch");
        // Sibling went away: now alone, so close.
        assert!(self_close_decision(&mut state, Some(0)));
    }

    #[test]
    fn self_close_holds_while_siblings_remain() {
        let mut state = SelfCloseState {
            seen_sibling: true,
            empty_startup_observations: 0,
        };
        assert!(!self_close_decision(&mut state, Some(2)));
    }

    #[test]
    fn self_close_never_fires_on_unknown_count() {
        let mut state = SelfCloseState {
            seen_sibling: true,
            empty_startup_observations: 0,
        };
        assert!(!self_close_decision(&mut state, None));
        assert!(
            state.seen_sibling,
            "an unknown count must not clear the latch"
        );
    }

    #[test]
    fn session_exit_holds_at_birth_before_a_working_view() {
        // The `rimzd` tab is born first, so "only the daemon view" is birth, not
        // teardown: hold, and do not latch.
        let mut state = SessionExitState::default();
        assert!(!state.should_detach(Some(true)));
        assert!(!state.seen_other_view);
    }

    #[test]
    fn session_exit_latches_then_detaches_when_the_room_empties() {
        let mut state = SessionExitState::default();
        assert!(!state.should_detach(Some(false))); // a working view appears → latch
        assert!(state.seen_other_view);
        assert!(state.should_detach(Some(true))); // it closed → only the daemon view → detach
    }

    #[test]
    fn session_exit_holds_while_a_working_view_exists() {
        let mut state = SessionExitState {
            seen_other_view: true,
            fired: false,
        };
        assert!(!state.should_detach(Some(false)));
    }

    #[test]
    fn session_exit_never_fires_on_none() {
        let mut state = SessionExitState {
            seen_other_view: true,
            fired: false,
        };
        assert!(!state.should_detach(None));
        assert!(
            state.seen_other_view,
            "an unknown signal must not clear the latch"
        );
    }

    #[test]
    fn session_exit_fires_exactly_once() {
        let mut state = SessionExitState {
            seen_other_view: true,
            fired: false,
        };
        assert!(state.should_detach(Some(true)));
        assert!(
            !state.should_detach(Some(true)),
            "a second tick must not re-detach"
        );
    }

    /// A timestamp `secs` seconds past a fixed epoch, for ordering selections.
    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(2_000_000_000 + secs).unwrap()
    }

    #[test]
    fn cold_start_adopts_the_first_valid_external_focus() {
        // No local selection and no prior external focus: the first valid report
        // seeds both `external_focus` and the highlight.
        let ws = workspace();
        let focused = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", true),
            ],
        );
        let mut ui = UiState::default();

        reconcile_selection(&mut ui, &snapshot, Some((focused.clone(), ts(0))));

        assert_eq!(ui.selected_index, 1);
        assert_eq!(ui.selected_pane, Some(focused.clone()));
        assert_eq!(ui.external_focus, Some((focused, ts(0))));
    }

    #[test]
    fn external_focus_newer_than_the_click_is_adopted() {
        // A genuine external focus move stamped after the local click wins by
        // timestamp and moves the highlight.
        let ws = workspace();
        let clicked = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let moved = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", true),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            selected_pane: Some(clicked.clone()),
            local_selection: Some((clicked, ts(1))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, Some((moved.clone(), ts(2))));

        assert_eq!(ui.selected_index, 1);
        assert_eq!(ui.selected_pane, Some(moved.clone()));
        assert_eq!(ui.external_focus, Some((moved, ts(2))));
    }

    #[test]
    fn click_newer_than_a_lagging_from_pane_report_holds() {
        // The rollback case, stale-*after* the jump. A click pinned terminal_2 at
        // ts(2); terminal_1 is the pane it jumped from, last trusted at ts(0). A
        // lagging re-report of that from-pane lands *after* the click (ts(3)) —
        // newer, but the same pane, so the identity guard reads it as steady
        // state, not a move. `external_focus` does not refresh and the click
        // holds.
        let ws = workspace();
        let clicked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let from_pane = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(clicked.clone()),
            local_selection: Some((clicked.clone(), ts(2))),
            external_focus: Some((from_pane.clone(), ts(0))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, Some((from_pane.clone(), ts(3))));

        assert_eq!(ui.selected_index, 1, "held on the clicked pane");
        assert_eq!(ui.selected_pane, Some(clicked));
        assert_eq!(
            ui.external_focus,
            Some((from_pane, ts(0))),
            "a lagging re-report of the from-pane never refreshes external_focus"
        );
    }

    #[test]
    fn cross_tab_click_holds_against_a_repeated_stale_focus() {
        // A click on a pane in another tab pins terminal_3 at ts(2). The producer
        // keeps reporting the from-tab's focus (terminal_1) at ts(1); the first
        // adopts into external_focus, every repeat is steady-state (identity
        // guard), and the newer local pick wins every fold — no rollback.
        let ws = workspace();
        let clicked = PaneId::from_parts(MuxName::Zellij, "terminal_3");
        let own_tab_focus = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", true),
                pane("terminal_2", "tab_0", false),
                pane("terminal_3", "tab_1", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 2,
            selected_pane: Some(clicked.clone()),
            local_selection: Some((clicked.clone(), ts(2))),
            ..Default::default()
        };

        for _ in 0..3 {
            reconcile_selection(&mut ui, &snapshot, Some((own_tab_focus.clone(), ts(1))));
        }

        assert_eq!(ui.selected_index, 2, "held on the cross-tab clicked pane");
        assert_eq!(ui.selected_pane, Some(clicked));
    }

    #[test]
    fn sidebar_self_or_unknown_focus_is_inert() {
        // Focus on the sidebar itself (or an undiscoverable focus) arrives as
        // `None`. It leaves `external_focus` untouched, so the local click holds.
        let ws = workspace();
        let clicked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let baseline = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(clicked.clone()),
            local_selection: Some((clicked.clone(), ts(2))),
            external_focus: Some((baseline.clone(), ts(0))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, None);

        assert_eq!(ui.selected_index, 1);
        assert_eq!(ui.selected_pane, Some(clicked));
        assert_eq!(
            ui.external_focus,
            Some((baseline, ts(0))),
            "an inert report must not touch external_focus"
        );
    }

    #[test]
    fn focus_on_a_non_row_helper_pane_is_inert() {
        // Zellij can focus a non-agent helper pane (`claude rc`, `codex
        // app-server`) that the sidebar never renders as a row. Such a focus is
        // not a jump target: it leaves external_focus untouched, so a fresh local
        // click holds even though the helper focus is newer.
        let ws = workspace();
        let clicked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let helper = PaneId::from_parts(MuxName::Zellij, "terminal_99");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(clicked.clone()),
            local_selection: Some((clicked.clone(), ts(1))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, Some((helper, ts(2))));

        assert_eq!(ui.selected_index, 1);
        assert_eq!(ui.selected_pane, Some(clicked));
        assert_eq!(ui.external_focus, None, "a non-row focus never adopts");
    }

    #[test]
    fn external_move_to_a_third_pane_newer_than_the_click_adopts() {
        // From an established external_focus (terminal_1), a genuine move to a
        // different row (terminal_3) stamped after the click is adopted — the
        // identity guard passes (different pane) and the timestamp wins.
        let ws = workspace();
        let clicked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let from = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let third = PaneId::from_parts(MuxName::Zellij, "terminal_3");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
                pane("terminal_3", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(clicked.clone()),
            local_selection: Some((clicked, ts(1))),
            external_focus: Some((from, ts(0))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, Some((third.clone(), ts(2))));

        assert_eq!(ui.selected_index, 2);
        assert_eq!(ui.selected_pane, Some(third.clone()));
        assert_eq!(ui.external_focus, Some((third, ts(2))));
    }

    #[test]
    fn monotonic_guard_ignores_a_reordered_older_sample() {
        // external_focus was last trusted on terminal_2 at ts(2). A focus sample
        // for a different pane (terminal_1) arrives reordered with an *older*
        // stamp (ts(1)); the monotonic guard rejects it, so the highlight holds.
        let ws = workspace();
        let held = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let older = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(held.clone()),
            external_focus: Some((held.clone(), ts(2))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, Some((older, ts(1))));

        assert_eq!(ui.selected_index, 1);
        assert_eq!(ui.selected_pane, Some(held.clone()));
        assert_eq!(
            ui.external_focus,
            Some((held, ts(2))),
            "a reordered older sample never refreshes external_focus"
        );
    }

    #[test]
    fn selection_reanchors_to_its_pane_after_a_reorder() {
        // terminal_2 moved from row 1 to row 0 between folds with no focus edge;
        // the highlight follows its pane, not the old index.
        let ws = workspace();
        let clicked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let focus = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_2", "tab_0", false),
                pane("terminal_1", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(clicked.clone()),
            local_selection: Some((clicked.clone(), ts(2))),
            external_focus: Some((focus.clone(), ts(0))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, Some((focus, ts(0))));

        assert_eq!(ui.selected_index, 0, "re-anchored to the pane's new row");
        assert_eq!(ui.selected_pane, Some(clicked));
    }

    #[test]
    fn selection_drops_when_its_pane_leaves_the_room() {
        // The locally-selected pane is gone from the snapshot: drop the dangling
        // identity and clamp, so the next selection can re-seat it.
        let ws = workspace();
        let gone = PaneId::from_parts(MuxName::Zellij, "terminal_9");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(gone.clone()),
            local_selection: Some((gone, ts(2))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, None);

        assert_eq!(ui.selected_pane, None, "dangling identity dropped");
        assert_eq!(ui.local_selection, None, "absent local selection cleared");
        assert!(ui.selected_index < 2, "clamped to a valid row");
    }

    /// Lay out `snapshot` at a generous size through the real render path,
    /// returning the freshly-composed hit-test map — the same map the live draw
    /// stores on `UiState`. Width/height are wide and tall enough that nothing
    /// the tests probe is clipped.
    fn line_map_for(snapshot: &SidebarSnapshot, selected: usize) -> Vec<Option<usize>> {
        let ui = UiState {
            selected_index: selected,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        };
        let (_lines, map) = render::compose_lines(snapshot, None, &ui, 54, 64);
        map
    }

    /// The screen row a content-line index maps to: borderless, the body fills
    /// the frame from row 0, so map index `i` is screen row `i`.
    fn screen_row_for(map_index: usize) -> u16 {
        u16::try_from(map_index).unwrap()
    }

    #[test]
    fn row_index_maps_process_row_screen_positions() {
        let ws = workspace();
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let ui = UiState {
            line_map: line_map_for(&snapshot, 0),
            ..UiState::default()
        };

        // The worktree header is the first line that routes to row 0 — clicking
        // the pod name jumps into its first row — and the first process row
        // follows directly beneath it. Both route to row 0.
        let header = ui.line_map.iter().position(|m| *m == Some(0)).unwrap();
        let row0 = header + 1;
        let row1 = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();
        assert_eq!(
            ui.line_map[row0],
            Some(0),
            "the first process row follows its worktree header"
        );

        // The borderless title line at screen row 0 is inert chrome.
        assert_eq!(
            row_index_at_screen_position(&ui, 0),
            None,
            "the title line is not clickable content"
        );
        assert_eq!(
            row_index_at_screen_position(&ui, screen_row_for(header)),
            Some(0),
            "the worktree header jumps into its first row"
        );
        assert_eq!(
            row_index_at_screen_position(&ui, screen_row_for(row0)),
            Some(0)
        );
        assert_eq!(
            row_index_at_screen_position(&ui, screen_row_for(row1)),
            Some(1)
        );
        // The line just above the worktree header is the section gap — inert.
        assert_eq!(
            row_index_at_screen_position(&ui, screen_row_for(header - 1)),
            None,
            "the section gap is not a row"
        );
    }

    #[test]
    fn every_line_of_an_agent_block_routes_to_that_agent() {
        // The user-visible contract: the whole multi-line agent card is one
        // click target, the worktree header that jumps into it routes there too,
        // the gaps and `+K more` are inert, and a process row's single line
        // routes to its own index.
        let ws = workspace();
        let snapshot = clickable_block_snapshot(&ws);
        // Select the agent so its deeper stats lines appear too.
        let map = line_map_for(&snapshot, 0);

        // Index 0 is the agent (a multi-line card) plus the worktree header that
        // jumps into it; index 1 is the process row.
        let agent_lines = map.iter().filter(|m| **m == Some(0)).count();
        assert!(
            agent_lines >= 4,
            "the worktree header plus the selected agent card (identity + \
             description + gauge + stats) route to row 0, not {agent_lines} lines",
        );
        let process_lines = map.iter().filter(|m| **m == Some(1)).count();
        assert_eq!(process_lines, 1, "a process row is a single line");

        // No content line of the agent block is missed: every map slot routes
        // through the hit-test to exactly the row it was tagged with.
        let ui = UiState {
            line_map: map.clone(),
            ..UiState::default()
        };
        for (i, entry) in map.iter().enumerate() {
            let got = row_index_at_screen_position(&ui, screen_row_for(i));
            assert_eq!(got, *entry, "screen row {i} mismatched its map slot");
        }

        // The cockpit header, gaps, and the `+K more` hidden-count line are inert.
        assert!(
            map.contains(&None),
            "cockpit header / gaps / +K more stay inert"
        );
    }

    #[test]
    fn mouse_click_selects_clicked_row() {
        let ws = workspace();
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: line_map_for(&snapshot, 0),
            ..Default::default()
        };
        let row1 = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();

        let outcome = handle_mouse_click(1, screen_row_for(row1), &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::focus(1));
        assert_eq!(ui.selected_index, 1);
    }

    #[test]
    fn arrow_key_reports_immediate_ui_change() {
        let ws = workspace();
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        };

        let outcome = handle_key(KeyAction::Down, &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::redraw());
        assert_eq!(ui.selected_index, 1);
    }

    #[test]
    fn dismiss_key_requests_alert_dismissal() {
        let ws = workspace();
        let snapshot = snapshot_with_panes(&ws, vec![pane("terminal_1", "tab_0", false)]);
        let mut ui = UiState::default();

        let outcome = handle_key(KeyAction::Dismiss, &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::dismiss());
        assert!(outcome.dismiss);
        assert!(outcome.redraw);
        // Dismiss never moves the selection.
        assert_eq!(ui.selected_index, 0);
    }

    #[test]
    fn enter_reports_focus_after_highlight_redraw() {
        let ws = workspace();
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        };

        let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::focus(1));
        assert_eq!(ui.selected_index, 1);
    }

    // ---- last-known-good commit gate -------------------------------------

    fn gate_now() -> Timestamp {
        Timestamp::from_second(1_700_000_000).unwrap()
    }

    /// A snapshot whose single pane renders as a bare process row.
    fn process_on(ws: &WorkspaceId, raw: &str) -> SidebarSnapshot {
        snapshot_with_panes(ws, vec![pane(raw, "tab_0", false)])
    }

    #[test]
    fn gate_accepts_first_frame_against_placeholder() {
        let ws = workspace();
        // The placeholder prev has no panes; the first real frame is never a
        // regression to hold.
        assert_eq!(
            gate_commit(
                &snapshot(&ws),
                &agent_snapshot(&ws),
                &GateState::default(),
                gate_now()
            ),
            CommitDecision::Accept
        );
    }

    #[test]
    fn gate_holds_transient_agent_to_process_demotion() {
        let ws = workspace();
        // Same pane set {terminal_9}, but the agent row became a bare process —
        // the phantom flicker. Held until the escape hatch opens.
        assert_eq!(
            gate_commit(
                &agent_snapshot(&ws),
                &process_on(&ws, "terminal_9"),
                &GateState::default(),
                gate_now()
            ),
            CommitDecision::KeepPrior
        );
    }

    #[test]
    fn gate_releases_demotion_after_reject_count() {
        let ws = workspace();
        let gate = GateState {
            reject_streak: ACCEPT_REGRESSION_AFTER_REJECTS,
            rejecting_since: Some(gate_now()),
        };
        assert_eq!(
            gate_commit(
                &agent_snapshot(&ws),
                &process_on(&ws, "terminal_9"),
                &gate,
                gate_now()
            ),
            CommitDecision::Accept,
            "a stuck demotion must surface, not freeze forever"
        );
    }

    #[test]
    fn gate_releases_demotion_after_timeout_but_holds_while_brief() {
        let ws = workspace();
        let base = 1_700_000_000;
        let gate = GateState {
            reject_streak: 1,
            rejecting_since: Some(Timestamp::from_second(base).unwrap()),
        };
        let ceiling = ACCEPT_REGRESSION_AFTER.as_secs() as i64;
        // Still brief: held.
        assert_eq!(
            gate_commit(
                &agent_snapshot(&ws),
                &process_on(&ws, "terminal_9"),
                &gate,
                Timestamp::from_second(base + ceiling - 1).unwrap()
            ),
            CommitDecision::KeepPrior
        );
        // Past the ceiling: released.
        assert_eq!(
            gate_commit(
                &agent_snapshot(&ws),
                &process_on(&ws, "terminal_9"),
                &gate,
                Timestamp::from_second(base + ceiling).unwrap()
            ),
            CommitDecision::Accept
        );
    }

    #[test]
    fn gate_accepts_when_the_panel_set_changes() {
        let ws = workspace();
        // A pane closed (the demotion is on a different id): the room genuinely
        // changed, so accept rather than hold against a stale baseline.
        assert_eq!(
            gate_commit(
                &agent_snapshot(&ws),
                &process_on(&ws, "terminal_8"),
                &GateState::default(),
                gate_now()
            ),
            CommitDecision::Accept
        );
    }

    #[test]
    fn gate_accepts_a_non_regression() {
        let ws = workspace();
        assert_eq!(
            gate_commit(
                &agent_snapshot(&ws),
                &agent_snapshot(&ws),
                &GateState::default(),
                gate_now()
            ),
            CommitDecision::Accept
        );
    }

    #[test]
    fn reject_holds_prior_frame_as_render_and_baseline() {
        let ws = workspace();
        let prior = agent_snapshot(&ws);
        // A fresh fetch that demoted the agent on terminal_9 to a process row.
        let computed = compute_next_state(
            &ws,
            None,
            Ok(process_on(&ws, "terminal_9")),
            Some(prior.clone()),
            &Health::default(),
        );
        let (state, gate, rejected) =
            apply_gate(computed, true, &prior, &GateState::default(), gate_now());
        assert!(rejected);
        // Both the rendered frame AND the next-tick baseline stay the good
        // frame, so the cache never advances onto the demotion.
        assert!(matches!(
            state.snapshot.worktree_groups[0].rows[0].row_kind,
            rimz::SidebarRowKind::Agent
        ));
        let baseline = state.last_snapshot.expect("baseline retained");
        assert!(matches!(
            baseline.worktree_groups[0].rows[0].row_kind,
            rimz::SidebarRowKind::Agent
        ));
        assert_eq!(gate.reject_streak, 1);
        assert!(gate.rejecting_since.is_some());
        // Orthogonal to Health: a held regression is a *successful* fetch, so it
        // never arms the degraded alert nor counts toward self-close.
        assert!(state.health.alert.is_none());
        assert_eq!(state.health.failure_streak, 0);
    }

    #[test]
    fn accept_resets_the_gate() {
        let ws = workspace();
        let prior = agent_snapshot(&ws);
        let computed = compute_next_state(
            &ws,
            None,
            Ok(agent_snapshot(&ws)),
            Some(prior.clone()),
            &Health::default(),
        );
        // Carry a prior reject episode in; a clean accept clears it.
        let prev_gate = GateState {
            reject_streak: 2,
            rejecting_since: Some(gate_now()),
        };
        let (state, gate, rejected) = apply_gate(computed, true, &prior, &prev_gate, gate_now());
        assert!(!rejected);
        assert_eq!(gate, GateState::default());
        assert!(matches!(
            state.snapshot.worktree_groups[0].rows[0].row_kind,
            rimz::SidebarRowKind::Agent
        ));
    }
}
