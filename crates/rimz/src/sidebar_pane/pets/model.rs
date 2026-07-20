//! Maps sidebar attention states to pet actions, tracks, and animation timing.

use std::sync::LazyLock;
use std::time::Duration;

const IDLE_FPS: f64 = 1.6;
const MOVING_FPS: f64 = 4.0;
const WAVING_FPS: f64 = 3.5;
const JUMPING_FPS: f64 = 3.5;
const FAILED_FPS: f64 = 3.5;
const WAITING_FPS: f64 = 3.5;
const REVIEW_FPS: f64 = 3.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PetTrack {
    Idle,
    Thinking,
    Running,
    Waiting,
    Review,
    Ask,
    Jumping,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PetAction {
    Idle,
    Thinking,
    Running,
    Waiting,
    Review,
    Ask,
    Failed,
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
        let frame_ms = self.frame_duration(refresh_ms).as_millis() as u64;
        Duration::from_millis(frame_ms.saturating_mul(self.sprites.len() as u64))
    }

    pub(crate) fn frame_duration(&self, refresh_ms: u16) -> Duration {
        frame_duration_for_fps(self.fps, refresh_ms)
    }
}

pub(crate) struct AnimationSet {
    tracks: [Animation; 8],
}

impl AnimationSet {
    pub(crate) fn get(&self, track: PetTrack) -> &Animation {
        &self.tracks[track as usize]
    }
}

impl std::ops::Index<PetTrack> for AnimationSet {
    type Output = Animation;

    fn index(&self, track: PetTrack) -> &Self::Output {
        self.get(track)
    }
}

static ANIMATIONS: LazyLock<AnimationSet> = LazyLock::new(|| {
    let row = |row: usize, count: usize| (0..count).map(|col| row * 8 + col).collect::<Vec<_>>();
    let run_right = row(1, 8);
    let run_left = row(2, 8);
    let mut thinking = Vec::with_capacity((run_left.len() + run_right.len()) * 3);
    for _ in 0..3 {
        thinking.extend(run_left.iter().copied());
    }
    for _ in 0..3 {
        thinking.extend(run_right.iter().copied());
    }
    let waiting = row(6, 6);
    let mut ask = Vec::with_capacity(4 * 2 + waiting.len());
    ask.extend(row(3, 4));
    ask.extend(row(3, 4));
    ask.extend(waiting.iter().copied());

    AnimationSet {
        tracks: [
            Animation::new(row(0, 6), IDLE_FPS),
            Animation::new(thinking, MOVING_FPS),
            Animation::new(row(7, 6), MOVING_FPS),
            Animation::new(waiting, WAITING_FPS),
            Animation::new(row(8, 6), REVIEW_FPS),
            Animation::new(ask, WAVING_FPS),
            Animation::new(row(4, 5), JUMPING_FPS),
            Animation::new(row(5, 8), FAILED_FPS),
        ],
    }
});

pub(crate) fn animations() -> &'static AnimationSet {
    &ANIMATIONS
}

pub(crate) fn action_track(action: PetAction) -> PetTrack {
    match action {
        PetAction::Idle => PetTrack::Idle,
        PetAction::Thinking => PetTrack::Thinking,
        PetAction::Running => PetTrack::Running,
        PetAction::Waiting => PetTrack::Waiting,
        PetAction::Review => PetTrack::Review,
        PetAction::Ask => PetTrack::Ask,
        PetAction::Failed => PetTrack::Failed,
    }
}

pub(crate) fn action_changed(previous: Option<PetAction>, current: PetAction) -> bool {
    previous.is_some_and(|previous| previous != current)
}

pub(crate) fn track_frame_duration(track: PetTrack, refresh_ms: u16) -> Duration {
    animations().get(track).frame_duration(refresh_ms)
}

fn frame_duration_for_fps(fps: f64, refresh_ms: u16) -> Duration {
    let frame_ms = (1000.0 / fps).round() as u64;
    Duration::from_millis(frame_ms.max(u64::from(refresh_ms.max(1))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each track draws from its own band of the 8-wide petdex sheet. Wiring a
    /// track to the wrong row plays the wrong animation, so the expected
    /// sprites are spelled out against the sheet layout rather than recomputed
    /// with the same `row()` helper the catalog builds them with.
    #[test]
    fn default_catalog_matches_petdex_rows() {
        let animations = animations();
        for (track, expected) in [
            (PetTrack::Idle, (0..6).collect::<Vec<_>>()),
            (
                PetTrack::Thinking,
                (16..24)
                    .cycle()
                    .take(24)
                    .chain((8..16).cycle().take(24))
                    .collect(),
            ),
            (PetTrack::Running, (56..62).collect()),
            (PetTrack::Waiting, (48..54).collect()),
            (PetTrack::Review, (64..70).collect()),
            (
                PetTrack::Ask,
                (24..28).chain(24..28).chain(48..54).collect(),
            ),
            (PetTrack::Jumping, (32..37).collect()),
            (PetTrack::Failed, (40..48).collect()),
        ] {
            assert_eq!(animations[track].sprites, expected, "{track:?} sheet row");
        }
    }

    #[test]
    fn action_change_ignores_cold_start_and_same_action() {
        assert!(!action_changed(None, PetAction::Idle));
        assert!(!action_changed(Some(PetAction::Idle), PetAction::Idle));
        assert!(action_changed(Some(PetAction::Running), PetAction::Ask));
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
            track_frame_duration(PetTrack::Idle, 100),
            Duration::from_millis(625)
        );
        assert_eq!(
            track_frame_duration(PetTrack::Thinking, 100),
            Duration::from_millis(250)
        );
        assert_eq!(
            track_frame_duration(PetTrack::Jumping, 100),
            Duration::from_millis(286)
        );
        assert_eq!(
            track_frame_duration(PetTrack::Thinking, 500),
            Duration::from_millis(500)
        );
    }
}
