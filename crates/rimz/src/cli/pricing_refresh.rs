//! Hidden contributor helper for projecting the upstream pricing sources.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::cli::render;

#[derive(Debug, Args)]
pub struct PricingRefreshArgs {
    /// Destination for the compacted LiteLLM-shaped snapshot.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
    /// Validate upstream coverage without writing the snapshot.
    #[arg(long)]
    check: bool,
}

pub fn run(args: PricingRefreshArgs) -> Result<()> {
    let out_path = args.out.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("pricing/litellm-pricing.json")
    });
    let report = rimz::agents::pricing::source::refresh(&out_path, args.check)?;
    let providers = report
        .provider_model_counts
        .iter()
        .map(|(provider, count)| format!("{provider}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = render::out();
    if args.check {
        writeln!(
            out,
            "pricing coverage ok: {} models (LiteLLM {}, {providers})",
            report.model_count, report.litellm_model_count
        )?;
    } else {
        writeln!(
            out,
            "wrote {} models to {}",
            report.model_count,
            out_path.display()
        )?;
    }
    Ok(())
}
