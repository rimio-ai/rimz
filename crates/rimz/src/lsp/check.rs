//! Validate Markdown anchors against checkout files and language-server outlines.

use super::{LspErr, registry};
use super::{protocol::Symbols, query};
use pulldown_cmark::{Event, Parser};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const LINE_SLACK: u32 = 3;

mod fix;
mod show;
pub use show::show;

/// What `run` does to the notes before checking them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Check,
    Fix,
    FixHints,
}

pub fn run(
    notes: &[PathBuf],
    root: &Path,
    entries: &[registry::Entry],
    servers: &BTreeMap<String, crate::config::LspServerConfig>,
    mode: Mode,
) -> Result<Vec<Result<Report, query::QueryErr>>, query::QueryErr> {
    let mut context = Context::new(root, entries, servers)?;
    Ok(notes
        .iter()
        .map(|notes| check_file(notes, &mut context, mode))
        .collect())
}

fn check_file(
    notes: &Path,
    context: &mut Context<'_>,
    mode: Mode,
) -> Result<Report, query::QueryErr> {
    let root = context.checkout;
    let source = std::fs::read_to_string(notes)
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("cannot read {}: {error}", notes.display()),
            )
        })
        .map_err(LspErr::from)?;
    let (source, fixes, ambiguous) = if mode == Mode::Check {
        (source, None, None)
    } else {
        let hints = mode == Mode::FixHints;
        let (updated, fixes, ambiguous) = fix::rewrite(&source, context, hints)?;
        (updated, Some(fixes), hints.then_some(ambiguous))
    };
    let mut lengths = BTreeMap::new();
    let mut verdicts = Vec::new();
    for anchor in extract(&source) {
        let (anchor, path) = match context.resolve(anchor) {
            Ok(resolved) => resolved,
            Err(result) => {
                verdicts.push(*result);
                continue;
            }
        };
        if anchor.symbol.is_none() {
            let lines = match lengths.entry(path.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let bytes = std::fs::read(root.join(&path)).map_err(LspErr::from)?;
                    let lines = bytes.iter().filter(|byte| **byte == b'\n').count()
                        + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"));
                    *entry.insert(lines)
                }
            };
            verdicts.push(check_lines(anchor, path.clone(), lines));
            continue;
        }
        let Some(file) = context.outline(&path)? else {
            let mut result = verdict(anchor, Some(path.clone()), Status::Unchecked);
            result.detail = unchecked_detail(&path);
            verdicts.push(result);
            continue;
        };
        verdicts.push(check_symbol(anchor, path, &file.nodes));
    }
    // Written only once the anchors are judged, so a file that reports an
    // error is a file left as it was.
    if fixes.as_ref().is_some_and(|fixes| !fixes.is_empty()) {
        let target = std::fs::canonicalize(notes).map_err(LspErr::from)?;
        crate::disk::atomic::write_bytes_atomically(&target, source.as_bytes())
            .map_err(LspErr::from)?;
    }
    let mut report = Report::new(notes, root, verdicts);
    report.fixes = fixes;
    report.ambiguous = ambiguous;
    Ok(report)
}

struct Context<'a> {
    checkout: &'a Path,
    entries: &'a [registry::Entry],
    servers: &'a BTreeMap<String, crate::config::LspServerConfig>,
    files: Vec<PathBuf>,
    outlines: BTreeMap<PathBuf, FileOutline>,
    failures: BTreeMap<String, query::QueryErr>,
}

struct FileOutline {
    nodes: Vec<Candidate>,
    dirty: bool,
}

