use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agents::{AgentState, AgentStatus, ParkDemotion, TurnPhase};
use crate::agents::{
    Entitlement, ExtraCredits, ProviderAccountScope, RateLimitWindow, ResetCredits, SpendTally,
};
use crate::config::PaletteRole;
use crate::ids::LinkTier;
use crate::store::snapshot::row::{SidebarRow, SidebarSubAgent};

/// The displayed status and phase: card first, then child, then rowless demotion.
pub fn card_status(
    agent: &AgentState,
    row: Option<&SidebarRow>,
    child: Option<&SidebarSubAgent>,
    demotion: &ParkDemotion,
) -> (AgentStatus, TurnPhase) {
    if let Some(card) = row.and_then(SidebarRow::as_agent) {
        return (card.status, card.phase);
    }
    if let Some(child) = child {
        return (child.status, child.phase);
    }
    agent.rowless_status(demotion)
}

/// An actionable card, shared by the agent and team command scenes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeedsYou {
    pub status: AgentStatus,
    pub reason: Option<String>,
    pub since: jiff::Timestamp,
}

/// Project attention using the sidebar's waiting-child and native-failure lifts and rowless demotion.
pub fn needs_you(
    agent: &crate::agents::AgentState,
    row: Option<&SidebarRow>,
    demotion: &crate::agents::ParkDemotion,
    now: jiff::Timestamp,
) -> Option<NeedsYou> {
    use crate::store::snapshot::row::{AgentCard, CardDescription};

    let status = row
        .and_then(SidebarRow::attention_status)
        .unwrap_or_else(|| card_status(agent, row, None, demotion).0);
    if !status.is_attention() {
        return None;
    }
    let fallback;
    let card = match row.and_then(SidebarRow::as_agent) {
        Some(card) => card,
        None => {
            let (status, phase) = card_status(agent, row, None, demotion);
            fallback = AgentCard {
                status,
                phase,
                turn_error_label: agent
                    .displayed_turn_error()
                    .and_then(|(_, error)| error.label.clone()),
                ..AgentCard::from_agent(agent, now)
            };
            &fallback
        }
    };
    let ask = agent
        .open_ask
        .as_ref()
        .filter(|_| agent.is_awaiting_input());
    let reason = ask
        .and_then(|ask| ask.detail.clone())
        .or_else(|| match card.description(now) {
            CardDescription::Error(text)
            | CardDescription::Wait(text)
            | CardDescription::Activity(text) => Some(text),
            CardDescription::None => None,
        });
    Some(NeedsYou {
        status,
        reason,
        since: ask.map_or_else(
            || row.map_or(agent.last_activity, |row| row.last_activity),
            |ask| ask.since,
        ),
    })
}

/// One parked workflow run, drawn as an inert cockpit line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParkedWorkflow {
    pub run: crate::ids::WorkflowRunId,
    pub workflow: String,
    pub node: String,
    pub reason: crate::store::workflow::ParkReason,
    pub text: Option<String>,
    pub checkout: std::path::PathBuf,
    pub since: jiff::Timestamp,
}

/// One configured time-windowed dollar cap and its current metered state.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DailyBudgetView {
    pub cap_usd: f64,
    pub spend_usd: f64,
    pub parked: bool,
}

/// What auto-redeem would do if the longest Codex window ran dry right now.
/// A hypothetical, so it reads no live agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedeemForecast {
    /// Auto-redeem is off: only the last-30-minute expiry rescue would fire.
    Manual,
    /// A credit would be spent, or no natural reset is known to wait for.
    Armed,
    /// Nothing would fire: paid credit comes first or the natural reset is near. Firm
    /// until that reset only when it is nearer than `auto_redeem_min_gain` and
    /// the nearest credit outlives it by a day.
    Holding,
}

/// Remote-control badge for this provider: hidden, shown green/red by
/// managed-server health, or muted when the user started this room without it
/// (the `⇅ rc` flag).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteControlBadge {
    #[default]
    Hidden,
    Healthy,
    Down,
    OffForRun,
}

