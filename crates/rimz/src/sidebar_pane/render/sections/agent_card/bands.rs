//! The paging rule of an open delegation section. Every in-flight entry lists, unpaged:
//! a subagent of the current turn without a verdict (a stalled one and one parked on
//! background work included) in spawn order, a launched entry with something alive in
//! launch order, and a booking not finished in its draw order. Every other subagent,
//! launched entry and booking is finished: none draws until the section's
//! `▸ +N earlier` line is activated, and each activation shows [`PAGE_SIZE`] more,
//! newest finish first across the groups, ties kept in group order.

use jiff::Timestamp;

use crate::store::snapshot::{SidebarBooking, SidebarLaunched, SidebarSubAgent};

use super::sub_agent_finished;

/// The finished entries one activation of the section's earlier line shows.
const PAGE_SIZE: usize = 5;

/// A finished entry of a delegation section.
pub(super) enum Finished<'a> {
    SubAgent(&'a SidebarSubAgent),
    Launched(&'a SidebarLaunched),
    Booking(&'a SidebarBooking),
}

impl Finished<'_> {
    /// When the entry finished, `None` for one finished with no end stamp yet: a
    /// subagent's verdict instant, else its rest instant, else its last activity, a
    /// launched entry's last seat's end, and a booking's end stamp.
    fn finished_at(&self) -> Option<Timestamp> {
        match self {
            Self::SubAgent(child) => Some(
                child
                    .finished_at
                    .or(child.rested_at)
                    .unwrap_or(child.last_activity),
            ),
            Self::Launched(launched) => launched.ended_at,
            Self::Booking(booking) => super::bookings::fire(booking)?.ended_at,
        }
    }
}

pub(super) struct DelegationEntries<'a> {
    /// The in-flight subagents, in the projection's spawn order.
    pub(super) children: Vec<&'a SidebarSubAgent>,
    /// The in-flight launched entries, in launch order.
    pub(super) launched: Vec<&'a SidebarLaunched>,
    /// The in-flight bookings, in the card's draw order.
    pub(super) bookings: Vec<&'a SidebarBooking>,
    /// The finished entries the section's pages show, newest finish first.
    pub(super) shown: Vec<Finished<'a>>,
    /// The finished entries behind the earlier line.
    pub(super) hidden: usize,
}

fn finished_child(child: &SidebarSubAgent) -> bool {
    child.prior_turn || sub_agent_finished(child)
}

/// Whether a card's `children`, `launched` or `bookings` hold a finished entry.
pub(super) fn has_finished(
    children: &[SidebarSubAgent],
    launched: &[SidebarLaunched],
    bookings: &[SidebarBooking],
) -> bool {
    children.iter().any(finished_child)
        || launched.iter().any(SidebarLaunched::is_finished)
        || bookings.iter().any(SidebarBooking::is_finished)
}

/// A card's delegation entries over its `children`, `launched` and `bookings`, with
/// `pages` pages of finished entries shown.
pub(super) fn delegation_entries<'a>(
    children: &'a [SidebarSubAgent],
    launched: &'a [SidebarLaunched],
    bookings: &'a [SidebarBooking],
    pages: usize,
) -> DelegationEntries<'a> {
    let (finished_children, children): (Vec<_>, Vec<_>) =
        children.iter().partition(|child| finished_child(child));
    let (finished_launched, launched): (Vec<_>, Vec<_>) =
        launched.iter().partition(|launched| launched.is_finished());
    let (finished_bookings, bookings): (Vec<_>, Vec<_>) =
        bookings.iter().partition(|booking| booking.is_finished());
    let mut finished: Vec<Finished<'_>> = finished_children
        .into_iter()
        .map(Finished::SubAgent)
        .chain(finished_launched.into_iter().map(Finished::Launched))
        .chain(finished_bookings.into_iter().map(Finished::Booking))
        .collect();
    finished.sort_by_key(|entry| {
        let at = entry.finished_at();
        std::cmp::Reverse((at.is_none(), at))
    });
    let hidden = finished
        .len()
        .saturating_sub(pages.saturating_mul(PAGE_SIZE));
    finished.truncate(finished.len() - hidden);
    DelegationEntries {
        children,
        launched,
        bookings,
        shown: finished,
        hidden,
    }
}

#[cfg(test)]
mod tests {
    use crate::agents::AgentStatus;
    use jiff::{SignedDuration, Timestamp};

    use super::*;

    fn now() -> Timestamp {
        Timestamp::UNIX_EPOCH + SignedDuration::from_hours(1)
    }

