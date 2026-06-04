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

use crate::render::{self, Alert, Browse, UiState};

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

    // The snapshot subprocess (workspace resolve + `list-panes` + git) runs on
    // a background worker, so animation and input never block on it. The worker
    // posts `SNAPSHOT_WAKEUP` when a result is ready; `in_flight`/
    // `pending_refetch` coalesce requests so a ledger-delta storm or a slow
    // fetch can never queue more than one extra run.
    let (request_tx, request_rx) = std::sync::mpsc::channel::<FetchRequest>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<FetchOutcome>();
    // `JoinHandle` drops without blocking: the thread runs to completion on its
    // own when `request_tx` is dropped at function exit.
    let _fetch_handle = spawn_fetch_worker(
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
    let _probe_handle = spawn_self_close_probe_worker(
        config.clone(),
        socket_path.clone(),
        self_close_probe_rx,
        self_close_result_tx,
    );
    let mut self_close_probe_in_flight = false;
    let mut pending_self_close_probe: Option<Duration> = None;

    // Write the heartbeat immediately so the freshness gate never sees a gap.
    // Errors are non-fatal; the gate re-probes after the TTL.
    if let Err(err) = write_heartbeat(&config, &runtime, &socket_path) {
        warn!(
            session = %config.session_name,
            error = %err,
            "initial heartbeat write failed",
        );
    }

    // Fire the first fetch on the background worker and start the main loop
    // immediately rather than blocking on a synchronous call: the first fetch
    // can take several seconds (Zellij just started, git cold-start), and a
    // blocked main thread delays the self-close watchdog, stalling cleanup.
    // The placeholder snapshot renders while the first real result is in flight.
    let mut fetched_at = Instant::now();
    let mut should_exit = false;
    request_fetch(
        &request_tx,
        &mut in_flight,
        &mut pending_refetch,
        FetchRequest::default(),
        false,
    );

    // One fixed-timestep event loop. Events fold into the in-process model and
    // mark the frame dirty; the loop paints at most once per `ANIMATION_FRAME`
    // boundary, coalescing every change that landed mid-frame into a single
    // paint. Data and animation ride this frame grid; input paints synchronously
    // for instant feedback (see `apply_input`). The grid runs at `ANIMATION_FRAME`
    // while there is something to show (`active`) and relaxes to the `tick`
    // backstop when idle, snapping back the instant an event or animation
    // arrives. The loop blocks only in `recv`, so no path forks a subprocess on
    // the render thread and a busy fetch never freezes the spin or swallows a
    // keypress.
    let mut dirty = true;
    let mut next_frame = Instant::now();
    let mut last_self_close_check = Instant::now();
    // The sidebar's own pane width as of the last resize the loop processed. A
    // resize that grows it is the precondition for the self-close full-width
    // flash, so a grow holds its repaint (`paint_held`) until the sibling-count
    // verdict lands — close exits without painting, stay paints at the new size.
    let mut prev_width: Option<u16> = terminal.size().map(|s| s.width).ok();
    let mut paint_held = false;
    // Heartbeat writes live on the main thread (fast in-process atomic writes)
    // so the exit path can remove the file without racing a background writer.
    let mut last_heartbeat: Option<Instant> = None;
    while !should_exit {
        // A live row's spinner or a value-corner count-up keeps the frame grid
        // warm; a pending fold (`dirty`) does too. With neither, drop to the
        // slow data backstop until the next wakeup re-arms the grid.
        let phase = wall_clock_phase(anim_start);
        let animating = render::has_live_animation(&current) || ui.tally.any_rolling(phase);
        let active = animating || dirty;
        let timeout = if active {
            next_frame
                .saturating_duration_since(Instant::now())
                .max(FRAME_MIN_TIMEOUT)
        } else {
            // Cap by the watchdog so the self-close backstop fires on time even
            // when the data tick is much longer.
            let watchdog_due = SELF_CLOSE_WATCHDOG.saturating_sub(last_self_close_check.elapsed());
            tick.min(watchdog_due).max(FRAME_MIN_TIMEOUT)
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
                    let snapshot_ok = outcome.snapshot.is_ok();
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
                        anim_start,
                    )?;
                    should_exit = applied.should_exit;
                    rejected = applied.rejected;
                    if snapshot_ok {
                        last_self_close_check = Instant::now();
                    }
                    // The fold mutated the model; the frame phase paints it.
                    dirty = true;
                    if !should_exit {
                        // The snapshot carried a stay verdict (sibling count > 0,
                        // or unknowable): release any held grow-repaint so the
                        // frame phase paints it at the new size.
                        paint_held = false;
                    }
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
                if !should_exit {
                    // Stay/unknown verdict: a held grow-repaint is now safe to show.
                    paint_held = false;
                    if let Some(delay) = pending_self_close_probe.take() {
                        request_self_close_probe(
                            &self_close_probe_tx,
                            &mut self_close_probe_in_flight,
                            &mut pending_self_close_probe,
                            delay,
                        );
                    }
                }
            }
            // A recv timeout: the active grid reached a frame boundary, or the
            // idle backstop interval elapsed. It carries no state of its own —
            // the frame phase below advances the spin and paints, and the
            // backstop poll runs there too.
            Wakeup::Tick => {}
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
                // A grow is the mux handing the sidebar a freed sibling's space —
                // the precondition for the self-close full-width flash. Hold the
                // paint until the sibling-count verdict the probe and fetch below
                // request: a "close" verdict exits without ever painting the grown
                // frame (the frame phase guards on `should_exit`); a "stay" verdict
                // releases the hold and paints at the new size. A shrink, a
                // same-width resize, or an unreadable size cannot flash, so each
                // keeps the instant repaint for snappy attach/redraw feedback.
                let grew = match terminal.size().map(|s| s.width).ok() {
                    Some(width) => {
                        let grew = resize_grew(prev_width, width);
                        prev_width = Some(width);
                        grew
                    }
                    None => false,
                };
                if grew {
                    dirty = true;
                    paint_held = true;
                } else {
                    if apply_input(
                        Wakeup::Resize,
                        &mut ui,
                        &mut health,
                        &mut terminal,
                        &current,
                        anim_start,
                    )? {
                        dirty = false;
                    }
                    // A safe-width paint just landed; drop any stale hold a prior
                    // grow left pending so it cannot suppress this frame.
                    paint_held = false;
                }
                request_self_close_probe(
                    &self_close_probe_tx,
                    &mut self_close_probe_in_flight,
                    &mut pending_self_close_probe,
                    Duration::ZERO,
                );
                last_self_close_check = Instant::now();
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
                // Key/mouse input paints synchronously for instant feedback; a
                // paint settles any frame the loop owed.
                if apply_input(
                    wakeup,
                    &mut ui,
                    &mut health,
                    &mut terminal,
                    &current,
                    anim_start,
                )? {
                    dirty = false;
                }
            }
        }

        // Data backstop: catch pane/git drift no ledger delta announced. Self-
        // gated to the `tick` interval and a no-op while a fetch is in flight, so
        // it neither double-fires nor rides the frame grid (the removed
        // ACTIVE_REFRESH anti-pattern). Runs once per loop turn regardless of why
        // we woke.
        if fetched_at.elapsed() >= tick {
            request_fetch(
                &request_tx,
                &mut in_flight,
                &mut pending_refetch,
                FetchRequest::default(),
                false,
            );
        }

        // Heartbeat: fast in-process atomic write on the main thread so the
        // exit path (drop _heartbeat_cleanup) never races a background writer.
        if heartbeat_write_due(last_heartbeat) {
            last_heartbeat = Some(Instant::now());
            if let Err(err) = write_heartbeat(&config, &runtime, &socket_path) {
                warn!(
                    session = %config.session_name,
                    error = %err,
                    "heartbeat write failed",
                );
            }
        }

        // Self-close watchdog: if no resize event fired (e.g. background sessions
        // where the mux omits SIGWINCH after a pane closes), ask the normal
        // snapshot path to refresh so the snapshot's own-view count can close a
        // lone sidebar. This preserves the one-producer bound: consumers read
        // the shared pane cache in process instead of each forking `list-panes`.
        // The resize path remains the fast lane and still runs the metadata-only
        // probe for the full-width-flash guard.
        if last_self_close_check.elapsed() >= SELF_CLOSE_WATCHDOG {
            last_self_close_check = Instant::now();
            request_fetch(
                &request_tx,
                &mut in_flight,
                &mut pending_refetch,
                FetchRequest::default(),
                false,
            );
        }

        // Frame phase: at the boundary, advance the spin and paint once, folding
        // every change that landed this frame into a single draw. Paint when the
        // model changed (`dirty`) or a row is animating; an idle frame is a bare
        // timer wake with no recompose. While idle, keep the grid armed so the
        // next event paints within one `ANIMATION_FRAME`.
        let now = Instant::now();
        if dirty {
            let dirty_deadline = now + ANIMATION_FRAME;
            if next_frame > dirty_deadline {
                next_frame = dirty_deadline;
            }
        }
        // `!should_exit`: once the tab has emptied, never paint again — this is
        // what stops the last frame from flashing at the grown/full width on the
        // way out. `!paint_held`: a grow resize defers its paint until the
        // sibling-count verdict releases the hold (see the resize handler).
        if !should_exit && !paint_held && active && now >= next_frame {
            ui.animation_phase = wall_clock_phase(anim_start);
            let animating = render::animation_cadence(&current) != render::AnimationCadence::None
                || ui.tally.any_rolling(ui.animation_phase);
            if dirty || animating {
                render::draw_to_terminal_with_ui(
                    &mut terminal,
                    &current,
                    health.alert.as_ref(),
                    &mut ui,
                )?;
                dirty = false;
            }
            next_frame = next_frame_after(next_frame, now, frame_interval(&current, &ui));
        } else if !active {
            next_frame = now + ANIMATION_FRAME;
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

/// A resize that grows the pane width is the necessary precondition for the
/// self-close full-width flash: the mux handed the sidebar a freed sibling's
/// space. An unknown previous width (the first resize) counts as a grow so the
/// cautious held path is taken.
fn resize_grew(prev: Option<u16>, new: u16) -> bool {
    prev.is_none_or(|p| new > p)
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
/// Watchdog interval for the self-close backstop: when no resize event arrives
/// (e.g. background Zellij sessions that omit SIGWINCH after a pane closes),
/// this asks the normal snapshot path for a fresh own-view count. Sized at 2s
/// so cleanup stays prompt even when a caller configured a much slower data tick.
const SELF_CLOSE_WATCHDOG: Duration = Duration::from_secs(2);
/// Maximum time the self-close probe spends waiting for the mux backend's
/// `list-panes` subprocess. Shorter than the default 30s backend timeout so
/// a hung Zellij does not pin the sidebar open indefinitely. Resize probes are
/// the fast path for the full-width-flash guard; the periodic backstop uses the
/// shared snapshot fetch instead.
const PROBE_COMMAND_TIMEOUT: Duration = Duration::from_secs(4);

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

/// Slow cosmetic animation tick for attention breathing and empty-idle loading
/// dots. These visual states already hold the same rendered frame for several
/// base phases, so redrawing them at 10fps wastes CPU in an idle or blocked room.
const SLOW_ANIMATION_FRAME: Duration = Duration::from_millis(300);

/// Floor for the frame-boundary recv timeout. When the loop is at or past the
/// next frame boundary, the time-to-boundary is zero; a 1ms floor lets an
/// already-queued datagram drain on this turn without a zero-timeout busy spin.
const FRAME_MIN_TIMEOUT: Duration = Duration::from_millis(1);

/// The animation frame index for `now`, derived from elapsed wall-clock since
/// the serve loop's monotonic base. Every redraw path sets the phase from this,
/// so the spin advances on real time and survives re-fetches and ledger deltas
/// without a per-tick counter that a break-and-refetch could reset.
fn wall_clock_phase(start: Instant) -> u64 {
    (start.elapsed().as_millis() / ANIMATION_FRAME.as_millis()) as u64
}

fn frame_interval(snapshot: &SidebarSnapshot, ui: &UiState) -> Duration {
    if ui.tally.any_rolling(ui.animation_phase) {
        return ANIMATION_FRAME;
    }
    match render::animation_cadence(snapshot) {
        render::AnimationCadence::Fast => ANIMATION_FRAME,
        render::AnimationCadence::Slow => SLOW_ANIMATION_FRAME,
        render::AnimationCadence::None => ANIMATION_FRAME,
    }
}

fn tick_for(seconds: u64) -> Duration {
    Duration::from_secs(seconds.max(1))
}

/// The next frame boundary after painting the frame scheduled for `scheduled`
/// at wall-clock `now`. The grid normally advances by exactly one `frame`, so
/// paints hold a fixed cadence regardless of how long a paint took. When the
/// loop has fallen a full frame or more behind — a slow paint or a scheduler
/// hiccup — it snaps onto the boundary one `frame` ahead of `now` rather than
/// replaying every missed boundary, so a backlog can never spiral into a burst
/// of catch-up paints.
fn next_frame_after(scheduled: Instant, now: Instant, frame: Duration) -> Instant {
    let advanced = scheduled + frame;
    if advanced <= now {
        now + frame
    } else {
        advanced
    }
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
        wired_lazy_kinds: Vec::new(),
        own_view: None,
        only_daemon_view_remains: false,
        project_root: None,
        worktree_roots: Vec::new(),
        sidebar: rimz::config::SidebarConfig::default(),
        providers: Vec::new(),
        value_tally: None,
        reflects_log: None,
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

/// One refresh cycle's result: the snapshot fetch outcome.
struct FetchOutcome {
    snapshot: std::result::Result<SidebarSnapshot, String>,
}

/// Fetch the snapshot. Runs on the fetch worker thread (and once inline for the
/// first frame), keeping the producer's `list-panes` + git round-trip off the
/// render/input loop so animation never stalls on it.
///
/// One producer per workspace, one renderer per tab. The eldest live instance
/// is the producer: it forks `rimz sidebar snapshot` (`list-panes`/git) and
/// publishes the shared cache. Every younger instance is a consumer — it reads
/// that published frame **in process** ([`rimz::sidebar::snapshot::read_published_snapshot`]),
/// folding only its own-pane exclusion, so it never forks a subprocess, never
/// runs `list-panes`/git, and never exits — a per-tab renderer stays alive and
/// paints. The mux/git round-trip is paid once per workspace; a consumer with no
/// published frame yet reports a soft miss so the gate holds its last good frame.
fn run_fetch(config: &ServeConfig, runtime: &RuntimePaths, request: FetchRequest) -> FetchOutcome {
    let is_producer = !rimz::sidebar::elder_sidebar_present(runtime, &config.instance_id);
    let exclude = rimz::mux::own_pane_id(config.mux);
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
    FetchOutcome { snapshot }
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
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let waker = UnixDatagram::unbound().ok();
        while let Ok(first) = request_rx.recv() {
            // Coalesce any requests that piled up into one run, keeping the
            // strongest intent and the newest pane-freshness floor.
            let mut request = first;
            while let Ok(extra) = request_rx.try_recv() {
                request.merge(extra);
            }
            let outcome = run_fetch(&config, &runtime, request);
            if result_tx.send(outcome).is_err() {
                return;
            }
            if let Some(waker) = &waker {
                let _ = waker.send_to(SNAPSHOT_WAKEUP, &socket_path);
            }
        }
    })
}

fn heartbeat_write_due(last_heartbeat: Option<Instant>) -> bool {
    last_heartbeat.is_none_or(|last| last.elapsed() >= HEARTBEAT_WRITE_INTERVAL)
}

fn spawn_self_close_probe_worker(
    config: ServeConfig,
    socket_path: PathBuf,
    request_rx: std::sync::mpsc::Receiver<SelfCloseProbeRequest>,
    result_tx: std::sync::mpsc::Sender<SelfCloseProbeOutcome>,
) -> std::thread::JoinHandle<()> {
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
    })
}

