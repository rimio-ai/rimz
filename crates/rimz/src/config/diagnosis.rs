//! Actionable diagnoses for malformed user-owned TOML files.

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

const SNIPPET_MAX_CHARS: usize = 72;

/// A TOML failure classified while both the parser error and source text are available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFileDiagnosis {
    path: PathBuf,
    line: Option<usize>,
    snippet: Option<String>,
    problem: String,
    fix: String,
    raw_message: String,
}

impl ConfigFileDiagnosis {
    pub(crate) fn from_toml_edit(path: &Path, text: &str, error: &toml_edit::TomlError) -> Self {
        Self::build(path, text, error.message(), error.span())
    }

    pub(crate) fn from_toml_de(path: &Path, text: &str, error: &toml::de::Error) -> Self {
        Self::build(path, text, error.message(), error.span())
    }

    pub(crate) fn spanless(path: &Path, message: impl Into<String>) -> Self {
        let raw_message = message.into();
        let problem = first_line(&raw_message);
        let fix = format!("correct {}, then re-run", path.display());
        Self {
            path: path.to_path_buf(),
            line: None,
            snippet: None,
            problem,
            fix,
            raw_message,
        }
    }

    fn build(path: &Path, text: &str, message: &str, span: Option<Range<usize>>) -> Self {
        let location = span
            .as_ref()
            .and_then(|span| source_location(text, span.start));
        let line = location.as_ref().map(|(line, _)| *line);
        let snippet = location.and_then(|(_, snippet)| snippet);
        let duplicate_key = message
            .contains("duplicate key")
            .then(|| span.as_ref().and_then(|span| key_at_span(text, span)))
            .flatten();
        let (problem, fix) = match duplicate_key {
            Some(key) => {
                let problem = format!("`{key}` is defined more than once in the same table");
                let location = line.map_or_else(
                    || path.display().to_string(),
                    |line| format!("{}:{line}", path.display()),
                );
                (
                    problem,
                    format!("remove the extra `{key}` at {location}, then re-run"),
                )
            }
            None => {
                let location = line.map_or_else(
                    || path.display().to_string(),
                    |line| format!("{}:{line}", path.display()),
                );
                (
                    first_line(message),
                    format!("correct {location}, then re-run"),
                )
            }
        };
        Self {
            path: path.to_path_buf(),
            line,
            snippet,
            problem,
            fix,
            raw_message: message.to_owned(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn line(&self) -> Option<usize> {
        self.line
    }

    pub fn problem(&self) -> &str {
        &self.problem
    }

    pub fn fix(&self) -> &str {
        &self.fix
    }

    pub fn raw_message(&self) -> &str {
        &self.raw_message
    }

    /// A one-line problem description for notices that already carry their own fix.
    pub fn summary(&self) -> String {
        self.line.map_or_else(
            || self.problem.clone(),
            |line| format!("line {line}: {}", self.problem),
        )
    }
}

impl fmt::Display for ConfigFileDiagnosis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let (Some(line), Some(snippet)) = (self.line, self.snippet.as_deref()) {
            let width = line.to_string().len();
            writeln!(f, "{line:>width$} | {snippet}")?;
            writeln!(f, "{:>width$} | {}", "", self.problem)?;
        } else {
            writeln!(f, "{}", self.problem)?;
        }
        write!(f, "fix: {}", self.fix)
    }
}

impl std::error::Error for ConfigFileDiagnosis {}

fn first_line(message: &str) -> String {
    message.lines().next().unwrap_or(message).trim().to_owned()
}

fn source_location(text: &str, offset: usize) -> Option<(usize, Option<String>)> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |index| index + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |index| offset + index);
    let snippet = text.get(start..end)?.trim();
    Some((
        line,
        (!snippet.is_empty()).then(|| truncate_snippet(snippet)),
    ))
}

fn truncate_snippet(snippet: &str) -> String {
    if snippet.chars().count() <= SNIPPET_MAX_CHARS {
        return snippet.to_owned();
    }
    snippet
        .chars()
        .take(SNIPPET_MAX_CHARS - 1)
        .chain(std::iter::once('…'))
        .collect()
}

fn key_at_span(text: &str, span: &Range<usize>) -> Option<String> {
    let key = text.get(span.clone())?.trim();
    if key.is_empty() {
        return None;
    }
    let key = key
        .strip_prefix('"')
        .and_then(|key| key.strip_suffix('"'))
        .or_else(|| {
            key.strip_prefix('\'')
                .and_then(|key| key.strip_suffix('\''))
        })
        .unwrap_or(key);
    Some(key.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::DocumentMut;

    #[test]
    fn duplicate_key_names_the_second_key_and_fix() {
        let text = "[resume]\nauto_continue = false\nauto_continue = true\n";
        let error = text.parse::<DocumentMut>().expect_err("duplicate key");

        let diagnosis =
            ConfigFileDiagnosis::from_toml_edit(Path::new("/tmp/config.toml"), text, &error);

        assert_eq!(diagnosis.line(), Some(3));
        assert_eq!(
            diagnosis.problem(),
            "`auto_continue` is defined more than once in the same table"
        );
        assert_eq!(
            diagnosis.fix(),
            "remove the extra `auto_continue` at /tmp/config.toml:3, then re-run"
        );
        assert_eq!(
            diagnosis.to_string(),
            "3 | auto_continue = true\n  | `auto_continue` is defined more than once in the same table\nfix: remove the extra `auto_continue` at /tmp/config.toml:3, then re-run"
        );
    }

    #[test]
    fn syntax_error_keeps_the_parser_problem_and_computes_the_line() {
        let text = "valid = true\nbroken = [\n";
        let error = toml::from_str::<toml::Value>(text).expect_err("syntax error");

        let diagnosis = ConfigFileDiagnosis::from_toml_de(Path::new("settings.toml"), text, &error);

        assert_eq!(diagnosis.line(), Some(2));
        assert_eq!(diagnosis.problem(), error.message());
        assert_eq!(diagnosis.fix(), "correct settings.toml:2, then re-run");
    }

    #[test]
    fn long_snippets_are_valid_utf8_and_bounded() {
        let text = format!("key = \"{}\" nope\n", "é".repeat(80));
        let error = toml::from_str::<toml::Value>(&text).expect_err("syntax error");

        let diagnosis = ConfigFileDiagnosis::from_toml_de(Path::new("config.toml"), &text, &error);
        let snippet = diagnosis.snippet.expect("snippet");

        assert_eq!(snippet.chars().count(), SNIPPET_MAX_CHARS);
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn spanless_diagnosis_has_a_path_only_fix() {
        let diagnosis = ConfigFileDiagnosis::spanless(
            Path::new(".rimz/config.toml"),
            "invalid type: string, expected a table",
        );

        assert_eq!(diagnosis.line(), None);
        assert_eq!(
            diagnosis.summary(),
            "invalid type: string, expected a table"
        );
        assert_eq!(
            diagnosis.to_string(),
            "invalid type: string, expected a table\nfix: correct .rimz/config.toml, then re-run"
        );
    }
}