/// One provider's aggregate dashboard block, pinned to the bottom of the
/// sidebar. Account-scoped: every session of one agent kind folds into one
/// block — summed spend and tokens, plus the freshest session's plan, version,
/// and rate-limit windows — so the budgets render once per account, never per
/// row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarProviderPanel {
    pub kind: String,
    #[serde(default, skip_serializing_if = "crate::ids::LoginName::is_default")]
    pub account: crate::ids::LoginName,
    /// Account cache identity selected by the adapter for this panel.
    #[serde(default, skip_serializing_if = "ProviderAccountScope::is_kind_wide")]
    pub account_scope: ProviderAccountScope,
    #[serde(default)]
    pub entitlement: Entitlement,
    /// Opaque account fingerprint whose live windows this panel represents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_key: Option<String>,
    /// Theme-resolved product title; named accounts append ` · label`, falling
    /// back to the account name when no valid display label is set.
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
    /// Brand plan label (`Claude Max 20x`, `ChatGPT Pro 200`); `None` when unknown.
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
    pub spending: Option<Arc<SpendTally>>,
    /// Configured provider-login local-day dollar cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_budget: Option<DailyBudgetView>,
    /// Counting provider-login UTC-calendar-month dollar cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub month_budget: Option<DailyBudgetView>,
    /// Paid usage beyond subscription windows: provider extra credits or
    /// API-key spend against a monthly budget or optional display ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_credits: Option<ExtraCredits>,
    /// Banked rate-limit reset credits, shown as a compact header marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<ResetCredits>,
    /// Auto-redeem forecast for the reset-credit marker: manual (auto-redeem
    /// off), armed (a dry window would spend a credit), or holding (paid credit
    /// comes first or the fleet waits for the natural reset). `None` without reset credits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redeem_forecast: Option<RedeemForecast>,
    /// Descriptor-declared budget-window labels, painted as placeholder tracks
    /// while a metered account has no window readings yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub window_placeholders: Vec<String>,
    /// The account-scoped budget windows, ordered short→long by duration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<RateLimitWindow>,
}

