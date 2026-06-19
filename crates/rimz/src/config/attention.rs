use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

/// `[agents.attention]`: timing knobs for the attention projection. The values
/// are per-machine display/routing preferences, never ledger truth.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AttentionConfig {
    /// Seconds a `running` agent may record no completed tool or turn activity
    /// before the sidebar projects it to the actionable `!` attention bucket.
    pub stalled_after_secs: NonZeroU32,
    /// Seconds a row may record no activity before the sidebar treats it as
    /// inactive and sinks it beneath every live row, whatever its status — one
    /// hour by default, the boundary the agent's own prompt cache crosses, so a
    /// card that has gone cold reads as cold.
    pub inactive_after_secs: NonZeroU32,
}

impl Default for AttentionConfig {
    fn default() -> Self {
        Self {
            stalled_after_secs: NonZeroU32::new(crate::feed::DEFAULT_STALL_AFTER_SECS)
                .expect("non-zero default stall window"),
            inactive_after_secs: NonZeroU32::new(crate::feed::DEFAULT_INACTIVE_AFTER_SECS)
                .expect("non-zero default inactive window"),
        }
    }
}
