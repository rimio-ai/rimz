//! Shared first-run appearance setup.

use std::io::{BufRead, Write};

use anyhow::Result;
use rimz::config::{MachineConfig, ThemeStyle};
use unicode_width::UnicodeWidthStr;

use super::{config, render};

const CARD_TEXT_WIDTH: usize = 44;
pub(crate) const CONSENT_REVERSIBLE: &str = "Reversible any time with `rimz hooks uninstall`.";
const CONSENT_INTRO: &str = "Rimz routes attention across your coding agents into one sidebar.";
const CONSENT_BOUNDARY: &str =
    "These hooks only report events to Rimz. They never answer a prompt for you.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Defaults {
    modern_style: bool,
    pet_enabled: bool,
}

impl Defaults {
    pub(crate) fn from_config(config: &MachineConfig) -> Self {
        Self {
            modern_style: config.theme.style == Some(ThemeStyle::Modern),
            pet_enabled: config.theme.pets.enabled,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Answers {
    defaults: Defaults,
    modern_style: bool,
    pet_enabled: bool,
}

pub(crate) fn run(defaults: Defaults, intro_rendered: bool) -> Result<()> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = render::err();
    if !intro_rendered {
        write_intro_card(&mut out)?;
        writeln!(out)?;
    }
    let answers = ask(defaults, &mut input, &mut out)?;
    apply(&answers, &mut out)?;
    write_next_steps(&mut out)
}

pub(crate) fn ask(
    defaults: Defaults,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Answers> {
    for line in probe_lines() {
        writeln!(out, "{line}")?;
    }
    writeln!(out)?;

    let Some(modern_style) = prompt_bool(
        "  Icons and gradient render cleanly?",
        defaults.modern_style,
        input,
        out,
    )?
    else {
        return Ok(Answers {
            defaults,
            modern_style: defaults.modern_style,
            pet_enabled: defaults.pet_enabled,
        });
    };

    let Some(pet_enabled) = prompt_bool(
        "  Want a pet? It lives in the sidebar and reacts to your fleet.",
        defaults.pet_enabled,
        input,
        out,
    )?
    else {
        return Ok(Answers {
            defaults,
            modern_style,
            pet_enabled: defaults.pet_enabled,
        });
    };

    Ok(Answers {
        defaults,
        modern_style,
        pet_enabled,
    })
}

pub(crate) fn apply(answers: &Answers, out: &mut dyn Write) -> Result<()> {
    if answers.modern_style {
        config::set_config_key("theme.style", "modern")?;
        writeln!(out, "✓ modern style: truecolor + Nerd Font icons")?;
    } else if answers.defaults.modern_style {
        config::set_config_key("theme.style", "default")?;
        writeln!(out, "✓ default style: auto color + Unicode glyphs")?;
    }

    if answers.pet_enabled {
        config::set_config_key("theme.pets.enabled", "true")?;
        writeln!(out, "✓ rocky joins the room (rimz list-pets: more)")?;
    } else if answers.defaults.pet_enabled {
        config::set_config_key("theme.pets.enabled", "false")?;
        writeln!(out, "✓ pet disabled")?;
    }
    Ok(())
}

pub(crate) fn write_next_steps(out: &mut dyn Write) -> Result<()> {
    let loop_path = rimz::config::MachineConfig::loop_path();
    let loop_path = render::home_relative(&loop_path.display().to_string());
    let loop_hint = format!("Hands-off loop knobs live in {loop_path}.");
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::FAINT,
            "Next: docs/guide/setup.md for setup, `rimz config` for preferences."
        )
    )?;
    writeln!(out, "{}", render::paint(render::palette::FAINT, &loop_hint))?;
    Ok(())
}

pub(crate) fn write_intro_card(out: &mut dyn Write) -> Result<()> {
    for line in intro_card_lines(render::terminal_columns(80)) {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

pub(crate) fn intro_card_lines(term_cols: usize) -> Vec<String> {
    let card_text = intro_card_text();
    let box_width = CARD_TEXT_WIDTH + 4;
    if term_cols < box_width {
        return card_text
            .iter()
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("  {line}")
                }
            })
            .collect();
    }

    let rule = "─".repeat(CARD_TEXT_WIDTH + 2);
    let mut lines = Vec::with_capacity(card_text.len() + 2);
    lines.push(format!("╭{rule}╮"));
    for line in card_text {
        let pad = CARD_TEXT_WIDTH.saturating_sub(line.width());
        lines.push(format!("│ {line}{:pad$} │", "", pad = pad));
    }
    lines.push(format!("╰{rule}╯"));
    lines
}

