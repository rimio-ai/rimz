//! Batched source reads using the notes anchor grammar and outline.

use super::super::protocol::{Position, Range};
use super::*;

pub fn show(
    arguments: &[String],
    full: bool,
    root: &Path,
    entries: &[registry::Entry],
    servers: &BTreeMap<String, crate::config::LspServerConfig>,
) -> super::super::Result<Vec<(String, i32)>> {
    let anchors = arguments
        .iter()
        .map(|argument| parse_argument(argument))
        .collect::<super::super::Result<Vec<_>>>()?;
    let mut context = match Context::new(root, entries, servers) {
        Ok(context) => context,
        Err(error) => {
            return Ok(arguments
                .iter()
                .map(|argument| failure(argument, "error", &error.to_string(), error.exit_code()))
                .collect());
        }
    };
    let mut sources = BTreeMap::new();
    let mut blocks = Vec::new();
    for (argument, (anchor, position)) in arguments.iter().zip(anchors) {
        let needs_outline = anchor.symbol.is_some() || position.is_some();
        let (anchor, path, _) = match context.resolve(anchor) {
            Ok(resolved) => resolved,
            Err(verdict) => {
                let (status, code) = match verdict.status {
                    Status::External => ("external", 5),
                    Status::AmbiguousPath => ("ambiguous-path", 6),
                    _ => ("missing-path", 5),
                };
                blocks.push(failure(argument, status, &verdict.detail, code));
                continue;
            }
        };
        let (nodes, dirty) = if needs_outline {
            match context.outline(&path) {
                Ok(Some(file)) => (file.nodes.as_slice(), file.dirty),
                Ok(None) => {
                    blocks.push(failure(argument, "unchecked", &unchecked_detail(&path), 3));
                    continue;
                }
                Err(error) => {
                    blocks.push(failure(
                        argument,
                        "error",
                        &error.to_string(),
                        error.exit_code(),
                    ));
                    continue;
                }
            }
        } else {
            match context.is_dirty(&path) {
                Ok(dirty) => (&[][..], dirty),
                Err(error) => {
                    blocks.push(failure(argument, "error", &error.to_string(), 1));
                    continue;
                }
            }
        };
        let source = match sources.entry(path.clone()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                match std::fs::read_to_string(root.join(&path)) {
                    Ok(source) => entry.insert(source),
                    Err(error) => {
                        blocks.push(failure(argument, "error", &error.to_string(), 1));
                        continue;
                    }
                }
            }
        };
        blocks.push(render_item(
            &anchor, position, &path, nodes, source, dirty, full,
        ));
    }
    Ok(blocks)
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
        "invalid anchor {argument}; use path::Symbol, path:line:col, or path:start-end"
    )))
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
        .or(anchor.hint)
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
mod tests {
    use super::*;
    use serde_json::json;

    fn nodes() -> Vec<Candidate> {
        outline(json!([
            {"name":"Parent","kind":5,
             "range":{"start":{"line":0,"character":0},"end":{"line":204,"character":1}},
             "selectionRange":{"start":{"line":1,"character":3},"end":{"line":1,"character":9}},
             "children":[
                {"name":"child","kind":6,
                 "range":{"start":{"line":2,"character":0},"end":{"line":4,"character":1}},
                 "selectionRange":{"start":{"line":3,"character":3},"end":{"line":3,"character":8}}}
             ]},
            {"name":"child","kind":12,
             "range":{"start":{"line":207,"character":0},"end":{"line":208,"character":1}},
             "selectionRange":{"start":{"line":207,"character":3},"end":{"line":207,"character":8}}}
        ]))
        .unwrap()
    }

    fn source() -> String {
        (1..=210).map(|n| format!("  line {n}\n")).collect()
    }

    fn read(argument: &str, dirty: bool, full: bool) -> (String, i32) {
        let (anchor, position) = parse_argument(argument).unwrap();
        render_item(
            &anchor,
            position,
            Path::new("src/lib.rs"),
            &nodes(),
            &source(),
            dirty,
            full,
        )
    }

    #[test]
    fn show_symbol_positions_and_pasted_spans_read_the_same_body() {
        let expected = "src/lib.rs:3-5\n     3\t  line 3\n     4\t  line 4\n     5\t  line 5\n";
        for argument in [
            "src/lib.rs::Parent::child",
            "src/lib.rs::Parent::child ~100",
            "src/lib.rs:4:4",
            "src/lib.rs:5:1",
            "src/lib.rs:4:4 (3-5)",
            "src/lib.rs:3-5",
        ] {
            assert_eq!(
                read(argument, false, false),
                (expected.into(), 0),
                "{argument}"
            );
        }
        assert_eq!(
            read("src/lib.rs::Parent::child", true, false).0,
            expected.replace(":3-5\n", ":3-5  (unsaved in editor)\n")
        );
    }

    #[test]
    fn show_zoom_uses_child_selection_and_full_never_truncates() {
        assert_eq!(read("src/lib.rs::Parent", true, false), ("src/lib.rs:1-205  (unsaved in editor)  (outline: 205 lines; --full prints the body)\n     4\tline 4 (3-5)\n".into(), 0));
        let body = read("src/lib.rs::Parent", false, true);
        assert_eq!(body.1, 0);
        assert_eq!(body.0.lines().count(), 206);
        assert!(body.0.ends_with("   205\t  line 205\n"));
        let mut leaf = nodes();
        leaf.truncate(1);
        assert_eq!(
            render_item(
                &parse_argument("src/lib.rs::Parent").unwrap().0,
                None,
                Path::new("src/lib.rs"),
                &leaf,
                &source(),
                false,
                false
            ),
            body
        );
    }