impl SidebarProviderPanel {
    pub(crate) fn login_key(&self) -> crate::ids::LoginKey {
        crate::ids::LoginKey {
            kind: crate::ids::AgentKind::new_unchecked(&self.kind),
            name: self.account.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarWorktreeKind {
    /// A cooperation lane. A worktree-backed lane carries the git
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

/// Where an open pull request stands with its forge's merge queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreePrQueue {
    Queued,
    Dequeued,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCi {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarPipeline {
    /// Declared stages, excluding Done.
    pub stages: Vec<String>,
    pub stage: String,
    pub owner: Option<String>,
    pub started_at: Option<jiff::Timestamp>,
    /// Start of the open visit to the board stage; absent when the ledger's open stage differs.
    pub stage_started_at: Option<jiff::Timestamp>,
    #[serde(default)]
    pub stage_prior_secs: u64,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub visited: BTreeSet<String>,
    pub done_at: Option<jiff::Timestamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelinePosition {
    At(usize),
    Done,
    Undeclared,
}

impl SidebarPipeline {
    pub fn position(&self) -> PipelinePosition {
        if self.stage == crate::config::DONE_STAGE {
            return PipelinePosition::Done;
        }
        self.stages
            .iter()
            .position(|stage| stage == &self.stage)
            .map_or(PipelinePosition::Undeclared, PipelinePosition::At)
    }

    /// Accumulated time in the current stage; `None` without an open visit or at `Done`.
    pub fn span_secs(&self, now: jiff::Timestamp) -> Option<u64> {
        if self.position() == PipelinePosition::Done {
            return None;
        }
        let start = self.stage_started_at?;
        if start > now {
            return None;
        }
        Some(self.stage_prior_secs + u64::try_from(now.duration_since(start).as_secs()).ok()?)
    }

    pub fn total_secs(&self, now: jiff::Timestamp) -> Option<u64> {
        let start = self.started_at?;
        let end = if self.position() == PipelinePosition::Done {
            now.min(self.done_at?)
        } else {
            now
        };
        if start > end {
            return None;
        }
        u64::try_from(end.duration_since(start).as_secs()).ok()
    }

    pub fn clock_running(&self) -> bool {
        (self.stage_started_at.is_some() || self.started_at.is_some())
            && self.position() != PipelinePosition::Done
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrStack {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub below: Vec<StackPr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub above: Vec<Vec<StackPr>>,
}

impl PrStack {
    pub fn is_empty(&self) -> bool {
        self.below.is_empty() && self.above.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackPr {
    #[serde(default)]
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<SidebarPipeline>,
    pub status_counts: Vec<SidebarStatusCount>,
    pub rows: std::sync::Arc<Vec<SidebarRow>>,
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
    /// Best-effort merge-queue state of this worktree's open pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_queue: Option<WorktreePrQueue>,
    /// Best-effort CI verdict for the trunk's branch or the branch's open/merged pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<WorktreeCi>,
    /// Best-effort linked pull-request number: the forge-resolved PR for the
    /// branch, else the worktree marker's `--from-pr` provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    /// Best-effort web URL for the forge-resolved pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(default, skip_serializing_if = "PrStack::is_empty")]
    pub pr_stack: PrStack,
}

/// A work unit within one projected group; keys never merge across groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SidebarUnitKey<'a> {
    Team(&'a str),
    Solo(&'a str),
}

/// A work unit's counted buckets and their row carriers, independent of the cockpit lens.
pub struct UnitStatus {
    buckets: Vec<AgentStatus>,
    calm_carrier: CalmCarrier,
}

enum CalmCarrier {
    Matching,
    Owner(String),
    All,
}

impl UnitStatus {
    /// Pure verdict over `(attention status, is owner, row ID)`; unfinished boards without owners use the unstaged rule.
    pub fn from_members<'a>(
        members: impl IntoIterator<Item = (AgentStatus, bool, &'a str)>,
        pipeline: Option<&SidebarPipeline>,
    ) -> Self {
        use AgentStatus::*;
        let members = members.into_iter().collect::<Vec<_>>();
        let owner = members
            .iter()
            .find(|(_, owner, _)| *owner)
            .map(|(status, _, id)| (*status, *id));
        let mut buckets = [Failed, Waiting, Paused]
            .into_iter()
            .filter(|status| members.iter().any(|(member, _, _)| member == status))
            .collect::<Vec<_>>();
        let (calm, calm_carrier) = match (pipeline, owner) {
            (Some(pipeline), _) if pipeline.position() == PipelinePosition::Done => {
                (Some(Success), CalmCarrier::All)
            }
            (Some(_), Some((status, owner_id))) => (
                match status {
                    Running | Sleeping => Some(status),
                    Idle | Success => Some(Idle),
                    Failed | Waiting | Paused => None,
                },
                CalmCarrier::Owner(owner_id.to_owned()),
            ),
            _ => (
                [Running, Sleeping, Idle, Success]
                    .into_iter()
                    .find(|status| members.iter().any(|(member, _, _)| member == status)),
                CalmCarrier::Matching,
            ),
        };
        if let Some(calm) = calm
            && (calm == Running || buckets.is_empty())
        {
            buckets.push(calm);
        }
        Self {
            buckets,
            calm_carrier,
        }
    }

    pub fn buckets(&self) -> &[AgentStatus] {
        &self.buckets
    }

    pub fn carries(&self, row: &SidebarRow, bucket: AgentStatus) -> bool {
        if !self.buckets.contains(&bucket) {
            return false;
        }
        if bucket.is_attention() {
            return row.attention_status() == Some(bucket);
        }
        match &self.calm_carrier {
            CalmCarrier::Matching => row.attention_status() == Some(bucket),
            CalmCarrier::Owner(owner_id) => row.id == *owner_id,
            CalmCarrier::All => true,
        }
    }

    pub fn headline(&self) -> AgentStatus {
        self.buckets.first().copied().unwrap_or(AgentStatus::Idle)
    }

    /// Whether the unit's calm verdict comes from a board at Done rather than member turns.
    pub fn is_board_done(&self) -> bool {
        matches!(self.calm_carrier, CalmCarrier::All)
    }
}

impl SidebarWorktreeGroup {
    pub fn unit_statuses(&self) -> BTreeMap<SidebarUnitKey<'_>, UnitStatus> {
        let owner = self.pipeline_owner();
        let mut units: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for row in self.rows.iter() {
            let Some(status) = row.attention_status() else {
                continue;
            };
            units.entry(self.unit_key(row)).or_default().push((
                status,
                owner.is_some_and(|owner| owner.id == row.id),
                row.id.as_str(),
            ));
        }
        units
            .into_iter()
            .map(|(key, members)| {
                let pipeline = match key {
                    SidebarUnitKey::Team(team) if Some(team) == self.team.as_deref() => {
                        self.pipeline.as_ref()
                    }
                    _ => None,
                };
                (key, UnitStatus::from_members(members, pipeline))
            })
            .collect()
    }

    pub(crate) fn pipeline_owner(&self) -> Option<&SidebarRow> {
        if self.kind == SidebarWorktreeKind::External {
            return None;
        }
        let team = self.team.as_deref()?;
        let owner = self.pipeline.as_ref()?.owner.as_deref()?;
        self.rows.iter().find(|row| {
            row.team() == Some(team)
                && row.as_agent().and_then(|card| card.handle.as_deref()) == Some(owner)
        })
    }

    pub(crate) fn unit_key<'a>(&self, row: &'a SidebarRow) -> SidebarUnitKey<'a> {
        if self.kind != SidebarWorktreeKind::External
            && let Some(team) = row.team()
        {
            return SidebarUnitKey::Team(team);
        }
        SidebarUnitKey::Solo(&row.id)
    }

    /// A finished roster with any agent collapses behind the two-line receipt.
    /// Process-only groups stay open.
    pub fn collapses(&self) -> bool {
        self.finished && self.rows.iter().any(SidebarRow::is_agent)
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
        .flat_map(|group| group.rows.iter())
        .filter(|row| row.unread)
        .filter(|row| {
            row.attention_status()
                .is_some_and(AgentStatus::is_actionable)
        })
        .min_by_key(|row| row.last_activity)
}

/// Triage tier for the `space`/`n` inbox walk. The jump banner count, the inbox
/// walk, and the lead unread row all read this vocabulary: unread rows that need
/// one look first, then read actionable rows, oldest activity first within each.
pub(crate) fn triage_key(row: &SidebarRow) -> Option<(u8, jiff::Timestamp)> {
    let status = row.attention_status()?;
    if row.unread && status.needs_a_look() {
        Some((0, row.last_activity))
    } else if !row.unread && status.is_actionable() {
        Some((1, row.last_activity))
    } else {
        None
    }
}

/// Units the jump banner counts: at least one unread member needs an answer,
/// matching [`lead_unread_row`]'s row predicate.
pub(crate) fn actionable_unread_count(groups: &[SidebarWorktreeGroup]) -> usize {
    groups
        .iter()
        .map(|group| {
            group
                .rows
                .iter()
                .filter(|row| {
                    row.unread
                        && row
                            .attention_status()
                            .is_some_and(AgentStatus::is_actionable)
                })
                .map(|row| group.unit_key(row))
                .collect::<BTreeSet<_>>()
                .len()
        })
        .sum()
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
    Idle { last_input_ms: u64 },
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
                    Self::Idle { last_input_ms }
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
    fn sidebar_link_health_serializes_tier_as_snake_case() {
        let health = SidebarLinkHealth {
            rtt_ms: Some(230),
            miss_pct: 4,
            tier: LinkTier::Degraded,
            freshness: SidebarLinkFreshness::Fresh,
            sampled_at_ms: 1,
        };
        let json = serde_json::to_value(&health).unwrap();
        assert_eq!(json["tier"], "degraded");
        assert_eq!(
            serde_json::from_value::<SidebarLinkHealth>(json).unwrap(),
            health
        );
    }

    fn collapse_test_row(id: &str, agent: bool) -> SidebarRow {
        SidebarRow {
            checkout: None,
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
                crate::store::snapshot::RowCard::Agent(std::sync::Arc::default())
            } else {
                crate::store::snapshot::RowCard::Process(
                    crate::store::snapshot::ProcessCard::default(),
                )
            },
        }
    }

    fn collapse_test_group(rows: Vec<SidebarRow>) -> SidebarWorktreeGroup {
        SidebarWorktreeGroup {
            pr_stack: Default::default(),
            key: "group".to_owned(),
            label: "group".to_owned(),
            label_qualifier: None,
            kind: SidebarWorktreeKind::Worktree,
            team: None,
            cohort_effort: None,
            pipeline: None,
            status_counts: Vec::new(),
            rows: rows.into(),
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
            pr_queue: None,
            ci: None,
            pr_number: None,
            pr_url: None,
        }
    }

    #[test]
    fn finished_groups_collapse_with_any_agent() {
        assert!(collapse_test_group(vec![collapse_test_row("solo", true)]).collapses());
        assert!(
            !collapse_test_group(vec![
                collapse_test_row("shell-one", false),
                collapse_test_row("shell-two", false),
            ])
            .collapses()
        );
        assert!(
            collapse_test_group(vec![
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
    fn worktree_group_without_pipeline_stays_serde_compatible() {
        let group = collapse_test_group(Vec::new());
        let mut value = serde_json::to_value(&group).unwrap();
        assert!(value.get("pipeline").is_none());
        value.as_object_mut().unwrap().remove("pipeline");
        let decoded: SidebarWorktreeGroup = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.pipeline, None);
    }

    fn pipeline(stage: &str) -> SidebarPipeline {
        SidebarPipeline {
            stages: vec!["Plan".to_owned(), "Implement".to_owned()],
            stage: stage.to_owned(),
            owner: Some("coder".to_owned()),
            started_at: Some(jiff::Timestamp::from_second(100).unwrap()),
            stage_started_at: Some(jiff::Timestamp::from_second(120).unwrap()),
            stage_prior_secs: 0,
            visited: BTreeSet::new(),
            done_at: None,
        }
    }

    #[test]
    fn pipeline_position_follows_the_board_stage() {
        assert_eq!(pipeline("Plan").position(), PipelinePosition::At(0));
        assert_eq!(pipeline("Implement").position(), PipelinePosition::At(1));
        assert_eq!(pipeline("Done").position(), PipelinePosition::Done);
        assert_eq!(pipeline("Review").position(), PipelinePosition::Undeclared);
    }

    #[test]
    fn pipeline_span_uses_the_stage_start_and_stops_at_done() {
        let now = jiff::Timestamp::from_second(200).unwrap();
        let mut run = pipeline("Implement");
        assert_eq!(run.span_secs(now), Some(80));
        run.done_at = Some(jiff::Timestamp::from_second(150).unwrap());
        assert_eq!(run.span_secs(now), Some(80));
        run.started_at = None;
        assert_eq!(run.span_secs(now), Some(80));
        run.started_at = Some(jiff::Timestamp::from_second(100).unwrap());
        run.stage = "Done".to_owned();
        assert_eq!(run.span_secs(now), None);
        assert_eq!(run.total_secs(now), Some(50));
        run.stage_started_at = None;
        assert_eq!(run.total_secs(now), Some(50));
        run.done_at = Some(jiff::Timestamp::from_second(250).unwrap());
        assert_eq!(run.total_secs(now), Some(100));
        run.done_at = Some(jiff::Timestamp::from_second(99).unwrap());
        assert_eq!(run.total_secs(now), None);
        run.done_at = None;
        assert_eq!(run.total_secs(now), None);
        run.done_at = Some(now);
        run.started_at = Some(jiff::Timestamp::from_second(201).unwrap());
        assert_eq!(run.total_secs(now), None);
        run.started_at = Some(jiff::Timestamp::new(199, 500_000_000).unwrap());
        assert_eq!(run.total_secs(now), Some(0));
        run.started_at = None;
        assert_eq!(run.total_secs(now), None);
        run.stage = "Implement".to_owned();
        run.stage_started_at = Some(jiff::Timestamp::from_second(201).unwrap());
        assert_eq!(run.span_secs(now), None);
        run.stage_started_at = Some(jiff::Timestamp::new(199, 500_000_000).unwrap());
        assert_eq!(run.span_secs(now), Some(0));
        run.stage_started_at = None;
        assert_eq!(run.span_secs(now), None);
    }

    #[test]
    fn pipeline_span_accumulates_closed_visits_only_with_an_open_visit() {
        let now = jiff::Timestamp::from_second(200).unwrap();
        let mut run = pipeline("Implement");
        run.stage_prior_secs = 60;
        assert_eq!(run.span_secs(now), Some(140));
        run.stage_started_at = Some(jiff::Timestamp::from_second(201).unwrap());
        assert_eq!(run.span_secs(now), None);
        run.stage_started_at = None;
        assert_eq!(run.span_secs(now), None);
        run.stage_started_at = Some(now);
        run.stage = "Done".to_owned();
        assert_eq!(run.span_secs(now), None);
    }

    #[test]
    fn pipeline_reads_older_cache_without_visit_fields() {
        let mut value = serde_json::to_value(pipeline("Implement")).unwrap();
        value.as_object_mut().unwrap().remove("stage_prior_secs");
        value.as_object_mut().unwrap().remove("visited");
        let decoded = serde_json::from_value::<SidebarPipeline>(value);
        assert!(decoded.is_ok(), "{decoded:?}");
        let decoded = decoded.unwrap();
        assert_eq!(decoded.stage_prior_secs, 0);
        assert!(decoded.visited.is_empty());
        let encoded = serde_json::to_value(decoded).unwrap();
        assert_eq!(encoded["stage_prior_secs"], 0);
        assert!(encoded.get("visited").is_none());
    }

    #[test]
    fn pipeline_clock_runs_only_with_a_start_outside_done() {
        assert!(pipeline("Implement").clock_running());
        assert!(pipeline("Review").clock_running());
        assert!(!pipeline("Done").clock_running());
        let mut run = pipeline("Plan");
        run.started_at = None;
        assert!(run.clock_running());
        run.started_at = Some(jiff::Timestamp::UNIX_EPOCH);
        run.stage_started_at = None;
        assert!(run.clock_running());
        run.started_at = None;
        assert!(!run.clock_running());
    }

    #[test]
    fn pipeline_total_runs_to_now_without_a_stage_stamp() {
        let now = jiff::Timestamp::from_second(200).unwrap();
        let mut run = pipeline("Implement");
        assert_eq!(run.total_secs(now), Some(100));
        run.stage_started_at = None;
        assert_eq!(run.total_secs(now), Some(100));
        run.done_at = Some(jiff::Timestamp::from_second(150).unwrap());
        assert_eq!(run.total_secs(now), Some(100));
        run.started_at = Some(jiff::Timestamp::from_second(201).unwrap());
        assert_eq!(run.total_secs(now), None);
        run.started_at = None;
        assert_eq!(run.total_secs(now), None);
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
        let idle_sample = PresenceSample {
            human_clients: 1,
            last_input_ms: Some(now - threshold_ms),
            sampled_at_ms: now,
        };
        assert_eq!(
            SidebarPresence::classify(idle_sample, now, threshold_ms),
            SidebarPresence::classify(idle_sample, now + 60_000, threshold_ms),
            "idle presence content must stay stable as the snapshot clock advances",
        );

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
                last_input_ms: now - threshold_ms,
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
                last_input_ms: now - threshold_ms,
            },
        );
    }
}
