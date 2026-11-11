//! What a loop task hands the processes and launches its fire starts: the
//! environment key naming the task, and the account its launches use.

use crate::agents::{ProviderLogin, RoomLoginErr};
use crate::config::MachineConfig;
use crate::disk::paths::RuntimePaths;
use crate::ids::AgentKind;
use crate::store::writer::LaunchLogin;

pub(crate) const LOOP_TASK_ENV: &str = "RIMZ_LOOP_TASK";

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
