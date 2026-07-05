use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use crate::agents::AgentState;
use crate::agents::{AgentAccount, AgentRateLimits, RateLimitWindow, SpendTally};
use crate::config::{ProviderTabsMode, ThemeColor};

use super::{SidebarProviderPanel, SidebarSnapshot};

impl SidebarSnapshot {
    /// Fold the agent rollup into per-provider dashboard blocks — one per agent
    /// kind, plus one for any provider with no active session this run that is
    /// logged in (an account-only block, so the dashboard shows your accounts
    /// and budgets between turns).
    /// Sums each kind's spend, tokens, and edited lines; takes the plan, version,
    /// and rate-limit windows from the freshest session (account state is shared,
    /// so the latest reading is truest). `probed_accounts` carries out-of-band
    /// login facts the context cannot (Claude's `auth status`, Codex's
    /// `auth.json`), preferred only when the freshest context has none — and a kind
    /// whose only signal is such a login still earns a block;
    /// `remote_control` carries the per-kind `⇅ rc` flag. Styling (emblem, color,
    /// name) resolves from `self.theme.providers` over the built-in defaults, so
    /// the renderer gets a ready-to-paint block. With no explicit
    /// `provider_list`, a *stacked* dashboard is capped to `max_provider_blocks`
    /// by today's spend; a *tabbed* dashboard (three or more providers under
    /// `auto`) is height-bounded by its active block, so it shows every
    /// discovered provider. The retained set paints in the registry's display
    /// order (`claude, codex, pi, opencode`) — the panels are the dashboard's
    /// tabs, so the row never reorders as spend shifts. An explicit
    /// `provider_list` overrides the shown set and order, with `all` expanding
    /// the remaining discovered providers (in that same display order) and
    /// bypassing the cap. Producer-only: the pure reducer leaves `providers`
    /// empty.
    pub fn with_provider_aggregates(
        mut self,
        probed_accounts: &BTreeMap<String, AgentAccount>,
        remote_control: &BTreeMap<String, bool>,
        provider_spending: &BTreeMap<String, SpendTally>,
    ) -> Self {
        let kinds = provider_kinds(&self.agents, probed_accounts);
        let mut panels: Vec<SidebarProviderPanel> = Vec::new();
        for kind in kinds {
            let sessions: Vec<&AgentState> = self
                .agents
                .iter()
                .filter(|agent| agent.parent_agent_id.is_none() && agent.kind == kind)
                .collect();
            // Nothing to show without a session or a logged-in account. Recorded
            // spend enriches an existing provider block but never creates the
            // provider section by itself.
            if sessions.is_empty()
                && !probed_accounts
                    .get(&kind)
                    .is_some_and(account_creates_provider_panel)
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
            // fresh capture stamp; its shortest window's passed reset gives it
            // away), drop individual windows whose own reset has passed, then keep
            // the most-drained survivor per duration — within a live window usage
            // only climbs, so this is both stable and the truest. Same inputs
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
            let plan = account
                .and_then(|account| account.plan)
                .filter(|plan| !plan.is_empty())
                .map(|raw| format_plan_label(&kind, &raw));

            let defaults = default_provider_style(&kind);
            let style = self.theme.providers.get(&kind);
            let product_name = style
                .and_then(|style| style.product_name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or(defaults.product_name);
            let art = style
                .and_then(|style| style.ascii_art.as_deref())
                .filter(|art| !art.is_empty())
                .map(|art| art.lines().map(ToOwned::to_owned).collect())
                .unwrap_or(defaults.art);
            let (color, color_rgb, color_role) = match style.and_then(|style| style.color) {
                Some(ThemeColor::Role(role)) => (defaults.color, None, Some(role)),
                Some(color @ ThemeColor::Indexed(_)) => (color.indexed(), None, None),
                Some(color @ ThemeColor::Rgb(red, green, blue)) => {
                    (color.indexed(), Some((red, green, blue)), None)
                }
                None => (defaults.color, defaults.color_rgb, None),
            };
            let remote_control = remote_control.get(&kind).copied().unwrap_or(false);
            let spending = provider_spending.get(&kind).cloned();

            panels.push(SidebarProviderPanel {
                kind,
                product_name,
                art,
                color,
                color_rgb,
                color_role,
                version,
                plan,
                metered,
                remote_control,
                spending,
                extra_credits: None,
                reset_credits: None,
                windows,
            });
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

fn provider_kinds(
    agents: &[AgentState],
    probed_accounts: &BTreeMap<String, AgentAccount>,
) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for agent in agents {
        if agent.parent_agent_id.is_none() && !kinds.iter().any(|known| agent.kind == **known) {
            kinds.push(agent.kind.to_string());
        }
    }
    for (kind, account) in probed_accounts {
        if account_creates_provider_panel(account) && !kinds.iter().any(|known| known == kind) {
            kinds.push(kind.clone());
        }
    }
    kinds
}

fn account_creates_provider_panel(account: &AgentAccount) -> bool {
    account.plan.as_deref().is_some_and(|plan| !plan.is_empty())
        || account.metered.is_some()
        || account
            .sub_provider
            .as_deref()
            .is_some_and(|provider| !provider.is_empty())
}

fn resolve_provider_panels(
    mut panels: Vec<SidebarProviderPanel>,
    provider_list: &[String],
    max_provider_blocks: usize,
    provider_tabs: ProviderTabsMode,
) -> Vec<SidebarProviderPanel> {
    if provider_list.is_empty() {
        // Today's JSONL spend decides only *which* panels survive the cap — the
        // provider you are actively spending on always earns its block, and a
        // token-only provider (Codex) ranks on the same transcript-derived
        // footing as a live-cost one. Ties break by registry display order, so a
        // capped, stacked dashboard keeps the earlier-listed providers.
        panels.sort_by(|left, right| {
            right
                .rank_cost()
                .partial_cmp(&left.rank_cost())
                .unwrap_or(Ordering::Equal)
                .then_with(|| display_order(left, right))
        });
        // The cap bounds the *stacked* dashboard's height; a tabbed dashboard is
        // bounded by its single active block, so it shows every provider. The
        // tab decision keys on the full discovered count, so the producer and
        // the renderer's `dashboard_tabbed` agree (no truncation when tabbed
        // means the count they see is identical).
        if !provider_tabs.tabs(panels.len()) {
            panels.truncate(max_provider_blocks);
        }
        // The shown set paints in the registry's canonical order (claude, codex,
        // pi, opencode) — the panels are the dashboard's tabs, and a tab row must
        // not reorder as today's spend shifts between providers.
        panels.sort_by(display_order);
        return panels;
    }

    let explicitly_named: BTreeSet<&str> = provider_list
        .iter()
        .filter_map(|kind| (kind != "all").then_some(kind.as_str()))
        .collect();
    let by_kind: BTreeMap<String, SidebarProviderPanel> = panels
        .into_iter()
        .map(|panel| (panel.kind.clone(), panel))
        .collect();
    let mut resolved = Vec::new();
    let mut emitted_named = BTreeSet::new();
    let mut emitted_all = false;
    for kind in provider_list {
        if kind == "all" {
            if !emitted_all {
                // `all` expands the not-yet-named providers in the same registry
                // display order as the default dashboard, so `["all"]` matches an
                // empty list.
                let mut remaining: Vec<SidebarProviderPanel> = by_kind
                    .values()
                    .filter(|panel| !explicitly_named.contains(panel.kind.as_str()))
                    .cloned()
                    .collect();
                remaining.sort_by(display_order);
                resolved.extend(remaining);
                emitted_all = true;
            }
            continue;
        }
        if emitted_named.insert(kind.as_str())
            && let Some(panel) = by_kind.get(kind)
        {
            resolved.push(panel.clone());
        }
    }
    resolved
}

/// Order two panels by the registry's canonical display order — each kind's slot
/// in [`known_kinds`](crate::agents::known_kinds) (`claude, codex, pi, opencode`),
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
/// one per [`duration_mins`](RateLimitWindow::duration_mins), tagged with the
/// reading's `observed_at`/`source` for the enrich layer to fuse with cached and
/// authoritative truth.
///
/// Each `reading` is one session's whole window set. A reading is rejected
/// wholesale by [`AgentRateLimits::content_stale_at`] — an idle session
/// re-emits a days-old payload with a fresh capture stamp, so its longer windows
/// look current even though they are not; the shortest window's passed reset
/// gives the whole payload away. Among surviving readings, the most-drained value
/// wins per duration: within one live window usage only climbs, so the highest
/// reading is the most current and the pick is stable against parallel sessions
/// reporting the same budget at different instants. A surviving reading can
/// still carry a longer window from a previous epoch while its shortest window is
/// mid-cycle, so each expired window is dropped before this comparison. Output
/// sorted short→long for a stable paint order; windows of unknown duration sort
/// last.
pub(super) fn fresh_windows<'a>(
    readings: impl Iterator<Item = &'a AgentRateLimits>,
    now: Timestamp,
) -> Vec<RateLimitWindow> {
    let mut by_duration: BTreeMap<Option<u32>, RateLimitWindow> = BTreeMap::new();
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
            by_duration
                .entry(window.duration_mins)
                .and_modify(|best| {
                    if window.used_percentage > best.used_percentage {
                        *best = window.clone();
                    }
                })
                .or_insert_with(|| window.clone());
        }
    }
    let mut windows: Vec<RateLimitWindow> = by_duration.into_values().collect();
    windows.sort_by_key(|window| window.duration_mins.unwrap_or(u32::MAX));
    windows
}

