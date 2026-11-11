//! User text quoted into launch prose: workflow briefs and loop trigger clauses.

pub(in crate::harness) fn span(text: &str) -> String {
    format!("`{}`", verbatim(text))
}

/// User text shown as typed: only `<`, which could open or close the
/// reminder tag, and control characters are escaped.
pub(in crate::harness) fn verbatim(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '<' => escaped.push_str("&lt;"),
            ch if ch.is_control() => escaped.extend(ch.escape_default()),
            ch => escaped.push(ch),
        }
    }
    escaped
}
