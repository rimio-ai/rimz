//! Verified, byte-preserving edits to boolean leaves in Claude's global config.

use std::collections::BTreeMap;

use serde_json::{Value, value::RawValue};

pub(super) fn set_true(text: &str, path: &[&str]) -> Option<String> {
    let mut expected: Value = serde_json::from_str(text).ok()?;
    let mut leaf = &mut expected;
    for key in path {
        leaf = leaf
            .as_object_mut()?
            .entry(*key)
            .or_insert_with(|| serde_json::json!({}));
    }
    *leaf = Value::Bool(true);
    let next = replace_leaf(text, path)?;
    (serde_json::from_str::<Value>(&next).ok()? == expected).then_some(next)
}

fn replace_leaf(text: &str, path: &[&str]) -> Option<String> {
    let Some((key, rest)) = path.split_first() else {
        return Some("true".into());
    };
    let members: BTreeMap<String, &RawValue> = serde_json::from_str(text).ok()?;
    if let Some(value) = members.get(*key) {
        let replacement = replace_leaf(value.get(), rest)?;
        // RawValue borrows this exact input, so its byte span is inside text.
        let start = value.get().as_ptr() as usize - text.as_ptr() as usize;
        let mut next = text.to_owned();
        next.replace_range(start..start + value.get().len(), &replacement);
        return Some(next);
    }
    let value = replace_leaf("{}", rest)?;
    let open = text.find('{')?;
    let (head, tail) = text.split_at(open + 1);
    let key = serde_json::to_string(key).ok()?;
    let suffix = if members.is_empty() { "\n" } else { "," };
    Some(format!("{head}\n  {key}: {value}{suffix}{tail}"))
}
