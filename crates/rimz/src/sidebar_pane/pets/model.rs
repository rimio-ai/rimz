use std::collections::BTreeMap;
use std::time::Duration;

const IDLE_FPS: f64 = 1.6;
const MOVING_FPS: f64 = 4.0;
const WAVING_FPS: f64 = 3.5;
const JUMPING_FPS: f64 = 3.5;
const FAILED_FPS: f64 = 3.5;
const WAITING_FPS: f64 = 3.5;
const REVIEW_FPS: f64 = 3.5;

pub(crate) const TRACK_IDLE: &str = "idle";
pub(crate) const TRACK_MOVING: &str = "moving";
pub(crate) const TRACK_WAVING: &str = "waving";
pub(crate) const TRACK_JUMPING: &str = "jumping";
pub(crate) const TRACK_FAILED: &str = "failed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FleetPetStatus {
    Idle,
    Running,
    Blocked,
    NeedsInput,
}

#[derive(Clone, Debug)]
pub(crate) struct Animation {
    sprites: Vec<usize>,
    fps: f64,
}

impl Animation {
    fn new(sprites: Vec<usize>, fps: f64) -> Self {
        Self { sprites, fps }
    }

    pub(crate) fn sprite_index(&self, phase: u64, refresh_ms: u16) -> usize {
        if self.sprites.is_empty() {
            return 0;
        }
        let elapsed = phase as f64 * f64::from(refresh_ms) / 1000.0;
        let position = (elapsed * self.fps).floor() as usize % self.sprites.len();
        self.sprites[position]
    }

    pub(crate) fn first_sprite(&self) -> usize {
        self.sprites.first().copied().unwrap_or(0)
    }

    pub(crate) fn loop_duration(&self, refresh_ms: u16) -> Duration {
        let frame_ms = frame_duration_for_fps(self.fps, refresh_ms).as_millis() as u64;
        Duration::from_millis(frame_ms.saturating_mul(self.sprites.len() as u64))
    }
}

pub(crate) type AnimationSet = BTreeMap<&'static str, Animation>;

pub(crate) fn default_animations() -> AnimationSet {
    let row = |row: usize, count: usize| (0..count).map(|col| row * 8 + col).collect::<Vec<_>>();
    let run_right = row(1, 8);
    let run_left = row(2, 8);
    let mut moving = run_right.clone();
    moving.extend(run_left.iter().copied());

    BTreeMap::from([
        (TRACK_IDLE, Animation::new(row(0, 6), IDLE_FPS)),
        ("run-right", Animation::new(run_right, MOVING_FPS)),
        ("run-left", Animation::new(run_left, MOVING_FPS)),
        (TRACK_WAVING, Animation::new(row(3, 4), WAVING_FPS)),
        (TRACK_JUMPING, Animation::new(row(4, 5), JUMPING_FPS)),
        (TRACK_FAILED, Animation::new(row(5, 8), FAILED_FPS)),
        ("waiting", Animation::new(row(6, 6), WAITING_FPS)),
        ("running", Animation::new(row(7, 6), MOVING_FPS)),
        ("review", Animation::new(row(8, 6), REVIEW_FPS)),
        (TRACK_MOVING, Animation::new(moving, MOVING_FPS)),
    ])
}

pub(crate) fn fleet_track(status: FleetPetStatus) -> &'static str {
    match status {
        FleetPetStatus::Idle => TRACK_IDLE,
        FleetPetStatus::Running => TRACK_MOVING,
        FleetPetStatus::Blocked => TRACK_FAILED,
        FleetPetStatus::NeedsInput => TRACK_WAVING,
    }
}

pub(crate) fn one_shot_track(
    previous: Option<FleetPetStatus>,
    current: FleetPetStatus,
) -> Option<&'static str> {
    match (previous, current) {
        (
            Some(FleetPetStatus::NeedsInput | FleetPetStatus::Blocked | FleetPetStatus::Running),
            FleetPetStatus::Idle,
        ) => Some(TRACK_JUMPING),
        _ => None,
    }
}

