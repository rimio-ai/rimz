use std::thread;

use ratatui::style::Color;

use crate::config::PetsGlyphMode;

use super::asset::{self, PetSource};
use super::catalog::BUILTIN_PETS;
use super::cellart::{self, PetCell};
use super::frames::RgbaImage;
use super::model::{self, Animation, default_animations};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewCell {
    pub ch: char,
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PetPreview {
    pub id: String,
    pub grid: Result<Vec<Vec<PreviewCell>>, String>,
}

fn listable_sources() -> Vec<(String, PetSource)> {
    let mut sources = BUILTIN_PETS
        .iter()
        .map(|pet| (pet.id.to_owned(), PetSource::Builtin(*pet)))
        .collect::<Vec<_>>();
    sources.extend(asset::installed_petdex_pets().into_iter().map(|slug| {
        let id = slug.clone();
        (id, PetSource::Petdex(slug))
    }));
    sources
}

pub fn listable_ids() -> Vec<String> {
    listable_sources()
        .into_iter()
        .map(|(id, _source)| id)
        .collect()
}

pub fn load_previews(
    cols: u16,
    rows: u16,
    glyphs: PetsGlyphMode,
) -> impl Iterator<Item = PetPreview> {
    let sources = listable_sources();
    let handles = sources
        .iter()
        .map(|(_id, source)| {
            let source = source.clone();
            thread::spawn(move || {
                super::load_pet(source)
                    .map_err(|err| err.to_string())
                    .map(|frames| render_sprite(&frames, cols, rows, glyphs))
            })
        })
        .collect::<Vec<_>>();

    sources
        .into_iter()
        .zip(handles)
        .map(|((id, _source), handle)| PetPreview {
            id,
            grid: handle
                .join()
                .unwrap_or_else(|_| Err("pet preview loader stopped".to_owned())),
        })
}

fn render_sprite(
    frames: &[RgbaImage],
    cols: u16,
    rows: u16,
    glyphs: PetsGlyphMode,
) -> Vec<Vec<PreviewCell>> {
    if frames.is_empty() {
        return Vec::new();
    }
    let sprite_index = default_animations()
        .get(model::TRACK_IDLE)
        .map(Animation::first_sprite)
        .unwrap_or(0)
        .min(frames.len().saturating_sub(1));
    cellart::render_frame(&frames[sprite_index], cols, rows, glyphs)
        .iter()
        .map(|row| row.iter().map(preview_cell).collect())
        .collect()
}

fn preview_cell(cell: &PetCell) -> PreviewCell {
    PreviewCell {
        ch: cell.ch,
        fg: rgb(cell.fg),
        bg: rgb(cell.bg),
    }
}

fn rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalog::FRAME_COUNT;
    use super::*;

    #[test]
    fn listable_ids_exposes_catalog_first() {
        let ids = listable_ids();
        assert_eq!(
            ids.iter()
                .take(BUILTIN_PETS.len())
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "codex",
                "dewey",
                "fireball",
                "rocky",
                "seedy",
                "stacky",
                "bsod",
                "null-signal",
            ]
        );
    }

    #[test]
    fn preview_idle_track_resolves_to_catalog_frame() {
        let sprite_index = default_animations()
            .get(model::TRACK_IDLE)
            .expect("idle preview track exists")
            .first_sprite();
        assert!(sprite_index < FRAME_COUNT);
    }

    #[test]
    fn preview_cell_maps_rgb_and_reset() {
        let cell = PetCell {
            ch: 'x',
            fg: Color::Rgb(1, 2, 3),
            bg: Color::Reset,
        };
        assert_eq!(
            preview_cell(&cell),
            PreviewCell {
                ch: 'x',
                fg: Some((1, 2, 3)),
                bg: None,
            }
        );
    }
}
