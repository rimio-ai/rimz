//! Raw SCIP evidence before atlas's reference filtering.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use scip::types::{Index, SymbolRole, symbol_information::Kind};
use serde::Serialize;
use serde_json::json;

use super::output::{self, OutputArgs};
use super::references::{
    FnRef, descriptor_tail_matches, has_role, normalized_document_path, occurrence_line, read_index,
};
use super::syntax::{self, SyntaxReport};
use super::{index, modules, set_once, sources, validate_scope, value};

const USAGE: &str = "cargo xtask atlas index [--doc <path>] [--symbol <name>]";
const SECTIONS: &[&str] = &["symbols", "occurrences"];

fn usage() -> String {
    format!("{USAGE}\n\n{}", output::USAGE)
}

#[derive(Debug, Default, Serialize)]
struct Query {
    doc: Option<PathBuf>,
    symbol: Option<String>,
}

#[derive(Debug)]
struct Args {
    query: Query,
    output: OutputArgs,
}

#[derive(Default, Serialize)]
struct Unlisted {
    locals: usize,
    no_line: usize,
}

#[derive(Default, Serialize)]
struct Symbol {
    symbol: String,
    display_name: Option<String>,
    kind: Option<String>,
    enclosing_symbol: Option<String>,
    definitions: Vec<String>,
    occurrences: usize,
}

#[derive(Serialize)]
struct Site {
    path: PathBuf,
    line: usize,
    roles: Vec<&'static str>,
    symbol: String,
    enclosing_fn: Option<FnRef>,
}

#[derive(Default)]
struct Report {
    doc_found: Option<bool>,
    unlisted: Unlisted,
    symbols: Vec<Symbol>,
    occurrences: Vec<Site>,
}

impl Report {
    fn enclose(&mut self, syntax: &SyntaxReport) {
        let files = syntax
            .files
            .iter()
            .map(|file| (file.path.as_path(), file))
            .collect::<BTreeMap<_, _>>();
        for site in &mut self.occurrences {
            site.enclosing_fn = files
                .get(site.path.as_path())
                .and_then(|file| file.enclosing_fn(site.line))
                .map(|function| FnRef {
                    label: function.label(),
                    line: function.line,
                });
        }
    }
}

pub(super) fn run(root: &Path, raw: &[String]) -> Result<()> {
    let Some(args) = parse_args(raw)? else {
        return OutputArgs::default().emit(&format!("{}\n", usage()));
    };
    let mut sources = sources::working_tree_rust_sources(root)?;
    let index_path = index::ensure(root, &sources)?;
    let index = read_index(&index_path)?;
    let crate_names = modules::workspace_crate_names(root)?;
    let mut report = build_report(&index, &args.query);
    let paths = report
        .occurrences
        .iter()
        .map(|site| site.path.clone())
        .collect::<BTreeSet<_>>();
    sources.retain(|source| paths.contains(&source.path));
    report.enclose(&syntax::analyze_sources(&sources, &crate_names));
    let rendered = if args.output.json {
        render_json(&report, &index_path, &args.query, &args.output)?
    } else {
        render_markdown(&report, &index_path, &args.query, &args.output)
    };
    args.output.emit(&rendered)
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut query = Query::default();
    let mut output = OutputArgs::default();
    let mut index = 0;
    while index < args.len() {
        if let Some(consumed) = output.parse_flag(args, index, "index")? {
            index += consumed;
            continue;
        }
        match args[index].as_str() {
            "--doc" => {
                let raw = value(args, index, "index", "--doc")?;
                validate_scope(raw, "--doc")?;
                set_once(
                    &mut query.doc,
                    normalized_document_path(raw),
                    "index",
                    "--doc",
                )?;
            }
            "--symbol" => {
                let raw = value(args, index, "index", "--symbol")?;
                if raw.contains('#')
                    || !syn::parse_str::<syn::Ident>(raw).is_ok_and(|ident| ident == raw)
                {
                    bail!(
                        "atlas index: invalid --symbol `{raw}`; --symbol takes one item name such as `open`; narrow with --doc <file that defines it>"
                    );
                }
                set_once(&mut query.symbol, raw.to_owned(), "index", "--symbol")?;
            }
            other => bail!("unknown atlas index argument `{other}`\n\n{}", usage()),
        }
        index += 2;
    }
    if query.doc.is_none() && query.symbol.is_none() {
        bail!(
            "atlas index needs --doc <path>, --symbol <name>, or both\n\n{}",
            usage()
        );
    }
    output.validate_sections("index", SECTIONS)?;
    Ok(Some(Args { query, output }))
}

