use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::inspect;
use super::syntax::FileSyntax;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct PassContract {
    pub(super) version: u8,
    pub(super) base: String,
    pub(super) paths: Vec<PathBuf>,
    pub(super) max_production_sloc_delta: i64,
    #[serde(default)]
    pub(super) assembly: Vec<AssemblyExpectation>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct AssemblyExpectation {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) max_items: usize,
}

pub(super) fn load(root: &Path, path: &Path, syntax_files: &[FileSyntax]) -> Result<PassContract> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading Atlas pass contract {}", path.display()))?;
    let contract = toml::from_str(&raw)
        .with_context(|| format!("parsing Atlas pass contract {}", path.display()))?;
    validate(root, syntax_files, contract)
        .with_context(|| format!("validating Atlas pass contract {}", path.display()))
}

fn validate(
    root: &Path,
    syntax_files: &[FileSyntax],
    contract: PassContract,
) -> Result<PassContract> {
    if contract.version != 1 {
        bail!(
            "unsupported pass contract version {}; expected version 1",
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
    if contract.max_production_sloc_delta >= 0 {
        bail!("pass contract max-production-sloc-delta must be negative");
    }
    for expectation in &contract.assembly {
        inspect::resolve_module(
            root,
            syntax_files,
            &expectation.from,
            "diff",
            "contract assembly.from",
        )?;
        inspect::resolve_module(
            root,
            syntax_files,
            &expectation.to,
            "diff",
            "contract assembly.to",
        )?;
    }
    Ok(contract)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::sources::Source;
    use super::*;

    fn syntax() -> (tempfile::TempDir, Vec<FileSyntax>) {
        let root = tempfile::tempdir().unwrap();
        let sources = [
            Source::new("src/caller.rs", "fn call() {}"),
            Source::new("src/provider.rs", "pub fn serve() {}"),
        ];
        let syntax = super::super::syntax::analyze_sources(&sources, &BTreeSet::new());
        (root, syntax.files)
    }

    fn contract() -> PassContract {
        PassContract {
            version: 1,
            base: "main".to_owned(),
            paths: vec![PathBuf::from("src")],
            max_production_sloc_delta: -1,
            assembly: vec![AssemblyExpectation {
                from: "caller".to_owned(),
                to: "provider".to_owned(),
                max_items: 3,
            }],
        }
    }

    #[test]
    fn contract_rejects_unknown_versions() {
        let (root, syntax) = syntax();
        let mut contract = contract();
        contract.version = 2;
        assert!(validate(root.path(), &syntax, contract).is_err());
    }

    #[test]
    fn contract_rejects_empty_paths() {
        let (root, syntax) = syntax();
        let mut contract = contract();
        contract.paths.clear();
        assert!(validate(root.path(), &syntax, contract).is_err());
    }

    #[test]
    fn contract_rejects_invalid_paths() {
        let (root, syntax) = syntax();
        let mut contract = contract();
        contract.paths = vec![PathBuf::from("../src")];
        assert!(validate(root.path(), &syntax, contract).is_err());
    }

    #[test]
    fn contract_rejects_nonnegative_sloc_budgets() {
        let (root, syntax) = syntax();
        let mut contract = contract();
        contract.max_production_sloc_delta = 0;
        assert!(validate(root.path(), &syntax, contract).is_err());
    }

    #[test]
    fn contract_rejects_unresolved_assembly_modules() {
        let (root, syntax) = syntax();
        let mut contract = contract();
        contract.assembly[0].to = "missing".to_owned();
        assert!(validate(root.path(), &syntax, contract).is_err());
    }
}
