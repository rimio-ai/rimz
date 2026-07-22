use super::*;
use crate::config::{PetsGlyphMode, PixelMode};

const REFRESH_MS: u16 = 100;

fn frame(
    action: PetAction,
    phase: u64,
    refresh_ms: u16,
    body: Option<PetRenderTier>,
    motion_enabled: bool,
    unread_triggered: bool,
) -> PetViewFrame {
    PetViewFrame {
        action,
        phase,
        refresh_ms,
        body,
        pixel_id_base: 0x120000,
        cell_aspect: CellAspect::NEUTRAL,
        motion_enabled,
        unread_triggered,
    }
}

/// The common frame: a cell-tier body at the default refresh, motion on, no
/// unread trigger.
fn cell_frame(action: PetAction, phase: u64) -> PetViewFrame {
    frame(
        action,
        phase,
        REFRESH_MS,
        Some(PetRenderTier::Cell),
        true,
        false,
    )
}

#[test]
fn render_tier_resolves_mode_caps_and_paintability() {
    use PetsGlyphMode::{Auto, Pixel, Sextant};
    let caps = |pixel_transport, kitty_clients| PixelRenderCaps {
        pixel_transport,
        kitty_clients,
    };

    for (mode, caps, pixel_paintable, tier) in [
        (Auto, caps(true, true), true, PetRenderTier::Pixel),
        (Auto, caps(true, true), false, PetRenderTier::Cell),
        (Auto, caps(true, false), true, PetRenderTier::Cell),
        (Auto, caps(false, true), true, PetRenderTier::Cell),
        (Pixel, caps(true, false), true, PetRenderTier::Pixel),
        (Pixel, caps(true, false), false, PetRenderTier::Cell),
        (Pixel, caps(false, true), true, PetRenderTier::Cell),
        (Sextant, caps(true, true), false, PetRenderTier::Cell),
    ] {
        assert_eq!(
            effective_render_tier(mode, PixelMode::Auto, caps, pixel_paintable),
            tier
        );
    }

    assert_eq!(
        effective_render_tier(Pixel, PixelMode::Off, caps(true, true), true,),
        PetRenderTier::Cell
    );
}

#[test]
fn disabled_config_clears_runtime_state() {
    let (_sender, receiver) = mpsc::channel();
    let mut assets = PetAssets {
        loaded: loaded_cell_assets(None).loaded,
        loading: Some(LoadingPet {
            id: "codex".to_owned(),
            key: cell_key(),
            receiver,
        }),
        previous_action: Some(PetAction::Running),
        jump_started_phase: Some(1),
        previous_unread_rows: BTreeSet::from(["agent-1".to_owned()]),
        caption: Some("x".to_owned()),
        failed: Some(FailedPet {
            id: "codex".to_owned(),
            key: cell_key(),
            caption: "pet unavailable".to_owned(),
            failed_at_phase: 0,
        }),
    };
    assert!(
        assets
            .view(&PetsConfig::default(), cell_frame(PetAction::Idle, 0))
            .is_none()
    );
    assert_eq!(assets.previous_action, None);
    assert_eq!(assets.jump_started_phase, None);
    assert!(assets.previous_unread_rows.is_empty());
    assert_eq!(assets.caption, None);
    assert!(assets.failed.is_none());
    assert!(assets.loaded.is_none());
    assert!(assets.loading.is_none());
}

