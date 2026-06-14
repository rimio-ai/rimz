//! Rimz CLI entry point.
//!
//! `main.rs` is the only place in the workspace allowed to use `anyhow` and
//! to install the `tracing_subscriber`. Library modules return typed errors
//! and emit `tracing` events; this wrapper turns them into a process exit.

#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

mod cli;

use anyhow::Result;
use rimz::observability;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

const DEFAULT_LOG_FILTER: &str = "warn";
const SIDEBAR_SERVE_LOG_FILTER: &str = "off";

fn main() -> Result<()> {
    // Start digesting the executable off-thread so the build-id Sentry tag is
    // usually ready by the time `dispatch` sets the command scope.
    rimz::build_id::warm();
    // Sentry is created before the subscriber so its bridge layer attaches to a
    // live client; the guard is held for the whole process and flushes on exit.
    let reporting = observability::init();
    install_tracing(reporting.enabled());
    reporting.report();
    cli::dispatch()
}

fn install_tracing(report_to_sentry: bool) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_log_filter()))
        .unwrap_or_else(|_| EnvFilter::new("warn"));
    // The env filter is per-layer on the fmt sink, so the sidebar's `off`
    // silences stderr without gating the Sentry layer's own `WARN` capture.
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter);
    let sentry_layer = report_to_sentry.then(observability::sentry_tracing_layer);
    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(sentry_layer)
        .init();
}

fn default_log_filter() -> &'static str {
    let mut saw_sidebar = false;
    for arg in std::env::args().skip(1) {
        if saw_sidebar && arg == "serve" {
            return SIDEBAR_SERVE_LOG_FILTER;
        }
        saw_sidebar = arg == "sidebar";
    }
    DEFAULT_LOG_FILTER
}