pub(crate) fn track_frame_duration(track: &str, refresh_ms: u16) -> Duration {
    let fps = match track {
        TRACK_IDLE => IDLE_FPS,
        "run-right" | "run-left" | TRACK_MOVING | "running" => MOVING_FPS,
        TRACK_WAVING => WAVING_FPS,
        TRACK_JUMPING => JUMPING_FPS,
        TRACK_FAILED => FAILED_FPS,
        "waiting" => WAITING_FPS,
        "review" => REVIEW_FPS,
        _ => IDLE_FPS,
    };
    frame_duration_for_fps(fps, refresh_ms)
}

fn frame_duration_for_fps(fps: f64, refresh_ms: u16) -> Duration {
    let frame_ms = (1000.0 / fps).round() as u64;
    Duration::from_millis(frame_ms.max(u64::from(refresh_ms.max(1))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_tracks_follow_attention_precedence_names() {
        assert_eq!(fleet_track(FleetPetStatus::NeedsInput), TRACK_WAVING);
        assert_eq!(fleet_track(FleetPetStatus::Blocked), TRACK_FAILED);
        assert_eq!(fleet_track(FleetPetStatus::Running), TRACK_MOVING);
        assert_eq!(fleet_track(FleetPetStatus::Idle), TRACK_IDLE);
    }

    #[test]
    fn default_catalog_matches_petdex_rows() {
        let animations = default_animations();
        assert_eq!(
            animations[TRACK_MOVING].sprites,
            (8..24).collect::<Vec<_>>()
        );
        assert_eq!(animations[TRACK_WAVING].sprites, vec![24, 25, 26, 27]);
        assert_eq!(animations[TRACK_JUMPING].sprites, vec![32, 33, 34, 35, 36]);
        assert_eq!(
            animations[TRACK_FAILED].sprites,
            (40..48).collect::<Vec<_>>()
        );
        assert_eq!(animations["waiting"].sprites, (48..54).collect::<Vec<_>>());
        assert_eq!(animations["running"].sprites, (56..62).collect::<Vec<_>>());
        assert_eq!(animations["review"].sprites, (64..70).collect::<Vec<_>>());
    }

    #[test]
    fn completion_jump_only_fires_on_work_to_idle_edges() {
        assert_eq!(
            one_shot_track(Some(FleetPetStatus::Running), FleetPetStatus::Idle),
            Some(TRACK_JUMPING)
        );
        assert_eq!(
            one_shot_track(Some(FleetPetStatus::NeedsInput), FleetPetStatus::Idle),
            Some(TRACK_JUMPING)
        );
        assert_eq!(
            one_shot_track(Some(FleetPetStatus::Blocked), FleetPetStatus::Idle),
            Some(TRACK_JUMPING)
        );
        assert_eq!(one_shot_track(None, FleetPetStatus::Idle), None);
        assert_eq!(
            one_shot_track(Some(FleetPetStatus::Idle), FleetPetStatus::Idle),
            None
        );
        assert_eq!(
            one_shot_track(Some(FleetPetStatus::Running), FleetPetStatus::Blocked),
            None
        );
    }

    #[test]
    fn animation_samples_from_phase_and_refresh_ms() {
        let animation = Animation::new(vec![10, 11, 12], 10.0);
        assert_eq!(animation.sprite_index(0, 100), 10);
        assert_eq!(animation.sprite_index(1, 100), 11);
        assert_eq!(animation.sprite_index(2, 100), 12);
        assert_eq!(animation.sprite_index(3, 100), 10);
        assert_eq!(animation.first_sprite(), 10);
    }

    #[test]
    fn track_frame_duration_uses_track_cadence() {
        assert_eq!(
            track_frame_duration(TRACK_IDLE, 100),
            Duration::from_millis(625)
        );
        assert_eq!(
            track_frame_duration(TRACK_MOVING, 100),
            Duration::from_millis(250)
        );
        assert_eq!(
            track_frame_duration(TRACK_JUMPING, 100),
            Duration::from_millis(286)
        );
        assert_eq!(
            track_frame_duration(TRACK_MOVING, 500),
            Duration::from_millis(500)
        );
    }
}
