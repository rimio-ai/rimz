//! Target loading and in-place tightening that preserves untouched TOML text.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Formatted, Item, Table, Value};

use crate::files;

pub(super) const TARGET_FILE: &str = "refactor-target.toml";

#[derive(Clone, Debug, Deserialize)]
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

/// Edit the existing file, preserving everything outside the tightened values.
pub(super) fn write(path: &Path, target: &Target) -> Result<()> {
    validate(path, target, 1)?;
    let changed = || {
        format!(
            "{} changed while tightening; restore a valid target and rerun atlas conform --tighten",
            path.display()
        )
    };
    let raw = fs::read_to_string(path).with_context(changed)?;
    let mut document = raw.parse::<DocumentMut>().with_context(changed)?;
    if document.get("version").and_then(Item::as_integer) != Some(i64::from(target.version)) {
        bail!("{}", changed());
    }
    for (key, count) in [
        ("module", target.modules.len()),
        ("strangler", target.strangler.len()),
    ] {
        match document.get(key) {
            None if count == 0 => {}
            Some(item) if count == 0 && item.as_array().is_some_and(|array| array.is_empty()) => {}
            Some(item)
                if item
                    .as_array_of_tables()
                    .is_some_and(|tables| tables.len() == count) => {}
            _ => bail!("{}", changed()),
        }
    }
    if let Some(tables) = document
        .get_mut("module")
        .and_then(Item::as_array_of_tables_mut)
    {
        for (table, rule) in tables.iter_mut().zip(&target.modules) {
            if table.get("path").and_then(Item::as_str).map(Path::new) != Some(rule.path.as_path())
            {
                bail!("{}", changed());
            }
            set_integer(table, "surface-budget", rule.surface_budget).with_context(changed)?;
            retain_admissions(
                table,
                "allowed-dependencies",
                rule.allowed_dependencies.as_deref(),
            )
            .with_context(changed)?;
            retain_admissions(
                table,
                "upward-dependencies",
                rule.upward_dependencies.as_deref(),
            )
            .with_context(changed)?;
        }
    }
    if let Some(tables) = document
        .get_mut("strangler")
        .and_then(Item::as_array_of_tables_mut)
    {
        for (table, rule) in tables.iter_mut().zip(&target.strangler) {
            if table.get("path").and_then(Item::as_str).map(Path::new) != Some(rule.path.as_path())
                || table.get("symbol").and_then(Item::as_str) != Some(rule.symbol.as_str())
            {
                bail!("{}", changed());
            }
            set_integer(table, "baseline", rule.baseline).with_context(changed)?;
        }
    }
    let mut rendered = document.to_string();
    if !raw.ends_with('\n') && rendered.ends_with('\n') {
        rendered.pop();
    }
    files::write_atomically(path, rendered.as_bytes())
}

fn set_integer(table: &mut Table, key: &str, number: usize) -> Result<()> {
    let Some(Value::Integer(value)) = table.get_mut(key).and_then(Item::as_value_mut) else {
        bail!("expected integer `{key}`");
    };
    let number =
        i64::try_from(number).with_context(|| format!("`{key}` exceeds TOML integer range"))?;
    if *value.value() != number {
        let decor = value.decor().clone();
        *value = Formatted::new(number);
        *value.decor_mut() = decor;
    }
    Ok(())
}

