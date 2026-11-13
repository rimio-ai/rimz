//! Visibility bands for an open delegation section over the current turn's
//! children. Unfinished children come first in spawn order, uncapped; finished
//! children follow newest finish first, up to [`MAX_FINISHED`], and the rest
//! are only counted. A child that finishes leaves the unfinished band for the
//! top of the finished band, and nothing else moves.

use crate::store::snapshot::SidebarSubAgent;

use super::sub_agent_finished;

/// The finished children an open section lists before it only counts the rest.
const MAX_FINISHED: usize = 5;

pub(super) struct DelegationBands<'a> {
    /// Every child not yet finished, in the projection's spawn order.
    pub(super) unfinished: Vec<&'a SidebarSubAgent>,
    /// The newest finished children, newest finish first.
    pub(super) finished: Vec<&'a SidebarSubAgent>,
    /// Finished children beyond the listed ones.
    pub(super) more_finished: usize,
}

/// Classify `children` (spawn-ordered by the projection) into bands. A finished
/// child's finish instant is `finished_at`, falling back to `last_activity`.
pub(super) fn delegation_bands<'a>(
    children: impl IntoIterator<Item = &'a SidebarSubAgent>,
) -> DelegationBands<'a> {
    let (mut finished, unfinished): (Vec<_>, Vec<_>) = children
        .into_iter()
        .partition(|child| sub_agent_finished(child));
    finished.sort_by(|a, b| {
        b.finished_at
            .unwrap_or(b.last_activity)
            .cmp(&a.finished_at.unwrap_or(a.last_activity))
            .then_with(|| a.id.cmp(&b.id))
    });
    let more_finished = finished.len().saturating_sub(MAX_FINISHED);
    finished.truncate(MAX_FINISHED);
    DelegationBands {
        unfinished,
        finished,
        more_finished,
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

    fn ids(children: &[&SidebarSubAgent]) -> Vec<String> {
        children.iter().map(|child| child.id.clone()).collect()
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
        let bands = delegation_bands(&children);
        assert_eq!(ids(&bands.finished), ["later", "early"]);
        assert_eq!(bands.more_finished, 0);
    }

    #[test]
    fn a_stalled_child_stays_unfinished_however_old() {
        let stalled = child("silent", AgentStatus::Failed, 86_400);
        let mut wire = serde_json::to_value(stalled).unwrap();
        wire["stalled"] = serde_json::json!(true);
        let children = vec![
            serde_json::from_value(wire).unwrap(),
            child("failed", AgentStatus::Failed, 86_400),
        ];
        let bands = delegation_bands(&children);
        assert_eq!(ids(&bands.unfinished), ["silent"]);
        assert_eq!(ids(&bands.finished), ["failed"]);
    }

    #[test]
    fn unfinished_keep_spawn_order_and_finished_run_newest_first() {
        let children = roster();
        let bands = delegation_bands(&children);
        assert_eq!(ids(&bands.unfinished), ["b", "d"]);
        assert_eq!(ids(&bands.finished), ["c", "f", "e", "a"]);
        assert_eq!(bands.more_finished, 0);
    }

    #[test]
    fn five_finished_list_whole_and_a_sixth_is_counted() {
        let finished = |count: usize| {
            (0..count)
                .map(|index| {
                    child(
                        &format!("f{index}"),
                        AgentStatus::Success,
                        i64::try_from(index).unwrap() * 60,
                    )
                })
                .chain([child("live", AgentStatus::Running, 0)])
                .collect::<Vec<_>>()
        };
        let five = finished(5);
        let bands = delegation_bands(&five);
        assert_eq!(ids(&bands.finished), ["f0", "f1", "f2", "f3", "f4"]);
        assert_eq!(bands.more_finished, 0);
        let six = finished(6);
        let bands = delegation_bands(&six);
        assert_eq!(ids(&bands.unfinished), ["live"]);
        assert_eq!(ids(&bands.finished), ["f0", "f1", "f2", "f3", "f4"]);
        assert_eq!(bands.more_finished, 1);
        // Unfinished children are never capped.
        let many: Vec<_> = (0..12)
            .map(|index| child(&format!("r{index}"), AgentStatus::Running, 0))
            .collect();
        assert_eq!(delegation_bands(&many).unfinished.len(), 12);
    }

    #[test]
    fn a_child_that_finishes_moves_to_the_top_of_finished_and_nothing_else_moves() {
        let mut children = roster();
        let before = delegation_bands(&children);
        let (before_unfinished, before_finished) = (ids(&before.unfinished), ids(&before.finished));
        children[1] = child("b", AgentStatus::Success, 0);
        let after = delegation_bands(&children);
        assert_eq!(
            ids(&after.unfinished),
            before_unfinished
                .into_iter()
                .filter(|id| id != "b")
                .collect::<Vec<_>>()
        );
        assert_eq!(ids(&after.finished)[0], "b");
        assert_eq!(ids(&after.finished)[1..], before_finished[..]);
    }
}
