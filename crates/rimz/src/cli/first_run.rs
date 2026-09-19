//! Shared first-run setup: appearance probes and hands-off automation consent.

use std::io::{BufRead, Write};

use anyhow::Result;
use rimz::config::{
    CellAspect, ColorDepth, ConfigEditor, IdleCompactMode, MachineConfig, PetsConfig,
};
use rimz::sidebar_pane::pets::{self, PetRenderTier};
use rimz::theme::{nerd_font_probe_glyphs, nerd_font_probe_gradient};

use super::list_pets::{LiveGraphicsPacer, write_pet_row, write_pixel_pet_row_with_pacer};
use super::render;
use super::render::status::{self, StateRole};

const HEADER_RULE_WIDTH: usize = 48;
const CONSENT_INTRO: &str = "RimZ routes attention across your coding agents into one sidebar.";
const AUTOMATION_LABEL_WIDTH: usize = 15;

/// The three-way answer to the consent question, in prompt order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutomationChoice {
    AllOn,
    AllOff,
    Choose,
}

impl AutomationChoice {
    const ALL: [Self; 3] = [Self::AllOn, Self::AllOff, Self::Choose];
    const WORDS: [&str; 3] = ["yes", "no", "choose"];

    fn suffix_label(self, is_default: bool) -> &'static str {
        match (self, is_default) {
            (Self::AllOn, false) => "y",
            (Self::AllOn, true) => "Y",
            (Self::AllOff, false) => "n",
            (Self::AllOff, true) => "N",
            (Self::Choose, false) => "choose",
            (Self::Choose, true) => "Choose",
        }
    }
}

/// One hands-off behaviour the consent question offers: off by default
/// because it acts on its own on the agent's session or the user's wallet.
#[derive(Debug)]
struct AutomationRow {
    key: &'static str,
    label: &'static str,
    cost: &'static str,
    on_value: &'static str,
    off_value: &'static str,
    is_on: fn(&MachineConfig) -> bool,
    needs_codex: bool,
}

