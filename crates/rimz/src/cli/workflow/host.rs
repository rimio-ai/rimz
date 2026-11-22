//! The workflow host a command drives: the library host, asking through the terminal.

use rimz::ids::EventId;
use rimz::launcher::workflow_host::Host;

use crate::cli::GlobalFlags;
use crate::cli::launch_interaction::CliInteraction;

pub(super) fn new<'a>(
    state: &'a rimz::StatePaths,
    globals: &GlobalFlags,
    woken_by: Option<EventId>,
) -> Host<'a> {
    Host::new(
        state,
        globals.mux,
        globals.root.clone(),
        woken_by,
        Box::new(CliInteraction),
    )
}
