use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::inspect;
use super::modules::module_is_within;
use super::syntax::FileSyntax;

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
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum PassKind {
    #[default]
    Module,
    Seam,
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
    pub(super) item: String,
    pub(super) to: String,
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
        validate_base_item(
            base_syntax_files,
            "rehome",
            &expectation.item,
            &contract.base,
        )?;
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
    Ok(contract)
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
    match contract.kind {
        PassKind::Module if contract.max_production_sloc_delta >= 0 => bail!(
            "pass contract max-production-sloc-delta must be negative for a module pass; a seam pass declares kind = \"seam\" and takes a flat ceiling"
        ),
        PassKind::Seam if contract.dependency.is_empty() && contract.rehome.is_empty() => bail!(
            "pass contract kind = \"seam\" needs a [[dependency]] or [[rehome]] row to prove the seam"
        ),
        _ => {}
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
    for expectation in &contract.delete {
        item_parts("delete", &expectation.item)?;
    }
    for expectation in &contract.rehome {
        item_parts("rehome", &expectation.item)?;
        validate_module_path("rehome.to", &expectation.to)?;
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
    let definitions = super::modules::items_for_key(files, key).len();
    if definitions == 0 {
        bail!("pass contract {kind} item `{key}` is not defined at base {base}");
    }
    if definitions > 1 {
        bail!(
            "pass contract {kind} item `{key}` is ambiguous at base {base} ({definitions} definitions)"
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
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

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
            kind: PassKind::Module,
            paths: vec![PathBuf::from("src")],
            max_production_sloc_delta: -1,
            assembly: vec![AssemblyExpectation {
                from: "caller".to_owned(),
                to: "provider".to_owned(),
                max_items: 3,
            }],
            esc: Vec::new(),
            delete: Vec::new(),
            rehome: Vec::new(),
            dependency: Vec::new(),
        }
    }

    #[test]
    fn contract_rejects_unknown_versions() {
        let (root, syntax) = syntax();
        let mut contract = contract();
        contract.version = 3;
        assert!(validate(root.path(), &syntax, &syntax, contract).is_err());
    }

    #[test]
    fn contract_rejects_empty_paths() {
        let (root, syntax) = syntax();
        let mut contract = contract();
        contract.paths.clear();
        assert!(validate(root.path(), &syntax, &syntax, contract).is_err());
    }

    #[test]
    fn contract_rejects_invalid_paths() {
        let (root, syntax) = syntax();
        let mut contract = contract();
        contract.paths = vec![PathBuf::from("../src")];
        assert!(validate(root.path(), &syntax, &syntax, contract).is_err());
    }

    #[test]
    fn module_contract_rejects_nonnegative_sloc_ceiling() {
        let (root, syntax) = syntax();
        let mut contract = contract();
        contract.max_production_sloc_delta = 0;
        let error = validate(root.path(), &syntax, &syntax, contract)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must be negative for a module pass"),
            "{error}"
        );
    }

    #[test]
    fn seam_contract_takes_a_flat_ceiling() {
        let (root, syntax) = syntax();
        let path = root.path().join("pass.toml");
        for ceiling in [0, 12] {
            fs::write(
                &path,
                format!(
                    r#"version = 2
base = "main"
kind = "seam"
paths = ["src"]
max-production-sloc-delta = {ceiling}

[[dependency]]
from = "caller"
to = "provider"
max-sites = 0
"#
                ),
            )
            .unwrap();

            let loaded = load(root.path(), &path, &syntax, &syntax).unwrap();

            assert_eq!(loaded.kind, PassKind::Seam);
            assert_eq!(loaded.max_production_sloc_delta, ceiling);
        }
    }

    #[test]
    fn seam_contract_needs_a_seam_row() {
        let (root, syntax) = syntax();
        let path = root.path().join("pass.toml");
        fs::write(
            &path,
            r#"version = 2
base = "main"
kind = "seam"
paths = ["src"]
max-production-sloc-delta = 0
"#,
        )
        .unwrap();

        let error = format!(
            "{:#}",
            load(root.path(), &path, &syntax, &syntax).unwrap_err()
        );

        assert!(
            error.contains("needs a [[dependency]] or [[rehome]] row"),
            "{error}"
        );
    }

    #[test]
    fn contract_rejects_unresolved_assembly_modules() {
        let (root, syntax) = syntax();
        let mut contract = contract();
        contract.assembly[0].to = "missing".to_owned();
        assert!(validate(root.path(), &syntax, &syntax, contract).is_err());
    }

    #[test]
    fn v2_contract_loads_esc_and_delete_rows() {
        let (root, current) = syntax();
        let base_sources = [
            Source::new("src/caller.rs", "fn call() {}"),
            Source::new(
                "src/provider.rs",
                "pub const OLD: usize = 1;\npub fn serve() {}",
            ),
        ];
        let base = super::super::syntax::analyze_sources(&base_sources, &BTreeSet::new());
        let path = root.path().join("pass.toml");
        fs::write(
            &path,
            r#"version = 2
base = "main"
paths = ["src"]
max-production-sloc-delta = -1

[[esc]]
path = "src"
max = 2

[[delete]]
item = "provider::OLD"
"#,
        )
        .unwrap();

        let loaded = load(root.path(), &path, &current, &base.files).unwrap();

        assert_eq!(loaded.version, 2);
        assert_eq!(loaded.esc.len(), 1);
        assert_eq!(loaded.delete.len(), 1);
    }

    #[test]
    fn v1_contract_is_still_accepted() {
        let (root, syntax) = syntax();
        let path = root.path().join("pass.toml");
        fs::write(
            &path,
            r#"version = 1
base = "main"
paths = ["src"]
max-production-sloc-delta = -1
"#,
        )
        .unwrap();

        let loaded = load(root.path(), &path, &syntax, &syntax).unwrap();

        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.kind, PassKind::Module);
        assert!(loaded.esc.is_empty());
        assert!(loaded.delete.is_empty());
        assert!(loaded.rehome.is_empty());
        assert!(loaded.dependency.is_empty());
    }

    #[test]
    fn delete_row_for_item_absent_at_base_is_rejected_at_load() {
        let (root, syntax) = syntax();
        let path = root.path().join("pass.toml");
        fs::write(
            &path,
            r#"version = 2
base = "HEAD~3"
paths = ["src"]
max-production-sloc-delta = -1

[[delete]]
item = "provider::NEVER_DEFINED"
"#,
        )
        .unwrap();

        let error = load(root.path(), &path, &syntax, &syntax).unwrap_err();

        assert!(
            format!("{error:#}").contains("provider::NEVER_DEFINED` is not defined at base HEAD~3")
        );
    }

    #[test]
    fn v2_contract_loads_rehome_rows() {
        let root = tempfile::tempdir().unwrap();
        let base_sources = [Source::new(
            "src/message.rs",
            "pub struct Thing;\npub fn send() {}",
        )];
        let current_sources = [
            Source::new("src/message.rs", "pub fn send() {}"),
            Source::new("src/store.rs", "pub struct Thing;"),
        ];
        let base = super::super::syntax::analyze_sources(&base_sources, &BTreeSet::new());
        let current = super::super::syntax::analyze_sources(&current_sources, &BTreeSet::new());
        let path = root.path().join("pass.toml");
        fs::write(
            &path,
            r#"version = 2
base = "main"
paths = ["src"]
max-production-sloc-delta = -1

[[rehome]]
item = "message::Thing"
to = "store"
"#,
        )
        .unwrap();

        let loaded = load(root.path(), &path, &current.files, &base.files).unwrap();

        assert_eq!(loaded.rehome.len(), 1);
        assert_eq!(loaded.rehome[0].item, "message::Thing");
        assert_eq!(loaded.rehome[0].to, "store");
    }

    #[test]
    fn rehome_row_for_item_absent_at_base_is_rejected_at_load() {
        let (root, syntax) = syntax();
        let path = root.path().join("pass.toml");
        fs::write(
            &path,
            r#"version = 2
base = "HEAD~3"
paths = ["src"]
max-production-sloc-delta = -1

[[rehome]]
item = "provider::NEVER_DEFINED"
to = "caller"
"#,
        )
        .unwrap();

        let error = load(root.path(), &path, &syntax, &syntax).unwrap_err();

        assert!(
            format!("{error:#}").contains("provider::NEVER_DEFINED` is not defined at base HEAD~3")
        );
    }

    #[test]
    fn v2_contract_loads_dependency_rows() {
        let (root, syntax) = syntax();
        let path = root.path().join("pass.toml");
        fs::write(
            &path,
            r#"version = 2
base = "main"
paths = ["src"]
max-production-sloc-delta = -1

[[dependency]]
from = "caller"
to = "provider"
max-sites = 0
"#,
        )
        .unwrap();

        let loaded = load(root.path(), &path, &syntax, &syntax).unwrap();

        assert_eq!(loaded.dependency.len(), 1);
        assert_eq!(loaded.dependency[0].from, "caller");
        assert_eq!(loaded.dependency[0].to, "provider");
        assert_eq!(loaded.dependency[0].max_sites, 0);
    }
}
