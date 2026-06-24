//! `rimz list-pets` - print or preview the bundled provider-dashboard pets.

use std::io::{IsTerminal, Write};

use anyhow::Result;
use clap::Args;
use rimz::sidebar_pane::pets::{self, PetPose, PreviewCell};

use super::{GlobalFlags, machine_config};
use crate::cli::render;

const PREVIEW_COLS: u16 = 12;
const PREVIEW_ROWS: u16 = 6;
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
    let poses_per_row = usize::from(
        width
            .saturating_add(GAP)
            .checked_div(PREVIEW_COLS + GAP)
            .unwrap_or(1)
            .max(1),
    );

    for (index, preview) in pets::load_previews(PREVIEW_COLS, PREVIEW_ROWS, glyphs)
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            writeln!(out)?;
        }
        writeln!(
            out,
            "{}  {}",
            render::paint(render::palette::ACCENT.bold(), preview.id),
            render::paint(render::palette::MUTED, preview.blurb)
        )?;
        match preview.poses {
            Ok(poses) => write_poses(&mut out, &poses, poses_per_row)?,
            Err(_) => writeln!(
                out,
                "{}",
                render::paint(
                    render::palette::FAINT,
                    "(unavailable - check network, or RIMZ_PETS_OFFLINE serves cache only)"
                )
            )?,
        }
    }
    Ok(())
}

fn write_poses(
    out: &mut impl Write,
    poses: &[PetPose],
    poses_per_row: usize,
) -> std::io::Result<()> {
    for chunk in poses.chunks(poses_per_row) {
        for row in 0..usize::from(PREVIEW_ROWS) {
            for (index, pose) in chunk.iter().enumerate() {
                if index > 0 {
                    write!(out, "{:gap$}", "", gap = usize::from(GAP))?;
                }
                write!(out, "{}", pose_row(pose, row))?;
            }
            writeln!(out)?;
        }
        for (index, pose) in chunk.iter().enumerate() {
            if index > 0 {
                write!(out, "{:gap$}", "", gap = usize::from(GAP))?;
            }
            let centered = center(pose.label, usize::from(PREVIEW_COLS));
            write!(out, "{}", render::paint(render::palette::FAINT, &centered))?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn pose_row(pose: &PetPose, row: usize) -> String {
    let mut rendered = String::new();
    for col in 0..usize::from(PREVIEW_COLS) {
        match pose.grid.get(row).and_then(|cells| cells.get(col)) {
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
