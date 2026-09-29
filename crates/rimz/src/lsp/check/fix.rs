//! Refresh uniquely resolved hints using offsets into the original notes.

use super::*;

#[derive(Debug, Serialize)]
pub(super) struct Fix {
    pub(super) line: usize,
    pub(super) before: String,
    pub(super) after: String,
}

pub(super) fn rewrite(
    source: &str,
    context: &mut Context<'_>,
) -> Result<(String, Vec<Fix>), query::QueryErr> {
    let mut edits = Vec::new();
    let mut fixes = Vec::new();
    for anchor in extract(source) {
        if anchor.hint_source.is_none() || anchor.symbol.is_none() {
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
        let selected = hinted(&hits, anchor.hint);
        let node = match (selected.as_slice(), hits.as_slice()) {
            ([node], _) | (_, [node]) => node,
            _ => continue,
        };
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
    Ok((updated, fixes))
}
