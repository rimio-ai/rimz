use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::files;

pub(super) const TARGET_FILE: &str = "refactor-target.toml";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Target {
    pub(super) version: u8,
    pub(super) layers: Vec<Layer>,
    #[serde(default, rename = "module")]
    pub(super) modules: Vec<ModuleRule>,
    #[serde(default)]
    pub(super) strangler: Vec<StranglerRule>,
}

impl Target {
    pub(super) fn layer_ranks(&self) -> LayerRanks {
        LayerRanks::new(&self.layers)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub(super) enum Layer {
    Module(String),
    Group(Vec<String>),
}

impl Layer {
    pub(super) fn modules(&self) -> &[String] {
        match self {
            Self::Module(module) => std::slice::from_ref(module),
            Self::Group(modules) => modules,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct LayerRanks(BTreeMap<String, usize>);

impl LayerRanks {
    pub(super) fn new(layers: &[Layer]) -> Self {
        Self(
            layers
                .iter()
                .enumerate()
                .flat_map(|(rank, layer)| {
                    layer
                        .modules()
                        .iter()
                        .cloned()
                        .map(move |module| (module, rank))
                })
                .collect(),
        )
    }

    pub(super) fn get(&self, module: &str) -> Option<usize> {
        self.0.get(module).copied()
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) surface_goal: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) upward_debt: Option<Vec<String>>,
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
    if version != 4 {
        bail!(
            "{} has unsupported version {}; expected 4 (a v3 file is a strict subset: change `version = 3` to `version = 4`; otherwise reseed with `cargo xtask atlas conform --init`)",
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
    validate(path, &target, key_line(&raw, "layers"))?;
    Ok(Some(target))
}

fn validate(path: &Path, target: &Target, layers_line: usize) -> Result<()> {
    let mut seen_layers = BTreeSet::new();
    for layer in &target.layers {
        if layer.modules().is_empty() {
            bail!(
                "{}:{layers_line}: layer groups may not be empty",
                path.display()
            );
        }
        for module in layer.modules() {
            if !seen_layers.insert(module) {
                bail!(
                    "{}:{layers_line}: duplicate layer `{module}`",
                    path.display()
                );
            }
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
        if module
            .surface_goal
            .is_some_and(|goal| goal > module.surface_budget)
        {
            bail!(
                "{}:{}: module `{}` surface-goal exceeds surface-budget",
                path.display(),
                module.config_line,
                module.path.display()
            );
        }
        if module.allowed_imports.is_some() && module.upward_debt.is_some() {
            bail!(
                "{}:{}: module `{}` cannot set upward-debt with allowed-imports",
                path.display(),
                module.config_line,
                module.path.display()
            );
        }
        if let Some(debt) = &module.upward_debt {
            let admissions = module.upward_imports.as_deref().unwrap_or_default();
            if let Some(unadmitted) = debt.iter().find(|entry| !admissions.contains(entry)) {
                bail!(
                    "{}:{}: module `{}` upward-debt `{unadmitted}` is not present in upward-imports",
                    path.display(),
                    module.config_line,
                    module.path.display()
                );
            }
        }
    }
    Ok(())
}

pub(super) fn write(path: &Path, target: &Target) -> Result<()> {
    validate(path, target, 1)?;
    let mut target = target.clone();
    for layer in &mut target.layers {
        if let Layer::Group(modules) = layer
            && let [module] = modules.as_slice()
        {
            *layer = Layer::Module(module.clone());
        }
    }
    let mut rendered = toml::to_string_pretty(&target).context("rendering refactor target TOML")?;
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

fn key_line(raw: &str, key: &str) -> usize {
    raw.lines()
        .position(|line| {
            line.trim_start()
                .strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .map_or(1, |index| index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_accepts_kebab_case_module_fields() {
        let target: Target = toml::from_str(
            r#"
version = 4
layers = ["cli", "agents"]
[[module]]
path = "crates/rimz/src/cli"
allowed-imports = ["agents"]
surface-budget = 10
[[module]]
path = "crates/rimz/src/agents"
upward-imports = ["cli"]
surface-budget = 5
surface-goal = 3
upward-debt = ["cli"]
[[strangler]]
symbol = "old"
path = "crates/rimz/src/cli/mod.rs"
baseline = 2
"#,
        )
        .unwrap();
        assert_eq!(
            target.layers,
            [
                Layer::Module("cli".to_owned()),
                Layer::Module("agents".to_owned())
            ]
        );
        assert_eq!(
            target.modules[0].allowed_imports.as_deref().unwrap(),
            ["agents"]
        );
        assert!(target.modules[0].upward_imports.is_none());
        assert_eq!(target.modules[0].surface_budget, 10);
        assert!(target.modules[0].surface_goal.is_none());
        assert!(target.modules[0].upward_debt.is_none());
        assert!(target.modules[1].allowed_imports.is_none());
        assert_eq!(
            target.modules[1].upward_imports.as_deref().unwrap(),
            ["cli"]
        );
        assert_eq!(target.modules[1].surface_goal, Some(3));
        assert_eq!(target.modules[1].upward_debt.as_deref().unwrap(), ["cli"]);
        assert_eq!(target.strangler[0].baseline, 2);
    }

    #[test]
    fn older_targets_are_rejected_before_schema_deserialization() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(
            &path,
            "version = 3\n[[module]]\npath = \"src\"\npub-budget = 1\n",
        )
        .unwrap();
        let error = load(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported version 3; expected 4")
        );
        assert!(
            error
                .to_string()
                .contains("change `version = 3` to `version = 4`")
        );
    }

    #[test]
    fn duplicate_layers_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(&path, "version = 4\nlayers = [\"cli\", [\"cli\"]]\n").unwrap();

        let error = load(&path).unwrap_err();
        assert!(error.to_string().contains(":2: duplicate layer `cli`"));
    }

    #[test]
    fn module_import_modes_are_mutually_exclusive_and_report_the_section_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(
            &path,
            r#"version = 4
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
            r#"version = 4
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
            version: 4,
            layers: vec![
                Layer::Module("cli".to_owned()),
                Layer::Module("agents".to_owned()),
            ],
            modules: vec![
                ModuleRule {
                    path: PathBuf::from("src/cli"),
                    allowed_imports: None,
                    upward_imports: Some(vec!["agents".to_owned()]),
                    surface_budget: 4,
                    surface_goal: None,
                    upward_debt: None,
                    config_line: 99,
                },
                ModuleRule {
                    path: PathBuf::from("src/agents"),
                    allowed_imports: Some(Vec::new()),
                    upward_imports: None,
                    surface_budget: 2,
                    surface_goal: None,
                    upward_debt: None,
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

    #[test]
    fn grouped_layers_parse_and_singleton_groups_round_trip_as_strings() {
        let target: Target =
            toml::from_str("version = 4\nlayers = [[\"ids\", \"utils\"], [\"store\"], \"cli\"]\n")
                .unwrap();
        assert_eq!(
            target.layers,
            [
                Layer::Group(vec!["ids".to_owned(), "utils".to_owned()]),
                Layer::Group(vec!["store".to_owned()]),
                Layer::Module("cli".to_owned()),
            ]
        );
        let ranks = target.layer_ranks();
        assert_eq!(ranks.get("ids"), Some(0));
        assert_eq!(ranks.get("utils"), Some(0));
        assert_eq!(ranks.get("store"), Some(1));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        write(&path, &target).unwrap();
        let reparsed = load(&path).unwrap().unwrap();
        assert_eq!(
            reparsed.layers,
            [
                Layer::Group(vec!["ids".to_owned(), "utils".to_owned()]),
                Layer::Module("store".to_owned()),
                Layer::Module("cli".to_owned()),
            ]
        );
    }

    #[test]
    fn goal_and_debt_fields_validate_against_budget_and_admissions() {
        let error = load_fixture("surface-goal = 11\nupward-imports = [\"cli\"]\n");
        assert!(error.contains(":4: module `src/store`"));
        assert!(error.contains("surface-goal exceeds surface-budget"));

        let error = load_fixture(
            "surface-goal = 5\nupward-imports = [\"cli\"]\nupward-debt = [\"agents\"]\n",
        );
        assert!(error.contains(":4: module `src/store`"));
        assert!(error.contains("upward-debt `agents` is not present in upward-imports"));

        let error = load_fixture(
            "surface-goal = 5\nallowed-imports = [\"cli\"]\nupward-debt = [\"cli\"]\n",
        );
        assert!(error.contains(":4: module `src/store`"));
        assert!(error.contains("cannot set upward-debt with allowed-imports"));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(&path, "version = 4\nlayers = [[]]\n").unwrap();
        let error = load(&path).unwrap_err().to_string();
        assert!(error.contains(":2: layer groups may not be empty"));
    }

    fn load_fixture(module_fields: &str) -> String {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(
            &path,
            format!(
                "version = 4\nlayers = [\"store\", \"cli\"]\n\n[[module]]\npath = \"src/store\"\nsurface-budget = 10\n{module_fields}"
            ),
        )
        .unwrap();
        load(&path).unwrap_err().to_string()
    }
}
