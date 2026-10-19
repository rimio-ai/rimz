//! Portable measurements for architecture refactor programs.
//!
//! `atlas` deliberately contains no RimZ-module knowledge: the scope and
//! convergence rules arrive as command arguments or target data.
//!
//! ponytail: this is an xtask module while RimZ is its only consumer; extract
//! the source/syntax/history seams into a crate if another repository adopts it.

mod conform;
mod contract;
mod detect;
mod diff;
mod facts;
mod history;
mod index;
mod inspect;
mod ledger;
mod metrics;
mod modules;
mod occurrences;
mod output;
mod rank;
mod references;
mod shapes;
mod sources;
mod survey;
mod syntax;
mod target;

use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

pub(crate) const USAGE: &str = "cargo xtask atlas <verb> [flags]

Verbs:
  survey    produce one architecture-review sweep over a scope
  inspect   build one indexed module dossier and quote its heaviest caller
  index     list the cached SCIP index's raw occurrences for a file or an item name
  diff      compare a base or enforce an executable pass contract
  conform   compare the tree with refactor-target.toml constraints

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
        "index" => occurrences::run(root, rest),
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

struct ArgCursor<'a> {
    args: std::slice::Iter<'a, String>,
    flag: &'a str,
    verb: &'a str,
}

impl<'a> ArgCursor<'a> {
    fn new(args: &'a [String], verb: &'a str) -> Option<Self> {
        if args.iter().any(|arg| crate::is_help_flag(arg)) {
            return None;
        }
        Some(Self {
            args: args.iter(),
            flag: "",
            verb,
        })
    }

    fn next(&mut self) -> Option<&'a str> {
        self.flag = self.args.next()?.as_str();
        Some(self.flag)
    }

    fn value_of(&mut self, flag: &str) -> Result<&'a str> {
        self.args
            .next()
            .map(String::as_str)
            .ok_or_else(|| anyhow::anyhow!("atlas {} {flag} requires a value", self.verb))
    }

    fn once<T>(&self, slot: &mut Option<T>, value: T, flag: &str) -> Result<()> {
        if slot.is_some() {
            bail!("atlas {} {flag} may only be passed once", self.verb);
        }
        *slot = Some(value);
        Ok(())
    }

    fn switch(&self, slot: &mut bool, flag: &str) -> Result<()> {
        if *slot {
            bail!("atlas {} {flag} may only be passed once", self.verb);
        }
        *slot = true;
        Ok(())
    }

    fn positive(&mut self, slot: &mut Option<usize>, flag: &str) -> Result<()> {
        let parsed = self.value_of(flag)?.parse::<usize>().map_err(|_| {
            anyhow::anyhow!("atlas {} {flag} requires a positive integer", self.verb)
        })?;
        if parsed == 0 {
            bail!("atlas {} {flag} must be greater than zero", self.verb);
        }
        self.once(slot, parsed, flag)
    }
}
