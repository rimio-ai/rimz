//! `rimz list-pets` - print or preview bundled and installed provider-dashboard pets.

use std::io::{IsTerminal, Write};

use anyhow::Result;
use clap::Args;
use rimz::sidebar_pane::pets::{self, PetPixelPreview, PetPreview, PreviewCell};

use super::{GlobalFlags, machine_config};
use crate::cli::render;
pub(crate) use pacing::{LiveGraphicsPacer, PixelPacer};

pub(crate) mod pacing;

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
        return render::json_pretty(&ids);
    }

    let is_tty = std::io::stdout().is_terminal();
    let mut out = render::out();
    if !is_tty {
        for id in pets::listable_ids() {
            writeln!(out, "{id}")?;
        }
        return Ok(());
    }

    let config = machine_config();
    let pets_config = &config.theme.pets;
    let glyphs = pets_config.glyphs;
    let (caps, wrap_pixels) = pets::detect_pixel_render_env();
    let width = rimz::mux::detect_terminal_size()
        .map(|(cols, _)| cols)
        .unwrap_or(80);
    let tier = pets::resolve_render_tier(glyphs, caps);
    let slot = pets::dashboard_pet_size(tier);
    let per_row = usize::from(
        width
            .saturating_add(GAP)
            .checked_div(slot.cols.saturating_add(GAP))
            .unwrap_or(1)
            .max(1),
    );
    if tier == pets::PetRenderTier::Pixel {
        let mut pacer = wrap_pixels.then(LiveGraphicsPacer::open).flatten();
        let mut next_image_id = 1_u32;
        let previews = pets::load_pixel_previews().into_iter().map(move |preview| {
            let image_id = next_image_id;
            next_image_id = next_image_id.wrapping_add(1).max(1);
            (image_id, preview)
        });
        write_preview_chunks(
            &mut out,
            previews,
            per_row,
            |(_image_id, preview)| preview.frame.is_err(),
            |out, chunk| write_pixel_pet_row_with_pacer(out, chunk, wrap_pixels, pacer.as_mut()),
        )?;
        return Ok(());
    }
    let aspect = pets_config
        .cell_aspect
        .or_else(pets::probe_cell_aspect)
        .unwrap_or(rimz::config::CellAspect::NEUTRAL);
    let previews = pets::load_cell_previews(slot, aspect).into_iter();
    write_preview_chunks(
        &mut out,
        previews,
        per_row,
        |preview| preview.grid.is_err(),
        |out, chunk| write_pet_row(out, chunk, slot),
    )?;
    Ok(())
}

fn write_preview_chunks<W, T>(
    out: &mut W,
    mut previews: impl Iterator<Item = T>,
    per_row: usize,
    mut failed: impl FnMut(&T) -> bool,
    mut write_row: impl FnMut(&mut W, &[T]) -> std::io::Result<()>,
) -> std::io::Result<()>
where
    W: Write,
{
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
        any_failed |= chunk.iter().any(&mut failed);
        write_row(out, &chunk)?;
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

pub(crate) fn write_pet_row(
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

pub(crate) fn write_pixel_pet_row_with_pacer<P: PixelPacer>(
    out: &mut impl Write,
    chunk: &[(u32, PetPixelPreview)],
    wrap: bool,
    mut pacer: Option<&mut P>,
) -> std::io::Result<()> {
    for (image_id, preview) in chunk {
        let Ok(frame) = &preview.frame else {
            continue;
        };
        let png = pets::encode_png(frame.width, frame.height, &frame.data);
        for packet in pets::transmit_png_chunks(*image_id, &png) {
            out.write_all(&pets::wrap_pixel_payload(&packet, wrap))?;
        }
        let pacing = pacer.as_ref().is_some_and(|pacer| pacer.active());
        out.write_all(&pets::wrap_pixel_payload(
            &pets::virtual_place(
                *image_id,
                pets::DASHBOARD_PIXEL_PET.cols,
                pets::DASHBOARD_PIXEL_PET.rows,
                if pacing { 0 } else { 2 },
            ),
            wrap,
        ))?;
        if pacing {
            out.flush()?;
            if let Some(pacer) = pacer.as_mut() {
                pacer.wait_for_barrier();
            }
        }
    }
    pets::write_synchronized_pixel_output(out, |out| {
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
    fn pixel_preview_row_pads_failed_sprite_slot() {
        let failed = [(
            42,
            PetPixelPreview {
                id: "codex".to_owned(),
                frame: Err("offline".to_owned()),
            },
        )];
        let rendered = strip(|w| {
            write_pixel_pet_row_with_pacer(w, &failed, false, None::<&mut FakePixelPacer>)
        });
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

        write_pixel_pet_row_with_pacer(&mut bytes, &preview, true, None::<&mut FakePixelPacer>)
            .expect("render pixel row");

        let begin_sync = bytes
            .windows(b"\x1b[?2026h".len())
            .position(|window| window == b"\x1b[?2026h")
            .expect("draw phase starts synchronized output");
        let end_sync = bytes
            .windows(b"\x1b[?2026l".len())
            .rposition(|window| window == b"\x1b[?2026l")
            .expect("draw phase ends synchronized output");
        assert!(bytes[..begin_sync].starts_with(b"\x1bPtmux;\x1b\x1b_Ga=t,"));
        assert!(bytes[..begin_sync].ends_with(b"q=2;\x1b\x1b\\\x1b\\"));
        assert!(bytes.ends_with(b"\x1b[?2026l"));
        assert!(begin_sync < end_sync);
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

    #[test]
    fn pixel_preview_row_uses_verbose_placement_when_paced() {
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
        let mut pacer = FakePixelPacer {
            active: true,
            waits: 0,
        };
        let mut bytes = Vec::new();

        write_pixel_pet_row_with_pacer(&mut bytes, &preview, true, Some(&mut pacer))
            .expect("render paced pixel row");

        let text = String::from_utf8(bytes).expect("kitty escapes are utf8");
        assert!(text.contains("a=p,U=1,i=42,c=15,r=9,q=0"));
        assert_eq!(pacer.waits, 1);
    }

    struct FakePixelPacer {
        active: bool,
        waits: usize,
    }

    impl PixelPacer for FakePixelPacer {
        fn active(&self) -> bool {
            self.active
        }

        fn wait_for_barrier(&mut self) {
            self.waits += 1;
        }
    }
}
