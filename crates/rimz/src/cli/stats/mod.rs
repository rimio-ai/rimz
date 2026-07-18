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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use clap::Args;
use jiff::Timestamp;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::Serialize;
use unicode_width::UnicodeWidthChar;

use super::GlobalFlags;
use crate::cli::render;
use crate::cli::spinner::Spinner;
use rimz::RuntimePaths;
use rimz::agents::AgentAdapter;
use rimz::agents::pricing;
use rimz::agents::spending::{
    DaySpend, ProviderSpendingCache, SilentWalk, SpendProgress, SpendTally, SpendWindow, Spending,
    SpendingWalker, WalkObserver, WalkRequest, read_provider_spending_cache, unix_secs_now,
    user_input, utc_date, write_provider_spending_cache_with_day,
};
use rimz::config::{GlyphRole, MachineConfig, ThemeConfig};
use rimz::store::paths::state_home;
use rimz::store::single_flight::{Coalesced, coalesce};
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard};

const DAY_SECS: i64 = 86_400;
/// The density ramp: `·` marks no usage; the four shades climb through active days.
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
    /// Print the complete system-assist timeline instead of the dashboard.
    #[arg(long, conflicts_with_all = ["json", "refresh"])]
    pub assists: bool,
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
    if args.assists {
        let assists = AssistStats::load(&state_home(), Window::AllTime, Timestamp::now());
        return render_full(&assists);
    }
    if args.refresh {
        return run_refresh(args.dollars, args.hold);
    }
    let loaded = load_stats(!args.json)?;
    let today_day = unix_secs_now() as i64 / DAY_SECS;
    let assists = AssistStats::load(&state_home(), Window::AllTime, Timestamp::now());
    if args.json {
        return emit_json(&loaded.stats, &assists, today_day, args.dollars);
    }
    let glyphs = resolve_panel_glyphs(&super::machine_config().theme);
    let mut out = render::out();
    render_panel(
        &mut out,
        panel::PanelStats {
            usage: &loaded.stats,
            assists: &assists,
            active: None,
        },
        today_day,
        args.dollars,
        &glyphs,
        !loaded.header_printed,
        "\n",
    )
}

mod assists;
mod fmt;
mod hold;
mod json;
mod panel;

use assists::{AssistStats, render_full};
use hold::{load_cold_stats_with_spinner, run_refresh, should_animate_cold_stats};
use json::emit_json;
use panel::{render_panel, resolve_panel_glyphs};
#[cfg(test)]
use {assists::*, fmt::*, hold::*, panel::*};
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
    mut progress: Option<&mut dyn FnMut(SpendProgress)>,
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
        Coalesced::Produce(_guard) => {
            let now_secs = unix_secs_now();
            let files = walker.discover_spending_files(now_secs);
            if let Some(progress) = progress.as_deref_mut() {
                progress(SpendProgress {
                    finished_files: 0,
                    total_files: files.len(),
                });
            }
            Ok(compute_stats_from_files_at(
                paths, files, true, progress, walker, now_secs,
            ))
        }
        Coalesced::ProduceLocal => {
            let now_secs = unix_secs_now();
            let files = walker.discover_spending_files(now_secs);
            if let Some(progress) = progress.as_deref_mut() {
                progress(SpendProgress {
                    finished_files: 0,
                    total_files: files.len(),
                });
            }
            Ok(compute_stats_from_files_at(
                paths, files, false, progress, walker, now_secs,
            ))
        }
    }
}

fn load_or_refresh_stats_via_service(paths: &RuntimePaths) -> Result<Stats> {
    let request = rimz::agents::spending::service::SpendingServiceRequest::global(
        paths,
        MachineConfig::load_lenient().headline_spec(),
    );
    let caches = rimz::agents::spending::service::request(
        paths,
        request,
        rimz::agents::spending::service::SpendingServiceStartup::HostEligible,
    );
    match caches {
        Ok(caches) => Ok(Stats::from_provider(caches.provider)),
        Err(error) => load_published_stats(paths).ok_or_else(|| error.into()),
    }
}

/// Bounded one-shot fallback used by the cold stats spinner. Long-lived held
/// refreshes use the elected service and therefore keep no local walker.
fn load_direct_stats_with_progress(
    paths: &RuntimePaths,
    progress: &mut dyn FnMut(SpendProgress),
) -> Result<Stats> {
    let mut walker = SpendingWalker::new();
    load_or_refresh_stats(paths, Some(progress), &mut walker)
}

struct ProgressObserver<'a>(&'a mut dyn FnMut(SpendProgress));

impl WalkObserver for ProgressObserver<'_> {
    fn on_file(&mut self, progress: SpendProgress) {
        (self.0)(progress);
    }
}

#[cfg(test)]
fn compute_stats_from_files(
    paths: &RuntimePaths,
    files: Vec<(&'static dyn AgentAdapter, PathBuf)>,
    publish: bool,
    progress: Option<&mut dyn FnMut(SpendProgress)>,
    walker: &mut SpendingWalker,
) -> Stats {
    compute_stats_from_files_at(paths, files, publish, progress, walker, unix_secs_now())
}

fn compute_stats_from_files_at(
    paths: &RuntimePaths,
    files: Vec<(&'static dyn AgentAdapter, PathBuf)>,
    publish: bool,
    progress: Option<&mut dyn FnMut(SpendProgress)>,
    walker: &mut SpendingWalker,
    now_secs: u64,
) -> Stats {
    let cursor_path = paths.shared_spending_cursor_path();
    let prices = if publish {
        let unknowns = walker.recorded_unknown_models(&cursor_path, &files, now_secs);
        Arc::new(pricing::load_for_spending(
            &paths.shared_pricing_cache_path(),
            &unknowns,
        ))
    } else {
        pricing::cached_book(&paths.shared_pricing_cache_path())
    };
    let origin_overrides = HashMap::new();
    let user_inputs = user_input::load();
    let spec = MachineConfig::load_lenient().headline_spec();
    let req = WalkRequest {
        files: &files,
        prices: &prices,
        now_secs,
        origin_overrides: &origin_overrides,
        user_inputs: &user_inputs,
        scope: None,
        spec: &spec,
    };
    let result = match (publish, progress) {
        (true, Some(progress)) => {
            let mut observer = ProgressObserver(progress);
            walker.walk(&cursor_path, &req, &mut observer)
        }
        (true, None) => {
            let mut observer = SilentWalk;
            walker.walk(&cursor_path, &req, &mut observer)
        }
        (false, _) => {
            let mut observer = SilentWalk;
            walker.walk_local(&cursor_path, &req, &mut observer)
        }
    };
    if publish {
        write_provider_spending_cache_with_day(
            &paths.shared_provider_spending_path(),
            unix_millis_now(),
            &result.spending,
            &result.days,
            &result.models,
            &result.provider_day,
            result.day_cutoff_secs,
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
    paths.ensure_shared_dirs()?;
    Ok(())
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