fn retain_admissions(table: &mut Table, key: &str, list: Option<&[String]>) -> Result<()> {
    let Some(list) = list else {
        table.remove(key);
        return Ok(());
    };
    let array = table
        .get_mut(key)
        .and_then(Item::as_array_mut)
        .with_context(|| format!("tightening requires an existing `{key}` array"))?;
    array.retain(|value| {
        value
            .as_str()
            .is_some_and(|name| list.iter().any(|item| item == name))
    });
    Ok(())
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
    fn write_tightens_values_in_place_and_preserves_the_rest() {
        let fixture = r#"# Owned budgets
version = 5
layers = [["store"], ["cli", "message"]]

[[module]]
path = 'src/store'
upward-dependencies = [
    # retained admission
    'cli',
    "message",
]
# Keep this explanation.
surface-budget  =  0x10 # measured ceiling

[[module]]
path = "src/cli"
allowed-dependencies = [ 'store' ]
surface-budget = 1_000

[[strangler]]
symbol = 'legacy'
path = 'src/store'
baseline = 8 # remaining uses

[[verdict]]
kind = "pass-through"
key = "store::open"
reason = 'keeps the persistence boundary explicit'
# trailing comment

"#;
        let expected = r#"# Owned budgets
version = 5
layers = [["store"], ["cli", "message"]]

[[module]]
path = 'src/store'
upward-dependencies = [
    # retained admission
    'cli',
]
# Keep this explanation.
surface-budget  =  4 # measured ceiling

[[module]]
path = "src/cli"
allowed-dependencies = [ 'store' ]
surface-budget = 1_000

[[strangler]]
symbol = 'legacy'
path = 'src/store'
baseline = 2 # remaining uses

[[verdict]]
kind = "pass-through"
key = "store::open"
reason = 'keeps the persistence boundary explicit'
# trailing comment

"#;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(&path, fixture).unwrap();
        let mut target = load(&path).unwrap().unwrap();

        write(&path, &target).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), fixture);

        target.modules[0].surface_budget = 4;
        target.modules[0].upward_dependencies = Some(vec!["cli".to_owned()]);
        target.strangler[0].baseline = 2;

        write(&path, &target).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn write_removes_empty_upward_admissions_but_keeps_explicit_allowed_admissions() {
        let fixture = "version = 5\nlayers = []\n[[module]]\npath = 'src/store'\nupward-dependencies = ['cli']\nsurface-budget = 4\n[[module]]\npath = 'src/cli'\nallowed-dependencies = ['store']\nsurface-budget = 2";
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        fs::write(&path, fixture).unwrap();
        let mut target = load(&path).unwrap().unwrap();
        target.modules[0].upward_dependencies = None;
        target.modules[1].allowed_dependencies = Some(Vec::new());

        write(&path, &target).unwrap();
        let expected = "version = 5\nlayers = []\n[[module]]\npath = 'src/store'\nsurface-budget = 4\n[[module]]\npath = 'src/cli'\nallowed-dependencies = []\nsurface-budget = 2";
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);

        target.modules[0].upward_dependencies = Some(vec!["cli".to_owned()]);
        let error = format!("{:#}", write(&path, &target).unwrap_err());
        assert!(error.contains("requires an existing `upward-dependencies` array"));
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn write_preserves_explicit_empty_rule_arrays() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        let fixture =
            "version = 5\nlayers = []\nmodule = [ ] # no modules\nstrangler = [] # no stranglers\n";
        fs::write(&path, fixture).unwrap();
        let target = load(&path).unwrap().unwrap();

        write(&path, &target).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), fixture);
    }

    #[test]
    fn write_rejects_changed_rule_counts_and_identities_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        let header = "version = 5\nlayers = []\n";
        let first_module = "[[module]]\npath = 'src/store'\nsurface-budget = 4\n";
        let second_module = "[[module]]\npath = 'src/cli'\nsurface-budget = 2\n";
        let first_strangler =
            "[[strangler]]\npath = 'src/store'\nsymbol = 'legacy'\nbaseline = 3\n";
        let second_strangler = "[[strangler]]\npath = 'src/store'\nsymbol = 'old'\nbaseline = 2\n";
        let fixture =
            format!("{header}{first_module}{second_module}{first_strangler}{second_strangler}");
        fs::write(&path, &fixture).unwrap();
        let mut target = load(&path).unwrap().unwrap();
        target.modules[0].surface_budget = 1;
        target.strangler[0].baseline = 1;

        for changed in [
            format!("{header}{second_module}{first_module}{first_strangler}{second_strangler}"),
            format!("{header}{first_module}{second_module}{second_strangler}{first_strangler}"),
            format!("{header}{first_module}{first_strangler}{second_strangler}"),
            format!("{header}{first_module}{second_module}{first_strangler}"),
            fixture.replace("path = 'src/store'\nsymbol", "path = 'src/message'\nsymbol"),
            "version = [".to_owned(),
        ] {
            fs::write(&path, &changed).unwrap();
            let error = write(&path, &target).unwrap_err().to_string();
            assert!(error.contains("changed while tightening"), "{error}");
            assert_eq!(fs::read_to_string(&path).unwrap(), changed);
        }

        fs::remove_file(&path).unwrap();
        let error = write(&path, &target).unwrap_err().to_string();
        assert!(error.contains("changed while tightening"), "{error}");
        assert!(!path.exists());
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
