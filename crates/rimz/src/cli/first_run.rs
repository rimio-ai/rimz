//! Shared first-run appearance setup.

use std::io::{BufRead, Write};

use anyhow::Result;
use rimz::config::{MachineConfig, ThemeStyle};

use super::{config, render};

const HEADER_RULE_WIDTH: usize = 48;
const CONSENT_INTRO: &str = "Rimz routes attention across your coding agents into one sidebar.";

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
        write_header(&mut out)?;
        writeln!(out)?;
        writeln!(out, "{CONSENT_INTRO}")?;
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
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::MUTED,
            "  Y turns on the rich look above. Pick N if you see flat color bands,"
        )
    )?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::MUTED,
            "  boxes, or ? marks — RimZ falls back to plain colors and text glyphs."
        )
    )?;
    writeln!(out)?;

    let Some(modern_style) = prompt_bool(
        "  Enable the rich sidebar look?",
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
    let loop_hint = format!("Hands-off loop knobs: {loop_path}");
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::MUTED,
            "Next → docs/guide/setup.md · rimz config for preferences"
        )
    )?;
    writeln!(out, "{}", render::paint(render::palette::MUTED, &loop_hint))?;
    Ok(())
}

pub(crate) fn write_header(out: &mut dyn Write) -> Result<()> {
    writeln!(
        out,
        "{}",
        render::paint(render::palette::ACCENT.bold(), "rimz · first-run setup")
    )?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::FAINT,
            &header_rule(render::terminal_columns(80))
        )
    )?;
    Ok(())
}

fn header_rule(term_cols: usize) -> String {
    "─".repeat(term_cols.min(HEADER_RULE_WIDTH))
}

const GRADIENT_WIDTH: usize = 36;

fn probe_lines() -> Vec<String> {
    let gradient = rimz::sidebar_pane::render::nerd_font_probe_gradient(GRADIENT_WIDTH)
        .into_iter()
        .map(|(r, g, b)| format!("\x1b[38;2;{r};{g};{b}m█"))
        .chain(std::iter::once(String::from("\x1b[0m")))
        .collect::<String>();
    let glyphs = rimz::sidebar_pane::render::nerd_font_probe_glyphs().join("  ");
    vec![
        format!("  {gradient}  ← a full spread of colors (truecolor)"),
        format!("  {glyphs}  ← eight distinct icons (glyph font)"),
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
        assert!(rendered.contains("Enable the rich sidebar look?"));
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
        assert!(rendered.contains("Enable the rich sidebar look?"));
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

        assert!(lines.contains("\x1b[38;2;"));
        assert!(lines.contains('█'));
        assert!(lines.contains("a full spread of colors (truecolor)"));
        assert!(lines.contains(sample));
        assert!(lines.contains("eight distinct icons (glyph font)"));
    }

    #[test]
    fn probe_gradient_steps_smoothly_between_cells() {
        let stops = rimz::sidebar_pane::render::nerd_font_probe_gradient(GRADIENT_WIDTH);

        assert_eq!(stops.len(), GRADIENT_WIDTH);
        // Adjacent cells differ by small perceptual steps: interpolating the
        // anchors keeps neighbours close, which is what reads as one sweep
        // rather than the hard bands the raw anchor list produced.
        for pair in stops.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let delta = (i32::from(a.0) - i32::from(b.0)).abs()
                + (i32::from(a.1) - i32::from(b.1)).abs()
                + (i32::from(a.2) - i32::from(b.2)).abs();
            // Raw adjacent anchors jump by ~200; interpolation keeps every
            // cell-to-cell step well under this bound.
            assert!(delta <= 80, "harsh jump {a:?} -> {b:?} (delta {delta})");
        }
    }

    #[test]
    fn rendered_flow_names_each_question_once() {
        let defaults = Defaults {
            modern_style: false,
            pet_enabled: false,
        };

        let (_, rendered) = drive(defaults, b"\n\n");

        assert_eq!(rendered.matches("Enable the rich sidebar look?").count(), 1);
        assert_eq!(rendered.matches("Want a pet?").count(), 1);
    }

    #[test]
    fn header_uses_title_and_terminal_width_rule_without_box() {
        let rendered = strip(|w| write_header(w));

        assert!(rendered.contains("rimz · first-run setup"));
        assert!(rendered.contains('─'));
        assert!(!rendered.contains('╭'));
        assert!(!rendered.contains('╰'));
        assert_eq!(header_rule(80).chars().count(), 48);
        assert_eq!(header_rule(20).chars().count(), 20);
    }

    #[test]
    fn next_steps_are_muted_setup_config_and_loop_pointers() {
        let rendered = strip(|w| write_next_steps(w));

        assert!(rendered.contains("docs/guide/setup.md"));
        assert!(rendered.contains("rimz config"));
        assert!(rendered.contains("loop.toml"));
        assert!(rendered.contains("Hands-off loop knobs:"));
    }
}
