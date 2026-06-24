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
use rimz::sidebar_pane::render::scheme;

#[derive(Debug, Args)]
pub struct ListThemesArgs {
    /// Emit machine-readable JSON instead of one name per line.
    #[arg(long)]
    json: bool,
}

pub fn run(args: ListThemesArgs, _globals: &GlobalFlags) -> Result<()> {
    let names = scheme::available_scheme_names();
    if args.json {
        let rendered = serde_json::to_string_pretty(&names).expect("theme name vec serializes");
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return Ok(());
    }
    let is_tty = std::io::stdout().is_terminal();
    let mut out = render::out();
    let accent = render::palette::ACCENT;
    for name in &names {
        if is_tty {
            if let Some(swatch) = scheme::scheme_swatch(name) {
                for rgb in [
                    swatch.background,
                    swatch.foreground,
                    swatch.red,
                    swatch.green,
                    swatch.yellow,
                    swatch.blue,
                    swatch.magenta,
                    swatch.cyan,
                ] {
                    write!(out, "{}", render::paint(bg_style(rgb), "  "))?;
                }
                write!(out, " ")?;
            }
            writeln!(out, "{}{name}{}", accent.render(), accent.render_reset())?;
        } else {
            writeln!(out, "{name}")?;
        }
    }
    Ok(())
}

fn bg_style((red, green, blue): (u8, u8, u8)) -> anstyle::Style {
    anstyle::Style::new().bg_color(Some(anstyle::Color::Rgb(anstyle::RgbColor(
        red, green, blue,
    ))))
}
