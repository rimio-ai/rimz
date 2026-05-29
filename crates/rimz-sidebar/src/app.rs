//! Runtime loop for the native sidebar process.

use std::io;
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
use rimz::{MuxName, RuntimePaths, SidebarInstanceId, SidebarSnapshot, WorkspaceId};
use tracing::{debug, warn};

use crate::render::{self, Alert, UiState};

mod input;
use input::{KeyAction, Wakeup, decode_wakeup, encode_key, encode_mouse, wait_for_wakeup};

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
    let mut health = Health::default();
    let mut self_close = SelfCloseState::default();
    let mut ui = UiState::default();
    let mut reexec_to: Option<PathBuf> = None;
    // Monotonic base for the animation frame. Deriving the phase from elapsed
    // wall-clock (rather than a per-tick counter) keeps the spin continuous
    // across re-fetches and ledger deltas, so no redraw path can stall it.
    let anim_start = Instant::now();

    'serve: loop {
        let heartbeat_outcome = write_heartbeat(&config, &runtime, &socket_path);
        let snapshot_outcome = fetch_snapshot_for(
            &resolve_snapshot_bin(&config.rimz_bin),
            &config.workspace_id,
            Some(config.mux),
            Some(&config.session_name),
            own_pane_id(config.mux),
        );
        let fetched_at = Instant::now();

        let state = compute_next_state(
            &config.workspace_id,
            heartbeat_outcome.as_ref().err().map(|e| e.to_string()),
            snapshot_outcome.map_err(|e| e.to_string()),
            last_snapshot.take(),
            &health,
        );
        if let Err(err) = &heartbeat_outcome {
            warn!(error = %err, "sidebar heartbeat failed");
        }
        if let Some(alert) = state
            .health
            .alert
            .as_ref()
            .filter(|alert| alert.is_active())
        {
            warn!(reason = %alert.reason, "sidebar refresh degraded");
        }
        clamp_selection(&mut ui, &state.snapshot);
        last_snapshot = state.last_snapshot;
        health = state.health;
        ui.animation_phase = wall_clock_phase(anim_start);
        render::draw_to_terminal_with_ui(
            &mut terminal,
            &state.snapshot,
            health.alert.as_ref(),
            &ui,
        )?;

        // A renderer that has been degraded this long is non-functional and,
        // with a now-stale heartbeat, unreachable by `rimz reload` — so it gives
        // up rather than lingering as a zombie showing a frozen frame. The
        // common deleted-binary snapshot failure already self-heals in place
        // (`resolve_snapshot_bin` falls back to the installed `rimz` each tick);
        // give-up is the backstop for what that cannot cure — a failing
        // heartbeat write, a vanished ledger, or no `rimz` on `PATH` at all.
        // Exiting closes its `close_on_exit` pane; reload/attach recovery then
        // rebuilds a current-build sidebar against the live panes, and a lone
        // orphan with no working pane simply disappears. This is the degraded
        // twin of self-close: self-close fires when the view empties, give-up
        // fires when the view can no longer be read at all.
        if degraded_too_long(&health, Timestamp::now()) {
            warn!(
                session = %config.session_name,
                reason = health.alert.as_ref().map(|alert| alert.reason.as_str()),
                "sidebar degraded too long; exiting so the pane closes and reload/attach can rebuild it",
            );
            break;
        }

        // Own-view (sibling count, focus) now rides in on the snapshot itself —
        // the `rimz sidebar snapshot` CLI computes it from the same pane list it
        // already enumerated, so the renderer no longer spawns a second `pane
        // list` per tick.
        let own_view = state.snapshot.own_view.as_ref();
        sync_selection_to_focused_pane(
            &mut ui,
            &state.snapshot,
            own_view
                .filter(|view| !view.own_is_focused)
                .and_then(|view| view.focused_pane_id.as_ref()),
        );
        if self_close_decision(&mut self_close, own_view.map(|view| view.sibling_count)) {
            debug!(
                session = %config.session_name,
                "sidebar tab emptied; exiting so the pane closes itself",
            );
            break;
        }
        // Wait for the next wakeup. While a running agent is animating, fall
        // into a fast animation tick that only advances the spin frame and
        // repaints the *current* snapshot — no `rimz` subprocess per frame. We
        // leave this inner loop (to re-fetch) on the data tick or a ledger
        // delta. Input only mutates local UI, so it is handled in place and
        // never re-runs the snapshot burst — that per-keystroke refetch was the
        // input lag.
        loop {
            let animating = render::has_live_animation(&state.snapshot);
            let timeout = if animating {
                ANIMATION_FRAME.min(tick)
            } else {
                tick
            };
            socket.set_read_timeout(Some(timeout))?;
            match wait_for_wakeup(&socket)? {
                Wakeup::Tick if animating && fetched_at.elapsed() < tick => {
                    ui.animation_phase = wall_clock_phase(anim_start);
                    render::draw_to_terminal_with_ui(
                        &mut terminal,
                        &state.snapshot,
                        health.alert.as_ref(),
                        &ui,
                    )?;
                }
                // The poll timeout: re-fetch to catch pane/git drift that fires
                // no ledger delta.
                Wakeup::Tick => break,
                // A ledger delta: drain any sibling deltas the same mutation
                // burst queued so a streaming agent triggers one re-fetch, not
                // one per event. Queued input is applied in place during the
                // drain; a queued reload still wins.
                Wakeup::Ledger => {
                    if drain_coalescing(
                        &socket,
                        &mut ui,
                        &mut health,
                        &mut terminal,
                        &state.snapshot,
                        &config,
                    )? {
                        if let Some(target) = reexec_target() {
                            reexec_to = Some(target);
                            break 'serve;
                        }
                        warn!(
                            session = %config.session_name,
                            "reload requested but no renderer binary is on disk; keeping the current build",
                        );
                    }
                    break;
                }
                Wakeup::Reload => {
                    if let Some(target) = reexec_target() {
                        debug!(
                            session = %config.session_name,
                            target = %target.display(),
                            "reload requested; re-execing the renderer in place",
                        );
                        reexec_to = Some(target);
                        break 'serve;
                    }
                    // A reload that cannot find its replacement (a partial or
                    // in-flight install) must never make the sidebar vanish —
                    // keep serving the current build and re-fetch.
                    warn!(
                        session = %config.session_name,
                        "reload requested but no renderer binary is on disk; keeping the current build",
                    );
                    break;
                }
                wakeup => {
                    apply_input(
                        wakeup,
                        &mut ui,
                        &mut health,
                        &mut terminal,
                        &state.snapshot,
                        &config,
                    )?;
                }
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

/// Animation tick: how often a running agent's head advances a spin frame.
/// Clamped against the data tick so a slow `tick_seconds` never stutters, and
/// only used while [`render::has_live_animation`] reports something to move.
const ANIMATION_FRAME: Duration = Duration::from_millis(120);

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
        recently_answered: Vec::new(),
        recent_activity: Vec::new(),
        agents: Vec::new(),
        agent_hooks_ready: false,
        own_view: None,
        project_root: None,
    }
}

