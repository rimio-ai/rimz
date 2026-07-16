//! Store-provable agent session death rules shared by durable and view reaps.

use jiff::Timestamp;

use crate::agents::{AgentState, AgentStatus, SamePaneSessionPolicy, SessionOrigin};
use crate::pane::RuntimeOwnerKind;

/// Age in seconds after which a pidless agent session is reaped as a ghost.
/// A session that never captured a pid can't be reaped by process liveness, so
/// without a TTL it would linger forever; a few hours is long enough that a
/// genuinely live but pidless session (rare) survives, short enough that an
/// abandoned one clears on its own.
pub(crate) const GHOST_SESSION_TTL_SECS: i64 = 3 * 60 * 60;

pub(crate) fn agent_is_pidless(agent: &AgentState) -> bool {
    match agent.runtime_owner.as_ref().map(|owner| owner.kind) {
        Some(RuntimeOwnerKind::Agent | RuntimeOwnerKind::Script) => false,
        Some(RuntimeOwnerKind::Daemon) => true,
        None => true,
    }
}

/// The pid the hook recorded as this session's owner. In daemon mode this is
/// the shared app-server daemon; in standalone mode it is the session's own
/// process.
pub(crate) fn agent_owner_pid(agent: &AgentState) -> Option<u32> {
    agent.runtime_owner.as_ref().map(|owner| owner.pid)
}

pub(crate) fn session_age_secs(now: Timestamp, agent: &AgentState) -> i64 {
    now.duration_since(agent.last_activity).as_secs()
}

/// Whether `newer` proves that `older` no longer owns its session slot.
pub(crate) fn supersedes(older: &AgentState, newer: &AgentState) -> bool {
    newer.kind == older.kind
        && newer.agent_id != older.agent_id
        && newer.last_activity > older.last_activity
        && (older_yields_pane(older, newer)
            || cleared_conversation_supersedes(older, newer)
            || same_process_conversation_supersedes(older, newer))
}

/// A provider that follows its latest conversation can prove an in-place
/// session switch when both records name the same pane incarnation and agent
/// process. The outer activity guard establishes which record is newer. A
/// running, waiting, or paused owner remains authoritative because same-process
/// child hooks can carry a distinct conversation ID while its turn is open.
pub(crate) fn same_process_conversation_supersedes(older: &AgentState, newer: &AgentState) -> bool {
    if matches!(
        older.status,
        AgentStatus::Running | AgentStatus::Waiting | AgentStatus::Paused
    ) {
        return false;
    }
    if crate::agents::descriptor_by_kind(older.kind.as_str()).is_none_or(|descriptor| {
        descriptor.capabilities.same_pane_session != SamePaneSessionPolicy::FollowLatest
    }) {
        return false;
    }
    let same_pane = matches!(
        (older.pane.as_ref(), newer.pane.as_ref()),
        (Some(older_pane), Some(newer_pane))
            if older_pane.pane_id == newer_pane.pane_id
                && compatible_tokens(
                    older_pane.pane_process_start.as_ref(),
                    newer_pane.pane_process_start.as_ref(),
                )
    );
    if !same_pane {
        return false;
    }
    matches!(
        (older.runtime_owner.as_ref(), newer.runtime_owner.as_ref()),
        (Some(older_owner), Some(newer_owner))
            if older_owner.kind == RuntimeOwnerKind::Agent
                && newer_owner.kind == RuntimeOwnerKind::Agent
                && older_owner.pid == newer_owner.pid
                && compatible_tokens(
                    older_owner.process_start.as_ref(),
                    newer_owner.process_start.as_ref(),
                )
    )
}

fn compatible_tokens<T: PartialEq>(older: Option<&T>, newer: Option<&T>) -> bool {
    match (older, newer) {
        (Some(older), Some(newer)) => older == newer,
        _ => true,
    }
}

/// True when reaping `older` cannot drop a concurrently-live agent. The pane is
/// the unit of identity: an older session yields when the newer one *relaunched*
/// in its exact pane — a provably different process ([`relaunched_in_pane`]),
/// regardless of any branch checkout between the two — or when both are paneless
/// remnants of the same worktree (indistinguishable daemon/shared-pid ghosts).
/// An older paneless session does not yield to a newer distinctly stamped pane:
/// it may still be the occupant of another same-cwd lazy agent pane that only
/// the projection can bind.
pub(crate) fn older_yields_pane(older: &AgentState, newer: &AgentState) -> bool {
    match (older.pane.as_ref(), newer.pane.as_ref()) {
        (Some(older_pane), Some(newer_pane)) => {
            newer_pane.pane_id == older_pane.pane_id && relaunched_in_pane(older, newer)
        }
        (None, None) => {
            older.worktree_path == newer.worktree_path
                && older.worktree_branch == newer.worktree_branch
        }
        (None, Some(_)) | (Some(_), None) => false,
    }
}

