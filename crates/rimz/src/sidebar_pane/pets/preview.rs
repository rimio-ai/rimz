use std::thread;

use ratatui::style::Color;

use crate::config::PetsGlyphMode;

use super::asset::PetSource;
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
pub struct PetPose {
    pub label: &'static str,
    pub grid: Vec<Vec<PreviewCell>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PetPreview {
    pub id: &'static str,
    pub blurb: &'static str,
    pub poses: Result<Vec<PetPose>, String>,
}

const TRACK_RUN_RIGHT: &str = "run-right";

const PREVIEW_POSES: &[(&str, &str)] = &[
    ("idle", model::TRACK_IDLE),
    ("run", TRACK_RUN_RIGHT),
    ("wave", model::TRACK_WAVING),
    ("jump", model::TRACK_JUMPING),
    ("review", model::TRACK_REVIEW),
    ("oops", model::TRACK_FAILED),
];

pub fn builtin_ids() -> impl Iterator<Item = &'static str> {
    BUILTIN_PETS.iter().map(|pet| pet.id)
}

pub fn load_previews(cols: u16, rows: u16, glyphs: PetsGlyphMode) -> Vec<PetPreview> {
    let handles = BUILTIN_PETS
        .iter()
        .map(|pet| {
            let pet = *pet;
            thread::spawn(move || {
                super::load_pet(PetSource::Builtin(pet))
                    .map_err(|err| err.to_string())
                    .map(|frames| render_poses(&frames, cols, rows, glyphs))
            })
        })
        .collect::<Vec<_>>();

    BUILTIN_PETS
        .iter()
        .zip(handles)
        .map(|(pet, handle)| PetPreview {
            id: pet.id,
            blurb: pet.blurb,
            poses: handle
                .join()
                .unwrap_or_else(|_| Err("pet preview loader stopped".to_owned())),
        })
        .collect()
}

fn render_poses(frames: &[RgbaImage], cols: u16, rows: u16, glyphs: PetsGlyphMode) -> Vec<PetPose> {
    if frames.is_empty() {
        return Vec::new();
    }
    let animations = default_animations();
    PREVIEW_POSES
        .iter()
        .map(|&(label, track)| {
            let sprite_index = animations
                .get(track)
                .map(Animation::first_sprite)
                .unwrap_or(0)
                .min(frames.len().saturating_sub(1));
            let grid = cellart::render_frame(&frames[sprite_index], cols, rows, glyphs)
                .iter()
                .map(|row| row.iter().map(preview_cell).collect())
                .collect();
            PetPose { label, grid }
        })
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
    fn builtin_ids_exposes_catalog() {
        assert_eq!(builtin_ids().count(), 8);
    }

    #[test]
    fn preview_tracks_resolve_to_catalog_frames() {
        let animations = default_animations();
        for &(_, track) in PREVIEW_POSES {
            let sprite_index = animations
                .get(track)
                .unwrap_or_else(|| panic!("{track} preview track exists"))
                .first_sprite();
            assert!(sprite_index < FRAME_COUNT);
        }
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
