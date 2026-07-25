//! Thin contributor-task wrapper around RimZ's pricing source projection.

use std::path::Path;

use anyhow::{Result, bail};

use crate::runner::run;

const GENERATED_SNAPSHOT: &str = "crates/rimz/pricing/litellm-pricing.json";

pub(crate) fn pricing_refresh(root: &Path, args: &[String]) -> Result<()> {
    if args.iter().any(|arg| arg != "--check") || args.len() > 1 {
        bail!("pricing-refresh accepts only --check");
    }
    let mut command = vec![
        "run".to_owned(),
        "-p".to_owned(),
        "rimz".to_owned(),
        "--bin".to_owned(),
        "rimz".to_owned(),
        "--locked".to_owned(),
        "--".to_owned(),
        "pricing-refresh".to_owned(),
    ];
    // `--check` validates coverage in place; a destination is what turns the
    // same projection into a snapshot write.
    if args.is_empty() {
        command.push("--out".to_owned());
        command.push(root.join(GENERATED_SNAPSHOT).display().to_string());
    } else {
        command.extend(args.iter().cloned());
    }
    run(root, "cargo", command)
}
