use serde::{Deserialize, Serialize};

/// Resume-on-rebirth behavior. When a session is reborn — reboot, multiplexer
/// crash, or a Rimz-initiated rebirth of a stuck room — Rimz re-seeds the prior
/// agents from the durable rollup so the room comes up where the user left off
/// instead of empty. Backend-neutral product behavior the cli reads directly,
/// not a multiplexer preference.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ResumeConfig {
    /// Re-seed prior agents on any session birth. `--no-resume` overrides it
    /// per-invocation for a deliberately fresh start.
    pub on_rebirth: bool,
    /// Ceiling on agents auto-resumed into one reborn session, bounding the
    /// processes a long-lived workspace launches at birth. Overflow is reported,
    /// never silently dropped.
    pub max: usize,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        Self {
            on_rebirth: true,
            max: crate::resume::DEFAULT_RESUME_MAX,
        }
    }
}
