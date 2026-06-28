//! `rimz stats` — the wordmark, the token-usage heatmap, per-model and
//! per-agent breakdowns, and the account-global activity insights as a
//! standalone command. The lobby embeds the same panel; here it stands alone so
//! the figures read from inside a room, where the lobby never appears.
//!
//! Account-global: it reads the producer's published provider aggregate when
//! available. A standalone first launch takes the same shared spending election
//! as the sidebar producer, publishes the same provider-spending rollups, and
//! then subsequent runs return from the cache.
//!
//! Windows: the heatmap always reads the full available history (the cache spans
//! the trailing year). In the held dashboard, the windows row is a tab bar that
//! scopes the model breakdown, agent breakdown, and insights below it.
//! `--refresh` recomputes on a background thread, repaints the held frame in
//! place every minute, and re-centres promptly after a width change.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use clap::Args;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::Serialize;
use unicode_width::UnicodeWidthChar;

use super::GlobalFlags;
use crate::cli::render;
use crate::cli::spinner::{SPINNER_FRAMES, SPINNER_TICK};
use rimz::RuntimePaths;
use rimz::agents::AgentAdapter;
use rimz::agents::pricing;
use rimz::agents::spending::{
    DaySpend, HeadlineSpec, ProviderSpendingCache, SpendProgress, SpendTally, SpendWindow,
    Spending, SpendingWalker, discover_spending_files, read_provider_spending_cache, unix_secs_now,
    utc_date, write_provider_spending_cache_with_rollups,
};
use rimz::config::{GlyphRole, Semantic, ThemeConfig};
use rimz::ledger::single_flight::{Coalesced, coalesce};
use rimz::tui::{MouseCapture, TerminalModeGuard};

const DAY_SECS: i64 = 86_400;
/// The five-step density ramp: a calm day through your heaviest.
const RAMP: [char; 5] = ['·', '░', '▒', '▓', '█'];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// Widest heatmap span — about a year, GitHub's contribution window.
const MAX_WEEKS: usize = 52;
const MIN_WEEKS: usize = 4;
/// Left weekday-label column width (`"  Wed "`).
const GUTTER: usize = 6;
/// Named models shown before the rest fold into one "Other" row.
const MAX_MODELS: usize = 6;
const SPINNER_MIN_AGE: Duration = Duration::from_millis(150);
const PROGRESS_BAR_WIDTH: usize = 20;
const MIN_SHARE_BAR_WIDTH: usize = 10;
const STAT_GUTTER: usize = 3;
const SPENDING_WAIT_STEP: Duration = Duration::from_millis(20);
const SPENDING_WAIT_STEPS: u32 = 15;
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
/// Refresh-repaint line terminator: clear-to-EOL, then CRLF (raw mode). Lets a
/// repaint overwrite the prior frame in place without a whole-screen blank.
const REFRESH_NL: &str = "\x1b[K\r\n";
const REFRESH_POLL_TICK: Duration = Duration::from_millis(100);

/// The wordmark, spaced for a monospace terminal (the README carries a variant
/// retuned for proportional HTML rendering).
const WORDMARK: &str = r#"██████╗ ██╗███╗   ███╗  ███████╗
██╔══██╗██║████╗ ████║  ╚══███╔╝
██████╔╝██║██╔████╔██║    ███╔╝
██╔══██╗██║██║╚██╔╝██║   ███╔╝
██║  ██║██║██║ ╚═╝ ██║  ███████╗
╚═╝  ╚═╝╚═╝╚═╝     ╚═╝  ╚══════╝"#;
const TAGLINE: &str = "The control room for your coding agents";

#[derive(Debug, Args)]
pub struct StatsArgs {
    /// Scale the heatmap by dollars spent instead of tokens used.
    #[arg(long)]
    pub dollars: bool,
    /// Emit the stats as JSON instead of the panel.
    #[arg(long)]
    pub json: bool,
    /// Hold the panel open, refresh stats every 60s, and re-centre on resize.
    #[arg(long, conflicts_with = "json")]
    pub refresh: bool,
    /// Keep the refreshing panel alive through Ctrl-C in the rimzd daemon view.
    /// Set only by the view.
    #[arg(long, hide = true, requires = "refresh")]
    pub hold: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Window {
    AllTime,
    Week,
    Month,
    Year,
}

impl Window {
    const TABS: [Self; 4] = [Self::AllTime, Self::Week, Self::Month, Self::Year];

    fn next(self) -> Self {
        let index = Self::TABS
            .iter()
            .position(|window| *window == self)
            .unwrap_or(0);
        Self::TABS[(index + 1) % Self::TABS.len()]
    }

    fn prev(self) -> Self {
        let index = Self::TABS
            .iter()
            .position(|window| *window == self)
            .unwrap_or(0);
        Self::TABS[(index + Self::TABS.len() - 1) % Self::TABS.len()]
    }

    fn label(self) -> &'static str {
        match self {
            Self::AllTime => "All time",
            Self::Week => "Week",
            Self::Month => "Month",
            Self::Year => "Year",
        }
    }

    fn select(self, tally: &SpendTally) -> SpendWindow {
        match self {
            Self::AllTime | Self::Year => tally.year,
            Self::Week => tally.week,
            Self::Month => tally.month,
        }
    }

    fn span_days(self) -> u32 {
        match self {
            Self::AllTime => 28,
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
        }
    }
}

pub fn run(args: StatsArgs, _globals: &GlobalFlags) -> Result<()> {
    if args.refresh {
        return run_refresh(args.dollars, args.hold);
    }
    let loaded = load_stats(!args.json)?;
    let today_day = unix_secs_now() as i64 / DAY_SECS;
    if args.json {
        return emit_json(&loaded.stats, today_day, args.dollars);
    }
    let glyphs = resolve_panel_glyphs(&super::machine_config().theme);
    render_panel(
        &loaded.stats,
        today_day,
        args.dollars,
        &glyphs,
        !loaded.header_printed,
        "\n",
        None,
    )
}

fn run_refresh(dollars: bool, hold: bool) -> Result<()> {
    install_reload_signal()?;
    let glyphs = resolve_panel_glyphs(&super::machine_config().theme);
    let paths = RuntimePaths::shared();
    ensure_shared_runtime(&paths)?;
    // Raw mode makes keypresses typed events instead of echoed cooked input;
    // mouse reports from a sibling sidebar pane are drained below.
    let _input = TerminalModeGuard::enable(MouseCapture::Off)?;
    let mut current: Option<Stats> = None;
    let mut active = Window::AllTime;
    let mut walker = Some(SpendingWalker::new());
    loop {
        if let Some(target) = rimz::reload::reexec_target_if_build_changed() {
            return Err(reexec(&target));
        }
        let (tx, rx) = mpsc::channel();
        let worker_paths = paths.clone();
        let Some(worker_walker) = walker.take() else {
            return Err(anyhow!("stats refresh walker unavailable"));
        };
        thread::spawn(move || {
            let event = refresh_event(worker_walker, |walker| {
                load_or_refresh_stats(&worker_paths, None, walker)
            });
            let _ = tx.send(event);
        });
        match hold_cycle(hold, &mut current, &rx, dollars, &glyphs, &mut active)? {
            CycleExit::Refresh(next_walker) => {
                walker = Some(*next_walker);
            }
            CycleExit::Reload => {
                if let Some(target) = rimz::reload::current_reexec_target() {
                    return Err(reexec(&target));
                }
            }
            CycleExit::Quit => return Ok(()),
        }
    }
}

