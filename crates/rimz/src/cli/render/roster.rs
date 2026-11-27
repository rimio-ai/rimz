//! Shared agent roster rows and declared signal bindings.

use std::collections::BTreeMap;
use std::io::Write;

use unicode_width::UnicodeWidthStr;

use super::{paint, palette};

pub(crate) struct RosterRow {
    pub handle: String,
    pub kind: String,
    pub login: Option<rimz::ids::LoginName>,
    pub model: Option<String>,
    pub tier_note: Option<String>,
    pub tier: Option<rimz::agents::TierStamp>,
    pub leader: bool,
}

pub(crate) struct RosterSignal {
    pub signal: String,
    pub matches: BTreeMap<String, String>,
    pub role: String,
}

/// Agent roster rows. Each handle pads to the longest handle plus one before the two-space
/// separator, so a handle one character shorter than the longest carries four spaces, not
/// three.
pub(crate) struct Roster {
    rows: Vec<RosterRow>,
    signals: Vec<RosterSignal>,
    signal_width: usize,
    indent: usize,
}

impl Roster {
    pub(crate) fn new(rows: Vec<RosterRow>) -> Self {
        Self {
            rows,
            signals: Vec::new(),
            signal_width: usize::MAX,
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

    pub(crate) fn signal_width(mut self, width: usize) -> Self {
        self.signal_width = width;
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
            .map(|row| {
                row.kind.len() + row.login.as_ref().map_or(0, |name| name.as_str().len() + 1)
            })
            .max()
            .unwrap_or(0);
        let formatted_models: Vec<String> = self.rows.iter().map(formatted_model).collect();
        let model_width = formatted_models
            .iter()
            .map(|model| UnicodeWidthStr::width(model.as_str()))
            .max()
            .unwrap_or(0);
        for (row, mut model) in self.rows.iter().zip(formatted_models) {
            let handle = format!("@{:<handle_width$}", row.handle);
            let kind = row
                .login
                .as_ref()
                .map_or_else(|| row.kind.clone(), |name| format!("{}@{name}", row.kind));
            let kind = format!("{kind:<kind_width$}");
            if let Some(tier) = row.tier.as_ref().filter(|tier| tier.moved()) {
                let passes = tier
                    .passes()
                    .map(|pass| pass.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                model.push_str(&format!(" ({}; {passes})", tier.tier));
            }
            let model = if row.leader {
                let pad = model_width.saturating_sub(UnicodeWidthStr::width(model.as_str()));
                format!("{model}{}  <- leader", " ".repeat(pad))
            } else {
                model
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
            "{}",
            paint(
                palette::muted(),
                &super::clip_to_width(&format!("{indent}signals   {signals}"), self.signal_width)
            )
        )
    }
}

fn formatted_model(row: &RosterRow) -> String {
    let mut model = row.model.as_deref().map_or_else(
        || "-".to_owned(),
        |model| rimz::agents::model_display::display_model_for_kind(model, Some(row.kind.as_str())),
    );
    if let Some(note) = row.tier_note.as_deref() {
        model.push_str(&format!(" ({note})"));
    }
    model
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
                login: None,
                model: Some("fable".to_owned()),
                tier_note: None,
                tier: None,
                leader: true,
            },
            RosterRow {
                handle: "coder".to_owned(),
                kind: "codex".to_owned(),
                login: None,
                model: Some("gpt-6-astra".to_owned()),
                tier_note: None,
                tier: None,
                leader: false,
            },
            RosterRow {
                handle: "reviewer".to_owned(),
                kind: "claude".to_owned(),
                login: None,
                model: None,
                tier_note: None,
                tier: None,
                leader: false,
            },
        ])
        .indent(2)
        .render(&mut output)
        .unwrap();

        assert_eq!(
            String::from_utf8(output.into_inner()).unwrap(),
            "  @planner    claude  Fable        <- leader\n  @coder      codex   GPT 6 Astra\n  @reviewer   claude  -\n"
        );
    }

    #[test]
    fn roster_names_every_tier_pass_only_when_moved() {
        let mut stamp: rimz::agents::TierStamp = serde_json::from_value(serde_json::json!({
            "tier": "senior", "model": "astra",
            "unbuildable": [{"model": "opus", "error": "enable the skill"}],
            "skipped": [{"model": "fable", "reason": "logged_out"}]
        }))
        .unwrap();
        for moved in [true, false] {
            if !moved {
                stamp.unbuildable.clear();
                stamp.skipped.clear();
            }
            let mut output = anstream::StripStream::new(Vec::new());
            Roster::new(vec![RosterRow {
                handle: "coder".into(),
                kind: "codex".into(),
                login: None,
                model: Some("astra".into()),
                tier_note: None,
                tier: Some(stamp.clone()),
                leader: true,
            }])
            .render(&mut output)
            .unwrap();
            let text = String::from_utf8(output.into_inner()).unwrap();
            assert_eq!(
                text,
                if moved {
                    "@coder   codex  Astra (senior; opus: not buildable; fable: logged out)  <- leader\n"
                } else {
                    "@coder   codex  Astra  <- leader\n"
                }
            );
        }
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

    #[test]
    fn roster_clips_long_signals_to_the_callers_width() {
        let mut output = anstream::StripStream::new(Vec::new());
        Roster::new(Vec::new())
            .signals(vec![RosterSignal {
                signal: "ci.failed".into(),
                matches: BTreeMap::from([("path".into(), "/repo/long-worktree-path".into())]),
                role: "coder".into(),
            }])
            .indent(2)
            .signal_width(30)
            .render(&mut output)
            .unwrap();
        assert_eq!(
            String::from_utf8(output.into_inner()).unwrap(),
            "  signals   ci.failed (path=/…\n"
        );
    }
}
