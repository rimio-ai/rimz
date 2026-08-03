use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::agents::AgentStatus;
use crate::agents::{
    ExtraCredits, ProviderAccountScope, RateLimitWindow, ResetCredits, SpendTally,
};
use crate::config::PaletteRole;
use crate::remote::link::LinkTier;
use crate::store::snapshot::row::SidebarRow;

/// One configured local-day dollar cap and its current metered state.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DailyBudgetView {
    pub cap_usd: f64,
    pub spend_usd: f64,
    pub parked: bool,
}

/// Remote-control badge for this provider: hidden, or shown green/red by
/// managed-server health (the `⇅ rc` flag).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteControlBadge {
    #[default]
    Hidden,
    Healthy,
    Down,
}

/// One provider's aggregate dashboard block, pinned to the bottom of the
/// sidebar. Account-scoped: every session of one agent kind folds into one
/// block — summed spend and tokens, plus the freshest session's plan, version,
/// and rate-limit windows — so the budgets render once per account, never per
/// row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarProviderPanel {
    pub kind: String,
    /// Account cache identity selected by the adapter for this panel.
    #[serde(default, skip_serializing_if = "ProviderAccountScope::is_kind_wide")]
    pub account_scope: ProviderAccountScope,
    /// Header display name (`Claude`, `Codex`, …).
    pub product_name: String,
    /// Multi-line ASCII emblem, painted brand-colored at the block's left.
    pub art: Vec<String>,
    /// Tinted emblem runs painted over the brand tone; empty for one-tone art.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub art_tints: Vec<crate::agents::EmblemTint>,
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
    /// Visibility and managed-server health for the `⇅ rc` flag.
    #[serde(default)]
    pub remote_control: RemoteControlBadge,
    /// Currently bound, identity-bearing root panes for this provider. This is
    /// live room state and remains separate from historical transcript spend.
    #[serde(default)]
    pub active_sessions: u32,
    /// JSONL-computed headline / week / month / trailing-year spend and tokens
    /// for this provider, summed across all of its sessions' transcript history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spending: Option<SpendTally>,
    /// Configured provider-login local-day dollar cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_budget: Option<DailyBudgetView>,
    /// Paid usage beyond subscription windows: provider extra credits or
    /// API-key spend against an optional display ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_credits: Option<ExtraCredits>,
    /// Codex rate-limit reset credits, shown as a compact header marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<ResetCredits>,
    /// Descriptor-declared budget-window labels, painted as placeholder tracks
    /// while a metered account has no window readings yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub window_placeholders: Vec<String>,
    /// The account-scoped budget windows, ordered short→long by duration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<RateLimitWindow>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreePrCi {
    Pending,
    Passing,
    Failing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SidebarSeatEffort {
    pub cost_usd: Option<f64>,
    pub tokens: crate::agents::spending::EffortTokens,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SidebarCohortEffort {
    pub cost_usd: Option<f64>,
    pub tokens: crate::agents::spending::EffortTokens,
    pub active_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seats: BTreeMap<String, SidebarSeatEffort>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarWorktreeGroup {
    pub key: String,
    pub label: String,
    /// Dim repo qualifier appended to the header when another group renders
    /// the same label: the shortest trailing path suffix distinguishing the
    /// colliding checkouts. `None` when the label is unambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_qualifier: Option<String>,
    pub kind: SidebarWorktreeKind,
    /// The unique non-empty team carried by this group's team-stamped rows.
    /// Roleless processes and stray agents do not hide a cohort label; two
    /// distinct teams make the group ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cohort_effort: Option<SidebarCohortEffort>,
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
    /// Whether a channel lane is backed by a live RimZ worktree checkout
    /// (marker name matches the lane label). Stamped by the git projection
    /// every fold; drives the header's fork-vs-`#` lead without waiting for
    /// the first git read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worktree_backed: bool,
    /// Terminal line of work: git verdict `Done` with no attention or running
    /// member. Forces the archive band and collapses multi-agent rosters in
    /// renderers.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub finished: bool,
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
    /// Best-effort CI verdict for the branch's open pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_ci: Option<WorktreePrCi>,
    /// Best-effort linked pull-request number: the forge-resolved PR for the
    /// branch, else the worktree marker's `--from-pr` provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    /// Best-effort web URL for the forge-resolved pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
}

impl SidebarWorktreeGroup {
    /// A finished roster with several agents collapses behind the two-line
    /// receipt. Process rows never count, so a lone agent stays on screen.
    pub fn collapses(&self) -> bool {
        self.finished && self.rows.iter().filter(|row| row.is_agent()).count() > 1
    }
}

/// The unique non-empty team in a cohort. Unstamped members are tolerated;
/// two distinct stamped teams make the cohort ambiguous.
pub(super) fn cohort_team<'a>(teams: impl IntoIterator<Item = Option<&'a str>>) -> Option<&'a str> {
    let mut unique = None;
    for team in teams.into_iter().flatten().filter(|team| !team.is_empty()) {
        match unique {
            None => unique = Some(team),
            Some(current) if current == team => {}
            Some(_) => return None,
        }
    }
    unique
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

    fn collapse_test_row(id: &str, agent: bool) -> SidebarRow {
        SidebarRow {
            id: id.to_owned(),
            name: id.to_owned(),
            pane: None,
            worktree_path: None,
            worktree_branch: None,
            channel: None,
            unread: false,
            inactive: false,
            archived: false,
            attention_score: 0,
            last_activity: jiff::Timestamp::now(),
            card: if agent {
                crate::store::snapshot::RowCard::Agent(Box::default())
            } else {
                crate::store::snapshot::RowCard::Process(
                    crate::store::snapshot::ProcessCard::default(),
                )
            },
        }
    }

    fn collapse_test_group(rows: Vec<SidebarRow>) -> SidebarWorktreeGroup {
        SidebarWorktreeGroup {
            key: "group".to_owned(),
            label: "group".to_owned(),
            label_qualifier: None,
            kind: SidebarWorktreeKind::Worktree,
            team: None,
            cohort_effort: None,
            status_counts: Vec::new(),
            rows,
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            worktree_backed: false,
            finished: true,
            clean: None,
            landed: None,
            trunk_sync: None,
            pr_state: None,
            pr_ci: None,
            pr_number: None,
            pr_url: None,
        }
    }

    #[test]
    fn finished_groups_collapse_only_with_several_agents() {
        assert!(
            !collapse_test_group(vec![
                collapse_test_row("shell-one", false),
                collapse_test_row("shell-two", false),
            ])
            .collapses()
        );
        assert!(
            !collapse_test_group(vec![
                collapse_test_row("agent", true),
                collapse_test_row("shell-one", false),
                collapse_test_row("shell-two", false),
            ])
            .collapses()
        );
        assert!(
            collapse_test_group(vec![
                collapse_test_row("agent-one", true),
                collapse_test_row("agent-two", true),
                collapse_test_row("shell", false),
            ])
            .collapses()
        );
    }

    #[test]
    fn worktree_group_without_cohort_effort_stays_serde_compatible() {
        let group = collapse_test_group(vec![
            collapse_test_row("agent-one", true),
            collapse_test_row("agent-two", true),
        ]);
        let mut value = serde_json::to_value(&group).unwrap();
        value.as_object_mut().unwrap().remove("cohort_effort");

        let decoded: SidebarWorktreeGroup = serde_json::from_value(value).unwrap();

        assert_eq!(decoded.cohort_effort, None);
    }

    #[test]
    fn cohort_effort_without_seats_stays_serde_compatible() {
        let mut value = serde_json::to_value(SidebarCohortEffort {
            cost_usd: Some(1.0),
            ..SidebarCohortEffort::default()
        })
        .unwrap();
        value.as_object_mut().unwrap().remove("seats");

        let decoded: SidebarCohortEffort = serde_json::from_value(value).unwrap();

        assert!(decoded.seats.is_empty());
        assert_eq!(decoded.cost_usd, Some(1.0));
    }

    #[test]
    fn cohort_team_tolerates_strays_and_rejects_conflicts() {
        assert_eq!(cohort_team([Some("forge")]), Some("forge"));
        assert_eq!(
            cohort_team([Some("forge"), None, Some(""), Some("forge")]),
            Some("forge")
        );
        assert_eq!(cohort_team([Some("forge"), Some("docs")]), None);
        assert_eq!(cohort_team([None, Some("")]), None);
    }

    #[test]
    fn group_team_serializes_only_when_present() {
        let mut group = collapse_test_group(Vec::new());
        let without = serde_json::to_value(&group).unwrap();
        assert!(without.get("team").is_none());

        group.team = Some("forge".to_owned());
        let with = serde_json::to_value(&group).unwrap();
        assert_eq!(with["team"], "forge");
    }

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