fn build_report(index: &Index, query: &Query) -> Report {
    let documents = index
        .documents
        .iter()
        .map(|document| (normalized_document_path(&document.relative_path), document))
        .collect::<Vec<_>>();
    let mut symbols = BTreeMap::<&str, Symbol>::new();
    for (path, document) in &documents {
        if query.doc.as_ref().is_some_and(|doc| doc != path) {
            continue;
        }
        for occurrence in &document.occurrences {
            let symbol = occurrence.symbol.as_str();
            if symbol.starts_with("local ") {
                continue;
            }
            if let Some(name) = &query.symbol
                && (!descriptor_tail_matches(symbol, name)
                    || (query.doc.is_some() && !has_role(occurrence, SymbolRole::Definition)))
            {
                continue;
            }
            symbols.entry(symbol).or_insert_with(|| Symbol {
                symbol: symbol.to_owned(),
                ..Default::default()
            });
        }
    }
    let mut report = Report {
        doc_found: query
            .doc
            .as_ref()
            .map(|doc| documents.iter().any(|(path, _)| path == doc)),
        ..Default::default()
    };
    for (path, document) in &documents {
        let list_document = query.symbol.is_some() || query.doc.as_ref() == Some(path);
        for information in &document.symbols {
            let Some(symbol) = symbols.get_mut(information.symbol.as_str()) else {
                continue;
            };
            if symbol.display_name.is_none() && !information.display_name.is_empty() {
                symbol.display_name = Some(information.display_name.clone());
            }
            if symbol.enclosing_symbol.is_none() && !information.enclosing_symbol.is_empty() {
                symbol.enclosing_symbol = Some(information.enclosing_symbol.clone());
            }
            if symbol.kind.is_none() {
                symbol.kind = information
                    .kind
                    .enum_value()
                    .ok()
                    .filter(|kind| *kind != Kind::UnspecifiedKind)
                    .map(|kind| format!("{kind:?}"));
            }
        }
        for occurrence in &document.occurrences {
            if occurrence.symbol.starts_with("local ") {
                if list_document && query.symbol.is_none() {
                    report.unlisted.locals += 1;
                }
                continue;
            }
            let Some(symbol) = symbols.get_mut(occurrence.symbol.as_str()) else {
                continue;
            };
            let Some(line) = occurrence_line(occurrence) else {
                if list_document {
                    report.unlisted.no_line += 1;
                }
                continue;
            };
            if has_role(occurrence, SymbolRole::Definition) {
                symbol
                    .definitions
                    .push(format!("{}:{line}", path.display()));
            }
            if !list_document {
                continue;
            }
            symbol.occurrences += 1;
            let roles = [
                (SymbolRole::Definition, "definition"),
                (SymbolRole::Import, "import"),
                (SymbolRole::WriteAccess, "write"),
                (SymbolRole::ReadAccess, "read"),
                (SymbolRole::Generated, "generated"),
                (SymbolRole::Test, "test"),
                (SymbolRole::ForwardDefinition, "forward-definition"),
            ]
            .into_iter()
            .filter_map(|(role, name)| has_role(occurrence, role).then_some(name))
            .collect();
            report.occurrences.push(Site {
                path: path.clone(),
                line,
                roles,
                symbol: occurrence.symbol.clone(),
                enclosing_fn: None,
            });
        }
    }
    report.symbols = symbols
        .into_values()
        .map(|mut symbol| {
            symbol.definitions.sort();
            symbol.definitions.dedup();
            symbol
        })
        .collect();
    report
        .occurrences
        .sort_by(|a, b| (&a.path, a.line, &a.symbol).cmp(&(&b.path, b.line, &b.symbol)));
    report
}

