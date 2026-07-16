use super::panel::{
    PanelGeometry, PanelGlyphs, emit, header_lines, render_panel, resolve_panel_glyphs,
};
use super::*;

pub(super) fn run_refresh(dollars: bool, hold: bool) -> Result<()> {
    install_reload_signal()?;
    let glyphs = resolve_panel_glyphs(&super::super::machine_config().theme);
    let paths = RuntimePaths::shared();
    ensure_shared_runtime(&paths)?;
    // Raw mode makes keypresses typed events instead of echoed cooked input;
    // mouse reports from a sibling sidebar pane are drained below.
    let _input = TerminalModeGuard::enable(MouseCapture::Off, Screen::Main)?;
    let mut current: Option<Stats> = None;
    let mut active = Window::AllTime;
    loop {
        let (tx, rx) = mpsc::channel();
        let worker_paths = paths.clone();
        thread::spawn(move || {
            let event = refresh_event(|| load_or_refresh_stats_via_service(&worker_paths));
            let _ = tx.send(event);
        });
        match hold_cycle(hold, &mut current, &rx, dollars, &glyphs, &mut active)? {
            CycleExit::Refresh => {}
            CycleExit::Reload => {
                if let Some(target) = rimz::reload::current_reexec_target() {
                    return Err(reexec(&target));
                }
            }
            CycleExit::Quit => return Ok(()),
        }
    }
}

pub(super) fn reload_flag() -> &'static Arc<AtomicBool> {
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

/// Register SIGUSR1 -> reload flag so `rimz reload` can drive the same in-place
/// re-exec the `r` key runs. Registering replaces the default-terminate
/// disposition, so the dashboard catches the signal instead of dying.
#[cfg(unix)]
pub(super) fn install_reload_signal() -> std::io::Result<()> {
    use signal_hook::consts::signal::SIGUSR1;

    signal_hook::flag::register(SIGUSR1, reload_flag().clone()).map(|_| ())
}

#[cfg(not(unix))]
pub(super) fn install_reload_signal() -> std::io::Result<()> {
    Ok(())
}

/// Read-and-clear the reload request. Clearing on consume keeps a SIGUSR1 that
/// lands when no re-exec target resolves from latching into a busy loop.
pub(super) fn take_reload_request() -> bool {
    consume_reload_flag(reload_flag())
}

pub(super) fn consume_reload_flag(flag: &AtomicBool) -> bool {
    flag.swap(false, Ordering::SeqCst)
}

pub(super) enum CycleExit {
    Refresh,
    Reload,
    Quit,
}

pub(super) struct RefreshEvent {
    stats: Option<Result<Stats>>,
}

pub(super) fn refresh_event(load: impl FnOnce() -> Result<Stats>) -> RefreshEvent {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(load)) {
        Ok(stats) => RefreshEvent { stats: Some(stats) },
        Err(payload) => {
            tracing::warn!(
                panic = %panic_payload_message(payload.as_ref()),
                "stats refresh panicked"
            );
            RefreshEvent { stats: None }
        }
    }
}

pub(super) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

