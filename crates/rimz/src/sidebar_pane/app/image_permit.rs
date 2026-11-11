//! The host's one mint for the permit every pixel slot take and graphics write requires.

use std::time::Instant;

use crate::ids::PaneId;
use crate::sidebar_pane::pixel::ImagePermit;

use super::focus_push::FocusPeers;
use super::views::ViewsRecord;

/// A permit for `pane` at `now`: the last fresh client sample routes a client to
/// it, or it holds the host's live jump landing. Nothing else earns images.
pub(super) fn image_permit(
    record: &ViewsRecord,
    peers: Option<&FocusPeers>,
    pane: &PaneId,
    now: Instant,
) -> Option<ImagePermit> {
    (record.on_screen(pane) || peers.is_some_and(|peers| peers.lands_on(pane, now)))
        .then(ImagePermit::grant_by_host)
}
