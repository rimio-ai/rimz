//! The provider panel's budget reading: which bars a panel shows, in draw
//! order, and what each window's bar reads at the snapshot clock. It is
//! derived per call from the panel, never stored, since windows and credits
//! are rewritten after the fold.

use jiff::Timestamp;

use crate::agents::context::PaceReading;
use crate::agents::{Entitlement, ExtraCredits, RateLimitWindow};

use super::SidebarProviderPanel;

/// A panel's budget rows, in precedence order of the variants.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProviderBudget<'a> {
    /// No backing credential is known yet: no budget row.
    Undecided,
    /// The plan lapsed; `since` is `None` when the stamp is unrepresentable.
    Lapsed { since: Option<Timestamp> },
    /// An unmetered account: one `api` credit row.
    Api(CreditDisplay),
    /// A metered account whose windows have not arrived: one unknown row per
    /// definition-declared label, or one blank-label row when there are none.
    Pending(&'a [String]),
    /// The metered rows in draw order; empty when every window is a sub-cap.
    Bars(Vec<ProviderBar<'a>>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProviderBar<'a> {
    /// A window's row; the reference serves its label.
    Window {
        window: &'a RateLimitWindow,
        reading: WindowReading,
    },
    /// The `ex` paid-usage row.
    Extra(CreditDisplay),
}

/// What one window's bar reads, resolved in variant order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum WindowReading {
    /// An unlimited window: a full `∞` bar without ticks.
    Lifted,
    /// No usage reading and no spent longer window: an unknown track.
    Unknown,
    Draining {
        /// Remaining percent; 0 when a longer window is spent.
        remaining: u8,
        /// The countdown target; `None` when forced spent or not started.
        resets_at: Option<Timestamp>,
        /// Burn pace, only beside a countdown.
        pace: Option<PaceReading>,
        /// Remaining percents of the model sub-caps partitioning this window,
        /// in panel order.
        sub_caps: Vec<u8>,
        /// The absolute-amount ceiling a longer window sets, in percent.
        ceiling: Option<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CreditDisplay {
    Off,
    Missing,
    Unknown,
    Unlimited,
    Balance(f64),
    Drain { left: f64, percentage: u8 },
}

impl CreditDisplay {
    pub(super) fn classify(credits: Option<&ExtraCredits>) -> Self {
        let credits = match credits {
            Some(ExtraCredits::Unlimited) => return Self::Unlimited,
            Some(credits) => credits,
            None => return Self::Missing,
        };
        if credits.is_disabled() {
            return Self::Off;
        }
        if let Some(left) = credits.remaining_usd_left() {
            return match credits.remaining_percentage() {
                Some(percentage) => Self::Drain {
                    left,
                    percentage: percentage.max(u8::from(left > 0.0)),
                },
                None => Self::Balance(left),
            };
        }
        if credits.limit_usd().is_some() {
            Self::Unknown
        } else {
            // Preserve the legacy uncapped reading, including Claude/Kimi's
            // "figures unreported" Known values.
            Self::Unlimited
        }
    }

    /// Whether paid extra usage can still be spent: unlimited, or dollars left
    /// above zero. Only available credit takes a metered row from a window.
    pub(super) fn is_available(self) -> bool {
        match self {
            Self::Unlimited => true,
            Self::Balance(left) | Self::Drain { left, .. } => left > 0.0,
            Self::Off | Self::Missing | Self::Unknown => false,
        }
    }
}

impl SidebarProviderPanel {
    /// The budget bars this panel shows at `now`, the snapshot clock. A metered
    /// account drains bars for the two shortest windows, or every named quota;
    /// a spent longer cap stands alone, or leads available extra credit.
    /// Model-scoped sub-caps partition their parent's bar without a row, and
    /// lifted windows yield while an enforced candidate remains.
    pub(crate) fn budget(&self, now: Timestamp) -> ProviderBudget<'_> {
        if self.undecided {
            return ProviderBudget::Undecided;
        }
        if let Entitlement::Lapsed { since_ms } = self.entitlement {
            let since = i64::try_from(since_ms)
                .ok()
                .and_then(|millis| Timestamp::from_millisecond(millis).ok());
            return ProviderBudget::Lapsed { since };
        }
        let credits = CreditDisplay::classify(self.extra_credits.as_ref());
        if !self.metered {
            return ProviderBudget::Api(credits);
        }
        if self.windows.is_empty() {
            return ProviderBudget::Pending(&self.window_placeholders);
        }
        let (sub_caps, mut windows): (Vec<_>, Vec<_>) = self
            .windows
            .iter()
            .partition(|window| window.scope.is_some() && window.duration_mins.is_some());
        if windows.iter().any(|window| !window.lifted) {
            windows.retain(|window| !window.lifted);
        }
        let bars = select_provider_bars(credits, &windows, |window| {
            window_reading(
                window,
                &sub_caps,
                longer_window_limit(self, window, now),
                now,
            )
        });
        ProviderBudget::Bars(bars)
    }
}

/// The rows in draw order. Named quotas each keep a row; otherwise the two
/// shortest windows, unless a window longer than the first is spent: the
/// longest spent one then shows alone, or leads the `ex` row when credit is
/// available.
fn select_provider_bars<'a>(
    credits: CreditDisplay,
    windows: &[&'a RateLimitWindow],
    read: impl Fn(&RateLimitWindow) -> WindowReading,
) -> Vec<ProviderBar<'a>> {
    let bar = |window: &'a RateLimitWindow| ProviderBar::Window {
        window,
        reading: read(window),
    };
    if windows.iter().any(|window| window.scope.is_some()) {
        return windows.iter().copied().map(bar).collect();
    }
    let Some(&first) = windows.first() else {
        return Vec::new();
    };
    let second = windows.get(1).copied().unwrap_or(first);

    if let Some(longest_spent) = windows
        .iter()
        .copied()
        .filter(|window| window.is_spent())
        .max_by_key(|window| window.duration_mins.unwrap_or(0))
    {
        if credits.is_available() {
            return vec![bar(longest_spent), ProviderBar::Extra(credits)];
        }
        if !std::ptr::eq(first, longest_spent) {
            // A spent longer window forces every shorter one spent with no
            // countdown, so their rows would only repeat it.
            return vec![bar(longest_spent)];
        }
    }
    provider_window_pair(first, second, bar)
}

