use super::model::FleetPetStatus;

/// A canned line for the pet to "say" when the fused fleet status changes,
/// drawn from the matching pool by `seed` so repeated transitions vary. Returns
/// `None` while the status holds, so a line shows once per change and the prior
/// line keeps standing. `pool[0]` is the plain line; later entries add variety.
pub(crate) fn caption(
    previous: Option<FleetPetStatus>,
    current: FleetPetStatus,
    seed: u64,
) -> Option<&'static str> {
    if previous == Some(current) {
        return None;
    }
    let pool = match current {
        FleetPetStatus::NeedsInput => NEEDS_INPUT,
        FleetPetStatus::Blocked => BLOCKED,
        FleetPetStatus::Running => RUNNING,
        FleetPetStatus::Idle => match previous {
            Some(
                FleetPetStatus::NeedsInput | FleetPetStatus::Blocked | FleetPetStatus::Running,
            ) => CAUGHT_UP,
            _ => RESTING,
        },
    };
    Some(pool[(seed as usize) % pool.len()])
}

const NEEDS_INPUT: &[&str] = &[
    "someone needs you",
    "your turn",
    "tap in - you're up",
    "an agent's waiting",
    "psst, over here",
];

const BLOCKED: &[&str] = &[
    "rough patch - take a look",
    "something's stuck",
    "hit a snag",
    "we're wedged here",
    "this one needs a nudge",
];

const RUNNING: &[&str] = &[
    "room is moving",
    "all paws on deck",
    "things are humming",
    "work in flight",
    "cooking with gas",
];

const CAUGHT_UP: &[&str] = &[
    "all caught up",
    "inbox zero",
    "nothing left to chase",
    "clear skies",
    "good as done",
];

const RESTING: &[&str] = &[
    "resting",
    "taking a nap",
    "stretching out",
    "keeping watch",
    "just vibing",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captions_fire_on_status_transitions() {
        assert_eq!(
            caption(Some(FleetPetStatus::Running), FleetPetStatus::NeedsInput, 0),
            Some("someone needs you")
        );
        assert_eq!(
            caption(Some(FleetPetStatus::NeedsInput), FleetPetStatus::Idle, 0),
            Some("all caught up")
        );
        assert_eq!(
            caption(Some(FleetPetStatus::Idle), FleetPetStatus::Idle, 7),
            None
        );
    }

    #[test]
    fn seed_varies_the_line_within_a_pool() {
        let first = caption(None, FleetPetStatus::Running, 0);
        let second = caption(None, FleetPetStatus::Running, 1);
        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second, "different seeds pick different lines");
    }

    #[test]
    fn resting_is_the_cold_idle_line() {
        assert_eq!(caption(None, FleetPetStatus::Idle, 0), Some("resting"));
    }
}
