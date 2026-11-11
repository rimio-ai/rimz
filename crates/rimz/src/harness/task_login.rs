//! The account a loop task's launches use. It resolves a `store::writer::LaunchLogin`, so it
//! stays outside the sidebar import graph that `harness::loop_task` belongs to.

use crate::agents::{ProviderLogin, RoomLoginErr};
use crate::config::MachineConfig;
use crate::disk::paths::RuntimePaths;
use crate::ids::AgentKind;
use crate::store::writer::LaunchLogin;

/// The account a task's gates, hooks preflight, window reads, and launch use
/// for `kind`: the row's pin, else the room's current account, which is `None`
/// when the room's record resolves none.
pub fn task_login(
    launch: &LaunchLogin,
    kind: &str,
    runtime: &RuntimePaths,
) -> Result<Option<ProviderLogin>, RoomLoginErr> {
    match launch {
        LaunchLogin::RoomDefault => {
            Ok(crate::agents::RoomLoginSet::for_runtime(runtime).default_login(kind))
        }
        LaunchLogin::Pinned(name) => crate::agents::session_login(
            &AgentKind::new_unchecked(kind),
            Some(name),
            &MachineConfig::load_lenient().accounts,
        )
        .map(Some),
    }
}