/// Whether `newer` is a fresh `/clear` / `/new` conversation superseding
/// `older` in their shared pane. Both roots carrying `Fresh` rollout lineage
/// on one stamped pane are sequential conversations of a single terminal; the
/// older ended when the newer began, live pane or not. A running, waiting, or
/// paused owner remains authoritative because same-process child hooks can
/// carry a distinct conversation ID while its turn is open. A fork carries
/// `Forked` lineage and survives; unknown lineage keeps both.
pub(crate) fn cleared_conversation_supersedes(older: &AgentState, newer: &AgentState) -> bool {
    if matches!(
        older.status,
        AgentStatus::Running | AgentStatus::Waiting | AgentStatus::Paused
    ) {
        return false;
    }
    older.origin == Some(SessionOrigin::Fresh)
        && newer.origin == Some(SessionOrigin::Fresh)
        && matches!(
            (older.pane.as_ref(), newer.pane.as_ref()),
            (Some(older_pane), Some(newer_pane)) if older_pane.pane_id == newer_pane.pane_id
        )
}

/// Whether the newer session is a *relaunch* of the older in their shared pane —
/// a provably different process — rather than a same-process in-pane thread fork.
/// A Codex `/side` / `/btw` fork registers a fresh session id in the live process
/// the primary still owns, so the pair shares one owner pid and must not collapse;
/// only the projection (`stamped_agent_for_pane`) arbitrates such a pair, pinning
/// the pane to its earliest-registered primary. A standalone relaunch records two
/// distinct live pids, so it still collapses here. A daemon-routed relaunch shares
/// the daemon's owner pid like a fork, but the loaded-thread reaper
/// (`SidebarSnapshot::reap_runtime`) and the projection's process-start guard
/// (`crate::store::snapshot::panes::pane_start_allows_bind`) collapse it instead.
/// Unknown owners never collapse — a missing pid cannot prove a relaunch.
pub(crate) fn relaunched_in_pane(older: &AgentState, newer: &AgentState) -> bool {
    match (agent_owner_pid(older), agent_owner_pid(newer)) {
        (Some(older_pid), Some(newer_pid)) => older_pid != newer_pid,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, PaneId};
    use crate::pane::{PaneRef, RuntimeOwner};

    fn conversation_pair(status: AgentStatus) -> (AgentState, AgentState) {
        let older_at = Timestamp::UNIX_EPOCH;
        let newer_at = older_at + std::time::Duration::from_secs(1);
        let pane = PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, "%1"));
        let owner = RuntimeOwner::new(RuntimeOwnerKind::Agent, "agy", 42, Some("start".to_owned()));
        let mut older = crate::testkit::agent_state("antigravity", "older", older_at);
        older.status = status;
        older.pane = Some(pane.clone());
        older.runtime_owner = Some(owner.clone());
        older.origin = Some(SessionOrigin::Fresh);
        let mut newer = crate::testkit::agent_state("antigravity", "newer", newer_at);
        newer.pane = Some(pane);
        newer.runtime_owner = Some(owner);
        newer.origin = Some(SessionOrigin::Fresh);
        (older, newer)
    }

    #[test]
    fn mid_turn_session_death_keeps_same_process_owner_authoritative() {
        for status in [
            AgentStatus::Running,
            AgentStatus::Waiting,
            AgentStatus::Paused,
        ] {
            let (older, newer) = conversation_pair(status);
            assert!(!same_process_conversation_supersedes(&older, &newer));
            assert!(!cleared_conversation_supersedes(&older, &newer));
            assert!(!supersedes(&older, &newer));
        }
    }

    #[test]
    fn resting_session_death_still_follows_latest_conversation() {
        let (older, newer) = conversation_pair(AgentStatus::Success);
        assert!(same_process_conversation_supersedes(&older, &newer));
        assert!(cleared_conversation_supersedes(&older, &newer));
        assert!(supersedes(&older, &newer));
    }

    #[test]
    fn mid_turn_session_death_still_yields_to_a_relaunched_process() {
        let (older, mut newer) = conversation_pair(AgentStatus::Running);
        newer.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "agy",
            43,
            Some("replacement".to_owned()),
        ));
        assert!(older_yields_pane(&older, &newer));
        assert!(supersedes(&older, &newer));
    }
}
