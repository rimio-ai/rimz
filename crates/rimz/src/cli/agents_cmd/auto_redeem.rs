//! `rimz agents auto-redeem` — the hidden helper that consumes a Codex reset
//! credit after the elected producer finds a useful redemption.

use anyhow::{Context, Result};
use clap::Args;

use rimz::RuntimePaths;
use rimz::config::MachineConfig;
use rimz::ids::WorkspaceId;

#[derive(Debug, Args)]
pub(super) struct AutoRedeemArgs {
    #[arg(long)]
    workspace_id: String,
    #[arg(long)]
    kind: String,
    #[arg(long)]
    reason: String,
    #[arg(long)]
    request_id: uuid::Uuid,
}

pub(super) fn run_auto_redeem(args: AutoRedeemArgs) -> Result<()> {
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;
    let config = MachineConfig::load().context("loading auto-redeem config")?;

    let reset = rimz::harness::auto_redeem::execute_auto_redeem(
        &runtime,
        &args.kind,
        &args.reason,
        &args.request_id.to_string(),
        &config.resume,
    )
    .context("redeeming Codex reset credit")?;
    if reset {
        let _ = rimz::store::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}
