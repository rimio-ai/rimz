//! `rimz agents auto-redeem` — the hidden helper that consumes a Codex reset
//! credit after the elected producer finds a useful redemption.

use anyhow::{Context, Result};
use clap::Args;

use rimz::config::MachineConfig;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::ids::WorkspaceId;

use crate::cli::runtime_paths_for;

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
    let runtime = runtime_paths_for(workspace_id)?;
    let config = MachineConfig::load().context("loading auto-redeem config")?;

    let result = rimz::harness::auto_redeem::execute_auto_redeem(
        &runtime,
        &args.kind,
        &args.reason,
        &args.request_id.to_string(),
        &config.resume,
    );
    match result {
        Ok(Some(report)) => {
            append_report(&args.kind, args.request_id, &report, None);
            if report.reset {
                let _ = rimz::store::wakeup::wake_sidebars(&runtime);
            }
        }
        Ok(None) => {}
        Err(err) => {
            if let Some(report) = err.attempted_report() {
                append_report(&args.kind, args.request_id, report, Some(err.to_string()));
            }
            return Err(err).context("redeeming Codex reset credit");
        }
    }
    Ok(())
}

fn append_report(
    kind: &str,
    request_id: uuid::Uuid,
    report: &rimz::harness::auto_redeem::RedeemReport,
    error: Option<String>,
) {
    let outcome = if error.is_none() {
        report.outcome.map(|outcome| outcome.as_str().to_owned())
    } else {
        None
    };
    rimz::harness::assist_log::append(&AssistRecord {
        at: jiff::Timestamp::now(),
        assist: Assist::AutoRedeem {
            kind: kind.to_owned(),
            reason: report.reason,
            request_id: request_id.to_string(),
            credits: report.credits,
            soonest_expiry: report.soonest_expiry,
            natural_reset: report.natural_reset,
            outcome,
            windows_reset: report.windows_reset,
            window_resets: report.window_resets.clone(),
            error,
        },
    });
}
