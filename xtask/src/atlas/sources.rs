use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use syn::Meta;
use syn::punctuated::Punctuated;
use syn::token::Comma;

use crate::source_files;

use super::modules::path_in_scope;

#[derive(Clone, Debug, Serialize)]
pub(super) struct Source {
    pub(super) path: PathBuf,
    #[serde(skip)]
    pub(super) text: String,
    #[serde(skip)]
    kind: SourceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceKind {
    Production,
    Test,
    TestSupport,
}

impl Source {
    #[cfg(test)]
    pub(super) fn new(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        let path = path.into();
        Self {
            kind: if source_files::is_test_file(&path) {
                SourceKind::Test
            } else {
                SourceKind::Production
            },
            path,
            text: text.into(),
        }
    }

    pub(super) fn is_production(&self) -> bool {
        self.kind == SourceKind::Production
    }

    pub(super) fn is_test(&self) -> bool {
        self.kind == SourceKind::Test
    }
}

pub(super) fn scope_sources(root: &Path, scope: &Path, at: Option<&str>) -> Result<Vec<Source>> {
    let sources = all_sources(root, at)?;
    sources_in_scope(&sources, scope)
}

pub(super) fn all_sources(root: &Path, at: Option<&str>) -> Result<Vec<Source>> {
    let mut sources = if let Some(revision) = at {
        revision_sources(root, revision)?
    } else {
        source_files::tracked_rust_files(root)?
            .into_iter()
            .map(|path| {
                let relative = path
                    .strip_prefix(root)
                    .with_context(|| format!("making {} root-relative", path.display()))?
                    .to_path_buf();
                let text = fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                Ok(Source {
                    path: relative,
                    text,
                    kind: SourceKind::Production,
                })
            })
            .collect::<Result<Vec<_>>>()?
    };
    classify_sources(&mut sources);
    if sources.is_empty() {
        bail!("no tracked Rust files in the repository");
    }
    Ok(sources)
}

pub(super) fn sources_in_scope(sources: &[Source], scope: &Path) -> Result<Vec<Source>> {
    let sources = sources
        .iter()
        .filter(|source| path_in_scope(&source.path, scope))
        .cloned()
        .collect::<Vec<_>>();
    if sources.is_empty() {
        bail!("no tracked Rust files under `{}`", scope.display());
    }
    Ok(sources)
}

fn revision_sources(root: &Path, revision: &str) -> Result<Vec<Source>> {
    let output = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", revision, "--"])
        .current_dir(root)
        .output()
        .with_context(|| format!("listing Rust sources at `{revision}`"))?;
    if !output.status.success() {
        bail!(
            "git ls-tree `{revision}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let paths = String::from_utf8(output.stdout)
        .context("git ls-tree returned non-UTF-8 paths")?
        .lines()
        .map(PathBuf::from)
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting git cat-file --batch")?;
    {
        let stdin = child.stdin.as_mut().context("opening git cat-file stdin")?;
        for path in &paths {
            writeln!(stdin, "{revision}:{}", path.display())
                .context("writing git cat-file request")?;
        }
    }
    drop(child.stdin.take());
    let mut stdout = BufReader::new(child.stdout.take().context("opening git cat-file stdout")?);
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let mut header = String::new();
        stdout
            .read_line(&mut header)
            .context("reading git cat-file header")?;
        let fields = header.split_whitespace().collect::<Vec<_>>();
        if fields.last() == Some(&"missing") {
            bail!("git object `{revision}:{}` is missing", path.display());
        }
        let size = fields
            .get(2)
            .context("malformed git cat-file header")?
            .parse::<usize>()
            .context("invalid git cat-file object size")?;
        let mut bytes = vec![0; size];
        stdout
            .read_exact(&mut bytes)
            .context("reading git cat-file object")?;
        let mut newline = [0];
        stdout
            .read_exact(&mut newline)
            .context("reading git cat-file separator")?;
        let text = String::from_utf8(bytes)
            .with_context(|| format!("{} at `{revision}` is not UTF-8", path.display()))?;
        sources.push(Source {
            path,
            text,
            kind: SourceKind::Production,
        });
    }
    let status = child.wait().context("waiting for git cat-file")?;
    if !status.success() {
        bail!("git cat-file --batch failed");
    }
    Ok(sources)
}

pub(super) fn working_tree_rust_sources(root: &Path) -> Result<Vec<Source>> {
    let mut sources = Vec::new();
    walk(root, root, &mut sources)?;
    classify_sources(&mut sources);
    Ok(sources)
}

fn walk(root: &Path, directory: &Path, sources: &mut Vec<Source>) -> Result<()> {
    for entry in
        fs::read_dir(directory).with_context(|| format!("reading {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if path == root.join(".git") || path == root.join("target") {
                continue;
            }
            walk(root, &path, sources)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let text =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            sources.push(Source {
                path: path
                    .strip_prefix(root)
                    .with_context(|| format!("making {} root-relative", path.display()))?
                    .to_path_buf(),
                text,
                kind: SourceKind::Production,
            });
        }
    }
    Ok(())
}

fn classify_sources(sources: &mut [Source]) {
    for source in sources.iter_mut() {
        if source_files::is_test_file(&source.path) {
            source.kind = SourceKind::Test;
        }
    }

    let indexes = sources
        .iter()
        .enumerate()
        .map(|(index, source)| (source.path.clone(), index))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut modules = Vec::new();
    for (parent, source) in sources.iter().enumerate() {
        let Ok(file) = syn::parse_file(&source.text) else {
            continue;
        };
        for module in file.items.iter().filter_map(|item| match item {
            syn::Item::Mod(module) if module.content.is_none() => Some(module),
            _ => None,
        }) {
            let declared_kind = conditional_source_kind(&module.attrs);
            for candidate in module_file_candidates(&source.path, &module.ident.to_string()) {
                if let Some(child) = indexes.get(&candidate) {
                    modules.push((parent, *child, declared_kind));
                    break;
                }
            }
        }
    }

    loop {
        let mut changed = false;
        for &(parent, child, declared_kind) in &modules {
            let inherited = match sources[parent].kind {
                SourceKind::Production => declared_kind,
                kind => Some(kind),
            };
            let Some(kind) = inherited else {
                continue;
            };
            let merged = merge_source_kind(sources[child].kind, kind);
            if merged != sources[child].kind {
                sources[child].kind = merged;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

fn module_file_candidates(parent: &Path, module: &str) -> [PathBuf; 2] {
    let directory = match parent.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("lib.rs" | "main.rs" | "mod.rs") => parent.parent().unwrap_or_else(|| Path::new("")),
        _ => {
            let mut directory = parent.to_path_buf();
            directory.set_extension("");
            return [
                directory.join(format!("{module}.rs")),
                directory.join(module).join("mod.rs"),
            ];
        }
    };
    [
        directory.join(format!("{module}.rs")),
        directory.join(module).join("mod.rs"),
    ]
}

fn merge_source_kind(current: SourceKind, declared: SourceKind) -> SourceKind {
    match (current, declared) {
        (SourceKind::Test, _) | (_, SourceKind::Test) => SourceKind::Test,
        (SourceKind::TestSupport, _) | (_, SourceKind::TestSupport) => SourceKind::TestSupport,
        _ => SourceKind::Production,
    }
}

fn conditional_source_kind(attributes: &[syn::Attribute]) -> Option<SourceKind> {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("cfg"))
        .filter_map(|attribute| attribute.parse_args::<Meta>().ok())
        .filter_map(|predicate| cfg_predicate_kind(&predicate))
        .reduce(merge_source_kind)
}

fn cfg_predicate_kind(predicate: &Meta) -> Option<SourceKind> {
    match predicate {
        Meta::Path(path) if path.is_ident("test") => Some(SourceKind::Test),
        Meta::NameValue(value) if value.path.is_ident("feature") => match &value.value {
            syn::Expr::Lit(value) => match &value.lit {
                syn::Lit::Str(feature) if feature.value() == "testkit" => {
                    Some(SourceKind::TestSupport)
                }
                _ => None,
            },
            _ => None,
        },
        Meta::List(list) if list.path.is_ident("all") => list
            .parse_args_with(Punctuated::<Meta, Comma>::parse_terminated)
            .ok()?
            .iter()
            .filter_map(cfg_predicate_kind)
            .reduce(merge_source_kind),
        Meta::List(list) if list.path.is_ident("any") => {
            let predicates = list
                .parse_args_with(Punctuated::<Meta, Comma>::parse_terminated)
                .ok()?;
            let kinds = predicates
                .iter()
                .map(cfg_predicate_kind)
                .collect::<Option<Vec<_>>>()?;
            kinds.into_iter().reduce(merge_source_kind)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_tree_walk_keeps_source_modules_named_target() {
        let root = tempfile::tempdir().unwrap();
        let source_target = root.path().join("src/harness/target");
        fs::create_dir_all(&source_target).unwrap();
        fs::write(source_target.join("mod.rs"), "pub struct Kept;\n").unwrap();
        fs::create_dir_all(root.path().join("target/generated")).unwrap();
        fs::write(
            root.path().join("target/generated/ignored.rs"),
            "pub struct Ignored;\n",
        )
        .unwrap();

        let sources = working_tree_rust_sources(root.path()).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].path, Path::new("src/harness/target/mod.rs"));
    }

    #[test]
    fn source_classification_follows_conditional_external_modules() {
        let mut sources = vec![
            Source::new(
                "src/lib.rs",
                "#[cfg(feature = \"testkit\")]\nmod fixture;\n#[cfg(test)]\nmod checks;\n",
            ),
            Source::new("src/fixture.rs", "mod nested;\n"),
            Source::new("src/fixture/nested.rs", "fn helper() {}\n"),
            Source::new("src/checks.rs", "fn characterization() {}\n"),
        ];

        classify_sources(&mut sources);

        assert!(sources[0].is_production());
        assert_eq!(sources[1].kind, SourceKind::TestSupport);
        assert_eq!(sources[2].kind, SourceKind::TestSupport);
        assert!(sources[3].is_test());
    }

    #[test]
    fn cfg_composition_is_order_independent() {
        let test_then_support: syn::ItemMod = syn::parse_quote! {
            #[cfg(all(test, feature = "testkit"))]
            mod fixture;
        };
        let support_then_test: syn::ItemMod = syn::parse_quote! {
            #[cfg(all(feature = "testkit", test))]
            mod fixture;
        };
        let stacked: syn::ItemMod = syn::parse_quote! {
            #[cfg(feature = "testkit")]
            #[cfg(test)]
            mod fixture;
        };
        let production_alternative: syn::ItemMod = syn::parse_quote! {
            #[cfg(any(test, unix))]
            mod fixture;
        };

        assert_eq!(
            conditional_source_kind(&test_then_support.attrs),
            Some(SourceKind::Test)
        );
        assert_eq!(
            conditional_source_kind(&support_then_test.attrs),
            Some(SourceKind::Test)
        );
        assert_eq!(
            conditional_source_kind(&stacked.attrs),
            Some(SourceKind::Test)
        );
        assert_eq!(conditional_source_kind(&production_alternative.attrs), None);
    }
}
