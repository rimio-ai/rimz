//! Harness wakes still owed to a supervised agent session.

/// What still owes an agent session a harness wake, as the run fold sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwedWake {
    Wait,
    WakeInFlight,
    Subagents,
}

impl OwedWake {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::WakeInFlight => "wake in flight",
            Self::Subagents => "subagents",
        }
    }
}
