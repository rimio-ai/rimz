//! The `--filter` term grammar both list filters share: `<field><op><value>`
//! terms separated by whitespace, bare or JSON-quoted values joined by commas,
//! `*` for any present value and `none` for a missing one, and the wildcard
//! match. Each caller owns its field table; this module never names a field.

use winnow::combinator::{alt, delimited, repeat};
use winnow::token::{any, none_of, take_while};
use winnow::{ModalResult, Parser};

/// A comparison operator; `=` and `!=` take a value list, the rest one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

impl Op {
    fn lex(input: &mut &str) -> ModalResult<Self> {
        alt((
            "!=".value(Self::Ne),
            ">=".value(Self::Ge),
            "<=".value(Self::Le),
            "=".value(Self::Eq),
            ">".value(Self::Gt),
            "<".value(Self::Lt),
        ))
        .parse_next(input)
    }

    pub const fn ordered(self) -> bool {
        matches!(self, Self::Gt | Self::Ge | Self::Lt | Self::Le)
    }

    pub fn compares<T: PartialOrd>(self, have: T, want: T) -> bool {
        match self {
            Self::Eq => have == want,
            Self::Ne => have != want,
            Self::Gt => have > want,
            Self::Ge => have >= want,
            Self::Lt => have < want,
            Self::Le => have <= want,
        }
    }

    /// `=` holds when any value passes `test`, `!=` when none does.
    pub fn holds<V>(self, values: &[V], test: impl Fn(&V) -> bool) -> bool {
        let any = values.iter().any(test);
        if self == Self::Ne { !any } else { any }
    }
}

/// One lexed value: bare `*` and `none` (any case) are the grammar's own
/// words. Quoting makes `"none"` plain text; a quoted `"*"` is text too, which
/// a field that matches through [`glob()`] still reads as a wildcard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Word {
    Any,
    None,
    Text(String),
}

/// One lexed term: its text as written, the field name, operator and values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term<'a> {
    pub text: &'a str,
    pub field: &'a str,
    pub op: Op,
    pub values: Vec<Word>,
}

/// A term the grammar refuses before any field table reads it.
#[derive(Debug, thiserror::Error)]
pub enum TermError {
    #[error("filter term `{term}` has no operator: use one of = != > >= < <=")]
    NoOperator { term: String },
    #[error("filter term `{term}` has no field before its operator; see `--filter help`")]
    NoField { term: String },
    #[error("filter term `{term}` has an empty value")]
    EmptyValue { term: String },
    #[error("filter term `{term}` has an unterminated quote")]
    UnterminatedQuote { term: String },
    #[error("filter term `{term}` has a quote inside a word; quote the whole value")]
    StrayQuote { term: String },
}

/// The next term of `input` past any leading whitespace, or `None` at its end.
/// A field name is ASCII letters, digits, `_` and `-`.
pub fn next_term<'a>(input: &mut &'a str) -> Option<Result<Term<'a>, TermError>> {
    *input = input.trim_start();
    (!input.is_empty()).then(|| lex_term(input))
}

fn lex_term<'a>(input: &mut &'a str) -> Result<Term<'a>, TermError> {
    let start = *input;
    let word = start.split_whitespace().next().unwrap_or_default();
    let field: ModalResult<&str> = take_while(0.., |ch: char| {
        ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
    })
    .parse_next(input);
    let field = field.unwrap_or_default();
    let Ok(op) = Op::lex(input) else {
        return Err(TermError::NoOperator { term: word.into() });
    };
    if field.is_empty() {
        return Err(TermError::NoField { term: word.into() });
    }
    let mut values = vec![lex_value(input, word)?];
    while let Some(rest) = input.strip_prefix(',') {
        *input = rest;
        values.push(lex_value(input, word)?);
    }
    if !input.is_empty() && !input.starts_with(char::is_whitespace) {
        return Err(TermError::StrayQuote { term: word.into() });
    }
    Ok(Term {
        text: &start[..start.len() - input.len()],
        field,
        op,
        values,
    })
}

/// One value: a bare word up to whitespace, a comma, or a quote, or a
/// JSON-escaped quoted string.
fn lex_value(input: &mut &str, term: &str) -> Result<Word, TermError> {
    if input.starts_with('"') {
        let quoted: ModalResult<&str> = delimited(
            '"',
            repeat::<_, _, (), _, _>(0.., alt((('\\', any).void(), none_of(['"', '\\']).void()))),
            '"',
        )
        .take()
        .parse_next(input);
        let unterminated = || TermError::UnterminatedQuote { term: term.into() };
        let text = serde_json::from_str(quoted.map_err(|_| unterminated())?)
            .map_err(|_| unterminated())?;
        return Ok(Word::Text(text));
    }
    let bare: ModalResult<&str> = take_while(0.., |ch: char| {
        !ch.is_whitespace() && ch != ',' && ch != '"'
    })
    .parse_next(input);
    match bare.unwrap_or_default() {
        "" => Err(TermError::EmptyValue { term: term.into() }),
        "*" => Ok(Word::Any),
        none if none.eq_ignore_ascii_case("none") => Ok(Word::None),
        bare => Ok(Word::Text(bare.to_owned())),
    }
}

/// Whole-text match where each `*` stands for any run of characters.
pub fn glob(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(rest) = text.strip_prefix(parts.next().unwrap_or_default()) else {
        return false;
    };
    let parts = parts.collect::<Vec<_>>();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    let mut rest = rest;
    for part in middle {
        let Some(at) = rest.find(part) else {
            return false;
        };
        rest = &rest[at + part.len()..];
    }
    rest.ends_with(last)
}

#[cfg(test)]
mod tests;
