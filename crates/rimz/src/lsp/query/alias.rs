//! Uses of a target spelled by a `use … as <alias>` import. rust-analyzer's references and
//! incoming calls search the target's own name, so they never list an alias-spelled token
//! (rust-lang/rust-analyzer#16850, rust-lang/rust-analyzer#14079); this pass finds those tokens
//! itself and keeps each one `textDocument/definition` resolves to the target.

use super::*;

/// One alias-spelled use of the target.
struct AliasUse {
    location: Location,
    alias: String,
}

/// An alias a `use` declares, at the alias's own token.
struct Declared {
    uri: String,
    position: Position,
    alias: String,
    public: bool,
}

/// A token spelled like an alias, a use once definition resolves it to the target.
struct Candidate {
    uri: String,
    range: Range,
    end: usize,
    alias: String,
}

/// The server's `references` answer with each alias-spelled use of the target at `uri` and
/// `position` added, as a Location that also carries the `alias` it is spelled with.
pub(super) fn references(
    entry: &Entry,
    uri: &str,
    position: Position,
    mut result: Value,
) -> std::result::Result<Value, QueryErr> {
    let uses = uses(entry, uri, position, &locations(result.clone())?)?;
    if let Value::Array(entries) = &mut result {
        entries.extend(uses.into_iter().map(|found| {
            json!({"uri": found.location.uri, "range": found.location.range, "alias": found.alias})
        }));
    }
    Ok(result)
}

/// The incoming calls the server's answer misses: one per function and alias whose body names
/// the target through that alias, its `from` item built from the file's outline node, its
/// `fromRanges` the alias tokens in it, and an `alias` key. A use outside every function (a
/// `use` line) makes no call, as in the server's own answer.
pub(super) fn incoming_calls(
    entry: &Entry,
    uri: &str,
    position: Position,
) -> std::result::Result<Vec<Value>, QueryErr> {
    let references = request(
        entry,
        "textDocument/references",
        json!({
            "textDocument": {"uri": uri},
            "position": position,
            "context": {"includeDeclaration": true},
        }),
    )?;
    let uses = uses(entry, uri, position, &locations(references)?)?;
    let files: BTreeSet<_> = uses
        .iter()
        .map(|found| found.location.uri.as_str())
        .collect();
    let outlines: BTreeMap<_, _> = bounded_map(&files.into_iter().collect::<Vec<_>>(), |uri| {
        let outline = request(
            entry,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        )?;
        let nodes = match serde_json::from_value(outline) {
            Ok(Symbols::Tree(nodes)) => nodes,
            _ => Vec::new(),
        };
        Ok::<_, QueryErr>((*uri, nodes))
    })?
    .into_iter()
    .collect();
    let mut calls = BTreeMap::new();
    for found in &uses {
        let uri = found.location.uri.as_str();
        let Some(function) = outlines
            .get(uri)
            .and_then(|nodes| enclosing_function(nodes, found.location.range.start))
        else {
            continue;
        };
        calls
            .entry((
                DocumentKey::new(uri),
                function.selection_range.start,
                &found.alias,
            ))
            .or_insert_with(|| {
                let from = json!({
                    "name": function.name,
                    "kind": function.kind,
                    "uri": uri,
                    "range": function.range,
                    "selectionRange": function.selection_range,
                });
                (from, Vec::new())
            })
            .1
            .push(found.location.range);
    }
    Ok(calls
        .into_iter()
        .map(|((_, _, alias), (from, ranges))| {
            json!({"from": from, "fromRanges": ranges, "alias": alias})
        })
        .collect())
}

/// The innermost function or method whose range holds `position`.
fn enclosing_function(nodes: &[DocumentSymbol], position: Position) -> Option<&DocumentSymbol> {
    let node = nodes
        .iter()
        .find(|node| node.range.start <= position && position < node.range.end)?;
    enclosing_function(&node.children, position).or(matches!(node.kind, 6 | 12).then_some(node))
}

