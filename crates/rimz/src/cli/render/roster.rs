//! Shared agent roster rows and declared signal bindings.

use std::collections::BTreeMap;
use std::io::Write;

use super::{paint, palette};

pub(crate) struct RosterRow {
    pub handle: String,
    pub kind: String,
    pub model: Option<String>,
    pub leader: bool,
}

pub(crate) struct RosterSignal {
    pub signal: String,
    pub matches: BTreeMap<String, String>,
    pub role: String,
}

pub(crate) struct Roster {
    rows: Vec<RosterRow>,
    signals: Vec<RosterSignal>,
    indent: usize,
}

impl Roster {
    pub(crate) fn new(rows: Vec<RosterRow>) -> Self {
        Self {
            rows,
            signals: Vec::new(),
            indent: 0,
        }
    }

    pub(crate) fn signals(mut self, signals: Vec<RosterSignal>) -> Self {
        self.signals = signals;
        self
    }

    pub(crate) fn indent(mut self, n: usize) -> Self {
        self.indent = n;
        self
    }

    pub(crate) fn render(&self, w: &mut impl Write) -> std::io::Result<()> {
        let indent = " ".repeat(self.indent);
        let handle_width = self
            .rows
            .iter()
            .map(|row| row.handle.len() + 1)
            .max()
            .unwrap_or(0);
        let kind_width = self
            .rows
            .iter()
            .map(|row| row.kind.len())
            .max()
            .unwrap_or(0);
        let model_width = self
            .rows
            .iter()
            .map(|row| row.model.as_deref().unwrap_or("-").len())
            .max()
            .unwrap_or(0);
        for row in &self.rows {
            let handle = format!("@{:<handle_width$}", row.handle);
            let kind = format!("{:<kind_width$}", row.kind);
            let model = row.model.as_deref().unwrap_or("-");
            let model = if row.leader {
                format!("{model:<model_width$}  <- leader")
            } else {
                model.to_owned()
            };
            writeln!(
                w,
                "{indent}{}  {}  {}",
                paint(palette::identity(&row.kind), &handle),
                paint(palette::identity(&row.kind), &kind),
                paint(palette::muted(), &model)
            )?;
        }
        if self.signals.is_empty() {
            return Ok(());
        }
        let signals = self
            .signals
            .iter()
            .map(|binding| {
                let matches = if binding.matches.is_empty() {
                    String::new()
                } else {
                    let matches = binding
                        .matches
                        .iter()
                        .map(|(key, value)| super::one_line(&format!("{key}={value}")))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(" ({matches})")
                };
                format!("{}{matches} → @{}", binding.signal, binding.role)
            })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            w,
            "{indent}{}",
            paint(palette::muted(), &format!("signals   {signals}"))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_preserves_receipt_spacing_and_marks_leader() {
        let mut output = anstream::StripStream::new(Vec::new());
        Roster::new(vec![
            RosterRow {
                handle: "planner".to_owned(),
                kind: "claude".to_owned(),
                model: Some("fable".to_owned()),
                leader: true,
            },
            RosterRow {
                handle: "coder".to_owned(),
                kind: "codex".to_owned(),
                model: Some("gpt-6-astra".to_owned()),
                leader: false,
            },
            RosterRow {
                handle: "reviewer".to_owned(),
                kind: "claude".to_owned(),
                model: None,
                leader: false,
            },
        ])
        .indent(2)
        .render(&mut output)
        .unwrap();

        assert_eq!(
            String::from_utf8(output.into_inner()).unwrap(),
            "  @planner    claude  fable        <- leader\n  @coder      codex   gpt-6-astra\n  @reviewer   claude  -\n"
        );
    }

    #[test]
    fn roster_signals_align_with_receipt_keys() {
        let mut output = anstream::StripStream::new(Vec::new());
        Roster::new(Vec::new())
            .signals(vec![RosterSignal {
                signal: "ci.failed".to_owned(),
                matches: BTreeMap::new(),
                role: "coder".to_owned(),
            }])
            .indent(2)
            .render(&mut output)
            .unwrap();

        assert_eq!(
            String::from_utf8(output.into_inner()).unwrap(),
            "  signals   ci.failed → @coder\n"
        );
    }

    #[test]
    fn roster_signals_include_sorted_matches_and_multiple_bindings() {
        let mut output = anstream::StripStream::new(Vec::new());
        Roster::new(Vec::new())
            .signals(vec![
                RosterSignal {
                    signal: "ci.failed".to_owned(),
                    matches: BTreeMap::from([
                        ("path".to_owned(), "/x".to_owned()),
                        ("branch".to_owned(), "main".to_owned()),
                    ]),
                    role: "coder".to_owned(),
                },
                RosterSignal {
                    signal: "pr.opened".to_owned(),
                    matches: BTreeMap::new(),
                    role: "reviewer".to_owned(),
                },
            ])
            .indent(2)
            .render(&mut output)
            .unwrap();

        assert_eq!(
            String::from_utf8(output.into_inner()).unwrap(),
            "  signals   ci.failed (branch=main, path=/x) → @coder, pr.opened → @reviewer\n"
        );
    }
}
