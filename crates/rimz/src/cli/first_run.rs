//! Shared first-run appearance setup.

use std::io::{BufRead, Write};

use anyhow::Result;
use rimz::config::{CellAspect, ColorDepth, ConfigEditor, MachineConfig, PetsConfig};
use rimz::sidebar_pane::pets::{self, PetRenderTier};

use super::list_pets::{LiveGraphicsPacer, write_pet_row, write_pixel_pet_row_with_pacer};
use super::render;

const HEADER_RULE_WIDTH: usize = 48;
const CONSENT_INTRO: &str = "Rimz routes attention across your coding agents into one sidebar.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Defaults {
    truecolor: bool,
    nerd_font: bool,
    pet_enabled: bool,
}

impl Defaults {
    pub(crate) fn from_config(config: &MachineConfig, truecolor_advertised: bool) -> Self {
        Self {
            truecolor: config
                .theme
                .effective_theme_mode()
                .depth(truecolor_advertised)
                == ColorDepth::Truecolor,
            nerd_font: config.theme.glyph_set_source().as_deref() == Some("nerd_font"),
            pet_enabled: config.theme.pets.enabled,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Answers {
    defaults: Defaults,
    truecolor: bool,
    nerd_font: bool,
    pet_enabled: bool,
}

pub(crate) fn run(defaults: Defaults, pets_config: PetsConfig, intro_rendered: bool) -> Result<()> {
    let pet_preview = std::thread::spawn(move || build_pet_art(pets_config));
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = render::err();
    if !intro_rendered {
        write_header(&mut out)?;
        writeln!(out)?;
        writeln!(out, "{CONSENT_INTRO}")?;
        writeln!(out)?;
    }
    let answers = ask(
        defaults,
        move || pet_preview.join().ok().flatten(),
        &mut input,
        &mut out,
    )?;
    apply(&answers, &mut out)?;
    write_next_steps(&mut out)
}

pub(crate) fn ask(
    defaults: Defaults,
    pet_art: impl FnOnce() -> Option<String>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Answers> {
    writeln!(out, "{}", gradient_line())?;
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::MUTED,
            "  Y if the bar above is one smooth sweep; N if it breaks into flat"
        )
    )?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::MUTED,
            "  bands — RimZ then falls back to 256 colors."
        )
    )?;
    writeln!(out)?;

    let Some(truecolor) = prompt_bool("  Use truecolor?", defaults.truecolor, input, out)? else {
        return Ok(Answers {
            defaults,
            truecolor: defaults.truecolor,
            nerd_font: defaults.nerd_font,
            pet_enabled: defaults.pet_enabled,
        });
    };
    writeln!(out)?;

    writeln!(out, "{}", glyph_line())?;
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::MUTED,
            "  Y if you see eight distinct icons; N for boxes or ? marks — RimZ"
        )
    )?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::MUTED,
            "  then falls back to plain text glyphs. (Needs a Nerd Font.)"
        )
    )?;
    writeln!(out)?;

    let Some(nerd_font) = prompt_bool("  Use Nerd Font icons?", defaults.nerd_font, input, out)?
    else {
        return Ok(Answers {
            defaults,
            truecolor,
            nerd_font: defaults.nerd_font,
            pet_enabled: defaults.pet_enabled,
        });
    };
    writeln!(out)?;

    if let Some(art) = pet_art() {
        out.write_all(art.as_bytes())?;
        writeln!(out)?;
    }

    let Some(pet_enabled) = prompt_bool(
        "  Want a pet? It lives in the sidebar and reacts to your fleet.",
        defaults.pet_enabled,
        input,
        out,
    )?
    else {
        return Ok(Answers {
            defaults,
            truecolor,
            nerd_font,
            pet_enabled: defaults.pet_enabled,
        });
    };

    Ok(Answers {
        defaults,
        truecolor,
        nerd_font,
        pet_enabled,
    })
}

