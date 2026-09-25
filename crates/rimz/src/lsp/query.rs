//! Position and symbol resolution plus compact, provider-neutral query output.

mod client;
pub use client::{Output, execute, select};

use super::protocol::{
    CallHierarchyItem, DocumentSymbol, Location, Position, Range, SymbolInformation,
};
use super::{LspErr, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Position { path: PathBuf, position: Position },
    Symbol(String),
}

pub fn parse_target(raw: &str) -> Result<Target> {
    if raw.is_empty() {
        return Err(LspErr::Protocol("empty position or symbol".into()));
    }
    let mut parts = raw.rsplitn(3, ':');
    let column = parts.next();
    let line = parts.next();
    let path = parts.next();
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let (Some(column), Some(line), Some(path)) = (column, line, path) else {
        if line.is_some() && column.is_some_and(numeric) {
            return Err(LspErr::Protocol(format!(
                "invalid position {raw}; use path:line:col with 1-based line and column"
            )));
        }
        return Ok(Target::Symbol(raw.to_owned()));
    };
    // Rust qualified symbol names contain colons too; only a numeric suffix denotes an editor position.
    if !numeric(column) && !numeric(line) {
        return Ok(Target::Symbol(raw.to_owned()));
    }
    let parse = |value: &str| {
        value
            .parse::<u32>()
            .ok()
            .and_then(|value| value.checked_sub(1))
            .ok_or_else(|| {
                LspErr::Protocol(format!(
                    "invalid position {raw}; use path:line:col with 1-based line and column"
                ))
            })
    };
    if path.is_empty() {
        return Err(LspErr::Protocol("position has no path".into()));
    }
    Ok(Target::Position {
        path: path.into(),
        position: Position {
            line: parse(line)?,
            character: parse(column)?,
        },
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verb {
    Def,
    Refs,
    Hover,
    Impl,
    Symbols,
    Find,
    Callers,
    Callees,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryErr {
    #[error("no language server for {root} ({reason}); use grep", root = root.display())]
    Unavailable {
        root: PathBuf,
        reason: UnavailableReason,
    },
    #[error("{server} still indexing after {seconds}s; use grep for this question")]
    Indexing { server: String, seconds: u64 },
    #[error(transparent)]
    Failed(#[from] LspErr),
}

impl QueryErr {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Unavailable { .. } => 3,
            Self::Indexing { .. } => 4,
            Self::Failed(_) => 1,
        }
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum UnavailableReason {
    #[error("none configured")]
    NoneConfigured,
    #[error("not running")]
    NotRunning,
    #[error("not started: memory short at launch")]
    MemoryShort,
    #[error("stopped: memory pressure")]
    MemoryPressure,
    #[error("stopped: crashed")]
    Crashed,
    #[error("stopped: checkout removed")]
    CheckoutRemoved,
    #[error("stopped by hand")]
    StoppedByHand,
    #[error("stopped: {0}")]
    Stopped(super::registry::StopReason),
}

impl UnavailableReason {
    pub fn stopped(reason: &super::registry::StopReason) -> Self {
        use super::registry::StopReason;
        match reason {
            StopReason::MemoryPressure => Self::MemoryPressure,
            StopReason::Crashed => Self::Crashed,
            StopReason::CheckoutRemoved => Self::CheckoutRemoved,
            StopReason::StoppedByHand => Self::StoppedByHand,
            reason => Self::Stopped(*reason),
        }
    }
}

pub enum SymbolResolution {
    Missing,
    Unique(Location),
    Ambiguous(Vec<SymbolInformation>),
}

pub fn resolve_symbol(name: &str, result: Value) -> Result<SymbolResolution> {
    let (container, name) = name
        .rsplit_once("::")
        .map_or((None, name), |(container, name)| (Some(container), name));
    let symbols: Vec<SymbolInformation> = if result.is_null() {
        Vec::new()
    } else {
        serde_json::from_value(result)?
    };
    let mut matches: Vec<_> = symbols
        .into_iter()
        .filter(|symbol| {
            symbol.name == name
                && container
                    .is_none_or(|container| symbol.container_name.as_deref() == Some(container))
        })
        .collect();
    Ok(match matches.len() {
        0 => SymbolResolution::Missing,
        1 => SymbolResolution::Unique(matches.remove(0).location),
        _ => SymbolResolution::Ambiguous(matches),
    })
}

pub fn file_path(uri: &str) -> Result<PathBuf> {
    url::Url::parse(uri)
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .ok_or_else(|| LspErr::Protocol(format!("not a file URI: {uri}")))
}

fn displayed_path(root: &Path, uri: &str) -> Result<String> {
    let path = file_path(uri)?;
    Ok(path
        .strip_prefix(root)
        .unwrap_or(&path)
        .display()
        .to_string())
}

fn position_text(root: &Path, uri: &str, position: Position) -> Result<String> {
    Ok(format!(
        "{}:{}:{}",
        displayed_path(root, uri)?,
        u64::from(position.line) + 1,
        u64::from(position.character) + 1
    ))
}

fn kind_name(kind: u32) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum-member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type-parameter",
        _ => "symbol",
    }
}

fn render_find(root: &Path, symbols: &[SymbolInformation]) -> Result<String> {
    let mut lines = Vec::new();
    for symbol in symbols {
        let container = symbol
            .container_name
            .as_ref()
            .map_or_else(String::new, |name| format!(" in {name}"));
        lines.push(format!(
            "{} {}  {}{container}",
            kind_name(symbol.kind),
            symbol.name,
            position_text(root, &symbol.location.uri, symbol.location.range.start)?
        ));
    }
    Ok(finish(lines))
}

pub fn render_ambiguous(root: &Path, name: &str, symbols: &[SymbolInformation]) -> Result<String> {
    Ok(format!(
        "ambiguous: {} symbols named {name}; rerun with a position\n{}",
        symbols.len(),
        render_find(root, symbols)?
    ))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LocationResult {
    Location(Location),
    Link {
        #[serde(rename = "targetUri")]
        uri: String,
        #[serde(rename = "targetSelectionRange")]
        range: Range,
    },
}

fn locations(result: Value) -> Result<Vec<Location>> {
    let values = match result {
        Value::Null => return Ok(Vec::new()),
        Value::Array(values) => values,
        value => vec![value],
    };
    values
        .into_iter()
        .map(|value| {
            Ok(match serde_json::from_value::<LocationResult>(value)? {
                LocationResult::Location(location) => location,
                LocationResult::Link { uri, range } => Location { uri, range },
            })
        })
        .collect()
}

fn finish(lines: Vec<String>) -> String {
    if lines.is_empty() {
        "no results\n".into()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HoverContents {
    Text(String),
    Code { language: String, value: String },
    Markup { value: String },
    Array(Vec<HoverContents>),
}

impl HoverContents {
    fn text(self) -> String {
        match self {
            Self::Text(text) | Self::Markup { value: text } => text,
            Self::Code { language, value } => format!("```{language}\n{value}\n```"),
            Self::Array(contents) => contents
                .into_iter()
                .map(Self::text)
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }
}

/// `document_uri` is supplied for document-symbol trees, which omit their own URI.
pub fn render(
    verb: Verb,
    root: &Path,
    document_uri: Option<&str>,
    result: Value,
) -> Result<String> {
    render_with_source(verb, root, document_uri, result, |uri, line| {
        let text = std::fs::read_to_string(file_path(uri)?)?;
        text.lines()
            .nth(line as usize)
            .map(str::to_owned)
            .ok_or_else(|| LspErr::Protocol("location is past the end of the file".into()))
    })
}

fn render_with_source(
    verb: Verb,
    root: &Path,
    document_uri: Option<&str>,
    result: Value,
    mut source: impl FnMut(&str, u32) -> Result<String>,
) -> Result<String> {
    if result.is_null() {
        return Ok("no results\n".into());
    }
    match verb {
        Verb::Def | Verb::Refs | Verb::Impl => {
            let mut sorted = BTreeMap::new();
            for location in locations(result)? {
                let position = location.range.start;
                sorted.insert((displayed_path(root, &location.uri)?, position), location);
            }
            let mut lines = Vec::new();
            for location in sorted.into_values() {
                let mut line = position_text(root, &location.uri, location.range.start)?;
                if let Ok(text) = source(&location.uri, location.range.start.line) {
                    line.push_str("  ");
                    line.push_str(text.trim());
                }
                lines.push(line);
            }
            Ok(finish(lines))
        }
        Verb::Hover => {
            #[derive(Deserialize)]
            struct Hover {
                contents: HoverContents,
            }
            let hover: Hover = serde_json::from_value(result)?;
            Ok(format!("{}\n", hover.contents.text().trim_end()))
        }
        Verb::Find => render_find(
            root,
            &serde_json::from_value::<Vec<SymbolInformation>>(result)?,
        ),
        Verb::Symbols => {
            #[derive(Deserialize)]
            #[serde(untagged)]
            enum Symbols {
                Flat(Vec<SymbolInformation>),
                Tree(Vec<DocumentSymbol>),
            }
            match serde_json::from_value(result)? {
                Symbols::Flat(symbols) => render_find(root, &symbols),
                Symbols::Tree(symbols) => {
                    let uri = document_uri.ok_or_else(|| {
                        LspErr::Protocol("document symbols need a file URI".into())
                    })?;
                    let mut lines = Vec::new();
                    let mut pending: Vec<_> =
                        symbols.iter().rev().map(|symbol| (symbol, 0)).collect();
                    while let Some((symbol, depth)) = pending.pop() {
                        lines.push(format!(
                            "{}{} {}  {}",
                            "  ".repeat(depth),
                            kind_name(symbol.kind),
                            symbol.name,
                            position_text(root, uri, symbol.selection_range.start)?
                        ));
                        pending
                            .extend(symbol.children.iter().rev().map(|child| (child, depth + 1)));
                    }
                    Ok(finish(lines))
                }
            }
        }
        Verb::Callers | Verb::Callees => {
            #[derive(Deserialize)]
            struct Incoming {
                from: CallHierarchyItem,
            }
            #[derive(Deserialize)]
            struct Outgoing {
                to: CallHierarchyItem,
            }
            let items: Vec<CallHierarchyItem> = if verb == Verb::Callers {
                serde_json::from_value::<Vec<Incoming>>(result)?
                    .into_iter()
                    .map(|call| call.from)
                    .collect()
            } else {
                serde_json::from_value::<Vec<Outgoing>>(result)?
                    .into_iter()
                    .map(|call| call.to)
                    .collect()
            };
            let mut sorted = BTreeMap::new();
            for item in items {
                sorted.insert(
                    (
                        displayed_path(root, &item.uri)?,
                        item.selection_range.start,
                        item.name.clone(),
                    ),
                    item,
                );
            }
            Ok(finish(
                sorted
                    .into_values()
                    .map(|item| {
                        Ok(format!(
                            "{}  {}",
                            item.name,
                            position_text(root, &item.uri, item.selection_range.start)?
                        ))
                    })
                    .collect::<Result<_>>()?,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn range() -> Value {
        json!({"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 6}})
    }

    #[test]
    fn configured_but_absent_server_has_neutral_reason() {
        let config = serde_json::from_value(serde_json::json!({"command": ["server"], "extensions": ["rs"], "root-markers": ["Cargo.toml"]})).unwrap();
        let error = select(
            Path::new("/no-refusal-record"),
            Vec::new(),
            &BTreeMap::from([("rust".into(), config)]),
            Some("rust"),
            None,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "no language server for /no-refusal-record (not running); use grep"
        );
    }

    #[test]
    fn missing_source_does_not_discard_locations() {
        let result = render_with_source(
            Verb::Refs,
            Path::new("/checkout"),
            None,
            json!([{"uri": "file:///checkout/gone.rs", "range": range()}]),
            |_, _| Err(LspErr::Protocol("missing line".into())),
        );
        assert_eq!(result.unwrap(), "gone.rs:2:3\n");
    }

    #[test]
    fn qualified_symbols_match_their_container() {
        let symbol = |container| json!({"name": "method", "containerName": container, "kind": 12, "location": {"uri": "file:///checkout/lib.rs", "range": range()}});
        assert!(matches!(
            resolve_symbol("Type::method", json!([symbol("Type"), symbol("Other")])).unwrap(),
            SymbolResolution::Unique(_)
        ));
    }

    #[test]
    fn incomplete_position_requires_a_column() {
        assert!(
            parse_target("src/lib.rs:12")
                .unwrap_err()
                .to_string()
                .contains("path:line:col")
        );
    }

    #[test]
    fn verb_text_renderers() {
        let root = Path::new("/checkout");
        let uri = "file:///checkout/src/lib.rs";
        let location = json!({"uri": uri, "range": range()});
        let show = |verb, result| {
            render_with_source(verb, root, Some(uri), result, |_, _| {
                Ok("  fn work() {}  ".into())
            })
            .unwrap()
        };
        insta::assert_snapshot!(show(Verb::Def, location.clone()), @"src/lib.rs:2:3  fn work() {}");
        insta::assert_snapshot!(show(Verb::Refs, json!([location, location])), @"src/lib.rs:2:3  fn work() {}");
        insta::assert_snapshot!(show(Verb::Impl, json!([{"targetUri": uri, "targetSelectionRange": range()}])), @"src/lib.rs:2:3  fn work() {}");
        insta::assert_snapshot!(show(Verb::Hover, json!({"contents": [{"language": "rust", "value": "fn work()"}, "Does work."]})), @"
        ```rust
        fn work()
        ```

        Does work.
        ");
        let child =
            json!({"name": "work", "kind": 12, "range": range(), "selectionRange": range()});
        insta::assert_snapshot!(show(Verb::Symbols, json!([{"name": "Engine", "kind": 23, "range": range(), "selectionRange": range(), "children": [child]}])), @"
        struct Engine  src/lib.rs:2:3
          function work  src/lib.rs:2:3
        ");
        let symbol =
            json!({"name": "work", "kind": 12, "location": location, "containerName": "Engine"});
        insta::assert_snapshot!(show(Verb::Find, json!([symbol])), @"function work  src/lib.rs:2:3 in Engine");
        let item = json!({"name": "work", "kind": 12, "uri": uri, "range": range(), "selectionRange": range()});
        insta::assert_snapshot!(show(Verb::Callers, json!([{"from": item}, {"from": item}])), @"work  src/lib.rs:2:3");
        insta::assert_snapshot!(show(Verb::Callees, json!([{"to": item}])), @"work  src/lib.rs:2:3");
        assert_eq!(show(Verb::Refs, json!([])), "no results\n");
        assert_eq!(
            show(
                Verb::Hover,
                json!({"contents": {"kind": "markdown", "value": "**work**"}})
            ),
            "**work**\n"
        );
        assert_eq!(
            show(Verb::Symbols, json!([symbol])),
            show(Verb::Find, json!([symbol]))
        );
    }

    #[test]
    fn symbols_require_exact_names_and_ambiguity_is_not_a_guess() {
        let symbol = |name| json!({"name": name, "kind": 12, "location": {"uri": "file:///checkout/lib.rs", "range": range()}});
        assert!(matches!(
            resolve_symbol("work", json!([symbol("worker")])).unwrap(),
            SymbolResolution::Missing
        ));
        assert!(matches!(
            resolve_symbol("work", json!([symbol("worker"), symbol("work")])).unwrap(),
            SymbolResolution::Unique(_)
        ));
        let SymbolResolution::Ambiguous(symbols) =
            resolve_symbol("work", json!([symbol("work"), symbol("work")])).unwrap()
        else {
            panic!("ambiguous symbol");
        };
        insta::assert_snapshot!(render_ambiguous(Path::new("/checkout"), "work", &symbols).unwrap(), @"
        ambiguous: 2 symbols named work; rerun with a position
        function work  lib.rs:2:3
        function work  lib.rs:2:3
        ");
    }

    #[test]
    fn unavailable_and_indexing_errors_preserve_skill_exit_contract() {
        let reasons = [
            UnavailableReason::NoneConfigured,
            UnavailableReason::MemoryShort,
            UnavailableReason::MemoryPressure,
            UnavailableReason::Crashed,
            UnavailableReason::CheckoutRemoved,
            UnavailableReason::StoppedByHand,
        ];
        let lines = reasons
            .into_iter()
            .map(|reason| {
                let error = QueryErr::Unavailable {
                    root: "/checkout".into(),
                    reason,
                };
                assert_eq!(error.exit_code(), 3);
                error.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(lines, @"
        no language server for /checkout (none configured); use grep
        no language server for /checkout (not started: memory short at launch); use grep
        no language server for /checkout (stopped: memory pressure); use grep
        no language server for /checkout (stopped: crashed); use grep
        no language server for /checkout (stopped: checkout removed); use grep
        no language server for /checkout (stopped by hand); use grep
        ");
        let indexing = QueryErr::Indexing {
            server: "rust".into(),
            seconds: 47,
        };
        assert_eq!(indexing.exit_code(), 4);
        insta::assert_snapshot!(indexing.to_string(), @"rust still indexing after 47s; use grep for this question");
    }

    #[test]
    fn editor_positions_are_one_based_and_symbols_are_not_guessed() {
        assert_eq!(
            parse_target("src/lib.rs:2:3").unwrap(),
            Target::Position {
                path: "src/lib.rs".into(),
                position: Position {
                    line: 1,
                    character: 2
                },
            }
        );
        assert_eq!(
            parse_target("MuxBackend").unwrap(),
            Target::Symbol("MuxBackend".into())
        );
        assert!(parse_target("src/lib.rs:0:1").is_err());
        assert_eq!(
            parse_target("Type::method").unwrap(),
            Target::Symbol("Type::method".into())
        );
    }
}