fn provider_window_pair<'a>(
    left: &'a RateLimitWindow,
    right: &'a RateLimitWindow,
    bar: impl Fn(&'a RateLimitWindow) -> ProviderBar<'a>,
) -> Vec<ProviderBar<'a>> {
    if std::ptr::eq(left, right) {
        vec![bar(left)]
    } else {
        vec![bar(left), bar(right)]
    }
}

#[derive(Clone, Copy)]
enum LongerWindowLimit {
    None,
    Spent,
    Ceiling(u8),
}

/// What the panel's longer unscoped windows impose on `window`: any spent one
/// forces it spent, else the tightest absolute remainder below the shown one
/// sets a ceiling. Reads every panel window, lifted and unknown ones included,
/// not only the bar candidates.
fn longer_window_limit(
    panel: &SidebarProviderPanel,
    window: &RateLimitWindow,
    now: Timestamp,
) -> LongerWindowLimit {
    if window.scope.is_some() {
        return LongerWindowLimit::None;
    }
    let Some(mins) = window.duration_mins.filter(|mins| *mins > 0) else {
        return LongerWindowLimit::None;
    };
    let ceiling_axis = window
        .amount
        .zip(window.remaining_percentage(now))
        .filter(|_| !window.lifted);
    let mut ceiling_remaining: Option<u64> = None;
    for other in panel.windows.iter().filter(|other| {
        other.scope.is_none()
            && other
                .duration_mins
                .is_some_and(|other_mins| other_mins > mins)
    }) {
        if other.is_spent() {
            return LongerWindowLimit::Spent;
        }
        let Some((short, shown_remaining)) = ceiling_axis else {
            continue;
        };
        let Some(long) = other
            .amount
            .filter(|_| !other.lifted && other.used_percentage.is_some())
        else {
            continue;
        };
        let remaining = long.limit.saturating_sub(long.used);
        if u128::from(remaining) * 100 < u128::from(shown_remaining) * u128::from(short.limit) {
            ceiling_remaining =
                Some(ceiling_remaining.map_or(remaining, |prior| prior.min(remaining)));
        }
    }
    match (ceiling_remaining, ceiling_axis) {
        (Some(remaining), Some((short, _))) => LongerWindowLimit::Ceiling(
            (u128::from(remaining) * 100 / u128::from(short.limit)).max(1) as u8,
        ),
        _ => LongerWindowLimit::None,
    }
}

/// A window that has not started reads full without a countdown: these
/// sliding windows begin counting only on the first token, so the provider
/// keeps `resets_at` a full window ahead until then.
fn window_reading(
    window: &RateLimitWindow,
    sub_caps: &[&RateLimitWindow],
    longer: LongerWindowLimit,
    now: Timestamp,
) -> WindowReading {
    let forced_spent = matches!(longer, LongerWindowLimit::Spent);
    if window.lifted {
        return WindowReading::Lifted;
    }
    if !forced_spent && window.used_percentage.is_none() {
        return WindowReading::Unknown;
    }
    let remaining = if forced_spent {
        0
    } else {
        window.remaining_percentage(now).unwrap_or(0)
    };
    let resets_at = window
        .resets_at
        .filter(|_| !forced_spent && !window.not_started(now));
    WindowReading::Draining {
        remaining,
        resets_at,
        pace: resets_at.and_then(|_| window.pace(now)),
        sub_caps: sub_caps
            .iter()
            .filter(|sub| sub.sub_cap_of(window))
            .filter_map(|sub| Some(100u8.saturating_sub(sub.used_percentage?)))
            .collect(),
        ceiling: match longer {
            LongerWindowLimit::Ceiling(remaining_pct) => Some(remaining_pct),
            LongerWindowLimit::None | LongerWindowLimit::Spent => None,
        },
    }
}
