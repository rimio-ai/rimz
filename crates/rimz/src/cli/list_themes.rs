//! `rimz list-themes` — print the bundled sidebar theme names.
//!
//! The names are the keys of the embedded Alacritty catalog (sorted), each
//! usable verbatim as `[theme] scheme` or `rimz config set
//! theme.scheme <name>`. `--json` emits the same list as an array for
//! scripting.

use std::io::{IsTerminal, Write};

use anyhow::Result;
use clap::Args;

use super::GlobalFlags;
use crate::cli::render;
use rimz::config;
use rimz::theme::scheme;

const GROUP_GAP: usize = 3;

#[derive(Debug, Args)]
pub struct ListThemesArgs {
    /// Emit machine-readable JSON instead of one name per line.
    #[arg(long)]
    json: bool,
}

pub fn run(args: ListThemesArgs, _globals: &GlobalFlags) -> Result<()> {
    let names = config::available_scheme_names();
    if args.json {
        return render::json_pretty(&names);
    }
    let is_tty = std::io::stdout().is_terminal();
    let mut out = render::out();
    if !is_tty {
        for name in &names {
            writeln!(out, "{name}")?;
        }
        return Ok(());
    }

    let name_w = names
        .iter()
        .map(|name| name.chars().count())
        .max()
        .unwrap_or(0);
    write_legend(&mut out, name_w)?;
    let accent = render::palette::accent();
    for name in &names {
        write!(out, "{}{name}{}", accent.render(), accent.render_reset())?;
        let pad = name_w.saturating_sub(name.chars().count());
        write!(out, "{:pad$}  ", "", pad = pad)?;
        if let Some(swatch) = scheme::scheme_swatch(name) {
            write_swatch(&mut out, &swatch)?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn write_legend(out: &mut impl Write, name_w: usize) -> std::io::Result<()> {
    write!(out, "{:w$}  ", "", w = name_w)?;
    write_legend_group(out, &["bg", "fg"])?;
    write!(out, "{:gap$}", "", gap = GROUP_GAP)?;
    write_legend_group(out, &["r", "g", "y", "b", "m", "c"])?;
    writeln!(out)
}

fn write_legend_group(out: &mut impl Write, tokens: &[&str]) -> std::io::Result<()> {
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 {
            write!(out, " ")?;
        }
        write!(
            out,
            "{}",
            render::paint(render::palette::faint(), &format!("{token:<2}"))
        )?;
    }
    Ok(())
}

fn write_swatch(out: &mut impl Write, swatch: &scheme::SchemeSwatch) -> std::io::Result<()> {
    write_chips(out, &[swatch.background, swatch.foreground])?;
    write!(out, "{:gap$}", "", gap = GROUP_GAP)?;
    write_chips(
        out,
        &[
            swatch.red,
            swatch.green,
            swatch.yellow,
            swatch.blue,
            swatch.magenta,
            swatch.cyan,
        ],
    )
}

fn write_chips(out: &mut impl Write, rgbs: &[(u8, u8, u8)]) -> std::io::Result<()> {
    for (index, &rgb) in rgbs.iter().enumerate() {
        if index > 0 {
            write!(out, " ")?;
        }
        write!(out, "{}", render::paint(bg_style(rgb), "  "))?;
    }
    Ok(())
}

fn bg_style(rgb: (u8, u8, u8)) -> anstyle::Style {
    render::palette::rgb_bg(rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(
        render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> std::io::Result<()>,
    ) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_one(&mut stream).expect("render to in-memory buffer");
        String::from_utf8(stream.into_inner()).expect("utf-8")
    }

    #[test]
    fn legend_aligns_grouped_chip_labels() {
        assert_eq!(
            strip(|w| write_legend(w, 4)),
            "      bg fg   r  g  y  b  m  c \n"
        );
    }

    #[test]
    fn swatch_writes_background_chips_in_grouped_order() {
        let swatch = scheme::SchemeSwatch {
            background: (0, 0, 0),
            foreground: (255, 255, 255),
            red: (255, 0, 0),
            green: (0, 255, 0),
            yellow: (255, 255, 0),
            blue: (0, 0, 255),
            magenta: (255, 0, 255),
            cyan: (0, 255, 255),
        };
        let mut raw = Vec::new();
        write_swatch(&mut raw, &swatch).expect("render swatch");
        let raw = String::from_utf8(raw).expect("utf-8");
        let sequences = [
            swatch.background,
            swatch.foreground,
            swatch.red,
            swatch.green,
            swatch.yellow,
            swatch.blue,
            swatch.magenta,
            swatch.cyan,
        ]
        .map(|rgb| bg_style(rgb).render().to_string());
        let positions = sequences
            .iter()
            .map(|sequence| {
                raw.find(sequence)
                    .unwrap_or_else(|| panic!("{sequence:?} in swatch"))
            })
            .collect::<Vec<_>>();
        assert!(
            positions.windows(2).all(|window| window[0] < window[1]),
            "swatch sequences out of order: {raw:?}"
        );
    }
}
