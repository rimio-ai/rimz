use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

/// Default `[agents.attention] inactive_after_secs`: a row with no activity for
/// this long sinks into the inactive partition, beneath every live row.
const DEFAULT_INACTIVE_AFTER_SECS: u32 = crate::agents::ATTENTION_AGE_CEILING_SECS as u32;

/// Default `[agents.attention] archive_after_secs`: a row with no activity for
/// this long stops competing with hot or warm work and parks in the archive
/// partition.
const DEFAULT_ARCHIVE_AFTER_SECS: u32 = 24 * 60 * 60;

/// Default window before a `running` agent with no activity is treated as
/// stalled. The per-machine `[agents.attention] stalled_after_secs` setting
/// overrides this for the live sidebar projection.
const DEFAULT_STALL_AFTER_SECS: u32 = 30 * 60;

/// Consecutive identical tool calls before the sidebar annotates a card.
const DEFAULT_TOOL_REPEAT_WARN_AFTER: u32 = 3;

/// Consecutive identical tool calls before the sidebar routes attention.
const DEFAULT_TOOL_REPEAT_ATTENTION_AFTER: u32 = 20;

/// Default silence window credited to a working span before estimated active
/// time pauses. The next progress signal resumes accrual without counting the
/// intervening idle gap.
const DEFAULT_ACTIVE_GRACE_SECS: u32 = 3 * 60;

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
    /// Consecutive identical tool calls before the sidebar annotates the card.
    pub tool_repeat_warn_after: NonZeroU32,
    /// Consecutive identical tool calls before the sidebar routes attention.
    pub tool_repeat_attention_after: NonZeroU32,
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
            active_grace_secs: NonZeroU32::new(DEFAULT_ACTIVE_GRACE_SECS)
                .expect("non-zero default active-time grace"),
            stalled_after_secs: NonZeroU32::new(DEFAULT_STALL_AFTER_SECS)
                .expect("non-zero default stall window"),
            tool_repeat_warn_after: NonZeroU32::new(DEFAULT_TOOL_REPEAT_WARN_AFTER)
                .expect("non-zero default tool-repeat warning threshold"),
            tool_repeat_attention_after: NonZeroU32::new(DEFAULT_TOOL_REPEAT_ATTENTION_AFTER)
                .expect("non-zero default tool-repeat attention threshold"),
            inactive_after_secs: NonZeroU32::new(DEFAULT_INACTIVE_AFTER_SECS)
                .expect("non-zero default inactive window"),
            archive_after_secs: NonZeroU32::new(DEFAULT_ARCHIVE_AFTER_SECS)
                .expect("non-zero default archive window"),
        }
    }
}