impl<'a> Context<'a> {
    fn new(
        root: &'a Path,
        entries: &'a [registry::Entry],
        servers: &'a BTreeMap<String, crate::config::LspServerConfig>,
    ) -> Result<Self, query::QueryErr> {
        let output = crate::proc::git_command(root)
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .output()
            .map_err(LspErr::from)?;
        if !output.status.success() {
            return Err(LspErr::from(std::io::Error::other(format!(
                "cannot list checkout files in {}: {}",
                root.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )))
            .into());
        }
        use std::os::unix::ffi::OsStrExt;
        let mut files: Vec<_> = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(std::ffi::OsStr::from_bytes(path)))
            .filter(|path| root.join(path).is_file())
            .collect();
        files.sort();
        files.dedup();
        Ok(Self {
            checkout: root,
            entries,
            servers,
            files,
            outlines: BTreeMap::new(),
            failures: BTreeMap::new(),
        })
    }

    fn resolve(&self, mut anchor: Anchor) -> Result<(Anchor, PathBuf), Box<Verdict>> {
        if let Some(qualifier) = anchor.qualifier.take() {
            let mut result = verdict(anchor, None, Status::External);
            result.detail = qualifier;
            return Err(Box::new(result));
        }
        let matches = resolve(self.checkout, &self.files, &anchor.path);
        if matches.len() != 1 {
            let status = if matches.is_empty() {
                Status::MissingPath
            } else {
                Status::AmbiguousPath
            };
            let mut result = verdict(anchor, None, status);
            result.detail = if matches.is_empty() {
                "no matching checkout file".into()
            } else {
                let names: Vec<_> = matches
                    .iter()
                    .take(5)
                    .map(|path| path.display().to_string())
                    .collect();
                let more = if matches.len() > 5 {
                    format!(", +{} more", matches.len() - 5)
                } else {
                    String::new()
                };
                format!("{} files: {}{more}", matches.len(), names.join(", "))
            };
            result.files = matches;
            return Err(Box::new(result));
        }
        Ok((anchor, matches[0].clone()))
    }

    fn is_dirty(&self, path: &Path) -> Result<bool, LspErr> {
        let uri = url::Url::from_file_path(self.checkout.join(path))
            .map_err(|()| LspErr::Protocol(format!("not an absolute path: {}", path.display())))?;
        let key = query::DocumentKey::new(uri.as_str());
        Ok(self
            .entries
            .iter()
            .flat_map(query::dirty_documents)
            .any(|dirty| query::DocumentKey::new(&dirty) == key))
    }

    fn outline(&mut self, path: &Path) -> Result<Option<&FileOutline>, query::QueryErr> {
        let (checkout, entries, servers) = (self.checkout, self.entries, self.servers);
        self.outline_with(
            path,
            || query::select(checkout, entries.to_vec(), servers, None, Some(path)),
            |server, path| {
                query::execute(
                    server,
                    query::Verb::Symbols,
                    &query::Target::File(path.to_owned()),
                )
            },
        )
    }

    fn outline_with(
        &mut self,
        path: &Path,
        select: impl FnOnce() -> Result<registry::Entry, query::QueryErr>,
        request: impl FnOnce(&registry::Entry, &Path) -> Result<query::Output, query::QueryErr>,
    ) -> Result<Option<&FileOutline>, query::QueryErr> {
        let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        if !self
            .servers
            .values()
            .any(|config| config.extensions.iter().any(|ext| ext == extension))
        {
            return Ok(None);
        }
        let dirty = self.is_dirty(path)?;
        let file = match self.outlines.entry(path.to_owned()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                // Selection probes broker liveness, so check the memo before selecting again.
                let mut candidates = self.entries.iter().filter(|entry| {
                    entry.root == self.checkout
                        && self.servers.get(&entry.server).is_some_and(|config| {
                            config.extensions.iter().any(|ext| ext == extension)
                        })
                });
                let server = candidates.next().filter(|_| candidates.next().is_none());
                if let Some(error) = server
                    .and_then(|server| self.failures.get(&server.server))
                    .and_then(Self::server_error)
                {
                    return Err(error);
                }
                let result = select().and_then(|server| request(&server, path));
                if let Err(error) = &result
                    && let Some(error) = Self::server_error(error)
                    && let Some(server) = server
                {
                    self.failures.insert(server.server.clone(), error);
                }
                let query::Output::Answer { result, .. } = result? else {
                    return Err(LspErr::Protocol(
                        "document symbols returned a lookup outcome".into(),
                    )
                    .into());
                };
                entry.insert(FileOutline {
                    nodes: outline(result)?,
                    dirty,
                })
            }
        };
        Ok(Some(file))
    }

    fn server_error(error: &query::QueryErr) -> Option<query::QueryErr> {
        match error {
            query::QueryErr::Unavailable { root, reason } => Some(query::QueryErr::Unavailable {
                root: root.clone(),
                reason: reason.clone(),
            }),
            query::QueryErr::Indexing { server, seconds } => Some(query::QueryErr::Indexing {
                server: server.clone(),
                seconds: *seconds,
            }),
            query::QueryErr::Failed(_) => None,
        }
    }
}

