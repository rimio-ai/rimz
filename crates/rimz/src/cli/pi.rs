//! Pi account-usage refresh helper.
//!
//! Spawned by the sidebar producer for best-effort OAuth usage enrichment. It
//! reads Pi's selected backing-provider OAuth token, reuses the sibling provider
//! usage fetcher, and writes account-scoped Pi windows into the shared cache.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use rimz::ids::WorkspaceId;
use rimz::sidebar::cache::unix_now_ms;
use rimz::{RuntimePaths, agents, config::MachineConfig};

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct PiArgs {
    #[command(subcommand)]
    command: PiSubcmd,
}

#[derive(Debug, Subcommand)]
enum PiSubcmd {
    /// Refresh Pi account usage into the shared runtime cache. The sidebar
    /// producer spawns this detached; humans usually do not run it.
    #[command(hide = true)]
    RefreshUsage {
        /// Workspace whose runtime cache the account usage is written into.
        #[arg(long)]
        workspace_id: String,
    },
}

impl PiArgs {
    pub(crate) fn command_label(&self) -> &'static str {
        match &self.command {
            PiSubcmd::RefreshUsage { .. } => "pi refresh-usage",
        }
    }
}

pub fn run(args: PiArgs, _globals: &GlobalFlags) -> Result<()> {
    match args.command {
        PiSubcmd::RefreshUsage { workspace_id } => refresh_usage(&workspace_id),
    }
}

fn refresh_usage(workspace_id: &str) -> Result<()> {
    let workspace_id: WorkspaceId = workspace_id.parse().context("parsing workspace id")?;
    let runtime =
        RuntimePaths::for_workspace(workspace_id.clone()).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;

    let config = MachineConfig::load().unwrap_or_default();
    if !config.accounts.oauth_usage || agents::credits::oauth_usage_offline() {
        return Ok(());
    }

    let mut fetched_windows = None;
    let wrote = rimz::sidebar::enrich::merge_provider_credits_entry_if_due(&runtime, "pi", || {
        match agents::pi::fetch_oauth_usage() {
            Some(usage) => {
                fetched_windows = usage.rate_limits.clone();
                rimz::sidebar::enrich::ProviderCreditsEntry {
                    observed_at_ms: unix_now_ms(),
                    ok: true,
                    extra_credits: usage.extra_credits,
                }
            }
            None => rimz::sidebar::enrich::ProviderCreditsEntry {
                observed_at_ms: unix_now_ms(),
                ok: false,
                extra_credits: None,
            },
        }
    })
    .is_some();

    if let Some(rate_limits) = fetched_windows {
        rimz::sidebar::enrich::merge_account_rate_limits(&runtime, "pi", rate_limits);
    }
    if wrote {
        let _ = rimz::ledger::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}
