//! `rimz agents auto-redeem` — the hidden helper that consumes a Codex reset
//! credit after the elected producer finds a useful redemption.

use anyhow::{Context, Result};

use rimz::config::MachineConfig;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::auto_redeem::AutoRedeemRequest;

use crate::cli::runtime_paths_for;

pub(super) fn run_auto_redeem(request: AutoRedeemRequest) -> Result<()> {
    let runtime = runtime_paths_for(request.workspace_id)?;
    let config = MachineConfig::load().context("loading auto-redeem config")?;

    let result = rimz::harness::auto_redeem::execute_auto_redeem(
        &runtime,
        &request.kind,
        request.reason,
        request.request_id,
        &config.resume,
    );
    match result {
        Ok(Some(report)) => {
            append_report(request.kind.as_str(), request.request_id, &report, None);
            if report.reset {
                let _ = rimz::sidebar::wakeup::wake_store_delta(&runtime, None, None);
            }
        }
        Ok(None) => {}
        Err(err) => {
            if let Some(report) = err.attempted_report() {
                append_report(
                    request.kind.as_str(),
                    request.request_id,
                    report,
                    Some(err.to_string()),
                );
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
