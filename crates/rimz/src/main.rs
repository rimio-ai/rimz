//! RimZ CLI entry point.
//!
//! The private CLI tree is the RimZ binary's `anyhow` boundary, and `main.rs`
//! installs the `tracing_subscriber`. Library modules return typed errors and
//! emit `tracing` events; this wrapper renders failures and selects an exit
//! code.

#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

mod cli;

use rimz::observability;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

const DEFAULT_LOG_FILTER: &str = "warn";
const RENDERED_PANE_LOG_FILTER: &str = "off";

fn main() -> std::process::ExitCode {
    // Completion runs before observability and build-id startup: every TAB is
    // latency-sensitive, and clap_complete owns stdout for this request.
    cli::complete_env();
    // Start reading the executable identity off-thread so the build-id Sentry
    // tag is usually ready by the time `dispatch` sets the command scope.
    rimz::build_id::warm();
    // Sentry is created before the subscriber so its reporting layer attaches to a
    // live client; the guard is held for the whole process and flushes on exit.
    let reporting = observability::init();
    install_tracing(reporting.enabled());
    reporting.report();
    match cli::dispatch() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            cli::report(&error);
            std::process::ExitCode::FAILURE
        }
    }
}

fn install_tracing(report_to_sentry: bool) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_log_filter()))
        .unwrap_or_else(|_| EnvFilter::new("warn"));
    // The env filter is per-layer on the fmt sink, so rendered panes silence
    // stderr without gating the Sentry layer's own `WARN` capture.
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
    log_filter_for(std::env::args().skip(1))
}

fn log_filter_for(args: impl Iterator<Item = String>) -> &'static str {
    let mut saw_sidebar = false;
    let mut saw_stats = false;
    for arg in args {
        if saw_sidebar && arg == "serve" {
            return RENDERED_PANE_LOG_FILTER;
        }
        if saw_stats && arg == "--refresh" {
            return RENDERED_PANE_LOG_FILTER;
        }
        saw_sidebar |= arg == "sidebar";
        saw_stats |= arg == "stats";
    }
    DEFAULT_LOG_FILTER
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(args: &[&str]) -> &'static str {
        log_filter_for(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn rendered_pane_commands_disable_stderr_logging() {
        assert_eq!(filter(&["sidebar", "serve"]), "off");
        assert_eq!(filter(&["stats", "--refresh"]), "off");
        assert_eq!(filter(&["stats", "--refresh", "--hold"]), "off");
    }

    #[test]
    fn ordinary_commands_keep_default_stderr_logging() {
        assert_eq!(filter(&["stats"]), "warn");
        assert_eq!(filter(&["stats", "--json"]), "warn");
        assert_eq!(filter(&["sidebar", "--refresh-ms", "500"]), "warn");
    }
}
