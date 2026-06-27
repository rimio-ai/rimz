use super::*;
use crate::config::PetsGlyphMode;

fn frame(
    action: PetAction,
    phase: u64,
    refresh_ms: u16,
    size: Option<PetGridSize>,
    motion_enabled: bool,
    unread_triggered: bool,
) -> PetViewFrame {
    PetViewFrame {
        action,
        phase,
        refresh_ms,
        size,
        motion_enabled,
        unread_triggered,
        tier: PetRenderTier::Cell(GlyphTier::Sextant),
    }
}

#[test]
fn dashboard_pet_size_uses_fixed_tier_footprints() {
    assert_eq!(DASHBOARD_PIXEL_PET, PetGridSize { cols: 15, rows: 9 });
    assert_eq!(DASHBOARD_CELL_PET, PetGridSize { cols: 18, rows: 9 });
    assert_eq!(
        dashboard_pet_size(PetRenderTier::Pixel),
        DASHBOARD_PIXEL_PET
    );
    assert_eq!(
        dashboard_pet_size(PetRenderTier::Cell(GlyphTier::Sextant)),
        DASHBOARD_CELL_PET
    );
    assert_eq!(
        dashboard_pet_size(PetRenderTier::Cell(GlyphTier::Octant)),
        DASHBOARD_CELL_PET
    );
}

#[test]
fn render_tier_resolves_mode_and_caps() {
    use PetsGlyphMode::{Auto, Octant, Pixel, Sextant};
    let caps = |pixel| PetRenderCaps { pixel };
    for (mode, caps, tier) in [
        (Auto, caps(true), PetRenderTier::Pixel),
        (Auto, caps(false), PetRenderTier::Cell(GlyphTier::Sextant)),
        (Pixel, caps(false), PetRenderTier::Cell(GlyphTier::Sextant)),
        (
            Octant,
            PetRenderCaps::default(),
            PetRenderTier::Cell(GlyphTier::Octant),
        ),
        (Sextant, caps(true), PetRenderTier::Cell(GlyphTier::Sextant)),
    ] {
        assert_eq!(resolve_render_tier(mode, caps), tier);
    }
}

#[test]
fn effective_render_tier_downgrades_only_unpaintable_pixels() {
    use PetsGlyphMode::{Octant, Pixel, Sextant};
    let pixel_caps = PetRenderCaps { pixel: true };

    assert_eq!(
        effective_render_tier(Pixel, pixel_caps, true),
        PetRenderTier::Pixel
    );
    assert_eq!(
        effective_render_tier(Pixel, pixel_caps, false),
        PetRenderTier::Cell(GlyphTier::Sextant)
    );
    assert_eq!(
        effective_render_tier(Octant, PetRenderCaps::default(), false),
        PetRenderTier::Cell(GlyphTier::Octant)
    );
    assert_eq!(
        effective_render_tier(Sextant, pixel_caps, false),
        PetRenderTier::Cell(GlyphTier::Sextant)
    );
}

#[test]
fn disabled_config_clears_runtime_state() {
    let mut assets = PetAssets {
        previous_action: Some(PetAction::Running),
        jump_started_phase: Some(1),
        previous_unread_rows: BTreeSet::from(["agent-1".to_owned()]),
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
                frame(
                    PetAction::Idle,
                    0,
                    100,
                    Some(PetGridSize { cols: 12, rows: 6 }),
                    true,
                    false,
                ),
            )
            .is_none()
    );
    assert_eq!(assets.previous_action, None);
    assert_eq!(assets.jump_started_phase, None);
    assert!(assets.previous_unread_rows.is_empty());
    assert_eq!(assets.caption, None);
    assert!(assets.failed.is_none());
}

#[test]
fn empty_pet_selector_rests_with_no_pet() {
    let mut assets = loaded_assets(Some(PetAction::Running));
    assets.jump_started_phase = Some(3);
    assets.previous_unread_rows = BTreeSet::from(["agent-1".to_owned()]);
    assets.caption = Some("running".to_owned());
    assets.failed = Some(FailedPet {
        id: "codex".to_owned(),
        caption: "pet unavailable".to_owned(),
        failed_at_phase: 0,
    });
    let config = PetsConfig {
        enabled: true,
        pet: "  ".to_owned(),
        glyphs: PetsGlyphMode::Auto,
        voice: true,
    };
    let view = assets
        .view(
            &config,
            frame(
                PetAction::Idle,
                0,
                100,
                Some(PetGridSize { cols: 12, rows: 6 }),
                true,
                false,
            ),
        )
        .expect("enabled pets produce a view");
    assert_eq!(view.grid, None);
    assert_eq!(view.caption.as_deref(), Some("no pet selected"));
    assert!(assets.loading.is_none(), "an empty selector loads nothing");
    assert!(assets.loaded.is_none());
    assert_eq!(assets.previous_action, None);
    assert_eq!(assets.jump_started_phase, None);
    assert_eq!(
        assets.previous_unread_rows,
        BTreeSet::from(["agent-1".to_owned()])
    );
    assert!(assets.failed.is_none());
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
            frame(
                PetAction::Idle,
                0,
                100,
                Some(PetGridSize { cols: 12, rows: 6 }),
                true,
                false,
            ),
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
        .view(&config, frame(PetAction::Idle, 0, 100, None, true, false))
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
            frame(
                PetAction::Idle,
                0,
                100,
                Some(PetGridSize { cols: 12, rows: 6 }),
                true,
                false,
            ),
        )
        .expect("enabled pets produce a view");
    assert!(!view.loading);
    assert_eq!(view.caption.as_deref(), Some("pet unavailable"));
    assert!(assets.loading.is_none());
    assert!(assets.failed.is_some());

    let view = assets
        .view(
            &config,
            frame(
                PetAction::Idle,
                1,
                100,
                Some(PetGridSize { cols: 12, rows: 6 }),
                true,
                false,
            ),
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
fn action_transition_jumps_once_then_settles() {
    let refresh_ms = 100;
    let mut assets = loaded_assets(Some(PetAction::Running));
    let config = enabled_config();
    let size = Some(PetGridSize { cols: 12, rows: 6 });

    let view = assets
        .view(
            &config,
            frame(PetAction::Ask, 10, refresh_ms, size, true, false),
        )
        .expect("enabled pets produce a view");
    assert_eq!(view.active_track, model::TRACK_JUMPING);
    assert_eq!(assets.jump_started_phase, Some(10));

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
            frame(
                PetAction::Ask,
                10 + phases as u64,
                refresh_ms,
                size,
                true,
                false,
            ),
        )
        .expect("enabled pets produce a view");
    assert_eq!(view.active_track, model::TRACK_ASK);
    assert_eq!(assets.jump_started_phase, None);
}