fn unchecked_detail(path: &Path) -> String {
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    format!("no server configured for .{extension}")
}

#[derive(Debug, PartialEq, Eq)]
struct Anchor {
    qualifier: Option<String>,
    line: usize,
    text: String,
    path: String,
    symbol: Option<Vec<String>>,
    hint: Option<[u32; 2]>,
    hint_text: Option<std::ops::Range<usize>>,
    hint_source: Option<std::ops::Range<usize>>,
    path_source: Option<std::ops::Range<usize>>,
    span_end: Option<usize>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct Candidate {
    name: String,
    #[serde(skip)]
    path: Vec<String>,
    kind: &'static str,
    range: [u32; 2],
    #[serde(skip)]
    extent: super::protocol::Range,
    #[serde(skip)]
    selection: super::protocol::Range,
    #[serde(skip)]
    parent: Option<usize>,
}

#[derive(Debug, Serialize)]
struct Verdict {
    line: usize,
    text: String,
    status: Status,
    path: Option<PathBuf>,
    symbol: Option<Vec<String>>,
    hint: Option<[u32; 2]>,
    range: Option<[u32; 2]>,
    candidates: Vec<Candidate>,
    files: Vec<PathBuf>,
    detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Status {
    Ok,
    External,
    MissingPath,
    AmbiguousPath,
    MissingSymbol,
    LineOutside,
    Unchecked,
}

#[derive(Debug, Serialize)]
pub struct Report {
    notes: PathBuf,
    checkout: PathBuf,
    anchors: Vec<Verdict>,
    summary: Summary,
    #[serde(skip_serializing_if = "Option::is_none")]
    fixes: Option<Vec<fix::Fix>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ambiguous: Option<Vec<fix::Ambiguous>>,
}

#[derive(Debug, Default, Serialize)]
struct Summary {
    anchors: usize,
    ok: usize,
    failed: usize,
    unchecked: usize,
    external: usize,
}

impl Report {
    fn new(notes: &Path, checkout: &Path, anchors: Vec<Verdict>) -> Self {
        let mut summary = Summary {
            anchors: anchors.len(),
            ..Summary::default()
        };
        for anchor in &anchors {
            match anchor.status {
                Status::Ok => summary.ok += 1,
                Status::Unchecked => summary.unchecked += 1,
                Status::External => summary.external += 1,
                Status::MissingPath
                | Status::AmbiguousPath
                | Status::MissingSymbol
                | Status::LineOutside => summary.failed += 1,
            }
        }
        Self {
            notes: notes.into(),
            checkout: checkout.into(),
            anchors,
            summary,
            fixes: None,
            ambiguous: None,
        }
    }

    pub fn exit_code(&self) -> i32 {
        if self.summary.failed == 0 { 0 } else { 7 }
    }

