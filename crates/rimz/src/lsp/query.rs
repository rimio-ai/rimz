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
    #[error("not started: memory short")]
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
    Missing { candidates: Vec<SymbolInformation> },
    Unique(SymbolInformation),
    Ambiguous(Vec<SymbolInformation>),
}

fn collapse_symbols(
    symbols: Vec<SymbolInformation>,
    mut definition: impl FnMut(&Location) -> std::result::Result<Vec<Location>, QueryErr>,
) -> std::result::Result<SymbolResolution, QueryErr> {
    let mut groups = BTreeMap::new();
    for symbol in symbols {
        let resolved = <[Location; 1]>::try_from(definition(&symbol.location)?)
            .map(|[resolved]| resolved)
            .unwrap_or_else(|_| symbol.location.clone());
        groups
            .entry((resolved.uri.clone(), resolved.range.start))
            .and_modify(|(indexed, _): &mut (SymbolInformation, Location)| {
                if symbol.location.uri == resolved.uri
                    && symbol.location.range.start == resolved.range.start
                {
                    *indexed = symbol.clone();
                }
            })
            .or_insert((symbol, resolved));
    }
    let mut symbols: Vec<_> = groups.into_values().collect();
    Ok(if symbols.len() == 1 {
        let (mut symbol, definition) = symbols.remove(0);
        symbol.location = definition;
        SymbolResolution::Unique(symbol)
    } else {
        SymbolResolution::Ambiguous(symbols.into_iter().map(|(symbol, _)| symbol).collect())
    })
}

fn without_generics(raw: &str) -> String {
    let mut depth = 0_u32;
    raw.chars()
        .filter(|character| match character {
            '<' => {
                depth += 1;
                false
            }
            '>' => {
                depth = depth.saturating_sub(1);
                false
            }
            _ => depth == 0,
        })
        .collect()
}

fn name_segments(raw: &str) -> Vec<String> {
    without_generics(raw.strip_suffix("()").unwrap_or(raw))
        .split("::")
        .skip_while(|segment| matches!(*segment, "crate" | "self" | "super"))
        .map(str::to_owned)
        .collect()
}

fn module_path(root: &Path, uri: &str) -> Result<Vec<String>> {
    let file = file_path(uri)?;
    let Ok(relative) = file.strip_prefix(root) else {
        return Ok(Vec::new());
    };
    let mut path: Vec<_> = relative
        .with_extension("")
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if path
        .last()
        .is_some_and(|stem| matches!(stem.as_str(), "mod" | "lib" | "main"))
    {
        path.pop();
    }
    Ok(path)
}

fn container_path(symbol: &SymbolInformation) -> Vec<String> {
    symbol
        .container_name
        .as_deref()
        .map_or_else(Vec::new, |container| {
            without_generics(container)
                .split("::")
                .map(str::to_owned)
                .collect()
        })
}

fn symbol_path(root: &Path, symbol: &SymbolInformation) -> Result<Vec<String>> {
    Ok(module_path(root, &symbol.location.uri)?
        .into_iter()
        .filter(|segment| segment != "src")
        .chain(container_path(symbol))
        .collect())
}