#[test]
fn unread_trigger_jumps_once_without_action_change() {
    let refresh_ms = 100;
    let mut assets = loaded_assets(Some(PetAction::Running));
    let config = enabled_config();
    let size = Some(PetGridSize { cols: 12, rows: 6 });

    let view = assets
        .view(
            &config,
            frame(PetAction::Running, 10, refresh_ms, size, true, true),
        )
        .expect("enabled pets produce a view");
    assert_eq!(view.active_track, model::TRACK_JUMPING);
    assert_eq!(assets.jump_started_phase, Some(10));

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
            frame(
                PetAction::Running,
                10 + phases as u64,
                refresh_ms,
                size,
                true,
                false,
            ),
        )
        .expect("enabled pets produce a view");
    assert_eq!(view.active_track, model::TRACK_RUNNING);
    assert_eq!(assets.jump_started_phase, None);
}

#[test]
fn static_mode_skips_transition_jump() {
    let mut assets = loaded_assets(Some(PetAction::Running));
    let view = assets
        .view(
            &enabled_config(),
            frame(
                PetAction::Idle,
                10,
                100,
                Some(PetGridSize { cols: 12, rows: 6 }),
                false,
                false,
            ),
        )
        .expect("enabled pets produce a view");

    assert_eq!(view.active_track, model::TRACK_IDLE);
    assert_eq!(assets.jump_started_phase, None);
}

#[test]
fn pixel_view_resolves_sprite_without_cell_grid() {
    let mut assets = loaded_assets(Some(PetAction::Running));
    let config = enabled_config();
    let size = PetGridSize { cols: 12, rows: 6 };

    let view = assets
        .view(
            &config,
            PetViewFrame {
                action: PetAction::Idle,
                phase: 0,
                refresh_ms: 100,
                size: Some(size),
                motion_enabled: true,
                unread_triggered: false,
                tier: PetRenderTier::Pixel,
            },
        )
        .expect("enabled pets produce a view");

    assert_eq!(view.grid, None);
    let pixel = view.pixel.expect("pixel view");
    assert_eq!(pixel.pet_id, "codex");
    assert_eq!(pixel.size, size);
    assert_eq!(view.active_track, model::TRACK_JUMPING);
    assert!(
        assets
            .pixel_frame(&pixel.pet_id, pixel.sprite_index)
            .is_some()
    );
    assert!(assets.loaded.as_ref().expect("loaded").memo.is_empty());
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
            .loaded_grid(LoadedGridRequest {
                pet_id: "codex",
                previous_action: None,
                frame: PetViewFrame {
                    action: PetAction::Idle,
                    phase: 0,
                    refresh_ms: 100,
                    size: Some(PetGridSize { cols: 12, rows: 6 }),
                    motion_enabled: true,
                    unread_triggered: false,
                    tier: PetRenderTier::Cell(GlyphTier::Sextant),
                },
                size: PetGridSize { cols: 12, rows: 6 },
                tier: GlyphTier::Sextant,
            })
            .is_some()
    );
    assert_eq!(assets.loaded.as_ref().expect("loaded").memo.len(), 1);

    assert!(
        assets
            .loaded_grid(LoadedGridRequest {
                pet_id: "codex",
                previous_action: None,
                frame: PetViewFrame {
                    action: PetAction::Idle,
                    phase: 0,
                    refresh_ms: 100,
                    size: Some(PetGridSize { cols: 13, rows: 6 }),
                    motion_enabled: true,
                    unread_triggered: false,
                    tier: PetRenderTier::Cell(GlyphTier::Sextant),
                },
                size: PetGridSize { cols: 13, rows: 6 },
                tier: GlyphTier::Sextant,
            })
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

fn loaded_assets(previous_action: Option<PetAction>) -> PetAssets {
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
        previous_action,
        ..PetAssets::default()
    }
}