// Rows are compared by their config key: the table holds each key once.
impl PartialEq for AutomationRow {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for AutomationRow {}

static AUTOMATION_ROWS: [AutomationRow; 3] = [
    AutomationRow {
        key: "resume.auto_continue",
        label: "auto-continue",
        cost: "types continue after a rate limit or API error",
        on_value: "true",
        off_value: "false",
        is_on: |config| config.resume.auto_continue,
        needs_codex: false,
    },
    AutomationRow {
        key: "resume.auto_redeem",
        label: "auto-redeem",
        cost: "spends a Codex reset credit when a limit blocks hours",
        on_value: "true",
        off_value: "false",
        is_on: |config| config.resume.auto_redeem,
        needs_codex: true,
    },
    AutomationRow {
        key: "harness.idle_compact",
        label: "idle compaction",
        cost: "compacts a long-idle agent while others run; lossy",
        on_value: IdleCompactMode::Auto.as_str(),
        off_value: IdleCompactMode::Off.as_str(),
        is_on: |config| config.harness.idle_compact != IdleCompactMode::Off,
        needs_codex: false,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OfferedRow {
    row: &'static AutomationRow,
    on: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Defaults {
    truecolor: bool,
    nerd_font: bool,
    pet_enabled: bool,
    automation: Vec<OfferedRow>,
}

impl Defaults {
    fn from_config(
        config: &MachineConfig,
        truecolor_advertised: bool,
        codex_detected: bool,
    ) -> Self {
        Self {
            truecolor: config
                .theme
                .effective_theme_mode()
                .depth(truecolor_advertised)
                == ColorDepth::Truecolor,
            nerd_font: config.theme.glyph_set_source() == Some("nerd_font"),
            pet_enabled: config.theme.pets.enabled,
            automation: AUTOMATION_ROWS
                .iter()
                .filter(|row| codex_detected || !row.needs_codex)
                .map(|row| OfferedRow {
                    row,
                    on: (row.is_on)(config),
                })
                .collect(),
        }
    }

    fn automation_states(&self) -> Vec<bool> {
        self.automation.iter().map(|offered| offered.on).collect()
    }

    /// The three-way answer that leaves every offered row as it is.
    fn automation_choice(&self) -> AutomationChoice {
        if self.automation.iter().all(|offered| offered.on) {
            AutomationChoice::AllOn
        } else if self.automation.iter().all(|offered| !offered.on) {
            AutomationChoice::AllOff
        } else {
            AutomationChoice::Choose
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Answers {
    defaults: Defaults,
    truecolor: bool,
    nerd_font: bool,
    pet_enabled: bool,
    /// One state per offered row, in `defaults.automation` order.
    automation: Vec<bool>,
}

pub(crate) fn run(config: &MachineConfig, intro_rendered: bool) -> Result<()> {
    let defaults = Defaults::from_config(
        config,
        rimz::tui::truecolor(),
        rimz::harness::auto_redeem::provider_located(),
    );
    let pets_config = config.theme.pets.clone();
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

fn ask(
    defaults: Defaults,
    pet_art: impl FnOnce() -> Option<String>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Answers> {
    let kept = defaults.automation_states();
    writeln!(out, "{}", gradient_line())?;
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::muted(),
            "  Y if the bar above is one smooth sweep; N if it breaks into flat"
        )
    )?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::muted(),
            "  bands — RimZ then falls back to 256 colors."
        )
    )?;
    writeln!(out)?;

    let Some(truecolor) = prompt_bool("  Use truecolor?", defaults.truecolor, input, out)? else {
        return Ok(Answers {
            truecolor: defaults.truecolor,
            nerd_font: defaults.nerd_font,
            pet_enabled: defaults.pet_enabled,
            automation: kept,
            defaults,
        });
    };
    writeln!(out)?;

    writeln!(out, "{}", glyph_line())?;
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::muted(),
            "  Y if you see eight distinct icons; N for boxes or ? marks — RimZ"
        )
    )?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::muted(),
            "  then falls back to plain text glyphs. (Needs a Nerd Font.)"
        )
    )?;
    writeln!(out)?;

    let Some(nerd_font) = prompt_bool("  Use Nerd Font icons?", defaults.nerd_font, input, out)?
    else {
        return Ok(Answers {
            truecolor,
            nerd_font: defaults.nerd_font,
            pet_enabled: defaults.pet_enabled,
            automation: kept,
            defaults,
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
            truecolor,
            nerd_font,
            pet_enabled: defaults.pet_enabled,
            automation: kept,
            defaults,
        });
    };
    writeln!(out)?;

    let automation = ask_automation(&defaults, input, out)?.unwrap_or(kept);
    Ok(Answers {
        defaults,
        truecolor,
        nerd_font,
        pet_enabled,
        automation,
    })
}

/// The consent question: one row per offered behaviour, answered for all of
/// them at once or row by row. `None` on EOF before the three-way answer; EOF
/// inside `choose` keeps the rows answered so far and defaults the rest.
fn ask_automation(
    defaults: &Defaults,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Option<Vec<bool>>> {
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::muted(),
            "  These act on their own, each action traced in `rimz stats`:"
        )
    )?;
    for offered in &defaults.automation {
        writeln!(out, "{}", automation_row_line(offered))?;
    }
    writeln!(out)?;

    let Some(choice) = prompt_choice(
        "  Enable hands-off automation?",
        defaults.automation_choice(),
        input,
        out,
    )?
    else {
        return Ok(None);
    };
    match choice {
        AutomationChoice::AllOn => return Ok(Some(vec![true; defaults.automation.len()])),
        AutomationChoice::AllOff => return Ok(Some(vec![false; defaults.automation.len()])),
        AutomationChoice::Choose => {}
    }
    let mut states = defaults.automation_states();
    for (state, offered) in states.iter_mut().zip(&defaults.automation) {
        let prompt = format!("    {}?", offered.row.label);
        let Some(on) = prompt_bool(&prompt, offered.on, input, out)? else {
            break;
        };
        *state = on;
    }
    Ok(Some(states))
}