/// Each alias-spelled use of the target at `uri` and `position`, found from the server's
/// `references` to it. A reference followed by `as <alias>` declares an alias, searched in its
/// declaring file and the Rust files under the directory `scope` names, and followed through the
/// aliases its own uses declare. A token counts only where definition resolves it to the target's
/// own definition; a declaring token and a position the server returned are never listed.
fn uses(
    entry: &Entry,
    uri: &str,
    position: Position,
    references: &[Location],
) -> std::result::Result<Vec<AliasUse>, QueryErr> {
    let mut sources = Sources::default();
    let mut declared: Vec<_> = references
        .iter()
        .filter_map(|reference| {
            let source = sources.get(&reference.uri)?;
            let end = source.offset(reference.range.end)?;
            declared_at(&reference.uri, source, end)
        })
        .collect();
    if declared.is_empty() {
        return Ok(Vec::new());
    }
    let definitions = |uri: &str, position: Position| {
        let params = json!({"textDocument": {"uri": uri}, "position": position});
        Ok::<_, QueryErr>(
            match reply(entry, "textDocument/definition", params, 110_000)? {
                Ok(result) => Ok(locations(result)?
                    .into_iter()
                    .map(|location| (DocumentKey::new(&location.uri), location.range.start))
                    .collect::<Vec<_>>()),
                Err(error) => Err(error),
            },
        )
    };
    let targets: BTreeSet<_> = definitions(uri, position)?
        .map_err(|error| LspErr::Protocol(error.to_string()))?
        .into_iter()
        .collect();
    // A server error reply (a file outside every crate it loads) leaves the token unconfirmed.
    let resolves = |uri: &str, position: Position| {
        Ok::<_, QueryErr>(definitions(uri, position)?.is_ok_and(|definitions| {
            definitions
                .iter()
                .any(|definition| targets.contains(definition))
        }))
    };
    let mut known: BTreeSet<_> = references
        .iter()
        .map(|reference| (DocumentKey::new(&reference.uri), reference.range.start))
        .collect();
    let mut searched = BTreeSet::new();
    let mut walked = BTreeMap::new();
    let mut uses = Vec::new();
    while !declared.is_empty() {
        known.extend(
            declared
                .iter()
                .map(|alias| (DocumentKey::new(&alias.uri), alias.position)),
        );
        let confirmed = bounded_map(&declared, |alias| resolves(&alias.uri, alias.position))?;
        let mut candidates = Vec::new();
        for alias in declared
            .iter()
            .zip(confirmed)
            .filter_map(|(alias, ok)| ok.then_some(alias))
        {
            let directory = scope(&entry.root, alias)?;
            if !searched.insert((alias.alias.clone(), directory.clone())) {
                continue;
            }
            let files = walked.entry(directory).or_insert_with_key(|directory| {
                let mut files = Vec::new();
                rust_files(directory, &mut files);
                files
            });
            let declaring = DocumentKey::new(&alias.uri);
            let files = std::iter::once(alias.uri.clone()).chain(
                files
                    .iter()
                    .filter_map(|file| url::Url::from_file_path(file).ok())
                    .map(|uri| uri.to_string())
                    .filter(|uri| DocumentKey::new(uri) != declaring),
            );
            for file in files {
                let Some(source) = sources.get(&file) else {
                    continue;
                };
                for at in identifier_offsets(&source.text, &alias.alias) {
                    let start = source.position(at);
                    if known.insert((DocumentKey::new(&file), start)) {
                        let end = at + alias.alias.len();
                        candidates.push(Candidate {
                            uri: file.clone(),
                            range: Range {
                                start,
                                end: source.position(end),
                            },
                            end,
                            alias: alias.alias.clone(),
                        });
                    }
                }
            }
        }
        let confirmed = bounded_map(&candidates, |candidate| {
            resolves(&candidate.uri, candidate.range.start)
        })?;
        declared = Vec::new();
        for (candidate, _) in candidates.into_iter().zip(confirmed).filter(|(_, ok)| *ok) {
            if let Some(source) = sources.get(&candidate.uri) {
                declared.extend(declared_at(&candidate.uri, source, candidate.end));
                if is_declared_name(&source.text, candidate.end - candidate.alias.len()) {
                    continue;
                }
            }
            uses.push(AliasUse {
                location: Location {
                    uri: candidate.uri,
                    range: candidate.range,
                },
                alias: candidate.alias,
            });
        }
    }
    uses.sort_by(|a, b| {
        (DocumentKey::new(&a.location.uri), a.location.range.start)
            .cmp(&(DocumentKey::new(&b.location.uri), b.location.range.start))
    });
    Ok(uses)
}

fn declared_at(uri: &str, source: &Source, end: usize) -> Option<Declared> {
    let declaration = declaration(&source.text, end)?;
    Some(Declared {
        uri: uri.to_owned(),
        position: source.position(declaration.offset),
        alias: declaration.alias.to_owned(),
        public: declaration.public,
    })
}

/// The directory an alias `declared` is searched under, besides its declaring file: a private
/// alias's declaring file's directory, a superset of the declaring module and its descendants that
/// also holds the sibling files a module mounts with `#[path]` (`use super::*` reaches the alias
/// there); `root` for a `pub` alias.
fn scope(root: &Path, declared: &Declared) -> std::result::Result<PathBuf, QueryErr> {
    if declared.public {
        return Ok(root.to_owned());
    }
    let path = file_path(&declared.uri)?;
    Ok(path.parent().unwrap_or(Path::new("/")).to_owned())
}