fn reload_flag() -> &'static Arc<AtomicBool> {
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

/// Register SIGUSR1 -> reload flag so `rimz reload` can drive the same in-place
/// re-exec the `r` key runs. Registering replaces the default-terminate
/// disposition, so the dashboard catches the signal instead of dying.
#[cfg(unix)]
fn install_reload_signal() -> std::io::Result<()> {
    use signal_hook::consts::signal::SIGUSR1;

    signal_hook::flag::register(SIGUSR1, reload_flag().clone()).map(|_| ())
}

#[cfg(not(unix))]
fn install_reload_signal() -> std::io::Result<()> {
    Ok(())
}

/// Read-and-clear the reload request. Clearing on consume keeps a SIGUSR1 that
/// lands when no re-exec target resolves from latching into a busy loop.
fn take_reload_request() -> bool {
    consume_reload_flag(reload_flag())
}

fn consume_reload_flag(flag: &AtomicBool) -> bool {
    flag.swap(false, Ordering::SeqCst)
}

enum CycleExit {
    Refresh(Box<SpendingWalker>),
    Reload,
    Quit,
}

struct RefreshEvent {
    stats: Option<Result<Stats>>,
    walker: SpendingWalker,
}

fn refresh_event(
    mut walker: SpendingWalker,
    load: impl FnOnce(&mut SpendingWalker) -> Result<Stats>,
) -> RefreshEvent {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load(&mut walker))) {
        Ok(stats) => RefreshEvent {
            stats: Some(stats),
            walker,
        },
        Err(payload) => {
            tracing::warn!(
                panic = %panic_payload_message(payload.as_ref()),
                "stats refresh panicked"
            );
            RefreshEvent {
                stats: None,
                walker: SpendingWalker::new(),
            }
        }
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

fn hold_cycle(
    hold: bool,
    current: &mut Option<Stats>,
    rx: &mpsc::Receiver<RefreshEvent>,
    dollars: bool,
    glyphs: &PanelGlyphs,
    active: &mut Window,
) -> Result<CycleExit> {
    let deadline = Instant::now() + REFRESH_INTERVAL;
    let mut returned_walker: Option<SpendingWalker> = None;
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
                returned_walker = Some(event.walker);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) if returned_walker.is_none() => {
                return Ok(CycleExit::Refresh(Box::new(SpendingWalker::new())));
            }
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        let now = Instant::now();
        if now >= deadline
            && let Some(walker) = returned_walker
        {
            return Ok(CycleExit::Refresh(Box::new(walker)));
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
enum KeyOutcome {
    Reload,
    Quit,
    NextWindow,
    PrevWindow,
    Ignore,
}

fn key_outcome(key: KeyEvent, hold: bool) -> KeyOutcome {
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

fn reexec(target: &Path) -> anyhow::Error {
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
fn repaint(stats: &Stats, dollars: bool, glyphs: &PanelGlyphs, active: Window) -> Result<()> {
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

struct LoadedStats {
    stats: Stats,
    header_printed: bool,
}

/// The deduplicated account-global history `rimz stats` renders: per-day buckets,
/// the per-model and per-agent breakdowns, and producer-priced trailing windows.
struct Stats {
    by_day: BTreeMap<i64, DaySpend>,
    by_model: BTreeMap<String, SpendTally>,
    by_agent: BTreeMap<String, SpendTally>,
    total: SpendTally,
}

impl Stats {
    fn from_provider(cache: ProviderSpendingCache) -> Self {
        let ProviderSpendingCache {
            days,
            models,
            spending,
            ..
        } = cache;
        let Spending { total, by_provider } = spending;
        Stats {
            by_day: days,
            by_model: models,
            by_agent: by_provider,
            total,
        }
    }
}

struct PanelGlyphs {
    sessions: String,
    total: String,
    input: String,
    output: String,
    cache_read: String,
    bar_filled: String,
    bar_track: String,
}

fn resolve_panel_glyphs(theme: &ThemeConfig) -> PanelGlyphs {
    let glyph = |role| rimz::sidebar_pane::render::theme_glyph(theme, role);
    PanelGlyphs {
        sessions: glyph(GlyphRole::CockpitSessions),
        total: glyph(GlyphRole::TokensTotal),
        input: glyph(GlyphRole::TokensInput),
        output: glyph(GlyphRole::TokensOutput),
        cache_read: glyph(GlyphRole::TokensCacheRead),
        bar_filled: glyph(GlyphRole::MeterBarFilled),
        bar_track: glyph(GlyphRole::MeterBarTrack),
    }
}

fn load_stats(human: bool) -> Result<LoadedStats> {
    let paths = RuntimePaths::shared();
    load_stats_from_paths(&paths, human)
}

fn load_stats_from_paths(paths: &RuntimePaths, human: bool) -> Result<LoadedStats> {
    ensure_shared_runtime(paths)?;
    if let Some(stats) = load_published_stats(paths) {
        return Ok(LoadedStats {
            stats,
            header_printed: false,
        });
    }

    if should_animate_cold_stats(
        human,
        std::io::stdout().is_terminal(),
        std::io::stderr().is_terminal(),
    ) {
        return load_cold_stats_with_spinner(paths);
    }

    let mut walker = SpendingWalker::new();
    let stats = load_or_refresh_stats(paths, None, &mut walker)?;
    Ok(LoadedStats {
        stats,
        header_printed: false,
    })
}

fn load_published_stats(paths: &RuntimePaths) -> Option<Stats> {
    let cache = read_provider_spending_cache(&paths.shared_provider_spending_path());
    cache
        .is_current_version()
        .then(|| Stats::from_provider(cache))
}

fn load_or_refresh_stats(
    paths: &RuntimePaths,
    progress: Option<&mut dyn FnMut(SpendProgress)>,
    walker: &mut SpendingWalker,
) -> Result<Stats> {
    if let Some(stats) = load_published_stats(paths) {
        return Ok(stats);
    }
    let fresh = || load_published_stats(paths);
    match coalesce(
        &paths.shared_spending_lock(),
        SPENDING_WAIT_STEP,
        SPENDING_WAIT_STEPS,
        fresh,
    ) {
        Coalesced::Shared(stats) => Ok(stats),
        Coalesced::Produce(_guard) => Ok(compute_stats_from_files(
            paths,
            discover_spending_files(),
            true,
            progress,
            walker,
        )),
        Coalesced::ProduceLocal => Ok(compute_stats_from_files(
            paths,
            discover_spending_files(),
            false,
            progress,
            walker,
        )),
    }
}

fn compute_stats_from_files(
    paths: &RuntimePaths,
    files: Vec<(&'static dyn AgentAdapter, PathBuf)>,
    publish: bool,
    progress: Option<&mut dyn FnMut(SpendProgress)>,
    walker: &mut SpendingWalker,
) -> Stats {
    let cursor_path = paths.shared_spending_cursor_path();
    let now_secs = unix_secs_now();
    let prices = if publish {
        let unknowns = walker.recorded_unknown_models(&cursor_path, &files, now_secs);
        pricing::load_for_spending(&paths.shared_pricing_cache_path(), &unknowns)
    } else {
        pricing::load_cached_for_spending(&paths.shared_pricing_cache_path())
    };
    let result = match (publish, progress) {
        (true, Some(progress)) => walker.walk_with_progress(
            &cursor_path,
            &files,
            &prices,
            now_secs,
            &Default::default(),
            None,
            &HeadlineSpec::default(),
            progress,
        ),
        (true, None) => walker.walk(
            &cursor_path,
            &files,
            &prices,
            now_secs,
            &Default::default(),
            None,
            &HeadlineSpec::default(),
        ),
        (false, _) => walker.walk_local(
            &cursor_path,
            &files,
            &prices,
            now_secs,
            &Default::default(),
            None,
            &HeadlineSpec::default(),
        ),
    };
    if publish {
        write_provider_spending_cache_with_rollups(
            &paths.shared_provider_spending_path(),
            unix_millis_now(),
            &result.spending,
            &result.days,
            &result.models,
        );
    }
    let Spending { total, by_provider } = result.spending;
    Stats {
        by_day: result.days,
        by_model: result.models,
        by_agent: by_provider,
        total,
    }
}

fn ensure_shared_runtime(paths: &RuntimePaths) -> Result<()> {
    let rimz_root = paths
        .shared_root
        .parent()
        .ok_or_else(|| anyhow!("invalid Rimz shared runtime path"))?;
    let runtime_root = rimz_root
        .parent()
        .ok_or_else(|| anyhow!("invalid Rimz runtime path"))?;
    rimz::ledger::paths::ensure_private_runtime_dir(runtime_root)?;
    rimz::ledger::paths::ensure_private_runtime_dir(rimz_root)?;
    rimz::ledger::paths::ensure_private_runtime_dir(&paths.shared_root)?;
    Ok(())
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn load_cold_stats_with_spinner(paths: &RuntimePaths) -> Result<LoadedStats> {
    let geometry = PanelGeometry::current();
    emit(&header_lines(geometry.panel_width), geometry.outer, "\n")?;

    let file_count = discover_spending_files().len();
    let paths = paths.clone();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let progress_tx = tx.clone();
        let mut progress = |progress| {
            let _ = progress_tx.send(ColdStatsEvent::Progress(progress));
        };
        let mut walker = SpendingWalker::new();
        let stats = load_or_refresh_stats(&paths, Some(&mut progress), &mut walker);
        let _ = tx.send(ColdStatsEvent::Done(Box::new(stats)));
    });

    let stats = wait_for_cold_stats(rx, file_count);
    if worker.join().is_err() {
        return Err(anyhow!("stats worker panicked"));
    }
    let stats = stats?;
    Ok(LoadedStats {
        stats,
        header_printed: true,
    })
}

enum ColdStatsEvent {
    Progress(SpendProgress),
    Done(Box<Result<Stats>>),
}

fn wait_for_cold_stats(rx: mpsc::Receiver<ColdStatsEvent>, file_count: usize) -> Result<Stats> {
    let start = Instant::now();
    let mut frame = 0;
    let mut shown = false;
    let mut last_draw: Option<Instant> = None;
    let mut progress = SpendProgress {
        finished_files: 0,
        total_files: file_count,
    };
    loop {
        match rx.recv_timeout(SPINNER_TICK) {
            Ok(ColdStatsEvent::Done(stats)) => {
                if shown {
                    clear_spinner_line()?;
                }
                return *stats;
            }
            Ok(ColdStatsEvent::Progress(next)) => {
                progress = next;
                if start.elapsed() >= SPINNER_MIN_AGE
                    && last_draw.is_none_or(|draw| draw.elapsed() >= SPINNER_TICK)
                {
                    draw_progress(&mut frame, progress, &mut last_draw)?;
                    shown = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if start.elapsed() < SPINNER_MIN_AGE {
                    continue;
                }
                draw_progress(&mut frame, progress, &mut last_draw)?;
                shown = true;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if shown {
                    clear_spinner_line()?;
                }
                return Err(anyhow!("stats worker exited before sending a result"));
            }
        }
    }
}

fn draw_progress(
    frame: &mut usize,
    progress: SpendProgress,
    last_draw: &mut Option<Instant>,
) -> Result<()> {
    write_progress_line(SPINNER_FRAMES[*frame % SPINNER_FRAMES.len()], progress)?;
    *frame += 1;
    *last_draw = Some(Instant::now());
    Ok(())
}

fn should_animate_cold_stats(human: bool, stdout_tty: bool, stderr_tty: bool) -> bool {
    human && stdout_tty && stderr_tty
}

fn write_progress_line(frame: char, progress: SpendProgress) -> Result<()> {
    let total = progress.total_files;
    let done = progress.finished_files.min(total);
    let plural = if total == 1 { "" } else { "s" };
    let count_width = total.max(1).to_string().len();
    let bar = progress_bar(done, total);
    let mut stderr = std::io::stderr().lock();
    write!(
        stderr,
        "\r{frame} Reading session file{plural} [{bar}] {done:>count_width$}/{total}"
    )?;
    stderr.flush()?;
    Ok(())
}

fn progress_bar(done: usize, total: usize) -> String {
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

fn clear_spinner_line() -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "\r\x1b[K")?;
    stderr.flush()?;
    Ok(())
}

// ── The grid ───────────────────────────────────────────────────────────────────

/// The metric a cell scales by, per mode.
fn metric(day: &DaySpend, dollars: bool) -> f64 {
    if dollars { day.usd } else { day.tokens as f64 }
}

/// Day of week with Sunday = 0, GitHub's column start. Epoch day 0 is a
/// Thursday (= 4).
fn dow_sun0(day: i64) -> i64 {
    ((day % 7) + 4).rem_euclid(7)
}

/// The Sunday that opens the week containing `day`.
fn week_start(day: i64) -> i64 {
    day - dow_sun0(day)
}

struct Grid {
    weeks: usize,
    today_day: i64,
    /// `cells[col][row]` metric for an in-range day; `None` for a future day in
    /// the current week, drawn blank like GitHub.
    cells: Vec<[Option<f64>; 7]>,
    max: f64,
}

impl Grid {
    fn build(
        by_day: &BTreeMap<i64, DaySpend>,
        today_day: i64,
        weeks: usize,
        dollars: bool,
    ) -> Self {
        let last_sunday = week_start(today_day);
        let mut cells = Vec::with_capacity(weeks);
        let mut max = 0.0_f64;
        for col in 0..weeks {
            let col_sunday = last_sunday - ((weeks - 1 - col) as i64) * 7;
            let mut week = [None; 7];
            for (row, slot) in week.iter_mut().enumerate() {
                let day = col_sunday + row as i64;
                if day > today_day {
                    continue;
                }
                let value = by_day.get(&day).map(|d| metric(d, dollars)).unwrap_or(0.0);
                *slot = Some(value);
                max = max.max(value);
            }
            cells.push(week);
        }
        Self {
            weeks,
            today_day,
            cells,
            max,
        }
    }

    fn col_sunday(&self, col: usize) -> i64 {
        week_start(self.today_day) - ((self.weeks - 1 - col) as i64) * 7
    }
}

/// Map a cell value onto a ramp index `0..=4`, scaled to the busiest day in
/// view, so the texture reads against your own rhythm.
fn level(value: f64, max: f64) -> usize {
    if max <= 0.0 {
        return 0;
    }
    ((value / max) * 4.0).round().clamp(0.0, 4.0) as usize
}

// ── Rendering ────────────────────────────────────────────────────────────────

struct PanelGeometry {
    weeks: usize,
    panel_width: usize,
    outer: usize,
}

impl PanelGeometry {
    fn current() -> Self {
        let cols = term_cols();
        let weeks = weeks_for_terminal(cols);
        let panel_width = GUTTER + weeks * 2;
        let outer = cols.saturating_sub(panel_width) / 2;
        PanelGeometry {
            weeks,
            panel_width,
            outer,
        }
    }
}

fn render_panel(
    stats: &Stats,
    today_day: i64,
    dollars: bool,
    glyphs: &PanelGlyphs,
    include_header: bool,
    nl: &str,
    active: Option<Window>,
) -> Result<()> {
    let geometry = PanelGeometry::current();
    let mut lines: Vec<String> = Vec::new();
    if include_header {
        lines.extend(header_lines(geometry.panel_width));
    }

    if stats.by_day.is_empty() {
        let message = "No token usage recorded yet - run an agent and check back.";
        lines.push(center(
            &render::paint(muted(), message),
            message.chars().count(),
            geometry.panel_width,
        ));
        return emit(&lines, geometry.outer, nl);
    }

    heatmap_lines(&mut lines, stats, today_day, geometry.weeks, dollars);
    let selected = active.unwrap_or(Window::AllTime);
    let models = model_breakdown(stats, selected);
    let agents = agent_breakdown(stats, selected);
    let name_w = models
        .iter()
        .map(|(name, _)| display_width(name))
        .chain(agents.iter().map(|agent| display_width(&agent.name)))
        .max()
        .unwrap_or(0);

    lines.push(String::new());
    windows_lines(&mut lines, stats, active);
    let model_rows = model_cells(&models, name_w, glyphs);
    let agent_rows = agent_cells(&agents, name_w, glyphs);
    let pct_w = stat_pct_width(&model_rows, &agent_rows);
    let layout = stat_section_layout(&model_rows, &agent_rows, pct_w, geometry.panel_width);
    if !model_rows.is_empty() {
        lines.push(String::new());
        emit_stat_section(&mut lines, "Models", &model_rows, layout, glyphs);
    }
    if !agent_rows.is_empty() {
        lines.push(String::new());
        emit_stat_section(&mut lines, "Agents", &agent_rows, layout, glyphs);
    }
    lines.push(String::new());
    insights_lines(&mut lines, stats, today_day, geometry.panel_width, selected);

    emit(&lines, geometry.outer, nl)
}

fn header_lines(panel_width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    lines.push(String::new());
    // Indent the wordmark as one block (a single shared pad), so its lines stay
    // internally aligned rather than each centring to its own width.
    let art_width = WORDMARK
        .lines()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0);
    let wm_indent = " ".repeat(panel_width.saturating_sub(art_width) / 2);
    for line in WORDMARK.lines() {
        lines.push(format!("{wm_indent}{}", render::paint(brand(), line)));
    }
    lines.push(center(
        &render::paint(muted(), TAGLINE),
        TAGLINE.chars().count(),
        panel_width,
    ));
    lines.push(String::new());
    lines
}

/// Print the assembled panel, each line indented to centre the block on screen.
fn emit(lines: &[String], outer: usize, nl: &str) -> Result<()> {
    let pad = " ".repeat(outer);
    let mut out = render::out();
    for line in lines {
        if line.is_empty() {
            write!(out, "{nl}")?;
        } else {
            write!(out, "{pad}{line}{nl}")?;
        }
    }
    Ok(())
}

/// The heatmap: a header, the month row, seven weekday rows, and the ramp key.
fn heatmap_lines(
    lines: &mut Vec<String>,
    stats: &Stats,
    today_day: i64,
    weeks: usize,
    dollars: bool,
) {
    let grid = Grid::build(&stats.by_day, today_day, weeks, dollars);
    let header = if dollars {
        "Spend activity"
    } else {
        "Token activity"
    };
    lines.push(format!("  {}", render::paint(meta(), header)));
    lines.push(render::paint(muted(), &month_row(&grid)));

    let styles = ramp_styles();
    for row in 0..7 {
        let label = match row {
            1 => "Mon",
            3 => "Wed",
            5 => "Fri",
            _ => "",
        };
        let mut line = render::paint(muted(), &format!("  {label:<4}"));
        for week in &grid.cells {
            match week[row] {
                Some(value) => {
                    let lvl = level(value, grid.max);
                    line.push_str(&render::paint(styles[lvl], &RAMP[lvl].to_string()));
                    line.push(' ');
                }
                None => line.push_str("  "),
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.push(format!("  {}", ramp_key(&styles)));
}

/// The compact `Less · ░ ▒ ▓ █ More` key in the cool ramp.
fn ramp_key(styles: &[anstyle::Style; 5]) -> String {
    let mut s = format!("{} ", render::paint(muted(), "Less"));
    for (lvl, glyph) in RAMP.iter().enumerate() {
        s.push_str(&render::paint(styles[lvl], &glyph.to_string()));
        s.push(' ');
    }
    s.push_str(&render::paint(muted(), "More"));
    s
}

/// The windows row: a static totals row in reports, a tab bar in held dashboards.
fn windows_lines(lines: &mut Vec<String>, stats: &Stats, active: Option<Window>) {
    let cells = Window::TABS.map(|window| {
        let tokens = stats_tokens(&window.select(&stats.total));
        (window, window.label(), fmt_tokens(tokens))
    });
    let sep = render::paint(muted(), "  ·  ");
    let Some(active) = active else {
        let row = cells
            .into_iter()
            .map(|(_, label, tokens)| {
                format!(
                    "{} {}",
                    render::paint(muted(), label),
                    render::paint(cool(), &tokens)
                )
            })
            .collect::<Vec<_>>()
            .join(&sep);
        lines.push(format!("  {row}"));
        return;
    };

    let inner_w = cells
        .iter()
        .map(|(_, label, tokens)| label.chars().count() + 1 + tokens.chars().count())
        .max()
        .unwrap_or(0);
    let row = cells
        .into_iter()
        .map(|(window, label, tokens)| {
            let pad = " "
                .repeat(inner_w.saturating_sub(label.chars().count() + 1 + tokens.chars().count()));
            if window == active {
                render::paint(active_tab(), &format!(" {label} {tokens}{pad} "))
            } else {
                format!(
                    " {} {}{pad} ",
                    render::paint(muted(), label),
                    render::paint(cool(), &tokens)
                )
            }
        })
        .collect::<Vec<_>>()
        .join(&sep);
    lines.push(format!("  {row}"));
}

struct StatCell {
    left_full: String,
    left_compact: String,
    share_pct: f64,
}

impl StatCell {
    fn left(&self, compact: bool) -> &str {
        if compact {
            &self.left_compact
        } else {
            &self.left_full
        }
    }
}

#[derive(Clone, Copy)]
struct StatSectionLayout {
    compact: bool,
    left_w: usize,
    pct_w: usize,
    bar_w: usize,
}

/// The per-model token breakdown, before the shared share column is appended.
fn model_cells(
    models: &[(String, SpendWindow)],
    name_w: usize,
    glyphs: &PanelGlyphs,
) -> Vec<StatCell> {
    let total_usd: f64 = models.iter().map(|(_, model)| model.usd).sum();

    struct ModelRow {
        name: String,
        usd: String,
        input: String,
        output: String,
        cache_read: String,
        share_pct: f64,
    }

    let rows = models
        .iter()
        .map(|(name, spend)| {
            let pct = if total_usd > 0.0 {
                spend.usd / total_usd * 100.0
            } else {
                0.0
            };
            ModelRow {
                name: name.clone(),
                usd: fmt_usd(spend.usd),
                input: fmt_tokens_lower(spend.input),
                output: fmt_tokens_lower(spend.output),
                cache_read: fmt_tokens_lower(spend.cache_read),
                share_pct: pct,
            }
        })
        .collect::<Vec<_>>();
    let usd_w = rows
        .iter()
        .map(|row| display_width(&row.usd))
        .max()
        .unwrap_or(0);
    let input_w = rows
        .iter()
        .map(|row| display_width(&row.input))
        .max()
        .unwrap_or(0);
    let output_w = rows
        .iter()
        .map(|row| display_width(&row.output))
        .max()
        .unwrap_or(0);
    let cache_w = rows
        .iter()
        .map(|row| display_width(&row.cache_read))
        .max()
        .unwrap_or(0);
    let sep = render::paint(muted(), "·");

    rows.iter()
        .map(|row| {
            let name = pad_to(&render::paint(cool(), &row.name), name_w);
            let left_full = format!(
                "{} {name} {} {sep} {} {} {sep} {} {} {sep} {} {}",
                render::paint(cool(), "●"),
                pad_left(&row.usd, usd_w),
                render::paint(muted(), &glyphs.input),
                pad_left(&row.input, input_w),
                render::paint(muted(), &glyphs.output),
                pad_left(&row.output, output_w),
                render::paint(muted(), &glyphs.cache_read),
                pad_left(&row.cache_read, cache_w),
            );
            let left_compact = format!(
                "{} {name} {}",
                render::paint(cool(), "●"),
                pad_left(&row.usd, usd_w),
            );
            StatCell {
                left_full,
                left_compact,
                share_pct: row.share_pct,
            }
        })
        .collect()
}

fn model_breakdown(stats: &Stats, active: Window) -> Vec<(String, SpendWindow)> {
    let total: u64 = stats
        .by_model
        .values()
        .map(|tally| active.select(tally).tokens)
        .sum();
    if total == 0 {
        return Vec::new();
    }

    let mut named: Vec<(String, SpendWindow)> = Vec::new();
    let mut other = SpendWindow::default();
    for (id, tally) in &stats.by_model {
        let spend = active.select(tally);
        if spend.tokens == 0 {
            continue;
        }
        if id.is_empty() {
            fold_window(&mut other, &spend);
        } else {
            named.push((friendly_model(id), spend));
        }
    }
    named.sort_by(|a, b| {
        b.1.usd
            .partial_cmp(&a.1.usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.tokens.cmp(&a.1.tokens))
    });
    if named.len() > MAX_MODELS {
        for (_, spend) in named.split_off(MAX_MODELS) {
            fold_window(&mut other, &spend);
        }
    }
    if other.tokens > 0 {
        named.push(("Other".to_string(), other));
    }
    named
}

fn fold_window(acc: &mut SpendWindow, add: &SpendWindow) {
    acc.usd += add.usd;
    acc.tokens += add.tokens;
    acc.input += add.input;
    acc.output += add.output;
    acc.cache_write += add.cache_write;
    acc.cache_read += add.cache_read;
    acc.sessions += add.sessions;
}

fn stats_tokens(window: &SpendWindow) -> u64 {
    window.tokens + window.cache_read
}

struct AgentBreakdown<'a> {
    kind: &'a str,
    name: String,
    window: SpendWindow,
    share: f64,
}

fn agent_breakdown(stats: &Stats, active: Window) -> Vec<AgentBreakdown<'_>> {
    let total_sessions: u32 = stats
        .by_agent
        .values()
        .map(|tally| active.select(tally).sessions)
        .sum();

    let mut agents: Vec<_> = stats
        .by_agent
        .iter()
        .filter_map(|(kind, tally)| {
            let window = active.select(tally);
            (window.tokens > 0).then(|| AgentBreakdown {
                kind: kind.as_str(),
                name: agent_display_name(kind),
                window,
                share: if total_sessions > 0 {
                    window.sessions as f64 / total_sessions as f64
                } else {
                    0.0
                },
            })
        })
        .collect();
    agents.sort_by(|a, b| {
        b.window
            .sessions
            .cmp(&a.window.sessions)
            .then_with(|| b.window.tokens.cmp(&a.window.tokens))
    });
    agents
}

fn agent_cells(
    agents: &[AgentBreakdown<'_>],
    name_w: usize,
    glyphs: &PanelGlyphs,
) -> Vec<StatCell> {
    if agents.is_empty() {
        return Vec::new();
    }

    struct AgentRow {
        name: String,
        sessions: String,
        tokens: String,
        usd: String,
        share_pct: f64,
    }

    let rows = agents
        .iter()
        .map(|agent| AgentRow {
            name: agent.name.clone(),
            sessions: agent.window.sessions.to_string(),
            tokens: fmt_tokens(stats_tokens(&agent.window)),
            usd: fmt_usd(agent.window.usd),
            share_pct: agent.share * 100.0,
        })
        .collect::<Vec<_>>();
    let sess_w = rows
        .iter()
        .map(|row| display_width(&row.sessions))
        .max()
        .unwrap_or(0);
    let tok_w = rows
        .iter()
        .map(|row| display_width(&row.tokens))
        .max()
        .unwrap_or(0);
    let usd_w = rows
        .iter()
        .map(|row| display_width(&row.usd))
        .max()
        .unwrap_or(0);
    let sep = render::paint(muted(), "·");

    rows.iter()
        .map(|row| {
            let name = pad_to(&render::paint(cool(), &row.name), name_w);
            let left = format!(
                "{} {name} {} {} {sep} {} {} {sep} {}",
                render::paint(cool(), "●"),
                render::paint(muted(), &glyphs.sessions),
                pad_left(&row.sessions, sess_w),
                render::paint(muted(), &glyphs.total),
                pad_left(&row.tokens, tok_w),
                pad_left(&row.usd, usd_w),
            );
            StatCell {
                left_full: left.clone(),
                left_compact: left,
                share_pct: row.share_pct,
            }
        })
        .collect()
}

fn stat_pct_width(model_cells: &[StatCell], agent_cells: &[StatCell]) -> usize {
    model_cells
        .iter()
        .chain(agent_cells)
        .map(|cell| display_width(&format!("{:.1}", cell.share_pct)))
        .max()
        .unwrap_or(0)
}

fn stat_section_layout(
    model_cells: &[StatCell],
    agent_cells: &[StatCell],
    pct_w: usize,
    panel_width: usize,
) -> StatSectionLayout {
    let full_left_w = stat_left_width(model_cells, agent_cells, false);
    let prefix_w = stat_prefix_width(full_left_w, pct_w);
    if prefix_w + 1 + MIN_SHARE_BAR_WIDTH <= panel_width {
        StatSectionLayout {
            compact: false,
            left_w: full_left_w,
            pct_w,
            bar_w: panel_width - prefix_w - 1,
        }
    } else if prefix_w <= panel_width {
        StatSectionLayout {
            compact: false,
            left_w: full_left_w,
            pct_w,
            bar_w: 0,
        }
    } else {
        let compact_left_w = stat_left_width(model_cells, agent_cells, true);
        StatSectionLayout {
            compact: true,
            left_w: compact_left_w,
            pct_w,
            bar_w: 0,
        }
    }
}

fn stat_left_width(model_cells: &[StatCell], agent_cells: &[StatCell], compact: bool) -> usize {
    model_cells
        .iter()
        .chain(agent_cells)
        .map(|cell| display_width(cell.left(compact)))
        .max()
        .unwrap_or(0)
}

/// The stat row up to and including the `%`, before the share bar.
fn stat_prefix_width(left_w: usize, pct_w: usize) -> usize {
    2 + left_w + STAT_GUTTER + pct_w + 1
}

fn share_bar(share_pct: f64, width: usize, glyphs: &PanelGlyphs) -> String {
    let filled = ((share_pct / 100.0) * width as f64)
        .round()
        .clamp(0.0, width as f64) as usize;
    format!(
        "{}{}",
        render::paint(cool(), &glyphs.bar_filled.repeat(filled)),
        render::paint(
            rgb(Semantic::DEFAULT.faint),
            &glyphs.bar_track.repeat(width - filled),
        ),
    )
}

fn emit_stat_section(
    lines: &mut Vec<String>,
    header: &str,
    cells: &[StatCell],
    layout: StatSectionLayout,
    glyphs: &PanelGlyphs,
) {
    if cells.is_empty() {
        return;
    }

    lines.push(format!("  {}", render::paint(meta(), header)));
    let gutter = " ".repeat(STAT_GUTTER);
    for cell in cells {
        let pct = format!("{:.1}", cell.share_pct);
        let mut line = format!(
            "  {}{gutter}{}%",
            pad_to(cell.left(layout.compact), layout.left_w),
            pad_left(&pct, layout.pct_w),
        );
        if layout.bar_w > 0 {
            line.push(' ');
            line.push_str(&share_bar(cell.share_pct, layout.bar_w, glyphs));
        }
        lines.push(line);
    }
}

fn agent_display_name(kind: &str) -> String {
    rimz::agents::descriptor_by_kind(kind)
        .map(|descriptor| descriptor.display_name.to_owned())
        .unwrap_or_else(|| capitalize(kind))
}

/// Sessions, active-day ratio, most active day, and longest / current streak.
fn insights_lines(
    lines: &mut Vec<String>,
    stats: &Stats,
    today_day: i64,
    panel_width: usize,
    active: Window,
) {
    let activity = Activity::of(&stats.by_day, today_day, active);
    let selected = active.select(&stats.total);
    lines.push(format!(
        "  {} {}",
        render::paint(muted(), "Sessions:"),
        group_thousands(selected.sessions as u64)
    ));

    let most = activity
        .most_active
        .map(fmt_day)
        .unwrap_or_else(|| "—".to_string());
    let left = [
        kv(
            "Active days:",
            &format!("{}/{}", activity.active_count, activity.window_days),
        ),
        kv("Most active day:", &most),
    ];
    let right = [
        kv("Longest streak:", &plural_days(activity.longest_streak)),
        kv("Current streak:", &plural_days(activity.current_streak)),
    ];

    let insight_gutter = 6;
    let split = left
        .iter()
        .map(|line| display_width(line))
        .max()
        .unwrap_or(0)
        + insight_gutter;
    let right_w = right
        .iter()
        .map(|line| display_width(line))
        .max()
        .unwrap_or(0);
    if split + right_w <= panel_width {
        for (l, r) in left.iter().zip(right.iter()) {
            lines.push(
                format!("  {}{}", pad_to(l, split), r)
                    .trim_end()
                    .to_string(),
            );
        }
    } else {
        for line in left.into_iter().chain(right) {
            lines.push(format!("  {line}"));
        }
    }
}

/// A muted `label` followed by its value — the insight line shape.
fn kv(label: &str, value: &str) -> String {
    format!("{} {value}", render::paint(muted(), label))
}

/// A day count with a pluralized unit: `1 day`, `27 days`.
fn plural_days(n: u32) -> String {
    format!("{n} day{}", if n == 1 { "" } else { "s" })
}

/// The month-abbrev header: a label sits over the column where each new month
/// begins, like the GitHub graph.
fn month_row(grid: &Grid) -> String {
    let mut buf = vec![' '; grid.weeks * 2];
    let months = (0..grid.weeks)
        .map(|col| {
            let date = utc_date(grid.col_sunday(col).max(0) as u64 * DAY_SECS as u64);
            date.get(5..7)
                .and_then(|m| m.parse::<usize>().ok())
                .filter(|m| (1..=12).contains(m))
        })
        .collect::<Vec<_>>();

    let mut col = 0;
    while col < months.len() {
        let month = months[col];
        let mut end = col + 1;
        while end < months.len() && months[end] == month {
            end += 1;
        }
        if end - col >= 2
            && let Some(idx) = month
        {
            let start = col * 2;
            for (i, ch) in MONTHS[idx - 1].chars().enumerate() {
                if let Some(slot) = buf.get_mut(start + i) {
                    *slot = ch;
                }
            }
        }
        col = end;
    }
    let cells: String = buf.into_iter().collect();
    format!("{:<GUTTER$}{}", "", cells)
}

/// Centre `text` (of `visible` printable columns) within `width`, padding left.
fn center(text: &str, visible: usize, width: usize) -> String {
    let pad = width.saturating_sub(visible) / 2;
    format!("{}{text}", " ".repeat(pad))
}

/// `s` right-padded to `width` printable columns (ANSI-aware), for column layout.
fn pad_to(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(display_width(s))))
}

/// `s` left-padded to `width` printable columns (ANSI-aware).
fn pad_left(s: &str, width: usize) -> String {
    format!("{}{s}", " ".repeat(width.saturating_sub(display_width(s))))
}

/// Display width of a string, skipping ANSI SGR escapes.
fn display_width(s: &str) -> usize {
    let mut count = 0;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            count += UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    count
}

// ── Trailing windows and activity ──────────────────────────────────────────────

/// Cadence read from the per-day history: the selected-window active ratio, the
/// heaviest single day, and the longest and current active-day streaks.
struct Activity {
    active_count: u32,
    window_days: u32,
    most_active: Option<i64>,
    longest_streak: u32,
    current_streak: u32,
}

impl Activity {
    fn of(by_day: &BTreeMap<i64, DaySpend>, today_day: i64, window: Window) -> Self {
        let active: BTreeSet<i64> = by_day
            .iter()
            .filter(|(_, d)| d.tokens > 0)
            .map(|(&day, _)| day)
            .collect();

        if window == Window::AllTime {
            let active_count = (today_day - 27..=today_day)
                .filter(|day| active.contains(day))
                .count() as u32;
            let most_active = by_day
                .iter()
                .filter(|(_, d)| d.tokens > 0)
                .max_by_key(|(_, d)| d.tokens)
                .map(|(&day, _)| day);
            let (longest_streak, current_streak) = streaks(&active, today_day);
            return Activity {
                active_count,
                window_days: window.span_days(),
                most_active,
                longest_streak,
                current_streak,
            };
        }

        let span = window.span_days();
        let cutoff = today_day - (span as i64 - 1);
        let scoped_active: BTreeSet<i64> = active
            .into_iter()
            .filter(|day| *day >= cutoff && *day <= today_day)
            .collect();
        let active_count = scoped_active.len() as u32;

        let most_active = by_day
            .iter()
            .filter(|(day, d)| **day >= cutoff && **day <= today_day && d.tokens > 0)
            .max_by_key(|(_, d)| d.tokens)
            .map(|(&day, _)| day);
        let (longest_streak, current_streak) = streaks(&scoped_active, today_day);

        Self {
            active_count,
            window_days: span,
            most_active,
            longest_streak,
            current_streak,
        }
    }
}

fn streaks(active: &BTreeSet<i64>, today_day: i64) -> (u32, u32) {
    // Longest run of consecutive active days in the selected set.
    let mut longest_streak = 0;
    let mut run = 0;
    let mut prev: Option<i64> = None;
    for &day in active {
        run = if prev == Some(day - 1) { run + 1 } else { 1 };
        longest_streak = longest_streak.max(run);
        prev = Some(day);
    }

    // Current run ending at today; a today with no activity yet does not break
    // a streak that ran through yesterday.
    let mut cursor = if active.contains(&today_day) {
        today_day
    } else {
        today_day - 1
    };
    let mut current_streak = 0;
    while active.contains(&cursor) {
        current_streak += 1;
        cursor -= 1;
    }

    (longest_streak, current_streak)
}

// ── Styling ────────────────────────────────────────────────────────────────────

fn rgb(rgb: (u8, u8, u8)) -> anstyle::Style {
    anstyle::Style::new().fg_color(Some(rgb_color(rgb)))
}

fn rgb_color(rgb: (u8, u8, u8)) -> anstyle::Color {
    anstyle::Color::Rgb(anstyle::RgbColor(rgb.0, rgb.1, rgb.2))
}

fn brand() -> anstyle::Style {
    rgb(Semantic::DEFAULT.accent).bold()
}

fn muted() -> anstyle::Style {
    rgb(Semantic::DEFAULT.muted)
}

fn meta() -> anstyle::Style {
    rgb(Semantic::DEFAULT.meta)
}

fn cool() -> anstyle::Style {
    rgb(Semantic::DEFAULT.cool)
}

fn active_tab() -> anstyle::Style {
    anstyle::Style::new()
        .fg_color(Some(rgb_color(Semantic::DEFAULT.selection_bg)))
        .bg_color(Some(rgb_color(Semantic::DEFAULT.cool)))
        .bold()
}

/// One cool ramp, lightness-varying, held distinct from the status reds and
/// greens so a busy day reads as volume, not as good or wrong. Density carries
/// the reading under `NO_COLOR`; this only reinforces it.
fn ramp_styles() -> [anstyle::Style; 5] {
    let scale = |(r, g, b): (u8, u8, u8), f: f32| {
        (
            (r as f32 * f) as u8,
            (g as f32 * f) as u8,
            (b as f32 * f) as u8,
        )
    };
    let cool = Semantic::DEFAULT.cool;
    [
        rgb(Semantic::DEFAULT.faint),
        rgb(scale(cool, 0.50)),
        rgb(scale(cool, 0.66)),
        rgb(scale(cool, 0.82)),
        rgb(cool),
    ]
}

// ── Model names ────────────────────────────────────────────────────────────────

/// A transcript model id rendered for people: `claude-opus-4-8` → `Opus 4.8`,
/// `gpt-5-codex` → `GPT-5 Codex`. An 8-digit date suffix is dropped; an
/// unrecognised id falls back to itself.
fn friendly_model(id: &str) -> String {
    let id = strip_date_suffix(id.trim());
    if let Some(rest) = id.strip_prefix("claude-") {
        let mut parts = rest.splitn(2, '-');
        let family = capitalize(parts.next().unwrap_or(rest));
        return match parts.next() {
            Some(ver) if !ver.is_empty() => format!("{family} {}", ver.replace('-', ".")),
            _ => family,
        };
    }
    let parts: Vec<&str> = id.split('-').collect();
    if parts.first() == Some(&"gpt") && parts.len() >= 2 {
        let mut name = format!("GPT-{}", parts[1]);
        for token in &parts[2..] {
            name.push(' ');
            name.push_str(&capitalize(token));
        }
        return name;
    }
    id.to_string()
}

/// Drop a trailing `-YYYYMMDD` 8-digit date stamp, leaving the base model id.
fn strip_date_suffix(id: &str) -> &str {
    match id.rsplit_once('-') {
        Some((base, tail)) if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => id,
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

// ── Number formatting ──────────────────────────────────────────────────────────

/// Tokens as `412M`, `5.2B`, `950K`. Billions keep one decimal; smaller units
/// round.
fn fmt_tokens(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.0}M", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.0}K", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}

/// Tokens at one-decimal precision in lowercase units (`61.0m`, `1.2b`), the
/// finer register the per-model In/Out lines read in.
fn fmt_tokens_lower(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1}b", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}m", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.1}k", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}

/// Dollars as `$8,666` — rounded, thousands grouped.
fn fmt_usd(v: f64) -> String {
    let whole = v.round() as i64;
    let sign = if whole < 0 { "-" } else { "" };
    format!("{sign}${}", group_thousands(whole.unsigned_abs()))
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped
}

/// A day key (days since the epoch) as `May 29`.
fn fmt_day(day: i64) -> String {
    let date = utc_date(day.max(0) as u64 * DAY_SECS as u64);
    let month = date
        .get(5..7)
        .and_then(|m| m.parse::<usize>().ok())
        .filter(|m| (1..=12).contains(m));
    let dom = date.get(8..10).and_then(|d| d.parse::<u32>().ok());
    match (month, dom) {
        (Some(m), Some(d)) => format!("{} {d}", MONTHS[m - 1]),
        _ => date,
    }
}

/// Terminal width in columns; a non-TTY (a pipe) falls back to 80.
fn term_cols() -> usize {
    ratatui::crossterm::terminal::size()
        .map(|(c, _)| c as usize)
        .unwrap_or(80)
}

/// Heatmap span from the terminal width: wider screens show more weeks.
fn weeks_for_terminal(cols: usize) -> usize {
    (cols.saturating_sub(GUTTER) / 2).clamp(MIN_WEEKS, MAX_WEEKS)
}

// ── JSON ─────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct StatsJson {
    unit: &'static str,
    sessions: u32,
    active_days_28: u32,
    longest_streak: u32,
    current_streak: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    most_active_day: Option<String>,
    windows: WindowsJson,
    models: Vec<ModelJson>,
    agents: Vec<AgentJson>,
    days: Vec<DayJson>,
}

#[derive(Serialize)]
struct WindowsJson {
    week: WindowJson,
    month: WindowJson,
    year: WindowJson,
}

#[derive(Serialize)]
struct WindowJson {
    tokens: u64,
    usd: f64,
}

#[derive(Serialize)]
struct ModelJson {
    model: String,
    name: String,
    tokens: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    usd: f64,
    share: f64,
}

#[derive(Serialize)]
struct AgentJson {
    kind: String,
    name: String,
    tokens: u64,
    usd: f64,
    sessions: u32,
    share: f64,
}

#[derive(Serialize)]
struct DayJson {
    date: String,
    tokens: u64,
    usd: f64,
}

fn emit_json(stats: &Stats, today_day: i64, dollars: bool) -> Result<()> {
    let active = Window::AllTime;
    let activity = Activity::of(&stats.by_day, today_day, active);
    let total_usd: f64 = stats
        .by_model
        .values()
        .map(|tally| active.select(tally).usd)
        .sum();

    let mut models: Vec<ModelJson> = stats
        .by_model
        .iter()
        .map(|(id, tally)| {
            let spend = active.select(tally);
            ModelJson {
                model: id.clone(),
                name: if id.is_empty() {
                    "Other".to_string()
                } else {
                    friendly_model(id)
                },
                tokens: stats_tokens(&spend),
                input: spend.input,
                output: spend.output,
                cache_read: spend.cache_read,
                usd: spend.usd,
                share: if total_usd > 0.0 {
                    spend.usd / total_usd
                } else {
                    0.0
                },
            }
        })
        .collect();
    models.sort_by(|a, b| {
        b.usd
            .partial_cmp(&a.usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.tokens.cmp(&a.tokens))
    });

    let agents = agent_breakdown(stats, active)
        .into_iter()
        .map(|agent| AgentJson {
            kind: agent.kind.to_owned(),
            name: agent.name,
            tokens: stats_tokens(&agent.window),
            usd: agent.window.usd,
            sessions: agent.window.sessions,
            share: agent.share,
        })
        .collect();

    let days = stats
        .by_day
        .iter()
        .map(|(&day, spend)| DayJson {
            date: utc_date(day.max(0) as u64 * DAY_SECS as u64),
            tokens: spend.tokens,
            usd: spend.usd,
        })
        .collect();

    let doc = StatsJson {
        unit: if dollars { "usd" } else { "tokens" },
        sessions: stats.total.year.sessions,
        active_days_28: activity.active_count,
        longest_streak: activity.longest_streak,
        current_streak: activity.current_streak,
        most_active_day: activity
            .most_active
            .map(|day| utc_date(day.max(0) as u64 * DAY_SECS as u64)),
        windows: WindowsJson {
            week: WindowJson {
                tokens: stats_tokens(&stats.total.week),
                usd: stats.total.week.usd,
            },
            month: WindowJson {
                tokens: stats_tokens(&stats.total.month),
                usd: stats.total.month.usd,
            },
            year: WindowJson {
                tokens: stats_tokens(&stats.total.year),
                usd: stats.total.year.usd,
            },
        },
        models,
        agents,
        days,
    };
    let rendered = serde_json::to_string_pretty(&doc).expect("StatsJson serializes");
    #[expect(clippy::print_stdout, reason = "json emitter")]
    {
        println!("{rendered}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
