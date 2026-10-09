//! Complete unique short paths or paths one file's outline singles out, refresh
//! uniquely resolved hints, and insert missing ones on request, using offsets
//! into the original notes.

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
    for (index, mut anchor) in extract(source).into_iter().enumerate() {
        let before = anchor.text.clone();
        let mut changed = false;
        if let Some((range, full)) = defining_file(context, &anchor)? {
            let shift = |at: usize| at - anchor.path.len() + full.len();
            anchor.text.replace_range(..anchor.path.len(), &full);
            anchor.hint_text = anchor
                .hint_text
                .map(|hint| shift(hint.start)..shift(hint.end));
            anchor.path = full.clone();
            edits.push((range, full));
            changed = true;
        }
        let line = anchor.line;
        let mut after = anchor.text.clone();
        if let Some((range, replacement, text)) =
            hint_edit(source, index, context, anchor, hints, &mut ambiguous)?
        {
            edits.push((range, replacement));
            after = text;
            changed = true;
        }
        if changed {
            fixes.push(Fix {
                line,
                before,
                after,
            });
        }
    }
    let mut updated = source.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        updated.replace_range(range, &replacement);
    }
    Ok((updated, fixes, ambiguous))
}

/// The source range of an anchor's short path and the checkout file it
/// completes to. A unique suffix needs no outline or editor check. Several
/// matches require a symbol in exactly one outline and no unsaved candidates;
/// a hint never chooses between defining files. A file whose whole path no
/// anchor can spell is never completed.
fn defining_file(
    context: &mut Context<'_>,
    anchor: &Anchor,
) -> Result<Option<(std::ops::Range<usize>, String)>, query::QueryErr> {
    let Some(range) = &anchor.path_source else {
        return Ok(None);
    };
    let PathMatch::Suffix(candidates) = resolve(context.checkout, &context.files, &anchor.path)
    else {
        return Ok(None);
    };
    if let [path] = candidates.as_slice() {
        return Ok(anchor_path(path).map(|full| (range.clone(), full.to_owned())));
    }
    let Some(chain) = &anchor.symbol else {
        return Ok(None);
    };
    let mut defining = None;
    for path in candidates {
        let Some(file) = context.outline(&path)? else {
            return Ok(None);
        };
        if file.dirty {
            return Ok(None);
        }
        if !symbol_hits(&file.nodes, chain).is_empty() && defining.replace(path).is_some() {
            return Ok(None);
        }
    }
    let Some(path) = defining else {
        return Ok(None);
    };
    Ok(anchor_path(&path).map(|full| (range.clone(), full.to_owned())))
}

/// The hint refresh, or with `hints` the hint insertion, for one anchor: the
/// source range, its replacement, and the anchor text after the change.
fn hint_edit(
    source: &str,
    index: usize,
    context: &mut Context<'_>,
    anchor: Anchor,
    hints: bool,
    ambiguous: &mut Vec<Ambiguous>,
) -> Result<Option<(std::ops::Range<usize>, String, String)>, query::QueryErr> {
    let insert_at = anchor.span_end.filter(|_| hints && anchor.hint.is_none());
    if anchor.hint_source.is_none() && insert_at.is_none() {
        return Ok(None);
    }
    let Ok((anchor, path, _)) = context.resolve(anchor) else {
        return Ok(None);
    };
    let Some(file) = context.outline(&path)? else {
        return Ok(None);
    };
    if file.dirty {
        return Ok(None);
    }
    let Some(chain) = &anchor.symbol else {
        return Ok(None);
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
        return Ok(None);
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
            return Ok(None);
        }
        let after = format!("{} {hint}", anchor.text);
        return Ok(Some((at..at, format!(" {hint}"), after)));
    }
    let (Some(source_range), Some(text_range)) = (anchor.hint_source, anchor.hint_text) else {
        return Ok(None);
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
        return Ok(None);
    }
    let mut after = anchor.text;
    after.replace_range(text_range, &replacement);
    Ok(Some((source_range, replacement, after)))
}

/// The one item an anchor names. A hinted anchor names the single hit its hint
/// overlaps, else the sole hit. A hintless anchor, or a hinted one whose hint
/// overlaps no hit, names its sole hit, or the one hit whose own name is the
/// anchor's last segment when every other hit matched only through its last
/// token, as impl blocks of a type do. A hint overlapping only an impl block
/// keeps naming that block: it may point there on purpose.
fn named<'a>(
    hits: &[&'a Candidate],
    chain: &[String],
    hint: Option<[u32; 2]>,
) -> Option<&'a Candidate> {
    if hint.is_some() {
        match (hinted(hits, hint).as_slice(), hits) {
            ([node], _) | (_, [node]) => return Some(*node),
            ([], _) => {}
            _ => return None,
        }
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
