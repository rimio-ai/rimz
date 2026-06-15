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
mod voice;

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::config::{PetsConfig, PetsGlyphMode};

#[cfg(test)]
pub(crate) use cellart::PetCell;
pub(crate) use cellart::PetCellGrid;
pub(crate) use model::FleetPetStatus;

use asset::PetSource;
use frames::RgbaImage;
use model::AnimationSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PetView {
    pub(crate) grid: Option<PetCellGrid>,
    pub(crate) caption: Option<String>,
    pub(crate) loading: bool,
    pub(crate) status: FleetPetStatus,
    pub(crate) active_track: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PetGridSize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl PetGridSize {
    const MIN_COLS: u16 = 12;
    const MAX_COLS: u16 = 20;
    const MIN_ROWS: u16 = 4;
    const MAX_ROWS: u16 = 10;

    pub(crate) fn for_sidebar_space(width: u16, height: u16) -> Option<Self> {
        if width < Self::MIN_COLS {
            return None;
        }
        let cols = width
            .saturating_sub(10)
            .clamp(Self::MIN_COLS, Self::MAX_COLS);
        let rows = ((u32::from(cols) * catalog::FRAME_HEIGHT) / catalog::FRAME_WIDTH / 2)
            .clamp(u32::from(Self::MIN_ROWS), u32::from(Self::MAX_ROWS)) as u16;
        let max_rows_for_height = height / 3;
        if max_rows_for_height < Self::MIN_ROWS {
            return None;
        }
        Some(Self {
            cols,
            rows: rows.min(max_rows_for_height.min(Self::MAX_ROWS)),
        })
    }
}

#[derive(Default)]
pub(crate) struct PetAssets {
    loaded: Option<LoadedPet>,
    loading: Option<LoadingPet>,
    failed: Option<FailedPet>,
    previous_status: Option<FleetPetStatus>,
    celebrate_started_phase: Option<u64>,
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
    animations: AnimationSet,
    memo: HashMap<MemoKey, PetCellGrid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct MemoKey {
    sprite_index: usize,
    cols: u16,
    rows: u16,
    glyphs: PetsGlyphMode,
}

type LoadResult = Result<Vec<RgbaImage>, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SelectedTrack {
    name: &'static str,
    phase: u64,
}

impl PetAssets {
    pub(crate) fn view(
        &mut self,
        config: &PetsConfig,
        status: FleetPetStatus,
        phase: u64,
        refresh_ms: u16,
        size: Option<PetGridSize>,
        motion_enabled: bool,
    ) -> Option<PetView> {
        if !config.enabled {
            self.loaded = None;
            self.loading = None;
            self.failed = None;
            self.previous_status = None;
            self.celebrate_started_phase = None;
            self.caption = None;
            return None;
        }

        let previous_status = self.previous_status;
        let mut active_track = model::fleet_track(status);
        let Some(source) = asset::resolve_pet_source(&config.pet) else {
            self.loaded = None;
            self.loading = None;
            self.failed = None;
            self.previous_status = None;
            self.celebrate_started_phase = None;
            self.caption = Some("no pet selected".to_owned());
            return Some(PetView {
                grid: None,
                caption: self.caption.clone(),
                loading: false,
                status,
                active_track,
            });
        };
        let id = source.id();
        let id: &str = id.as_ref();

        self.poll_loader(phase);
        self.clear_mismatched_pet(id);
        if size.is_some() {
            self.ensure_loading(&source, phase, refresh_ms);
        }
        self.observe_status(status, config.voice, phase);

        let loading = size.is_some()
            && self
                .loading
                .as_ref()
                .is_some_and(|loading| loading.id == id);
        let unavailable_caption = self
            .failed
            .as_ref()
            .filter(|failed| failed.id == id)
            .map(|failed| failed.caption.clone());
        let grid = size.and_then(|size| {
            self.loaded_grid(
                id,
                previous_status,
                status,
                phase,
                refresh_ms,
                size,
                config.glyphs,
                motion_enabled,
            )
            .map(|(grid, track)| {
                active_track = track;
                grid
            })
        });
        Some(PetView {
            grid,
            caption: unavailable_caption
                .or_else(|| self.caption.clone())
                .or_else(|| loading.then(|| "fetching pet...".to_owned())),
            loading,
            status,
            active_track,
        })
    }

    fn observe_status(&mut self, status: FleetPetStatus, voice: bool, seed: u64) {
        if !voice {
            self.caption = None;
            self.previous_status = Some(status);
            return;
        }
        // `voice::caption` returns `Some` only on a status transition, so a
        // `None` means same-status and the prior caption stands.
        if let Some(next) = voice::caption(self.previous_status, status, seed) {
            self.caption = Some(next.to_owned());
        }
        self.previous_status = Some(status);
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
                    animations: model::default_animations(),
                    memo: HashMap::new(),
                });
                self.failed = None;
            }
            Err(err) => {
                tracing::debug!(pet = %id, error = %err, "pet asset unavailable");
                self.loaded = None;
                self.celebrate_started_phase = None;
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
            self.celebrate_started_phase = None;
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

    #[allow(clippy::too_many_arguments)]
    fn loaded_grid(
        &mut self,
        pet_id: &str,
        previous_status: Option<FleetPetStatus>,
        status: FleetPetStatus,
        phase: u64,
        refresh_ms: u16,
        size: PetGridSize,
        glyphs: PetsGlyphMode,
        motion_enabled: bool,
    ) -> Option<(PetCellGrid, &'static str)> {
        let jump_duration = {
            let loaded = self.loaded.as_ref()?;
            if loaded.id != pet_id {
                return None;
            }
            loaded
                .animations
                .get(model::TRACK_JUMPING)
                .map(|animation| animation.loop_duration(refresh_ms))
        };
        let track = self.selected_track(
            previous_status,
            status,
            phase,
            refresh_ms,
            motion_enabled,
            jump_duration,
        );
        let loaded = self.loaded.as_mut()?;
        if loaded.id != pet_id {
            return None;
        }
        let sprite_index = loaded
            .animations
            .get(track.name)
            .map(|animation| {
                if motion_enabled {
                    animation.sprite_index(track.phase, refresh_ms)
                } else {
                    animation.first_sprite()
                }
            })
            .unwrap_or(0)
            .min(loaded.frames.len().saturating_sub(1));
        let key = MemoKey {
            sprite_index,
            cols: size.cols,
            rows: size.rows,
            glyphs,
        };
        loaded.memo.retain(|memo_key, _| {
            memo_key.cols == size.cols && memo_key.rows == size.rows && memo_key.glyphs == glyphs
        });
        if let Some(grid) = loaded.memo.get(&key) {
            return Some((grid.clone(), track.name));
        }
        let frame = loaded.frames.get(sprite_index)?;
        let grid = cellart::render_frame(frame, size.cols, size.rows, glyphs);
        loaded.memo.insert(key, grid.clone());
        Some((grid, track.name))
    }

    fn selected_track(
        &mut self,
        previous_status: Option<FleetPetStatus>,
        status: FleetPetStatus,
        phase: u64,
        refresh_ms: u16,
        motion_enabled: bool,
        jump_duration: Option<std::time::Duration>,
    ) -> SelectedTrack {
        let steady = model::fleet_track(status);
        if !motion_enabled || status != FleetPetStatus::Idle {
            self.celebrate_started_phase = None;
            return SelectedTrack {
                name: steady,
                phase,
            };
        }
        if model::one_shot_track(previous_status, status).is_some() {
            self.celebrate_started_phase = jump_duration.map(|_| phase);
        }
        if let (Some(started), Some(duration)) = (self.celebrate_started_phase, jump_duration) {
            if phase_elapsed(started, phase, refresh_ms) < duration {
                return SelectedTrack {
                    name: model::TRACK_JUMPING,
                    phase: phase.saturating_sub(started),
                };
            }
            self.celebrate_started_phase = None;
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
mod tests {
    use super::*;
    use crate::config::PetsGlyphMode;

    #[test]
    fn pet_grid_size_tracks_sidebar_width_inside_bounds() {
        assert_eq!(
            PetGridSize::for_sidebar_space(20, 34).expect("size").cols,
            12
        );
        let wide = PetGridSize::for_sidebar_space(54, 34).expect("size");
        assert_eq!((wide.cols, wide.rows), (20, 10), "caps at 20x10");
        assert_eq!(
            PetGridSize::for_sidebar_space(200, 34).expect("size").cols,
            20
        );
        assert_eq!(
            PetGridSize::for_sidebar_space(54, 20).expect("size").rows,
            6
        );
        assert!(PetGridSize::for_sidebar_space(11, 34).is_none());
        assert!(PetGridSize::for_sidebar_space(54, 11).is_none());
    }

    #[test]
    fn disabled_config_clears_runtime_state() {
        let mut assets = PetAssets {
            previous_status: Some(FleetPetStatus::Running),
            celebrate_started_phase: Some(1),
            caption: Some("x".to_owned()),
            failed: Some(FailedPet {
                id: "codex".to_owned(),
                caption: "pet unavailable".to_owned(),
                failed_at_phase: 0,
            }),
            ..PetAssets::default()
        };
        assert!(
            assets
                .view(
                    &PetsConfig::default(),
                    FleetPetStatus::Idle,
                    0,
                    100,
                    Some(PetGridSize { cols: 12, rows: 6 }),
                    true,
                )
                .is_none()
        );
        assert_eq!(assets.previous_status, None);
        assert_eq!(assets.celebrate_started_phase, None);
        assert_eq!(assets.caption, None);
        assert!(assets.failed.is_none());
    }

    #[test]
    fn empty_pet_selector_rests_with_no_pet() {
        let mut assets = PetAssets::default();
        let config = PetsConfig {
            enabled: true,
            pet: "  ".to_owned(),
            glyphs: PetsGlyphMode::Auto,
            voice: true,
        };
        let view = assets
            .view(
                &config,
                FleetPetStatus::Idle,
                0,
                100,
                Some(PetGridSize { cols: 12, rows: 6 }),
                true,
            )
            .expect("enabled pets produce a view");
        assert_eq!(view.grid, None);
        assert_eq!(view.caption.as_deref(), Some("no pet selected"));
        assert!(assets.loading.is_none(), "an empty selector loads nothing");
    }

    #[test]
    fn local_pet_path_begins_loading_under_its_own_id() {
        let mut assets = PetAssets::default();
        let config = PetsConfig {
            enabled: true,
            pet: "/no/such/pet/sheet.webp".to_owned(),
            glyphs: PetsGlyphMode::Auto,
            voice: true,
        };
        // `poll_loader` runs before the spawn, so the first view always reports
        // loading regardless of how fast the loader thread fails the read.
        let view = assets
            .view(
                &config,
                FleetPetStatus::Idle,
                0,
                100,
                Some(PetGridSize { cols: 12, rows: 6 }),
                true,
            )
            .expect("enabled pets produce a view");
        assert_eq!(view.grid, None);
        assert!(view.loading, "a local-path selector spawns a loader");
        assert!(
            assets
                .loading
                .as_ref()
                .is_some_and(|loading| loading.id == "/no/such/pet/sheet.webp"),
            "the loader is keyed by the local path"
        );
    }

    #[test]
    fn missing_body_size_does_not_start_asset_loading() {
        let mut assets = PetAssets::default();
        let config = PetsConfig {
            enabled: true,
            pet: "codex".to_owned(),
            glyphs: PetsGlyphMode::Auto,
            voice: true,
        };

        let view = assets
            .view(&config, FleetPetStatus::Idle, 0, 100, None, true)
            .expect("enabled pets produce a view");

        assert_eq!(view.grid, None);
        assert!(!view.loading);
        assert_eq!(view.caption.as_deref(), Some("resting"));
        assert!(assets.loading.is_none());
    }

    #[test]
    fn failed_loader_settles_without_immediate_retry() {
        let mut assets = PetAssets::default();
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Err("offline".to_owned()))
            .expect("send failure");
        assets.loading = Some(LoadingPet {
            id: "codex".to_owned(),
            receiver,
        });
        let config = PetsConfig {
            enabled: true,
            pet: "codex".to_owned(),
            glyphs: PetsGlyphMode::Auto,
            voice: true,
        };

        let view = assets
            .view(
                &config,
                FleetPetStatus::Idle,
                0,
                100,
                Some(PetGridSize { cols: 12, rows: 6 }),
                true,
            )
            .expect("enabled pets produce a view");
        assert!(!view.loading);
        assert_eq!(view.caption.as_deref(), Some("pet unavailable"));
        assert!(assets.loading.is_none());
        assert!(assets.failed.is_some());

        let view = assets
            .view(
                &config,
                FleetPetStatus::Idle,
                1,
                100,
                Some(PetGridSize { cols: 12, rows: 6 }),
                true,
            )
            .expect("enabled pets produce a view");
        assert!(!view.loading);
        assert!(assets.loading.is_none());
    }

    #[test]
    fn retry_due_waits_for_cooldown_then_clears() {
        // 20s cooldown at 100ms refresh is 200 phases.
        assert!(!retry_due(0, 0, 100));
        assert!(!retry_due(0, 199, 100));
        assert!(retry_due(0, 200, 100));
        assert!(retry_due(0, 5_000, 100));
        // The window tracks the failure point, not the origin.
        assert!(!retry_due(1_000, 1_100, 100));
        assert!(retry_due(1_000, 1_200, 100));
    }

    #[test]
    fn work_to_idle_transition_jumps_once_then_settles() {
        let refresh_ms = 100;
        let mut assets = loaded_assets(Some(FleetPetStatus::Running));
        let config = enabled_config();
        let size = Some(PetGridSize { cols: 12, rows: 6 });

        let view = assets
            .view(&config, FleetPetStatus::Idle, 10, refresh_ms, size, true)
            .expect("enabled pets produce a view");
        assert_eq!(view.active_track, model::TRACK_JUMPING);
        assert_eq!(assets.celebrate_started_phase, Some(10));

        let jump = model::default_animations()
            .remove(model::TRACK_JUMPING)
            .expect("jumping track");
        let phases = jump
            .loop_duration(refresh_ms)
            .as_millis()
            .div_ceil(u128::from(refresh_ms));
        let view = assets
            .view(
                &config,
                FleetPetStatus::Idle,
                10 + phases as u64,
                refresh_ms,
                size,
                true,
            )
            .expect("enabled pets produce a view");
        assert_eq!(view.active_track, model::TRACK_IDLE);
        assert_eq!(assets.celebrate_started_phase, None);
    }

    #[test]
    fn static_mode_skips_completion_jump() {
        let mut assets = loaded_assets(Some(FleetPetStatus::Running));
        let view = assets
            .view(
                &enabled_config(),
                FleetPetStatus::Idle,
                10,
                100,
                Some(PetGridSize { cols: 12, rows: 6 }),
                false,
            )
            .expect("enabled pets produce a view");

        assert_eq!(view.active_track, model::TRACK_IDLE);
        assert_eq!(assets.celebrate_started_phase, None);
    }

    #[test]
    fn memoized_grids_are_evicted_on_resize() {
        let frame = RgbaImage {
            width: 1,
            height: 1,
            data: vec![255, 0, 0, 255],
        };
        let mut assets = PetAssets {
            loaded: Some(LoadedPet {
                id: "codex".to_owned(),
                frames: vec![frame; catalog::FRAME_COUNT],
                animations: model::default_animations(),
                memo: HashMap::new(),
            }),
            ..PetAssets::default()
        };

        assert!(
            assets
                .loaded_grid(
                    "codex",
                    None,
                    FleetPetStatus::Idle,
                    0,
                    100,
                    PetGridSize { cols: 12, rows: 6 },
                    PetsGlyphMode::Half,
                    true,
                )
                .is_some()
        );
        assert_eq!(assets.loaded.as_ref().expect("loaded").memo.len(), 1);

        assert!(
            assets
                .loaded_grid(
                    "codex",
                    None,
                    FleetPetStatus::Idle,
                    0,
                    100,
                    PetGridSize { cols: 13, rows: 6 },
                    PetsGlyphMode::Half,
                    true,
                )
                .is_some()
        );
        let memo = &assets.loaded.as_ref().expect("loaded").memo;
        assert_eq!(memo.len(), 1);
        assert!(memo.keys().all(|key| key.cols == 13));
    }

    fn enabled_config() -> PetsConfig {
        PetsConfig {
            enabled: true,
            pet: "codex".to_owned(),
            glyphs: PetsGlyphMode::Auto,
            voice: true,
        }
    }

    fn loaded_assets(previous_status: Option<FleetPetStatus>) -> PetAssets {
        let frame = RgbaImage {
            width: 1,
            height: 1,
            data: vec![255, 0, 0, 255],
        };
        PetAssets {
            loaded: Some(LoadedPet {
                id: "codex".to_owned(),
                frames: vec![frame; catalog::FRAME_COUNT],
                animations: model::default_animations(),
                memo: HashMap::new(),
            }),
            previous_status,
            ..PetAssets::default()
        }
    }
}
