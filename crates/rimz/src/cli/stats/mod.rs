//! `rimz stats` — account-global spend, local-day activity, and model, agent,
//! and subagent breakdowns, readable in or out of a room or project.
//! The wordmark and activity insights remain in the held dashboard.
//! Internals: `docs/internals/stats.md`.
//!
//! Account-global: it reads the producer's published provider aggregate when
//! available. A standalone first launch takes the same shared spending election
//! as the sidebar producer, publishes the same provider-spending rollups, and
//! then subsequent runs return from a cache aggregated for the same local day.
//!
//! Windows: the heatmap always reads the full available history (the cache spans
//! the trailing year). `--window` scopes model, provider, and subagent breakdowns
//! in every mode, defaulting to today. It sets the held dashboard's initial tab.
//! `--refresh` recomputes on a background thread, retries an unavailable first
//! frame after five seconds, then repaints in place every minute and re-centres
//! promptly after a width change.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

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
use rimz::agents::spending::{
    DaySpend, ModelTally, ProviderSpendingCache, SpendProgress, SpendTally, SpendWindow, Spending,
    read_provider_spending_cache, refresh_global_spending_direct, unix_secs_now, utc_date,
};
use rimz::config::{GlyphRole, MachineConfig, ThemeConfig};
use rimz::disk::paths::logs_dir;
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
/// Breakdown rows shown before the tail folds into the final "Other" row.
const MAX_MODELS: usize = 6;
const MAX_AGENTS: usize = 6;
/// A named breakdown row must carry at least one percent of its section's
/// dollars or work tokens (input plus output, excluding cache reads).
const MIN_BREAKDOWN_SHARE: f64 = 0.01;
const SPINNER_MIN_AGE: Duration = Duration::from_millis(150);
const PROGRESS_BAR_WIDTH: usize = 20;
const MIN_SHARE_BAR_WIDTH: usize = 10;
const STAT_GUTTER: usize = 3;
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const EMPTY_REFRESH_RETRY: Duration = Duration::from_secs(5);
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
    /// Window for the model, agent, and subagent breakdowns (the Spend row always shows all four).
    #[arg(long, value_enum, default_value = "today", conflicts_with = "assists")]
    window: Window,
    /// Print effective system assists, newest first.
    #[arg(long, conflicts_with_all = ["subagents", "refresh"])]
    pub assists: bool,
    /// Show every subagent profile's spend in the selected window.
    #[arg(long, conflicts_with = "refresh")]
    pub subagents: bool,
    /// Number of assists to show; 0 shows all retained events.
    #[arg(short = 'n', default_value_t = 50, requires = "assists")]
    pub limit: usize,
    /// Include held and undelivered assist attempts.
    #[arg(long, requires = "assists")]
    pub all: bool,
    /// Open the held view, refreshing every 60s and re-centring on resize.
    #[arg(long, conflicts_with = "json")]
    pub refresh: bool,
    /// Keep the refreshing panel alive through Ctrl-C in the rimzd daemon view.
    /// Set only by the view.
    #[arg(long, hide = true, requires = "refresh")]
    pub hold: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
enum Window {
    Today,
    Week,
    Month,
    Year,
}

impl Window {
    const TABS: [Self; 4] = [Self::Today, Self::Week, Self::Month, Self::Year];

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
            Self::Today => "Today",
            Self::Week => "Week",
            Self::Month => "Month",
            Self::Year => "Year",
        }
    }

    fn select(self, tally: &SpendTally) -> SpendWindow {
        match self {
            Self::Today => tally.day.clone(),
            Self::Year => tally.year.clone(),
            Self::Week => tally.week.clone(),
            Self::Month => tally.month.clone(),
        }
    }

    fn span_days(self) -> u32 {
        match self {
            Self::Today => 1,
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
        }
    }
}

