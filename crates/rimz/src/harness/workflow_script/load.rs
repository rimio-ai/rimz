//! Reading a workflow script: its `meta` statement as data, without running anything, and its
//! body as the source of one async function that keeps the file's line numbers.

use serde::Deserialize;
use serde_json::{Map, Value};

/// What a workflow says of itself in its first statement.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub name: String,
    pub description: String,
}

/// A script taken apart: its `meta`, and its body with the `meta` statement blanked, so the
/// body's lines are the file's.
pub(super) struct Loaded {
    pub(super) meta: Meta,
    pub(super) body: String,
}

const SHAPE: &str = "a workflow script starts with `export const meta = { name, description }`";

/// How deep the literal of `meta` may nest.
const DEPTH: usize = 64;

/// Take `text` apart, or say at which line it does not fit the shape of a workflow script.
pub(super) fn load(text: &str) -> Result<Loaded, String> {
    let mut cursor = Cursor { text, at: 0 };
    cursor.skip_space()?;
    let start = cursor.at;
    let line = cursor.line();
    for word in ["export", "const", "meta"] {
        if cursor.word() != Some(word) {
            return Err(format!("line {line}: {SHAPE}"));
        }
        cursor.skip_space()?;
    }
    if !cursor.eat('=') {
        return Err(format!("line {line}: {SHAPE}"));
    }
    let value = cursor.literal(0)?;
    let end = cursor.at;
    let line_of_literal = cursor.line();
    cursor.skip_space()?;
    let end = if cursor.eat(';') {
        cursor.at
    } else if cursor.rest().is_empty() {
        end
    } else if let Some(token) = cursor.continuation(cursor.line() == line_of_literal) {
        return Err(format!(
            "line {}: the `meta` statement ends at its literal, so `{token}` cannot continue it: end it with `;` or a new line",
            cursor.line()
        ));
    } else {
        end
    };
    let meta =
        serde_json::from_value(value).map_err(|error| format!("line {line}: `meta`: {error}"))?;
    let blank: String = text[start..end]
        .chars()
        .map(|char| if char == '\n' { '\n' } else { ' ' })
        .collect();
    Ok(Loaded {
        meta,
        body: format!("{}{blank}{}", &text[..start], &text[end..]),
    })
}

/// The body of a called workflow's script: as `load` takes it apart, or, for text that may
/// leave `meta` out and does not start with an `export`, the whole text.
pub(super) fn body(text: &str, meta_optional: bool) -> Result<String, String> {
    if meta_optional {
        let mut cursor = Cursor { text, at: 0 };
        cursor.skip_space()?;
        if cursor.word() != Some("export") {
            return Ok(text.to_owned());
        }
    }
    load(text).map(|loaded| loaded.body)
}

struct Cursor<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Cursor<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn line(&self) -> usize {
        self.text[..self.at].matches('\n').count() + 1
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn eat(&mut self, char: char) -> bool {
        let found = self.peek() == Some(char);
        if found {
            self.at += char.len_utf8();
        }
        found
    }

    /// Whitespace and comments.
    fn skip_space(&mut self) -> Result<(), String> {
        loop {
            let rest = self.rest();
            let trimmed = rest.trim_start();
            self.at += rest.len() - trimmed.len();
            if trimmed.starts_with("//") {
                self.at += trimmed.find('\n').unwrap_or(trimmed.len());
            } else if let Some(comment) = trimmed.strip_prefix("/*") {
                let line = self.line();
                let close = comment
                    .find("*/")
                    .ok_or_else(|| format!("line {line}: a comment is not closed"))?;
                self.at += 2 + close + 2;
            } else {
                return Ok(());
            }
        }
    }