fn run_self_close_probe(config: &ServeConfig) -> SelfCloseProbeOutcome {
    let Some(own) = rimz::mux::own_pane_id(config.mux) else {
        return SelfCloseProbeOutcome {
            sibling_count: None,
            error: None,
        };
    };
    match rimz::mux::backend_for(config.mux).list_panes(PaneListOptions {
        session_name: Some(config.session_name.clone()),
        command_timeout: Some(PROBE_COMMAND_TIMEOUT),
    }) {
        Ok(panes) => SelfCloseProbeOutcome {
            // This probe reads only `sibling_count`.
            sibling_count: rimz::SidebarOwnView::from_panes(&own, &panes)
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
    anim_start: Instant,
) -> Result<ApplyOutcome> {
    // The gate compares the incoming snapshot against the last frame we actually
    // committed; `current` still holds it until we overwrite it below.
    let fetch_was_ok = outcome.snapshot.is_ok();
    let prev_good = current.clone();
    let computed = compute_next_state(
        &config.workspace_id,
        None,
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
    // Reconcile the highlight as part of the fold, before the next frame paints:
    // re-anchor the identity-keyed selection to its row (so a status-churn
    // reorder never slides it onto a neighbour) and re-derive the baseline from
    // the own view's active pane. Selection is derived state — queried from the
    // mux each fold and same-tab by construction — so an external tab switch or
    // focus move lands on the very next frame. The derivation is filtered to a
    // non-sidebar row: a sidebar-self-active or non-row active pane derives
    // `None` and the baseline holds its last value.
    let derived = current
        .own_view
        .as_ref()
        .filter(|view| !view.own_is_active)
        .and_then(|view| view.active_pane_id.clone())
        .filter(|pane| row_index_of_pane(current, pane).is_some());
    reconcile_selection(ui, current, derived);
    ui.animation_phase = wall_clock_phase(anim_start);
    // Fold the fresh tally into the count-up: a higher figure starts an eased
    // roll that the next frames paint, a reset or first value snaps. A fetch
    // without a tally leaves the rolls untouched. The serve loop paints the
    // folded state on its next frame boundary; this path never draws.
    if let Some(tally) = current.value_tally.as_ref() {
        ui.tally.observe(tally, ui.animation_phase);
    }

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
    // backstop. The focus-driven selection reconcile already ran in the fold
    // above.
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
        rimz::mux::own_pane_id(config.mux),
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
/// was the input lag. Input paints synchronously so a keypress or click feels
/// instant rather than waiting for the next frame; the returned `bool` reports
/// whether it painted, so the serve loop can clear its frame-pending flag.
fn apply_input(
    wakeup: Wakeup,
    ui: &mut UiState,
    health: &mut Health,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snapshot: &SidebarSnapshot,
    anim_start: Instant,
) -> Result<bool> {
    let outcome = handle_wakeup(wakeup, ui, snapshot);
    if outcome.dismiss {
        health.alert = None;
    }
    if outcome.redraw {
        // Carry the live spin phase into the instant paint so a keypress mid-spin
        // never rewinds the animation to a stale frame.
        ui.animation_phase = wall_clock_phase(anim_start);
        render::draw_to_terminal_with_ui(terminal, snapshot, health.alert.as_ref(), ui)?;
    }
    if let Some(pane) = outcome.focus {
        // A jump fires the one-way focus command at the resolved pane and
        // mutates no UI state. The highlight moves only when the derived
        // baseline catches up on a later fold — late, never wrong.
        spawn_pane_focus(pane);
    }
    Ok(outcome.redraw)
}

fn handle_wakeup(wakeup: Wakeup, ui: &mut UiState, snapshot: &SidebarSnapshot) -> InputOutcome {
    match wakeup {
        Wakeup::Key(action) => handle_key(action, ui, snapshot),
        Wakeup::MouseClick { column, row } => handle_mouse_click(column, row, ui, snapshot),
        Wakeup::Resize => InputOutcome::redraw(),
        // The serve loop intercepts these before dispatching here: a tick or a
        // ledger delta is a re-fetch trigger, worker completions are folded,
        // and a reload re-execs.
        Wakeup::Tick
        | Wakeup::Ledger { .. }
        | Wakeup::Reload
        | Wakeup::Snapshot
        | Wakeup::SelfCloseProbe => InputOutcome::default(),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct InputOutcome {
    redraw: bool,
    /// The pane to fire the one-way focus command at — `Some` only on a jump
    /// action. The handler resolves the target and returns it without moving
    /// the highlight: selection stays derived state, so there is nothing to
    /// repaint until the baseline catches up.
    focus: Option<PaneId>,
    dismiss: bool,
}

impl InputOutcome {
    fn redraw() -> Self {
        Self {
            redraw: true,
            focus: None,
            dismiss: false,
        }
    }

    fn focus(pane: PaneId) -> Self {
        Self {
            redraw: false,
            focus: Some(pane),
            dismiss: false,
        }
    }

    fn dismiss() -> Self {
        Self {
            redraw: true,
            focus: None,
            dismiss: true,
        }
    }
}

fn handle_key(action: KeyAction, ui: &mut UiState, snapshot: &SidebarSnapshot) -> InputOutcome {
    match action {
        KeyAction::Up => {
            if ui.selected_index > 0 {
                select_row(ui, snapshot, ui.selected_index - 1);
                begin_or_continue_browse(ui);
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Down => {
            let len = visible_row_count(snapshot);
            if ui.selected_index + 1 < len {
                select_row(ui, snapshot, ui.selected_index + 1);
                begin_or_continue_browse(ui);
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Enter => {
            // Jump on the current row: fire the focus command at the selected
            // pane without touching selection — the highlight follows once the
            // derived baseline catches up, identical to a click.
            match ui.selected_pane.clone() {
                Some(pane) => InputOutcome::focus(pane),
                None => InputOutcome::default(),
            }
        }
        KeyAction::Space => {
            if let Some(index) = next_attention_index(snapshot, ui.selected_index)
                && let Some(pane) = pane_at_row(snapshot, index)
            {
                return InputOutcome::focus(pane);
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
            if let Some(pane) = pane_at_row(snapshot, index) {
                return InputOutcome::focus(pane);
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
    if let Some(index) = row_index_at_screen_position(ui, row)
        && let Some(pane) = pane_at_row(snapshot, index)
    {
        return InputOutcome::focus(pane);
    }
    InputOutcome::default()
}

/// Point the highlight at a visible row by index — the identity-keyed selection
/// (`selected_pane`) plus its derived render index. A pure positioner for the
/// arrow-key browse; the jump actions resolve their target through
/// [`pane_at_row`] instead and never move the highlight.
fn select_row(ui: &mut UiState, snapshot: &SidebarSnapshot, index: usize) {
    ui.selected_index = index;
    ui.selected_pane = pane_at_row(snapshot, index);
}

/// The pane backing visible row `index`, or `None` for a pane-less row or an
/// out-of-range index.
fn pane_at_row(snapshot: &SidebarSnapshot, index: usize) -> Option<PaneId> {
    visible_rows(snapshot)
        .nth(index)
        .and_then(|row| row.pane.as_ref())
        .map(|pane| pane.pane_id.clone())
}

/// Pin the just-selected pane as the arrow-browse pick. The first arrow of a
/// browse captures the baseline it began from — the clear condition — and a
/// later arrow only moves the pick, so a long browse keeps one anchor and a
/// mid-browse baseline change still ends it. Roams every visible row, other
/// tabs' rows included.
fn begin_or_continue_browse(ui: &mut UiState) {
    if let Some(pane) = ui.selected_pane.clone() {
        let baseline_at_start = match ui.browse.take() {
            Some(browse) => browse.baseline_at_start,
            None => ui.baseline_pane.clone(),
        };
        ui.browse = Some(Browse {
            pane,
            baseline_at_start,
        });
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

/// Reconcile the highlight after folding a new snapshot. Selection is *derived*
/// state: the baseline is the own view's active working pane, re-queried from
/// the mux every fold — same-tab by construction — so the highlight always
/// reconverges on where the user actually is; it cannot desynchronize, only lag
/// a frame. One transient local layer rides above it: the arrow-key [`Browse`]
/// pick. A jump moves no local state — its highlight arrives here, when the
/// baseline catches up. Keyed on pane identity, never position.
///
/// `derived` is the snapshot's active-pane derivation, pre-filtered at the call
/// site to a non-sidebar row: `Some(pane)` iff `!own_is_active` and the view's
/// active pane is a row in this snapshot; `None` otherwise.
///
/// Ordered rules:
/// 1. **Hold-last baseline.** A `Some` derivation advances `baseline_pane`; a
///    `None` holds it, so a momentary "no active row" gap (the sidebar itself
///    focused) never blanks or moves the highlight.
/// 2. **Browse.** A live browse pins its pick while the baseline still equals
///    the value captured at browse start; a genuine baseline change ends it.
/// 3. **Follow the baseline** — the steady state.
/// 4. **Reanchor.** State whose pane left the room is dropped, and
///    `anchor_selection` re-derives `selected_index` by identity.
fn reconcile_selection(ui: &mut UiState, snapshot: &SidebarSnapshot, derived: Option<PaneId>) {
    // 1. Hold-last baseline: a Some derivation advances it, a None holds it.
    if let Some(pane) = derived {
        ui.baseline_pane = Some(pane);
    }

    // 2. Browse: hold the roamed pick while the baseline hasn't genuinely
    //    moved; on a baseline change the take stands — the browse ends and the
    //    highlight follows the new baseline.
    let mut pinned = false;
    if let Some(browse) = ui.browse.take()
        && ui.baseline_pane == browse.baseline_at_start
    {
        ui.selected_pane = Some(browse.pane.clone());
        ui.browse = Some(browse);
        pinned = true;
    }

    // 3. Steady state: the highlight is the derived baseline.
    if !pinned && let Some(pane) = ui.baseline_pane.clone() {
        ui.selected_pane = Some(pane);
    }

    // 4. Drop state whose pane left the room — so a pick whose pane closed
    //    stops shadowing the baseline — then re-anchor by identity.
    if let Some(pane) = ui.baseline_pane.clone()
        && row_index_of_pane(snapshot, &pane).is_none()
    {
        ui.baseline_pane = None;
    }
    if let Some(browse) = &ui.browse
        && row_index_of_pane(snapshot, &browse.pane).is_none()
    {
        ui.browse = None;
    }
    anchor_selection(ui, snapshot);
}

/// Re-derive `selected_index` from the identity-keyed `selected_pane`. When the
/// selected pane has left the room its row is gone, so drop the dangling
/// identity and clamp the index — the next mirror report or pick re-seats it.
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

/// Focus the pane on a detached thread so the keypress/click returns instantly:
/// `focus_pane` forks the mux client (`zellij action focus-pane-id` / the tmux
/// equivalent), which must never block the loop. The snapshot-bound pane is
/// focused directly — no `rimz pane focus` child, no per-click `list-panes`
/// re-validation; a pane recycled in the sub-second window since the snapshot
/// self-corrects on the next refresh.
/// Errors are logged, not surfaced — a missed jump is a retriable annoyance,
/// never a reason to block the UI. The command is the whole jump: no local
/// state changes, and the highlight converges on the next data fold (the
/// backstop tick or a ledger wakeup) once the mux reports the new focus.
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
                    thinking: false,
                    pane: Some(pane),
                    request_id: None,
                    surface: None,
                    task: None,
                    prompt: None,
                    model: None,
                    effort: None,
                    context_pct: None,
                    context_window: None,
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
                    parked_on_background: false,
                    turn_error_label: None,
                })
                .collect(),
            hidden_count: 0,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
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
            thinking: false,
            pane: Some(pane("terminal_9", "tab_0", false)),
            request_id: None,
            surface: None,
            task: Some("inspect auth".to_owned()),
            prompt: None,
            model: Some("Opus".to_owned()),
            effort: None,
            context_pct: None,
            context_window: None,
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
            parked_on_background: false,
            turn_error_label: None,
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
            commits_behind: None,
            trunk: None,
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
            thinking: false,
            pane: Some(pane("terminal_9", "tab_0", false)),
            request_id: None,
            surface: None,
            task: Some("inspect auth".to_owned()),
            prompt: None,
            model: Some("Opus".to_owned()),
            effort: Some("high".to_owned()),
            context_pct: Some(38),
            context_window: None,
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
            parked_on_background: false,
            turn_error_label: None,
        };
        let process = rimz::SidebarRow {
            row_kind: rimz::SidebarRowKind::Process,
            id: "terminal_10".to_owned(),
            name: "zsh".to_owned(),
            status: None,
            thinking: false,
            pane: Some(pane("terminal_10", "tab_0", false)),
            request_id: None,
            surface: None,
            task: None,
            prompt: None,
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
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
            parked_on_background: false,
            turn_error_label: None,
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
            commits_behind: None,
            trunk: None,
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
    fn frame_grid_advances_one_frame_when_on_time() {
        let base = Instant::now();
        // Painted at the scheduled boundary: the next boundary is exactly one
        // frame later, holding the fixed cadence.
        assert_eq!(
            next_frame_after(base, base, ANIMATION_FRAME),
            base + ANIMATION_FRAME
        );
    }

    #[test]
    fn frame_grid_snaps_forward_when_behind() {
        let base = Instant::now();
        // Scheduled several frames in the past relative to `now`: rather than
        // replaying every missed boundary, the grid snaps to one frame ahead of
        // `now`, so a slow paint never spirals into a burst of catch-up frames.
        let now = base + ANIMATION_FRAME * 5;
        assert_eq!(
            next_frame_after(base, now, ANIMATION_FRAME),
            now + ANIMATION_FRAME
        );
    }

    #[test]
    fn frame_interval_slows_cosmetic_animation_only() {
        let ws = workspace();
        let mut slow = snapshot(&ws);
        slow.worktree_groups = vec![rimz::SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![rimz::SidebarRow {
                row_kind: rimz::SidebarRowKind::Agent,
                id: "claude-1".to_owned(),
                name: "claude".to_owned(),
                status: Some(rimz::feed::AgentStatus::Waiting),
                thinking: false,
                pane: None,
                request_id: None,
                surface: None,
                task: Some("allow cargo fmt".to_owned()),
                prompt: None,
                model: None,
                effort: None,
                context_pct: None,
                context_window: None,
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
                parked_on_background: false,
                turn_error_label: None,
            }],
            hidden_count: 0,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
        }];

        assert_eq!(
            frame_interval(&slow, &UiState::default()),
            SLOW_ANIMATION_FRAME
        );

        slow.worktree_groups[0].rows[0].status = Some(rimz::feed::AgentStatus::Running);
        assert_eq!(frame_interval(&slow, &UiState::default()), ANIMATION_FRAME);
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
    fn resize_grew_treats_strictly_larger_width_as_grow() {
        // A grow is the flash precondition (the mux handed us a sibling's space),
        // so it takes the held path; a shrink or same width keeps the instant
        // repaint, and the first resize (no prior width) is held cautiously.
        assert!(resize_grew(Some(30), 120), "wider pane is a grow");
        assert!(!resize_grew(Some(120), 30), "narrower pane is not a grow");
        assert!(!resize_grew(Some(80), 80), "same width is not a grow");
        assert!(
            resize_grew(None, 1),
            "an unknown previous width counts as a grow"
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

    /// A browse pick of `pane`, begun while the derived baseline was `baseline`.
    fn browse(pane: &PaneId, baseline: Option<&PaneId>) -> Browse {
        Browse {
            pane: pane.clone(),
            baseline_at_start: baseline.cloned(),
        }
    }

    #[test]
    fn cold_start_derives_from_first_active_pane() {
        // No baseline and no local layer: the first frame's active-pane
        // derivation seeds both the baseline and the highlight.
        let ws = workspace();
        let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", true),
            ],
        );
        let mut ui = UiState::default();

        reconcile_selection(&mut ui, &snapshot, Some(active.clone()));

        assert_eq!(ui.selected_index, 1);
        assert_eq!(ui.selected_pane, Some(active.clone()));
        assert_eq!(ui.baseline_pane, Some(active));
    }

    #[test]
    fn cold_start_with_no_derivation_holds_none() {
        // No baseline, no local layer, a None derivation: nothing to follow, so
        // the highlight stays unseated (index clamped to row 0) until a frame
        // derives an active row — never a fleet-row guess that may sit in
        // another tab.
        let ws = workspace();
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState::default();

        reconcile_selection(&mut ui, &snapshot, None);

        assert_eq!(ui.selected_pane, None);
        assert_eq!(ui.selected_index, 0);
    }

    #[test]
    fn baseline_change_moves_the_highlight() {
        // No local layer: the highlight follows the derived baseline, so a
        // genuine external move (the user focused terminal_3) lands on the very
        // next fold.
        let ws = workspace();
        let was = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let now_active = PaneId::from_parts(MuxName::Zellij, "terminal_3");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
                pane("terminal_3", "tab_0", true),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            selected_pane: Some(was.clone()),
            baseline_pane: Some(was),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, Some(now_active.clone()));

        assert_eq!(ui.selected_index, 2);
        assert_eq!(ui.selected_pane, Some(now_active.clone()));
        assert_eq!(ui.baseline_pane, Some(now_active));
    }

    #[test]
    fn none_derivation_holds_last_baseline() {
        // The sidebar itself is the view's active pane (the user focused it to
        // type), or the active pane is not a row: the derivation is None, the
        // baseline holds, and the highlight stays put.
        let ws = workspace();
        let held = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            selected_pane: Some(held.clone()),
            baseline_pane: Some(held.clone()),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, None);

        assert_eq!(ui.selected_pane, Some(held.clone()));
        assert_eq!(ui.baseline_pane, Some(held));
    }

    #[test]
    fn highlight_moves_only_when_the_baseline_catches_up() {
        // The "accepts latency" contract behind the one-packet jump: a jump
        // action fires the focus command and mutates nothing, so a fold still
        // deriving the old pane keeps the old highlight, and the jumped pane
        // lights up only once the mux reports it focused.
        let ws = workspace();
        let from = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let jumped = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", true),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            selected_pane: Some(from.clone()),
            baseline_pane: Some(from.clone()),
            line_map: line_map_for(&snapshot, 0),
            ..Default::default()
        };

        // Click terminal_2's row: the outcome carries the target, the UI holds.
        let row1 = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();
        let outcome = handle_mouse_click(1, screen_row_for(row1), &mut ui, &snapshot);
        assert_eq!(outcome.focus, Some(jumped.clone()));
        assert_eq!(ui.selected_pane, Some(from.clone()));

        // A fold still deriving the pre-jump pane keeps the old highlight.
        reconcile_selection(&mut ui, &snapshot, Some(from.clone()));
        assert_eq!(ui.selected_pane, Some(from));

        // The fold that derives the jumped pane moves it.
        reconcile_selection(&mut ui, &snapshot, Some(jumped.clone()));
        assert_eq!(ui.selected_pane, Some(jumped.clone()));
        assert_eq!(ui.baseline_pane, Some(jumped));
    }

    #[test]
    fn browse_roams_other_tabs_rows() {
        // The browse pick may walk every visible row — another tab's included
        // (the cross-tab peek that expands a remote card) — while the derived
        // baseline stays untouched underneath.
        let ws = workspace();
        let here = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let remote = PaneId::from_parts(MuxName::Zellij, "terminal_9");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", true),
                pane("terminal_9", "tab_7", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            selected_pane: Some(here.clone()),
            baseline_pane: Some(here.clone()),
            ..Default::default()
        };

        select_row(&mut ui, &snapshot, 1);
        begin_or_continue_browse(&mut ui);
        // While browsing the user has the sidebar focused, so frames derive None.
        reconcile_selection(&mut ui, &snapshot, None);

        assert_eq!(ui.selected_pane, Some(remote), "the pick roams cross-tab");
        assert_eq!(ui.baseline_pane, Some(here), "the baseline never moves");
    }

    #[test]
    fn browse_holds_across_inert_frames() {
        // Browsing with the baseline unchanged: None derivations hold the
        // baseline, the anchor still matches, the pick holds.
        let ws = workspace();
        let picked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let baseline = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", true),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(picked.clone()),
            baseline_pane: Some(baseline.clone()),
            browse: Some(browse(&picked, Some(&baseline))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, None);
        reconcile_selection(&mut ui, &snapshot, None);

        assert_eq!(ui.selected_pane, Some(picked));
        assert!(ui.browse.is_some(), "still browsing");
    }

    #[test]
    fn browse_clears_on_baseline_change() {
        // A genuine baseline change — the user focused another working pane —
        // ends the browse, and the highlight follows the new baseline.
        let ws = workspace();
        let picked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let moved = PaneId::from_parts(MuxName::Zellij, "terminal_3");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
                pane("terminal_3", "tab_0", true),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(picked.clone()),
            baseline_pane: Some(anchor.clone()),
            browse: Some(browse(&picked, Some(&anchor))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, Some(moved.clone()));

        assert_eq!(ui.browse, None, "a real move ends the browse");
        assert_eq!(ui.selected_pane, Some(moved));
    }

    #[test]
    fn browse_survives_a_jump_and_ends_on_baseline_change() {
        // A jump mutates nothing, the browse included: an Enter mid-browse
        // leaves the pick in place, so the highlight holds still until the
        // derived baseline catches up underneath it — no flicker back to the
        // old pane. The browse then ends on the genuine baseline change.
        let ws = workspace();
        let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let picked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", true),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            baseline_pane: Some(anchor.clone()),
            ..Default::default()
        };

        select_row(&mut ui, &snapshot, 1);
        begin_or_continue_browse(&mut ui);
        let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
        assert_eq!(outcome.focus, Some(picked.clone()));
        assert!(ui.browse.is_some(), "the jump leaves the browse in place");

        // An inert fold (baseline unchanged) keeps the pick pinned.
        reconcile_selection(&mut ui, &snapshot, Some(anchor));
        assert!(ui.browse.is_some());
        assert_eq!(ui.selected_pane, Some(picked.clone()));

        // The fold that derives the jumped pane ends the browse seamlessly —
        // the baseline takes over on the same pane.
        reconcile_selection(&mut ui, &snapshot, Some(picked.clone()));
        assert_eq!(ui.browse, None, "a real baseline change ends the browse");
        assert_eq!(ui.selected_pane, Some(picked));
    }

    #[test]
    fn continued_browse_keeps_the_first_anchor() {
        // The second arrow continues the browse: the pick moves, but the anchor
        // (baseline_at_start) stays the one captured when browsing began, so a
        // baseline change mid-browse still ends it.
        let ws = workspace();
        let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", true),
                pane("terminal_2", "tab_0", false),
                pane("terminal_3", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            baseline_pane: Some(anchor.clone()),
            ..Default::default()
        };

        select_row(&mut ui, &snapshot, 1);
        begin_or_continue_browse(&mut ui);
        // The baseline advances mid-browse (rule 1 of an intervening fold)...
        ui.baseline_pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_3"));
        select_row(&mut ui, &snapshot, 2);
        begin_or_continue_browse(&mut ui);

        assert_eq!(
            ui.browse.as_ref().map(|b| b.baseline_at_start.clone()),
            Some(Some(anchor)),
            "the anchor is the browse-start baseline, not the latest one"
        );
    }

    #[test]
    fn selection_reanchors_to_its_pane_after_a_reorder() {
        // terminal_2 moved from row 1 to row 0 between folds with no baseline
        // change; the highlight follows its pane, not the old index.
        let ws = workspace();
        let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_2", "tab_0", true),
                pane("terminal_1", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(active.clone()),
            baseline_pane: Some(active.clone()),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, Some(active.clone()));

        assert_eq!(ui.selected_index, 0, "re-anchored to the pane's new row");
        assert_eq!(ui.selected_pane, Some(active));
    }

    #[test]
    fn selection_drops_when_its_pane_leaves_the_room() {
        // The baseline's pane is gone from the snapshot: drop the dangling
        // identity and clamp, so the next derivation can re-seat it.
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
            baseline_pane: Some(gone),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, None);

        assert_eq!(ui.selected_pane, None, "dangling identity dropped");
        assert_eq!(ui.baseline_pane, None, "absent baseline cleared");
        assert!(ui.selected_index < 2, "clamped to a valid row");
    }

    #[test]
    fn browse_drops_when_its_pane_leaves_the_room() {
        // A browse picks terminal_9, which then closes. The pick must not keep
        // shadowing the baseline — it is dropped, so the highlight reconverges
        // on the next fold.
        let ws = workspace();
        let gone = PaneId::from_parts(MuxName::Zellij, "terminal_9");
        let real = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", true),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(gone.clone()),
            baseline_pane: Some(real.clone()),
            browse: Some(browse(&gone, Some(&real))),
            ..Default::default()
        };

        reconcile_selection(&mut ui, &snapshot, None);
        assert_eq!(ui.browse, None, "the dead pick is dropped");

        // The next fold reconverges on the live baseline.
        reconcile_selection(&mut ui, &snapshot, None);
        assert_eq!(ui.selected_pane, Some(real));
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
    fn mouse_click_fires_focus_without_moving_selection() {
        let ws = workspace();
        let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
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

        assert_eq!(outcome, InputOutcome::focus(target));
        assert!(!outcome.redraw, "a jump changes nothing to repaint");
        assert_eq!(ui.selected_index, 0, "the click moves no selection");
        assert_eq!(ui.selected_pane, None);
        assert_eq!(ui.browse, None);
    }

    #[test]
    fn digit_fires_focus_at_the_ordinal_row_without_selecting() {
        let ws = workspace();
        let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState::default();

        let outcome = handle_key(KeyAction::Digit(2), &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::focus(target));
        assert_eq!(ui.selected_index, 0, "the digit moves no selection");
        assert_eq!(ui.selected_pane, None);

        // An out-of-range ordinal resolves no pane and does nothing.
        let outcome = handle_key(KeyAction::Digit(9), &mut ui, &snapshot);
        assert_eq!(outcome, InputOutcome::default());
    }

    #[test]
    fn space_fires_focus_at_the_next_attention_row_without_selecting() {
        let ws = workspace();
        let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let mut snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", true),
                pane("terminal_2", "tab_0", false),
            ],
        );
        snapshot.worktree_groups[0].rows[1].status = Some(rimz::feed::AgentStatus::Waiting);
        let mut ui = UiState::default();

        let outcome = handle_key(KeyAction::Space, &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::focus(target));
        assert_eq!(ui.selected_index, 0, "the triage key moves no selection");
        assert_eq!(ui.selected_pane, None);
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
        assert!(ui.browse.is_some(), "an arrow begins a browse pick");
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
    fn enter_fires_focus_at_the_selected_pane_without_mutating_ui() {
        let ws = workspace();
        let selected = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", false),
            ],
        );
        let mut ui = UiState {
            selected_index: 1,
            selected_pane: Some(selected.clone()),
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        };

        let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::focus(selected.clone()));
        assert_eq!(ui.selected_index, 1);
        assert_eq!(
            ui.selected_pane,
            Some(selected),
            "Enter reads, never writes"
        );

        // With nothing selected there is no target and nothing happens.
        ui.selected_pane = None;
        let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
        assert_eq!(outcome, InputOutcome::default());
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