/// Every `.rs` file under `directory`, skipping `target/` and dot-directories at any depth and
/// following no symlinked directory; an unreadable directory contributes none.
fn rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let (Ok(kind), name) = (entry.file_type(), entry.file_name()) else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if name != "target" && !name.as_encoded_bytes().starts_with(b".") {
                rust_files(&path, files);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

/// Saved file texts by document, read once; an unreadable file is `None`.
#[derive(Default)]
struct Sources(BTreeMap<DocumentKey, Option<Source>>);

impl Sources {
    fn get(&mut self, uri: &str) -> Option<&Source> {
        self.0
            .entry(DocumentKey::new(uri))
            .or_insert_with(|| {
                let text = std::fs::read_to_string(file_path(uri).ok()?).ok()?;
                let lines = std::iter::once(0)
                    .chain(text.match_indices('\n').map(|(at, _)| at + 1))
                    .collect();
                Some(Source { text, lines })
            })
            .as_ref()
    }
}

/// A file's text, with LSP positions (UTF-16 columns) mapped to byte offsets and back.
struct Source {
    text: String,
    lines: Vec<usize>,
}

impl Source {
    fn offset(&self, position: Position) -> Option<usize> {
        let start = *self.lines.get(position.line as usize)?;
        let line = &self.text[start..];
        let line = &line[..line.find('\n').unwrap_or(line.len())];
        let mut units = 0;
        for (at, character) in line.char_indices() {
            if units >= position.character {
                return Some(start + at);
            }
            units += character.len_utf16() as u32;
        }
        (units >= position.character).then_some(start + line.len())
    }

    fn position(&self, offset: usize) -> Position {
        let line = self.lines.partition_point(|start| *start <= offset) - 1;
        Position {
            line: line as u32,
            character: self.text[self.lines[line]..offset].encode_utf16().count() as u32,
        }
    }
}

/// The alias a `use … as alias` declares, at byte `offset` of its file, and whether the `use`
/// carries a `pub` visibility of any reach.
#[derive(Debug, PartialEq, Eq)]
struct Declaration<'a> {
    alias: &'a str,
    offset: usize,
    public: bool,
}

/// The alias declared by the reference that ends at byte `end` of `text`: a path inside a `use`
/// followed by `as <alias>`. A cast (`TARGET as u32`, outside every `use`) and `as _` declare
/// none.
fn declaration(text: &str, end: usize) -> Option<Declaration<'_>> {
    let name = text[end..].trim_start().strip_prefix("as")?;
    if !name.starts_with(char::is_whitespace) {
        return None;
    }
    let name = name.trim_start();
    let alias = &name[..name.find(|c| !identifier_char(c)).unwrap_or(name.len())];
    if alias == "_" || !is_identifier(alias) {
        return None;
    }
    let statement = &text[..end];
    let statement = &statement[statement.rfind(';').map_or(0, |at| at + 1)..];
    let keyword = identifier_offsets(statement, "use").last()?;
    Some(Declaration {
        alias,
        offset: text.len() - name.len(),
        public: is_public(&statement[..keyword]),
    })
}

/// Whether the token at byte `start` of `text` is the name a `use … as` declares, which is an
/// alias's own declaring token, not a use of it.
fn is_declared_name(text: &str, start: usize) -> bool {
    let Some(path) = text[..start].trim_end().strip_suffix("as") else {
        return false;
    };
    if !path.ends_with(char::is_whitespace) {
        return false;
    }
    let statement = &path[path.rfind(';').map_or(0, |at| at + 1)..];
    identifier_offsets(statement, "use").next().is_some()
}

/// Whether the text before a `use` keyword ends in `pub`, `pub(crate)`, or another `pub(…)`.
fn is_public(before: &str) -> bool {
    let before = before.trim_end();
    let before = match before.strip_suffix(')') {
        Some(rest) => match rest.rfind('(') {
            Some(open) => rest[..open].trim_end(),
            None => return false,
        },
        None => before,
    };
    before
        .strip_suffix("pub")
        .is_some_and(|rest| !rest.chars().next_back().is_some_and(identifier_char))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_use_path_followed_by_as_declares_an_alias_and_a_cast_or_underscore_none() {
        let cases = [
            "use crate::m::target as renamed;",
            "pub use m::target as reexported;",
            "#[doc(hidden)]\npub(in crate::a) use m::{other, target as t};",
            "pub(crate) use m::target\n    as wrapped;",
            "fn f() {}\nuse m::target as local;",
            "use m::target as _;",
            "use m::target;",
            "use a::b;\nfn f() -> usize {\n    target as usize\n}",
            "use m::target as;",
            "spub use m::target as hidden;",
        ];
        let found: Vec<_> = cases
            .iter()
            .map(|text| {
                let end = text.find("target").unwrap() + "target".len();
                declaration(text, end).map(|found| {
                    (
                        found.alias,
                        found.public,
                        text[found.offset..].starts_with(found.alias),
                    )
                })
            })
            .collect();
        assert_eq!(
            found,
            [
                Some(("renamed", false, true)),
                Some(("reexported", true, true)),
                Some(("t", true, true)),
                Some(("wrapped", true, true)),
                Some(("local", false, true)),
                None,
                None,
                None,
                None,
                Some(("hidden", false, true)),
            ]
        );
    }
}
