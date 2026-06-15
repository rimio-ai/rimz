use std::collections::BTreeMap;
use std::time::Duration;

const IDLE_FPS: f64 = 1.6;
const RUNNING_FPS: f64 = 8.0;
const WAITING_FPS: f64 = 6.5;
const FAILED_FPS: f64 = 7.0;

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
}

pub(crate) type AnimationSet = BTreeMap<&'static str, Animation>;

pub(crate) fn default_animations() -> AnimationSet {
    let row = |row: usize, count: usize| (0..count).map(|col| row * 8 + col).collect::<Vec<_>>();
    BTreeMap::from([
        ("idle", Animation::new(vec![0, 1, 2, 3, 4, 5], IDLE_FPS)),
        ("running", Animation::new(row(7, 6), RUNNING_FPS)),
        ("waiting", Animation::new(row(6, 6), WAITING_FPS)),
        ("failed", Animation::new(row(5, 8), FAILED_FPS)),
    ])
}

pub(crate) fn fleet_track(status: FleetPetStatus) -> &'static str {
    match status {
        FleetPetStatus::Idle => "idle",
        FleetPetStatus::Running => "running",
        FleetPetStatus::Blocked => "failed",
        FleetPetStatus::NeedsInput => "waiting",
    }
}

pub(crate) fn fleet_track_frame_duration(status: FleetPetStatus, refresh_ms: u16) -> Duration {
    let fps = match status {
        FleetPetStatus::Idle => IDLE_FPS,
        FleetPetStatus::Running => RUNNING_FPS,
        FleetPetStatus::Blocked => FAILED_FPS,
        FleetPetStatus::NeedsInput => WAITING_FPS,
    };
    let track_ms = (1000.0 / fps).round() as u64;
    Duration::from_millis(track_ms.max(u64::from(refresh_ms.max(1))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_tracks_follow_attention_precedence_names() {
        assert_eq!(fleet_track(FleetPetStatus::NeedsInput), "waiting");
        assert_eq!(fleet_track(FleetPetStatus::Blocked), "failed");
        assert_eq!(fleet_track(FleetPetStatus::Running), "running");
        assert_eq!(fleet_track(FleetPetStatus::Idle), "idle");
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
    fn fleet_track_frame_duration_uses_track_cadence() {
        assert_eq!(
            fleet_track_frame_duration(FleetPetStatus::Idle, 100),
            Duration::from_millis(625)
        );
        assert_eq!(
            fleet_track_frame_duration(FleetPetStatus::Running, 100),
            Duration::from_millis(125)
        );
        assert_eq!(
            fleet_track_frame_duration(FleetPetStatus::Running, 500),
            Duration::from_millis(500)
        );
    }
}
