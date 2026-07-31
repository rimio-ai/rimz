//! Portable measurements for architecture refactor programs.
//!
//! `atlas` deliberately contains no RimZ-module knowledge: the scope and
//! convergence rules arrive as command arguments or target data.
//!
//! ponytail: this is an xtask module while RimZ is its only consumer; extract
//! the source/syntax/history seams into a crate if another repository adopts it.

mod api;
mod conform;
mod history;
mod metrics;
mod modules;
mod rank;
mod seams;
mod shapes;
mod sources;
mod syntax;
mod target;

use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

pub(crate) const USAGE: &str = "cargo xtask atlas <verb> [flags]

Verbs:
  rank      prioritize modules by size, interface, churn, pace, and complexity
  seams     report imports, external surface, co-change, and divergence
  api       report boundary shape and outside identifier occurrences
  shapes    cluster large functions by shared call choreography
  conform   compare the tree with refactor-target.toml budgets

Run `cargo xtask atlas <verb> --help` for verb-specific flags.";

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas help is the command's stdout contract"
)]
pub(crate) fn atlas(root: &Path, args: &[String]) -> Result<()> {
    let Some((verb, rest)) = args.split_first() else {
        println!("{USAGE}");
        return Ok(());
    };
    if crate::is_help_flag(verb) {
        println!("{USAGE}");
        return Ok(());
    }
    match verb.as_str() {
        "rank" => rank::run(root, rest),
        "seams" => seams::run(root, rest),
        "api" => api::run(root, rest),
        "shapes" => shapes::run(root, rest),
        "conform" => conform::run(root, rest),
        _ => bail!("unknown atlas verb `{verb}`\n\n{USAGE}"),
    }
}

pub(crate) fn conform_ratchet(root: &Path) -> Result<()> {
    conform::ratchet(root)
}

fn validate_scope(value: &str, flag: &str) -> Result<PathBuf> {
    if value.is_empty() {
        bail!("atlas {flag} requires a non-empty root-relative path");
    }
    let path = PathBuf::from(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        bail!("atlas {flag} must be root-relative and may not contain `..`");
    }
    Ok(path)
}

fn value<'a>(args: &'a [String], index: usize, verb: &str, flag: &str) -> Result<&'a str> {
    args.get(index + 1)
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("atlas {verb} {flag} requires a value"))
}

fn set_once<T>(slot: &mut Option<T>, value: T, verb: &str, flag: &str) -> Result<()> {
    if slot.is_some() {
        bail!("atlas {verb} {flag} may only be passed once");
    }
    *slot = Some(value);
    Ok(())
}

fn positive_usize(value: &str, verb: &str, flag: &str) -> Result<usize> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("atlas {verb} {flag} requires a positive integer"))?;
    if parsed == 0 {
        bail!("atlas {verb} {flag} must be greater than zero");
    }
    Ok(parsed)
}

fn finite_nonnegative(value: &str, verb: &str, flag: &str) -> Result<f64> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| anyhow::anyhow!("atlas {verb} {flag} requires a non-negative number"))?;
    if !parsed.is_finite() || parsed < 0.0 {
        bail!("atlas {verb} {flag} requires a finite non-negative number");
    }
    Ok(parsed)
}
