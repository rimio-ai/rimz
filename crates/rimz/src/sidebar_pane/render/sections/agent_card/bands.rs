//! Visibility bands for an open delegation section. Live children come first in
//! spawn order, uncapped; finished children follow newest-landed first while
//! they ended inside the recent window and up to the recent cap; every other
//! finished child is older and folds behind `+K older` — unless no live or recent
//! row is visible, where the card shows the older rows directly rather than a
//! lone fold that costs a second click. The collapsed list is a
//! prefix of the expanded one, and a child that finishes moves from the end of
//! the live band to the top of the recent band.

use jiff::Timestamp;

use crate::store::snapshot::SidebarSubAgent;

use super::sub_agent_finished;
use crate::sidebar_pane::render::fmt::age_secs;

pub(super) struct DelegationBands<'a> {
    /// Every child not yet finished, in the projection's spawn order.
    pub(super) live: Vec<&'a SidebarSubAgent>,
    /// Finished children inside the window and cap, newest landed first.
    pub(super) recent: Vec<&'a SidebarSubAgent>,
    /// Every other finished child, continuing the recent band's order.
    pub(super) older: Vec<&'a SidebarSubAgent>,
}

/// Classify `children` (spawn-ordered by the projection) into bands. A finished
/// child's `last_activity` is its landed instant.
pub(super) fn delegation_bands(
    children: &[SidebarSubAgent],
    now: Timestamp,
    recent_secs: u64,
    max_recent: usize,
) -> DelegationBands<'_> {
    let (mut finished, live): (Vec<_>, Vec<_>) =
        children.iter().partition(|child| sub_agent_finished(child));
    finished.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.id.cmp(&b.id))
    });
    let window = i64::try_from(recent_secs).unwrap_or(i64::MAX);
    let recent_len = finished
        .iter()
        .take(max_recent)
        .take_while(|child| age_secs(child.last_activity, now) < window)
        .count();
    let older = finished.split_off(recent_len);
    DelegationBands {
        live,
        recent: finished,
        older,
    }
}

#[cfg(test)]
mod tests {
    use crate::agents::AgentStatus;
    use jiff::SignedDuration;

    use super::*;

    fn child(
        id: &str,
        status: AgentStatus,
        landed_secs_ago: i64,
        now: Timestamp,
    ) -> SidebarSubAgent {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": id,
            "status": status,
            "last_activity": now - SignedDuration::from_secs(landed_secs_ago),
        }))
        .expect("minimal subagent")
    }

    fn ids(children: &[&SidebarSubAgent]) -> Vec<String> {
        children.iter().map(|child| child.id.clone()).collect()
    }

    fn visible(bands: &DelegationBands<'_>) -> Vec<String> {
        ids(&bands.live)
            .into_iter()
            .chain(ids(&bands.recent))
            .collect()
    }

    fn all(bands: &DelegationBands<'_>) -> Vec<String> {
        visible(bands)
            .into_iter()
            .chain(ids(&bands.older))
            .collect()
    }

    fn roster(now: Timestamp) -> Vec<SidebarSubAgent> {
        vec![
            child("a", AgentStatus::Success, 1_200, now),
            child("b", AgentStatus::Running, 30, now),
            child("c", AgentStatus::Failed, 60, now),
            child("d", AgentStatus::Waiting, 5, now),
            child("e", AgentStatus::Success, 300, now),
            child("f", AgentStatus::Success, 120, now),
        ]
    }

    #[test]
    fn live_keeps_spawn_order_and_recent_runs_newest_first() {
        let now = Timestamp::UNIX_EPOCH + SignedDuration::from_hours(1);
        let children = roster(now);
        let bands = delegation_bands(&children, now, 900, 5);
        assert_eq!(ids(&bands.live), ["b", "d"]);
        assert_eq!(ids(&bands.recent), ["c", "f", "e"]);
        assert_eq!(ids(&bands.older), ["a"]);
    }

    #[test]
    fn window_and_cap_trim_recent_from_its_tail_only() {
        let now = Timestamp::UNIX_EPOCH + SignedDuration::from_hours(1);
        let children = roster(now);
        let wide = delegation_bands(&children, now, 3_600, 5);
        for (secs, cap) in [(900, 5), (200, 5), (3_600, 2), (0, 5), (3_600, 0)] {
            let bands = delegation_bands(&children, now, secs, cap);
            assert_eq!(all(&bands), all(&wide), "{secs}s cap {cap}");
            assert!(all(&bands).starts_with(&visible(&bands)));
            assert_eq!(ids(&bands.live), ids(&wide.live));
            assert!(ids(&wide.recent).starts_with(&ids(&bands.recent)));
        }
        assert!(delegation_bands(&children, now, 0, 5).recent.is_empty());
        assert!(delegation_bands(&children, now, 3_600, 0).recent.is_empty());
    }

    #[test]
    fn a_child_that_finishes_moves_to_the_top_of_recent_and_nothing_else_moves() {
        let now = Timestamp::UNIX_EPOCH + SignedDuration::from_hours(1);
        let mut children = roster(now);
        let before = delegation_bands(&children, now, 900, 5);
        let (before_live, before_recent) = (ids(&before.live), ids(&before.recent));
        children[1] = child("b", AgentStatus::Success, 0, now);
        let after = delegation_bands(&children, now, 900, 5);
        assert_eq!(ids(&after.live), ["d"]);
        assert_eq!(
            ids(&after.live),
            before_live
                .into_iter()
                .filter(|id| id != "b")
                .collect::<Vec<_>>()
        );
        assert_eq!(ids(&after.recent)[0], "b");
        assert_eq!(ids(&after.recent)[1..], before_recent[..]);
    }
}
