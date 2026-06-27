//! `rimz list-pets` - print or preview bundled and installed provider-dashboard pets.

use std::io::{IsTerminal, Write};

use anyhow::Result;
use clap::Args;
use rimz::config::PetsGlyphMode;
use rimz::sidebar_pane::pets::{self, PetPixelPreview, PetPreview, PreviewCell};

use super::{GlobalFlags, machine_config};
use crate::cli::render;

const GAP: u16 = 2;

#[derive(Debug, Args)]
pub struct ListPetsArgs {
    /// Emit machine-readable JSON instead of one id per line.
    #[arg(long)]
    json: bool,
}

pub fn run(args: ListPetsArgs, _globals: &GlobalFlags) -> Result<()> {
    if args.json {
        let ids = pets::listable_ids();
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
        for id in pets::listable_ids() {
            writeln!(out, "{id}")?;
        }
        return Ok(());
    }

    let glyphs = machine_config().theme.pets.glyphs;
    let (caps, wrap_pixels) = pets::detect_pet_render_env(glyphs);
    let width = rimz::mux::detect_terminal_size()
        .map(|(cols, _)| cols)
        .unwrap_or(80);
    let branch = preview_branch(glyphs, caps);
    let slot = preview_slot(branch);
    let per_row = usize::from(
        width
            .saturating_add(GAP)
            .checked_div(slot.cols.saturating_add(GAP))
            .unwrap_or(1)
            .max(1),
    );
    if branch == PreviewBranch::Pixel {
        write_pixel_previews(&mut out, per_row, wrap_pixels)?;
        return Ok(());
    }
    let mut previews = pets::load_previews_with_caps(slot.cols, slot.rows, glyphs, caps);
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
        write_pet_row(&mut out, &chunk, slot)?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreviewBranch {
    Pixel,
    Cell,
}

fn preview_branch(glyphs: PetsGlyphMode, caps: pets::PetRenderCaps) -> PreviewBranch {
    if pets::previews_use_pixels(glyphs, caps) {
        PreviewBranch::Pixel
    } else {
        PreviewBranch::Cell
    }
}

fn preview_slot(branch: PreviewBranch) -> pets::PetGridSize {
    match branch {
        PreviewBranch::Pixel => pets::DASHBOARD_PIXEL_PET,
        PreviewBranch::Cell => pets::DASHBOARD_CELL_PET,
    }
}

fn write_pixel_previews(out: &mut impl Write, per_row: usize, wrap: bool) -> std::io::Result<()> {
    let mut previews = pets::load_pixel_previews();
    let mut any_failed = false;
    let mut first = true;
    let mut next_image_id = 1_u32;
    loop {
        let chunk = previews
            .by_ref()
            .take(per_row)
            .map(|preview| {
                let image_id = next_image_id;
                next_image_id = next_image_id.wrapping_add(1).max(1);
                (image_id, preview)
            })
            .collect::<Vec<_>>();
        if chunk.is_empty() {
            break;
        }
        if !first {
            writeln!(out)?;
        }
        first = false;
        any_failed |= chunk
            .iter()
            .any(|(_image_id, preview)| preview.frame.is_err());
        write_pixel_pet_row(out, &chunk, wrap)?;
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

fn write_pet_row(
    out: &mut impl Write,
    chunk: &[PetPreview],
    slot: pets::PetGridSize,
) -> std::io::Result<()> {
    for row in 0..usize::from(slot.rows) {
        for (index, preview) in chunk.iter().enumerate() {
            if index > 0 {
                write!(out, "{:gap$}", "", gap = usize::from(GAP))?;
            }
            write!(out, "{}", sprite_row(preview, row, slot))?;
        }
        writeln!(out)?;
    }
    for (index, preview) in chunk.iter().enumerate() {
        if index > 0 {
            write!(out, "{:gap$}", "", gap = usize::from(GAP))?;
        }
        let centered = center(&preview.id, usize::from(slot.cols));
        write!(
            out,
            "{}",
            render::paint(render::palette::ACCENT.bold(), &centered)
        )?;
    }
    writeln!(out)
}

fn write_pixel_pet_row(
    out: &mut impl Write,
    chunk: &[(u32, PetPixelPreview)],
    wrap: bool,
) -> std::io::Result<()> {
    pets::write_synchronized_pixel_output(out, |out| {
        for (image_id, preview) in chunk {
            let Ok(frame) = &preview.frame else {
                continue;
            };
            for packet in
                pets::transmit_rgba_chunks(*image_id, frame.width, frame.height, &frame.data)
            {
                out.write_all(&pets::wrap_pixel_payload(&packet, wrap))?;
            }
            out.write_all(&pets::wrap_pixel_payload(
                &pets::virtual_place(
                    *image_id,
                    pets::DASHBOARD_PIXEL_PET.cols,
                    pets::DASHBOARD_PIXEL_PET.rows,
                ),
                wrap,
            ))?;
        }
        for row in 0..pets::DASHBOARD_PIXEL_PET.rows {
            for (index, (image_id, preview)) in chunk.iter().enumerate() {
                if index > 0 {
                    write!(out, "{:gap$}", "", gap = usize::from(GAP))?;
                }
                if preview.frame.is_ok() {
                    out.write_all(&pets::inline_placeholder_row(
                        *image_id,
                        row,
                        pets::DASHBOARD_PIXEL_PET.cols,
                    ))?;
                } else {
                    write!(
                        out,
                        "{:width$}",
                        "",
                        width = usize::from(pets::DASHBOARD_PIXEL_PET.cols)
                    )?;
                }
            }
            writeln!(out)?;
        }
        for (index, (_image_id, preview)) in chunk.iter().enumerate() {
            if index > 0 {
                write!(out, "{:gap$}", "", gap = usize::from(GAP))?;
            }
            let centered = center(&preview.id, usize::from(pets::DASHBOARD_PIXEL_PET.cols));
            write!(
                out,
                "{}",
                render::paint(render::palette::ACCENT.bold(), &centered)
            )?;
        }
        writeln!(out)
    })
}

fn sprite_row(preview: &PetPreview, row: usize, slot: pets::PetGridSize) -> String {
    let grid = preview.grid.as_ref().ok();
    let mut rendered = String::new();
    for col in 0..usize::from(slot.cols) {
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
        let slot = pets::DASHBOARD_CELL_PET;
        let short = PetPreview {
            id: "codex".to_owned(),
            grid: Ok(vec![vec![PreviewCell {
                ch: 'x',
                fg: Some((1, 2, 3)),
                bg: None,
            }]]),
        };
        let failed = PetPreview {
            id: "codex".to_owned(),
            grid: Err("unavailable".to_owned()),
        };

        assert_eq!(
            strip(|w| write!(w, "{}", sprite_row(&short, 0, slot))),
            format!("x{:pad$}", "", pad = usize::from(slot.cols - 1))
        );
        assert_eq!(
            sprite_row(&failed, 0, slot),
            " ".repeat(usize::from(slot.cols))
        );
    }

    #[test]
    fn center_places_id_inside_preview_width() {
        assert_eq!(
            center("codex", usize::from(pets::DASHBOARD_CELL_PET.cols)),
            "      codex       "
        );
    }

    #[test]
    fn preview_branch_uses_pixels_only_when_mode_and_caps_allow() {
        assert_eq!(
            preview_branch(PetsGlyphMode::Auto, pets::PetRenderCaps { pixel: true }),
            PreviewBranch::Pixel
        );
        assert_eq!(
            preview_branch(PetsGlyphMode::Auto, pets::PetRenderCaps { pixel: false }),
            PreviewBranch::Cell
        );
        assert_eq!(
            preview_branch(PetsGlyphMode::Sextant, pets::PetRenderCaps { pixel: true }),
            PreviewBranch::Cell
        );
    }

    #[test]
    fn pixel_preview_row_pads_failed_sprite_slot() {
        let failed = [(
            42,
            PetPixelPreview {
                id: "codex".to_owned(),
                frame: Err("offline".to_owned()),
            },
        )];
        let rendered = strip(|w| write_pixel_pet_row(w, &failed, false));
        let lines = rendered.lines().collect::<Vec<_>>();

        assert_eq!(lines.len(), usize::from(pets::DASHBOARD_PIXEL_PET.rows) + 1);
        assert_eq!(
            lines[0],
            " ".repeat(usize::from(pets::DASHBOARD_PIXEL_PET.cols))
        );
        assert_eq!(
            lines[usize::from(pets::DASHBOARD_PIXEL_PET.rows)],
            "     codex     "
        );
    }

    #[test]
    fn pixel_preview_row_brackets_output_in_synchronized_output() {
        let preview = [(
            42,
            PetPixelPreview {
                id: "codex".to_owned(),
                frame: Ok(pets::PixelPreviewFrame {
                    width: 1,
                    height: 1,
                    data: vec![0, 1, 2, 3],
                }),
            },
        )];
        let mut bytes = Vec::new();

        write_pixel_pet_row(&mut bytes, &preview, true).expect("render pixel row");

        assert!(bytes.starts_with(b"\x1b[?2026h"));
        assert!(bytes.ends_with(b"\x1b[?2026l"));
        assert!(
            !bytes
                .windows(b"\x1bPtmux;\x1b\x1b[?2026h".len())
                .any(|window| window == b"\x1bPtmux;\x1b\x1b[?2026h")
        );
        assert!(
            !bytes
                .windows(b"\x1bPtmux;\x1b\x1b[?2026l".len())
                .any(|window| window == b"\x1bPtmux;\x1b\x1b[?2026l")
        );
    }
}
