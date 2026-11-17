//! The heavy-pass writer of the machine-wide model refusal record.
//!
//! Each pass reads the record and the room's capacities, and writes only when
//! the room's agents raise a stamp: a limit marker a fresh unspent usage read
//! confirms, a second marker on a standing refusal, or a clean turn on the
//! refused model after it. The write is a read-modify-write under the record's
//! lock, so rooms writing in either order end equal; a failure is logged and the
//! next pass retries.

use jiff::Timestamp;

use crate::RuntimePaths;
use crate::agents::{AgentState, ModelRefusals, ProviderCapacity, RoomLoginSet};

pub(super) fn record_model_refusals(
    agents: &[AgentState],
    runtime: &RuntimePaths,
    logins: &RoomLoginSet,
    now: Timestamp,
) {
    let path = runtime.shared_model_refusals_path();
    let capacities = ProviderCapacity::read_all(runtime, logins);
    if !ModelRefusals::read(&path).observe(agents, &capacities, now) {
        return;
    }
    let _guard =
        match crate::disk::lock::WorkspaceLock::acquire(&runtime.shared_model_refusals_lock()) {
            Ok(guard) => guard,
            Err(err) => {
                tracing::warn!(
                    tags.operation = "cache.model_refusals_lock",
                    error = &err as &dyn std::error::Error,
                    "model refusal record could not be locked",
                );
                return;
            }
        };
    let mut refusals = ModelRefusals::read(&path);
    if !refusals.observe(agents, &capacities, now) {
        return;
    }
    if let Err(err) = crate::disk::atomic::write_temp_then_rename_cache(&path, &refusals) {
        tracing::warn!(
            path = %path.display(),
            tags.operation = "cache.model_refusals_write",
            error = &err as &dyn std::error::Error,
            "model refusal record write failed",
        );
    }
}

#[cfg(test)]
mod tests;
