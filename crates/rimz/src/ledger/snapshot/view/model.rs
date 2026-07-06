use serde::{Deserialize, Serialize};

use crate::agents::AgentStatus;
use crate::agents::{ExtraCredits, RateLimitWindow, ResetCredits, SpendTally};
use crate::config::PaletteRole;
use crate::ledger::snapshot::row::SidebarRow;
use crate::remote::link::LinkTier;

/// One provider's aggregate dashboard block, pinned to the bottom of the
/// sidebar. Account-scoped: every session of one agent kind folds into one
/// block — summed spend and tokens, plus the freshest session's plan, version,
/// and rate-limit windows — so the budgets render once per account, never per
/// row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarProviderPanel {
    pub kind: String,
    /// Header display name (`Claude`, `Codex`, …).
    pub product_name: String,
    /// Multi-line ASCII emblem, painted brand-colored at the block's left.
    pub art: Vec<String>,
    /// 256-color index for the emblem.
    pub color: u8,
    /// Truecolor brand tone for renderers using RGB depth. Older snapshots may
    /// omit it; renderers fall back to `color`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_rgb: Option<(u8, u8, u8)>,
    /// Palette role for brand tones linked to the active theme palette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_role: Option<PaletteRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Brand plan label (`Claude Max`, `ChatGPT Pro`); `None` when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Whether the account is metered by rate-limit windows.
    pub metered: bool,
    /// Whether remote control is enabled for this provider (the `⇅ rc` flag).
    pub remote_control: bool,
    /// JSONL-computed headline / week / month / trailing-year spend and tokens
    /// for this provider, summed across all of its sessions' transcript history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spending: Option<SpendTally>,
    /// Paid usage beyond subscription windows: provider extra credits or
    /// API-key spend against an optional display ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_credits: Option<ExtraCredits>,
    /// Codex rate-limit reset credits, shown as a compact header marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<ResetCredits>,
    /// The account-scoped budget windows, ordered short→long by duration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<RateLimitWindow>,
}

