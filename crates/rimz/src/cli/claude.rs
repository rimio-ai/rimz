//! Claude account-usage refresh helper.
//!
//! Spawned by the sidebar producer for best-effort OAuth usage enrichment. It is
//! deliberately quiet and exits successfully on every provider-side failure; the
//! shared credits cache records success/failure so producers back off.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use rimz::agents;
use rimz::ids::WorkspaceId;
use rimz::sidebar::cache::unix_now_ms;
use rimz::{RuntimePaths, config::MachineConfig};

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct ClaudeArgs {
    #[command(subcommand)]
    command: ClaudeSubcmd,
}

#[derive(Debug, Subcommand)]
enum ClaudeSubcmd {
    /// Refresh Claude account usage into the shared runtime cache. The sidebar
    /// producer spawns this detached; humans usually do not run it.
    #[command(hide = true)]
    RefreshUsage {
        /// Workspace whose runtime cache the account usage is written into.
        #[arg(long)]
        workspace_id: String,
        /// Also merge included-budget windows into the shared rate-limit cache.
        #[arg(long)]
        merge_windows: bool,
        /// Claude Code version to advertise to the usage endpoint.
        #[arg(long)]
        agent_version: Option<String>,
    },
}

pub fn run(args: ClaudeArgs, _globals: &GlobalFlags) -> Result<()> {
    match args.command {
        ClaudeSubcmd::RefreshUsage {
            workspace_id,
            merge_windows,
            agent_version,
        } => refresh_usage(&workspace_id, merge_windows, agent_version.as_deref()),
    }
}

fn refresh_usage(
    workspace_id: &str,
    merge_windows: bool,
    agent_version: Option<&str>,
) -> Result<()> {
    let workspace_id: WorkspaceId = workspace_id.parse().context("parsing workspace id")?;
    let runtime =
        RuntimePaths::for_workspace(workspace_id.clone()).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;

    let config = MachineConfig::load().unwrap_or_default();
    if !config.accounts.oauth_usage || agents::credits::oauth_usage_offline() {
        return Ok(());
    }

    let mut fetched_windows = None;
    let wrote =
        rimz::sidebar::enrich::merge_provider_credits_entry_if_due(&runtime, "claude", || {
            match agents::claude::fetch_oauth_usage(agent_version) {
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

    if merge_windows {
        if let Some(rate_limits) = fetched_windows {
            rimz::sidebar::enrich::merge_account_rate_limits(&runtime, "claude", rate_limits);
        }
    }
    if wrote {
        let _ = rimz::ledger::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}
