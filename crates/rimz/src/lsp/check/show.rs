//! Batched source reads using the notes anchor grammar, symbol names, and outline.

use super::super::protocol::{Position, Range};
use super::*;

#[expect(
    clippy::too_many_arguments,
    reason = "the CLI's query context and flags arrive as separate borrows"
)]
pub fn show(
    arguments: &[String],
    full: bool,
    root: &Path,
    folder: &Path,
    entries: &[registry::Entry],
    servers: &BTreeMap<String, crate::config::LspServerConfig>,
    server: Option<&str>,
    usage: Option<&super::super::usage::Log>,
) -> super::super::Result<Vec<(String, i32)>> {
    let parsed = parse_arguments(arguments)?;
    let usage = usage.map(|log| (log, "show"));
    let mut context = match Context::new(root, entries, servers, server, usage) {
        Ok(context) => context,
        Err(error) => {
            return Ok(arguments
                .iter()
                .map(|argument| failure(argument, "error", &error.to_string(), error.exit_code()))
                .collect());
        }
    };
    Ok(read_arguments(
        &mut context,
        arguments,
        parsed,
        folder,
        full,
        |context, name| context.definitions(name),
    ))
}

enum Argument {
    Read(Anchor, Option<Position>),
    Name(String),
}

fn parse_arguments(arguments: &[String]) -> super::super::Result<Vec<Argument>> {
    arguments
        .iter()
        .map(|argument| match parse_argument(argument) {
            Ok((anchor, position)) => Ok(Argument::Read(anchor, position)),
            Err(_) if query::is_qualified_name(argument) => Ok(Argument::Name(argument.clone())),
            Err(error) => Err(error),
        })
        .collect()
}

