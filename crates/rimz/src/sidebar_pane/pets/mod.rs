//! Opt-in dashboard pet: asset loading, sprite slicing, cell-art conversion,
//! animation track selection, and canned captions.
//!
//! The renderer receives only [`PetView`] data. Network, disk, decode, and
//! memoized cell-art work stays here, owned by the serve loop.

mod asset;
mod catalog;
mod cellart;
mod frames;
mod model;
mod painter;
mod preview;
mod voice;

use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::config::{CellAspect, PetsConfig, PetsGlyphMode, PixelMode};

#[cfg(test)]
pub(crate) use cellart::PetCell;
pub(crate) use cellart::PetCellGrid;
pub use cellart::probe_cell_aspect;
pub(crate) use model::PetAction;
pub(crate) use painter::PixelPainter;
pub use preview::{
    PetPixelPreview, PetPreview, PixelPreviewFrame, PreviewCell, listable_ids, load_cell_preview,
    load_cell_previews, load_pixel_preview, load_pixel_previews,
};

use crate::sidebar_pane::pixel::{PixelRenderCaps, RgbaImage};
use asset::PetSource;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PetView {
    pub(crate) body: Option<PetBody>,
    pub(crate) caption: Option<String>,
    pub(crate) frame_interval: Option<std::time::Duration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PetBody {
    Cell(PetCellGrid),
    Pixel(PetPixelView),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PetPixelView {
    pub(crate) pet_id: String,
    pub(crate) sprite_index: usize,
    pub(crate) image_id: u32,
    pub(crate) size: PetGridSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PetGridSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PetViewFrame {
    pub(crate) action: PetAction,
    pub(crate) phase: u64,
    pub(crate) refresh_ms: u16,
    pub(crate) body: Option<PetRenderTier>,
    pub(crate) pixel_id_base: u32,
    pub(crate) cell_aspect: CellAspect,
    pub(crate) motion_enabled: bool,
    pub(crate) unread_triggered: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PetRenderTier {
    Pixel,
    Cell,
}

pub fn resolve_render_tier(mode: PetsGlyphMode, caps: PixelRenderCaps) -> PetRenderTier {
    match mode {
        PetsGlyphMode::Sextant => PetRenderTier::Cell,
        PetsGlyphMode::Pixel if caps.pixel_transport => PetRenderTier::Pixel,
        PetsGlyphMode::Auto if caps.pixel_transport && caps.kitty_clients => PetRenderTier::Pixel,
        _ => PetRenderTier::Cell,
    }
}

/// The tier actually painted this frame: `resolve_render_tier` downgraded to
/// cell art when pixels resolve but cannot paint here — no provider block to
/// ride, or a suppressed body. Cell tiers pass through untouched.
pub(crate) fn effective_render_tier(
    mode: PetsGlyphMode,
    pixel_mode: PixelMode,
    caps: PixelRenderCaps,
    pixel_paintable: bool,
) -> PetRenderTier {
    if pixel_mode == PixelMode::Off {
        return PetRenderTier::Cell;
    }
    match resolve_render_tier(mode, caps) {
        PetRenderTier::Pixel if !pixel_paintable => PetRenderTier::Cell,
        tier => tier,
    }
}

pub const DASHBOARD_PIXEL_PET: PetGridSize = PetGridSize { cols: 15, rows: 9 };
pub const DASHBOARD_CELL_PET: PetGridSize = PetGridSize { cols: 18, rows: 9 };

pub fn dashboard_pet_size(tier: PetRenderTier) -> PetGridSize {
    match tier {
        PetRenderTier::Pixel => DASHBOARD_PIXEL_PET,
        PetRenderTier::Cell => DASHBOARD_CELL_PET,
    }
}

#[derive(Default)]
pub(crate) struct PetAssets {
    loaded: Option<LoadedPet>,
    loading: Option<LoadingPet>,
    failed: Option<FailedPet>,
    previous_action: Option<PetAction>,
    jump_started_phase: Option<u64>,
    previous_unread_rows: BTreeSet<String>,
    caption: Option<String>,
}

struct LoadingPet {
    id: String,
    key: PreparationKey,
    receiver: Receiver<LoadResult>,
}

struct FailedPet {
    id: String,
    key: PreparationKey,
    caption: String,
    failed_at_phase: u64,
}

/// Wall-clock span, in milliseconds, before a failed asset load is retried. A
/// first fetch can fail transiently — a cold network the moment pets are
/// switched on, a CDN blip — and latching forever would strand the pet on
/// "pet unavailable" for the whole session even once the asset is reachable.
/// Retrying on a fixed cooldown self-heals without spawning a loader thread
/// every frame.
const RETRY_COOLDOWN_MS: u64 = 20_000;

/// Whether enough phases have elapsed since `failed_at_phase` to re-attempt a
/// load. `phase` is the monotonic wall-clock animation phase, so the cooldown
/// tracks real time at whatever cadence the serve loop wakes.
fn retry_due(failed_at_phase: u64, phase: u64, refresh_ms: u16) -> bool {
    let cooldown_phases = (RETRY_COOLDOWN_MS / u64::from(refresh_ms.max(1))).max(1);
    phase.saturating_sub(failed_at_phase) >= cooldown_phases
}

fn phase_elapsed(started_phase: u64, phase: u64, refresh_ms: u16) -> std::time::Duration {
    std::time::Duration::from_millis(
        phase
            .saturating_sub(started_phase)
            .saturating_mul(u64::from(refresh_ms.max(1))),
    )
}

struct LoadedPet {
    id: String,
    key: PreparationKey,
    asset: LoadedPetAsset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PreparationKey {
    tier: PetRenderTier,
    size: PetGridSize,
    aspect: CellAspect,
}

impl PreparationKey {
    fn new(tier: PetRenderTier, size: PetGridSize, aspect: CellAspect) -> Self {
        Self {
            tier,
            size,
            aspect: match tier {
                PetRenderTier::Pixel => CellAspect::NEUTRAL,
                PetRenderTier::Cell => aspect,
            },
        }
    }
}

enum LoadedPetAsset {
    Cell(Vec<PetCellGrid>),
    Pixel(Vec<RgbaImage>),
}

type LoadResult = Result<LoadedPetAsset, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SelectedTrack {
    track: model::PetTrack,
    phase: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LoadedSpriteRequest<'a> {
    pet_id: &'a str,
    previous_action: Option<PetAction>,
    frame: PetViewFrame,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackSelection {
    previous_action: Option<PetAction>,
    action: PetAction,
    phase: u64,
    refresh_ms: u16,
    motion_enabled: bool,
    jump_duration: Option<std::time::Duration>,
    unread_triggered: bool,
}

impl PetAssets {
    #[cfg(test)]
    pub(crate) fn test_loaded_pixel_frame(pet_id: &str) -> Self {
        let frame = RgbaImage {
            width: 1,
            height: 1,
            data: vec![255, 0, 0, 255],
        };
        Self {
            loaded: Some(LoadedPet {
                id: pet_id.to_owned(),
                key: PreparationKey::new(
                    PetRenderTier::Pixel,
                    DASHBOARD_PIXEL_PET,
                    CellAspect::NEUTRAL,
                ),
                asset: LoadedPetAsset::Pixel(vec![frame; catalog::FRAME_COUNT]),
            }),
            ..Self::default()
        }
    }

    pub(crate) fn observe_unread_rows(
        &mut self,
        unread_rows: impl IntoIterator<Item = String>,
    ) -> bool {
        let unread_rows = unread_rows.into_iter().collect::<BTreeSet<_>>();
        let triggered = unread_rows
            .iter()
            .any(|row| !self.previous_unread_rows.contains(row));
        self.previous_unread_rows = unread_rows;
        triggered
    }

    /// Clear the loader, action, jump, and caption state so the next frame
    /// starts cold. `previous_unread_rows` is owned by the per-frame
    /// `observe_unread_rows` call in the serve loop, so it is left untouched
    /// here; the full-teardown disabled path clears it on its own.
    fn reset_runtime_state(&mut self) {
        self.loaded = None;
        self.loading = None;
        self.failed = None;
        self.previous_action = None;
        self.jump_started_phase = None;
        self.caption = None;
    }

    pub(crate) fn view(&mut self, config: &PetsConfig, frame: PetViewFrame) -> Option<PetView> {
        let PetViewFrame {
            action,
            phase,
            refresh_ms,
            body: body_tier,
            pixel_id_base,
            cell_aspect,
            motion_enabled: _,
            unread_triggered: _,
        } = frame;
        if !config.enabled {
            self.reset_runtime_state();
            self.previous_unread_rows.clear();
            return None;
        }

        let previous_action = self.previous_action;
        let mut active_track = model::action_track(action);
        let Some(source) = asset::resolve_pet_source(&config.pet) else {
            self.reset_runtime_state();
            self.caption = Some("no pet selected".to_owned());
            return Some(PetView {
                body: None,
                caption: self.caption.clone(),
                frame_interval: None,
            });
        };
        let id = source.id();
        let id: &str = id.as_ref();

        let preparation =
            body_tier.map(|tier| PreparationKey::new(tier, dashboard_pet_size(tier), cell_aspect));
        self.poll_loader(phase);
        self.clear_mismatched_pet(id, preparation);
        if let Some(key) = preparation {
            self.ensure_loading(&source, key, phase, refresh_ms);
        }
        self.observe_action(action, config.voice, phase);

        let loading = preparation.is_some_and(|key| {
            self.loading
                .as_ref()
                .is_some_and(|loading| loading.id == id && loading.key == key)
        });
        let unavailable_caption = self
            .failed
            .as_ref()
            .filter(|failed| failed.id == id)
            .map(|failed| failed.caption.clone());
        let body = preparation.and_then(|key| {
            let (sprite_index, track) = self.loaded_sprite(LoadedSpriteRequest {
                pet_id: id,
                previous_action,
                frame,
            })?;
            match key.tier {
                PetRenderTier::Pixel => {
                    active_track = track;
                    Some(PetBody::Pixel(PetPixelView {
                        pet_id: id.to_owned(),
                        sprite_index,
                        image_id: crate::sidebar_pane::pixel::sprite_image_id(
                            pixel_id_base,
                            sprite_index,
                        ),
                        size: key.size,
                    }))
                }
                PetRenderTier::Cell => self.loaded_grid(id, key, sprite_index).map(|grid| {
                    active_track = track;
                    PetBody::Cell(grid)
                }),
            }
        });
        let frame_interval = if loading {
            Some(crate::sidebar::timing::animation_frame(refresh_ms))
        } else if body.is_some() && frame.motion_enabled {
            Some(model::track_frame_duration(active_track, refresh_ms))
        } else {
            None
        };
        Some(PetView {
            body,
            caption: unavailable_caption
                .or_else(|| self.caption.clone())
                .or_else(|| loading.then(|| "fetching pet...".to_owned())),
            frame_interval,
        })
    }

    pub(crate) fn pixel_frame(&self, pet_id: &str, sprite_index: usize) -> Option<&RgbaImage> {
        let loaded = self.loaded.as_ref()?;
        if loaded.id != pet_id {
            return None;
        }
        match &loaded.asset {
            LoadedPetAsset::Pixel(frames) => frames.get(sprite_index),
            LoadedPetAsset::Cell(_) => None,
        }
    }

    fn observe_action(&mut self, action: PetAction, voice: bool, seed: u64) {
        if !voice {
            self.caption = None;
            self.previous_action = Some(action);
            return;
        }
        // `voice::caption` returns `Some` only on an action transition, so a
        // `None` means same-action and the prior caption stands.
        if let Some(next) = voice::caption(self.previous_action, action, seed) {
            self.caption = Some(next.to_owned());
        }
        self.previous_action = Some(action);
    }
    fn poll_loader(&mut self, phase: u64) {
        let Some(loading) = &self.loading else {
            return;
        };
        let result = match loading.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("pet loader stopped".to_owned()),
        };
        let id = loading.id.clone();
        let key = loading.key;
        self.loading = None;
        match result {
            Ok(asset) => {
                self.loaded = Some(LoadedPet { id, key, asset });
                self.failed = None;
            }
            Err(err) => {
                tracing::debug!(pet = %id, error = %err, "pet asset unavailable");
                self.loaded = None;
                self.jump_started_phase = None;
                self.failed = Some(FailedPet {
                    id,
                    key,
                    caption: "pet unavailable".to_owned(),
                    failed_at_phase: phase,
                });
            }
        }
    }

    fn clear_mismatched_pet(&mut self, pet_id: &str, key: Option<PreparationKey>) {
        if self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.id != pet_id || key.is_some_and(|key| loaded.key != key))
        {
            self.loaded = None;
            self.jump_started_phase = None;
        }
        if self.loading.as_ref().is_some_and(|loading| {
            loading.id != pet_id || key.is_some_and(|key| loading.key != key)
        }) {
            self.loading = None;
        }
        if self
            .failed
            .as_ref()
            .is_some_and(|failed| failed.id != pet_id || key.is_some_and(|key| failed.key != key))
        {
            self.failed = None;
        }
    }

    fn ensure_loading(
        &mut self,
        source: &PetSource,
        key: PreparationKey,
        phase: u64,
        refresh_ms: u16,
    ) {
        let id = source.id();
        let id: &str = id.as_ref();
        if self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.id == id && loaded.key == key)
            || self
                .loading
                .as_ref()
                .is_some_and(|loading| loading.id == id && loading.key == key)
        {
            return;
        }
        // A latched failure holds off a fresh attempt until the cooldown
        // elapses, so a transient miss recovers without a per-frame retry storm.
        if let Some(failed) = self.failed.as_ref()
            && failed.id == id
            && failed.key == key
            && !retry_due(failed.failed_at_phase, phase, refresh_ms)
        {
            return;
        }
        self.loaded = None;
        let (sender, receiver) = mpsc::channel();
        let source = source.clone();
        let spawned = thread::Builder::new()
            .name("rimz-pet-assets".to_owned())
            .spawn(move || {
                let result = load_prepared_pet(source, key).map_err(|err| err.to_string());
                let _ = sender.send(result);
            });
        match spawned {
            Ok(_) => {
                self.loading = Some(LoadingPet {
                    id: id.to_owned(),
                    key,
                    receiver,
                });
            }
            Err(err) => {
                self.failed = Some(FailedPet {
                    id: id.to_owned(),
                    key,
                    caption: "pet unavailable".to_owned(),
                    failed_at_phase: phase,
                });
                tracing::debug!(pet = %id, error = %err, "pet asset loader failed");
            }
        }
    }

    fn loaded_grid(
        &self,
        pet_id: &str,
        key: PreparationKey,
        sprite_index: usize,
    ) -> Option<PetCellGrid> {
        let loaded = self.loaded.as_ref()?;
        if loaded.id != pet_id || loaded.key != key {
            return None;
        }
        match &loaded.asset {
            LoadedPetAsset::Cell(grids) => grids.get(sprite_index).cloned(),
            LoadedPetAsset::Pixel(_) => None,
        }
    }

    fn loaded_sprite(
        &mut self,
        request: LoadedSpriteRequest<'_>,
    ) -> Option<(usize, model::PetTrack)> {
        let LoadedSpriteRequest {
            pet_id,
            previous_action,
            frame,
        } = request;
        let jump_duration = {
            let loaded = self.loaded.as_ref()?;
            if loaded.id != pet_id {
                return None;
            }
            Some(
                model::animations()
                    .get(model::PetTrack::Jumping)
                    .loop_duration(frame.refresh_ms),
            )
        };
        let track = self.selected_track(TrackSelection {
            previous_action,
            action: frame.action,
            phase: frame.phase,
            refresh_ms: frame.refresh_ms,
            motion_enabled: frame.motion_enabled,
            jump_duration,
            unread_triggered: frame.unread_triggered,
        });
        let loaded = self.loaded.as_ref()?;
        if loaded.id != pet_id {
            return None;
        }
        let frame_count = match &loaded.asset {
            LoadedPetAsset::Cell(grids) => grids.len(),
            LoadedPetAsset::Pixel(frames) => frames.len(),
        };
        let animation = model::animations().get(track.track);
        let sprite_index = (if frame.motion_enabled {
            animation.sprite_index(track.phase, frame.refresh_ms)
        } else {
            animation.first_sprite()
        })
        .min(frame_count.saturating_sub(1));
        Some((sprite_index, track.track))
    }

    fn selected_track(&mut self, selection: TrackSelection) -> SelectedTrack {
        let TrackSelection {
            previous_action,
            action,
            phase,
            refresh_ms,
            motion_enabled,
            jump_duration,
            unread_triggered,
        } = selection;
        let steady = model::action_track(action);
        if !motion_enabled {
            self.jump_started_phase = None;
            return SelectedTrack {
                track: steady,
                phase,
            };
        }
        if model::action_changed(previous_action, action) || unread_triggered {
            self.jump_started_phase = jump_duration.map(|_| phase);
        }
        if let (Some(started), Some(duration)) = (self.jump_started_phase, jump_duration) {
            if phase_elapsed(started, phase, refresh_ms) < duration {
                return SelectedTrack {
                    track: model::PetTrack::Jumping,
                    phase: phase.saturating_sub(started),
                };
            }
            self.jump_started_phase = None;
        }
        SelectedTrack {
            track: steady,
            phase,
        }
    }
}

fn load_prepared_pet(
    source: PetSource,
    key: PreparationKey,
) -> Result<LoadedPetAsset, asset::AssetErr> {
    asset::resolve_and_decode(&source, |bytes| match key.tier {
        PetRenderTier::Cell => Ok(LoadedPetAsset::Cell(
            frames::decode_sheet(bytes)?
                .into_iter()
                .map(|frame| {
                    cellart::render_frame(&frame, key.size.cols, key.size.rows, key.aspect)
                })
                .collect(),
        )),
        PetRenderTier::Pixel => frames::decode_sheet(bytes).map(LoadedPetAsset::Pixel),
    })
}

#[cfg(test)]
mod tests;