    #[test]
    fn show_clamps_line_range_to_the_last_disk_line() {
        assert_eq!(
            read("src/lib.rs:209-211", false, false),
            (
                "src/lib.rs:209-210\n   209\t  line 209\n   210\t  line 210\n".into(),
                0
            )
        );
        for argument in ["src/lib.rs:211-212", "src/lib.rs:0-2", "src/lib.rs:2-1"] {
            assert_eq!(
                read(argument, false, false),
                (format!("{argument}  line-outside  file has 210 lines\n"), 5)
            );
        }
    }

    #[test]
    fn show_clamps_symbol_and_position_ranges_from_unsaved_outlines() {
        let mut nodes = nodes();
        nodes[2].range[1] = 215;
        for argument in ["src/lib.rs::child ~208", "src/lib.rs:208:4"] {
            let (anchor, position) = parse_argument(argument).unwrap();
            for dirty in [false, true] {
                let marker = if dirty { "  (unsaved in editor)" } else { "" };
                assert_eq!(
                    render_item(
                        &anchor,
                        position,
                        Path::new("src/lib.rs"),
                        &nodes,
                        &source(),
                        dirty,
                        false
                    ),
                    (
                        format!(
                            "src/lib.rs:208-210{marker}\n   208\t  line 208\n   209\t  line 209\n   210\t  line 210\n"
                        ),
                        0
                    )
                );
            }
        }
    }

    #[test]
    fn show_zoom_omits_children_past_the_clamped_range() {
        let mut nodes = nodes();
        nodes[0].range[1] = 215;
        let mut outside = nodes[1].clone();
        outside.range = [211, 215];
        outside.selection.start.line = 210;
        nodes.push(outside);
        assert_eq!(
            render_item(&parse_argument("src/lib.rs::Parent").unwrap().0, None, Path::new("src/lib.rs"), &nodes, &source(), false, false),
            ("src/lib.rs:1-210  (outline: 210 lines; --full prints the body)\n     4\tline 4 (3-5)\n".into(), 0)
        );
    }

    #[test]
    fn show_reports_missing_and_outside() {
        let argument = "src/lib.rs::Parent::chld";
        let (anchor, _) = parse_argument(argument).unwrap();
        let checked = check_symbol(anchor, PathBuf::from("src/lib.rs"), &nodes());
        assert_eq!(checked.detail, "class Parent is at 1-205");
        assert_eq!(
            read(argument, false, false),
            (
                format!("{argument}  missing-symbol  {}\n", checked.detail),
                5
            )
        );
        assert_eq!(
            read("src/lib.rs::child ~4", false, false).0,
            read("src/lib.rs::Parent::child", false, false).0
        );
        for argument in [
            "src/lib.rs::absent",
            "src/lib.rs:210:50",
            "src/lib.rs:211-212",
            "src/lib.rs:0-2",
        ] {
            assert_eq!(read(argument, false, false).1, 5, "{argument}");
        }
    }

    #[test]
    fn show_prints_all_hits_unless_a_hint_selects_a_subset() {
        let first = read("src/lib.rs::Parent::child", false, false).0;
        let second = read("src/lib.rs:208-209", false, false).0;
        for argument in [
            "src/lib.rs::child",
            "src/lib.rs::child ~100",
            "src/lib.rs::child ~4-208",
        ] {
            assert_eq!(
                read(argument, false, false),
                (format!("{first}\n{second}"), 0)
            );
        }
        assert_eq!(read("src/lib.rs::child ~4", false, false), (first, 0));
    }

    #[test]
    fn show_type_prints_struct_and_impl_in_outline_order() {
        let mut nodes = nodes();
        nodes.remove(0);
        nodes[0].name = "Type".into();
        nodes[0].path = vec!["Type".into()];
        nodes[0].kind = "struct";
        nodes[0].parent = None;
        nodes[1].name = "impl Type".into();
        nodes[1].path = vec!["impl Type".into()];
        for (argument, spans) in [
            ("src/lib.rs::Type", vec!["3-5", "208-209"]),
            ("src/lib.rs::Type ~4", vec!["3-5"]),
        ] {
            let (text, exit) = render_item(
                &parse_argument(argument).unwrap().0,
                None,
                Path::new("src/lib.rs"),
                &nodes,
                &source(),
                false,
                false,
            );
            assert_eq!(exit, 0);
            let headers: Vec<_> = text
                .lines()
                .filter_map(|line| line.strip_prefix("src/lib.rs:"))
                .collect();
            assert_eq!(headers, spans);
        }
    }

    #[test]
    fn show_validates_the_whole_batch_before_reading() {
        assert!(
            show(
                &["src/lib.rs::Parent".into(), "not-an-anchor".into()],
                false,
                Path::new("/does-not-exist"),
                &[],
                &BTreeMap::new()
            )
            .is_err()
        );
    }
}