    fn child(id: &str, status: AgentStatus, landed_secs_ago: i64) -> SidebarSubAgent {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "kind": "claude",
            "name": id,
            "status": status,
            "last_activity": now() - SignedDuration::from_secs(landed_secs_ago),
        }))
        .expect("minimal subagent")
    }

    /// A booking whose fire ended `ended_secs_ago` (no end stamp when `None`), finished
    /// when `finished`.
    fn booking(task: &str, ended_secs_ago: Option<i64>, finished: bool) -> SidebarBooking {
        serde_json::from_value(serde_json::json!({
            "task": task,
            "starts": {"kind": "profile", "name": "coder"},
            "band": "fired",
            "state": {
                "state": "fired",
                "at": now() - SignedDuration::from_hours(1),
                "ended_at": ended_secs_ago.map(|ago| now() - SignedDuration::from_secs(ago)),
                "pr": {"pr": "unread"},
                "finished": finished,
            },
        }))
        .expect("minimal booking")
    }

    fn ids(children: &[&SidebarSubAgent]) -> Vec<String> {
        children.iter().map(|child| child.id.clone()).collect()
    }

    fn shown(entries: &DelegationEntries<'_>) -> Vec<String> {
        entries
            .shown
            .iter()
            .map(|entry| match entry {
                Finished::SubAgent(child) => child.id.clone(),
                Finished::Launched(launched) => launched.launches[0].clone(),
                Finished::Booking(booking) => booking.task.clone(),
            })
            .collect()
    }

    fn roster() -> Vec<SidebarSubAgent> {
        vec![
            child("a", AgentStatus::Success, 1_200),
            child("b", AgentStatus::Running, 30),
            child("c", AgentStatus::Failed, 60),
            child("d", AgentStatus::Waiting, 5),
            child("e", AgentStatus::Success, 300),
            child("f", AgentStatus::Success, 120),
        ]
    }

    #[test]
    fn finish_order_reads_finished_at_before_later_activity() {
        let mut wire = serde_json::to_value(child("early", AgentStatus::Success, 0)).unwrap();
        wire["finished_at"] = serde_json::json!(now() - SignedDuration::from_secs(1_200));
        let children = vec![
            serde_json::from_value(wire).unwrap(),
            child("later", AgentStatus::Success, 60),
        ];
        let entries = delegation_entries(&children, &[], &[], 1);
        assert_eq!(shown(&entries), ["later", "early"]);
        assert_eq!(entries.hidden, 0);
    }

    #[test]
    fn a_stalled_child_stays_in_flight_however_old() {
        let stalled = child("silent", AgentStatus::Failed, 86_400);
        let mut wire = serde_json::to_value(stalled).unwrap();
        wire["stalled"] = serde_json::json!(true);
        let children = vec![
            serde_json::from_value(wire).unwrap(),
            child("failed", AgentStatus::Failed, 86_400),
        ];
        let entries = delegation_entries(&children, &[], &[], 0);
        assert_eq!(ids(&entries.children), ["silent"]);
        assert_eq!(entries.hidden, 1);
    }

    #[test]
    fn in_flight_children_keep_spawn_order_and_finished_ones_wait_for_a_page() {
        let children = roster();
        let closed = delegation_entries(&children, &[], &[], 0);
        assert_eq!(ids(&closed.children), ["b", "d"]);
        assert!(closed.shown.is_empty());
        assert_eq!(closed.hidden, 4);
        let paged = delegation_entries(&children, &[], &[], 1);
        assert_eq!(ids(&paged.children), ["b", "d"]);
        assert_eq!(shown(&paged), ["c", "f", "e", "a"]);
        assert_eq!(paged.hidden, 0);
    }

    #[test]
    fn each_page_shows_five_more_newest_first_and_in_flight_entries_are_never_paged() {
        let mut children: Vec<_> = (0..12)
            .map(|index| child(&format!("f{index}"), AgentStatus::Success, index * 60))
            .collect();
        children.extend((0..12).map(|index| child(&format!("r{index}"), AgentStatus::Running, 0)));
        for (pages, listed, hidden) in [(0, 0, 12), (1, 5, 7), (2, 10, 2), (3, 12, 0), (9, 12, 0)] {
            let entries = delegation_entries(&children, &[], &[], pages);
            assert_eq!(entries.children.len(), 12, "{pages} pages");
            assert_eq!(entries.shown.len(), listed, "{pages} pages");
            assert_eq!(entries.hidden, hidden, "{pages} pages");
        }
        let two = delegation_entries(&children, &[], &[], 2);
        let expected: Vec<String> = (0..10).map(|index| format!("f{index}")).collect();
        assert_eq!(shown(&two), expected);
    }

    #[test]
    fn a_prior_turn_child_is_finished_whatever_its_status_and_sorts_by_its_rest() {
        let mut idle = serde_json::to_value(child("idle", AgentStatus::Idle, 0)).unwrap();
        idle["prior_turn"] = serde_json::json!(true);
        idle["rested_at"] = serde_json::json!(now() - SignedDuration::from_secs(900));
        let children = vec![
            serde_json::from_value(idle).unwrap(),
            child("landed", AgentStatus::Success, 600),
        ];
        let entries = delegation_entries(&children, &[], &[], 1);
        assert!(entries.children.is_empty());
        assert_eq!(shown(&entries), ["landed", "idle"]);
    }

    #[test]
    fn finished_bookings_interleave_by_end_and_an_unstamped_end_reads_newest() {
        let children = vec![
            child("sub-old", AgentStatus::Success, 900),
            child("sub-tie", AgentStatus::Success, 300),
        ];
        let bookings = vec![
            booking("open", Some(60), false),
            booking("tie", Some(300), true),
            booking("unstamped", None, true),
            booking("newer", Some(120), true),
        ];
        let entries = delegation_entries(&children, &[], &bookings, 1);
        assert_eq!(
            entries
                .bookings
                .iter()
                .map(|booking| booking.task.as_str())
                .collect::<Vec<_>>(),
            ["open"]
        );
        // On a tie the subagent, earlier in group order, comes first.
        assert_eq!(
            shown(&entries),
            ["unstamped", "newer", "sub-tie", "tie", "sub-old"]
        );
        assert!(has_finished(&[], &[], &bookings));
        assert!(!has_finished(&[], &[], &bookings[..1]));
    }

    /// A launched entry with nothing alive is finished at its last seat's end, between a
    /// subagent and a booking on a tie; a running one lists in flight.
    #[test]
    fn finished_launched_entries_interleave_by_end_between_subagents_and_bookings() {
        let launched = |launch: &str, ended_secs_ago: Option<i64>| -> SidebarLaunched {
            serde_json::from_value(serde_json::json!({
                "starts": {"kind": "profile", "name": "coder"},
                "launches": [launch],
                "at": now() - SignedDuration::from_hours(1),
                "ended_at": ended_secs_ago.map(|ago| now() - SignedDuration::from_secs(ago)),
                "pr": {"pr": "unread"},
            }))
            .expect("minimal launched entry")
        };
        let mut running = launched("running", None);
        running.live = Some(
            serde_json::from_value(serde_json::json!({"status": "running"}))
                .expect("minimal live launch"),
        );
        let children = vec![child("sub-tie", AgentStatus::Success, 300)];
        let entries_launched = vec![
            launched("newest", Some(60)),
            running,
            launched("tie", Some(300)),
        ];
        let bookings = vec![booking("booked-tie", Some(300), true)];
        let entries = delegation_entries(&children, &entries_launched, &bookings, 1);
        assert_eq!(
            entries
                .launched
                .iter()
                .map(|launched| launched.launches[0].as_str())
                .collect::<Vec<_>>(),
            ["running"]
        );
        assert_eq!(shown(&entries), ["newest", "sub-tie", "tie", "booked-tie"]);
        assert!(has_finished(&[], &entries_launched, &[]));
        assert!(!has_finished(&[], &entries_launched[1..2], &[]));
    }

    #[test]
    fn a_child_that_finishes_moves_to_the_top_of_finished_and_nothing_else_moves() {
        let mut children = roster();
        let before = delegation_entries(&children, &[], &[], 1);
        let (before_children, before_shown) = (ids(&before.children), shown(&before));
        children[1] = child("b", AgentStatus::Success, 0);
        let after = delegation_entries(&children, &[], &[], 2);
        assert_eq!(
            ids(&after.children),
            before_children
                .into_iter()
                .filter(|id| id != "b")
                .collect::<Vec<_>>()
        );
        assert_eq!(shown(&after)[0], "b");
        assert_eq!(shown(&after)[1..], before_shown[..]);
    }

    #[test]
    fn a_parked_child_stays_in_flight_above_the_finished_pages() {
        let mut parked = child("parked", AgentStatus::Success, 1_200);
        parked.phase = crate::agents::TurnPhase::Parked;
        let mut children: Vec<_> = (0..6)
            .map(|index| child(&format!("f{index}"), AgentStatus::Success, index * 60))
            .collect();
        children.push(parked);
        let entries = delegation_entries(&children, &[], &[], 1);
        assert_eq!(ids(&entries.children), ["parked"]);
        assert_eq!(shown(&entries), ["f0", "f1", "f2", "f3", "f4"]);
        assert_eq!(entries.hidden, 1);
    }
}
