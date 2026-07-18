use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use crate::agents::AgentState;
use crate::agents::context::RateLimitWindowKey;
use crate::agents::{AgentAccount, AgentRateLimits, RateLimitWindow, SpendTally};
use crate::config::ProviderTabsMode;
use crate::theme::{
    BrandColor, provider_title_case, resolve_provider_brand, resolve_provider_identity,
};

use super::{RemoteControlBadge, SidebarProviderPanel, SidebarSnapshot};

impl SidebarSnapshot {
    /// Fold the agent rollup into per-provider dashboard blocks — one per agent
    /// kind, plus one for any provider with no active session this run whose
    /// probed account has something to show: a metered login, a non-empty
    /// identity, or recorded spend (an account-only block, so the dashboard
    /// shows substantive accounts and budgets between turns).
    /// Sums each kind's spend, tokens, and edited lines; takes the plan, version,
    /// and rate-limit windows from the freshest session (account state is shared,
    /// so the latest reading is truest). `probed_accounts` carries out-of-band
    /// login facts the context cannot (Claude's `auth status`, Codex's
    /// `auth.json`), preferred only when the freshest context has none — and a kind
    /// whose only signal is a qualifying probed account still earns a block;
    /// `remote_control` carries the per-kind `⇅ rc` visibility and managed-server
    /// health. Styling (emblem, color, name) and the descriptor-declared empty
    /// budget-window shape resolve here, so the renderer gets a ready-to-paint
    /// block. With no explicit
    /// `provider_list`, providers paint in usage-rank order: live sessions,
    /// then week/month/year session counts, then login and credential recency.
    /// This deliberately ignores intra-day spend shifts. A *stacked* dashboard
    /// caps that ranked list to `max_provider_blocks`; a *tabbed* dashboard
    /// (three or more providers under `auto`) is height-bounded by its active
    /// block, so it shows every discovered provider. An explicit
    /// `provider_list` overrides the shown set and order, with `all` expanding
    /// the remaining discovered providers in usage-rank order and
    /// bypassing the cap. Producer-only: the pure reducer leaves `providers`
    /// empty.
    pub fn with_provider_aggregates(
        mut self,
        probed_accounts: &BTreeMap<String, AgentAccount>,
        remote_control: &BTreeMap<String, RemoteControlBadge>,
        provider_spending: &BTreeMap<String, SpendTally>,
    ) -> Self {
        let kinds = provider_kinds(&self.agents, probed_accounts, provider_spending);
        let mut panels = Vec::new();
        for kind in kinds {
            let active_sessions = u32::try_from(
                self.agent_panes
                    .iter()
                    .filter(|pane| pane.kind.as_str() == kind && pane.agent_id.is_some())
                    .map(|pane| pane.pane_id.raw())
                    .collect::<BTreeSet<_>>()
                    .len(),
            )
            .unwrap_or(u32::MAX);
            let sessions: Vec<&AgentState> = self
                .agents
                .iter()
                .filter(|agent| agent.parent_agent_id.is_none() && agent.kind == kind)
                .collect();
            // Nothing to show without a session or a substantive logged-in
            // account. Recorded spend qualifies a probed account but never
            // creates the provider section for a logged-out provider by itself.
            if sessions.is_empty()
                && !probed_accounts.get(&kind).is_some_and(|account| {
                    account_creates_provider_panel(account, provider_spending.get(&kind))
                })
            {
                continue;
            }

            // The freshest context wins the account-scoped facts (plan, version)
            // — every session shares one account.
            let freshest = sessions
                .iter()
                .filter_map(|agent| agent.context.as_ref())
                .max_by_key(|context| context.observed_at);
            // A live session's rich-context version wins; the out-of-band
            // binary probe covers a provider whose sessions have not reported
            // one.
            let version = freshest
                .and_then(|context| context.agent_version.clone())
                .or_else(|| {
                    probed_accounts
                        .get(&kind)
                        .and_then(|account| account.version.clone())
                });
            let account = freshest
                .and_then(|context| context.account.clone())
                .or_else(|| probed_accounts.get(&kind).cloned());

            // The budget windows are account-scoped too, but the *freshest*
            // session is not the truest reading: parallel sessions report the same
            // window at slightly different instants, so "freshest wins" flips
            // between ticks and the bar flickers. Instead, reject content-stale
            // readings whole (an idle session re-emits a days-old payload with a
            // fresh capture stamp; its shortest applicable reset gives it away),
            // drop individual windows whose own reset has passed, then keep the
            // most-drained survivor per stable identity — within a live window
            // usage only climbs, so this is both stable and the truest. Same inputs
            // always yield the same bars, regardless of which session reported
            // last; the enrich layer fuses this live reading with the cached and
            // authoritative truth.
            let now = self.now;
            let windows_for = |of_kind: &str| {
                fresh_windows(
                    self.agents
                        .iter()
                        .filter(|agent| agent.parent_agent_id.is_none() && agent.kind == *of_kind)
                        .filter_map(|agent| agent.context.as_ref()?.rate_limits.as_ref()),
                    now,
                )
            };
            let windows = windows_for(&kind);
            let has_windows = !windows.is_empty();

            let metered = account
                .as_ref()
                .and_then(|account| account.metered)
                .unwrap_or(has_windows);
            let account_scope = account
                .as_ref()
                .map(|account| account.scope.clone())
                .unwrap_or_default();
            let plan = account
                .and_then(|account| account.plan.clone())
                .filter(|plan| !plan.is_empty())
                .map(|raw| format_plan_label(&kind, &raw));

            let identity = resolve_provider_identity(&kind, &self.theme.providers);
            let (color, color_rgb, color_role) = match identity.brand {
                BrandColor::Role(role) => (
                    resolve_provider_brand(&kind, &BTreeMap::new()).indexed(),
                    None,
                    Some(role),
                ),
                BrandColor::Indexed(index) => (index, None, None),
                BrandColor::Rgb(red, green, blue) => {
                    (identity.brand.indexed(), Some((red, green, blue)), None)
                }
                BrandColor::Brand { index, rgb } => (index, Some(rgb), None),
            };
            let remote_control = remote_control.get(&kind).copied().unwrap_or_default();
            let window_placeholders = crate::agents::descriptor_by_kind(&kind)
                .map(|descriptor| {
                    descriptor
                        .expected_windows
                        .iter()
                        .map(|&label| label.to_owned())
                        .collect()
                })
                .unwrap_or_default();
            let tally = provider_spending.get(&kind);
            let spending = tally.cloned();
            let rank = ProviderRank {
                live: !sessions.is_empty(),
                week_sessions: tally.map_or(0, |tally| tally.week.sessions),
                month_sessions: tally.map_or(0, |tally| tally.month.sessions),
                year_sessions: tally.map_or(0, |tally| tally.year.sessions),
                logged_in: probed_accounts
                    .get(&kind)
                    .is_some_and(|account| account_creates_provider_panel(account, tally)),
                credentials_updated_at_ms: probed_accounts
                    .get(&kind)
                    .and_then(|account| account.credentials_updated_at_ms),
            };

            panels.push((
                SidebarProviderPanel {
                    kind,
                    account_scope,
                    product_name: identity.product_name,
                    art: identity.art,
                    art_tints: identity.art_tints,
                    color,
                    color_rgb,
                    color_role,
                    version,
                    plan,
                    metered,
                    remote_control,
                    active_sessions,
                    spending,
                    day_budget: None,
                    extra_credits: None,
                    reset_credits: None,
                    window_placeholders,
                    windows,
                },
                rank,
            ));
        }

        self.providers = resolve_provider_panels(
            panels,
            &self.theme.display.provider_list,
            self.theme.display.max_provider_blocks,
            self.theme.display.provider_tabs,
        );
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ProviderRank {
    live: bool,
    week_sessions: u32,
    month_sessions: u32,
    year_sessions: u32,
    logged_in: bool,
    credentials_updated_at_ms: Option<u64>,
}

impl ProviderRank {
    fn compare(left: Self, right: Self) -> Ordering {
        right
            .live
            .cmp(&left.live)
            .then_with(|| right.week_sessions.cmp(&left.week_sessions))
            .then_with(|| right.month_sessions.cmp(&left.month_sessions))
            .then_with(|| right.year_sessions.cmp(&left.year_sessions))
            .then_with(|| right.logged_in.cmp(&left.logged_in))
            .then_with(|| {
                right
                    .credentials_updated_at_ms
                    .cmp(&left.credentials_updated_at_ms)
            })
    }
}

fn provider_kinds(
    agents: &[AgentState],
    probed_accounts: &BTreeMap<String, AgentAccount>,
    provider_spending: &BTreeMap<String, SpendTally>,
) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for agent in agents {
        if agent.parent_agent_id.is_none() && !kinds.iter().any(|known| agent.kind == **known) {
            kinds.push(agent.kind.to_string());
        }
    }
    for (kind, account) in probed_accounts {
        if account_creates_provider_panel(account, provider_spending.get(kind))
            && !kinds.iter().any(|known| known == kind)
        {
            kinds.push(kind.clone());
        }
    }
    kinds
}

fn account_creates_provider_panel(account: &AgentAccount, tally: Option<&SpendTally>) -> bool {
    account.metered == Some(true)
        || account
            .account_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
        || tally.is_some_and(|tally| tally.year.sessions > 0)
}

fn resolve_provider_panels(
    mut panels: Vec<(SidebarProviderPanel, ProviderRank)>,
    provider_list: &[String],
    max_provider_blocks: usize,
    provider_tabs: ProviderTabsMode,
) -> Vec<SidebarProviderPanel> {
    if provider_list.is_empty() {
        panels.sort_by(|(left_panel, left_rank), (right_panel, right_rank)| {
            ProviderRank::compare(*left_rank, *right_rank)
                .then_with(|| display_order(left_panel, right_panel))
        });
        // The cap bounds the *stacked* dashboard's height; a tabbed dashboard is
        // bounded by its single active block, so it shows every provider. The
        // tab decision keys on the full discovered count, so the producer and
        // the renderer's `dashboard_tabbed` agree (no truncation when tabbed
        // means the count they see is identical).
        if !provider_tabs.tabs(panels.len()) {
            panels.truncate(max_provider_blocks);
        }
        return panels.into_iter().map(|(panel, _)| panel).collect();
    }

    let explicitly_named: BTreeSet<&str> = provider_list
        .iter()
        .filter_map(|kind| (kind != "all").then_some(kind.as_str()))
        .collect();
    let by_kind: BTreeMap<String, (SidebarProviderPanel, ProviderRank)> = panels
        .into_iter()
        .map(|entry| (entry.0.kind.clone(), entry))
        .collect();
    let mut resolved = Vec::new();
    let mut emitted_named = BTreeSet::new();
    let mut emitted_all = false;
    for kind in provider_list {
        if kind == "all" {
            if !emitted_all {
                // `all` expands the not-yet-named providers in the same usage
                // order as the default dashboard, so `["all"]` matches an empty
                // list.
                let mut remaining: Vec<(SidebarProviderPanel, ProviderRank)> = by_kind
                    .values()
                    .filter(|(panel, _)| !explicitly_named.contains(panel.kind.as_str()))
                    .cloned()
                    .collect();
                remaining.sort_by(|(left_panel, left_rank), (right_panel, right_rank)| {
                    ProviderRank::compare(*left_rank, *right_rank)
                        .then_with(|| display_order(left_panel, right_panel))
                });
                resolved.extend(remaining.into_iter().map(|(panel, _)| panel));
                emitted_all = true;
            }
            continue;
        }
        if emitted_named.insert(kind.as_str())
            && let Some((panel, _)) = by_kind.get(kind)
        {
            resolved.push(panel.clone());
        }
    }
    resolved
}

/// Order two panels by the registry's canonical display order — each kind's slot
/// in [`known_kinds`](crate::agents::known_kinds) (`claude, codex, amp, copilot, kimi, pi, opencode, antigravity, cursor, droid, kiro, qwen, grok`),
/// an unregistered kind sorting last by name. The default dashboard and `all`
/// expansion both use this, so the row reads in the canonical agent order rather
/// than an alphabetical accident; an explicit `provider_list` overrides it.
fn display_order(left: &SidebarProviderPanel, right: &SidebarProviderPanel) -> Ordering {
    display_rank(&left.kind)
        .cmp(&display_rank(&right.kind))
        .then_with(|| left.kind.cmp(&right.kind))
}

/// A kind's position in the registry's display order; unregistered kinds sort
/// last.
fn display_rank(kind: &str) -> usize {
    crate::agents::known_kinds()
        .position(|known| known == kind)
        .unwrap_or(usize::MAX)
}

/// The content-fresh live budget windows across every session of a provider,
/// one per duration or provider-defined scope, tagged with the
/// reading's `observed_at`/`source` for the enrich layer to fuse with cached and
/// authoritative truth.
///
/// Each `reading` is one session's whole window set. A reading is rejected
/// wholesale by [`AgentRateLimits::content_stale_at`] — an idle session
/// re-emits a days-old payload with a fresh capture stamp, so its longer windows
/// look current even though they are not; the shortest applicable reset gives
/// the whole payload away. Among surviving readings, the most-drained value wins
/// per stable identity: within one live window usage only climbs, so the highest
/// reading is the most current and the pick is stable against parallel sessions
/// reporting the same budget at different instants. A surviving reading can
/// still carry a longer window from a previous epoch while its shortest window
/// is mid-cycle, so each expired window is dropped before this comparison.
/// Output keeps duration windows short→long, followed by scoped windows in
/// stable scope-id order.
pub(super) fn fresh_windows<'a>(
    readings: impl Iterator<Item = &'a AgentRateLimits>,
    now: Timestamp,
) -> Vec<RateLimitWindow> {
    let mut by_window: BTreeMap<RateLimitWindowKey, RateLimitWindow> = BTreeMap::new();
    for reading in readings {
        if reading.content_stale_at(now) {
            continue;
        }
        for window in &reading.windows {
            if window.used_percentage.is_none() {
                continue;
            }
            if window.resets_at.is_some_and(|reset| reset <= now) {
                continue;
            }
            by_window
                .entry(window.key())
                .and_modify(|best| {
                    if window.used_percentage > best.used_percentage {
                        *best = window.clone();
                    }
                })
                .or_insert_with(|| window.clone());
        }
    }
    let mut windows: Vec<RateLimitWindow> = by_window.into_values().collect();
    sort_windows(&mut windows);
    windows
}

pub(crate) fn sort_windows(windows: &mut [RateLimitWindow]) {
    windows.sort_by(|left, right| match (&left.scope, &right.scope) {
        (None, None) => left
            .duration_mins
            .unwrap_or(u32::MAX)
            .cmp(&right.duration_mins.unwrap_or(u32::MAX)),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(_), Some(_)) => left.key().cmp(&right.key()),
    });
}

/// Format a raw provider plan tier into its brand label, per the adapter's
/// [`crate::agents::PlanLabel`]: Claude's tiers prefix `Claude` (`max` →
/// `Claude Max`), Codex's prefix `ChatGPT` (`pro` → `ChatGPT Pro`); any other
/// provider just title-cases the tier.
pub(crate) fn format_plan_label(kind: &str, raw: &str) -> String {
    match crate::agents::descriptor_by_kind(kind).map(|descriptor| &descriptor.plan_label) {
        Some(label) => label.format(raw),
        None => provider_title_case(raw),
    }
}