    pub fn render(&self, json: bool) -> super::Result<String> {
        if json {
            return Ok(format!("{}\n", serde_json::to_string_pretty(self)?));
        }
        let mut text = String::new();
        for fix in self.fixes.iter().flatten() {
            text.push_str(&format!(
                "{}:{}  fixed  {}  {}\n",
                self.notes.display(),
                fix.line,
                fix.before,
                fix.after
            ));
        }
        for anchor in self.ambiguous.iter().flatten() {
            text.push_str(&format!(
                "{}:{}  ambiguous-symbol  {}  {}\n",
                self.notes.display(),
                anchor.line,
                anchor.text,
                anchor.detail
            ));
        }
        for anchor in &self.anchors {
            let status = match anchor.status {
                Status::Ok | Status::External => continue,
                Status::MissingPath => "missing-path",
                Status::AmbiguousPath => "ambiguous-path",
                Status::MissingSymbol => "missing-symbol",
                Status::LineOutside => "line-outside",
                Status::Unchecked => "unchecked",
            };
            text.push_str(&format!(
                "{}:{}  {status}  {}  {}\n",
                self.notes.display(),
                anchor.line,
                anchor.text,
                anchor.detail
            ));
        }
        match self.ambiguous.as_ref().map_or(0, Vec::len) {
            0 => {}
            1 => text.push_str("1 anchor left without a hint: several items match\n"),
            count => text.push_str(&format!(
                "{count} anchors left without a hint: several items match\n"
            )),
        }
        text.push_str(&format!(
            "{} anchors in {}: {} ok, {} failed, {} unchecked, {} external\n",
            self.summary.anchors,
            self.notes.display(),
            self.summary.ok,
            self.summary.failed,
            self.summary.unchecked,
            self.summary.external
        ));
        Ok(text)
    }
}

fn extract(notes: &str) -> Vec<Anchor> {
    let mut events = Parser::new(notes).into_offset_iter().peekable();
    let mut anchors = Vec::new();
    while let Some((event, offset)) = events.next() {
        let Event::Code(code) = event else { continue };
        let line = notes[..offset.start]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
            + 1;
        let qualified = code.split_once(':').and_then(|(qualifier, rest)| {
            let (repository, revision) = qualifier.split_once('@')?;
            let (owner, repo) = repository.split_once('/')?;
            if revision.is_empty()
                || revision.chars().any(char::is_whitespace)
                || [owner, repo].iter().any(|segment| {
                    segment.is_empty()
                        || !segment
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                })
            {
                return None;
            }
            let mut anchor = parse_anchor(rest, line)?;
            anchor.qualifier = Some(qualifier.into());
            anchor.text = code.to_string();
            Some(anchor)
        });
        let Some(mut anchor) = qualified.or_else(|| parse_anchor(&code, line)) else {
            continue;
        };
        let raw = &notes[offset.clone()];
        let delimiter = raw.bytes().take_while(|byte| *byte == b'`').count();
        let content = &raw[delimiter..raw.len() - delimiter];
        let padding = usize::from(
            content
                .strip_prefix(' ')
                .and_then(|text| text.strip_suffix(' '))
                == Some(code.as_ref()),
        );
        let mapped = content.get(padding..content.len() - padding) == Some(code.as_ref());
        if mapped && anchor.qualifier.is_none() {
            anchor.span_end = Some(offset.end);
            let start = offset.start + delimiter + padding;
            anchor.path_source = Some(start..start + anchor.path.len());
            anchor.hint_source = anchor
                .hint_text
                .as_ref()
                .map(|hint| start + hint.start..start + hint.end);
        }
        if anchor.hint.is_none()
            && let Some((Event::Text(following), following_offset)) = events.peek()
            && let Some((found, end)) = parse_hint(following, false)
        {
            anchor.hint = Some(found);
            anchor.hint_text = Some(anchor.text.len()..anchor.text.len() + end);
            if mapped && notes[following_offset.clone()].starts_with(&following[..end]) {
                anchor.hint_source = Some(following_offset.start..following_offset.start + end);
            }
            anchor.text.push_str(&following[..end]);
        }
        anchors.push(anchor);
    }
    anchors
}

fn parse_anchor(code: &str, line: usize) -> Option<Anchor> {
    let (path, rest) = code.split_once(':')?;
    if !query::is_file_head(path) {
        return None;
    }
    let (symbol, hint, hint_text) = if let Some(raw) = rest.strip_prefix(':') {
        let mut depth = 0_u32;
        let end = raw
            .char_indices()
            .find_map(|(i, ch)| {
                match ch {
                    '<' => depth += 1,
                    '>' => depth = depth.saturating_sub(1),
                    _ if depth == 0
                        && (ch.is_whitespace()
                            || "({[,~#!=;".contains(ch)
                            || (ch == ':'
                                && !raw[..i].ends_with(':')
                                && raw[i + 1..]
                                    .starts_with(|c: char| c.is_ascii_digit() || c == '~'))) =>
                    {
                        return Some(i);
                    }
                    _ => {}
                }
                None
            })
            .unwrap_or(raw.len());
        let name = query::without_generics(&raw[..end]);
        let chain = segments(name.trim_end_matches([':', '.']));
        if chain.is_empty() {
            return None;
        }
        let tail = &raw[end..];
        let hint = parse_hint(tail, false)
            .map(|hint| (tail, hint))
            .or_else(|| {
                tail.strip_prefix('(')
                    .and_then(|tail| tail.split_once(')'))
                    .and_then(|(_, tail)| parse_hint(tail, false).map(|hint| (tail, hint)))
            });
        let hint_text = hint.map(|(tail, (_, consumed))| {
            let start = code.len() - tail.len();
            start..start + consumed
        });
        (Some(chain), hint.map(|(_, (lines, _))| lines), hint_text)
    } else {
        let (hint, _) = parse_hint(rest, true)?;
        (None, Some(hint), None)
    };
    Some(Anchor {
        qualifier: None,
        line,
        text: code.into(),
        path: path.into(),
        symbol,
        hint,
        hint_text,
        hint_source: None,
        path_source: None,
        span_end: None,
    })
}

fn parse_hint(raw: &str, line_only: bool) -> Option<([u32; 2], usize)> {
    let mut tail = raw.trim_start();
    if line_only {
        if !raw.starts_with(|ch: char| ch.is_ascii_digit() || ch == '~') {
            return None;
        }
    } else if !tail.starts_with(['(', ':', '~']) {
        return None;
    }
    let parenthesized = tail.starts_with('(');
    if parenthesized {
        tail = &tail[1..];
    }
    tail = tail.strip_prefix(':').unwrap_or(tail);
    tail = tail.strip_prefix('~').unwrap_or(tail);
    let number = |tail: &str| {
        let end = tail.bytes().take_while(u8::is_ascii_digit).count();
        Some((tail[..end].parse::<u32>().ok()?, end))
    };
    let (start, end) = number(tail)?;
    tail = &tail[end..];
    let mut last = start;
    if let Some(rest) = tail.strip_prefix('-') {
        let (value, end) = number(rest)?;
        last = value;
        tail = &rest[end..];
    } else if let Some(rest) = tail.strip_prefix(':') {
        let (_, end) = number(rest)?;
        tail = &rest[end..];
        if line_only && tail.trim_start().starts_with('(') {
            let (hint, consumed) = parse_hint(tail, false)?;
            if !tail[consumed..].trim().is_empty() {
                return None;
            }
            return Some((hint, raw.len() - tail.len() + consumed));
        }
    }
    if parenthesized {
        tail = tail.strip_prefix(')')?;
    }
    if line_only && !tail.trim().is_empty() {
        return None;
    }
    Some(([start, last], raw.len() - tail.len()))
}

fn segments(name: &str) -> Vec<String> {
    name.split("::")
        .flat_map(|part| part.split('.'))
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

fn resolve(root: &Path, files: &[PathBuf], path: &str) -> Vec<PathBuf> {
    let Some(path) = query::checkout_relative(root, Path::new(path)) else {
        return Vec::new();
    };
    if files.contains(&path) || root.join(&path).is_file() {
        return vec![path];
    }
    files
        .iter()
        .filter(|file| file.ends_with(&path))
        .cloned()
        .collect()
}

fn outline(value: serde_json::Value) -> super::Result<Vec<Candidate>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let mut nodes = Vec::new();
    match serde_json::from_value(value)? {
        Symbols::Flat(symbols) => {
            for symbol in symbols {
                let mut path = symbol
                    .container_name
                    .as_deref()
                    .map(segments)
                    .unwrap_or_default();
                path.push(symbol.name);
                nodes.push(Candidate {
                    name: path.join("::"),
                    path,
                    kind: query::kind_name(symbol.kind),
                    range: query::span_lines(symbol.location.range),
                    extent: symbol.location.range,
                    selection: symbol.location.range,
                    parent: None,
                });
            }
        }
        Symbols::Tree(symbols) => {
            let mut pending: Vec<_> = symbols
                .into_iter()
                .rev()
                .map(|symbol| (symbol, Vec::new(), None))
                .collect();
            while let Some((symbol, mut path, parent)) = pending.pop() {
                path.push(symbol.name);
                let index = nodes.len();
                nodes.push(Candidate {
                    name: path.join("::"),
                    path: path.clone(),
                    kind: query::kind_name(symbol.kind),
                    range: query::span_lines(symbol.range),
                    extent: symbol.range,
                    selection: symbol.selection_range,
                    parent,
                });
                pending.extend(
                    symbol
                        .children
                        .into_iter()
                        .rev()
                        .map(|child| (child, path.clone(), Some(index))),
                );
            }
        }
    }
    Ok(nodes)
}

fn verdict(anchor: Anchor, path: Option<PathBuf>, status: Status) -> Verdict {
    Verdict {
        line: anchor.line,
        text: anchor.text,
        status,
        path,
        symbol: anchor.symbol,
        hint: anchor.hint,
        range: None,
        candidates: Vec::new(),
        files: Vec::new(),
        detail: String::new(),
    }
}

fn matches_segment(name: &str, segment: &str) -> bool {
    let name = query::without_generics(name);
    name == segment || name.split_whitespace().next_back() == Some(segment)
}

fn matches_chain(node: &Candidate, chain: &[String]) -> bool {
    let Some((last, ancestors)) = node.path.split_last() else {
        return false;
    };
    let Some((wanted, qualifiers)) = chain.split_last() else {
        return false;
    };
    if !matches_segment(last, wanted) {
        return false;
    }
    let mut ancestors = ancestors.iter();
    qualifiers
        .iter()
        .all(|qualifier| ancestors.any(|name| matches_segment(name, qualifier)))
}

fn check_symbol(anchor: Anchor, path: PathBuf, nodes: &[Candidate]) -> Verdict {
    // Symbol checks are only called for extracted nonempty chains.
    let chain = anchor.symbol.as_ref().expect("symbol anchor has a chain");
    let hits: Vec<_> = symbol_hits(nodes, chain).into_iter().cloned().collect();
    let found = hits
        .iter()
        .find(|node| anchor.hint.is_none_or(|hint| overlaps(node, hint)));
    if let Some(found) = found {
        let range = found.range;
        let mut result = verdict(anchor, Some(path), Status::Ok);
        result.range = Some(range);
        return result;
    }
    let status = if hits.is_empty() {
        Status::MissingSymbol
    } else {
        Status::LineOutside
    };
    let (candidates, detail) = candidate_details(nodes, chain, hits);
    let mut result = verdict(anchor, Some(path), status);
    result.detail = detail;
    result.candidates = candidates;
    result
}

fn candidate_details(
    nodes: &[Candidate],
    chain: &[String],
    mut candidates: Vec<Candidate>,
) -> (Vec<Candidate>, String) {
    if candidates.is_empty() {
        let last = chain.last().expect("extracted symbol chain is nonempty");
        let mut ranked: Vec<_> = nodes
            .iter()
            .filter_map(|node| {
                let leaf = query::without_generics(node.path.last()?);
                let leaf = leaf.as_str();
                let rank = if matches_segment(leaf, last) {
                    0
                } else if leaf.contains(last) {
                    1
                } else if last.contains(leaf)
                    || chain.iter().any(|segment| matches_segment(leaf, segment))
                {
                    2
                } else {
                    return None;
                };
                Some((rank, node))
            })
            .collect();
        ranked.sort_by_key(|(rank, _)| *rank);
        candidates = ranked
            .into_iter()
            .take(3)
            .map(|(_, node)| node.clone())
            .collect();
    }
    let detail = if candidates.is_empty() {
        "no matching outline symbol".into()
    } else {
        candidates
            .iter()
            .map(|node| {
                format!(
                    "{} {} is at {}-{}",
                    node.kind, node.name, node.range[0], node.range[1]
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    (candidates, detail)
}

fn symbol_hits<'a>(nodes: &'a [Candidate], chain: &[String]) -> Vec<&'a Candidate> {
    nodes
        .iter()
        .filter(|node| matches_chain(node, chain))
        .collect()
}

fn overlaps(node: &Candidate, [start, end]: [u32; 2]) -> bool {
    start > 0
        && start <= end
        && start <= node.range[1].saturating_add(LINE_SLACK)
        && end >= node.range[0].saturating_sub(LINE_SLACK)
}

fn hinted<'a>(hits: &[&'a Candidate], hint: Option<[u32; 2]>) -> Vec<&'a Candidate> {
    hits.iter()
        .copied()
        .filter(|node| hint.is_some_and(|hint| overlaps(node, hint)))
        .collect()
}

fn check_lines(anchor: Anchor, path: PathBuf, lines: usize) -> Verdict {
    let valid = anchor
        .hint
        .is_some_and(|[start, end]| start > 0 && start <= end && end as usize <= lines);
    let mut result = verdict(
        anchor,
        Some(path),
        if valid {
            Status::Ok
        } else {
            Status::LineOutside
        },
    );
    result.detail = format!("file has {lines} lines");
    result
}

#[cfg(test)]
mod tests;
