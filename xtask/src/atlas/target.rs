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
    pub(super) layers: Vec<Vec<String>>,
    #[serde(default, rename = "module")]
    pub(super) modules: Vec<ModuleRule>,
    #[serde(default)]
    pub(super) strangler: Vec<StranglerRule>,
    #[serde(default, rename = "verdict")]
    pub(super) verdicts: Vec<Verdict>,
}

impl Target {
    pub(super) fn layer_ranks(&self) -> LayerRanks {
        LayerRanks::new(&self.layers)
    }
}

#[derive(Clone, Debug)]
pub(super) struct LayerRanks(BTreeMap<String, usize>);

impl LayerRanks {
    pub(super) fn new(layers: &[Vec<String>]) -> Self {
        Self(
            layers
                .iter()
                .enumerate()
                .flat_map(|(rank, modules)| {
                    modules.iter().cloned().map(move |module| (module, rank))
                })
                .collect(),
        )
    }

    pub(super) fn get(&self, module: &str) -> Option<usize> {
        self.0.get(module).copied()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct ModuleRule {
    pub(super) path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) allowed_dependencies: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) upward_dependencies: Option<Vec<String>>,
    pub(super) surface_budget: usize,
    #[serde(skip)]
    pub(super) config_line: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct StranglerRule {
    pub(super) symbol: String,
    pub(super) path: PathBuf,
    pub(super) baseline: usize,
    #[serde(skip)]
    pub(super) config_line: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct Verdict {
    pub(super) kind: VerdictKind,
    pub(super) key: String,
    pub(super) reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum VerdictKind {
    Item,
    PassThrough,
    Guard,
    Shape,
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
    if version != 5 {
        bail!(
            "{} has unsupported version {}; expected 5",
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
        if layer.is_empty() {
            bail!(
                "{}:{layers_line}: layer groups may not be empty",
                path.display()
            );
        }
        for module in layer {
            if !seen_layers.insert(module) {
                bail!(
                    "{}:{layers_line}: duplicate layer `{module}`",
                    path.display()
                );
            }
        }
    }
    for module in &target.modules {
        if module.allowed_dependencies.is_some() && module.upward_dependencies.is_some() {
            bail!(
                "{}:{}: module `{}` cannot set both allowed-dependencies and upward-dependencies",
                path.display(),
                module.config_line,
                module.path.display()
            );
        }
    }
    for strangler in &target.strangler {
        if syn::parse_str::<syn::Ident>(&strangler.symbol).is_err() {
            bail!(
                "{}:{}: strangler symbol `{}` must be one Rust identifier",
                path.display(),
                strangler.config_line,
                strangler.symbol
            );
        }
    }
    let mut verdicts = BTreeSet::new();
    for verdict in &target.verdicts {
        if verdict.key.trim().is_empty() {
            bail!("{}: verdict keys may not be empty", path.display());
        }
        if verdict.reason.trim().is_empty() {
            bail!(
                "{}: verdict {:?} `{}` requires a non-empty reason",
                path.display(),
                verdict.kind,
                verdict.key
            );
        }
        if !verdicts.insert((verdict.kind, verdict.key.as_str())) {
            bail!(
                "{}: duplicate verdict {:?} `{}`",
                path.display(),
                verdict.kind,
                verdict.key
            );
        }
    }
    Ok(())
}

pub(super) fn write(path: &Path, target: &Target) -> Result<()> {
    validate(path, target, 1)?;
    let mut rendered = toml::to_string_pretty(target).context("rendering refactor target TOML")?;
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    files::write_atomically(path, rendered.as_bytes())
}

#[derive(Serialize)]
struct RuleBlock<'a> {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    module: Vec<&'a ModuleRule>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    strangler: Vec<&'a StranglerRule>,
}

pub(super) fn render_module_rule(rule: &ModuleRule) -> String {
    render_rule_block(RuleBlock {
        module: vec![rule],
        strangler: Vec::new(),
    })
}

pub(super) fn render_strangler_rule(rule: &StranglerRule) -> String {
    render_rule_block(RuleBlock {
        module: Vec::new(),
        strangler: vec![rule],
    })
}

fn render_rule_block(block: RuleBlock<'_>) -> String {
    // These rule types contain only values TOML can represent.
    toml::to_string_pretty(&block).expect("serializing an Atlas rule cannot fail")
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
    fn target_v5_rejects_goal_debt_and_older_versions() {
        let error = load_fixture(4, "");
        assert!(error.contains("unsupported version 4; expected 5"));

        for field in ["surface-goal = 1", "upward-debt = [\"cli\"]"] {
            let error = load_fixture(5, field);
            assert!(error.contains("unknown field"), "{error}");
        }
    }

    #[test]
    fn target_preserves_verdicts_through_write() {
        let target = Target {
            version: 5,
            layers: vec![vec!["store".to_owned()], vec!["cli".to_owned()]],
            modules: vec![ModuleRule {
                path: PathBuf::from("src/store"),
                allowed_dependencies: None,
                upward_dependencies: Some(vec!["cli".to_owned()]),
                surface_budget: 4,
                config_line: 9,
            }],
            strangler: Vec::new(),
            verdicts: vec![Verdict {
                kind: VerdictKind::PassThrough,
                key: "store::open".to_owned(),
                reason: "keeps the persistence boundary explicit".to_owned(),
            }],
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");

        write(&path, &target).unwrap();
        let reparsed = load(&path).unwrap().unwrap();

        assert_eq!(reparsed.layers, target.layers);
        assert_eq!(reparsed.verdicts, target.verdicts);
        assert_eq!(
            reparsed.modules[0].upward_dependencies,
            target.modules[0].upward_dependencies
        );
    }

    #[test]
    fn rendered_rules_round_trip_with_target_formatting() {
        let module = ModuleRule {
            path: PathBuf::from("src/store"),
            allowed_dependencies: Some(vec!["agents".to_owned(), "message".to_owned()]),
            upward_dependencies: None,
            surface_budget: 4,
            config_line: 0,
        };
        let rendered = render_module_rule(&module);
        let document: toml::Value = toml::from_str(&rendered).unwrap();
        let reparsed: ModuleRule = document["module"][0].clone().try_into().unwrap();

        assert_eq!(reparsed, module);
        assert!(
            rendered.contains("allowed-dependencies = [\n    \"agents\",\n    \"message\",\n]")
        );

        let strangler = StranglerRule {
            symbol: "legacy".to_owned(),
            path: PathBuf::from("src/store"),
            baseline: 2,
            config_line: 0,
        };
        let rendered = render_strangler_rule(&strangler);
        let document: toml::Value = toml::from_str(&rendered).unwrap();
        let reparsed: StranglerRule = document["strangler"][0].clone().try_into().unwrap();

        assert_eq!(reparsed, strangler);
    }

    #[test]
    fn strangler_symbol_must_be_one_identifier() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(
            &path,
            "version = 5\nlayers = []\n[[strangler]]\nsymbol = \"store::run\"\npath = \"src\"\nbaseline = 1\n",
        )
        .unwrap();

        let error = load(&path).unwrap_err().to_string();
        assert!(error.contains("must be one Rust identifier"));
    }

    #[test]
    fn verdicts_require_reasons_and_unique_kind_keys() {
        let error =
            load_verdicts("[[verdict]]\nkind = \"item\"\nkey = \"store::open\"\nreason = \" \"\n");
        assert!(error.contains("requires a non-empty reason"));

        let error = load_verdicts(
            "[[verdict]]\nkind = \"guard\"\nkey = \"ready\"\nreason = \"one\"\n[[verdict]]\nkind = \"guard\"\nkey = \"ready\"\nreason = \"two\"\n",
        );
        assert!(error.contains("duplicate verdict"));
    }

    #[test]
    fn layer_groups_validate() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(&path, "version = 5\nlayers = [[\"store\"], [\"store\"]]\n").unwrap();
        assert!(
            load(&path)
                .unwrap_err()
                .to_string()
                .contains("duplicate layer")
        );
    }

    fn load_fixture(version: u8, module_fields: &str) -> String {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(
            &path,
            format!(
                "version = {version}\nlayers = [[\"store\"], [\"cli\"]]\n[[module]]\npath = \"src/store\"\nsurface-budget = 10\n{module_fields}\n"
            ),
        )
        .unwrap();
        format!("{:#}", load(&path).unwrap_err())
    }

    fn load_verdicts(verdicts: &str) -> String {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(&path, format!("version = 5\nlayers = []\n{verdicts}")).unwrap();
        load(&path).unwrap_err().to_string()
    }
}