pub fn run(args: StatsArgs, _globals: &GlobalFlags) -> Result<()> {
    if args.assists {
        let assists = AssistStats::from_records(
            "all retained",
            rimz::harness::assist_log::recent(&logs_dir(), None),
        );
        return render_full(&assists, args.limit, args.all, args.json);
    }
    if args.refresh {
        return run_refresh(args.dollars, args.hold, args.window);
    }
    let stats = load_stats(!args.json)?;
    let today_day = stats.today_day(unix_secs_now());
    if args.subagents {
        if args.json {
            return json::emit_subagents(&stats, args.window);
        }
        return panel::render_spend(
            &mut render::out(),
            &stats,
            today_day,
            args.dollars,
            true,
            args.window,
            "\n",
        );
    }
    if args.json {
        let assists =
            AssistStats::load(&logs_dir(), args.window, Timestamp::now(), &stats.timezone);
        return emit_json(&stats, &assists, today_day, args.dollars, args.window);
    }
    panel::render_spend(
        &mut render::out(),
        &stats,
        today_day,
        args.dollars,
        false,
        args.window,
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
#[cfg(test)]
use {assists::*, fmt::*, hold::*, panel::*};
/// The deduplicated account-global history `rimz stats` renders: per-day buckets,
/// the per-model and per-agent breakdowns, and producer-priced trailing windows.
struct Stats {
    timezone: String,
    refreshed_at_ms: u64,
    day_cutoff_secs: u64,
    subagents: BTreeMap<String, SpendTally>,
    recorded_since_secs: Option<u64>,
    by_day: BTreeMap<i64, DaySpend>,
    by_model: BTreeMap<String, ModelTally>,
    by_agent: BTreeMap<String, SpendTally>,
    total: SpendTally,
}

impl Stats {
    fn from_provider(cache: ProviderSpendingCache) -> Self {
        let ProviderSpendingCache {
            days,
            models,
            spending,
            timezone,
            refreshed_at_ms,
            day_cutoff_secs,
            roles,
            ..
        } = cache;
        let Spending {
            total, by_provider, ..
        } = spending;
        Stats {
            timezone,
            refreshed_at_ms,
            day_cutoff_secs,
            subagents: roles.subagents,
            recorded_since_secs: roles.recorded_since_secs,
            by_day: days,
            by_model: models,
            by_agent: by_provider,
            total,
        }
    }

    fn today_day(&self, now_secs: u64) -> i64 {
        rimz::agents::spending::local_day_index(now_secs, Some(&self.timezone)).unwrap_or(0)
    }

    fn as_of(&self) -> jiff::Zoned {
        Timestamp::from_millisecond(self.refreshed_at_ms as i64)
            .unwrap_or(Timestamp::UNIX_EPOCH)
            .to_zoned(jiff::tz::TimeZone::get(&self.timezone).unwrap_or(jiff::tz::TimeZone::UTC))
    }

    fn published_today(&self) -> bool {
        self.today_day(self.day_cutoff_secs) == self.today_day(unix_secs_now())
    }

    fn recorded_since(&self) -> Option<jiff::Zoned> {
        Some(
            Timestamp::from_second(i64::try_from(self.recorded_since_secs?).ok()?)
                .ok()?
                .to_zoned(
                    jiff::tz::TimeZone::get(&self.timezone).unwrap_or(jiff::tz::TimeZone::UTC),
                ),
        )
    }
}

fn load_stats(human: bool) -> Result<Stats> {
    let paths = RuntimePaths::shared();
    load_stats_from_paths(&paths, human)
}

fn load_stats_from_paths(paths: &RuntimePaths, human: bool) -> Result<Stats> {
    ensure_shared_runtime(paths)?;
    if let Some(stats) = load_today_stats(paths) {
        return Ok(stats);
    }

    if should_animate_cold_stats(
        human,
        std::io::stdout().is_terminal(),
        std::io::stderr().is_terminal(),
    ) {
        return load_cold_stats_with_spinner(paths);
    }

    load_or_refresh_stats(paths, None)
}

fn load_today_stats(paths: &RuntimePaths) -> Option<Stats> {
    load_published_stats(paths).filter(Stats::published_today)
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
) -> Result<Stats> {
    if let Some(stats) = load_today_stats(paths) {
        return Ok(stats);
    }
    let spec = MachineConfig::load_lenient().headline_spec();
    let provider = if let Some(progress) = progress {
        refresh_global_spending_direct(paths, &spec, progress)
    } else {
        refresh_global_spending_direct(paths, &spec, &mut |_| {})
    };
    provider
        .is_current_version()
        .then(|| Stats::from_provider(provider))
        .filter(Stats::published_today)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Spending refresh unavailable: could not read today's figures from session files; check their availability and retry"
            )
        })
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
    load_or_refresh_stats(paths, Some(progress))
}

fn ensure_shared_runtime(paths: &RuntimePaths) -> Result<()> {
    paths.ensure_shared_dirs()?;
    Ok(())
}

#[cfg(test)]
mod tests;
