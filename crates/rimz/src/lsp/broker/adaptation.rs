//! Pure reply-shape negotiation and URI spelling adaptation.

use serde_json::{Value, json};

pub(super) fn reply(value: &mut Value, method: &str, capabilities: &Value) {
    if let Some(name @ ("definition" | "typeDefinition" | "implementation" | "declaration")) =
        method.strip_prefix("textDocument/")
        && capabilities["textDocument"][name]["linkSupport"] != true
        && let Some(locations) = value.as_array_mut()
    {
        for location in locations {
            if location.get("targetUri").is_some() {
                *location =
                    json!({"uri":location["targetUri"],"range":location["targetSelectionRange"]});
            }
        }
    }
    if capabilities["textDocument"]["completion"]["completionItem"]["snippetSupport"] != true {
        match method {
            "textDocument/completion" => {
                let items = if value.is_array() {
                    value.as_array_mut()
                } else {
                    value.get_mut("items").and_then(Value::as_array_mut)
                };
                if let Some(items) = items {
                    for item in items {
                        completion(item);
                    }
                }
            }
            "completionItem/resolve" => completion(value),
            _ => {}
        }
    }
    if capabilities["experimental"]["snippetTextEdit"] != true {
        snippet_edits(value);
    }
}

fn completion(item: &mut Value) {
    if item["insertTextFormat"] != 2 {
        return;
    }
    if let Some(text) = item.get_mut("insertText") {
        plain(text);
    }
    if let Some(text) = item
        .get_mut("textEdit")
        .and_then(|edit| edit.get_mut("newText"))
    {
        plain(text);
    }
    item["insertTextFormat"] = json!(1);
}

fn snippet_edits(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if object.get("insertTextFormat") == Some(&json!(2))
                && let Some(text) = object.get_mut("newText").filter(|text| text.is_string())
            {
                plain(text);
                object.remove("insertTextFormat");
            }
            for child in object.values_mut() {
                snippet_edits(child);
            }
        }
        Value::Array(array) => {
            for child in array {
                snippet_edits(child);
            }
        }
        _ => {}
    }
}

fn plain(value: &mut Value) {
    let Some(text) = value.as_str() else { return };
    let mut chars = text.chars().peekable();
    let mut output = String::new();
    let mut defaults = 0;
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars
                .peek()
                .is_some_and(|ch| matches!(ch, '$' | '}' | '\\')) =>
            {
                output.extend(chars.next());
            }
            '}' if defaults > 0 => defaults -= 1,
            '$' => {
                let braced = chars.peek() == Some(&'{');
                if braced {
                    chars.next();
                }
                let numeric = chars.peek().is_some_and(char::is_ascii_digit);
                let mut identifier = false;
                while chars.peek().is_some_and(|ch| {
                    ch.is_ascii_digit() || (!numeric && (ch.is_ascii_alphabetic() || *ch == '_'))
                }) {
                    identifier = true;
                    chars.next();
                }
                if !identifier {
                    output.push('$');
                    if braced {
                        output.push('{');
                    }
                    continue;
                }
                if !braced {
                    continue;
                }
                match chars.next() {
                    Some(':') => defaults += 1,
                    Some('|') => {
                        let mut first = true;
                        while let Some(ch) = chars.next() {
                            match ch {
                                '|' if chars.peek() == Some(&'}') => {
                                    chars.next();
                                    break;
                                }
                                ',' => first = false,
                                '\\' if chars
                                    .peek()
                                    .is_some_and(|ch| matches!(ch, ',' | '|' | '\\')) =>
                                {
                                    let escaped = chars.next();
                                    if first {
                                        output.extend(escaped);
                                    }
                                }
                                _ if first => output.push(ch),
                                _ => {}
                            }
                        }
                    }
                    Some('/') => {
                        // Transforms have no variable/tabstop value in a plain-text insertion.
                        let mut braces = 1;
                        while let Some(ch) = chars.next() {
                            match ch {
                                '\\' => {
                                    chars.next();
                                }
                                '{' => braces += 1,
                                '}' => {
                                    braces -= 1;
                                    if braces == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => output.push(ch),
        }
    }
    *value = Value::String(output);
}

pub(super) fn uris(value: &mut Value, map: &impl Fn(&str) -> Option<String>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if matches!(key.as_str(), "uri" | "targetUri")
                    && let Some(mapped) = child.as_str().and_then(map)
                {
                    *child = Value::String(mapped);
                }
                if key == "changes"
                    && let Some(changes) = child.as_object_mut()
                {
                    *changes = std::mem::take(changes)
                        .into_iter()
                        .map(|(uri, edits)| (map(&uri).unwrap_or(uri), edits))
                        .collect();
                }
                uris(child, map);
            }
        }
        Value::Array(array) => {
            for child in array {
                uris(child, map);
            }
        }
        _ => {}
    }
}
