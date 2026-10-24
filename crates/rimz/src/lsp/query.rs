//! Position and symbol resolution plus compact, provider-neutral query output.

mod client;
pub use client::{Output, classification_outline, execute, select};

use super::protocol::{
    CallHierarchyItem, DocumentSymbol, Location, Position, Range, SymbolInformation, Symbols,
};
use super::{LspErr, NamedItem, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Position { path: PathBuf, position: Position },
    Symbol(String),
    File(PathBuf),
    Find(String),
}

impl Target {
    pub fn parse(verb: Verb, raw: &str) -> Result<Self> {
        match verb {
            Verb::Symbols => return Ok(Self::File(raw.into())),
            Verb::Find => return Ok(Self::Find(raw.into())),
            _ => {}
        }
        if raw.is_empty() {
            return Err(LspErr::Protocol("empty position or symbol".into()));
        }
        let mut parts = raw.rsplitn(3, ':');
        let column = parts.next();
        let line = parts.next();
        let path = parts.next();
        let numeric =
            |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        let (Some(column), Some(line), Some(path)) = (column, line, path) else {
            if line.is_some() && column.is_some_and(numeric) {
                if let Some(path) = line.filter(|path| !path.is_empty())
                    && let Some(line) = column.and_then(|line| line.parse::<u32>().ok())
                    && line > 0
                {
                    return Err(LspErr::MissingColumn {
                        path: path.into(),
                        line,
                        item: None,
                    });
                }
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

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Position { path, .. } | Self::File(path) => Some(path),
            Self::Symbol(name) => file_qualified(name).map(|(file, _)| Path::new(file)),
            Self::Find(_) => None,
        }
    }
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

// 600s covers about 99% of measured Rust starts (p99 481s); rerun below that ceiling.
const INDEXING_RERUN_CEILING_SECS: u64 = 600;

#[derive(Debug, thiserror::Error)]
pub enum QueryErr {
    #[error("language server {server} for {root} stopped: memory pressure; use grep for this question and retry on the next", root = root.display())]
    MemoryPressure { root: PathBuf, server: String },
    #[error("language server {server} for {root} not started: memory short{figures}; use grep for this question and retry on the next", root = root.display(), figures = shortfall.as_ref().map(|shortfall| format!(", needs {}, {}", crate::utils::size::decimal_bytes(shortfall.estimate_bytes), shortfall.headroom())).unwrap_or_default())]
    MemoryShort {
        root: PathBuf,
        server: String,
        shortfall: Option<Box<super::admission::Shortfall>>,
    },
    #[error("no language server for {root} ({reason}); use grep", root = root.display())]
    Unavailable {
        root: PathBuf,
        reason: UnavailableReason,
    },
    #[error(
        "{server} still indexing after {seconds}s; {next}",
        next = if *seconds < INDEXING_RERUN_CEILING_SECS {
            "rerun to keep waiting"
        } else {
            "use grep for this question and retry on the next"
        }
    )]
    Indexing { server: String, seconds: u64 },
    #[error(
        "no language server handles .{extension} files ({coverage}); use grep for this target",
        coverage = coverage.iter().map(|(name, extensions)| {
            let extensions = extensions.iter().map(|ext| format!(".{ext}"))
                .collect::<Vec<_>>().join(", ");
            format!("{name} covers {extensions}")
        }).collect::<Vec<_>>().join("; ")
    )]
    UncoveredExtension {
        extension: String,
        coverage: BTreeMap<String, Vec<String>>,
    },
    #[error(
        "multiple language servers for {root}: {names}; choose --server NAME",
        root = root.display(),
        names = servers.iter().map(String::as_str).collect::<Vec<_>>().join(", ")
    )]
    MultipleServers {
        root: PathBuf,
        servers: BTreeSet<String>,
    },
    #[error(transparent)]
    Failed(#[from] LspErr),
}

impl QueryErr {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Unavailable { .. } => 3,
            Self::Indexing { .. } | Self::MemoryShort { .. } | Self::MemoryPressure { .. } => 4,
            Self::UncoveredExtension { .. } | Self::MultipleServers { .. } | Self::Failed(_) => 1,
        }
    }

    pub fn server_error(&self) -> Option<Self> {
        Some(match self {
            Self::Unavailable { root, reason } => Self::Unavailable {
                root: root.clone(),
                reason: reason.clone(),
            },
            Self::Indexing { server, seconds } => Self::Indexing {
                server: server.clone(),
                seconds: *seconds,
            },
            Self::MemoryShort {
                root,
                server,
                shortfall,
            } => Self::MemoryShort {
                root: root.clone(),
                server: server.clone(),
                shortfall: shortfall.clone(),
            },
            Self::MemoryPressure { root, server } => Self::MemoryPressure {
                root: root.clone(),
                server: server.clone(),
            },
            Self::UncoveredExtension { .. } | Self::MultipleServers { .. } | Self::Failed(_) => {
                return None;
            }
        })
    }

    fn stopped(entry: &super::registry::Entry, reason: &super::registry::StopReason) -> Self {
        use super::registry::StopReason;
        if *reason == StopReason::MemoryPressure {
            return Self::MemoryPressure {
                root: entry.root.clone(),
                server: entry.server.clone(),
            };
        }
        Self::Unavailable {
            root: entry.root.clone(),
            reason: match reason {
                StopReason::Crashed => UnavailableReason::Crashed,
                StopReason::CheckoutRemoved => UnavailableReason::CheckoutRemoved,
                StopReason::StoppedByHand => UnavailableReason::StoppedByHand,
                reason => UnavailableReason::Stopped(*reason),
            },
        }
    }
}

