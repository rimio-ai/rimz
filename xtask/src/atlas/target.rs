use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::files;

pub(super) const TARGET_FILE: &str = "refactor-target.toml";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Target {
    pub(super) version: u8,
    pub(super) layers: Vec<String>,
    #[serde(default, rename = "module")]
    pub(super) modules: Vec<ModuleRule>,
    #[serde(default)]
    pub(super) strangler: Vec<StranglerRule>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) struct ModuleRule {
    pub(super) path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) allowed_imports: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) upward_imports: Option<Vec<String>>,
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
    if version != 3 {
        bail!(
            "{} has unsupported version {}; expected 3 (reseed with `cargo xtask atlas conform --init` or convert v2 by hand: layers + upward-imports)",
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
    validate(path, &target)?;
    Ok(Some(target))
}

fn validate(path: &Path, target: &Target) -> Result<()> {
    let mut seen_layers = BTreeSet::new();
    for layer in &target.layers {
        if !seen_layers.insert(layer) {
            bail!("{} has duplicate layer `{layer}`", path.display());
        }
    }
    for module in &target.modules {
        if module.allowed_imports.is_some() && module.upward_imports.is_some() {
            bail!(
                "{}:{}: module `{}` cannot set both allowed-imports and upward-imports",
                path.display(),
                module.config_line,
                module.path.display()
            );
        }
    }
    Ok(())
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
version = 3
layers = ["cli", "agents"]
[[module]]
path = "crates/rimz/src/cli"
allowed-imports = ["agents"]
surface-budget = 10
[[module]]
path = "crates/rimz/src/agents"
upward-imports = ["cli"]
surface-budget = 5
[[strangler]]
symbol = "old"
path = "crates/rimz/src/cli/mod.rs"
baseline = 2
"#,
        )
        .unwrap();
        assert_eq!(target.layers, ["cli", "agents"]);
        assert_eq!(
            target.modules[0].allowed_imports.as_deref().unwrap(),
            ["agents"]
        );
        assert!(target.modules[0].upward_imports.is_none());
        assert_eq!(target.modules[0].surface_budget, 10);
        assert!(target.modules[1].allowed_imports.is_none());
        assert_eq!(
            target.modules[1].upward_imports.as_deref().unwrap(),
            ["cli"]
        );
        assert_eq!(target.strangler[0].baseline, 2);
    }

    #[test]
    fn older_targets_are_rejected_before_schema_deserialization() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(
            &path,
            "version = 2\n[[module]]\npath = \"src\"\npub-budget = 1\n",
        )
        .unwrap();
        let error = load(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported version 2; expected 3")
        );
    }

    #[test]
    fn duplicate_layers_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(&path, "version = 3\nlayers = [\"cli\", \"cli\"]\n").unwrap();

        let error = load(&path).unwrap_err();
        assert!(error.to_string().contains("duplicate layer `cli`"));
    }

    #[test]
    fn module_import_modes_are_mutually_exclusive_and_report_the_section_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(
            &path,
            r#"version = 3
layers = ["cli", "agents"]

[[module]]
path = "crates/rimz/src/cli"
allowed-imports = ["agents"]
upward-imports = ["agents"]
surface-budget = 10
"#,
        )
        .unwrap();

        let error = load(&path).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(":4: module `crates/rimz/src/cli`"));
        assert!(message.contains("cannot set both allowed-imports and upward-imports"));
    }

    #[test]
    fn load_tracks_module_and_strangler_section_lines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(
            &path,
            r#"version = 3
layers = []

[[module]]
path = "src/first"
surface-budget = 1

[[module]]
path = "src/second"
allowed-imports = []
surface-budget = 2

[[strangler]]
symbol = "old"
path = "src"
baseline = 3
"#,
        )
        .unwrap();

        let target = load(&path).unwrap().unwrap();
        assert_eq!(target.modules[0].config_line, 4);
        assert_eq!(target.modules[1].config_line, 8);
        assert_eq!(target.strangler[0].config_line, 13);
    }

    #[test]
    fn serialization_preserves_layers_and_present_import_mode_only() {
        let target = Target {
            version: 3,
            layers: vec!["cli".to_owned(), "agents".to_owned()],
            modules: vec![
                ModuleRule {
                    path: PathBuf::from("src/cli"),
                    allowed_imports: None,
                    upward_imports: Some(vec!["agents".to_owned()]),
                    surface_budget: 4,
                    config_line: 99,
                },
                ModuleRule {
                    path: PathBuf::from("src/agents"),
                    allowed_imports: Some(Vec::new()),
                    upward_imports: None,
                    surface_budget: 2,
                    config_line: 100,
                },
            ],
            strangler: Vec::new(),
        };

        let rendered = toml::to_string_pretty(&target).unwrap();
        assert!(rendered.contains("layers = ["));
        assert_eq!(rendered.matches("upward-imports").count(), 1);
        assert_eq!(rendered.matches("allowed-imports").count(), 1);
        assert!(!rendered.contains("config-line"));
        let reparsed: Target = toml::from_str(&rendered).unwrap();
        assert_eq!(reparsed.layers, target.layers);
        assert_eq!(
            reparsed.modules[0].upward_imports,
            target.modules[0].upward_imports
        );
        assert_eq!(
            reparsed.modules[1].allowed_imports,
            target.modules[1].allowed_imports
        );
    }
}
