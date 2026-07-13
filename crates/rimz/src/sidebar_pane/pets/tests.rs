use super::*;
use crate::config::{PetsGlyphMode, PixelMode};

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

#[test]
fn render_tier_resolves_mode_caps_and_paintability() {
    use PetsGlyphMode::{Auto, Pixel, Sextant};
    let caps = |pixel_transport, kitty_term| PixelRenderCaps {
        pixel_transport,
        kitty_term,
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
                    Some(PetRenderTier::Cell),
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
        cell_aspect: None,
        voice: true,
    };
    let view = assets
        .view(
            &config,
            frame(
                PetAction::Idle,
                0,
                100,
                Some(PetRenderTier::Cell),
                true,
                false,
            ),
        )
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
    let config = PetsConfig {
        enabled: true,
        pet: "/no/such/pet/sheet.webp".to_owned(),
        glyphs: PetsGlyphMode::Auto,
        cell_aspect: None,
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
                Some(PetRenderTier::Cell),
                true,
                false,
            ),
        )
        .expect("enabled pets produce a view");
    assert_eq!(view.body, None);
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
        cell_aspect: None,
        voice: true,
    };

    let view = assets
        .view(&config, frame(PetAction::Idle, 0, 100, None, true, false))
        .expect("enabled pets produce a view");

    assert_eq!(view.body, None);
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
        cell_aspect: None,
        voice: true,
    };

    let view = assets
        .view(
            &config,
            frame(
                PetAction::Idle,
                0,
                100,
                Some(PetRenderTier::Cell),
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
                Some(PetRenderTier::Cell),
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
fn jump_plays_once_per_trigger_then_settles() {
    let refresh_ms = 100;
    let config = enabled_config();
    let body = Some(PetRenderTier::Cell);
    for (previous, action, unread_triggered, steady_track) in [
        (PetAction::Running, PetAction::Ask, false, model::TRACK_ASK),
        (
            PetAction::Running,
            PetAction::Running,
            true,
            model::TRACK_RUNNING,
        ),
    ] {
        let mut assets = loaded_assets(Some(previous));

        let view = assets
            .view(
                &config,
                frame(action, 10, refresh_ms, body, true, unread_triggered),
            )
            .expect("enabled pets produce a view");
        assert_eq!(view.active_track, model::TRACK_JUMPING);
        assert_eq!(assets.jump_started_phase, Some(10));

        let jump = model::animations()
            .get(model::TRACK_JUMPING)
            .expect("jumping track");
        let phases = jump
            .loop_duration(refresh_ms)
            .as_millis()
            .div_ceil(u128::from(refresh_ms));
        let view = assets
            .view(
                &config,
                frame(action, 10 + phases as u64, refresh_ms, body, true, false),
            )
            .expect("enabled pets produce a view");
        assert_eq!(view.active_track, steady_track);
        assert_eq!(assets.jump_started_phase, None);
    }
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
                Some(PetRenderTier::Cell),
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

    let view = assets
        .view(
            &config,
            PetViewFrame {
                action: PetAction::Idle,
                phase: 0,
                refresh_ms: 100,
                body: Some(PetRenderTier::Pixel),
                pixel_id_base: 0x120000,
                cell_aspect: CellAspect::NEUTRAL,
                motion_enabled: true,
                unread_triggered: false,
            },
        )
        .expect("enabled pets produce a view");

    let PetBody::Pixel(pixel) = view.body.expect("pixel view") else {
        panic!("expected pixel body");
    };
    assert_eq!(pixel.pet_id, "codex");
    assert_eq!(pixel.size, DASHBOARD_PIXEL_PET);
    assert_eq!(pixel.image_id, 0x120000 + pixel.sprite_index as u32);
    assert_eq!(view.active_track, model::TRACK_JUMPING);
    assert!(
        assets
            .pixel_frame(&pixel.pet_id, pixel.sprite_index)
            .is_some()
    );
    assert!(assets.loaded.as_ref().expect("loaded").memo.is_empty());
}

#[test]
fn memoized_grids_are_evicted_on_size_or_aspect_change() {
    let frame = RgbaImage {
        width: 1,
        height: 1,
        data: vec![255, 0, 0, 255],
    };
    let mut assets = PetAssets {
        loaded: Some(LoadedPet {
            id: "codex".to_owned(),
            frames: vec![frame; catalog::FRAME_COUNT],
            memo: HashMap::new(),
        }),
        ..PetAssets::default()
    };

    assert!(
        assets
            .loaded_grid(
                "codex",
                0,
                PetGridSize { cols: 12, rows: 6 },
                CellAspect::NEUTRAL,
            )
            .is_some()
    );
    assert_eq!(assets.loaded.as_ref().expect("loaded").memo.len(), 1);

    assert!(
        assets
            .loaded_grid(
                "codex",
                0,
                PetGridSize { cols: 13, rows: 6 },
                CellAspect::NEUTRAL,
            )
            .is_some()
    );
    let memo = &assets.loaded.as_ref().expect("loaded").memo;
    assert_eq!(memo.len(), 1);
    assert!(memo.keys().all(|key| key.cols == 13));

    let changed = CellAspect::from_ratio(2.5).expect("valid aspect");
    assert!(
        assets
            .loaded_grid("codex", 0, PetGridSize { cols: 13, rows: 6 }, changed)
            .is_some()
    );
    let memo = &assets.loaded.as_ref().expect("loaded").memo;
    assert_eq!(memo.len(), 1);
    assert!(memo.keys().all(|key| key.aspect == changed));
}

#[test]
fn observe_unread_rows_triggers_only_on_new_rows() {
    let mut assets = PetAssets::default();

    assert!(assets.observe_unread_rows(["agent-1".to_owned(), "agent-2".to_owned()]));
    assert!(!assets.observe_unread_rows(["agent-1".to_owned(), "agent-2".to_owned()]));
    assert!(!assets.observe_unread_rows(["agent-2".to_owned()]));
    assert!(assets.observe_unread_rows(["agent-1".to_owned(), "agent-2".to_owned()]));
}

fn enabled_config() -> PetsConfig {
    PetsConfig {
        enabled: true,
        pet: "codex".to_owned(),
        glyphs: PetsGlyphMode::Auto,
        cell_aspect: None,
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
            memo: HashMap::new(),
        }),
        previous_action,
        ..PetAssets::default()
    }
}
