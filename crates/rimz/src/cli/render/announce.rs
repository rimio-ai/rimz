//! The ending every blocking wait shares when messages are queued for the
//! agent running it: one stderr line and exit 75.

use std::io::Write;

use rimz::message::announce::Announcement;

/// Exit status of a wait that stopped because messages are queued for its
/// caller (sysexits' `EX_TEMPFAIL`): nothing it awaited failed or timed out.
pub(crate) const MESSAGE_STOP_EXIT: i32 = 75;

/// The announcement: how many messages, from whom, that they arrive when the
/// caller ends its turn, and that the awaited work is still running. It
/// carries no message text and no command to rerun.
pub(crate) fn write_message_stop(
    err: &mut impl Write,
    announcement: &Announcement,
) -> std::io::Result<()> {
    let senders = announcement
        .senders
        .iter()
        .map(rimz::store::message::MessageSender::render)
        .collect::<Vec<_>>()
        .join(", ");
    let (noun, verb) = if announcement.count == 1 {
        ("message", "arrives")
    } else {
        ("messages", "arrive")
    };
    writeln!(
        err,
        "rimz: stopped waiting: {} {noun} from {senders} {verb} when you end your turn; \
         what you waited on is still running",
        announcement.count,
    )?;
    err.flush()
}