fn automation_row_line(offered: &OfferedRow) -> String {
    let (state, role) = if offered.on {
        ("on ", StateRole::Success)
    } else {
        ("off", StateRole::Neutral)
    };
    format!(
        "  {:<width$}  {}  {}",
        offered.row.label,
        render::paint(status::role(role), state),
        render::paint(render::palette::muted(), offered.row.cost),
        width = AUTOMATION_LABEL_WIDTH,
    )
}

fn apply(answers: &Answers, out: &mut dyn Write) -> Result<()> {
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

    let mut turned_on = Vec::new();
    let mut turned_off = Vec::new();
    for (offered, &on) in answers.defaults.automation.iter().zip(&answers.automation) {
        if on == offered.on {
            continue;
        }
        let row = offered.row;
        editor.set(row.key, if on { row.on_value } else { row.off_value })?;
        if on {
            turned_on.push(row.label);
        } else {
            turned_off.push(row.label);
        }
    }
    if !turned_on.is_empty() {
        writeln!(out, "✓ {} on", turned_on.join(" + "))?;
    }
    if !turned_off.is_empty() {
        writeln!(out, "✓ {} off", turned_off.join(" + "))?;
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
            render::palette::muted(),
            "Next → docs/guide/setup.md · rimz config for preferences"
        )
    )?;
    writeln!(
        out,
        "{}",
        render::paint(render::palette::muted(), &loop_hint)
    )?;
    Ok(())
}

pub(crate) fn write_header(out: &mut dyn Write) -> Result<()> {
    writeln!(
        out,
        "{}",
        render::paint(render::palette::header(), "rimz · first-run setup")
    )?;
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::faint(),
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
    let gradient = nerd_font_probe_gradient(GRADIENT_WIDTH)
        .into_iter()
        .map(|(r, g, b)| format!("\x1b[38;2;{r};{g};{b}m█"))
        .chain(std::iter::once(String::from("\x1b[0m")))
        .collect::<String>();
    format!("  {gradient}  ← a full spread of colors")
}

fn glyph_line() -> String {
    let glyphs = nerd_font_probe_glyphs().join("    ");
    format!("  {glyphs}  ← eight distinct icons")
}

