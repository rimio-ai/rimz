//! Debounced, sticky health of the refresh loop, and the give-up rule that
//! exits a renderer degraded past rescue.

use std::time::Duration;

use jiff::Timestamp;

use crate::render::Alert;

/// A single transient fetch hiccup must not flash a scary banner: the loop
/// already holds the last good frame, so absorb the first failures silently
/// and only raise an alert once a failure persists this many consecutive
/// fetches. Sustained failures still surface promptly (~one tick apart).
pub(super) const ALERT_AFTER_FAILURES: u32 = 2;

/// Debounced, sticky health of the refresh loop. `failure_streak` counts
/// consecutive failed fetches so a lone blip never alarms; `alert` is the
/// bottom-of-sidebar notice, which survives recovery (marked recovered) until
/// the user dismisses it.
#[derive(Clone, Debug, Default)]
pub struct Health {
    pub failure_streak: u32,
    pub alert: Option<Alert>,
}

/// Fold the latest fetch outcome into the debounced, sticky health.
///
/// - A failure bumps the streak and, once it crosses [`ALERT_AFTER_FAILURES`],
///   arms (or refreshes) an active alert, preserving `since` so "for Ns" grows
///   monotonically across an episode.
/// - A success resets the streak and marks any active alert recovered, leaving
///   it pinned to the bottom until the user dismisses it.
pub(super) fn next_health(previous: &Health, failure: Option<String>) -> Health {
    match failure {
        Some(reason) => {
            let failure_streak = previous.failure_streak.saturating_add(1);
            let alert = if failure_streak >= ALERT_AFTER_FAILURES {
                let since = previous
                    .alert
                    .as_ref()
                    .filter(|alert| alert.is_active())
                    .map(|alert| alert.since)
                    .unwrap_or_else(Timestamp::now);
                Some(Alert {
                    reason,
                    since,
                    recovered_at: None,
                })
            } else {
                // Below the threshold: absorb the blip, but keep any lingering
                // recovered alert from a previous episode.
                previous.alert.clone()
            };
            Health {
                failure_streak,
                alert,
            }
        }
        None => {
            let alert = previous.alert.clone().map(|mut alert| {
                if alert.is_active() {
                    alert.recovered_at = Some(Timestamp::now());
                }
                alert
            });
            Health {
                failure_streak: 0,
                alert,
            }
        }
    }
}

/// How long the refresh loop may stay continuously degraded before the renderer
/// gives up and exits. Generous so a transient mux hiccup or the sub-second gap
/// while `cargo install` swaps `rimz` never closes a healthy sidebar; short
/// enough that a genuinely broken renderer (deleted ledger, dead mux, or an old
/// build past the current runtime contract) heals on the next reload/attach
/// instead of lingering for minutes.
pub(super) const GIVE_UP_AFTER_DEGRADED: Duration = Duration::from_secs(30);

/// Whether the refresh loop has been *continuously* degraded past
/// [`GIVE_UP_AFTER_DEGRADED`]. Keys off the sticky health alert: `since` is
/// pinned to the start of the current failure episode and any successful fetch
/// clears the active state (the alert lingers only as a dim recovered notice),
/// so this fires only on an unbroken run of failures, never after a recovery.
pub(super) fn degraded_too_long(health: &Health, now: Timestamp) -> bool {
    health
        .alert
        .as_ref()
        .filter(|alert| alert.is_active())
        .is_some_and(|alert| {
            now.duration_since(alert.since).as_secs() >= GIVE_UP_AFTER_DEGRADED.as_secs() as i64
        })
}

#[cfg(test)]
mod tests;
