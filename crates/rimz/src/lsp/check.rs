//! Validate Markdown anchors against checkout files and language-server outlines.

use super::{LspErr, registry};
use super::{protocol::Symbols, query};
use pulldown_cmark::{Event, Parser};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const LINE_SLACK: u32 = 3;

pub fn run(
    notes: &Path,
    root: &Path,
    entries: &[registry::Entry],
    servers: &BTreeMap<String, crate::config::LspServerConfig>,
) -> Result<Report, query::QueryErr> {
    let source = std::fs::read_to_string(notes)
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("cannot read {}: {error}", notes.display()),
            )
        })
        .map_err(LspErr::from)?;
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
    let mut outlines = BTreeMap::new();
    let mut lengths = BTreeMap::new();
    let mut verdicts = Vec::new();
    for anchor in extract(&source) {
        let matches = resolve(root, &files, &anchor.path);
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
            verdicts.push(result);
            continue;
        }
        let path = &matches[0];
        if anchor.symbol.is_none() {
            let lines = match lengths.entry(path.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let bytes = std::fs::read(root.join(path)).map_err(LspErr::from)?;
                    let lines = bytes.iter().filter(|byte| **byte == b'\n').count()
                        + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"));
                    *entry.insert(lines)
                }
            };
            verdicts.push(check_lines(anchor, path.clone(), lines));
            continue;
        }
        let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        if !servers
            .values()
            .any(|config| config.extensions.iter().any(|ext| ext == extension))
        {
            let mut result = verdict(anchor, Some(path.clone()), Status::Unchecked);
            result.detail = format!("no server configured for .{extension}");
            verdicts.push(result);
            continue;
        }
        let nodes = match outlines.entry(path.clone()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let server = query::select(root, entries.to_vec(), servers, None, Some(path))?;
                let query::Output::Answer { result, .. } = query::execute(
                    &server,
                    query::Verb::Symbols,
                    &query::Target::File(path.clone()),
                )?
                else {
                    return Err(LspErr::Protocol(
                        "document symbols returned a lookup outcome".into(),
                    )
                    .into());
                };
                entry.insert(outline(result)?)
            }
        };
        verdicts.push(check_symbol(anchor, path.clone(), nodes));
    }
    Ok(Report::new(notes, root, verdicts))
}

#[derive(Debug, PartialEq, Eq)]
struct Anchor {
    line: usize,
    text: String,
    path: String,
    symbol: Option<Vec<String>>,
    hint: Option<[u32; 2]>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct Candidate {
    name: String,
    #[serde(skip)]
    path: Vec<String>,
    kind: &'static str,
    range: [u32; 2],
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
}

#[derive(Debug, Default, Serialize)]
struct Summary {
    anchors: usize,
    ok: usize,
    failed: usize,
    unchecked: usize,
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
                _ => summary.failed += 1,
            }
        }
        Self {
            notes: notes.into(),
            checkout: checkout.into(),
            anchors,
            summary,
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
        for anchor in &self.anchors {
            let status = match anchor.status {
                Status::Ok => continue,
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
        text.push_str(&format!(
            "{} anchors in {}: {} ok, {} failed, {} unchecked\n",
            self.summary.anchors,
            self.notes.display(),
            self.summary.ok,
            self.summary.failed,
            self.summary.unchecked
        ));
        Ok(text)
    }
}

fn extract(notes: &str) -> Vec<Anchor> {
    let mut events = Parser::new(notes).into_offset_iter().peekable();
    let mut anchors = Vec::new();
    while let Some((event, offset)) = events.next() {
        let Event::Code(code) = event else { continue };
        let Some((path, rest)) = code.split_once(':') else {
            continue;
        };
        let Some(extension) = Path::new(path).extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if path.chars().any(char::is_whitespace)
            || !extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
            || !extension.bytes().any(|byte| byte.is_ascii_alphabetic())
        {
            continue;
        }
        let mut text = code.to_string();
        let (symbol, mut hint) = if let Some(raw) = rest.strip_prefix(':') {
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
                                    && raw[i + 1..].starts_with(|c: char| {
                                        c.is_ascii_digit() || c == '~'
                                    }))) =>
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
                continue;
            }
            let tail = &raw[end..];
            let hint = parse_hint(tail, false)
                .or_else(|| {
                    tail.strip_prefix('(')
                        .and_then(|tail| tail.split_once(')'))
                        .and_then(|(_, tail)| parse_hint(tail, false))
                })
                .map(|(hint, _)| hint);
            (Some(chain), hint)
        } else {
            let Some((hint, _)) = parse_hint(rest, true) else {
                continue;
            };
            (None, Some(hint))
        };
        if hint.is_none()
            && let Some((Event::Text(following), _)) = events.peek()
            && let Some((found, end)) = parse_hint(following, false)
        {
            hint = Some(found);
            text.push_str(&following[..end]);
        }
        anchors.push(Anchor {
            line: notes[..offset.start]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1,
            text,
            path: path.into(),
            symbol,
            hint,
        });
    }
    anchors
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
    let path = Path::new(path);
    let path = if path.is_absolute() {
        let Ok(path) = path.strip_prefix(root) else {
            return Vec::new();
        };
        path
    } else {
        path
    };
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Vec::new();
    }
    let path: PathBuf = path
        .components()
        .filter(|part| !matches!(part, std::path::Component::CurDir))
        .collect();
    if files.contains(&path) {
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
                    range: [
                        symbol.location.range.start.line + 1,
                        symbol.location.range.end.line + 1,
                    ],
                });
            }
        }
        Symbols::Tree(symbols) => {
            let mut pending: Vec<_> = symbols
                .into_iter()
                .rev()
                .map(|symbol| (symbol, Vec::new()))
                .collect();
            while let Some((symbol, mut path)) = pending.pop() {
                path.push(symbol.name);
                nodes.push(Candidate {
                    name: path.join("::"),
                    path: path.clone(),
                    kind: query::kind_name(symbol.kind),
                    range: [symbol.range.start.line + 1, symbol.range.end.line + 1],
                });
                pending.extend(
                    symbol
                        .children
                        .into_iter()
                        .rev()
                        .map(|child| (child, path.clone())),
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
    let hits: Vec<_> = nodes
        .iter()
        .filter(|node| matches_chain(node, chain))
        .cloned()
        .collect();
    let found = hits.iter().find(|node| {
        anchor.hint.is_none_or(|[start, end]| {
            start > 0
                && start <= end
                && start <= node.range[1].saturating_add(LINE_SLACK)
                && end >= node.range[0].saturating_sub(LINE_SLACK)
        })
    });
    if let Some(found) = found {
        let range = found.range;
        let mut result = verdict(anchor, Some(path), Status::Ok);
        result.range = Some(range);
        return result;
    }
    let (status, candidates) = if hits.is_empty() {
        let last = chain.last().expect("extracted symbol chain is nonempty");
        let mut candidates: Vec<_> = nodes
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
        candidates.sort_by_key(|(rank, _)| *rank);
        (
            Status::MissingSymbol,
            candidates
                .into_iter()
                .take(3)
                .map(|(_, node)| node.clone())
                .collect(),
        )
    } else {
        (Status::LineOutside, hits)
    };
    let mut result = verdict(anchor, Some(path), status);
    result.detail = if candidates.is_empty() {
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
    result.candidates = candidates;
    result
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