    /// The token at the cursor when it would continue the expression before it: any token on
    /// the expression's own line, and after a line break one that JavaScript joins to it.
    fn continuation(&self, same_line: bool) -> Option<&'a str> {
        let rest = self.rest();
        // After a line break, `++` and `--` start a statement: their postfix forms take no line
        // break, so JavaScript ends the statement before them.
        if !same_line && (rest.starts_with("++") || rest.starts_with("--")) {
            return None;
        }
        let word = rest
            .find(|char: char| !(char.is_alphanumeric() || char == '_' || char == '$'))
            .unwrap_or(rest.len());
        let token = match word {
            0 if rest.starts_with("!=") => &rest[..2],
            0 => &rest[..rest.chars().next()?.len_utf8()],
            _ => &rest[..word],
        };
        let joins = matches!(token, "in" | "instanceof" | "!=")
            || token.starts_with([
                '(', '[', '`', '.', '?', ',', '=', '+', '-', '*', '/', '%', '<', '>', '&', '|', '^',
            ]);
        (same_line || joins).then_some(token)
    }

    fn word(&mut self) -> Option<&'a str> {
        let rest = self.rest();
        let len = rest
            .find(|char: char| !(char.is_alphanumeric() || char == '_' || char == '$'))
            .unwrap_or(rest.len());
        if len == 0 || rest.starts_with(|char: char| char.is_ascii_digit()) {
            return None;
        }
        self.at += len;
        Some(&self.text[self.at - len..self.at])
    }

    fn not_literal(&self) -> String {
        let token: String = self
            .rest()
            .chars()
            .take_while(|char| !char.is_whitespace())
            .take(20)
            .collect();
        format!(
            "line {}: `meta` is a plain literal of strings, numbers, booleans, null, arrays, and objects, and `{token}` is not one",
            self.line()
        )
    }

    fn literal(&mut self, depth: usize) -> Result<Value, String> {
        self.skip_space()?;
        if depth == DEPTH && matches!(self.peek(), Some('{' | '[')) {
            return Err(format!(
                "line {}: `meta` nests deeper than {DEPTH} levels",
                self.line()
            ));
        }
        match self.peek() {
            Some('{') => self.object(depth + 1),
            Some('[') => self.array(depth + 1),
            Some(quote @ ('\'' | '"' | '`')) => self.string(quote).map(Value::String),
            Some(char) if char.is_ascii_digit() || char == '-' || char == '.' => self.number(),
            _ => {
                let start = self.at;
                match self.word() {
                    Some("true") => Ok(Value::Bool(true)),
                    Some("false") => Ok(Value::Bool(false)),
                    Some("null") => Ok(Value::Null),
                    _ => {
                        self.at = start;
                        Err(self.not_literal())
                    }
                }
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.eat('{');
        let mut map = Map::new();
        loop {
            self.skip_space()?;
            if self.eat('}') {
                return Ok(Value::Object(map));
            }
            let key = match self.peek() {
                Some(quote @ ('\'' | '"')) => self.string(quote)?,
                _ => self.word().ok_or_else(|| self.not_literal())?.to_owned(),
            };
            self.skip_space()?;
            if !self.eat(':') {
                return Err(self.not_literal());
            }
            let value = self.literal(depth)?;
            map.insert(key, value);
            self.skip_space()?;
            if !self.eat(',') {
                self.skip_space()?;
                if !self.eat('}') {
                    return Err(self.not_literal());
                }
                return Ok(Value::Object(map));
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.eat('[');
        let mut items = Vec::new();
        loop {
            self.skip_space()?;
            if self.eat(']') {
                return Ok(Value::Array(items));
            }
            items.push(self.literal(depth)?);
            self.skip_space()?;
            if !self.eat(',') {
                if !self.eat(']') {
                    return Err(self.not_literal());
                }
                return Ok(Value::Array(items));
            }
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let rest = self.rest();
        let len = rest
            .find(|char: char| {
                !(char.is_ascii_digit() || matches!(char, '-' | '+' | '.' | 'e' | 'E'))
            })
            .unwrap_or(rest.len());
        let number = serde_json::from_str::<serde_json::Number>(&rest[..len])
            .map_err(|_| self.not_literal())?;
        self.at += len;
        Ok(Value::Number(number))
    }

    fn string(&mut self, quote: char) -> Result<String, String> {
        let line = self.line();
        let unclosed = || format!("line {line}: a string in `meta` is not closed");
        self.eat(quote);
        let mut out = String::new();
        let mut chars = self.rest().char_indices();
        while let Some((index, char)) = chars.next() {
            match char {
                _ if char == quote => {
                    self.at += index + 1;
                    return Ok(out);
                }
                '$' if quote == '`' && self.rest()[index..].starts_with("${") => {
                    self.at += index;
                    return Err(self.not_literal());
                }
                '\n' if quote != '`' => return Err(unclosed()),
                '\\' => {
                    let (_, escaped) = chars.next().ok_or_else(unclosed)?;
                    match escaped {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'v' => out.push('\u{b}'),
                        '0' => out.push('\0'),
                        '\n' => {}
                        'u' | 'x' => {
                            let hex: String =
                                if escaped == 'u' && self.rest()[index + 2..].starts_with('{') {
                                    chars.next();
                                    chars
                                        .by_ref()
                                        .map(|(_, char)| char)
                                        .take_while(|char| *char != '}')
                                        .collect()
                                } else {
                                    let len = if escaped == 'u' { 4 } else { 2 };
                                    chars.by_ref().take(len).map(|(_, char)| char).collect()
                                };
                            let decoded =
                                u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
                            out.push(decoded.ok_or_else(|| {
                                format!(
                                    "line {line}: a string in `meta` holds a bad \\{escaped} escape"
                                )
                            })?);
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
        Err(unclosed())
    }
}
