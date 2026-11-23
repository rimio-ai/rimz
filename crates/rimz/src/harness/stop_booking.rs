//! Booking and withdrawing an agent's soft stop (`rimz agents stop --when-idle`): the durable
//! request the room host and its helper act on later (`super::idle_stop`). Kept apart from that
//! module, which the sidebar imports, since booking writes the store.

use std::time::Duration;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::StatePaths;
use crate::agents::AgentState;
use crate::harness::ancestry::{CallerIdentity, resolve_launch_caller};
use crate::store::idle_stop::{self, IdleStopRequest};
use crate::store::snapshot::SidebarSnapshot;

/// The rest a soft stop waits for when its request names none.
pub const IDLE_REST_DEFAULT: &str = "3m";

/// The rest a soft-stop request names: a duration, or `None` for `off`, which
/// withdraws a pending request.
pub fn parse_idle_rest(raw: &str) -> Result<Option<Duration>> {
    if raw == "off" {
        return Ok(None);
    }
    Ok(Some(crate::utils::time::parse_compact_duration(raw)?))
}

/// What a soft-stop request did.
#[derive(Debug, Serialize)]
#[serde(tag = "booking", rename_all = "snake_case")]
pub enum StopBooking {
    /// The pending request was withdrawn.
    Withdrawn,
    /// A withdrawal found no pending request.
    NonePending,
    /// The stop is booked after `after_secs` idle, replacing a pending request of
    /// `replaces_after_secs` when there was one.
    Booked {
        after_secs: u64,
        replaces_after_secs: Option<u64>,
    },
}

/// Book `agent`'s stop once it has rested `after`, or withdraw its pending booking when `after`
/// is none. `caller` reads who asks, recorded on a booking; `snapshot` names the agents.
pub fn book_stop(
    paths: &StatePaths,
    snapshot: &SidebarSnapshot,
    agent: &AgentState,
    after: Option<Duration>,
    caller: impl FnOnce() -> Result<Option<CallerIdentity>>,
) -> Result<StopBooking> {
    let peers = crate::address::addressable_agents(snapshot);
    let label = crate::address::named_handle(agent, &peers, true);
    let pending = idle_stop::read(paths)
        .into_iter()
        .find(|request| request.kind == agent.kind && request.agent_id == agent.agent_id);
    let Some(after) = after else {
        return Ok(
            if idle_stop::withdraw(paths, &agent.kind, &agent.agent_id)? {
                StopBooking::Withdrawn
            } else {
                StopBooking::NonePending
            },
        );
    };
    if agent.ended_at.is_some() {
        bail!(
            "{label} has ended, and --when-idle needs one live agent; reopen it with `rimz agents resume` first"
        );
    }
    if agent.is_provider_subagent() {
        bail!(
            "{label} is a provider subagent and ends with its parent; request the stop on the parent"
        );
    }
    let requested_by = caller()?
        .and_then(|caller| resolve_launch_caller(&snapshot.agents, &caller).ok())
        .map(|caller| crate::address::agent_handle(caller, &peers, true));
    idle_stop::arm(
        paths,
        IdleStopRequest {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            stop: crate::agents::IdleStop {
                after_secs: after.as_secs(),
                requested_at: jiff::Timestamp::now(),
                requested_by,
            },
        },
    )?;
    Ok(StopBooking::Booked {
        after_secs: after.as_secs(),
        replaces_after_secs: pending.map(|request| request.stop.after_secs),
    })
}