#[test]
fn empty_pet_selector_rests_with_no_pet() {
    let mut assets = loaded_cell_assets(Some(PetAction::Running));
    assets.jump_started_phase = Some(3);
    assets.previous_unread_rows = BTreeSet::from(["agent-1".to_owned()]);
    assets.caption = Some("running".to_owned());
    assets.failed = Some(FailedPet {
        id: "codex".to_owned(),
        key: cell_key(),
        caption: "pet unavailable".to_owned(),
        failed_at_phase: 0,
    });
    let view = assets
        .view(&config_for("  "), cell_frame(PetAction::Idle, 0))
        .expect("enabled pets produce a view");
    assert_eq!(view.body, None);
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
    // `poll_loader` runs before the spawn, so the first view always reports
    // loading regardless of how fast the loader thread fails the read.
    let view = assets
        .view(
            &config_for("/no/such/pet/sheet.webp"),
            cell_frame(PetAction::Idle, 0),
        )
        .expect("enabled pets produce a view");
    assert_eq!(view.body, None);
    assert_eq!(
        view.frame_interval,
        Some(crate::sidebar::timing::animation_frame(REFRESH_MS)),
        "a local-path selector uses loading cadence"
    );
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
    let view = assets
        .view(
            &enabled_config(),
            frame(PetAction::Idle, 0, REFRESH_MS, None, true, false),
        )
        .expect("enabled pets produce a view");

    assert_eq!(view.body, None);
    assert_eq!(view.frame_interval, None);
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
        key: cell_key(),
        receiver,
    });
    let config = enabled_config();

    let view = assets
        .view(&config, cell_frame(PetAction::Idle, 0))
        .expect("enabled pets produce a view");
    assert_eq!(view.frame_interval, None);
    assert_eq!(view.caption.as_deref(), Some("pet unavailable"));
    assert!(assets.loading.is_none());
    assert!(assets.failed.is_some());

    let view = assets
        .view(&config, cell_frame(PetAction::Idle, 1))
        .expect("enabled pets produce a view");
    assert_eq!(view.frame_interval, None);
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
fn jump_plays_once_per_trigger_then_settles() {
    let config = enabled_config();
    for (previous, action, unread_triggered, steady_track) in [
        (
            PetAction::Running,
            PetAction::Ask,
            false,
            model::PetTrack::Ask,
        ),
        (
            PetAction::Running,
            PetAction::Running,
            true,
            model::PetTrack::Running,
        ),
    ] {
        let mut assets = loaded_cell_assets(Some(previous));

        let view = assets
            .view(
                &config,
                frame(
                    action,
                    10,
                    REFRESH_MS,
                    Some(PetRenderTier::Cell),
                    true,
                    unread_triggered,
                ),
            )
            .expect("enabled pets produce a view");
        assert_eq!(
            view.frame_interval,
            Some(model::track_frame_duration(
                model::PetTrack::Jumping,
                REFRESH_MS
            ))
        );
        assert_eq!(assets.jump_started_phase, Some(10));

        let jump = model::animations().get(model::PetTrack::Jumping);
        let phases = jump
            .loop_duration(REFRESH_MS)
            .as_millis()
            .div_ceil(u128::from(REFRESH_MS));
        let view = assets
            .view(&config, cell_frame(action, 10 + phases as u64))
            .expect("enabled pets produce a view");
        assert_eq!(
            view.frame_interval,
            Some(model::track_frame_duration(steady_track, REFRESH_MS))
        );
        assert_eq!(assets.jump_started_phase, None);
    }
}

#[test]
fn static_mode_skips_transition_jump() {
    let mut assets = loaded_cell_assets(Some(PetAction::Running));
    let view = assets
        .view(
            &enabled_config(),
            frame(
                PetAction::Idle,
                10,
                REFRESH_MS,
                Some(PetRenderTier::Cell),
                false,
                false,
            ),
        )
        .expect("enabled pets produce a view");

    assert_eq!(view.frame_interval, None);
    assert_eq!(assets.jump_started_phase, None);
}

#[test]
fn pixel_view_resolves_sprite_without_cell_grid() {
    let mut assets = loaded_pixel_assets(Some(PetAction::Running));

    let view = assets
        .view(
            &enabled_config(),
            frame(
                PetAction::Idle,
                0,
                REFRESH_MS,
                Some(PetRenderTier::Pixel),
                true,
                false,
            ),
        )
        .expect("enabled pets produce a view");

    let PetBody::Pixel(pixel) = view.body.expect("pixel view") else {
        panic!("expected pixel body");
    };
    assert_eq!(pixel.pet_id, "codex");
    assert_eq!(pixel.size, DASHBOARD_PIXEL_PET);
    assert_eq!(pixel.image_id, 0x120000 + pixel.sprite_index as u32);
    assert_eq!(
        view.frame_interval,
        Some(model::track_frame_duration(
            model::PetTrack::Jumping,
            REFRESH_MS
        ))
    );
    assert!(
        assets
            .pixel_frame(&pixel.pet_id, pixel.sprite_index)
            .is_some()
    );
    assert!(matches!(
        assets.loaded.as_ref().expect("loaded").asset,
        LoadedPetAsset::Pixel(_)
    ));
}