fn intro_card_text() -> Vec<String> {
    let mut lines = vec!["rimz · first-run setup".to_owned(), String::new()];
    lines.extend(wrap_words(CONSENT_INTRO, CARD_TEXT_WIDTH));
    lines.push(String::new());
    lines.extend(wrap_words(CONSENT_BOUNDARY, CARD_TEXT_WIDTH));
    lines
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let next_width = if current.is_empty() {
            word.width()
        } else {
            current.width() + 1 + word.width()
        };
        if next_width > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn probe_lines() -> Vec<String> {
    let gradient = [
        (125, 207, 255),
        (105, 192, 255),
        (122, 162, 247),
        (146, 138, 255),
        (187, 154, 247),
        (247, 118, 142),
        (255, 158, 100),
        (224, 175, 104),
        (158, 206, 106),
        (115, 218, 202),
        (42, 195, 222),
        (125, 207, 255),
    ]
    .into_iter()
    .map(|(r, g, b)| format!("\x1b[48;2;{r};{g};{b}m▐\x1b[0m"))
    .collect::<String>();
    let glyphs = rimz::sidebar_pane::render::nerd_font_probe_glyphs().join("  ");
    vec![
        format!("  {gradient}  (smooth color gradient)"),
        format!("  {glyphs}  (distinct icons)"),
    ]
}

fn prompt_bool(
    prompt: &str,
    default_yes: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Option<bool>> {
    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    loop {
        write!(
            out,
            "{prompt} {} ",
            render::paint(render::palette::ACCENT.bold(), suffix)
        )?;
        out.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            writeln!(out)?;
            return Ok(None);
        }
        let answer = answer.trim();
        if answer.is_empty() {
            return Ok(Some(default_yes));
        }
        if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
            return Ok(Some(true));
        }
        if answer.eq_ignore_ascii_case("n") || answer.eq_ignore_ascii_case("no") {
            return Ok(Some(false));
        }
        writeln!(out, "  Enter y or n.")?;
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn strip(render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> Result<()>) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_one(&mut stream).expect("render");
        String::from_utf8(stream.into_inner()).expect("utf8")
    }

    fn drive(defaults: Defaults, input: &[u8]) -> (Answers, String) {
        let mut input = Cursor::new(input.to_vec());
        let mut stream = anstream::StripStream::new(Vec::new());
        let answers = ask(defaults, &mut input, &mut stream).expect("ask");
        let rendered = String::from_utf8(stream.into_inner()).expect("utf8");
        (answers, rendered)
    }

    #[test]
    fn prompt_accepts_declines_and_defaults() {
        let defaults = Defaults {
            modern_style: false,
            pet_enabled: false,
        };

        let (answers, rendered) = drive(defaults, b"y\nn\n");

        assert!(answers.modern_style);
        assert!(!answers.pet_enabled);
        assert!(rendered.contains("Icons and gradient render cleanly?"));
        assert!(rendered.contains("Want a pet?"));
        assert_eq!(rendered.matches("[y/N]").count(), 2);
    }

    #[test]
    fn prompt_eof_keeps_defaults() {
        let defaults = Defaults {
            modern_style: true,
            pet_enabled: true,
        };

        let (answers, rendered) = drive(defaults, b"");

        assert!(answers.modern_style);
        assert!(answers.pet_enabled);
        assert!(rendered.contains("Icons and gradient render cleanly?"));
        assert!(!rendered.contains("Want a pet?"));
    }

    #[test]
    fn rerun_defaults_flip_with_no_answers() {
        let defaults = Defaults {
            modern_style: true,
            pet_enabled: true,
        };

        let (answers, rendered) = drive(defaults, b"n\nn\n");

        assert!(!answers.modern_style);
        assert!(!answers.pet_enabled);
        assert_eq!(rendered.matches("[Y/n]").count(), 2);
    }

    #[test]
    fn probe_lines_emit_truecolor_and_real_sidebar_glyphs() {
        let lines = probe_lines().join("\n");
        let sample = rimz::sidebar_pane::render::nerd_font_probe_glyphs()[0];

        assert!(lines.contains("\x1b[48;2;"));
        assert!(lines.contains("(smooth color gradient)"));
        assert!(lines.contains(sample));
        assert!(lines.contains("(distinct icons)"));
    }

    #[test]
    fn rendered_flow_names_each_question_once() {
        let defaults = Defaults {
            modern_style: false,
            pet_enabled: false,
        };

        let (_, rendered) = drive(defaults, b"\n\n");

        assert_eq!(
            rendered
                .matches("Icons and gradient render cleanly?")
                .count(),
            1
        );
        assert_eq!(rendered.matches("Want a pet?").count(), 1);
    }

    #[test]
    fn intro_card_lines_use_border_when_wide_and_plain_when_narrow() {
        let wide = intro_card_lines(80).join("\n");
        assert!(wide.contains('╭'));
        assert!(wide.contains('╰'));

        let narrow = intro_card_lines(20).join("\n");
        assert!(!narrow.contains('╭'));
        assert!(narrow.contains("These hooks only report events to Rimz."));
        assert!(narrow.contains("never answer a prompt for you."));
    }

    #[test]
    fn next_steps_are_faint_setup_and_config_pointers() {
        let rendered = strip(|w| write_next_steps(w));

        assert!(rendered.contains("docs/guide/setup.md"));
        assert!(rendered.contains("rimz config"));
        assert!(rendered.contains("loop.toml"));
    }
}
