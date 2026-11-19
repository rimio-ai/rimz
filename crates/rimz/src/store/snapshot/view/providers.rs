use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use crate::agents::AgentState;
use crate::agents::context::RateLimitWindowKey;
use crate::agents::{AgentAccount, AgentRateLimits, RateLimitWindow, SpendTally};
use crate::config::ProviderTabsMode;
use crate::ids::LoginKey;
use crate::theme::{
    BrandColor, provider_title_case, resolve_provider_brand, resolve_provider_identity,
};

use super::{ProviderPlan, RemoteControlBadge, SidebarProviderPanel, SidebarSnapshot};

impl SidebarSnapshot {
    /// Fold each login in `logins` into its dashboard block; a live agent or a
    /// probed account on any other login earns none. The caller names the set:
    /// the room's current login per kind for display, every in-use login for
    /// the producer's cache writes. Reported Pi backing credentials become
    /// alternates inside the same block, with whole-login stats and spend.
    /// A login in the set needs a session on it or a substantive probed account; an account-only block needs a metered login, a non-empty identity, or recorded spend, so the dashboard shows substantive accounts and budgets between turns.
    /// Sums each login's spend, tokens, and edited lines. Plan, metering,
    /// account id and version come only from `probed_accounts`, the login's
    /// fused record in `accounts.json` that every reading (probe, live session,
    /// usage read) merges into; no live context is read here, and windows stay
    /// empty until the rate-limit cache projects them (the producer gathers
    /// live windows with [`SidebarSnapshot::gather_live_windows`] first). A
    /// kind whose only signal is a qualifying probed account still earns a block;
    /// `remote_control` carries the per-kind `⇅ rc` visibility and managed-server
    /// health. Styling (emblem, color, name) and the definition-declared empty
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
    /// bypassing the cap. Ordering, cap, and tabs see only the set's blocks.
    /// The machine-config fold calls this on producer and consumer alike; the
    /// pure reducer leaves `providers` empty.
    pub(crate) fn with_provider_aggregates(
        mut self,
        accounts: &crate::config::AccountsConfig,
        probed_accounts: &BTreeMap<LoginKey, AgentAccount>,
        remote_control: &BTreeMap<String, RemoteControlBadge>,
        provider_spending: &BTreeMap<LoginKey, SpendTally>,
        logins: &BTreeSet<LoginKey>,
    ) -> Self {
        let mut panels = Vec::new();
        for key in logins.iter().cloned() {
            let kind = key.kind.to_string();
            let sessions: Vec<&AgentState> = self
                .agents
                .iter()
                .filter(|agent| !agent.is_provider_subagent() && agent.login_key() == key)
                .collect();
            let active_sessions = u32::try_from(
                self.agent_panes
                    .iter()
                    .filter(|pane| {
                        pane.kind.as_str() == kind
                            && sessions
                                .iter()
                                .any(|agent| pane.agent_id.as_ref() == Some(&agent.agent_id))
                    })
                    .map(|pane| pane.pane_id.raw())
                    .collect::<BTreeSet<_>>()
                    .len(),
            )
            .unwrap_or(u32::MAX);
            // Nothing to show without a session or a substantive logged-in
            // account. Recorded spend qualifies a probed account but never
            // creates the provider section for a logged-out provider by itself.
            if sessions.is_empty()
                && !probed_accounts.get(&key).is_some_and(|account| {
                    account_creates_provider_panel(account, provider_spending.get(&key))
                })
            {
                continue;
            }

            let account = probed_accounts.get(&key).cloned();
            let version = account.as_ref().and_then(|account| account.version.clone());
            let sub_provider = account
                .as_ref()
                .and_then(|account| account.sub_provider.clone());
            let undecided = account
                .as_ref()
                .is_some_and(|account| account.is_undecided());

            // A delegated login's sessions may run on other credentials, so
            // only the record names the selected one's owner there.
            let account_key = match account.as_ref() {
                Some(account) if !account.credentials.is_empty() => account.account_key.clone(),
                _ => sessions
                    .iter()
                    .filter(|agent| agent.account_key.is_some())
                    .max_by_key(|agent| (agent.registered_at, agent.agent_id.clone()))
                    .and_then(|agent| agent.account_key.clone()),
            };
            let metered = !undecided
                && account
                    .as_ref()
                    .and_then(|account| account.metered)
                    .unwrap_or(account_key.is_some());
            let account_scope = account
                .as_ref()
                .map(|account| account.scope.clone())
                .unwrap_or_default();
            let plan = account
                .as_ref()
                .and_then(|account| account.plan.clone())
                .filter(|plan| !plan.is_empty())
                .map(|tier| ProviderPlan {
                    tier,
                    name: account
                        .as_ref()
                        .and_then(|account| account.plan_name.clone()),
                });

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
            let window_placeholders = crate::agents::spec_by_kind(&kind)
                .map(|definition| {
                    definition
                        .expected_windows
                        .iter()
                        .map(|&label| label.to_owned())
                        .collect()
                })
                .unwrap_or_default();
            let tally = provider_spending.get(&key);
            let spending = tally.cloned();
            let rank = ProviderRank {
                live: !sessions.is_empty(),
                week_sessions: tally.map_or(0, |tally| tally.week.sessions),
                month_sessions: tally.map_or(0, |tally| tally.month.sessions),
                year_sessions: tally.map_or(0, |tally| tally.year.sessions),
                logged_in: probed_accounts
                    .get(&key)
                    .is_some_and(|account| account_creates_provider_panel(account, tally)),
                credentials_updated_at_ms: probed_accounts
                    .get(&key)
                    .and_then(|account| account.credentials_updated_at_ms),
            };

            let mut panel = SidebarProviderPanel {
                kind,
                account: key.name.clone(),
                sub_provider,
                account_scope,
                entitlement: Default::default(),
                account_key,
                product_name: accounts.provider_title(&identity.product_name, &key),
                art: identity.art,
                art_tints: identity.art_tints,
                color,
                color_rgb,
                color_role,
                version,
                plan,
                metered,
                undecided,
                alternates: Vec::new(),
                remote_control,
                active_sessions,
                spending: spending.map(std::sync::Arc::new),
                day_budget: None,
                month_budget: None,
                extra_credits: None,
                reset_credits: None,
                redeem_forecast: None,
                window_placeholders,
                windows: Vec::new(),
            };
            // The record lists the credentials live sessions run on beside the
            // selected one (`ProviderRecord::display_account`); each is an
            // alternate.
            let providers = account
                .as_ref()
                .into_iter()
                .flat_map(|account| account.credentials.keys())
                .filter(|provider| Some(provider.as_str()) != panel.sub_provider.as_deref());
            panel.alternates = providers
                .map(|provider| {
                    let credential = account
                        .as_ref()
                        .and_then(|account| account.credentials.get(provider));
                    let mut alternate = panel.clone();
                    alternate.sub_provider = Some(provider.to_owned());
                    alternate.account_scope = credential
                        .map(|account| account.scope.clone())
                        .unwrap_or_default();
                    alternate.account_key =
                        credential.and_then(|account| account.account_key.clone());
                    alternate.plan = credential
                        .and_then(|account| account.plan.as_deref())
                        .filter(|plan| !plan.is_empty())
                        .map(|tier| ProviderPlan {
                            tier: tier.to_owned(),
                            name: credential.and_then(|account| account.plan_name.clone()),
                        });
                    alternate.metered = credential
                        .and_then(|account| account.metered)
                        .unwrap_or(false);
                    alternate.undecided = false;
                    alternate
                })
                .collect();
            panels.push((panel, rank));
        }

        self.providers = resolve_provider_panels(
            panels,
            &self.theme.display.provider_list,
            self.theme.display.max_provider_blocks,
            self.theme.display.provider_tabs,
        );
        self
    }

