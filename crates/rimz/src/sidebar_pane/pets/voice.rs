//! Canned pet captions keyed by attention-action transitions.

use super::model::PetAction;

/// A canned line for the pet to "say" when the selected-card action changes,
/// drawn from the matching pool by `seed` so repeated transitions vary. Returns
/// `None` while the action holds, so a line shows once per change and the prior
/// line keeps standing. `pool[0]` is the plain line; later entries add variety.
pub(crate) fn caption(
    previous: Option<PetAction>,
    current: PetAction,
    seed: u64,
) -> Option<&'static str> {
    if previous == Some(current) {
        return None;
    }
    let pool = match current {
        PetAction::Ask => ASK,
        PetAction::Failed => FAILED,
        PetAction::Thinking => THINKING,
        PetAction::Running => RUNNING,
        PetAction::Waiting => WAITING,
        PetAction::Review => REVIEW,
        PetAction::Idle => match previous {
            Some(
                PetAction::Ask
                | PetAction::Failed
                | PetAction::Thinking
                | PetAction::Running
                | PetAction::Waiting
                | PetAction::Review,
            ) => CAUGHT_UP,
            _ => RESTING,
        },
    };
    Some(pool[(seed as usize) % pool.len()])
}

const ASK: &[&str] = &[
    "someone needs you",
    "your turn",
    "tap in - you're up",
    "an agent's waiting",
    "psst, over here",
];

const FAILED: &[&str] = &[
    "rough patch - take a look",
    "something's stuck",
    "hit a snag",
    "we're wedged here",
    "this one needs a nudge",
];

const THINKING: &[&str] = &[
    "thinking it through",
    "reading the room",
    "working it out",
    "still reasoning",
    "mapping the path",
];

const RUNNING: &[&str] = &[
    "room is moving",
    "things are humming",
    "work in flight",
    "cooking with gas",
    "hands are busy",
];

const WAITING: &[&str] = &[
    "waiting on work",
    "background work",
    "standing by",
    "holding the line",
    "watching the lane",
];

const REVIEW: &[&str] = &[
    "reviewing context",
    "condensing context",
    "tidying memory",
    "making room",
    "compressing notes",
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
    fn captions_fire_on_action_transitions() {
        assert_eq!(
            caption(Some(PetAction::Running), PetAction::Ask, 0),
            Some("someone needs you")
        );
        assert_eq!(
            caption(Some(PetAction::Ask), PetAction::Idle, 0),
            Some("all caught up")
        );
        assert_eq!(caption(Some(PetAction::Idle), PetAction::Idle, 7), None);
    }

    #[test]
    fn seed_varies_the_line_within_a_pool() {
        let first = caption(None, PetAction::Running, 0);
        let second = caption(None, PetAction::Running, 1);
        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second, "different seeds pick different lines");
    }

    #[test]
    fn resting_is_the_cold_idle_line() {
        assert_eq!(caption(None, PetAction::Idle, 0), Some("resting"));
    }
}
