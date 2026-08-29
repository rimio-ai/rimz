//! Pure publish verdicts for producer pane frames.

use crate::diag::record::FrameRejectReason;
use crate::ids::PaneId;
use crate::sidebar::frame::PaneFrame;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PublishVerdict {
    Publish,
    Reject(FrameRejectReason),
}

pub(super) fn frame_publish_verdict(
    fresh: &PaneFrame,
    own_pane: Option<&PaneId>,
) -> PublishVerdict {
    if pane_count(fresh) == 0 {
        return PublishVerdict::Reject(FrameRejectReason::Empty);
    }
    if own_pane_missing(fresh, own_pane) {
        return PublishVerdict::Reject(FrameRejectReason::MissingOwnPane);
    }
    PublishVerdict::Publish
}

pub(super) fn own_pane_missing(frame: &PaneFrame, own_pane: Option<&PaneId>) -> bool {
    own_pane.is_some_and(|own_pane| !frame_contains_pane(frame, own_pane))
}

pub(super) fn shrink_needs_verification(fresh: &PaneFrame, prior: Option<&PaneFrame>) -> bool {
    let Some(prior) = prior else {
        return false;
    };
    let prior_count = pane_count(prior);
    let fresh_count = pane_count(fresh);
    prior_count > 0 && fresh_count.saturating_mul(2) < prior_count
}

pub(super) fn pane_count(frame: &PaneFrame) -> usize {
    frame.pane_states().count()
}

fn frame_contains_pane(frame: &PaneFrame, pane_id: &PaneId) -> bool {
    frame.pane_states().any(|pane| pane.pane_id == *pane_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MuxName;
    use crate::sidebar::frame::assemble_frame;
    use crate::sidebar::produce::test_support::pane;

    fn pane_id(raw: &str) -> PaneId {
        PaneId::from_parts(MuxName::Zellij, raw)
    }

    #[test]
    fn empty_frame_rejects() {
        let fresh = assemble_frame(Vec::new(), 2, "s");

        assert_eq!(
            frame_publish_verdict(&fresh, None),
            PublishVerdict::Reject(FrameRejectReason::Empty)
        );
    }

    #[test]
    fn missing_own_pane_rejects() {
        let fresh = assemble_frame(vec![pane("terminal_2", Some("zsh"), Some("/repo"))], 2, "s");
        let own = pane_id("terminal_1");

        assert_eq!(
            frame_publish_verdict(&fresh, Some(&own)),
            PublishVerdict::Reject(FrameRejectReason::MissingOwnPane)
        );
    }

    #[test]
    fn large_shrink_needs_verification() {
        for (name, prior_count, fresh_count, expected) in [
            ("large shrink", Some(3), 1, true),
            ("exactly half", Some(4), 2, false),
            ("no prior", None, 1, false),
        ] {
            let prior = prior_count.map(|count| frame_with_pane_count(count, 1));
            let fresh = frame_with_pane_count(fresh_count, 2);

            assert_eq!(
                shrink_needs_verification(&fresh, prior.as_ref()),
                expected,
                "{name}"
            );
        }
    }

    fn frame_with_pane_count(count: usize, produced_at_ms: u64) -> PaneFrame {
        assemble_frame(
            (1..=count)
                .map(|index| pane(&format!("terminal_{index}"), Some("zsh"), Some("/repo")))
                .collect(),
            produced_at_ms,
            "s",
        )
    }
}