fn read_arguments(
    context: &mut Context<'_>,
    arguments: &[String],
    parsed: Vec<Argument>,
    folder: &Path,
    full: bool,
    mut define: impl FnMut(&mut Context<'_>, &str) -> Result<query::Output, query::QueryErr>,
) -> Vec<(String, i32)> {
    let mut sources = BTreeMap::new();
    let mut blocks = Vec::new();
    for (argument, parsed) in arguments.iter().zip(parsed) {
        match parsed {
            Argument::Read(anchor, position) => blocks.push(read(
                context,
                &mut sources,
                argument,
                (anchor, position),
                folder,
                full,
            )),
            Argument::Name(name) => {
                let output = define(context, &name);
                blocks.extend(read_definitions(context, &mut sources, &name, output, full));
            }
        }
    }
    blocks
}

/// One block per definition location, each read as `show <path:line:col>` reads it at the
/// checkout root.
fn read_definitions(
    context: &mut Context<'_>,
    sources: &mut BTreeMap<PathBuf, String>,
    name: &str,
    output: Result<query::Output, query::QueryErr>,
    full: bool,
) -> Vec<(String, i32)> {
    let root = context.checkout;
    let result = match output {
        Ok(query::Output::Answer { result, .. }) => result,
        Ok(output) => {
            return vec![match query::render_outcome(root, &output, false) {
                Ok(text) => (text, output.exit_code()),
                Err(error) => failure(name, "error", &error.to_string(), 1),
            }];
        }
        Err(error) => {
            return vec![failure(
                name,
                "error",
                &error.to_string(),
                error.exit_code(),
            )];
        }
    };
    let targets = match query::definition_targets(root, result) {
        Ok(targets) if targets.is_empty() => {
            return vec![failure(name, "missing-symbol", "no definition found", 5)];
        }
        Ok(targets) => targets,
        Err(error) => return vec![failure(name, "error", &error.to_string(), 1)],
    };
    targets
        .iter()
        .map(|target| match parse_argument(target) {
            // A printed target is checkout-relative or absolute, never relative to the caller's
            // folder, so it is read from the checkout root.
            Ok(parsed) => read(context, sources, target, parsed, Path::new(""), full),
            Err(error) => failure(target, "error", &error.to_string(), 1),
        })
        .collect()
}

fn read(
    context: &mut Context<'_>,
    sources: &mut BTreeMap<PathBuf, String>,
    argument: &str,
    (anchor, position): (Anchor, Option<Position>),
    folder: &Path,
    full: bool,
) -> (String, i32) {
    let checkout = context.checkout;
    let line_target = is_line_target(&anchor);
    let needs_outline = anchor.symbol.is_some() || position.is_some() || line_target;
    let (anchor, path) = match resolve_argument(context, anchor, folder) {
        Ok(resolved) => resolved,
        Err(verdict) => {
            let (status, code) = match verdict.status {
                Status::External => ("external", 5),
                Status::AmbiguousPath => ("ambiguous-path", 6),
                _ => ("missing-path", 5),
            };
            return failure(argument, status, &verdict.detail, code);
        }
    };
    let outline = if needs_outline {
        match context.outline(&path) {
            Ok(Some(file)) => Some(file),
            Ok(None) | Err(_) if line_target => None,
            Ok(None) => return failure(argument, "unchecked", &unchecked_detail(&path), 3),
            Err(error) => {
                return failure(argument, "error", &error.to_string(), error.exit_code());
            }
        }
    } else {
        None
    };
    let (nodes, dirty) = if let Some(file) = outline {
        (file.nodes.as_slice(), file.dirty)
    } else {
        match context.is_dirty(&path) {
            Ok(dirty) => (&[][..], dirty),
            Err(error) => return failure(argument, "error", &error.to_string(), 1),
        }
    };
    let source = match sources.entry(path.clone()) {
        std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::btree_map::Entry::Vacant(entry) => {
            match std::fs::read_to_string(checkout.join(&path)) {
                Ok(source) => entry.insert(source),
                Err(error) => return failure(argument, "error", &error.to_string(), 1),
            }
        }
    };
    render_item(&anchor, position, &path, nodes, source, dirty, full)
}

fn resolve_argument(
    context: &Context<'_>,
    anchor: Anchor,
    folder: &Path,
) -> Result<(Anchor, PathBuf), Box<Verdict>> {
    if anchor.qualifier.is_some() || Path::new(&anchor.path).is_absolute() {
        return context
            .resolve(anchor)
            .map(|(anchor, path, _)| (anchor, path));
    }
    let error = match query::typed_path(context.checkout, folder, Path::new(&anchor.path)) {
        Ok(path) => return Ok((anchor, path)),
        Err(error) => error,
    };
    let written = anchor.path.clone();
    context
        .resolve(anchor)
        .map(|(anchor, path, _)| (anchor, path))
        .map_err(|mut verdict| {
            if verdict.status == Status::MissingPath {
                verdict.detail = format!("{error}; no checkout file ends with {written}");
            }
            verdict
        })
}

fn parse_argument(argument: &str) -> super::super::Result<(Anchor, Option<Position>)> {
    if let Ok(query::Target::Position { path, position }) =
        query::Target::parse(query::Verb::Def, argument)
    {
        return Ok((
            Anchor {
                qualifier: None,
                line: 1,
                text: argument.into(),
                path: path.display().to_string(),
                symbol: None,
                hint: None,
                hint_text: None,
                hint_source: None,
                path_source: None,
                span_end: None,
            },
            Some(position),
        ));
    }
    let mut anchors = extract(&format!("`{argument}`"));
    if anchors.len() == 1 && anchors[0].text == argument {
        return Ok((anchors.remove(0), None));
    }
    Err(LspErr::Protocol(format!(
        "invalid anchor {argument}; use path::Symbol, a symbol name, path:line, path:line:col, \
         or path:start-end"
    )))
}

fn is_line_target(anchor: &Anchor) -> bool {
    anchor.symbol.is_none()
        && anchor.text.rsplit_once(':').is_some_and(|(path, line)| {
            let line = line.strip_prefix('~').unwrap_or(line);
            path.ends_with(&anchor.path)
                && !line.is_empty()
                && line.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn failure(argument: &str, status: &str, detail: &str, code: i32) -> (String, i32) {
    (format!("{argument}  {status}  {detail}\n"), code)
}

fn contains(range: Range, position: Position) -> bool {
    range.start <= position && position < range.end
}

fn render_item(
    anchor: &Anchor,
    position: Option<Position>,
    path: &Path,
    nodes: &[Candidate],
    source: &str,
    dirty: bool,
    full: bool,
) -> (String, i32) {
    let argument = &anchor.text;
    let selected = if let Some(position) = position {
        nodes
            .iter()
            .filter(|node| contains(node.selection, position))
            .min_by_key(|node| (std::cmp::Reverse(node.extent.start), node.extent.end))
            .or_else(|| {
                nodes
                    .iter()
                    .filter(|node| contains(node.extent, position))
                    .min_by_key(|node| (std::cmp::Reverse(node.extent.start), node.extent.end))
            })
    } else if let Some(chain) = &anchor.symbol {
        let hits = symbol_hits(nodes, chain);
        if hits.is_empty() {
            let (_, detail) = candidate_details(nodes, chain, Vec::new());
            return failure(argument, "missing-symbol", &detail, 5);
        }
        let overlapping = hinted(&hits, anchor.hint);
        let selected = if overlapping.is_empty() {
            hits
        } else {
            overlapping
        };
        let mut text = String::new();
        let mut exit = 0;
        for node in selected {
            let (block, code) =
                render_selected(anchor, Some(node), path, nodes, source, dirty, full);
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&block);
            exit = exit.max(code);
        }
        return (text, exit);
    } else if is_line_target(anchor) {
        anchor
            .hint
            .and_then(|hint| hint.lines[0].checked_sub(1))
            .filter(|line| (*line as usize) < source.lines().count())
            .and_then(|line| {
                nodes
                    .iter()
                    .filter(|node| {
                        node.extent.start.line <= line
                            && (line < node.extent.end.line
                                || (line == node.extent.end.line && node.extent.end.character > 0))
                    })
                    .min_by_key(|node| (std::cmp::Reverse(node.extent.start), node.extent.end))
            })
    } else {
        None
    };
    if (position.is_some() || anchor.symbol.is_some()) && selected.is_none() {
        return failure(argument, "missing-symbol", "no matching outline symbol", 5);
    }
    render_selected(anchor, selected, path, nodes, source, dirty, full)
}

fn render_selected(
    anchor: &Anchor,
    selected: Option<&Candidate>,
    path: &Path,
    nodes: &[Candidate],
    source: &str,
    dirty: bool,
    full: bool,
) -> (String, i32) {
    let argument = &anchor.text;
    let [start, end] = selected
        .map(|node| node.range)
        .or(anchor.hint.map(|hint| hint.lines))
        .expect("line anchor has a hint");
    let lines: Vec<_> = source.lines().collect();
    if start == 0 || start > end || start as usize > lines.len() {
        return failure(
            argument,
            "line-outside",
            &format!("file has {} lines", lines.len()),
            5,
        );
    }
    let end = end.min(lines.len() as u32);
    let mut text = format!(
        "{}:{start}-{end}{}",
        path.display(),
        if dirty { "  (unsaved in editor)" } else { "" }
    );
    let index =
        selected.and_then(|selected| nodes.iter().position(|node| std::ptr::eq(node, selected)));
    let children: Vec<_> = nodes
        .iter()
        .filter(|node| index.is_some() && node.parent == index)
        .collect();
    if !full && end - start + 1 > 200 && !children.is_empty() {
        text.push_str(&format!(
            "  (outline: {} lines; --full prints the body)\n",
            end - start + 1
        ));
        for child in children {
            let line = child.selection.start.line as usize;
            let Some(source) = lines.get(line) else {
                continue;
            };
            text.push_str(&format!(
                "{:>6}\t{} ({}-{})\n",
                line + 1,
                query::snippet(source),
                child.range[0],
                child.range[1]
            ));
        }
    } else {
        text.push('\n');
        for line in start..=end {
            text.push_str(&format!("{line:>6}\t{}\n", lines[line as usize - 1]));
        }
    }
    (text, 0)
}

#[cfg(test)]
mod tests;