    /// Gather each panel's live rate-limit windows from the sessions on its
    /// login that speak for its backing credential and birth account, and
    /// each metered alternate's from the sessions on that sub-provider. Only
    /// the producer calls this, right before it fuses the windows into the
    /// rate-limit cache, so the display sees windows through that cache alone.
    /// Content-stale readings and expired windows are rejected, then the
    /// newest content per identity wins, keeping highest usage within a
    /// 10-second render-lag band; unchanged statusline windows keep their
    /// content stamps, so an idle pre-credit high cannot beat a fresh
    /// post-credit low. A window observed before its login's identity floor
    /// in `floors` describes the previous account and is dropped before that
    /// ranking, so it cannot outrank a newer one. An undecided panel gathers
    /// none.
    pub(crate) fn gather_live_windows(&mut self, floors: &BTreeMap<LoginKey, Timestamp>) {
        let now = self.now;
        let agents = &self.agents;
        for panel in &mut self.providers {
            let key = panel.login_key();
            let floor = floors.get(&key).copied();
            let sessions: Vec<&AgentState> = agents
                .iter()
                .filter(|agent| !agent.is_provider_subagent() && agent.login_key() == key)
                .collect();
            let sub_provider_is = |agent: &AgentState, provider: Option<&str>| {
                agent
                    .context
                    .as_ref()
                    .and_then(|context| context.sub_provider.as_deref())
                    == provider
            };
            if !panel.undecided {
                let credential_sessions: Vec<_> = sessions
                    .iter()
                    .copied()
                    .filter(|agent| {
                        sub_provider_is(agent, None)
                            || sub_provider_is(agent, panel.sub_provider.as_deref())
                    })
                    .collect();
                panel.windows = account_windows(
                    &credential_sessions,
                    panel.account_key.as_deref(),
                    floor,
                    now,
                );
            }
            for alternate in panel.alternates.iter_mut().filter(|alt| alt.metered) {
                let sessions: Vec<_> = sessions
                    .iter()
                    .copied()
                    .filter(|agent| sub_provider_is(agent, alternate.sub_provider.as_deref()))
                    .collect();
                alternate.windows =
                    account_windows(&sessions, alternate.account_key.as_deref(), floor, now);
            }
        }
    }
}

