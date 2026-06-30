//! Pure publish verdicts for producer pane frames.

use crate::ids::PaneId;
use crate::schema::diag::FrameRejectReason;
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
    if let Some(own_pane) = own_pane
        && !frame_contains_pane(fresh, own_pane)
    {
        return PublishVerdict::Reject(FrameRejectReason::MissingOwnPane);
    }
    PublishVerdict::Publish
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
        let prior = assemble_frame(
            vec![
                pane("terminal_1", Some("zsh"), Some("/repo")),
                pane("terminal_2", Some("zsh"), Some("/repo")),
                pane("terminal_3", Some("zsh"), Some("/repo")),
            ],
            1,
            "s",
        );
        let fresh = assemble_frame(vec![pane("terminal_1", Some("zsh"), Some("/repo"))], 2, "s");

        assert!(shrink_needs_verification(&fresh, Some(&prior)));
    }
}
