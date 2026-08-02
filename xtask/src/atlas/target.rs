use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::files;

pub(super) const TARGET_FILE: &str = "refactor-target.toml";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Target {
    pub(super) version: u8,
    #[serde(default, rename = "module")]
    pub(super) modules: Vec<ModuleRule>,
    #[serde(default)]
    pub(super) strangler: Vec<StranglerRule>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) struct ModuleRule {
    pub(super) path: PathBuf,
    #[serde(default)]
    pub(super) allowed_imports: Vec<String>,
    pub(super) surface_budget: usize,
    #[serde(skip)]
    pub(super) config_line: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct StranglerRule {
    pub(super) symbol: String,
    pub(super) path: PathBuf,
    pub(super) baseline: usize,
    #[serde(skip)]
    pub(super) config_line: usize,
}

pub(super) fn load(path: &Path) -> Result<Option<Target>> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let document: toml::Value =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    let version = document
        .get("version")
        .and_then(toml::Value::as_integer)
        .unwrap_or_default();
    if version != 2 {
        bail!(
            "{} has unsupported version {}; expected 2 (reseed version 1 targets with `cargo xtask atlas conform --init`)",
            path.display(),
            version
        );
    }
    let mut target: Target =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    let module_lines = section_lines(&raw, "[[module]]");
    let strangler_lines = section_lines(&raw, "[[strangler]]");
    for (index, module) in target.modules.iter_mut().enumerate() {
        module.config_line = module_lines.get(index).copied().unwrap_or(1);
    }
    for (index, strangler) in target.strangler.iter_mut().enumerate() {
        strangler.config_line = strangler_lines.get(index).copied().unwrap_or(1);
    }
    Ok(Some(target))
}

pub(super) fn write(path: &Path, target: &Target) -> Result<()> {
    let mut rendered = toml::to_string_pretty(target).context("rendering refactor target TOML")?;
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    files::write_atomically(path, rendered.as_bytes())
}

fn section_lines(raw: &str, section: &str) -> Vec<usize> {
    raw.lines()
        .enumerate()
        .filter_map(|(index, line)| (line.trim() == section).then_some(index + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_accepts_kebab_case_module_fields() {
        let target: Target = toml::from_str(
            r#"
version = 2
[[module]]
path = "crates/rimz/src/cli"
allowed-imports = ["agents"]
surface-budget = 10
[[strangler]]
symbol = "old"
path = "crates/rimz/src/cli/mod.rs"
baseline = 2
"#,
        )
        .unwrap();
        assert_eq!(target.modules[0].allowed_imports, ["agents"]);
        assert_eq!(target.modules[0].surface_budget, 10);
        assert_eq!(target.strangler[0].baseline, 2);
    }

    #[test]
    fn version_one_targets_are_rejected_before_schema_deserialization() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(
            &path,
            "version = 1\n[[module]]\npath = \"src\"\npub-budget = 1\n",
        )
        .unwrap();
        let error = load(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported version 1; expected 2")
        );
    }
}
