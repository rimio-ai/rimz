//! Shared opt-in policy for a once-per-visit stalled-team nudge.

use crate::config::MachineConfig;

const NUDGE_AFTER_SECS: i64 = 55 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Off,
    On,
    Shadowed,
}

pub(crate) fn decision(config: &MachineConfig) -> Decision {
    if !config.harness.stall_nudge {
        return Decision::Off;
    }
    if i64::from(config.agents.attention.stalled_after_secs.get()) >= NUDGE_AFTER_SECS {
        return Decision::Shadowed;
    }
    Decision::On
}

#[cfg(test)]
mod tests;
