//! Hidden contributor helper for projecting the upstream pricing sources.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::cli::render;

#[derive(Debug, Args)]
pub struct PricingRefreshArgs {
    /// Destination for the compacted LiteLLM-shaped snapshot.
    #[arg(long, value_name = "PATH", required_unless_present = "check")]
    out: Option<PathBuf>,
    /// Validate upstream coverage without writing the snapshot.
    #[arg(long, conflicts_with = "out")]
    check: bool,
}

pub fn run(args: PricingRefreshArgs) -> Result<()> {
    let report = rimz::agents::pricing::source::refresh(args.out.as_deref())?;
    let providers = report
        .provider_model_counts
        .iter()
        .map(|(provider, count)| format!("{provider}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = render::out();
    match args.out {
        Some(path) => writeln!(
            out,
            "wrote {} models to {}",
            report.model_count,
            path.display()
        )?,
        None => writeln!(
            out,
            "pricing coverage ok: {} models (LiteLLM {}, {providers})",
            report.model_count, report.litellm_model_count
        )?,
    }
    Ok(())
}
