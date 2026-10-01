//! Refresh uniquely resolved hints, and insert missing ones on request, using
//! offsets into the original notes.

use super::*;

#[derive(Debug, Serialize)]
pub(super) struct Fix {
    pub(super) line: usize,
    pub(super) before: String,
    pub(super) after: String,
}

#[derive(Debug, Serialize)]
pub(super) struct Ambiguous {
    pub(super) line: usize,
    pub(super) text: String,
    pub(super) candidates: Vec<Candidate>,
    #[serde(skip)]
    pub(super) detail: String,
}

pub(super) fn rewrite(
    source: &str,
    context: &mut Context<'_>,
    hints: bool,
) -> Result<(String, Vec<Fix>, Vec<Ambiguous>), query::QueryErr> {
    let mut edits = Vec::new();
    let mut fixes = Vec::new();
    let mut ambiguous = Vec::new();
    for (index, anchor) in extract(source).into_iter().enumerate() {
        let insert_at = anchor.span_end.filter(|_| hints && anchor.hint.is_none());
        if anchor.symbol.is_none() || (anchor.hint_source.is_none() && insert_at.is_none()) {
            continue;
        }
        let Ok((anchor, path)) = context.resolve(anchor) else {
            continue;
        };
        let Some(file) = context.outline(&path)? else {
            continue;
        };
        if file.dirty {
            continue;
        }
        let Some(chain) = &anchor.symbol else {
            continue;
        };
        let hits = symbol_hits(&file.nodes, chain);
        let Some(node) = named(&hits, chain, anchor.hint) else {
            if insert_at.is_some() && hits.len() > 1 {
                let hits = hits.into_iter().cloned().collect();
                let (candidates, detail) = candidate_details(&file.nodes, chain, hits);
                ambiguous.push(Ambiguous {
                    line: anchor.line,
                    text: anchor.text,
                    candidates,
                    detail,
                });
            }
            continue;
        };
        if let Some(at) = insert_at {
            let [start, end] = node.range;
            let hint = if start == end {
                format!("({start})")
            } else {
                format!("({start}-{end})")
            };
            let mut trial = source.to_owned();
            trial.insert_str(at, &format!(" {hint}"));
            let reads_back = extract(&trial)
                .get(index)
                .is_some_and(|read| read.hint == Some(node.range) && read.hint_source.is_some());
            if !reads_back {
                continue;
            }
            fixes.push(Fix {
                line: anchor.line,
                after: format!("{} {hint}", anchor.text),
                before: anchor.text,
            });
            edits.push((at..at, format!(" {hint}")));
            continue;
        }
        let (Some(source_range), Some(text_range)) = (anchor.hint_source, anchor.hint_text) else {
            continue;
        };
        let hint = &source[source_range.clone()];
        let values = if hint
            .trim_start_matches(|ch: char| !ch.is_ascii_digit())
            .contains(':')
        {
            [
                node.selection.start.line + 1,
                node.selection.start.character + 1,
            ]
        } else {
            node.range
        };
        let mut replacement = String::new();
        let mut tail = hint;
        let mut index = 0;
        while let Some(start) = tail.find(|ch: char| ch.is_ascii_digit()) {
            replacement.push_str(&tail[..start]);
            let end = tail[start..].bytes().take_while(u8::is_ascii_digit).count();
            replacement.push_str(&values[index].to_string());
            index += 1;
            tail = &tail[start + end..];
        }
        replacement.push_str(tail);
        if replacement == hint {
            continue;
        }
        let mut after = anchor.text.clone();
        after.replace_range(text_range, &replacement);
        fixes.push(Fix {
            line: anchor.line,
            before: anchor.text,
            after,
        });
        edits.push((source_range, replacement));
    }
    let mut updated = source.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        updated.replace_range(range, &replacement);
    }
    Ok((updated, fixes, ambiguous))
}

/// The one item an anchor names. A hinted anchor names the single hit its hint
/// overlaps, else the sole hit. A hintless anchor names its sole hit, or the one
/// hit whose own name is the anchor's last segment when every other hit matched
/// only through its last token, as impl blocks of a type do.
fn named<'a>(
    hits: &[&'a Candidate],
    chain: &[String],
    hint: Option<[u32; 2]>,
) -> Option<&'a Candidate> {
    if hint.is_some() {
        return match (hinted(hits, hint).as_slice(), hits) {
            ([node], _) | (_, [node]) => Some(*node),
            _ => None,
        };
    }
    let wanted = chain.last()?;
    let mut direct = hits.iter().copied().filter(|node| {
        node.path
            .last()
            .is_some_and(|name| query::without_generics(name) == *wanted)
    });
    match (hits, direct.next(), direct.next()) {
        ([node], _, _) => Some(*node),
        (_, Some(node), None) => Some(node),
        _ => None,
    }
}
