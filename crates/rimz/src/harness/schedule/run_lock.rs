//! The run lock a loop task's runner holds, spelled once for the runner that
//! claims it and the clock pass that probes it. Read-only: it belongs to the
//! sidebar import graph and never claims or writes a lock.

use crate::ids::WorkspaceId;

/// The one spelling of a run lock's file name. `checkout` names the launch
/// checkout of a fan-out fire, which locks per checkout.
pub(super) fn file_name(name: &str, checkout: Option<&WorkspaceId>) -> String {
    match checkout {
        Some(checkout) => format!("loop-run-{name}-{checkout}.lock"),
        None => format!("loop-run-{name}.lock"),
    }
}
