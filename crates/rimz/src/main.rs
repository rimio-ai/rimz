//! Rimz CLI entry point.
//!
//! `main.rs` is the only place in the workspace allowed to use `anyhow` and
//! to install the `tracing_subscriber`. Library modules return typed errors
//! and emit `tracing` events; this wrapper turns them into a process exit.

#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

mod cli;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

const DEFAULT_LOG_FILTER: &str = "warn";
const SIDEBAR_SERVE_LOG_FILTER: &str = "off";

fn main() -> Result<()> {
    install_tracing();
    cli::dispatch()
}

fn install_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_log_filter()))
        .unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
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
