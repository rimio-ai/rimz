//! Thin contributor-task wrapper around RimZ's pricing source projection.

use std::path::Path;

use anyhow::{Result, bail};

use crate::runner::run;

const GENERATED_SNAPSHOT: &str = "crates/rimz/pricing/litellm-pricing.json";

pub(crate) fn pricing_refresh(root: &Path, args: &[String]) -> Result<()> {
    if args.iter().any(|arg| arg != "--check") || args.len() > 1 {
        bail!("pricing-refresh accepts only --check");
    }
    let out = root.join(GENERATED_SNAPSHOT);
    let mut command = vec![
        "run".to_owned(),
        "-p".to_owned(),
        "rimz".to_owned(),
        "--bin".to_owned(),
        "rimz".to_owned(),
        "--locked".to_owned(),
        "--".to_owned(),
        "pricing-refresh".to_owned(),
        "--out".to_owned(),
        out.display().to_string(),
    ];
    command.extend(args.iter().cloned());
    run(root, "cargo", command)
}
