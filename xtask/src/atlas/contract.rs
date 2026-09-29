use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::inspect;
use super::modules::module_is_within;
use super::syntax::{FileSyntax, FnBody, join_module};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct PassContract {
    pub(super) version: u8,
    pub(super) base: String,
    #[serde(default)]
    pub(super) kind: PassKind,
    pub(super) paths: Vec<PathBuf>,
    pub(super) max_production_sloc_delta: i64,
    #[serde(default)]
    pub(super) assembly: Vec<AssemblyExpectation>,
    #[serde(default)]
    pub(super) esc: Vec<EscExpectation>,
    #[serde(default)]
    pub(super) delete: Vec<DeleteExpectation>,
    #[serde(default)]
    pub(super) rehome: Vec<RehomeExpectation>,
    #[serde(default)]
    pub(super) dependency: Vec<DependencyExpectation>,
    #[serde(default)]
    pub(super) cx: Vec<CxExpectation>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CxExpectation {
    pub(super) item: Option<String>,
    pub(super) path: Option<PathBuf>,
    pub(super) max: f64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum PassKind {
    #[default]
    Module,
    Seam,
    /// Adds a user-decided capability to `xtask/`: its ceiling is priced from
    /// its target, so `diff --expect` asks it for no narrowing.
    Tooling,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct AssemblyExpectation {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) max_items: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct EscExpectation {
    pub(super) path: PathBuf,
    pub(super) max: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct DeleteExpectation {
    pub(super) item: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct RehomeExpectation {
    pub(super) item: Option<String>,
    pub(super) from: Option<String>,
    pub(super) to: String,
    pub(super) min_decisions: Option<u64>,
}

/// The one form a `[[rehome]]` row takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RehomeForm<'a> {
    /// `item` + `to`: one definition leaves whole and lands once under `to`.
    Item(&'a str),
    /// `from` + `to` + `min-decisions`: decisions leave `from` for `to`
    /// while `from`'s entry functions stay.
    Logic { from: &'a str, min_decisions: u64 },
}

impl RehomeExpectation {
    pub(super) fn form(&self) -> Result<RehomeForm<'_>> {
        match (&self.item, &self.from, self.min_decisions) {
            (Some(item), None, None) => Ok(RehomeForm::Item(item)),
            (None, Some(from), Some(min_decisions)) => Ok(RehomeForm::Logic {
                from,
                min_decisions,
            }),
            _ => bail!(
                "pass contract rehome row to `{}` must set exactly one form: `item` + `to` (item form), or `from` + `to` + `min-decisions` (logic form)",
                self.to
            ),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct DependencyExpectation {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) max_sites: usize,
}

pub(super) fn read_base(path: &Path) -> Result<String> {
    let contract = parse(path)?;
    validate_schema(&contract)
        .with_context(|| format!("validating Atlas pass contract {}", path.display()))?;
    Ok(contract.base)
}

pub(super) fn load(
    root: &Path,
    path: &Path,
    current_syntax_files: &[FileSyntax],
    base_syntax_files: &[FileSyntax],
) -> Result<PassContract> {
    let contract = parse(path)?;
    validate(root, current_syntax_files, base_syntax_files, contract)
        .with_context(|| format!("validating Atlas pass contract {}", path.display()))
}

impl PassContract {
    /// Whether a row reads `rust-code-analysis` metrics: a `[[cx]]` row or a
    /// logic-form `[[rehome]]` row.
    pub(super) fn needs_metrics(&self) -> bool {
        !self.cx.is_empty()
            || self
                .rehome
                .iter()
                .any(|row| matches!(row.form(), Ok(RehomeForm::Logic { .. })))
    }
}

fn parse(path: &Path) -> Result<PassContract> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading Atlas pass contract {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parsing Atlas pass contract {}", path.display()))
}

fn validate(
    root: &Path,
    current_syntax_files: &[FileSyntax],
    base_syntax_files: &[FileSyntax],
    contract: PassContract,
) -> Result<PassContract> {
    validate_schema(&contract)?;
    for expectation in &contract.assembly {
        inspect::resolve_module(
            root,
            current_syntax_files,
            &expectation.from,
            "diff",
            "contract assembly.from",
        )?;
        inspect::resolve_module(
            root,
            current_syntax_files,
            &expectation.to,
            "diff",
            "contract assembly.to",
        )?;
    }
    for expectation in &contract.delete {
        validate_base_item(
            base_syntax_files,
            "delete",
            &expectation.item,
            &contract.base,
        )?;
    }
    for expectation in &contract.rehome {
        match expectation.form()? {
            RehomeForm::Item(item) => {
                validate_base_item(base_syntax_files, "rehome", item, &contract.base)?;
                if !base_syntax_files
                    .iter()
                    .chain(current_syntax_files)
                    .any(|file| module_is_within(&file.module_path, &expectation.to))
                {
                    bail!(
                        "pass contract rehome.to `{}` does not match a module at base or current",
                        expectation.to
                    );
                }
            }
            RehomeForm::Logic { from, .. } => {
                if !has_production_module(&contract, base_syntax_files, from) {
                    bail!(
                        "pass contract rehome.from `{from}` has no production files inside pass contract paths at base {}",
                        contract.base
                    );
                }
                // A new owner is allowed: `to` may exist only at current.
                if !has_production_module(&contract, base_syntax_files, &expectation.to)
                    && !has_production_module(&contract, current_syntax_files, &expectation.to)
                {
                    bail!(
                        "pass contract rehome.to `{}` has no production files inside pass contract paths at base or current",
                        expectation.to
                    );
                }
            }
        }
    }
    for expectation in &contract.cx {
        let Some(item) = &expectation.item else {
            continue;
        };
        let base = cx_functions(base_syntax_files, item);
        let current = cx_functions(current_syntax_files, item);
        if base.is_empty() && current.is_empty() {
            bail!("pass contract cx item `{item}` is defined at neither base nor current");
        }
        for (side, functions) in [("base", base), ("current", current)] {
            if functions.len() > 1 {
                let sites = functions
                    .iter()
                    .map(|function| format!("{}:{}", function.path.display(), function.line))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!("pass contract cx item `{item}` is ambiguous at {side}: {sites}");
            }
            for function in functions {
                validate_cx_path(&contract, &function.path)?;
            }
        }
    }
    Ok(contract)
}

pub(super) fn cx_functions<'a>(files: &'a [FileSyntax], item: &str) -> Vec<&'a FnBody> {
    files
        .iter()
        .flat_map(|file| &file.fns)
        .filter(|function| join_module(&function.module, &function.label()) == item)
        .collect()
}

/// Whether a production file of `module` (or beneath it) sits inside the
/// contract paths, the files a logic-form rehome measures.
fn has_production_module(contract: &PassContract, files: &[FileSyntax], module: &str) -> bool {
    files.iter().any(|file| {
        module_is_within(&file.module_path, module)
            && !crate::source_files::is_test_file(&file.path)
            && contract.paths.iter().any(|scope| {
                super::modules::path_in_scope(&file.path, scope)
                    || file.path == scope.with_extension("rs")
            })
    })
}

fn validate_cx_path(contract: &PassContract, path: &Path) -> Result<()> {
    super::validate_scope(
        path.to_str()
            .context("pass contract cx paths must contain valid UTF-8")?,
        "diff contract cx.path",
    )?;
    if !contract.paths.iter().any(|scope| {
        super::modules::path_in_scope(path, scope) || path == scope.with_extension("rs")
    }) {
        bail!(
            "pass contract cx path `{}` must be inside pass contract paths",
            path.display()
        );
    }
    Ok(())
}

fn validate_schema(contract: &PassContract) -> Result<()> {
    if !matches!(contract.version, 1 | 2) {
        bail!(
            "unsupported pass contract version {}; expected version 1 or 2",
            contract.version
        );
    }
    if contract.base.is_empty() {
        bail!("pass contract base must not be empty");
    }
    if contract.paths.is_empty() {
        bail!("pass contract paths must not be empty");
    }
    for path in &contract.paths {
        super::validate_scope(
            path.to_str()
                .context("pass contract paths must contain valid UTF-8")?,
            "diff contract path",
        )?;
    }
    if contract.kind == PassKind::Seam
        && contract.dependency.is_empty()
        && contract.rehome.is_empty()
    {
        bail!(
            "pass contract kind = \"seam\" needs a [[dependency]] or [[rehome]] row to prove the seam"
        );
    }
    for expectation in &contract.esc {
        let path = super::validate_scope(
            expectation
                .path
                .to_str()
                .context("pass contract esc paths must contain valid UTF-8")?,
            "diff contract esc.path",
        )?;
        if !contract
            .paths
            .iter()
            .any(|scope| path.starts_with(scope) || path == scope.with_extension("rs"))
        {
            bail!(
                "pass contract esc path `{}` must be inside pass contract paths",
                path.display()
            );
        }
    }
    for expectation in &contract.cx {
        match (&expectation.item, &expectation.path) {
            (Some(item), None) => {
                item_parts("cx", item)?;
            }
            (None, Some(path)) => validate_cx_path(contract, path)?,
            _ => bail!("pass contract cx row must set exactly one of item or path"),
        }
    }
    for expectation in &contract.delete {
        item_parts("delete", &expectation.item)?;
    }
    for expectation in &contract.rehome {
        validate_module_path("rehome.to", &expectation.to)?;
        match expectation.form()? {
            RehomeForm::Item(item) => {
                item_parts("rehome", item)?;
            }
            RehomeForm::Logic {
                from,
                min_decisions,
            } => {
                validate_module_path("rehome.from", from)?;
                if min_decisions == 0 {
                    bail!(
                        "pass contract rehome row `{from}` → `{}` needs min-decisions >= 1",
                        expectation.to
                    );
                }
                if module_is_within(from, &expectation.to)
                    || module_is_within(&expectation.to, from)
                {
                    bail!(
                        "pass contract rehome row `{from}` → `{}` names modules within one another; from and to must be disjoint",
                        expectation.to
                    );
                }
            }
        }
    }
    for expectation in &contract.dependency {
        validate_module_path("dependency.from", &expectation.from)?;
        validate_module_path("dependency.to", &expectation.to)?;
    }
    Ok(())
}

fn item_parts<'a>(kind: &str, key: &'a str) -> Result<(&'a str, &'a str)> {
    let Some((module, name)) = key.rsplit_once("::") else {
        bail!("pass contract {kind} item `{key}` must use `module::Name`");
    };
    if module.is_empty() || name.is_empty() {
        bail!("pass contract {kind} item `{key}` must use `module::Name`");
    }
    Ok((module, name))
}

fn validate_base_item(files: &[FileSyntax], kind: &str, key: &str, base: &str) -> Result<()> {
    item_parts(kind, key)?;
    let definitions = super::modules::definitions_for_key(files, key);
    if definitions.is_empty() {
        bail!("pass contract {kind} item `{key}` is not defined at base {base}");
    }
    if definitions.len() > 1 {
        let sites = definitions
            .iter()
            .map(|definition| format!("{}:{}", definition.file.path.display(), definition.line))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "pass contract {kind} item `{key}` is ambiguous at base {base} ({} definitions: {sites})",
            definitions.len()
        );
    }
    Ok(())
}

fn validate_module_path(field: &str, module: &str) -> Result<()> {
    let valid = !module.is_empty()
        && module.split("::").all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|first| first == '_' || first.is_alphabetic())
                && chars.all(|character| character == '_' || character.is_alphanumeric())
        });
    if !valid {
        bail!("pass contract {field} `{module}` must be a non-empty `::`-separated module path");
    }
    Ok(())
}

#[cfg(test)]
#[path = "contract/tests.rs"]
mod tests;