impl SidebarProviderPanel {
    /// The figure the dashboard ranks panels by: headline JSONL spend, so the
    /// provider you are spending on in the configured window floats to the top.
    pub(super) fn rank_cost(&self) -> f64 {
        self.spending.as_ref().map_or(0.0, |s| s.headline.usd)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarWorktreeKind {
    /// A durable named cooperation lane. A worktree-backed lane carries the git
    /// story and leads with fork/merge glyphs; a plain lane has no git story
    /// and leads with `#`.
    Channel,
    /// A group root with a git story: a repo room's worktree checkout or a
    /// git-backed row's own resolved worktree.
    Worktree,
    /// A non-repo room's own pod — panes at the root and in non-repo subdirs.
    Root,
    /// The out-of-project catch-all: untethered scripts/CI and shells whose cwd
    /// is outside every group root.
    External,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeTrunkSync {
    Pristine,
    Diverged,
    Merged,
    Reconciling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreePrState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarWorktreeGroup {
    pub key: String,
    pub label: String,
    pub kind: SidebarWorktreeKind,
    pub status_counts: Vec<SidebarStatusCount>,
    pub rows: Vec<SidebarRow>,
    /// Total insertions and deletions relative to trunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_added: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_removed: Option<u32>,
    /// Commits this worktree carries ahead of trunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits_ahead: Option<u32>,
    /// Commits trunk carries past this worktree's fork point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits_behind: Option<u32>,
    /// The resolved trunk name the diff and commit delta compare against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trunk: Option<String>,
    /// Whether the working tree is clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clean: Option<bool>,
    /// Whether committed content is proven landed on the resolved trunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed: Option<bool>,
    /// The semantic trunk state rendered by the group header's git cluster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trunk_sync: Option<WorktreeTrunkSync>,
    /// Best-effort pull-request state for this worktree's branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_state: Option<WorktreePrState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarStatusCount {
    pub status: AgentStatus,
    pub count: usize,
}

/// The single highest-priority unread row: the oldest (min `last_activity`)
/// unread row that needs an *answer* (`waiting`/`failed`). The one global
/// attention lead the sidebar shimmers and snaps the viewport to — computed over
/// the whole unfiltered roster so a make-up filter never shifts it, mirroring
/// the `␣` triage head (oldest actionable first). `None` when nothing unread
/// needs an answer. The single home for the lead-unread rule; the renderer's
/// `lead_unread` and the viewport's unread-focus snap both read it.
pub fn lead_unread_row(groups: &[SidebarWorktreeGroup]) -> Option<&SidebarRow> {
    groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.unread)
        .filter(|row| row.status().is_some_and(AgentStatus::is_actionable))
        .min_by_key(|row| row.last_activity)
}

/// Triage tier for the `space`/`n` inbox walk. The jump banner count, the inbox
/// walk, and the lead unread row all read this vocabulary: unread rows that need
/// one look first, then read actionable rows, oldest activity first within each.
pub fn triage_key(row: &SidebarRow) -> Option<(u8, jiff::Timestamp)> {
    let status = row.status()?;
    if row.unread && status.needs_a_look() {
        Some((0, row.last_activity))
    } else if !row.unread && status.is_actionable() {
        Some((1, row.last_activity))
    } else {
        None
    }
}

/// Rows the jump banner counts: unread rows that need an answer, matching
/// [`lead_unread_row`]'s predicate.
pub fn actionable_unread_count(groups: &[SidebarWorktreeGroup]) -> usize {
    groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.unread && row.status().is_some_and(AgentStatus::is_actionable))
        .count()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarLinkFreshness {
    Fresh,
    Stale,
}

/// The producer's raw client-presence sample, classified into
/// [`SidebarPresence`] at enrich time against the configured idle window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceSample {
    pub human_clients: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_input_ms: Option<u64>,
    pub sampled_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SidebarPresence {
    Active,
    Idle { idle_ms: u64 },
    Detached,
}

impl SidebarPresence {
    /// Classify client presence from the producer's mux sample against the
    /// reader's clock. `last_input_ms` is `None` when the backend cannot report
    /// trustworthy input idle (Zellij), so an attached client reads active until
    /// it detaches.
    pub fn classify(sample: PresenceSample, now_ms: u64, idle_threshold_ms: u64) -> Self {
        if sample.human_clients == 0 {
            return Self::Detached;
        }
        match sample.last_input_ms {
            Some(last_input_ms) => {
                let idle_ms = now_ms.saturating_sub(last_input_ms);
                if idle_ms >= idle_threshold_ms {
                    Self::Idle { idle_ms }
                } else {
                    Self::Active
                }
            }
            None => Self::Active,
        }
    }

    pub fn shows_badge(self) -> bool {
        matches!(self, Self::Idle { .. } | Self::Detached)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarLinkHealth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<u32>,
    pub miss_pct: u16,
    pub tier: LinkTier,
    pub freshness: SidebarLinkFreshness,
    pub sampled_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_classifies_detached_active_idle_boundary_and_unknown_idle() {
        let now = 1_700_000_000_000;
        let threshold_ms = 15 * 60 * 1_000;

        assert_eq!(
            SidebarPresence::classify(
                PresenceSample {
                    human_clients: 0,
                    last_input_ms: Some(now),
                    sampled_at_ms: now,
                },
                now,
                threshold_ms,
            ),
            SidebarPresence::Detached
        );
        assert_eq!(
            SidebarPresence::classify(
                PresenceSample {
                    human_clients: 1,
                    last_input_ms: Some(now - threshold_ms + 1),
                    sampled_at_ms: now,
                },
                now,
                threshold_ms,
            ),
            SidebarPresence::Active,
        );
        assert_eq!(
            SidebarPresence::classify(
                PresenceSample {
                    human_clients: 1,
                    last_input_ms: Some(now - threshold_ms),
                    sampled_at_ms: now,
                },
                now,
                threshold_ms,
            ),
            SidebarPresence::Idle {
                idle_ms: threshold_ms,
            },
        );
        assert_eq!(
            SidebarPresence::classify(
                PresenceSample {
                    human_clients: 1,
                    last_input_ms: None,
                    sampled_at_ms: now,
                },
                now,
                threshold_ms,
            ),
            SidebarPresence::Active,
        );
        assert_eq!(
            SidebarPresence::classify(
                PresenceSample {
                    human_clients: 1,
                    last_input_ms: Some(now - threshold_ms),
                    sampled_at_ms: now - 10_000,
                },
                now + 5_000,
                threshold_ms,
            ),
            SidebarPresence::Idle {
                idle_ms: threshold_ms + 5_000,
            },
        );
    }
}