/// Bundle returned by [`compute_next_state`]; the loop applies it verbatim.
#[derive(Clone, Debug)]
pub struct RenderState {
    pub snapshot: SidebarSnapshot,
    pub health: Health,
    pub last_snapshot: Option<SidebarSnapshot>,
}

fn fetch_snapshot_for(
    rimz_bin: &Path,
    workspace_id: &WorkspaceId,
    mux: Option<MuxName>,
    session_name: Option<&str>,
    exclude_pane_id: Option<PaneId>,
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
    config: &ServeConfig,
) -> Result<()> {
    let outcome = handle_wakeup(wakeup, ui, snapshot);
    if outcome.dismiss {
        health.alert = None;
    }
    if outcome.redraw {
        render::draw_to_terminal_with_ui(terminal, snapshot, health.alert.as_ref(), ui)?;
    }
    if let Some(index) = outcome.focus_index {
        focus_selected_row(snapshot, index, config);
    }
    Ok(())
}

/// Drain every datagram already queued on the wakeup socket without blocking.
/// Queued ledger deltas and ticks fold into the single re-fetch the caller is
/// about to do; queued input is applied in place; a queued reload is reported
/// so the caller can re-exec. Returns whether a reload was seen. The socket is
/// always restored to blocking mode before returning.
fn drain_coalescing(
    socket: &UnixDatagram,
    ui: &mut UiState,
    health: &mut Health,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snapshot: &SidebarSnapshot,
    config: &ServeConfig,
) -> Result<bool> {
    socket.set_nonblocking(true)?;
    let mut reload = false;
    let mut buf = [0_u8; 4096];
    loop {
        match socket.recv(&mut buf) {
            Ok(n) => match decode_wakeup(&buf[..n]) {
                Wakeup::Ledger | Wakeup::Tick => {}
                Wakeup::Reload => {
                    reload = true;
                    break;
                }
                input => apply_input(input, ui, health, terminal, snapshot, config)?,
            },
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) => {
                let _ = socket.set_nonblocking(false);
                return Err(err.into());
            }
        }
    }
    socket.set_nonblocking(false)?;
    Ok(reload)
}

