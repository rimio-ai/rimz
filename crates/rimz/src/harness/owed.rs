//! Wakes and prompts still owed to a supervised agent session.

use crate::Store;
use crate::ids::{AgentKind, AgentSessionId};
use crate::message::turn_wait::TurnWaitView;
use crate::store::StoreErr;
use crate::store::run::ParkCause;
use crate::store::snapshot::find_agent;

use super::fleet::FleetRuns;

/// Whether this session is still owed a harness wake, or a prompt a person or
/// an agent sent it. Each term is read before the writes that would hide it.
///
/// The fleet term goes first: the digest reporter stamps `report_message_id`
/// on the child rows and only then queues the digest, so a reader that finds
/// the rows unstamped is owed, and one that finds them stamped reads the queue
/// afterwards, where the digest by then is. Reading the queue first would
/// widen that window by this reader's own projection and runs-directory time.
///
/// The team reporter queues before stamping the run: an open run is owed
/// (unless its board already reads Done), and a settled run's report is in
/// the later queue read. That read must therefore count `TeamReport` too.
///
/// The workflow term reads every run in the room's live directory that the
/// agent started without `--detach`: its report is owed until the collector
/// marks it sent, and the collector queues the report before that mark, so a
/// reader that finds it sent finds the report in the later queue read.
///
/// A continue a compaction command owes counts from the queue read alone, from
/// its queuing until it is terminal; a smart-compaction reminder owes nothing.
///
/// The wait terms keep `TurnWaitView`'s order, catalog → queue → rollup: a
/// wake publishes its message record before it consumes its catalog row, and a
/// provider's turn start lands before the delivery ack settles that record, so
/// every wake in flight shows in at least one of the three reads. The prompt
/// term comes last, from the same queue read.
///
/// Accepted gap, now bounded by the reporter's own two lock holds: it stamps
/// and queues under separate locks, so a parent Stop landing between them can
/// complete with a digest about to be queued. Keep that tested durability
/// sequence; the stranded-park settle closes its half with a second look.
pub fn owed_wake(
    store: &Store,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Result<Option<OwedWake>, StoreErr> {
    let rows = store.audit_rows()?;
    if let Some(launcher) = rows.find(kind, agent_id) {
        let has_members = super::fleet::has_members(rows.iter(), launcher);
        let launched_team = rows
            .iter()
            .any(|agent| agent.is_team_seat() && agent.launcher_is(launcher));
        if has_members || launched_team {
            let runs = super::run::listing(store.paths())?;
            if has_members {
                let fleet = FleetRuns::of(rows.iter(), &runs, launcher);
                if fleet.any_running() || !fleet.unreported().is_empty() {
                    return Ok(Some(OwedWake::Subagents));
                }
            }
            if launched_team
                && super::fleet::owed_team_runs(rows.iter(), &runs, launcher)
                    .iter()
                    .any(|run| {
                        let stage = super::scratch::board_stage(&run.worktree_path);
                        super::fleet::team_stage_pending(
                            stage.as_ref().map(|stage| stage.name.as_str()),
                        )
                    })
            {
                return Ok(Some(OwedWake::Team));
            }
        }
        if owes_workflow_report(store, launcher)? {
            return Ok(Some(OwedWake::Workflow));
        }
    }
    let view = TurnWaitView::load(store)?;
    let Some(agent) = find_agent(&view.snapshot.agents, kind, agent_id) else {
        return Ok(None);
    };
    if view.soft_compact_in_flight(agent) {
        return Ok(Some(OwedWake::SoftCompact));
    }
    if !agent.pending_waits.is_empty() {
        return Ok(Some(OwedWake::Wait));
    }
    if view.wake_in_flight(agent, true) {
        return Ok(Some(OwedWake::WakeInFlight));
    }
    Ok(view.message_in_flight(agent).then_some(OwedWake::Message))
}

/// Whether a run `agent` started without `--detach` has not yet sent it its report: a run
/// still going, or one finished with its report owed, whatever version wrote its record.
fn owes_workflow_report(
    store: &Store,
    agent: &crate::agents::AgentState,
) -> Result<bool, StoreErr> {
    use crate::store::workflow::ReportMark;
    Ok(crate::store::workflow::live(store.paths())?
        .iter()
        .filter_map(|run| run.class.header())
        .any(|header| {
            header.report == Some(ReportMark::Owed)
                && header
                    .starter
                    .as_ref()
                    .is_some_and(|starter| starter.is_agent(agent))
        }))
}

/// What still owes an agent session a wake or a prompt, as the run fold sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwedWake {
    Wait,
    WakeInFlight,
    Subagents,
    Team,
    SoftCompact,
    Message,
    Workflow,
}

impl OwedWake {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::WakeInFlight => "wake in flight",
            Self::Subagents => "subagents",
            Self::Team => "team",
            Self::SoftCompact => "soft compaction",
            Self::Message => "message",
            Self::Workflow => "workflow run",
        }
    }

    /// The park cause a run record stores for this term.
    pub(super) const fn park_cause(self) -> ParkCause {
        match self {
            Self::Wait => ParkCause::Wait,
            Self::WakeInFlight => ParkCause::WakeInFlight,
            Self::Subagents => ParkCause::Subagents,
            Self::Team => ParkCause::Team,
            Self::SoftCompact => ParkCause::SoftCompact,
            Self::Message => ParkCause::Message,
            Self::Workflow => ParkCause::Wait,
        }
    }
}

#[cfg(test)]
mod tests;