pub fn missing_column(root: &Path, path: &Path, line: u32, outline: Option<Value>) -> QueryErr {
    let item = outline
        .and_then(|value| serde_json::from_value::<Symbols>(value).ok())
        .and_then(|outline| match outline {
            Symbols::Tree(symbols) => {
                let mut pending: Vec<_> = symbols.into_iter().rev().collect();
                while let Some(symbol) = pending.pop() {
                    if u64::from(symbol.selection_range.start.line) + 1 == u64::from(line) {
                        return Some(NamedItem {
                            kind: kind_name(symbol.kind),
                            name: symbol.name,
                            column: u64::from(symbol.selection_range.start.character) + 1,
                        });
                    }
                    pending.extend(symbol.children.into_iter().rev());
                }
                None
            }
            Symbols::Flat(symbols) => {
                let symbol = symbols.iter().find(|symbol| {
                    u64::from(symbol.location.range.start.line) + 1 == u64::from(line)
                })?;
                let candidate = Candidate::new(root, symbol).ok()?;
                Some(NamedItem {
                    kind: candidate.kind,
                    name: candidate.name,
                    column: u64::from(symbol.location.range.start.character) + 1,
                })
            }
        });
    LspErr::MissingColumn {
        path: path.into(),
        line,
        item,
    }
    .into()
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum UnavailableReason {
    #[error("none configured")]
    NoneConfigured,
    #[error("not running")]
    NotRunning,
    #[error("stopped: crashed; see rimz lsp status")]
    Crashed,
    #[error("stopped: checkout removed")]
    CheckoutRemoved,
    #[error("stopped by hand")]
    StoppedByHand,
    #[error("stopped: {0}")]
    Stopped(super::registry::StopReason),
}

enum SymbolResolution {
    Missing { candidates: Vec<SymbolInformation> },
    Unique(SymbolInformation),
    Ambiguous(Vec<SymbolInformation>),
}

impl SymbolResolution {
    fn into_symbols(self) -> Vec<SymbolInformation> {
        match self {
            Self::Unique(symbol) => vec![symbol],
            Self::Ambiguous(symbols)
            | Self::Missing {
                candidates: symbols,
            } => symbols,
        }
    }
}

fn prefer_exact(
    root: &Path,
    name: &str,
    symbols: Vec<SymbolInformation>,
) -> Result<Vec<SymbolInformation>> {
    let (file, chain) =
        file_qualified(name).map_or((None, name), |(file, chain)| (Some(file), chain));
    let segments = name_segments(chain);
    let container = segments.iter().rev().nth(1);
    if container.is_none() && file.is_none() {
        return Ok(symbols);
    }
    let mut exact = Vec::new();
    let mut skipped = Vec::new();
    for symbol in symbols {
        let direct = match container {
            Some(container) => symbol_path(root, &symbol)?.last() == Some(container),
            None => container_path(&symbol).is_empty(),
        };
        if direct {
            exact.push(symbol);
        } else {
            skipped.push(symbol);
        }
    }
    Ok(if exact.is_empty() { skipped } else { exact })
}

fn enrich_containers(
    symbols: &mut [SymbolInformation],
    outlines: &BTreeMap<String, Symbols>,
) -> bool {
    let containers: Option<Vec<_>> = symbols
        .iter()
        .map(|symbol| {
            let (_, ancestors) = outline_node(symbol, outlines.get(&symbol.location.uri)?)?;
            let path: Vec<_> = ancestors
                .into_iter()
                .map(|name| {
                    without_generics(name)
                        .split_whitespace()
                        .next_back()
                        .unwrap_or("")
                        .to_owned()
                })
                .collect();
            Some((!path.is_empty()).then(|| path.join("::")))
        })
        .collect();
    let Some(containers) = containers else {
        return false;
    };
    for (symbol, container) in symbols.iter_mut().zip(containers) {
        symbol.container_name = container;
    }
    true
}

fn narrow_candidates(
    root: &Path,
    name: &str,
    candidates: &[SymbolInformation],
) -> Result<Vec<SymbolInformation>> {
    let (file, chain) =
        file_qualified(name).map_or((None, name), |(file, chain)| (Some(file), chain));
    let mut qualifier = name_segments(chain);
    let member = qualifier.pop().unwrap_or_default();
    if let Some(file) = file {
        return Ok(
            match match_symbols(root, &format!("{file}::{member}"), candidates.to_vec())? {
                SymbolResolution::Missing { .. } => Vec::new(),
                found => found.into_symbols(),
            },
        );
    }
    let mut narrowed = Vec::new();
    for mut symbol in candidates.iter().cloned() {
        symbol.container_name = None;
        let path = symbol_path(root, &symbol)?;
        for length in (1..=qualifier.len()).rev() {
            if path.last() != qualifier.get(length - 1) {
                continue;
            }
            let target = format!("{}::{member}", qualifier[..length].join("::"));
            if !matches!(
                match_symbols(root, &target, vec![symbol.clone()])?,
                SymbolResolution::Missing { .. }
            ) {
                narrowed.push(symbol);
                break;
            }
        }
    }
    Ok(narrowed)
}

fn rematch_enriched(
    root: &Path,
    name: &str,
    candidates: &[SymbolInformation],
    outlines: &BTreeMap<String, Symbols>,
) -> Result<Option<SymbolResolution>> {
    let mut symbols = candidates.to_vec();
    if !enrich_containers(&mut symbols, outlines) {
        return Ok(None);
    }
    let matches = match match_symbols(root, name, symbols)? {
        SymbolResolution::Missing { .. } => return Ok(None),
        found => found.into_symbols(),
    };
    let mut matches = prefer_exact(root, name, matches)?;
    Ok(Some(if matches.len() == 1 {
        SymbolResolution::Unique(matches.remove(0))
    } else {
        SymbolResolution::Ambiguous(matches)
    }))
}

fn outline_node<'a>(
    symbol: &SymbolInformation,
    outline: &'a Symbols,
) -> Option<(&'a DocumentSymbol, Vec<&'a str>)> {
    let Symbols::Tree(nodes) = outline else {
        return None;
    };
    let mut all = Vec::new();
    let mut pending: Vec<_> = nodes.iter().map(|node| (node, Vec::new())).collect();
    while let Some((node, ancestors)) = pending.pop() {
        let mut path = ancestors.clone();
        path.push(node.name.as_str());
        pending.extend(node.children.iter().map(|child| (child, path.clone())));
        all.push((node, ancestors));
    }
    // rust-analyzer's `workspace/symbol` range is the item's name, so its
    // start is the outline node's selection start. ty's range is the whole
    // declaration: the node is then the first same-named one whose name falls
    // inside it, which is the declaration's own name, not a nested namesake's.
    let range = symbol.location.range;
    all.iter()
        .find(|(node, _)| node.selection_range.start == range.start)
        .or_else(|| {
            all.iter()
                .filter(|(node, _)| {
                    node.name == symbol.name
                        && range.start <= node.selection_range.start
                        && node.selection_range.start < range.end
                })
                .min_by_key(|(node, _)| node.selection_range.start)
        })
        .map(|(node, ancestors)| (*node, ancestors.clone()))
}

