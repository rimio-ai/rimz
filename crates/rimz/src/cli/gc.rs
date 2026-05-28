//! `rimz gc` — remove stale runtime liveness hints.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;

use super::{GlobalFlags, open_ledger};
use rimz::ledger::gc;
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct GcArgs {
    /// Remove runtime artifacts older than this duration (`30s`, `5m`, `1h`).
    #[arg(long, default_value = "24h", value_parser = parse_duration)]
    older_than: Duration,
}

pub fn run(args: GcArgs, globals: &GlobalFlags) -> Result<()> {
    if args.older_than.is_zero() {
        bail!("--older-than must be greater than zero");
    }
    let report = gc::collect_runtime(args.older_than).context("collecting runtime garbage")?;
    let abandoned = match WorkspaceResolver::resolve(".", globals.root.clone()) {
        Ok(workspace) => {
            let ledger = open_ledger(&workspace)?;
            ledger
                .abandon_dead_owned_items(&workspace.session_name)
                .context("abandoning dead owned feed items")?
        }
        Err(_) => 0,
    };
    #[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
    {
        println!("gc complete");
        println!("  older than    : {}s", args.older_than.as_secs());
        println!("  feed abandoned: {abandoned}");
        println!("  runtime roots : {}", report.runtime_roots_scanned);
        println!("  heartbeats    : {}", report.heartbeat_files_removed);
        println!("  sidebar socks : {}", report.sidebar_sockets_removed);
        println!("  dirs removed  : {}", report.dirs_removed);
        println!("  bytes removed : {}", report.bytes_removed);
    }
    Ok(())
}

fn parse_duration(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_accepts_short_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("30").is_err());
        assert!(parse_duration("30d").is_err());
    }
}