pub fn resolve_symbol(root: &Path, name: &str, result: Value) -> Result<SymbolResolution> {
    let mut qualifier = name_segments(name);
    let name = qualifier.pop().unwrap_or_default();
    let symbols: Vec<SymbolInformation> = if result.is_null() {
        Vec::new()
    } else {
        serde_json::from_value(result)?
    };
    let candidates: Vec<_> = symbols
        .into_iter()
        .filter(|symbol| symbol.name == name)
        .collect();
    let mut matches = Vec::new();
    for symbol in &candidates {
        let path = symbol_path(root, symbol)?;
        let mut segments = path.iter();
        if qualifier
            .iter()
            .all(|wanted| segments.any(|segment| segment == wanted))
        {
            matches.push(symbol.clone());
        }
    }
    Ok(match matches.len() {
        0 => SymbolResolution::Missing { candidates },
        1 => SymbolResolution::Unique(matches.remove(0)),
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

#[derive(Serialize)]
struct Candidate {
    name: String,
    kind: &'static str,
    position: String,
}

impl Candidate {
    fn new(root: &Path, symbol: &SymbolInformation) -> Result<Self> {
        let path = module_path(root, &symbol.location.uri)?;
        let start = path
            .iter()
            .position(|segment| segment == "src")
            .map_or(0, |index| index + 1);
        let name = path
            .into_iter()
            .skip(start)
            .filter(|segment| segment != "src")
            .chain(container_path(symbol))
            .chain([symbol.name.clone()])
            .collect::<Vec<_>>()
            .join("::");
        Ok(Self {
            name,
            kind: kind_name(symbol.kind),
            position: position_text(root, &symbol.location.uri, symbol.location.range.start)?,
        })
    }

    fn line(&self) -> String {
        format!("{} {}  {}", self.kind, self.name, self.position)
    }
}

fn render_find(root: &Path, symbols: &[SymbolInformation]) -> Result<String> {
    Ok(finish(
        symbols
            .iter()
            .map(|symbol| Ok(Candidate::new(root, symbol)?.line()))
            .collect::<Result<_>>()?,
    ))
}

fn rank_find(root: &Path, query: &str, result: Value) -> Result<Value> {
    if result.is_null() {
        return Ok(result);
    }
    let query = query.to_lowercase();
    let values: Vec<Value> = serde_json::from_value(result)?;
    let mut ranked = Vec::new();
    for value in values {
        let symbol: SymbolInformation = serde_json::from_value(value.clone())?;
        let candidate = Candidate::new(root, &symbol)?;
        let name = symbol.name.to_lowercase();
        let rank = if name == query {
            0
        } else if name.starts_with(&query) {
            1
        } else if name.contains(&query) {
            2
        } else {
            3
        };
        ranked.push((rank, candidate.name, candidate.position, value));
    }
    ranked.sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
    Ok(Value::Array(
        ranked.into_iter().map(|(_, _, _, value)| value).collect(),
    ))
}

pub fn render_outcome(root: &Path, output: &Output, json: bool) -> Result<String> {
    let (Output::Ambiguous { name, symbols } | Output::NotFound { name, symbols }) = output else {
        return Err(LspErr::Protocol(
            "render lookup candidates only for not-found or ambiguous outcomes".into(),
        ));
    };
    let mut qualifier = name_segments(name);
    let last = qualifier.pop().unwrap_or_default();
    let mut candidates = Vec::new();
    for symbol in symbols {
        let path = symbol_path(root, symbol)?;
        let score = qualifier
            .iter()
            .filter(|segment| path.contains(segment))
            .count();
        candidates.push((std::cmp::Reverse(score), Candidate::new(root, symbol)?));
    }
    candidates
        .sort_by(|a, b| (&a.0, &a.1.name, &a.1.position).cmp(&(&b.0, &b.1.name, &b.1.position)));
    let candidates: Vec<_> = candidates
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect();
    let missing = matches!(output, Output::NotFound { .. });
    if json {
        return Ok(format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "outcome": if missing { "not-found" } else { "ambiguous" },
                "name": name, "candidates": candidates,
            }))?
        ));
    }
    let count = candidates.len();
    let header = if missing {
        if count == 0 {
            format!("not found: {name}")
        } else {
            format!(
                "not found: {name}; {count} {} named {last}:",
                if count == 1 { "symbol" } else { "symbols" }
            )
        }
    } else {
        format!(
            "ambiguous: {count} symbols named {name}; rerun with one of these names or a position"
        )
    };
    let mut lines = vec![header];
    lines.extend(candidates.iter().take(20).map(Candidate::line));
    if count > 20 {
        lines.push(format!(
            "{} more; narrow with a qualifier or use find",
            count - 20
        ));
    }
    Ok(finish(lines))
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Checkout,
    External,
}

/// `document_uri` is supplied for document-symbol trees, which omit their own URI.
pub fn render(
    verb: Verb,
    root: &Path,
    document_uri: Option<&str>,
    result: Value,
    scope: Scope,
) -> Result<String> {
    render_with_source(verb, root, document_uri, result, scope, |uri, line| {
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
    scope: Scope,
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
            let hidden = if scope == Scope::Checkout {
                let total = sorted.len();
                sorted.retain(|(path, _, _), _| !Path::new(path).is_absolute());
                total - sorted.len()
            } else {
                0
            };
            let mut output = finish(
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
            );
            if hidden > 0 {
                output.push_str(&format!(
                    "{hidden} outside the checkout hidden; add --external to show them\n"
                ));
            }
            Ok(output)
        }
    }
}

#[cfg(test)]
mod tests;
