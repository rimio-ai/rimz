//! Builds `rimz list-pets` previews from the same catalog, cache, and render
//! tier resolver as the live dashboard.

use std::thread;

use ratatui::style::Color;

use crate::config::CellAspect;

use super::PetGridSize;
use super::asset::{self, PetSource};
use super::catalog::BUILTIN_PETS;
use super::cellart::{self, PetCell};
use super::frames::RgbaImage;
use super::model;

const PREVIEW_FETCH_CONCURRENCY: usize = 2;

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
    sources: Vec<(String, PetSource)>,
    load: impl Fn(PetSource) -> Result<T, String> + Sync,
) -> Vec<(String, Result<T, String>)>
where
    T: Send,
{
    let load = &load;
    let mut out = Vec::with_capacity(sources.len());
    for batch in sources.chunks(PREVIEW_FETCH_CONCURRENCY) {
        thread::scope(|scope| {
            let handles = batch
                .iter()
                .map(|(id, source)| {
                    let source = source.clone();
                    (id.clone(), scope.spawn(move || load(source)))
                })
                .collect::<Vec<_>>();
            for (id, handle) in handles {
                let result = handle
                    .join()
                    .unwrap_or_else(|_| Err("pet preview loader stopped".to_owned()));
                out.push((id, result));
            }
        });
    }
    out
}

pub fn load_cell_previews(size: PetGridSize, aspect: CellAspect) -> Vec<PetPreview> {
    load_preview_results(listable_sources(), move |source| {
        super::load_pet(source)
            .map_err(|err| err.to_string())
            .map(|frames| render_sprite(&frames, size, aspect))
    })
    .into_iter()
    .map(|(id, grid)| PetPreview { id, grid })
    .collect()
}

pub fn load_cell_preview(
    selector: &str,
    size: PetGridSize,
    aspect: CellAspect,
) -> Option<PetPreview> {
    let source = asset::resolve_pet_source(selector)?;
    let id = source.id().into_owned();
    let grid = super::load_pet(source)
        .map_err(|err| err.to_string())
        .map(|frames| render_sprite(&frames, size, aspect));
    Some(PetPreview { id, grid })
}

pub fn load_pixel_previews() -> Vec<PetPixelPreview> {
    load_preview_results(listable_sources(), |source| {
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

pub fn load_pixel_preview(selector: &str) -> Option<PetPixelPreview> {
    let source = asset::resolve_pet_source(selector)?;
    let id = source.id().into_owned();
    let frame = super::load_pet(source)
        .map_err(|err| err.to_string())
        .and_then(|frames| {
            idle_sprite(&frames)
                .cloned()
                .map(PixelPreviewFrame::from)
                .ok_or_else(|| "pet sheet has no frames".to_owned())
        });
    Some(PetPixelPreview { id, frame })
}

fn render_sprite(
    frames: &[RgbaImage],
    size: PetGridSize,
    aspect: CellAspect,
) -> Vec<Vec<PreviewCell>> {
    let Some(sprite) = idle_sprite(frames) else {
        return Vec::new();
    };
    cellart::render_frame(sprite, size.cols, size.rows, aspect)
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
    use super::*;

    #[test]
    fn load_preview_results_bounds_concurrency_and_preserves_order() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let inflight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let sources: Vec<(String, PetSource)> = (0..6)
            .map(|i| (format!("p{i}"), PetSource::Petdex(format!("p{i}"))))
            .collect();

        let results = load_preview_results(sources.clone(), |_source| {
            let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            inflight.fetch_sub(1, Ordering::SeqCst);
            Ok::<(), String>(())
        });

        assert_eq!(
            results.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            sources.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            "results follow source order",
        );
        assert!(
            peak.load(Ordering::SeqCst) <= PREVIEW_FETCH_CONCURRENCY,
            "never more than two fetches in flight",
        );
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