fn outline_members(
    container: &SymbolInformation,
    name: &str,
    outline: &Symbols,
) -> Vec<SymbolInformation> {
    let Some((node, _)) = outline_node(container, outline) else {
        return Vec::new();
    };
    let mut path = container_path(container);
    path.push(container.name.clone());
    let container_name = path.join("::");
    node.children
        .iter()
        .filter(|child| child.name == name)
        .map(|child| SymbolInformation {
            name: child.name.clone(),
            kind: child.kind,
            location: Location {
                uri: container.location.uri.clone(),
                range: child.selection_range,
            },
            container_name: Some(container_name.clone()),
        })
        .collect()
}

fn collapse_symbols(
    symbols: Vec<SymbolInformation>,
    definition: impl FnOnce(&[SymbolInformation]) -> std::result::Result<Vec<Vec<Location>>, QueryErr>,
) -> std::result::Result<SymbolResolution, QueryErr> {
    let definitions = definition(&symbols)?;
    let mut groups = BTreeMap::new();
    for (symbol, definition) in symbols.into_iter().zip(definitions) {
        let resolved = <[Location; 1]>::try_from(definition)
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

fn collapse_candidates(
    symbols: Vec<SymbolInformation>,
    definition: impl FnOnce(&[SymbolInformation]) -> std::result::Result<Vec<Vec<Location>>, QueryErr>,
) -> std::result::Result<Vec<SymbolInformation>, QueryErr> {
    Ok(collapse_symbols(symbols, definition)?.into_symbols())
}

// Resolve a fixed head, leaving room for aliases beyond the 20 displayed candidates.
const COLLAPSE_CAP: usize = 30;

fn collapse_ranked(
    root: &Path,
    name: &str,
    symbols: Vec<SymbolInformation>,
    definition: impl FnOnce(&[SymbolInformation]) -> std::result::Result<Vec<Vec<Location>>, QueryErr>,
) -> std::result::Result<(SymbolResolution, usize), QueryErr> {
    let ranked = rank_candidates(root, name, &symbols)?;
    let unresolved = symbols.len().saturating_sub(COLLAPSE_CAP);
    let indices: Vec<_> = ranked
        .into_iter()
        .take(COLLAPSE_CAP)
        .map(|(index, _)| index)
        .collect();
    let head: Vec<_> = indices
        .iter()
        .map(|&index| symbols[index].clone())
        .collect();
    let definitions = definition(&head)?;
    // Request in rank order, but retain the original indexed alias preference within each group.
    let mut resolved: Vec<_> = indices
        .into_iter()
        .zip(head.into_iter().zip(definitions))
        .collect();
    resolved.sort_by_key(|(index, _)| *index);
    let (head, definitions): (Vec<_>, Vec<_>) =
        resolved.into_iter().map(|(_, result)| result).unzip();
    let mut symbols = collapse_candidates(head, |_| Ok(definitions))?;
    let resolution = if symbols.len() == 1 && unresolved == 0 {
        SymbolResolution::Unique(symbols.remove(0))
    } else {
        SymbolResolution::Ambiguous(symbols)
    };
    Ok((resolution, unresolved))
}

pub(super) fn without_generics(raw: &str) -> String {
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

/// Whether an anchor's head names a file: no whitespace, an extension of ASCII alphanumerics
/// with at least one letter, and no `/`-separated component that holds `..` beside a character
/// other than a dot. Struct-update and range text such as `..Default` or `a..b` is not a file,
/// while `..` and an abbreviating `...` stay path components.
pub(super) fn is_file_head(path: &str) -> bool {
    let Some(extension) = Path::new(path).extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    !path.chars().any(char::is_whitespace)
        && path.split('/').all(|component| {
            !component.contains("..") || component.bytes().all(|byte| byte == b'.')
        })
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && extension.bytes().any(|byte| byte.is_ascii_alphabetic())
}

/// Splits a `path::Name` target at its first `::` when the head names a file.
fn file_qualified(raw: &str) -> Option<(&str, &str)> {
    let (file, name) = raw.split_once("::")?;
    is_file_head(file).then_some((file, name))
}

/// An anchor path made checkout-relative with `.` components dropped; `None` when it leaves the
/// checkout.
pub(super) fn checkout_relative(root: &Path, path: &Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path.strip_prefix(root).ok()?
    } else {
        path
    };
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(
        path.components()
            .filter(|part| !matches!(part, std::path::Component::CurDir))
            .collect(),
    )
}

/// The checkout files a file-qualified target's path names, as `check` resolves an anchor: the
/// exact file when it exists, else every file the path is a component-wise suffix of.
enum NamedFiles {
    Exact(PathBuf),
    Suffix(PathBuf),
    Outside,
}

impl NamedFiles {
    fn new(root: &Path, written: &Path) -> Self {
        match checkout_relative(root, written) {
            None => Self::Outside,
            Some(path) if root.join(&path).is_file() => Self::Exact(path),
            Some(path) => Self::Suffix(path),
        }
    }

    fn contains(&self, root: &Path, uri: &str) -> Result<bool> {
        let file = file_path(uri)?;
        let Ok(relative) = file.strip_prefix(root) else {
            return Ok(false);
        };
        Ok(match self {
            Self::Exact(path) => relative == path,
            Self::Suffix(path) => relative.ends_with(path),
            Self::Outside => false,
        })
    }
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

/// A file-qualified `name` keeps only matches located in a file its head names; the
/// `Missing` candidates stay every exact-name symbol.
fn resolve_symbol(root: &Path, name: &str, result: Value) -> Result<SymbolResolution> {
    let symbols = if result.is_null() {
        Vec::new()
    } else {
        serde_json::from_value(result)?
    };
    match_symbols(root, name, symbols)
}

fn match_symbols(
    root: &Path,
    name: &str,
    symbols: Vec<SymbolInformation>,
) -> Result<SymbolResolution> {
    let (files, name) = file_qualified(name).map_or((None, name), |(file, name)| {
        (Some(NamedFiles::new(root, Path::new(file))), name)
    });
    let mut qualifier = name_segments(name);
    let name = qualifier.pop().unwrap_or_default();
    let candidates: Vec<_> = symbols
        .into_iter()
        .filter(|symbol| symbol.name == name)
        .collect();
    let mut matches = Vec::new();
    for symbol in &candidates {
        if let Some(files) = &files
            && !files.contains(root, &symbol.location.uri)?
        {
            continue;
        }
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct DocumentKey(String);

impl DocumentKey {
    pub(super) fn new(uri: &str) -> Self {
        let Ok(url) = url::Url::parse(uri) else {
            return Self(uri.to_owned());
        };
        let canonical = url
            .to_file_path()
            .ok()
            .and_then(|path| url::Url::from_file_path(path).ok())
            .unwrap_or(url);
        Self(canonical.into())
    }
}

fn file_path(uri: &str) -> Result<PathBuf> {
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

pub(super) fn kind_name(kind: u32) -> &'static str {
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

fn render_find(
    root: &Path,
    symbols: &[SymbolInformation],
    options: ListOptions,
    tests_last: bool,
    outlines: &mut impl FnMut(&str) -> Result<Value>,
) -> Result<String> {
    let mut scoped = Vec::new();
    let mut external = Vec::new();
    let mut hidden = 0;
    for symbol in symbols {
        let path = displayed_path(root, &symbol.location.uri)?;
        if !options.scope.includes(&path) {
            hidden += 1;
        } else if Scope::Checkout.includes(&path) {
            scoped.push(symbol);
        } else {
            external.push(symbol);
        }
    }
    scoped.extend(external);
    let total = scoped.len();
    let partition = partition_tests(
        root,
        scoped,
        options,
        tests_last,
        |symbol| (&symbol.location.uri, symbol.location.range.start),
        outlines,
    )?;
    let shown = options.shown(total);
    let mut lines = Vec::new();
    let mut tests_started = false;
    for (is_test, symbol) in partition.items.into_iter().take(shown) {
        if is_test && !tests_started {
            lines.push("tests:".into());
            tests_started = true;
        }
        lines.push(Candidate::new(root, symbol)?.line());
    }
    Ok(finish_list(
        lines,
        hidden,
        total,
        shown,
        "symbols",
        partition.tests,
        None,
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

fn rank_candidates(
    root: &Path,
    name: &str,
    symbols: &[SymbolInformation],
) -> Result<Vec<(usize, Candidate)>> {
    let mut qualifier = name_segments(name);
    qualifier.pop();
    let mut candidates = Vec::new();
    for (index, symbol) in symbols.iter().enumerate() {
        let path = symbol_path(root, symbol)?;
        let score = qualifier
            .iter()
            .filter(|segment| path.contains(segment))
            .count();
        candidates.push((
            std::cmp::Reverse(score),
            Candidate::new(root, symbol)?,
            index,
        ));
    }
    candidates
        .sort_by(|a, b| (&a.0, &a.1.name, &a.1.position).cmp(&(&b.0, &b.1.name, &b.1.position)));
    Ok(candidates
        .into_iter()
        .map(|(_, candidate, index)| (index, candidate))
        .collect())
}

fn outcome_candidates<'a>(
    root: &Path,
    output: &'a Output,
) -> Result<(&'a str, Vec<Candidate>, usize)> {
    let (Output::Ambiguous {
        name,
        symbols,
        unresolved,
    }
    | Output::NotFound {
        name,
        symbols,
        unresolved,
    }) = output
    else {
        return Err(LspErr::Protocol(
            "render lookup candidates only for not-found or ambiguous outcomes".into(),
        ));
    };
    let candidates = rank_candidates(root, name, symbols)?
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect();
    Ok((name, candidates, *unresolved))
}

pub fn outcome_json(root: &Path, output: &Output) -> Result<Value> {
    if let Output::Answer { result, .. } = output {
        return Ok(serde_json::json!({"outcome": "answer", "result": result}));
    }
    let (name, candidates, unresolved) = outcome_candidates(root, output)?;
    let total = candidates.len() + unresolved;
    Ok(serde_json::json!({
        "outcome": if matches!(output, Output::NotFound { .. }) { "not-found" } else { "ambiguous" },
        "name": name, "candidates": candidates,
        "total": total, "truncated": unresolved > 0,
    }))
}

pub fn render_outcome(root: &Path, output: &Output, json: bool) -> Result<String> {
    if json {
        return Ok(format!(
            "{}\n",
            serde_json::to_string_pretty(&outcome_json(root, output)?)?
        ));
    }
    let (name, candidates, unresolved) = outcome_candidates(root, output)?;
    let last = name_segments(name).pop().unwrap_or_default();
    let count = candidates.len() + unresolved;
    let bound = if unresolved == 0 { "" } else { "up to " };
    let missing = matches!(output, Output::NotFound { .. });
    let header = if missing {
        if count == 0 {
            format!("not found: {name}")
        } else {
            format!(
                "not found: {name}; {bound}{count} other {} named {last}:",
                if count == 1 { "symbol" } else { "symbols" }
            )
        }
    } else {
        format!(
            "ambiguous: {bound}{count} symbols named {name}; rerun with one of these names or a position"
        )
    };
    let mut lines = vec![header];
    lines.extend(candidates.iter().take(20).map(Candidate::line));
    let shown = candidates.len().min(20);
    if count > shown {
        lines.push(format!(
            "{} more; narrow with a qualifier or use find",
            count - shown
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
        #[serde(rename = "targetRange")]
        span: Option<Range>,
    },
}

fn locations(result: Value) -> Result<Vec<Location>> {
    Ok(locations_with_spans(result)?
        .into_iter()
        .map(|(location, _)| location)
        .collect())
}

pub(super) fn span_lines(range: Range) -> [u32; 2] {
    [range.start.line + 1, range.end.line + 1]
}

fn locations_with_spans(result: Value) -> Result<Vec<(Location, Option<Range>)>> {
    let values = match result {
        Value::Null => return Ok(Vec::new()),
        Value::Array(values) => values,
        value => vec![value],
    };
    values
        .into_iter()
        .map(|value| {
            Ok(match serde_json::from_value::<LocationResult>(value)? {
                LocationResult::Location(location) => (location, None),
                LocationResult::Link { uri, range, span } => (Location { uri, range }, span),
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

fn finish_scoped(lines: Vec<String>, hidden: usize) -> String {
    let mut output = finish(lines);
    if hidden > 0 {
        output.push_str(&format!(
            "{hidden} outside the checkout hidden; add --external to show them\n"
        ));
    }
    output
}

fn finish_list(
    mut lines: Vec<String>,
    hidden: usize,
    total: usize,
    shown: usize,
    noun: &str,
    tests: Option<usize>,
    hidden_tests: Option<usize>,
) -> String {
    if shown < total {
        let count = match tests.filter(|_| hidden_tests.is_none()) {
            Some(tests) if tests > 0 => format!(", {tests} in tests"),
            _ => String::new(),
        };
        let order = if hidden_tests.is_none() && tests != Some(0) {
            ", tests last"
        } else {
            ""
        };
        lines.insert(0, format!("{total} {noun}{count} (showing {shown}{order})"));
        lines.push(format!("{} more; add --limit N or --all", total - shown));
    }
    let mut output = finish_scoped(lines, hidden);
    if let Some(tests) = hidden_tests.filter(|tests| *tests > 0) {
        output.push_str(&format!(
            "{tests} test {noun} hidden; drop --no-tests to show them\n"
        ));
    }
    output
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

impl Scope {
    fn includes(self, displayed_path: &str) -> bool {
        self == Self::External || !Path::new(displayed_path).is_absolute()
    }
}

const LIST_CAP: usize = 50;

#[derive(Clone, Copy)]
pub struct ListOptions {
    pub scope: Scope,
    pub limit: Option<NonZeroUsize>,
    pub no_tests: bool,
}

impl From<Scope> for ListOptions {
    fn from(scope: Scope) -> Self {
        Self {
            scope,
            limit: NonZeroUsize::new(LIST_CAP),
            no_tests: false,
        }
    }
}

impl ListOptions {
    fn shown(self, total: usize) -> usize {
        self.limit.map_or(total, |limit| total.min(limit.get()))
    }
}

/// `document_uri` is supplied for document-symbol trees, which omit their own URI.
pub fn render(
    verb: Verb,
    root: &Path,
    document_uri: Option<&str>,
    result: Value,
    options: ListOptions,
    dirty: &BTreeSet<String>,
    mut symbols: impl FnMut(&Path) -> Result<Value>,
) -> Result<String> {
    let mut files = BTreeMap::new();
    render_with_source(
        verb,
        root,
        document_uri,
        result,
        options,
        dirty,
        |uri, line| {
            let lines = files.entry(DocumentKey::new(uri)).or_insert_with(|| {
                file_path(uri)
                    .ok()
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
            });
            lines
                .as_ref()
                .and_then(|lines| lines.get(line as usize))
                .cloned()
                .ok_or_else(|| LspErr::Protocol("location is past the end of the file".into()))
        },
        |uri| symbols(&file_path(uri)?),
    )
}

const SNIPPET_WIDTH: usize = 100;

pub(super) fn snippet(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= SNIPPET_WIDTH {
        return text.to_owned();
    }
    text.chars().take(SNIPPET_WIDTH - 1).chain(['…']).collect()
}

pub fn dirty_documents(entry: &super::registry::Entry) -> BTreeSet<String> {
    entry
        .attached
        .iter()
        .flat_map(|editor| &editor.open)
        .filter(|document| document.owner && document.dirty)
        .map(|document| document.uri.clone())
        .collect()
}

fn grouped_position(
    lines: &mut Vec<String>,
    previous: &mut Option<(bool, String)>,
    is_test: bool,
    path: String,
    position: Position,
) -> String {
    if previous
        .as_ref()
        .is_none_or(|(test, previous)| *test != is_test || previous != &path)
    {
        if is_test && previous.as_ref().is_none_or(|(test, _)| !test) {
            lines.push("tests:".into());
        }
        lines.push(path.clone());
        *previous = Some((is_test, path));
    }
    format!("  {}:{}", position.line + 1, position.character + 1)
}

fn is_test_name(name: &str) -> bool {
    matches!(name, "tests" | "testkit" | "test_support") || name.ends_with("_tests")
}

fn test_ranges(uri: &str, result: Value) -> Result<Vec<Range>> {
    if result.is_null() {
        return Ok(Vec::new());
    }
    match serde_json::from_value(result)? {
        Symbols::Flat(symbols) => Ok(symbols
            .into_iter()
            .filter(|symbol| {
                symbol.kind == 2
                    && is_test_name(&symbol.name)
                    && DocumentKey::new(&symbol.location.uri) == DocumentKey::new(uri)
            })
            .map(|symbol| symbol.location.range)
            .collect()),
        Symbols::Tree(mut pending) => {
            let mut ranges = Vec::new();
            while let Some(symbol) = pending.pop() {
                if symbol.kind == 2 && is_test_name(&symbol.name) {
                    ranges.push(symbol.range);
                }
                pending.extend(symbol.children);
            }
            Ok(ranges)
        }
    }
}

struct TestPartition<T> {
    items: Vec<(bool, T)>,
    tests: Option<usize>,
    hidden: Option<usize>,
}

fn partition_tests<T>(
    root: &Path,
    items: impl IntoIterator<Item = T>,
    options: ListOptions,
    tests_last: bool,
    location: impl Fn(&T) -> (&str, Position),
    symbols: &mut impl FnMut(&str) -> Result<Value>,
) -> Result<TestPartition<T>> {
    if !tests_last {
        return Ok(TestPartition {
            items: items.into_iter().map(|item| (false, item)).collect(),
            tests: Some(0),
            hidden: options.no_tests.then_some(0),
        });
    }
    let mut ranges = BTreeMap::new();
    let mut pending = items.into_iter().peekable();
    let mut production = Vec::new();
    let mut tests = Vec::new();
    let mut test_count = 0;
    let mut outlines_failed = false;
    for item in pending.by_ref() {
        let (uri, position) = location(&item);
        let path = PathBuf::from(displayed_path(root, uri)?);
        let is_test = path
            .components()
            .any(|part| part.as_os_str().to_str().is_some_and(is_test_name))
            || path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(is_test_name)
            || (!outlines_failed
                && ranges
                    .entry(DocumentKey::new(uri))
                    .or_insert_with(|| {
                        match symbols(uri).and_then(|outline| test_ranges(uri, outline)) {
                            Ok(ranges) => ranges,
                            Err(_) => {
                                outlines_failed = true;
                                Vec::new()
                            }
                        }
                    })
                    .iter()
                    .any(|range| range.start <= position && position < range.end));
        if is_test {
            test_count += 1;
            if !options.no_tests {
                tests.push((true, item));
            }
        } else {
            production.push((false, item));
        }
        if !options.no_tests
            && options
                .limit
                .is_some_and(|limit| production.len() == limit.get())
        {
            break;
        }
    }
    production.extend(tests);
    Ok(TestPartition {
        items: production,
        tests: (!outlines_failed && pending.peek().is_none()).then_some(test_count),
        hidden: options.no_tests.then_some(test_count),
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "source and document symbols are independently injected for rendering tests"
)]
fn render_with_source(
    verb: Verb,
    root: &Path,
    document_uri: Option<&str>,
    result: Value,
    options: ListOptions,
    dirty: &BTreeSet<String>,
    mut source: impl FnMut(&str, u32) -> Result<String>,
    mut symbols: impl FnMut(&str) -> Result<Value>,
) -> Result<String> {
    let scope = options.scope;
    if result.is_null() {
        return Ok("no results\n".into());
    }
    let dirty: BTreeSet<_> = dirty.iter().map(|uri| DocumentKey::new(uri)).collect();
    let mut source_suffix = |location: &Location| {
        if dirty.contains(&DocumentKey::new(&location.uri)) {
            "  (unsaved in editor)".into()
        } else if let Ok(text) = source(&location.uri, location.range.start.line) {
            format!("  {}", snippet(&text))
        } else {
            String::new()
        }
    };
    match verb {
        Verb::Def => {
            let mut sorted = BTreeMap::new();
            for (location, span) in locations_with_spans(result)? {
                sorted.insert(
                    (displayed_path(root, &location.uri)?, location.range.start),
                    (location, span),
                );
            }
            let mut lines = Vec::new();
            for (location, span) in sorted.into_values() {
                let mut line = position_text(root, &location.uri, location.range.start)?;
                if let Some(span) = span.filter(|span| span.start != span.end) {
                    let [start, end] = span_lines(span);
                    if start != end {
                        line.push_str(&format!(" ({start}-{end})"));
                    }
                }
                lines.push(format!("{line}{}", source_suffix(&location)));
            }
            Ok(finish(lines))
        }
        Verb::Refs | Verb::Impl => {
            let mut sorted = BTreeMap::new();
            for location in locations(result)? {
                let position = location.range.start;
                let path = displayed_path(root, &location.uri)?;
                sorted.insert((!Scope::Checkout.includes(&path), path, position), location);
            }
            let total = sorted.len();
            sorted.retain(|(_, path, _), _| scope.includes(path));
            let hidden = total - sorted.len();
            let total = sorted.len();
            let partition = partition_tests(
                root,
                sorted,
                options,
                verb == Verb::Refs,
                |(_, location)| (&location.uri, location.range.start),
                &mut symbols,
            )?;
            let total = if options.no_tests {
                partition.items.len()
            } else {
                total
            };
            let shown = options.shown(total);
            let mut lines = Vec::new();
            let mut previous = None;
            for (is_test, ((_, path, position), location)) in
                partition.items.into_iter().take(shown)
            {
                let line = grouped_position(&mut lines, &mut previous, is_test, path, position);
                lines.push(format!("{line}{}", source_suffix(&location)));
            }
            let noun = if verb == Verb::Refs {
                "references"
            } else {
                "implementations"
            };
            Ok(finish_list(
                lines,
                hidden,
                total,
                shown,
                noun,
                partition.tests,
                partition.hidden,
            ))
        }
        Verb::Hover => {
            #[derive(Deserialize)]
            struct Hover {
                contents: HoverContents,
            }
            let hover: Hover = serde_json::from_value(result)?;
            let text: String = hover
                .contents
                .text()
                .trim_end()
                .split_inclusive('\n')
                .skip_while(|line| line.trim().is_empty())
                .collect();
            Ok(format!("{text}\n"))
        }
        Verb::Find => render_find(
            root,
            &serde_json::from_value::<Vec<SymbolInformation>>(result)?,
            options,
            true,
            &mut symbols,
        ),
        Verb::Symbols => match serde_json::from_value(result)? {
            Symbols::Flat(flat) => render_find(
                root,
                &flat,
                ListOptions {
                    scope: Scope::External,
                    limit: None,
                    no_tests: false,
                },
                false,
                &mut symbols,
            ),
            Symbols::Tree(symbols) => {
                let uri = document_uri
                    .ok_or_else(|| LspErr::Protocol("document symbols need a file URI".into()))?;
                let mut lines = Vec::new();
                let mut pending: Vec<_> = symbols.iter().rev().map(|symbol| (symbol, 0)).collect();
                while let Some((symbol, depth)) = pending.pop() {
                    lines.push(format!(
                        "{}{} {}  {}",
                        "  ".repeat(depth),
                        kind_name(symbol.kind),
                        symbol.name,
                        position_text(root, uri, symbol.selection_range.start)?
                    ));
                    pending.extend(symbol.children.iter().rev().map(|child| (child, depth + 1)));
                }
                Ok(finish(lines))
            }
        },
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
                let path = displayed_path(root, &item.uri)?;
                sorted.insert(
                    (
                        !Scope::Checkout.includes(&path),
                        path,
                        item.selection_range.start,
                        item.name.clone(),
                    ),
                    item,
                );
            }
            let total = sorted.len();
            sorted.retain(|(_, path, _, _), _| scope.includes(path));
            let hidden = total - sorted.len();
            let total = sorted.len();
            let partition = partition_tests(
                root,
                sorted,
                options,
                verb == Verb::Callers,
                |(_, item)| (&item.uri, item.selection_range.start),
                &mut symbols,
            )?;
            let total = if options.no_tests {
                partition.items.len()
            } else {
                total
            };
            let shown = options.shown(total);
            let mut lines = Vec::new();
            let mut previous = None;
            for (is_test, ((_, path, position, _), item)) in partition.items.into_iter().take(shown)
            {
                let line = grouped_position(&mut lines, &mut previous, is_test, path, position);
                lines.push(format!("{line}  {}", item.name));
            }
            let noun = if verb == Verb::Callers {
                "callers"
            } else {
                "callees"
            };
            Ok(finish_list(
                lines,
                hidden,
                total,
                shown,
                noun,
                partition.tests,
                partition.hidden,
            ))
        }
    }
}

#[cfg(test)]
mod tests;
