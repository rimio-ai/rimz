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

// Linux glibc only: jemalloc does not build for Apple targets under zig, and
// musl keeps its own allocator, which was never measured against jemalloc. A
// heap-profiling build passes `--cfg rimz_system_malloc` to keep glibc malloc.
#[cfg(all(target_os = "linux", target_env = "gnu", not(rimz_system_malloc)))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// SAFETY: jemalloc expects malloc_conf to point to a static NUL-terminated C string.
// An array reference is a thin pointer; a byte slice would have the wrong ABI.
#[cfg(all(target_os = "linux", target_env = "gnu", not(rimz_system_malloc)))]
#[expect(
    unsafe_code,
    reason = "jemalloc reads its configuration through this exported symbol"
)]
#[unsafe(export_name = "malloc_conf")]
static ALLOCATOR_CONFIG: &[u8; 17] = b"dirty_decay_ms:0\0";

const DEFAULT_LOG_FILTER: &str = "warn";
const RENDERED_PANE_LOG_FILTER: &str = "off";

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    if let Some(code) = rimz::sandbox::run_bind_helper() {
        return code;
    }
    // Completion runs before observability and build-id startup: every TAB is
    // latency-sensitive, and clap_complete owns stdout for this request.
    cli::complete_env();
    // Reporting can read the RimZ home, so restore scrubbed hooks before startup.
    #[cfg(unix)]
    cli::restore_hook_environment();
    // Background workers must not consume a signal pending across the exec handoff.
    #[cfg(unix)]
    let startup_mask = match rimz::child_process::CleanupSignalMask::block() {
        Ok(mask) => mask,
        Err(error) => {
            cli::report(&error.into());
            return std::process::ExitCode::FAILURE;
        }
    };
    // Start reading the executable identity off-thread so the build-id Sentry
    // tag is usually ready by the time `dispatch` sets the command scope.
    rimz::build_id::warm();
    // Sentry is created before the subscriber so its reporting layer attaches to a
    // live client; the guard is held for the whole process and flushes on exit.
    let reporting = observability::init();
    install_tracing(reporting.enabled());
    reporting.report();
    #[cfg(unix)]
    drop(startup_mask);
    match cli::dispatch() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            cli::report(&error);
            std::process::ExitCode::from(error_exit_code(&error))
        }
    }
}

fn error_exit_code(error: &anyhow::Error) -> u8 {
    if error.downcast_ref::<cli::UsageError>().is_some() {
        return 2;
    }
    error
        .downcast_ref::<cli::ExitCodeError>()
        .and_then(|error| u8::try_from(error.code).ok())
        .unwrap_or(1)
}

fn install_tracing(report_to_sentry: bool) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_log_filter()))
        .unwrap_or_else(|_| EnvFilter::new("warn"));
    // The env filter is per-layer on the fmt sink, so rendered panes silence
    // stderr without gating the Sentry layer's own `WARN` capture.
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(true)
        .with_writer(rimz::tui::TuiLogWriter)
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
        saw_sidebar = arg == "sidebar";
        saw_stats |= arg == "stats";
    }
    DEFAULT_LOG_FILTER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_preserve_explicit_exit_codes() {
        for code in [65, 66, 67, 127, 255] {
            let error = anyhow::Error::new(cli::ExitCodeError {
                code,
                message: "host refusal".to_owned(),
            })
            .context("connect");
            assert_eq!(error_exit_code(&error), code as u8);
        }
        assert_eq!(error_exit_code(&anyhow::anyhow!("ordinary failure")), 1);
    }

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
        assert_eq!(filter(&["sidebar", "snapshot", "serve"]), "warn");
    }
}
