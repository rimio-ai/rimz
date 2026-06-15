use serde::{Deserialize, Serialize};

/// The default nudge an enabled auto-continue sends a rate-limit-parked agent —
/// the text a human would type to pick the turn back up.
pub const DEFAULT_AUTO_CONTINUE_TEXT: &str = "continue";

/// Resume behavior, in two tenses. On a session *rebirth* — reboot, multiplexer
/// crash, or a Rimz-initiated rebirth of a stuck room — Rimz re-seeds the prior
/// agents from the durable rollup so the room comes up where the user left off
/// instead of empty. While the room is *live*, opt-in auto-continue picks a
/// rate-limit-parked agent's turn back up the moment its 5h/7d window resets.
/// Backend-neutral product behavior the cli and producer read directly, not a
/// multiplexer preference.
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
    /// Resume a rate-limit-parked agent the moment its spent 5h/7d window resets,
    /// by typing [`auto_continue_text`](Self::auto_continue_text) into its live
    /// pane. Off by default: Rimz types into a pane on its own only once you opt
    /// in. Best-effort and audited (`agent.resumed`); `overloaded` parks recover
    /// on a provider retry and are left alone.
    pub auto_continue: bool,
    /// The text the producer nudges a parked agent with when `auto_continue` is
    /// on. Sent as a bracketed paste plus a submit Enter, the same pane-send path
    /// `steer` uses.
    pub auto_continue_text: String,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        Self {
            on_rebirth: true,
            max: crate::resume::DEFAULT_RESUME_MAX,
            auto_continue: false,
            auto_continue_text: DEFAULT_AUTO_CONTINUE_TEXT.to_owned(),
        }
    }
}
