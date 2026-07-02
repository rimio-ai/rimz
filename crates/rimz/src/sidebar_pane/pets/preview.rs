//! Builds `rimz list-pets` previews from the same catalog, cache, and render
//! tier resolver as the live dashboard.

use std::thread;

use ratatui::style::Color;

use super::PetGridSize;
use super::asset::{self, PetSource};
use super::catalog::BUILTIN_PETS;
use super::cellart::{self, PetCell};
use super::frames::RgbaImage;
use super::model;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PixelPreviewFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PetPixelPreview {
    pub id: String,
    pub frame: Result<PixelPreviewFrame, String>,
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

fn load_preview_results<T>(
    load: impl Fn(PetSource) -> Result<T, String> + Copy + Send + 'static,
) -> Vec<(String, Result<T, String>)>
where
    T: Send + 'static,
{
    let sources = listable_sources();
    let handles = sources
        .iter()
        .map(|(_id, source)| {
            let source = source.clone();
            thread::spawn(move || load(source))
        })
        .collect::<Vec<_>>();

    sources
        .into_iter()
        .zip(handles)
        .map(|((id, _source), handle)| {
            let result = handle
                .join()
                .unwrap_or_else(|_| Err("pet preview loader stopped".to_owned()));
            (id, result)
        })
        .collect()
}

pub fn load_cell_previews(size: PetGridSize) -> Vec<PetPreview> {
    load_preview_results(move |source| {
        super::load_pet(source)
            .map_err(|err| err.to_string())
            .map(|frames| render_sprite(&frames, size))
    })
    .into_iter()
    .map(|(id, grid)| PetPreview { id, grid })
    .collect()
}

pub fn load_pixel_previews() -> Vec<PetPixelPreview> {
    load_preview_results(|source| {
        super::load_pet(source)
            .map_err(|err| err.to_string())
            .and_then(|frames| {
                idle_sprite(&frames)
                    .cloned()
                    .map(PixelPreviewFrame::from)
                    .ok_or_else(|| "pet sheet has no frames".to_owned())
            })
    })
    .into_iter()
    .map(|(id, frame)| PetPixelPreview { id, frame })
    .collect()
}

fn render_sprite(frames: &[RgbaImage], size: PetGridSize) -> Vec<Vec<PreviewCell>> {
    let Some(sprite) = idle_sprite(frames) else {
        return Vec::new();
    };
    cellart::render_frame(sprite, size.cols, size.rows)
        .iter()
        .map(|row| row.iter().map(preview_cell).collect())
        .collect()
}

fn idle_sprite(frames: &[RgbaImage]) -> Option<&RgbaImage> {
    if frames.is_empty() {
        return None;
    }
    let sprite_index = model::animations()
        .get(model::TRACK_IDLE)
        .map(|animation| animation.first_sprite())
        .unwrap_or(0)
        .min(frames.len().saturating_sub(1));
    frames.get(sprite_index)
}

impl From<RgbaImage> for PixelPreviewFrame {
    fn from(frame: RgbaImage) -> Self {
        Self {
            width: frame.width,
            height: frame.height,
            data: frame.data,
        }
    }
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
        let sprite_index = model::animations()
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
