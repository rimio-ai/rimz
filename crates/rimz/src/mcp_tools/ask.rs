//! `ask`: the job of `rimz asks raise TEXT` on yourself.

use serde::Serialize;

use super::Tools;
use super::room::Room;
use crate::launcher::context::{current_channel, resolve_caller, resolve_me, sender_for};
use crate::mcp::catalog::AskArgs;
use crate::mcp::{Answer, Failure};
use crate::store::raised;

/// What `ask` answers: the id of the ask it armed, which a clean end of this turn opens.
#[derive(Serialize)]
struct Armed {
    id: String,
    status: &'static str,
}

pub(super) fn arm(tools: &Tools, args: &AskArgs) -> Result<Answer, Failure> {
    let room = Room::open(&tools.start_dir)?;
    let snapshot = room.published_snapshot()?;
    let agent = resolve_me(&room.store, &snapshot)?;
    let caller = resolve_caller(&room.store)?;
    let channel = current_channel(&room.workspace, Some(&room.store));
    let actor = caller
        .as_ref()
        .map(|_| sender_for(caller.as_ref(), channel.as_deref(), false).render());
    let ask = raised::prepare_raise(
        room.store.paths(),
        agent,
        true,
        Some(args.brief.clone()),
        actor,
        jiff::Timestamp::now(),
    )
    .map_err(anyhow::Error::from)?;
    let id = raised::raise_audited(&room.store, &room.workspace.session_name, ask)
        .map_err(anyhow::Error::from)?;
    Ok(Answer::data(&Armed {
        id: id.shown(),
        status: "armed",
    }))
}
