//! Shared output flags for the atlas report verbs.
//!
//! Every report verb renders one `Report` value either as Markdown or JSON,
//! narrowed to the sections the caller asked for, and delivers it to stdout
//! or to `--out`. Parsing lives here so the three verbs accept the same flags.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::{set_once, value};

pub(super) const USAGE: &str = "  --json             emit the report as JSON instead of Markdown
  --out <file>       write the report to <file> instead of stdout
  --section <a,b>    only the named sections (comma-separated)";

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct OutputArgs {
    pub(super) json: bool,
    pub(super) out: Option<PathBuf>,
    sections: Option<BTreeSet<String>>,
}

impl OutputArgs {
    /// Consumes one output flag at `args[index]`; returns the number of
    /// arguments eaten, or `None` when the flag is not an output flag.
    pub(super) fn parse_flag(
        &mut self,
        args: &[String],
        index: usize,
        verb: &str,
    ) -> Result<Option<usize>> {
        match args[index].as_str() {
            "--json" => {
                if self.json {
                    bail!("atlas {verb} --json may only be passed once");
                }
                self.json = true;
                Ok(Some(1))
            }
            "--out" => {
                let raw = value(args, index, verb, "--out")?;
                if raw.is_empty() {
                    bail!("atlas {verb} --out requires a non-empty path");
                }
                set_once(&mut self.out, PathBuf::from(raw), verb, "--out")?;
                Ok(Some(2))
            }
            "--section" => {
                let raw = value(args, index, verb, "--section")?;
                let names = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>();
                if names.is_empty() {
                    bail!("atlas {verb} --section requires at least one section name");
                }
                set_once(&mut self.sections, names, verb, "--section")?;
                Ok(Some(2))
            }
            _ => Ok(None),
        }
    }

    /// Rejects section names the verb does not render.
    pub(super) fn validate_sections(&self, verb: &str, known: &[&str]) -> Result<()> {
        let Some(sections) = &self.sections else {
            return Ok(());
        };
        let unknown = sections
            .iter()
            .filter(|name| !known.contains(&name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            bail!(
                "atlas {verb} --section: unknown section(s) {}; known: {}",
                unknown.join(", "),
                known.join(", ")
            );
        }
        Ok(())
    }

    pub(super) fn wants(&self, section: &str) -> bool {
        self.sections
            .as_ref()
            .is_none_or(|sections| sections.contains(section))
    }

    /// Delivers the rendered report to `--out` or stdout.
    #[expect(
        clippy::print_stdout,
        reason = "atlas report output is the command's stdout contract"
    )]
    pub(super) fn emit(&self, rendered: &str) -> Result<()> {
        match &self.out {
            Some(path) => write_out(path, rendered),
            None => {
                print!("{rendered}");
                Ok(())
            }
        }
    }
}

fn write_out(path: &Path, rendered: &str) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating atlas output directory {}", parent.display()))?;
    }
    fs::write(path, rendered).with_context(|| format!("writing atlas output {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn parses_json_out_and_sections() {
        let raw = args(&[
            "--json",
            "--out",
            "/tmp/x.json",
            "--section",
            "rank, guards",
        ]);
        let mut output = OutputArgs::default();
        let mut index = 0;
        while index < raw.len() {
            let eaten = output.parse_flag(&raw, index, "survey").unwrap().unwrap();
            index += eaten;
        }
        assert!(output.json);
        assert_eq!(output.out.as_deref(), Some(Path::new("/tmp/x.json")));
        assert!(output.wants("rank"));
        assert!(output.wants("guards"));
        assert!(!output.wants("shapes"));
        output
            .validate_sections("survey", &["rank", "guards", "shapes"])
            .unwrap();
        assert!(output.validate_sections("survey", &["rank"]).is_err());
    }

    #[test]
    fn non_output_flags_are_left_alone() {
        let raw = args(&["--top", "3"]);
        let mut output = OutputArgs::default();
        assert_eq!(output.parse_flag(&raw, 0, "survey").unwrap(), None);
        assert!(output.wants("anything"));
    }

    #[test]
    fn repeated_and_empty_flags_fail() {
        let mut output = OutputArgs::default();
        let raw = args(&["--json", "--json"]);
        output.parse_flag(&raw, 0, "diff").unwrap();
        assert!(output.parse_flag(&raw, 1, "diff").is_err());
        let raw = args(&["--section", ","]);
        assert!(OutputArgs::default().parse_flag(&raw, 0, "diff").is_err());
    }
}
