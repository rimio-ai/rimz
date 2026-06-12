//! Compatibility facade for the sidebar snapshot data plane.
//!
//! New code should prefer the responsibility-named modules:
//! [`super::cache`] for runtime cache formats and TTLs, [`super::consumer`] for
//! read-only published snapshots, and [`super::enrich`] for the shared view-model
//! fold. This module keeps the existing `sidebar::snapshot::*` path stable for
//! CLI callers, tests, and downstream renderer projections.

pub use super::cache::{
    ACCOUNTS_RETRY_TTL, ACCOUNTS_TTL, AccountsCache, DIFF_STATS_IDLE_TTL, DIFF_STATS_TTL,
    DiffStats, DiffStatsCache, DiffStatsCacheEntry, EVENT_PANE_TTL, GIT_ACTIVITY_WINDOW,
    PRESENCE_STAMP_FRESH, PresenceStamp, SNAPSHOT_CACHE_TTL, WORKTREE_ROOTS_TTL,
    WorktreeRootsCache, effective_pane_ttl, presence_event_mode, presence_stamp_age_ms,
    presence_stamp_path, published_frame_age_ms, published_frame_observed_at_ms,
    published_frame_produced_at_ms, read_diff_stats_cache, read_snapshot_cache,
    snapshot_cache_is_fresh, unix_now_ms, write_presence_stamp,
};
pub use super::consumer::{RollupCursor, read_published_snapshot, rollup_snapshot};
pub use super::enrich::{
    EnrichMode, RateLimitsCache, apply_live_today_spend, cached_worktree_roots, enrich,
    fold_link_stats, hot_worktree_paths, live_row_costs, merge_account_rate_limits,
    needed_worktree_paths, project_diff_stats, wired_lazy_default_models, wired_lazy_kinds,
};
pub use super::frame::{
    CarriedPane, PaneFrame, PaneMetrics, PaneProcess, PaneState, TabFrame, assemble_frame,
};

#[cfg(test)]
pub(crate) use super::enrich::{
    CODEX_RATE_LIMIT_REFRESH_INTERVAL, CodexRateLimitRefresh, accounts_cache_version_refresh_due,
    apply_rate_limit_cache, codex_rate_limit_probe_due, codex_rate_limit_probe_marker,
    codex_rate_limit_refreshes, project_idle_window, provider_has_out_of_band_windows,
    read_rate_limits_cache, refresh_codex_transcript_context, stamp_context_severity,
    write_rate_limits_cache,
};
#[cfg(test)]
use crate::agents::{AgentRateLimits, RateLimitWindow};
#[cfg(test)]
use crate::feed::PaneRef;
#[cfg(test)]
use crate::ids::PaneId;
#[cfg(test)]
use crate::{RuntimePaths, SidebarSnapshot, SidebarWorktreeKind, StatePaths};
#[cfg(test)]
use jiff::{SignedDuration, Timestamp};
#[cfg(test)]
use std::collections::BTreeMap;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::time::{Duration, SystemTime};

#[cfg(test)]
mod tests;
