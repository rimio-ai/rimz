//! `rimz list-pets` - print or preview the bundled provider-dashboard pets.

use std::io::{IsTerminal, Write};

use anyhow::Result;
use clap::Args;
use rimz::sidebar_pane::pets::{self, PetPreview, PreviewCell};

use super::{GlobalFlags, machine_config};
use crate::cli::render;

const PREVIEW_COLS: u16 = 20;
const PREVIEW_ROWS: u16 = 10;
const GAP: u16 = 2;

#[derive(Debug, Args)]
pub struct ListPetsArgs {
    /// Emit machine-readable JSON instead of one id per line.
    #[arg(long)]
    json: bool,
}

pub fn run(args: ListPetsArgs, _globals: &GlobalFlags) -> Result<()> {
    let ids = pets::builtin_ids().collect::<Vec<_>>();
    if args.json {
        let rendered = serde_json::to_string_pretty(&ids).expect("pet id vec serializes");
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return Ok(());
    }

    let is_tty = std::io::stdout().is_terminal();
    let mut out = render::out();
    if !is_tty {
        for id in ids {
            writeln!(out, "{id}")?;
        }
        return Ok(());
    }

    let glyphs = machine_config().theme.pets.glyphs;
    let width = rimz::mux::detect_terminal_size()
        .map(|(cols, _)| cols)
        .unwrap_or(80);
    let per_row = usize::from(
        width
            .saturating_add(GAP)
            .checked_div(PREVIEW_COLS + GAP)
            .unwrap_or(1)
            .max(1),
    );
    let mut previews = pets::load_previews(PREVIEW_COLS, PREVIEW_ROWS, glyphs);
    let mut any_failed = false;
    let mut first = true;
    loop {
        let chunk = previews.by_ref().take(per_row).collect::<Vec<_>>();
        if chunk.is_empty() {
            break;
        }
        if !first {
            writeln!(out)?;
        }
        first = false;
        any_failed |= chunk.iter().any(|preview| preview.grid.is_err());
        write_pet_row(&mut out, &chunk)?;
        out.flush()?;
    }
    if any_failed {
        writeln!(
            out,
            "{}",
            render::paint(
                render::palette::FAINT,
                "(some pets unavailable - check network, or RIMZ_PETS_OFFLINE serves cache only)"
            )
        )?;
    }
    Ok(())
}

fn write_pet_row(out: &mut impl Write, chunk: &[PetPreview]) -> std::io::Result<()> {
    for row in 0..usize::from(PREVIEW_ROWS) {
        for (index, preview) in chunk.iter().enumerate() {
            if index > 0 {
                write!(out, "{:gap$}", "", gap = usize::from(GAP))?;
            }
            write!(out, "{}", sprite_row(preview, row))?;
        }
        writeln!(out)?;
    }
    for (index, preview) in chunk.iter().enumerate() {
        if index > 0 {
            write!(out, "{:gap$}", "", gap = usize::from(GAP))?;
        }
        let centered = center(preview.id, usize::from(PREVIEW_COLS));
        write!(
            out,
            "{}",
            render::paint(render::palette::ACCENT.bold(), &centered)
        )?;
    }
    writeln!(out)
}

fn sprite_row(preview: &PetPreview, row: usize) -> String {
    let grid = preview.grid.as_ref().ok();
    let mut rendered = String::new();
    for col in 0..usize::from(PREVIEW_COLS) {
        match grid
            .and_then(|grid| grid.get(row))
            .and_then(|cells| cells.get(col))
        {
            Some(cell) => rendered.push_str(&paint_cell(cell)),
            None => rendered.push(' '),
        }
    }
    rendered
}

fn center(label: &str, width: usize) -> String {
    let len = label.chars().count();
    if len >= width {
        return label.chars().take(width).collect();
    }
    let left = (width - len) / 2;
    let right = width - len - left;
    format!(
        "{:left$}{label}{:right$}",
        "",
        "",
        left = left,
        right = right
    )
}

fn paint_cell(cell: &PreviewCell) -> String {
    if cell.fg.is_none() && cell.bg.is_none() {
        return " ".to_owned();
    }
    let mut style = anstyle::Style::new();
    if let Some(rgb) = cell.fg {
        style = style.fg_color(Some(rgb_color(rgb)));
    }
    if let Some(rgb) = cell.bg {
        style = style.bg_color(Some(rgb_color(rgb)));
    }
    render::paint(style, &cell.ch.to_string())
}

fn rgb_color((red, green, blue): (u8, u8, u8)) -> anstyle::Color {
    anstyle::Color::Rgb(anstyle::RgbColor(red, green, blue))
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
    fn sprite_row_pads_short_or_failed_grid() {
        let short = PetPreview {
            id: "codex",
            grid: Ok(vec![vec![PreviewCell {
                ch: 'x',
                fg: Some((1, 2, 3)),
                bg: None,
            }]]),
        };
        let failed = PetPreview {
            id: "codex",
            grid: Err("unavailable".to_owned()),
        };

        assert_eq!(
            strip(|w| write!(w, "{}", sprite_row(&short, 0))),
            format!("x{:pad$}", "", pad = usize::from(PREVIEW_COLS - 1))
        );
        assert_eq!(
            sprite_row(&failed, 0),
            " ".repeat(usize::from(PREVIEW_COLS))
        );
    }

    #[test]
    fn center_places_id_inside_preview_width() {
        assert_eq!(
            center("codex", usize::from(PREVIEW_COLS)),
            "       codex        "
        );
    }
}
