use serde::{Deserialize, Serialize};

/// The default nudge an enabled auto-continue sends a parked agent — the text a
/// human would type to pick the turn back up.
pub const DEFAULT_AUTO_CONTINUE_TEXT: &str = "continue";

/// The default backoff ramp, in seconds, between non-clocked auto-continue
/// nudges. The first retry lands 3 minutes after the marker, then the 5-minute
/// gap repeats for every later retry. Override per machine; an empty ramp falls
/// back to a 300s gap.
pub const DEFAULT_AUTO_CONTINUE_BACKOFF_SECS: &[u64] = &[180, 300];

/// The default ceiling on backoff auto-continue attempts. At the [default ramp][`DEFAULT_AUTO_CONTINUE_BACKOFF_SECS`]
/// this spans ~58min (180 + 300x11) before the producer stops attempting
/// and leaves the row parked.
pub const DEFAULT_AUTO_CONTINUE_MAX_RETRIES: u32 = 12;

/// Resume behavior, in two tenses. On session *rebirth* after a machine reboot
/// — detected by boot id, or by boot time where the boot id is unreadable — or
/// after a mux crash with positive lost-agent markers, RimZ offers to re-seed
/// prior agents from the durable rollup; the prompt defaults to recovery, and
/// non-interactive starts recover. A host with no readable boot signal and no
/// lost-agent markers starts bare. A deliberate agent close — clean quit,
/// tab/pane close while the room remains healthy, or a whole-room close down to
/// `rimzd` while the machine stays up — keeps agents out of recovery. Empty
/// named channel tabs still reopen, and manual `rimz reset` starts fresh. While
/// the room is *live*, opt-in auto-continue picks a parked agent's turn back up
/// after a rate-limit window resets or a non-clocked retry backoff elapses.
/// Backend-neutral product behavior the cli and producer read directly, not a
/// multiplexer preference.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ResumeConfig {
    /// Offer to re-seed prior agents on session birth after a reboot or mux crash.
    /// The interactive prompt defaults yes, non-interactive starts recover, and
    /// `--no-resume` overrides it per-invocation for a deliberately fresh start.
    pub on_rebirth: bool,
    /// Ceiling on agents auto-resumed into one reborn session, bounding the
    /// processes a long-lived workspace launches at birth. Overflow is reported,
    /// never silently dropped.
    pub max: usize,
    /// Resume any live parked agent by typing
    /// [`auto_continue_text`](Self::auto_continue_text) into its pane: rate
    /// limits at the spent-window reset, overloads and transient server errors on
    /// the backoff ramp. Off by default: RimZ types into a pane on its own only
    /// once you opt in. Best-effort and traced through message records.
    pub auto_continue: bool,
    /// Retry ramp, in seconds, for non-clocked auto-continue. The last value
    /// repeats until [`auto_continue_max_retries`](Self::auto_continue_max_retries)
    /// is reached.
    pub auto_continue_backoff_secs: Vec<u64>,
    /// Number of auto-continue attempts before leaving the row paused.
    pub auto_continue_max_retries: u32,
    /// The text the producer nudges a parked agent with when `auto_continue` is
    /// on. Sent as a bracketed paste plus a submit Enter, the same pane-send path
    /// `message --steer` uses.
    pub auto_continue_text: String,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        Self {
            on_rebirth: true,
            max: crate::harness::resume::DEFAULT_RESUME_MAX,
            auto_continue: false,
            auto_continue_backoff_secs: DEFAULT_AUTO_CONTINUE_BACKOFF_SECS.to_vec(),
            auto_continue_max_retries: DEFAULT_AUTO_CONTINUE_MAX_RETRIES,
            auto_continue_text: DEFAULT_AUTO_CONTINUE_TEXT.to_owned(),
        }
    }
}
