use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

/// `[agents.attention]`: timing knobs for the attention projection. The values
/// are per-machine display/routing preferences, never store truth.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AttentionConfig {
    /// Seconds of silence an active-time span may accrue before it pauses.
    /// This bounds the work estimate independently of attention escalation.
    pub active_grace_secs: NonZeroU32,
    /// Seconds a `running` agent may record no completed tool or turn activity
    /// before the sidebar projects it to the actionable `!` attention bucket.
    pub stalled_after_secs: NonZeroU32,
    /// Seconds a row may record no activity before the sidebar treats it as
    /// inactive and sinks it beneath every live row, whatever its status — one
    /// hour by default, the boundary the agent's own prompt cache crosses, so a
    /// card that has gone cold reads as cold.
    pub inactive_after_secs: NonZeroU32,
    /// Seconds a row may record no activity before the sidebar parks it in the
    /// archive partition, below hot and warm work. Values at or below
    /// `inactive_after_secs` are lifted at projection time because this is a
    /// display preference, not a store invariant.
    pub archive_after_secs: NonZeroU32,
}

impl Default for AttentionConfig {
    fn default() -> Self {
        Self {
            active_grace_secs: NonZeroU32::new(crate::agents::DEFAULT_ACTIVE_GRACE_SECS)
                .expect("non-zero default active-time grace"),
            stalled_after_secs: NonZeroU32::new(crate::agents::DEFAULT_STALL_AFTER_SECS)
                .expect("non-zero default stall window"),
            inactive_after_secs: NonZeroU32::new(crate::agents::DEFAULT_INACTIVE_AFTER_SECS)
                .expect("non-zero default inactive window"),
            archive_after_secs: NonZeroU32::new(crate::agents::DEFAULT_ARCHIVE_AFTER_SECS)
                .expect("non-zero default archive window"),
        }
    }
}