pub(super) fn hold_cycle(
    hold: bool,
    current: &mut Option<Stats>,
    rx: &mpsc::Receiver<RefreshEvent>,
    dollars: bool,
    glyphs: &PanelGlyphs,
    active: &mut Window,
) -> Result<CycleExit> {
    let deadline = Instant::now() + REFRESH_INTERVAL;
    let mut refresh_finished = false;
    loop {
        if take_reload_request() {
            return Ok(CycleExit::Reload);
        }
        match rx.try_recv() {
            Ok(event) => {
                if let Some(stats) = event.stats {
                    *current = Some(stats?);
                    if let Some(stats) = current.as_ref() {
                        repaint(stats, dollars, glyphs, *active)?;
                    }
                }
                refresh_finished = true;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) if !refresh_finished => {
                return Ok(CycleExit::Refresh);
            }
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        let now = Instant::now();
        if now >= deadline && refresh_finished {
            return Ok(CycleExit::Refresh);
        }
        let timeout = if now >= deadline {
            REFRESH_POLL_TICK
        } else {
            (deadline - now).min(REFRESH_POLL_TICK)
        };
        match event::poll(timeout) {
            Ok(true) => match event::read() {
                Ok(Event::Resize(_, _)) => {
                    if let Some(stats) = current.as_ref() {
                        repaint(stats, dollars, glyphs, *active)?;
                    }
                }
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    match key_outcome(key, hold) {
                        KeyOutcome::Reload => return Ok(CycleExit::Reload),
                        KeyOutcome::Quit => return Ok(CycleExit::Quit),
                        KeyOutcome::NextWindow => {
                            *active = active.next();
                            if let Some(stats) = current.as_ref() {
                                repaint(stats, dollars, glyphs, *active)?;
                            }
                        }
                        KeyOutcome::PrevWindow => {
                            *active = active.prev();
                            if let Some(stats) = current.as_ref() {
                                repaint(stats, dollars, glyphs, *active)?;
                            }
                        }
                        KeyOutcome::Ignore => {}
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    if take_reload_request() {
                        return Ok(CycleExit::Reload);
                    }
                    return Ok(CycleExit::Quit);
                }
            },
            Ok(false) => {}
            Err(_) => {
                if take_reload_request() {
                    return Ok(CycleExit::Reload);
                }
                return Ok(CycleExit::Quit);
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum KeyOutcome {
    Reload,
    Quit,
    NextWindow,
    PrevWindow,
    Ignore,
}

pub(super) fn key_outcome(key: KeyEvent, hold: bool) -> KeyOutcome {
    match key.code {
        KeyCode::Tab => KeyOutcome::NextWindow,
        KeyCode::BackTab => KeyOutcome::PrevWindow,
        KeyCode::Char('r') | KeyCode::Char('R') => KeyOutcome::Reload,
        KeyCode::Char('c') | KeyCode::Char('C') => {
            if key.modifiers.contains(KeyModifiers::CONTROL) && !hold {
                KeyOutcome::Quit
            } else {
                KeyOutcome::Ignore
            }
        }
        _ => KeyOutcome::Ignore,
    }
}

pub(super) fn reexec(target: &Path) -> anyhow::Error {
    use std::os::unix::process::CommandExt;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let err = std::process::Command::new(target).args(&args).exec();
    anyhow!(
        "failed to reload stats: re-exec {} failed: {err}",
        target.display()
    )
}

/// Repaint the held panel in place: home the cursor, overwrite each line
/// (clearing to its end), then clear anything below without a whole-screen blank.
pub(super) fn repaint(
    stats: &Stats,
    dollars: bool,
    glyphs: &PanelGlyphs,
    active: Window,
) -> Result<()> {
    use ratatui::crossterm::{
        cursor::MoveTo,
        execute,
        terminal::{Clear, ClearType},
    };

    execute!(std::io::stdout(), MoveTo(0, 0))?;
    let today_day = unix_secs_now() as i64 / DAY_SECS;
    render_panel(
        stats,
        today_day,
        dollars,
        glyphs,
        true,
        REFRESH_NL,
        Some(active),
    )?;
    execute!(std::io::stdout(), Clear(ClearType::FromCursorDown))?;
    Ok(())
}

pub(super) fn load_cold_stats_with_spinner(paths: &RuntimePaths) -> Result<LoadedStats> {
    let geometry = PanelGeometry::current();
    emit(&header_lines(geometry.panel_width), geometry.outer, "\n")?;

    let file_count = discover_spending_files().len();
    let spinner = Spinner::delayed(
        progress_line(SpendProgress {
            finished_files: 0,
            total_files: file_count,
        }),
        SPINNER_MIN_AGE,
    );
    let mut progress = |progress| spinner.set(progress_line(progress));
    let stats = load_direct_stats_with_progress(paths, &mut progress)?;
    Ok(LoadedStats {
        stats,
        header_printed: true,
    })
}

pub(super) fn should_animate_cold_stats(human: bool, stdout_tty: bool, stderr_tty: bool) -> bool {
    human && stdout_tty && stderr_tty
}

pub(super) fn progress_line(progress: SpendProgress) -> String {
    let total = progress.total_files;
    let done = progress.finished_files.min(total);
    let plural = if total == 1 { "" } else { "s" };
    let count_width = total.max(1).to_string().len();
    let bar = progress_bar(done, total);
    format!("Reading session file{plural} [{bar}] {done:>count_width$}/{total}")
}

pub(super) fn progress_bar(done: usize, total: usize) -> String {
    let filled = done
        .saturating_mul(PROGRESS_BAR_WIDTH)
        .checked_div(total)
        .unwrap_or(0)
        .min(PROGRESS_BAR_WIDTH);
    let mut bar = String::with_capacity(PROGRESS_BAR_WIDTH);
    bar.extend(std::iter::repeat_n('█', filled));
    bar.extend(std::iter::repeat_n('░', PROGRESS_BAR_WIDTH - filled));
    bar
}

// ── The grid ───────────────────────────────────────────────────────────────────
