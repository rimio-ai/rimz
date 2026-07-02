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
mod pixel;
mod preview;
mod voice;

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::config::{PetsConfig, PetsGlyphMode};

#[cfg(test)]
pub(crate) use cellart::PetCell;
pub(crate) use cellart::PetCellGrid;
pub(crate) use model::PetAction;
pub(crate) use pixel::PixelPainter;
pub(crate) use pixel::probe::detect as detect_pet_render_caps;
pub use pixel::probe::{PetRenderCaps, detect_env as detect_pet_render_env};
pub use pixel::{
    inline_placeholder_row, transmit_rgba_chunks, virtual_place, wrap_pixel_payload,
    write_synchronized_pixel_output,
};
pub use preview::{
    PetPixelPreview, PetPreview, PixelPreviewFrame, PreviewCell, listable_ids, load_cell_previews,
    load_pixel_previews,
};

use asset::PetSource;
use frames::RgbaImage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PetView {
    pub(crate) body: Option<PetBody>,
    pub(crate) caption: Option<String>,
    pub(crate) loading: bool,
    pub(crate) action: PetAction,
    pub(crate) active_track: &'static str,
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
    pub(crate) size: PetGridSize,
}

impl PetView {
    pub(crate) fn has_body(&self) -> bool {
        self.body.is_some()
    }
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
    pub(crate) motion_enabled: bool,
    pub(crate) unread_triggered: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PetRenderTier {
    Pixel,
    Cell,
}

pub fn resolve_render_tier(mode: PetsGlyphMode, caps: PetRenderCaps) -> PetRenderTier {
    match mode {
        PetsGlyphMode::Sextant => PetRenderTier::Cell,
        PetsGlyphMode::Pixel if caps.pixel_transport => PetRenderTier::Pixel,
        PetsGlyphMode::Auto if caps.pixel_transport && caps.kitty_term => PetRenderTier::Pixel,
        _ => PetRenderTier::Cell,
    }
}

/// The tier actually painted this frame: `resolve_render_tier` downgraded to
/// cell art when pixels resolve but cannot paint here — no provider block to
/// ride, or a suppressed body. Cell tiers pass through untouched.
pub(crate) fn effective_render_tier(
    mode: PetsGlyphMode,
    caps: PetRenderCaps,
    pixel_paintable: bool,
) -> PetRenderTier {
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
    receiver: Receiver<LoadResult>,
}

struct FailedPet {
    id: String,
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
    frames: Vec<RgbaImage>,
    memo: HashMap<MemoKey, PetCellGrid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct MemoKey {
    sprite_index: usize,
    cols: u16,
    rows: u16,
}

type LoadResult = Result<Vec<RgbaImage>, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SelectedTrack {
    name: &'static str,
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
                frames: vec![frame; catalog::FRAME_COUNT],
                memo: HashMap::new(),
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
                loading: false,
                action,
                active_track,
            });
        };
        let id = source.id();
        let id: &str = id.as_ref();

        self.poll_loader(phase);
        self.clear_mismatched_pet(id);
        if body_tier.is_some() {
            self.ensure_loading(&source, phase, refresh_ms);
        }
        self.observe_action(action, config.voice, phase);

        let loading = body_tier.is_some()
            && self
                .loading
                .as_ref()
                .is_some_and(|loading| loading.id == id);
        let unavailable_caption = self
            .failed
            .as_ref()
            .filter(|failed| failed.id == id)
            .map(|failed| failed.caption.clone());
        let body = body_tier.and_then(|tier| {
            let size = dashboard_pet_size(tier);
            let (sprite_index, track) = self.loaded_sprite(LoadedSpriteRequest {
                pet_id: id,
                previous_action,
                frame,
            })?;
            match tier {
                PetRenderTier::Pixel => {
                    active_track = track;
                    Some(PetBody::Pixel(PetPixelView {
                        pet_id: id.to_owned(),
                        sprite_index,
                        size,
                    }))
                }
                PetRenderTier::Cell => self.loaded_grid(id, sprite_index, size).map(|grid| {
                    active_track = track;
                    PetBody::Cell(grid)
                }),
            }
        });
        Some(PetView {
            body,
            caption: unavailable_caption
                .or_else(|| self.caption.clone())
                .or_else(|| loading.then(|| "fetching pet...".to_owned())),
            loading,
            action,
            active_track,
        })
    }

    pub(crate) fn pixel_frame(&self, pet_id: &str, sprite_index: usize) -> Option<&RgbaImage> {
        let loaded = self.loaded.as_ref()?;
        (loaded.id == pet_id)
            .then(|| loaded.frames.get(sprite_index))
            .flatten()
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
        self.loading = None;
        match result {
            Ok(frames) => {
                self.loaded = Some(LoadedPet {
                    id,
                    frames,
                    memo: HashMap::new(),
                });
                self.failed = None;
            }
            Err(err) => {
                tracing::debug!(pet = %id, error = %err, "pet asset unavailable");
                self.loaded = None;
                self.jump_started_phase = None;
                self.failed = Some(FailedPet {
                    id,
                    caption: "pet unavailable".to_owned(),
                    failed_at_phase: phase,
                });
            }
        }
    }

    fn clear_mismatched_pet(&mut self, pet_id: &str) {
        if self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.id != pet_id)
        {
            self.loaded = None;
            self.jump_started_phase = None;
        }
        if self
            .loading
            .as_ref()
            .is_some_and(|loading| loading.id != pet_id)
        {
            self.loading = None;
        }
        if self
            .failed
            .as_ref()
            .is_some_and(|failed| failed.id != pet_id)
        {
            self.failed = None;
        }
    }

    fn ensure_loading(&mut self, source: &PetSource, phase: u64, refresh_ms: u16) {
        let id = source.id();
        let id: &str = id.as_ref();
        if self.loaded.as_ref().is_some_and(|loaded| loaded.id == id)
            || self
                .loading
                .as_ref()
                .is_some_and(|loading| loading.id == id)
        {
            return;
        }
        // A latched failure holds off a fresh attempt until the cooldown
        // elapses, so a transient miss recovers without a per-frame retry storm.
        if let Some(failed) = self.failed.as_ref()
            && failed.id == id
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
                let result = load_pet(source).map_err(|err| err.to_string());
                let _ = sender.send(result);
            });
        match spawned {
            Ok(_) => {
                self.loading = Some(LoadingPet {
                    id: id.to_owned(),
                    receiver,
                });
            }
            Err(err) => {
                self.failed = Some(FailedPet {
                    id: id.to_owned(),
                    caption: "pet unavailable".to_owned(),
                    failed_at_phase: phase,
                });
                tracing::debug!(pet = %id, error = %err, "pet asset loader failed");
            }
        }
    }

    fn loaded_grid(
        &mut self,
        pet_id: &str,
        sprite_index: usize,
        size: PetGridSize,
    ) -> Option<PetCellGrid> {
        let loaded = self.loaded.as_mut()?;
        if loaded.id != pet_id {
            return None;
        }
        let key = MemoKey {
            sprite_index,
            cols: size.cols,
            rows: size.rows,
        };
        loaded
            .memo
            .retain(|memo_key, _| memo_key.cols == size.cols && memo_key.rows == size.rows);
        if let Some(grid) = loaded.memo.get(&key) {
            return Some(grid.clone());
        }
        let frame = loaded.frames.get(sprite_index)?;
        let grid = cellart::render_frame(frame, size.cols, size.rows);
        loaded.memo.insert(key, grid.clone());
        Some(grid)
    }

    fn loaded_sprite(&mut self, request: LoadedSpriteRequest<'_>) -> Option<(usize, &'static str)> {
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
            model::animations()
                .get(model::TRACK_JUMPING)
                .map(|animation| animation.loop_duration(frame.refresh_ms))
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
        let sprite_index = model::animations()
            .get(track.name)
            .map(|animation| {
                if frame.motion_enabled {
                    animation.sprite_index(track.phase, frame.refresh_ms)
                } else {
                    animation.first_sprite()
                }
            })
            .unwrap_or(0)
            .min(loaded.frames.len().saturating_sub(1));
        Some((sprite_index, track.name))
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
                name: steady,
                phase,
            };
        }
        if model::action_changed(previous_action, action) || unread_triggered {
            self.jump_started_phase = jump_duration.map(|_| phase);
        }
        if let (Some(started), Some(duration)) = (self.jump_started_phase, jump_duration) {
            if phase_elapsed(started, phase, refresh_ms) < duration {
                return SelectedTrack {
                    name: model::TRACK_JUMPING,
                    phase: phase.saturating_sub(started),
                };
            }
            self.jump_started_phase = None;
        }
        SelectedTrack {
            name: steady,
            phase,
        }
    }
}

pub(crate) fn animation_frame(track: &str, refresh_ms: u16) -> std::time::Duration {
    model::track_frame_duration(track, refresh_ms)
}

fn load_pet(source: PetSource) -> Result<Vec<RgbaImage>, asset::AssetErr> {
    let resolved = asset::resolve_asset(&source)?;
    match frames::decode_sheet(&resolved.bytes) {
        Ok(frames) => Ok(frames),
        Err(err) => {
            // Evict only a cache entry on a decode miss; a user's local sheet
            // is read-only to Rimz.
            if let Some(path) = &resolved.evictable_cache {
                let _ = asset::remove_cached_asset(path);
            }
            Err(asset::AssetErr::Decode(err))
        }
    }
}

#[cfg(test)]
mod tests;