fn handle_wakeup(wakeup: Wakeup, ui: &mut UiState, snapshot: &SidebarSnapshot) -> InputOutcome {
    match wakeup {
        Wakeup::Key(action) => handle_key(action, ui, snapshot),
        Wakeup::MouseClick { column, row } => handle_mouse_click(column, row, ui, snapshot),
        Wakeup::Resize => InputOutcome::redraw(),
        // The serve loop intercepts these before dispatching here: a tick or a
        // ledger delta is the re-fetch trigger, and a reload re-execs.
        Wakeup::Tick | Wakeup::Ledger | Wakeup::Reload => InputOutcome::default(),
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
                ui.selected_index -= 1;
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Down => {
            let len = visible_row_count(snapshot);
            if ui.selected_index + 1 < len {
                ui.selected_index += 1;
                return InputOutcome::redraw();
            }
            InputOutcome::default()
        }
        KeyAction::Enter => InputOutcome::focus(ui.selected_index),
        KeyAction::Space => {
            if let Some(index) = next_attention_index(snapshot, ui.selected_index) {
                ui.selected_index = index;
                return InputOutcome::focus(ui.selected_index);
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
                ui.selected_index = index;
                return InputOutcome::focus(ui.selected_index);
            }
            InputOutcome::default()
        }
    }
}

fn handle_mouse_click(
    column: u16,
    row: u16,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
) -> InputOutcome {
    if let Some(index) = row_index_at_screen_position(snapshot, ui.selected_index, column, row) {
        ui.selected_index = index;
        return InputOutcome::focus(ui.selected_index);
    }
    InputOutcome::default()
}

fn clamp_selection(ui: &mut UiState, snapshot: &SidebarSnapshot) {
    let len = visible_row_count(snapshot);
    if len == 0 {
        ui.selected_index = 0;
    } else if ui.selected_index >= len {
        ui.selected_index = len - 1;
    }
}

fn sync_selection_to_focused_pane(
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    focused_pane_id: Option<&PaneId>,
) {
    let Some(focused_pane_id) = focused_pane_id else {
        return;
    };
    if let Some(index) = visible_rows(snapshot).position(|row| {
        row.pane
            .as_ref()
            .is_some_and(|pane| pane.pane_id == *focused_pane_id)
    }) {
        ui.selected_index = index;
    }
}

fn row_index_at_screen_position(
    snapshot: &SidebarSnapshot,
    selected_index: usize,
    column: u16,
    row: u16,
) -> Option<usize> {
    // The block border occupies row 0 and column 0. Ratatui renders the
    // snapshot body one cell in from the top-left border.
    if row == 0 || column == 0 {
        return None;
    }
    let target = usize::from(row - 1);
    row_index_at_content_line(snapshot, selected_index, target)
}

fn row_index_at_content_line(
    snapshot: &SidebarSnapshot,
    selected_index: usize,
    target: usize,
) -> Option<usize> {
    // The health alert is pinned below every row, so it never shifts the row
    // grid; clicks on it fall through to `None`.
    let mut line = 0_usize;
    let mut last_nonempty = false;

    if has_attention_line(snapshot) {
        if target == line {
            return None;
        }
        line += 1;
        last_nonempty = true;
    }

    if snapshot.worktree_groups.is_empty() {
        return None;
    }

    if last_nonempty {
        if target == line {
            return None;
        }
        line += 1;
    }

    let mut row_index = 0_usize;
    for (group_index, group) in snapshot.worktree_groups.iter().enumerate() {
        if group_index > 0 {
            if target == line {
                return None;
            }
            line += 1;
        }

        if target == line {
            return None;
        }
        line += 1;

        for row in &group.rows {
            if target == line {
                return Some(row_index);
            }
            line += 1;

            let selected = row_index == selected_index;
            for _ in 0..row_extra_line_count(row, selected) {
                if target == line {
                    return Some(row_index);
                }
                line += 1;
            }
            row_index += 1;
        }

        if group.hidden_count > 0 {
            if target == line {
                return None;
            }
            line += 1;
        }
    }
    None
}

