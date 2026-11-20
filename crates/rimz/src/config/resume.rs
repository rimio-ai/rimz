use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The default nudge an enabled auto-continue sends a parked agent — the text a
/// human would type to pick the turn back up.
const DEFAULT_AUTO_CONTINUE_TEXT: &str = "continue";

/// The default backoff ramp, in seconds, between non-clocked auto-continue
/// nudges. The first retry lands 3 minutes after the marker, then the 5-minute
/// gap repeats for every later retry. Override per machine; an empty ramp falls
/// back to a 300s gap.
const DEFAULT_AUTO_CONTINUE_BACKOFF_SECS: &[u64] = &[180, 300];

/// The default ceiling on backoff auto-continue attempts. At the [default ramp][`DEFAULT_AUTO_CONTINUE_BACKOFF_SECS`]
/// this spans ~58min (180 + 300x11) before the producer stops attempting
/// and leaves the row parked.
const DEFAULT_AUTO_CONTINUE_MAX_RETRIES: u32 = 12;

/// Resume behavior, in two tenses. On session *rebirth* after a machine reboot
/// — detected by boot id, or by boot time where the boot id is unreadable — or
/// after a mux crash with positive lost-agent markers, RimZ offers to re-seed
/// prior agents from the durable rollup; the prompt defaults to recovery, and
/// non-interactive starts recover. A host with no readable boot signal and no
/// lost-agent markers starts bare. A deliberate agent close — clean quit,
/// tab/pane close while the room remains healthy, or a whole-room close down to
/// `rimzd` while the machine stays up — keeps agents out of recovery. A manual
/// `rimz reset` starts fresh. While
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
    /// the backoff ramp. Recovery of these parks is off by default, best-effort,
    /// and traced through message records. When on, it also launches Claude with
    /// its own usage-limit resume on, and Claude's rate limits wait a grace past
    /// the reset so that resume goes first.
    pub auto_continue: bool,
    /// Retry ramp, in seconds, for non-clocked auto-continue. The last value
    /// repeats until [`auto_continue_max_retries`](Self::auto_continue_max_retries)
    /// is reached.
    pub auto_continue_backoff_secs: Vec<u64>,
    /// Number of auto-continue attempts before leaving the row paused.
    pub auto_continue_max_retries: u32,
    /// Text for park recovery when `auto_continue` is on, and for the continue
    /// `rimz agents compact` owes a busy agent regardless of that switch. Blank
    /// text owes that compaction no continue and withholds the smart-compaction
    /// reminder. Sent through the message queue as a paste plus Enter.
    pub auto_continue_text: String,
}

/// The default ceiling on agents auto-resumed into one reborn session, so a
/// long-lived workspace cannot fork-bomb a fleet of agent processes on birth.
/// Anything past it is reported, never silently dropped.
const DEFAULT_RESUME_MAX: usize = 128;

impl Default for ResumeConfig {
    fn default() -> Self {
        Self {
            on_rebirth: true,
            max: DEFAULT_RESUME_MAX,
            auto_continue: false,
            auto_continue_backoff_secs: DEFAULT_AUTO_CONTINUE_BACKOFF_SECS.to_vec(),
            auto_continue_max_retries: DEFAULT_AUTO_CONTINUE_MAX_RETRIES,
            auto_continue_text: DEFAULT_AUTO_CONTINUE_TEXT.to_owned(),
        }
    }
}

impl ResumeConfig {
    pub(crate) fn auto_continue_backoff(&self, retries: u32) -> Duration {
        let backoff_secs = &self.auto_continue_backoff_secs;
        let idx = (retries as usize).min(backoff_secs.len().saturating_sub(1));
        let fallback = DEFAULT_AUTO_CONTINUE_BACKOFF_SECS
            .last()
            .copied()
            .unwrap_or(180);
        Duration::from_secs(backoff_secs.get(idx).copied().unwrap_or(fallback))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overload_backoff_expands_then_repeats_the_last_step() {
        let mut config = ResumeConfig {
            auto_continue_backoff_secs: vec![60, 120, 180],
            ..ResumeConfig::default()
        };
        assert_eq!(config.auto_continue_backoff(0).as_secs(), 60);
        assert_eq!(config.auto_continue_backoff(1).as_secs(), 120);
        assert_eq!(config.auto_continue_backoff(2).as_secs(), 180);
        assert_eq!(config.auto_continue_backoff(9).as_secs(), 180);
        config.auto_continue_backoff_secs.clear();
        assert_eq!(config.auto_continue_backoff(0).as_secs(), 300);
    }

    #[test]
    fn default_resume_ceiling_is_128() {
        assert_eq!(ResumeConfig::default().max, 128);
    }
}