fn build_pet_art(pets_config: PetsConfig) -> Option<String> {
    let (caps, wrap_pixels) = rimz::sidebar_pane::detect_pixel_render_env();
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
            render::paint(render::palette::accent().bold(), suffix)
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

fn prompt_choice(
    prompt: &str,
    default: AutomationChoice,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Option<AutomationChoice>> {
    let labels = AutomationChoice::ALL
        .map(|choice| choice.suffix_label(choice == default))
        .join("/");
    let suffix = format!("[{labels}]");
    let default_index = AutomationChoice::ALL
        .iter()
        .position(|&choice| choice == default)
        .unwrap_or_default();
    loop {
        write!(
            out,
            "{prompt} {} ",
            render::paint(render::palette::accent().bold(), &suffix)
        )?;
        out.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            writeln!(out)?;
            return Ok(None);
        }
        if let Some(index) = super::parse_choice(&answer, &AutomationChoice::WORDS, default_index) {
            return Ok(Some(AutomationChoice::ALL[index]));
        }
        writeln!(out, "  Enter y, n, or choose.")?;
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

    fn fresh(codex_detected: bool) -> Defaults {
        Defaults::from_config(&MachineConfig::default(), false, codex_detected)
    }

    fn all_on(codex_detected: bool) -> Defaults {
        let mut config = MachineConfig::default();
        config.theme.mode = ThemeMode::Truecolor;
        config.theme.glyphs.set = Some("nerd_font".to_owned());
        config.theme.pets.enabled = true;
        config.resume.auto_continue = true;
        config.resume.auto_redeem = true;
        config.harness.idle_compact = IdleCompactMode::Auto;
        Defaults::from_config(&config, false, codex_detected)
    }

    fn offered_keys(defaults: &Defaults) -> Vec<&'static str> {
        defaults
            .automation
            .iter()
            .map(|offered| offered.row.key)
            .collect()
    }

    #[test]
    fn prompt_accepts_declines_and_defaults() {
        let (answers, rendered) = drive(fresh(false), b"y\ny\nn\n\n");

        assert!(answers.truecolor);
        assert!(answers.nerd_font);
        assert!(!answers.pet_enabled);
        assert_eq!(answers.automation, vec![false, false]);
        assert!(rendered.contains("Use truecolor?"));
        assert!(rendered.contains("Use Nerd Font icons?"));
        assert!(rendered.contains("Want a pet?"));
        assert!(rendered.contains("Enable hands-off automation? [y/N/choose]"));
        assert!(rendered.contains("traced in `rimz stats`"));
        assert_eq!(rendered.matches("[y/N] ").count(), 3);
        assert_eq!(rendered.matches("[Y/n] ").count(), 0);
    }

    #[test]
    fn prompt_eof_cascades_remaining_defaults() {
        let defaults = Defaults {
            nerd_font: false,
            ..all_on(true)
        };

        let (at_truecolor, rendered) = drive(defaults.clone(), b"");
        assert_eq!(
            at_truecolor,
            Answers {
                defaults: defaults.clone(),
                truecolor: true,
                nerd_font: false,
                pet_enabled: true,
                automation: vec![true, true, true],
            }
        );
        assert!(rendered.contains("Use truecolor?"));
        assert!(!rendered.contains("Use Nerd Font icons?"));

        let (at_nerd_font, rendered) = drive(defaults.clone(), b"n\n");
        assert!(!at_nerd_font.truecolor);
        assert!(!at_nerd_font.nerd_font);
        assert!(at_nerd_font.pet_enabled);
        assert_eq!(at_nerd_font.automation, vec![true, true, true]);
        assert!(rendered.contains("Use Nerd Font icons?"));
        assert!(!rendered.contains("Want a pet?"));

        let (at_pet, rendered) = drive(defaults.clone(), b"n\ny\n");
        assert!(at_pet.nerd_font);
        assert!(at_pet.pet_enabled);
        assert_eq!(at_pet.automation, vec![true, true, true]);
        assert!(rendered.contains("Want a pet?"));

        let (at_automation, rendered) = drive(defaults.clone(), b"n\ny\nn\n");
        assert!(!at_automation.pet_enabled);
        assert_eq!(at_automation.automation, vec![true, true, true]);
        assert!(rendered.contains("Enable hands-off automation?"));

        let (inside_choose, rendered) = drive(defaults, b"n\ny\nn\nc\nn\n");
        assert_eq!(inside_choose.automation, vec![false, true, true]);
        assert!(rendered.contains("    auto-redeem?"));
        assert!(!rendered.contains("    idle compaction?"));
    }

    #[test]
    fn rerun_defaults_flip_with_no_answers() {
        let (answers, rendered) = drive(all_on(true), b"n\nn\nn\nn\n");

        assert!(!answers.truecolor);
        assert!(!answers.nerd_font);
        assert!(!answers.pet_enabled);
        assert_eq!(answers.automation, vec![false, false, false]);
        assert_eq!(rendered.matches("[Y/n] ").count(), 3);
        assert_eq!(rendered.matches("[Y/n/choose] ").count(), 1);
    }

    #[test]
    fn enter_keeps_every_offered_row_in_each_state() {
        for defaults in [fresh(true), all_on(true)] {
            let states = defaults.automation_states();
            let (answers, _) = drive(defaults, b"\n\n\n\n");
            assert_eq!(answers.automation, states);
        }

        let mut mixed = MachineConfig::default();
        mixed.resume.auto_continue = true;
        let mixed = Defaults::from_config(&mixed, false, true);
        let (answers, rendered) = drive(mixed.clone(), b"\n\n\n\n\n\n\n");
        assert!(rendered.contains("[y/n/Choose]"));
        assert_eq!(answers.automation, vec![true, false, false]);
        assert_eq!(rendered.matches("    auto-continue? [Y/n]").count(), 1);
        assert_eq!(rendered.matches("    idle compaction? [y/N]").count(), 1);

        let (answers, _) = drive(mixed, b"\n\n\nn\n");
        assert_eq!(answers.automation, vec![false, false, false]);
    }

    #[test]
    fn codex_gate_hides_and_never_touches_auto_redeem() {
        let mut config = MachineConfig::default();
        config.resume.auto_redeem = true;

        let without = Defaults::from_config(&config, false, false);
        assert_eq!(
            offered_keys(&without),
            ["resume.auto_continue", "harness.idle_compact"]
        );
        assert_eq!(without.automation_choice(), AutomationChoice::AllOff);
        for input in [&b"\n\n\ny\n"[..], b"\n\n\nn\n", b"\n\n\nc\ny\ny\n"] {
            let (answers, rendered) = drive(without.clone(), input);
            assert_eq!(answers.automation.len(), 2);
            assert!(!rendered.contains("auto-redeem"));
        }

        let with = Defaults::from_config(&config, false, true);
        assert_eq!(
            offered_keys(&with),
            [
                "resume.auto_continue",
                "resume.auto_redeem",
                "harness.idle_compact"
            ]
        );
        assert_eq!(with.automation_choice(), AutomationChoice::Choose);
        let (_, rendered) = drive(with, b"\n\n\ny\n");
        assert!(rendered.contains("auto-redeem"));
        assert!(rendered.contains("Codex reset credit"));
    }

    #[test]
    fn hand_set_always_reads_on_and_survives_keeping_it_on() {
        let mut config = MachineConfig::default();
        config.harness.idle_compact = IdleCompactMode::Always;
        let defaults = Defaults::from_config(&config, false, false);
        assert_eq!(defaults.automation_states(), vec![false, true]);

        for keep in [&b"\n\n\ny\n"[..], b"\n\n\n\n\n\n"] {
            let (answers, _) = drive(defaults.clone(), keep);
            assert!(answers.automation[1], "idle compaction stays on: no write");
        }
        let (answers, _) = drive(defaults, b"\n\n\nn\n");
        assert!(!answers.automation[1]);
    }

    #[test]
    fn three_way_prompt_parses_words_prefixes_and_reprompts() {
        for (answer, expected) in [
            ("y", AutomationChoice::AllOn),
            ("Y", AutomationChoice::AllOn),
            ("yes", AutomationChoice::AllOn),
            ("N", AutomationChoice::AllOff),
            ("no", AutomationChoice::AllOff),
            ("c", AutomationChoice::Choose),
            ("choose", AutomationChoice::Choose),
            ("", AutomationChoice::AllOff),
        ] {
            let mut input = Cursor::new(format!("{answer}\n").into_bytes());
            let mut out = Vec::new();
            let choice =
                prompt_choice("?", AutomationChoice::AllOff, &mut input, &mut out).expect("prompt");
            assert_eq!(choice, Some(expected), "answer {answer:?}");
        }

        let mut input = Cursor::new(b"x\nc\n".to_vec());
        let mut out = anstream::StripStream::new(Vec::new());
        let choice =
            prompt_choice("?", AutomationChoice::AllOn, &mut input, &mut out).expect("prompt");
        let rendered = String::from_utf8(out.into_inner()).expect("utf8");
        assert_eq!(choice, Some(AutomationChoice::Choose));
        assert!(rendered.contains("Enter y, n, or choose."));
        assert_eq!(rendered.matches("? [Y/n/choose]").count(), 2);

        let mut eof = Cursor::new(Vec::new());
        assert_eq!(
            prompt_choice("?", AutomationChoice::AllOn, &mut eof, &mut Vec::new()).expect("eof"),
            None
        );
    }

    #[test]
    fn automation_rows_fit_eighty_columns() {
        for (row, on) in AUTOMATION_ROWS.iter().zip([true, false, true]) {
            let line = strip(|w| {
                writeln!(w, "{}", automation_row_line(&OfferedRow { row, on }))?;
                Ok(())
            });
            let line = line.trim_end_matches('\n');
            assert!(line.chars().count() <= 80, "{line:?} overflows 80 columns");
            assert!(line.contains(row.label) && line.contains(row.cost));
        }
    }

    #[test]
    fn defaults_fold_terminal_advertisement_and_explicit_theme_choices() {
        let fresh_config = MachineConfig::default();
        assert_eq!(
            Defaults::from_config(&fresh_config, false, false),
            Defaults {
                truecolor: false,
                nerd_font: false,
                pet_enabled: false,
                automation: fresh(false).automation,
            }
        );
        assert!(Defaults::from_config(&fresh_config, true, false).truecolor);

        let mut modern = MachineConfig::default();
        modern.theme.style = Some(ThemeStyle::Modern);
        let modern_defaults = Defaults::from_config(&modern, false, false);
        assert!(modern_defaults.truecolor);
        assert!(modern_defaults.nerd_font);
        assert!(!modern_defaults.pet_enabled);

        modern.theme.mode = ThemeMode::Indexed;
        modern.theme.glyphs.set = Some("unicode".to_owned());
        let explicit_fallbacks = Defaults::from_config(&modern, true, false);
        assert!(!explicit_fallbacks.truecolor);
        assert!(!explicit_fallbacks.nerd_font);

        let mut explicit_truecolor = MachineConfig::default();
        explicit_truecolor.theme.mode = ThemeMode::Truecolor;
        explicit_truecolor.theme.glyphs.set = Some("nerd_font".to_owned());
        let explicit = Defaults::from_config(&explicit_truecolor, false, false);
        assert!(explicit.truecolor);
        assert!(explicit.nerd_font);
        assert_eq!(explicit.automation_states(), vec![false, false]);

        explicit_truecolor.resume.auto_continue = true;
        assert_eq!(
            Defaults::from_config(&explicit_truecolor, false, false).automation_states(),
            vec![true, false]
        );
    }

    #[test]
    fn probe_lines_emit_truecolor_and_aligned_sidebar_glyphs() {
        let gradient = gradient_line();
        let glyphs = glyph_line();
        let sample = nerd_font_probe_glyphs()[0];

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
        let stops = nerd_font_probe_gradient(GRADIENT_WIDTH);

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
        let (_, rendered) = drive(fresh(true), b"\n\n\n\n");

        assert_eq!(rendered.matches("Use truecolor?").count(), 1);
        assert_eq!(rendered.matches("Use Nerd Font icons?").count(), 1);
        assert_eq!(rendered.matches("Want a pet?").count(), 1);
        assert_eq!(rendered.matches("Enable hands-off automation?").count(), 1);
    }

    #[test]
    fn pet_art_is_injected_only_when_available() {
        let (_, with_art) = drive_with_art(fresh(false), b"\n\n\n\n", || Some("ART\n".to_owned()));
        let nerd = with_art.find("Use Nerd Font icons?").expect("nerd prompt");
        let art = with_art.find("ART\n\n").expect("art with blank line");
        let pet = with_art.find("Want a pet?").expect("pet prompt");
        assert!(nerd < art && art < pet);

        let (_, without_art) = drive(fresh(false), b"\n\n\n\n");
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