fn has_attention_line(snapshot: &SidebarSnapshot) -> bool {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.status_counts)
        .any(|count| {
            count.count > 0
                && matches!(
                    count.status,
                    rimz::feed::AgentStatus::Waiting | rimz::feed::AgentStatus::Failed
                )
        })
}

fn row_extra_line_count(row: &rimz::SidebarRow, selected: bool) -> usize {
    if row.row_kind != rimz::SidebarRowKind::Agent {
        return 0;
    }
    let mut lines = 0;
    if row_has_capability_line(row) {
        lines += 1;
    }
    // Selection adds only the token-total line; the gauge stays inline on the
    // capability line whether selected or not (see `render::sections`).
    if selected && row.total_tokens.is_some() {
        lines += 1;
    }
    lines
}

fn row_has_capability_line(row: &rimz::SidebarRow) -> bool {
    row.model.as_deref().is_some_and(|value| !value.is_empty())
        || row.effort.as_deref().is_some_and(|value| !value.is_empty())
        || matches!(
            row.permission_posture,
            Some(rimz::feed::PermissionPosture::Auto | rimz::feed::PermissionPosture::Yolo)
        )
        || row.context_pct.is_some()
        || row.todo_total.unwrap_or(0) > 0
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

fn focus_selected_row(snapshot: &SidebarSnapshot, selected: usize, config: &ServeConfig) {
    let Some(row) = visible_rows(snapshot).nth(selected) else {
        return;
    };
    let Some(pane) = &row.pane else {
        return;
    };
    let mut command = Command::new(&config.rimz_bin);
    command.args(["pane", "focus", pane.pane_id.as_str(), "--session-name"]);
    command.arg(&pane.session_name);
    if let Some(start) = pane.pane_process_start {
        command.arg("--pane-process-start").arg(start.to_string());
    }
    match command.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => warn!(
            pane = %pane.pane_id,
            stderr = %String::from_utf8_lossy(&output.stderr),
            "sidebar pane focus failed",
        ),
        Err(err) => warn!(
            pane = %pane.pane_id,
            error = %err,
            "sidebar pane focus command failed",
        ),
    }
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
                    permission_posture: None,
                    plan_mode: false,
                    pane: Some(pane),
                    request_id: None,
                    surface: None,
                    task: None,
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
                })
                .collect(),
            hidden_count: 0,
            diff_added: None,
            diff_removed: None,
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
            plan_mode: false,
            pane: Some(pane("terminal_9", "tab_0", false)),
            request_id: None,
            surface: None,
            task: Some("inspect auth".to_owned()),
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
    fn selection_syncs_to_focused_pane_row() {
        let ws = workspace();
        let focused = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let snapshot = snapshot_with_panes(
            &ws,
            vec![
                pane("terminal_1", "tab_0", false),
                pane("terminal_2", "tab_0", true),
            ],
        );
        let mut ui = UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
        };

        sync_selection_to_focused_pane(&mut ui, &snapshot, Some(&focused));

        assert_eq!(ui.selected_index, 1);
    }

    #[test]
    fn selection_stays_put_when_focus_is_unknown() {
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
        };

        sync_selection_to_focused_pane(&mut ui, &snapshot, None);

        assert_eq!(ui.selected_index, 1);
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

        assert_eq!(
            row_index_at_screen_position(&snapshot, 0, 1, 1),
            None,
            "the group header is not a row"
        );
        assert_eq!(
            row_index_at_screen_position(&snapshot, 0, 0, 2),
            None,
            "the border is not clickable content"
        );
        assert_eq!(row_index_at_screen_position(&snapshot, 0, 1, 2), Some(0));
        assert_eq!(row_index_at_screen_position(&snapshot, 0, 1, 3), Some(1));
    }

    #[test]
    fn row_index_maps_agent_capability_line_to_same_row() {
        let ws = workspace();
        let snapshot = agent_snapshot(&ws);

        assert_eq!(row_index_at_screen_position(&snapshot, 0, 1, 2), Some(0));
        assert_eq!(
            row_index_at_screen_position(&snapshot, 0, 1, 3),
            Some(0),
            "clicking an agent capability line routes to that agent row"
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
        };

        let outcome = handle_mouse_click(1, 3, &mut ui, &snapshot);

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
        };

        let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::focus(1));
        assert_eq!(ui.selected_index, 1);
    }
}
