use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use protobuf::Message;
use scip::types::{Index, Occurrence, SymbolRole, occurrence};
use serde::Serialize;

use super::modules::crate_module_for_path;
use super::sources::Source;
use super::syntax::{FileSyntax, PubItem, SyntaxReport};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ItemKey {
    pub(super) path: PathBuf,
    pub(super) module: String,
    pub(super) name: String,
    pub(super) line: usize,
}

impl ItemKey {
    pub(super) fn new(file: &FileSyntax, item: &PubItem) -> Self {
        Self {
            path: file.path.clone(),
            module: item.module.clone(),
            name: item.name.clone(),
            line: item.line,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct ItemRefs {
    pub(super) production: BTreeSet<String>,
    pub(super) tests: BTreeSet<String>,
    pub(super) production_count: usize,
    pub(super) test_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum EdgeKind {
    Use,
    Reference,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Edge {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) item: String,
    pub(super) kind: EdgeKind,
    pub(super) test: bool,
}

#[derive(Debug, Default)]
pub(super) struct References {
    pub(super) items: BTreeMap<ItemKey, ItemRefs>,
    pub(super) edges: Vec<Edge>,
}

impl References {
    pub(super) fn load(
        index_path: &Path,
        syntax: &SyntaxReport,
        sources: &[Source],
    ) -> Result<Self> {
        let bytes = fs::read(index_path)
            .with_context(|| format!("reading SCIP index {}", index_path.display()))?;
        let index = Index::parse_from_bytes(&bytes)
            .with_context(|| format!("parsing SCIP index {}", index_path.display()))?;
        Ok(Self::from_index(&index, syntax, sources))
    }

    pub(super) fn get(&self, file: &FileSyntax, item: &PubItem) -> Option<&ItemRefs> {
        self.items.get(&ItemKey::new(file, item))
    }

    fn from_index(index: &Index, syntax: &SyntaxReport, sources: &[Source]) -> Self {
        let sources_by_path = sources
            .iter()
            .map(|source| (source.path.as_path(), source))
            .collect::<BTreeMap<_, _>>();
        let syntax_by_path = syntax
            .files
            .iter()
            .map(|file| (file.path.as_path(), file))
            .collect::<BTreeMap<_, _>>();
        let mut definitions = BTreeMap::<(PathBuf, usize), BTreeSet<&str>>::new();
        let mut occurrences = BTreeMap::<&str, Vec<ReferenceSite>>::new();

        for document in &index.documents {
            let path = normalized_document_path(&document.relative_path);
            let Some(source) = sources_by_path.get(path.as_path()).copied() else {
                continue;
            };
            // Test-support code contributes to neither production nor test
            // evidence, matching Atlas source-size and syntax classification.
            if !source.is_production() && !source.is_test() {
                continue;
            }
            let file_syntax = syntax_by_path.get(path.as_path()).copied();
            let module = crate_module_for_path(&path);
            for occurrence in &document.occurrences {
                let Some(line) = occurrence_line(occurrence) else {
                    continue;
                };
                if occurrence.symbol.is_empty() {
                    continue;
                }
                if has_role(occurrence, SymbolRole::Definition) {
                    if source.is_production() {
                        definitions
                            .entry((path.clone(), line))
                            .or_default()
                            .insert(&occurrence.symbol);
                    }
                    continue;
                }
                let test = source.is_test()
                    || file_syntax.is_some_and(|file| {
                        file.test_regions
                            .iter()
                            .any(|region| region.contains(&line))
                    });
                occurrences
                    .entry(&occurrence.symbol)
                    .or_default()
                    .push(ReferenceSite {
                        module: module.clone(),
                        test,
                    });
            }
        }

        let mut references = Self::default();
        for file in &syntax.files {
            for item in &file.pub_items {
                let Some(symbols) = definitions.get(&(file.path.clone(), item.line)) else {
                    continue;
                };
                let symbols = symbols
                    .iter()
                    .copied()
                    .filter(|symbol| descriptor_tail_matches(symbol, &item.name))
                    .collect::<BTreeSet<_>>();
                if symbols.is_empty() {
                    continue;
                }

                let mut item_refs = ItemRefs::default();
                for symbol in symbols {
                    for site in occurrences.get(symbol).into_iter().flatten() {
                        if site.test {
                            item_refs.tests.insert(site.module.clone());
                            item_refs.test_count += 1;
                        } else {
                            item_refs.production.insert(site.module.clone());
                            item_refs.production_count += 1;
                        }
                        references.edges.push(Edge {
                            from: site.module.clone(),
                            to: item.module.clone(),
                            item: item.name.clone(),
                            kind: EdgeKind::Reference,
                            test: site.test,
                        });
                    }
                }
                references.items.insert(ItemKey::new(file, item), item_refs);
            }
        }
        references
    }
}

#[derive(Debug)]
struct ReferenceSite {
    module: String,
    test: bool,
}

fn normalized_document_path(relative_path: &str) -> PathBuf {
    Path::new(relative_path)
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect()
}

fn occurrence_line(occurrence: &Occurrence) -> Option<usize> {
    let zero_based = match occurrence.typed_range.as_ref() {
        Some(occurrence::Typed_range::SingleLineRange(range)) => range.line,
        Some(occurrence::Typed_range::MultiLineRange(range)) => range.start_line,
        Some(_) => return None,
        None => *occurrence.range.first()?,
    };
    usize::try_from(zero_based).ok()?.checked_add(1)
}

fn has_role(occurrence: &Occurrence, role: SymbolRole) -> bool {
    occurrence.symbol_roles & role as i32 != 0
}

fn descriptor_tail_matches(symbol: &str, name: &str) -> bool {
    ["().", "#", ".", "/"]
        .iter()
        .any(|suffix| symbol.ends_with(&format!("{name}{suffix}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occurrence(line: i32, symbol: &str, definition: bool) -> Occurrence {
        Occurrence {
            range: vec![line, 0, 1],
            symbol: symbol.to_owned(),
            symbol_roles: if definition {
                SymbolRole::Definition as i32
            } else {
                0
            },
            ..Occurrence::default()
        }
    }

    fn document(path: &str, occurrences: Vec<Occurrence>) -> scip::types::Document {
        scip::types::Document {
            relative_path: path.to_owned(),
            occurrences,
            ..scip::types::Document::default()
        }
    }

    #[test]
    fn joins_definitions_and_classifies_production_and_test_references() {
        let sources = vec![
            Source::new(
                "crates/demo/src/lib.rs",
                "pub fn target() {}\n\n#[cfg(test)]\nmod tests {\n    fn calls_target() { super::target(); }\n}\n",
            ),
            Source::new(
                "crates/demo/src/caller.rs",
                "fn calls_target() { crate::target(); }\n",
            ),
            Source::new(
                "crates/demo/tests/public_api.rs",
                "fn calls_target() { demo::target(); }\n",
            ),
        ];
        let syntax = super::super::syntax::analyze_sources(&sources);
        let target = "rust-analyzer cargo demo 0.1.0 target().";
        let wrong = "rust-analyzer cargo demo 0.1.0 wrong().";
        let index = Index {
            documents: vec![
                document(
                    "./crates/demo/src/lib.rs",
                    vec![
                        occurrence(0, target, true),
                        occurrence(0, wrong, true),
                        occurrence(4, target, false),
                    ],
                ),
                document(
                    "crates/demo/src/caller.rs",
                    vec![occurrence(0, target, false), occurrence(0, wrong, false)],
                ),
                document(
                    "crates/demo/tests/public_api.rs",
                    vec![occurrence(0, target, false)],
                ),
                document("generated/untracked.rs", vec![occurrence(0, target, false)]),
            ],
            ..Index::default()
        };

        let directory = tempfile::tempdir().expect("temporary SCIP directory");
        let index_path = directory.path().join("index.scip");
        scip::write_message_to_file(&index_path, index).expect("write hand-built SCIP index");
        let references =
            References::load(&index_path, &syntax, &sources).expect("parse hand-built SCIP index");
        let (key, item_refs) = references
            .items
            .iter()
            .find(|(key, _)| key.name == "target")
            .expect("target should resolve by its definition site");
        assert_eq!(key.path, Path::new("crates/demo/src/lib.rs"));
        assert_eq!(item_refs.production, BTreeSet::from(["caller".to_owned()]));
        assert_eq!(item_refs.production_count, 1);
        assert_eq!(item_refs.tests, BTreeSet::from([String::new()]));
        assert_eq!(item_refs.test_count, 2);
        assert_eq!(references.edges.len(), 3);
        assert!(references.edges.iter().all(|edge| {
            edge.item == "target" && edge.kind == EdgeKind::Reference && edge.to.is_empty()
        }));
        assert_eq!(references.edges.iter().filter(|edge| edge.test).count(), 2);
    }

    #[test]
    fn accepts_every_scip_descriptor_tail_used_for_public_items() {
        for symbol in ["item().", "item#", "item.", "item/"] {
            assert!(descriptor_tail_matches(symbol, "item"));
        }
        assert!(!descriptor_tail_matches("other().", "item"));
    }

    #[test]
    fn typed_ranges_take_precedence_over_the_legacy_range() {
        let mut occurrence = Occurrence {
            range: vec![40, 0, 1],
            ..Occurrence::default()
        };
        occurrence.set_single_line_range(scip::types::SingleLineRange {
            line: 6,
            ..scip::types::SingleLineRange::default()
        });
        assert_eq!(occurrence_line(&occurrence), Some(7));
    }
}
