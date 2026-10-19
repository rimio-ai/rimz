//! Shared output flags for the atlas report verbs.
//!
//! Every report verb renders one `Report` value either as Markdown or JSON,
//! narrowed to the sections the caller asked for, and delivers it to stdout
//! or to `--out`. Parsing lives here so the report verbs accept the same flags.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::ArgCursor;

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
    /// Consumes the cursor's current output flag; returns `false` for other flags.
    pub(super) fn parse_flag(&mut self, cursor: &mut ArgCursor<'_>) -> Result<bool> {
        let verb = cursor.verb;
        match cursor.flag {
            "--json" => cursor.switch(&mut self.json, "--json")?,
            "--out" => {
                let raw = cursor.value_of("--out")?;
                if raw.is_empty() {
                    bail!("atlas {verb} --out requires a non-empty path");
                }
                cursor.once(&mut self.out, PathBuf::from(raw), "--out")?;
            }
            "--section" => {
                let raw = cursor.value_of("--section")?;
                let names = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>();
                if names.is_empty() {
                    bail!("atlas {verb} --section requires at least one section name");
                }
                cursor.once(&mut self.sections, names, "--section")?;
            }
            _ => return Ok(false),
        }
        Ok(true)
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

    /// Installs a preset section list; returns `false` when `--section`
    /// already chose one.
    pub(super) fn default_sections(&mut self, names: &[&str]) -> bool {
        if self.sections.is_some() {
            return false;
        }
        self.sections = Some(names.iter().map(|name| (*name).to_owned()).collect());
        true
    }

    pub(super) fn wants(&self, section: &str) -> bool {
        self.sections
            .as_ref()
            .is_none_or(|sections| sections.contains(section))
    }

    pub(super) fn named(&self, section: &str) -> bool {
        self.sections
            .as_ref()
            .is_some_and(|sections| sections.contains(section))
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
        let mut cursor = ArgCursor::new(&raw, "survey").unwrap();
        while cursor.next().is_some() {
            assert!(output.parse_flag(&mut cursor).unwrap());
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
        let mut cursor = ArgCursor::new(&raw, "survey").unwrap();
        cursor.next();
        assert!(!output.parse_flag(&mut cursor).unwrap());
        assert!(output.wants("anything"));
    }

    #[test]
    fn repeated_and_empty_flags_fail() {
        let mut output = OutputArgs::default();
        let raw = args(&["--json", "--json"]);
        let mut cursor = ArgCursor::new(&raw, "diff").unwrap();
        cursor.next();
        output.parse_flag(&mut cursor).unwrap();
        cursor.next();
        assert!(output.parse_flag(&mut cursor).is_err());
        let raw = args(&["--section", ","]);
        let mut cursor = ArgCursor::new(&raw, "diff").unwrap();
        cursor.next();
        assert!(OutputArgs::default().parse_flag(&mut cursor).is_err());
    }
}