#[test]
fn prepared_grids_are_invalidated_on_size_or_aspect_change() {
    let key = cell_key();
    let mut assets = PetAssets {
        loaded: Some(LoadedPet {
            id: "codex".to_owned(),
            key,
            asset: LoadedPetAsset::Cell(vec![vec![]; catalog::FRAME_COUNT]),
        }),
        ..PetAssets::default()
    };

    assert!(assets.loaded_grid("codex", key, 0).is_some());
    assets.clear_mismatched_pet(
        "codex",
        Some(PreparationKey {
            size: PetGridSize { cols: 13, rows: 6 },
            ..key
        }),
    );
    assert!(assets.loaded.is_none());

    assets.loaded = Some(LoadedPet {
        id: "codex".to_owned(),
        key,
        asset: LoadedPetAsset::Cell(vec![vec![]; catalog::FRAME_COUNT]),
    });
    assets.clear_mismatched_pet(
        "codex",
        Some(PreparationKey {
            aspect: CellAspect::from_ratio(2.5).expect("valid aspect"),
            ..key
        }),
    );
    assert!(assets.loaded.is_none());
}

#[test]
fn pixel_preparation_key_ignores_cell_aspect() {
    assert_eq!(
        PreparationKey::new(
            PetRenderTier::Pixel,
            DASHBOARD_PIXEL_PET,
            CellAspect::NEUTRAL,
        ),
        PreparationKey::new(
            PetRenderTier::Pixel,
            DASHBOARD_PIXEL_PET,
            CellAspect::from_ratio(2.5).expect("valid aspect"),
        )
    );
}

#[test]
fn observe_unread_rows_triggers_only_on_new_rows() {
    let mut assets = PetAssets::default();

    assert!(assets.observe_unread_rows(["agent-1".to_owned(), "agent-2".to_owned()]));
    assert!(!assets.observe_unread_rows(["agent-1".to_owned(), "agent-2".to_owned()]));
    assert!(!assets.observe_unread_rows(["agent-2".to_owned()]));
    assert!(assets.observe_unread_rows(["agent-1".to_owned(), "agent-2".to_owned()]));
}

fn config_for(pet: &str) -> PetsConfig {
    PetsConfig {
        enabled: true,
        pet: pet.to_owned(),
        glyphs: PetsGlyphMode::Auto,
        cell_aspect: None,
        voice: true,
    }
}

fn enabled_config() -> PetsConfig {
    config_for("codex")
}

fn cell_key() -> PreparationKey {
    PreparationKey {
        tier: PetRenderTier::Cell,
        size: DASHBOARD_CELL_PET,
        aspect: CellAspect::NEUTRAL,
    }
}

fn loaded_cell_assets(previous_action: Option<PetAction>) -> PetAssets {
    PetAssets {
        loaded: Some(LoadedPet {
            id: "codex".to_owned(),
            key: cell_key(),
            asset: LoadedPetAsset::Cell(vec![vec![]; catalog::FRAME_COUNT]),
        }),
        previous_action,
        ..PetAssets::default()
    }
}

fn loaded_pixel_assets(previous_action: Option<PetAction>) -> PetAssets {
    let frame = RgbaImage {
        width: 1,
        height: 1,
        data: vec![255, 0, 0, 255],
    };
    PetAssets {
        loaded: Some(LoadedPet {
            id: "codex".to_owned(),
            key: PreparationKey {
                tier: PetRenderTier::Pixel,
                size: DASHBOARD_PIXEL_PET,
                aspect: CellAspect::NEUTRAL,
            },
            asset: LoadedPetAsset::Pixel(vec![frame; catalog::FRAME_COUNT]),
        }),
        previous_action,
        ..PetAssets::default()
    }
}
