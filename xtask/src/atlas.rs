//! Portable measurements for architecture refactor programs.
//!
//! `atlas` deliberately contains no RimZ-module knowledge: the scope and
//! convergence rules arrive as command arguments or target data.
//!
//! ponytail: this is an xtask module while RimZ is its only consumer; extract
//! the source/syntax/history seams into a crate if another repository adopts it.

mod conform;
mod detect;
mod diff;
mod facts;
mod history;
mod index;
mod inspect;
mod metrics;
mod modules;
mod rank;
mod references;
mod shapes;
mod sources;
mod survey;
mod syntax;
mod target;

pub(super) const REPORT_VERSION: u8 = 4;

use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

pub(crate) const USAGE: &str = "cargo xtask atlas <verb> [flags]

Verbs:
  survey    produce one architecture-review sweep over a scope
  diff      compare a base revision with the working tree: surface, imports, edges, files
  inspect   show what one module's functions assemble from another, and where
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
        "diff" => diff::run(root, rest),
        "inspect" => inspect::run(root, rest),
        "survey" => survey::run(root, rest),
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

#[expect(
    clippy::print_stderr,
    reason = "atlas explicitly reports degraded no-index analysis"
)]
fn note_no_index() {
    eprintln!("atlas: --no-index omits exact-reference fields");
}