fn account_windows(
    sessions: &[&AgentState],
    account_key: Option<&str>,
    floor: Option<Timestamp>,
    now: Timestamp,
) -> Vec<RateLimitWindow> {
    let readings: Vec<AgentRateLimits> = sessions
        .iter()
        .filter(|agent| {
            account_key.is_none_or(|panel_key| {
                agent
                    .account_key
                    .as_deref()
                    .is_none_or(|key| key == panel_key)
            })
        })
        .filter_map(|agent| agent.context.as_ref()?.rate_limits.as_ref())
        .map(|limits| AgentRateLimits {
            windows: limits
                .windows
                .iter()
                .filter(|window| {
                    floor
                        .zip(window.observed_at)
                        .is_none_or(|(floor, observed)| observed >= floor)
                })
                .cloned()
                .collect(),
        })
        .collect();
    fresh_windows(readings.iter(), now)
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
    let by_login: BTreeMap<LoginKey, (SidebarProviderPanel, ProviderRank)> = panels
        .into_iter()
        .map(|entry| (entry.0.login_key(), entry))
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
                let mut remaining: Vec<(SidebarProviderPanel, ProviderRank)> = by_login
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
        if emitted_named.insert(kind.as_str()) {
            let mut named: Vec<_> = by_login
                .values()
                .filter(|(panel, _)| &panel.kind == kind)
                .map(|(panel, _)| panel.clone())
                .collect();
            named.sort_by(display_order);
            resolved.extend(named);
        }
    }
    resolved
}

/// Order two panels by the registry's canonical display order — each kind's slot
/// in [`known_kinds`](crate::agents::known_kinds),
/// an unregistered kind sorting last by name. The default dashboard and `all`
/// expansion both use this, so the row reads in the canonical agent order rather
/// than an alphabetical accident; an explicit `provider_list` overrides it.
fn display_order(left: &SidebarProviderPanel, right: &SidebarProviderPanel) -> Ordering {
    display_rank(&left.kind)
        .cmp(&display_rank(&right.kind))
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| right.account.is_default().cmp(&left.account.is_default()))
        .then_with(|| left.account.cmp(&right.account))
}

/// A kind's position in the registry's display order; unregistered kinds sort
/// last.
fn display_rank(kind: &str) -> usize {
    crate::agents::known_kinds()
        .position(|known| known == kind)
        .unwrap_or(usize::MAX)
}

/// How far behind the newest content stamp a window may sit and still tie on
/// usage: parallel sessions' render and spawn lag, not a reset's spread.
const LIVE_RENDER_LAG_BAND: jiff::SignedDuration = jiff::SignedDuration::from_secs(10);

/// The content-fresh live budget windows across every session of a provider,
/// one per duration or provider-defined scope, tagged with the
/// reading's `observed_at`/`source` for the enrich layer to fuse with cached and
/// authoritative truth.
///
/// Each `reading` is one session's whole window set. A reading is rejected
/// wholesale by [`AgentRateLimits::content_stale_at`], and each expired window
/// or window without usage is dropped. Per identity, highest usage wins among
/// content stamps within 10 seconds of the newest. Unstamped windows count as
/// oldest; when all are unstamped, highest usage wins. Equal usage prefers newer
/// provenance, then deterministic metadata ties, independent of session order.
/// Output keeps duration windows short→long, followed by scoped windows in
/// stable scope-id order.
pub(super) fn fresh_windows<'a>(
    readings: impl Iterator<Item = &'a AgentRateLimits>,
    now: Timestamp,
) -> Vec<RateLimitWindow> {
    let mut by_window: BTreeMap<RateLimitWindowKey, Vec<&RateLimitWindow>> = BTreeMap::new();
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
            by_window.entry(window.key()).or_default().push(window);
        }
    }
    let mut windows: Vec<RateLimitWindow> = by_window
        .into_values()
        .filter_map(|windows| {
            let newest = windows.iter().filter_map(|window| window.observed_at).max();
            windows
                .into_iter()
                .filter(|window| {
                    newest.is_none_or(|newest| {
                        window
                            .observed_at
                            .is_some_and(|at| newest.duration_since(at) <= LIVE_RENDER_LAG_BAND)
                    })
                })
                .max_by_key(|window| {
                    (
                        window.used_percentage,
                        window.observed_at,
                        window.source.is_authoritative(),
                        window.resets_at,
                        window.lifted,
                        window.duration_mins,
                        window.scope.as_ref().map(|scope| scope.label.as_str()),
                    )
                })
                .cloned()
        })
        .collect();
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

/// Format a provider display name or raw tier per the adapter's
/// [`crate::agents::definition::PlanLabel`]. A name wins verbatim (`Ultra` → `Claude Ultra`);
/// without one, the tier vocabulary applies (`pro` → `ChatGPT Pro 200`). Unknown kinds just
/// title-case the tier.
pub(super) fn format_plan_label(kind: &str, raw: &str, name: Option<&str>) -> String {
    match crate::agents::spec_by_kind(kind).map(|definition| &definition.plan_label) {
        Some(label) => label.format(raw, name),
        None => provider_title_case(raw),
    }
}