pub(crate) fn apply(answers: &Answers, out: &mut dyn Write) -> Result<()> {
    let editor = ConfigEditor::machine();
    if answers.truecolor != answers.defaults.truecolor {
        editor.set(
            "theme.mode",
            if answers.truecolor {
                "truecolor"
            } else {
                "256"
            },
        )?;
        writeln!(
            out,
            "✓ {}",
            if answers.truecolor {
                "truecolor"
            } else {
                "256-color palette"
            }
        )?;
    }

    if answers.nerd_font != answers.defaults.nerd_font {
        editor.set(
            "theme.glyphs.set",
            if answers.nerd_font {
                "nerd_font"
            } else {
                "unicode"
            },
        )?;
        writeln!(
            out,
            "✓ {}",
            if answers.nerd_font {
                "Nerd Font icons"
            } else {
                "Unicode glyphs"
            }
        )?;
    }

    if answers.pet_enabled {
        editor.set("theme.pets.enabled", "true")?;
        writeln!(out, "✓ rocky joins the room (rimz list-pets: more)")?;
    } else if answers.defaults.pet_enabled {
        editor.set("theme.pets.enabled", "false")?;
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

fn gradient_line() -> String {
    let gradient = rimz::sidebar_pane::render::nerd_font_probe_gradient(GRADIENT_WIDTH)
        .into_iter()
        .map(|(r, g, b)| format!("\x1b[38;2;{r};{g};{b}m█"))
        .chain(std::iter::once(String::from("\x1b[0m")))
        .collect::<String>();
    format!("  {gradient}  ← a full spread of colors")
}

fn glyph_line() -> String {
    let glyphs = rimz::sidebar_pane::render::nerd_font_probe_glyphs().join("    ");
    format!("  {glyphs}  ← eight distinct icons")
}

fn build_pet_art(pets_config: PetsConfig) -> Option<String> {
    let (caps, wrap_pixels) = pets::detect_pixel_render_env();
    let tier = pets::resolve_render_tier(pets_config.glyphs, caps);
    let mut buf = Vec::new();
    match tier {
        PetRenderTier::Pixel => {
            let preview = pets::load_pixel_preview(&pets_config.pet)?;
            preview.frame.as_ref().ok()?;
            write_pixel_pet_row_with_pacer(
                &mut buf,
                &[(1, preview)],
                wrap_pixels,
                None::<&mut LiveGraphicsPacer>,
            )
            .ok()?;
        }
        PetRenderTier::Cell => {
            let aspect = pets_config
                .cell_aspect
                .or_else(pets::probe_cell_aspect)
                .unwrap_or(CellAspect::NEUTRAL);
            let slot = pets::dashboard_pet_size(tier);
            let preview = pets::load_cell_preview(&pets_config.pet, slot, aspect)?;
            preview.grid.as_ref().ok()?;
            write_pet_row(&mut buf, &[preview], slot).ok()?;
        }
    }
    String::from_utf8(buf).ok()
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

    use rimz::config::{ThemeMode, ThemeStyle};

    use super::*;

    fn strip(render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> Result<()>) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_one(&mut stream).expect("render");
        String::from_utf8(stream.into_inner()).expect("utf8")
    }

    fn drive(defaults: Defaults, input: &[u8]) -> (Answers, String) {
        drive_with_art(defaults, input, || None)
    }

    fn drive_with_art(
        defaults: Defaults,
        input: &[u8],
        pet_art: impl FnOnce() -> Option<String>,
    ) -> (Answers, String) {
        let mut input = Cursor::new(input.to_vec());
        let mut stream = anstream::StripStream::new(Vec::new());
        let answers = ask(defaults, pet_art, &mut input, &mut stream).expect("ask");
        let rendered = String::from_utf8(stream.into_inner()).expect("utf8");
        (answers, rendered)
    }

    #[test]
    fn prompt_accepts_declines_and_defaults() {
        let defaults = Defaults {
            truecolor: false,
            nerd_font: false,
            pet_enabled: false,
        };

        let (answers, rendered) = drive(defaults, b"y\ny\nn\n");

        assert!(answers.truecolor);
        assert!(answers.nerd_font);
        assert!(!answers.pet_enabled);
        assert!(rendered.contains("Use truecolor?"));
        assert!(rendered.contains("Use Nerd Font icons?"));
        assert!(rendered.contains("Want a pet?"));
        assert_eq!(rendered.matches("[y/N]").count(), 3);
    }

    #[test]
    fn prompt_eof_cascades_remaining_defaults() {
        let defaults = Defaults {
            truecolor: true,
            nerd_font: false,
            pet_enabled: true,
        };

        let (at_truecolor, rendered) = drive(defaults, b"");
        assert_eq!(
            at_truecolor,
            Answers {
                defaults,
                truecolor: true,
                nerd_font: false,
                pet_enabled: true,
            }
        );
        assert!(rendered.contains("Use truecolor?"));
        assert!(!rendered.contains("Use Nerd Font icons?"));

        let (at_nerd_font, rendered) = drive(defaults, b"n\n");
        assert!(!at_nerd_font.truecolor);
        assert!(!at_nerd_font.nerd_font);
        assert!(at_nerd_font.pet_enabled);
        assert!(rendered.contains("Use Nerd Font icons?"));
        assert!(!rendered.contains("Want a pet?"));

        let (at_pet, rendered) = drive(defaults, b"n\ny\n");
        assert!(!at_pet.truecolor);
        assert!(at_pet.nerd_font);
        assert!(at_pet.pet_enabled);
        assert!(rendered.contains("Want a pet?"));
    }

    #[test]
    fn rerun_defaults_flip_with_no_answers() {
        let defaults = Defaults {
            truecolor: true,
            nerd_font: true,
            pet_enabled: true,
        };

        let (answers, rendered) = drive(defaults, b"n\nn\nn\n");

        assert!(!answers.truecolor);
        assert!(!answers.nerd_font);
        assert!(!answers.pet_enabled);
        assert_eq!(rendered.matches("[Y/n]").count(), 3);
    }

    #[test]
    fn defaults_fold_terminal_advertisement_and_explicit_theme_choices() {
        let fresh = MachineConfig::default();
        assert_eq!(
            Defaults::from_config(&fresh, false),
            Defaults {
                truecolor: false,
                nerd_font: false,
                pet_enabled: false,
            }
        );
        assert!(Defaults::from_config(&fresh, true).truecolor);

        let mut modern = MachineConfig::default();
        modern.theme.style = Some(ThemeStyle::Modern);
        assert_eq!(
            Defaults::from_config(&modern, false),
            Defaults {
                truecolor: true,
                nerd_font: true,
                pet_enabled: false,
            }
        );

        modern.theme.mode = ThemeMode::Indexed;
        modern.theme.glyphs.set = Some("unicode".to_owned());
        let explicit_fallbacks = Defaults::from_config(&modern, true);
        assert!(!explicit_fallbacks.truecolor);
        assert!(!explicit_fallbacks.nerd_font);

        let mut explicit_truecolor = MachineConfig::default();
        explicit_truecolor.theme.mode = ThemeMode::Truecolor;
        explicit_truecolor.theme.glyphs.set = Some("nerd_font".to_owned());
        let explicit = Defaults::from_config(&explicit_truecolor, false);
        assert!(explicit.truecolor);
        assert!(explicit.nerd_font);
    }

    #[test]
    fn probe_lines_emit_truecolor_and_aligned_sidebar_glyphs() {
        let gradient = gradient_line();
        let glyphs = glyph_line();
        let sample = rimz::sidebar_pane::render::nerd_font_probe_glyphs()[0];

        assert!(gradient.contains("\x1b[38;2;"));
        assert!(gradient.contains('█'));
        assert!(gradient.contains("a full spread of colors"));
        assert!(glyphs.contains(sample));
        assert!(glyphs.contains("eight distinct icons"));
        let glyph_field = glyphs
            .strip_prefix("  ")
            .and_then(|line| line.split("  ←").next())
            .expect("glyph field");
        assert_eq!(glyph_field.chars().count(), GRADIENT_WIDTH);
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
            truecolor: false,
            nerd_font: false,
            pet_enabled: false,
        };

        let (_, rendered) = drive(defaults, b"\n\n\n");

        assert_eq!(rendered.matches("Use truecolor?").count(), 1);
        assert_eq!(rendered.matches("Use Nerd Font icons?").count(), 1);
        assert_eq!(rendered.matches("Want a pet?").count(), 1);
    }

    #[test]
    fn pet_art_is_injected_only_when_available() {
        let defaults = Defaults {
            truecolor: false,
            nerd_font: false,
            pet_enabled: false,
        };

        let (_, with_art) = drive_with_art(defaults, b"\n\n\n", || Some("ART\n".to_owned()));
        let nerd = with_art.find("Use Nerd Font icons?").expect("nerd prompt");
        let art = with_art.find("ART\n\n").expect("art with blank line");
        let pet = with_art.find("Want a pet?").expect("pet prompt");
        assert!(nerd < art && art < pet);

        let (_, without_art) = drive(defaults, b"\n\n\n");
        assert!(!without_art.contains("ART"));
        assert!(without_art.contains("[y/N] \n  Want a pet?"));
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
