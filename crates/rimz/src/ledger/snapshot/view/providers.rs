use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use crate::agents::{AgentAccount, RateLimitWindow, SpendTally};
use crate::config::ThemeColor;
use crate::feed::AgentState;

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
    /// name) resolves from `self.sidebar.providers` over the built-in defaults, so
    /// the renderer gets a ready-to-paint block. With no explicit
    /// `provider_list`, the set is capped to `max_provider_blocks` by today's
    /// spend, then ordered stably by kind — the panels are the dashboard's tabs,
    /// so the row never reorders as spend shifts. An explicit `provider_list`
    /// supplies the shown set and order, with `all` expanding the remaining
    /// discovered providers and bypassing the cap. Producer-only: the pure
    /// reducer leaves `providers` empty.
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
            // between ticks and the bar flickers. Instead, pick each window stably
            // across every session, grouped by duration — drop readings whose reset
            // already passed (stale), then keep the most-drained survivor (most
            // conservative). Same inputs always yield the same bars, regardless of
            // which session reported last. A provider whose descriptor declares no
            // rate-limit windows renders the absence deliberately — its panel
            // never grows budget bars even if a stray reading lands in a session
            // context; an unregistered kind keeps whatever it reports.
            let now = self.now;
            let windows_for = |of_kind: &str| {
                stable_windows(
                    self.agents
                        .iter()
                        .filter(|agent| agent.parent_agent_id.is_none() && agent.kind == *of_kind)
                        .filter_map(|agent| agent.context.as_ref()?.rate_limits.as_ref())
                        .flat_map(|limits| limits.windows.iter().cloned()),
                    now,
                )
            };
            let declares_windows = crate::agents::descriptor_by_kind(&kind)
                .is_none_or(|descriptor| descriptor.capabilities.rate_limit_windows);
            let windows = if declares_windows {
                windows_for(&kind)
            } else {
                // A provider with no window surface of its own (Pi) running on a
                // metered sibling subscription shares that account's budget, so
                // its block borrows the sibling kind's windows — same account,
                // same bars. No metered sub, no mapped sibling, or no readings
                // → bar-less, exactly as before.
                account
                    .as_ref()
                    .filter(|account| account.metered == Some(true))
                    .and_then(|account| account.sub_provider.as_deref())
                    .and_then(crate::agents::kind_for_sub_provider)
                    .map(windows_for)
                    .unwrap_or_default()
            };
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
            let style = self.sidebar.providers.get(&kind);
            let product_name = style
                .and_then(|style| style.product_name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or(defaults.product_name);
            let art = style
                .and_then(|style| style.ascii_art.as_deref())
                .filter(|art| !art.is_empty())
                .map(|art| art.lines().map(ToOwned::to_owned).collect())
                .unwrap_or(defaults.art);
            let (color, color_rgb) = match style.and_then(|style| style.color) {
                Some(color @ ThemeColor::Indexed(_)) => (color.indexed(), None),
                Some(color @ ThemeColor::Rgb(red, green, blue)) => {
                    (color.indexed(), Some((red, green, blue)))
                }
                None => (defaults.color, defaults.color_rgb),
            };
            let remote_control = remote_control.get(&kind).copied().unwrap_or(false);
            let spending = provider_spending.get(&kind).cloned();

            panels.push(SidebarProviderPanel {
                kind,
                product_name,
                art,
                color,
                color_rgb,
                version,
                plan,
                metered,
                remote_control,
                spending,
                windows,
            });
        }

        self.providers = resolve_provider_panels(
            panels,
            &self.sidebar.provider_list,
            self.sidebar.max_provider_blocks,
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
) -> Vec<SidebarProviderPanel> {
    if provider_list.is_empty() {
        // Today's JSONL spend decides only *which* panels survive the cap — the
        // provider you are actively spending on always earns its block, and a
        // token-only provider (Codex) ranks on the same transcript-derived
        // footing as a live-cost one. The retained set then orders stably by
        // kind: the panels are the dashboard's tabs, and a tab row must not
        // reorder as today's spend shifts between providers.
        panels.sort_by(|left, right| {
            right
                .rank_cost()
                .partial_cmp(&left.rank_cost())
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.kind.cmp(&right.kind))
        });
        panels.truncate(max_provider_blocks);
        panels.sort_by(|left, right| left.kind.cmp(&right.kind));
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
                resolved.extend(by_kind.iter().filter_map(|(kind, panel)| {
                    (!explicitly_named.contains(kind.as_str())).then_some(panel.clone())
                }));
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

/// The account-stable *set* of budget windows across every session of a
/// provider, grouped by [`duration_mins`](RateLimitWindow::duration_mins).
/// Readings of the same duration run through [`stable_window`] independently, so
/// two sessions reporting one budget at different instants converge to a single
/// bar per duration. Output sorted short→long for a stable paint order; windows
/// of unknown duration sort last.
pub(super) fn stable_windows(
    windows: impl Iterator<Item = RateLimitWindow>,
    now: Timestamp,
) -> Vec<RateLimitWindow> {
    let mut groups: BTreeMap<Option<u32>, Vec<RateLimitWindow>> = BTreeMap::new();
    for window in windows {
        groups.entry(window.duration_mins).or_default().push(window);
    }
    let mut stable: Vec<RateLimitWindow> = groups
        .into_values()
        .filter_map(|group| stable_window(group.into_iter(), now))
        .collect();
    stable.sort_by_key(|window| window.duration_mins.unwrap_or(u32::MAX));
    stable
}

/// The account-stable reading of one rate-limit window (one duration) across
/// every session of a provider. Parallel sessions report the same shared budget
/// at different instants, so a "freshest wins" pick flickers; this is
/// deterministic instead.
///
/// Drop any reading whose `resets_at` has already passed — that window reset, so
/// its `used_percentage` is stale — then, among the survivors, keep the most
/// drained (highest `used_percentage`, so the bar never over-promises remaining
/// budget). A window with no reset instant can't be aged out, so it is kept as a
/// last-resort reading only when nothing with a live reset survives.
pub(super) fn stable_window(
    windows: impl Iterator<Item = RateLimitWindow>,
    now: Timestamp,
) -> Option<RateLimitWindow> {
    let mut live: Option<RateLimitWindow> = None;
    let mut undated: Option<RateLimitWindow> = None;
    for window in windows {
        if window.used_percentage.is_none() {
            continue;
        }
        match window.resets_at {
            Some(resets_at) if resets_at <= now => continue, // reset already passed — stale
            Some(_) => {
                if live
                    .as_ref()
                    .is_none_or(|best| window.used_percentage > best.used_percentage)
                {
                    live = Some(window);
                }
            }
            None => {
                if undated
                    .as_ref()
                    .is_none_or(|best| window.used_percentage > best.used_percentage)
                {
                    undated = Some(window);
                }
            }
        }
    }
    live.or(undated)
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