/// Built-in provider style, read from the adapter's brand descriptor
/// ([`crate::agents::Brand`]); used when the per-machine config overrides none
/// of it. An unregistered kind renders title-cased with no emblem in neutral
/// grey (244).
struct ProviderStyleDefaults {
    product_name: String,
    art: Vec<String>,
    color: u8,
    color_rgb: Option<(u8, u8, u8)>,
}

fn default_provider_style(kind: &str) -> ProviderStyleDefaults {
    if let Some(descriptor) = crate::agents::descriptor_by_kind(kind) {
        return ProviderStyleDefaults {
            product_name: descriptor.display_name.to_owned(),
            art: descriptor
                .brand
                .emblem
                .trim_matches('\n')
                .lines()
                .map(ToOwned::to_owned)
                .collect(),
            color: descriptor.brand.color,
            color_rgb: Some(descriptor.brand.color_rgb),
        };
    }
    ProviderStyleDefaults {
        product_name: provider_title_case(kind),
        art: Vec::new(),
        color: 244,
        color_rgb: None,
    }
}

/// Format a raw provider plan tier into its brand label, per the adapter's
/// [`crate::agents::PlanLabel`]: Claude's tiers prefix `Claude` (`max` →
/// `Claude Max`), Codex's prefix `ChatGPT` (`pro` → `ChatGPT Pro`); any other
/// provider just title-cases the tier.
fn format_plan_label(kind: &str, raw: &str) -> String {
    let tier = provider_title_case(raw);
    match crate::agents::descriptor_by_kind(kind).map(|descriptor| &descriptor.plan_label) {
        Some(crate::agents::PlanLabel::Prefixed { prefix }) => format!("{prefix} {tier}"),
        Some(crate::agents::PlanLabel::TitleCaseOnly) | None => tier,
    }
}

/// Title-case a `-`/`_`/space-delimited token (`gpt-5` → `Gpt 5`, `max` →
/// `Max`). ASCII-oriented; a non-ASCII leading char is uppercased as Unicode.
fn provider_title_case(value: &str) -> String {
    value
        .split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