fn render_json(
    report: &Report,
    index_path: &Path,
    query: &Query,
    output: &OutputArgs,
) -> Result<String> {
    let mut value = json!({ "index_path": index_path, "query": query, "doc_found": report.doc_found, "unlisted": report.unlisted });
    if output.wants("symbols") {
        value["symbols"] = serde_json::to_value(&report.symbols)?;
    }
    if output.wants("occurrences") {
        value["occurrences"] = serde_json::to_value(&report.occurrences)?;
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

fn markdown_code(value: &str) -> String {
    format!(
        "<code>{}</code>",
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('|', "&#124;")
            .replace('\n', "&#10;")
            .replace('\r', "&#13;")
    )
}

fn markdown_cell(value: Option<&str>) -> String {
    value.map_or_else(|| "-".to_owned(), markdown_code)
}

fn render_markdown(
    report: &Report,
    index_path: &Path,
    query: &Query,
    output: &OutputArgs,
) -> String {
    let mut text = format!(
        "# SCIP index\n\nIndex: {}\n",
        markdown_code(&index_path.display().to_string())
    );
    if let Some(doc) = &query.doc {
        let status = if report.doc_found == Some(true) {
            "found"
        } else {
            "not found"
        };
        let _ = writeln!(
            text,
            "\nDocument: {} ({status})",
            markdown_code(&doc.display().to_string())
        );
    }
    if let Some(symbol) = &query.symbol {
        let _ = writeln!(text, "\nSymbol: {}", markdown_code(symbol));
    }
    let _ = writeln!(
        text,
        "\nUnlisted: locals={}, no_line={}",
        report.unlisted.locals, report.unlisted.no_line
    );
    if output.wants("symbols") {
        text.push_str("\n## Symbols\n\n| Symbol | Display name | Kind | Enclosing symbol | Definitions | Occurrences |\n| --- | --- | --- | --- | --- | --- |\n");
        for symbol in &report.symbols {
            let _ = writeln!(
                text,
                "| {} | {} | {} | {} | {} | {} |",
                markdown_code(&symbol.symbol),
                markdown_cell(symbol.display_name.as_deref()),
                markdown_cell(symbol.kind.as_deref()),
                markdown_cell(symbol.enclosing_symbol.as_deref()),
                markdown_cell(
                    (!symbol.definitions.is_empty())
                        .then(|| symbol.definitions.join(", "))
                        .as_deref()
                ),
                symbol.occurrences
            );
        }
    }
    if output.wants("occurrences") {
        text.push_str("\n## Occurrences\n\n| Site | Roles | Enclosing fn | Symbol |\n| --- | --- | --- | --- |\n");
        for site in &report.occurrences {
            let roles = if site.roles.is_empty() {
                "reference".to_owned()
            } else {
                site.roles.join(", ")
            };
            let function = site
                .enclosing_fn
                .as_ref()
                .map(|function| format!("{}:{}", function.label, function.line));
            let _ = writeln!(
                text,
                "| {} | {roles} | {} | {} |",
                markdown_code(&format!("{}:{}", site.path.display(), site.line)),
                markdown_cell(function.as_deref()),
                markdown_code(&site.symbol)
            );
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::super::{sources::Source, syntax};
    use super::*;
    use scip::types::{
        Document, Occurrence, SymbolInformation, SymbolRole, symbol_information::Kind,
    };

    fn occurrence(line: i32, symbol: &str, roles: i32) -> Occurrence {
        Occurrence {
            range: vec![line, 0, 1],
            symbol: symbol.into(),
            symbol_roles: roles,
            ..Default::default()
        }
    }

    fn document(path: &str, occurrences: Vec<Occurrence>) -> Document {
        Document {
            relative_path: path.into(),
            occurrences,
            ..Default::default()
        }
    }

    fn report(doc: Option<&str>, symbol: Option<&str>, documents: Vec<Document>) -> Report {
        build_report(
            &Index {
                documents,
                ..Default::default()
            },
            &Query {
                doc: doc.map(PathBuf::from),
                symbol: symbol.map(String::from),
            },
        )
    }

    #[test]
    fn symbol_lists_index_wide_sites_and_metadata_before_filtering() {
        let name = "rust-analyzer cargo demo 0.0.0 ensure().";
        let mut definition = document("a.rs", vec![occurrence(0, name, 1)]);
        definition.symbols.push(SymbolInformation {
            symbol: name.into(),
            display_name: "ensure".into(),
            kind: Kind::Function.into(),
            enclosing_symbol: "owner#".into(),
            ..Default::default()
        });
        let result = report(
            None,
            Some("ensure"),
            vec![
                definition,
                document(
                    "b.rs",
                    vec![
                        occurrence(1, name, 0),
                        occurrence(2, "ensure_revision().", 0),
                        occurrence(2, "demo/should_ensure().", 0),
                        occurrence(3, "local 3", 0),
                        Occurrence {
                            symbol: name.into(),
                            ..Default::default()
                        },
                    ],
                ),
                document("generated/untracked.rs", vec![occurrence(4, name, 0)]),
            ],
        );
        assert_eq!(result.occurrences.len(), 3);
        assert_eq!(result.symbols.len(), 1);
        let symbol = &result.symbols[0];
        assert_eq!(symbol.occurrences, 3);
        assert_eq!(symbol.definitions, ["a.rs:1"]);
        assert_eq!(symbol.display_name.as_deref(), Some("ensure"));
        assert_eq!(symbol.kind.as_deref(), Some("Function"));
        assert_eq!(symbol.enclosing_symbol.as_deref(), Some("owner#"));
        assert_eq!((result.unlisted.locals, result.unlisted.no_line), (0, 1));
        assert_eq!(
            result.occurrences[2].path,
            Path::new("generated/untracked.rs")
        );
    }

    #[test]
    fn doc_and_symbol_select_by_definition_not_reference() {
        let result = report(
            Some("a.rs"),
            Some("open"),
            vec![
                document(
                    "a.rs",
                    vec![occurrence(0, "a/open().", 1), occurrence(1, "b/open().", 0)],
                ),
                document(
                    "b.rs",
                    vec![occurrence(0, "b/open().", 1), occurrence(1, "a/open().", 0)],
                ),
            ],
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].symbol, "a/open().");
        assert_eq!(result.occurrences.len(), 2);
        assert_eq!(result.occurrences[1].path, Path::new("b.rs"));
    }

    #[test]
    fn doc_only_normalizes_and_sorts_sites() {
        let result = report(
            Some("a.rs"),
            None,
            vec![
                document(
                    "./a.rs",
                    vec![
                        occurrence(4, "z#", 0),
                        occurrence(0, "a#", 0),
                        occurrence(2, "local 3", 0),
                    ],
                ),
                document("b.rs", vec![occurrence(0, "a#", 1)]),
            ],
        );
        assert_eq!(result.doc_found, Some(true));
        assert_eq!(
            result
                .occurrences
                .iter()
                .map(|site| site.line)
                .collect::<Vec<_>>(),
            [1, 5]
        );
        assert_eq!(result.symbols[0].definitions, ["b.rs:1"]);
        assert_eq!(result.unlisted.locals, 1);
    }

    #[test]
    fn roles_preserve_all_bits_and_plain_references() {
        let roles = [
            SymbolRole::Definition as i32,
            0,
            SymbolRole::Definition as i32 | SymbolRole::WriteAccess as i32,
            127,
        ];
        let result = report(
            Some("a.rs"),
            None,
            vec![document(
                "a.rs",
                roles
                    .into_iter()
                    .enumerate()
                    .map(|(line, roles)| occurrence(line as i32, "a#", roles))
                    .collect(),
            )],
        );
        assert_eq!(result.occurrences.len(), 4);
        assert_eq!(result.occurrences[0].roles, ["definition"]);
        assert!(result.occurrences[1].roles.is_empty());
        assert_eq!(result.occurrences[2].roles, ["definition", "write"]);
        assert_eq!(
            result.occurrences[3].roles,
            [
                "definition",
                "import",
                "write",
                "read",
                "generated",
                "test",
                "forward-definition"
            ]
        );
    }

    #[test]
    fn enclosing_function_uses_syntax_not_scip_range() {
        let path = "crates/demo/src/lib.rs";
        let syntax = syntax::analyze_sources(
            &[Source::new(
                path,
                "use target::target;\nstruct Foo;\nimpl Foo {\n fn run(&self) {\n target();\n }\n}\n",
            )],
            &Default::default(),
        );
        let index = Index {
            documents: vec![document(
                path,
                vec![occurrence(0, "target().", 0), occurrence(4, "target().", 0)],
            )],
            ..Default::default()
        };
        let mut result = build_report(
            &index,
            &Query {
                symbol: Some("target".into()),
                ..Default::default()
            },
        );
        result.enclose(&syntax);
        assert_eq!(result.occurrences.len(), 2);
        assert_eq!(result.occurrences[0].enclosing_fn, None);
        assert_eq!(
            result.occurrences[1].enclosing_fn,
            Some(FnRef {
                label: "Foo::run".into(),
                line: 4
            })
        );
    }

    #[test]
    fn absent_doc_is_an_empty_answer() {
        let result = report(
            Some("missing.rs"),
            None,
            vec![document("a.rs", vec![occurrence(0, "a#", 1)])],
        );
        assert_eq!(result.doc_found, Some(false));
        assert!(result.symbols.is_empty());
        assert!(result.occurrences.is_empty());
    }

    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|arg| (*arg).into()).collect()
    }

    #[test]
    fn markdown_renders_absent_metadata_as_a_plain_dash() {
        let parsed = parse_args(&args(&["--doc", "a.rs"])).unwrap().unwrap();
        let result = report(
            Some("a.rs"),
            None,
            vec![document("a.rs", vec![occurrence(0, "a#", 0)])],
        );
        let text = render_markdown(
            &result,
            Path::new("index.scip"),
            &parsed.query,
            &parsed.output,
        );
        assert!(!text.contains("<code></code>"), "{text}");
        assert!(
            text.contains("| <code>a#</code> | - | - | - | - | 1 |"),
            "{text}"
        );
        assert!(
            text.contains("| <code>a.rs:1</code> | reference | - | <code>a#</code> |"),
            "{text}"
        );
    }

    #[test]
    fn arguments_validate_selectors_and_sections() {
        for (raw, message) in [
            (
                vec![],
                "atlas index needs --doc <path>, --symbol <name>, or both",
            ),
            (
                vec!["--symbol", "Store::open"],
                "--symbol takes one item name such as `open`; narrow with --doc <file that defines it>",
            ),
            (
                vec!["--doc", "a.rs", "--doc", "b.rs"],
                "may only be passed once",
            ),
            (
                vec!["--symbol", "open", "--section", "bogus"],
                "known: symbols, occurrences",
            ),
        ] {
            let parsed = parse_args(&args(&raw));
            assert!(parsed.is_err(), "{raw:?} must be refused");
            assert!(parsed.unwrap_err().to_string().contains(message));
        }
        assert!(parse_args(&args(&["--help"])).unwrap().is_none());
        let parsed = parse_args(&args(&["--doc", "./a.rs", "--symbol", "open", "--json"]))
            .unwrap()
            .unwrap();
        assert_eq!(parsed.query.doc.as_deref(), Some(Path::new("a.rs")));
        assert!(parsed.output.json);
    }

    #[test]
    fn index_help_dispatches_without_an_index() {
        assert!(
            super::super::atlas(Path::new("/nonexistent"), &args(&["index", "--help"])).is_ok()
        );
    }
}
