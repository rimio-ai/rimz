//! Agent session supersession rules shared by durable and view reaps. The
//! writer attaches provider rest certificates before applying them; the
//! store-only view reap deliberately falls back to durable lifecycle state.

use jiff::Timestamp;

use crate::agents::{AgentState, SamePaneSessionPolicy, SessionOrigin};
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
            || same_process_conversation_supersedes(older, newer)
            || compaction_continuation_supersedes(older, newer))
}

/// Whether `newer` is the compacted continuation of `older` in the exact same
/// pane and agent-process incarnation. The provider-named predecessor is
/// stronger evidence than the running-owner guard: the old root cannot receive
/// another hook after Codex moves the continuation to a new session id.
///
/// ponytail: daemon-owned continuations abstain; add provider session-instance
/// proof before promoting app-server-routed sessions.
fn compaction_continuation_supersedes(older: &AgentState, newer: &AgentState) -> bool {
    newer.compacted_from.as_ref() == Some(&older.agent_id) && same_agent_instance(older, newer)
}

/// A provider that follows its latest conversation can prove an in-place
/// session switch when both records name the same pane incarnation and agent
/// process. The outer activity guard establishes which record is newer. An
/// open owner remains authoritative because same-process child hooks can carry
/// a distinct conversation ID while its turn is open.
fn same_process_conversation_supersedes(older: &AgentState, newer: &AgentState) -> bool {
    if older.holds_open_turn() {
        return false;
    }
    if crate::agents::spec_by_kind(older.kind.as_str()).is_none_or(|definition| {
        definition.capabilities.same_pane_session != SamePaneSessionPolicy::FollowLatest
    }) {
        return false;
    }
    same_agent_instance(older, newer)
}

/// Whether two roots name the same pane and agent-process incarnation.
fn same_agent_instance(older: &AgentState, newer: &AgentState) -> bool {
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
fn older_yields_pane(older: &AgentState, newer: &AgentState) -> bool {
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
/// older ended when the newer began, live pane or not. An open owner remains
/// authoritative because same-process child hooks can carry a distinct
/// conversation ID while its turn is open. A fork carries `Forked` lineage and
/// survives; unknown lineage keeps both. The writer reaper attaches provider
/// rest certificates before applying this rule.
fn cleared_conversation_supersedes(older: &AgentState, newer: &AgentState) -> bool {
    if older.holds_open_turn() {
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
fn relaunched_in_pane(older: &AgentState, newer: &AgentState) -> bool {
    match (agent_owner_pid(older), agent_owner_pid(newer)) {
        (Some(older_pid), Some(newer_pid)) => older_pid != newer_pid,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{
        AgentContext, AgentStatus, AgentTurnError, TurnErrorClass, TurnSettle, TurnSettleOutcome,
    };
    use crate::ids::{MuxName, PaneId};
    use crate::pane::{PaneRef, RuntimeOwner};

    fn conversation_pair(
        kind: &str,
        status: AgentStatus,
        newer_origin: Option<SessionOrigin>,
    ) -> (AgentState, AgentState) {
        let older_at = Timestamp::UNIX_EPOCH;
        let newer_at = older_at + std::time::Duration::from_secs(1);
        let pane = PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, "%1"));
        let owner = RuntimeOwner::new(RuntimeOwnerKind::Agent, "agy", 42, Some("start".to_owned()));
        let mut older = crate::testkit::agent_state(kind, "older", older_at);
        older.status = status;
        older.pane = Some(pane.clone());
        older.runtime_owner = Some(owner.clone());
        older.origin = Some(SessionOrigin::Fresh);
        let mut newer = crate::testkit::agent_state(kind, "newer", newer_at);
        newer.pane = Some(pane);
        newer.runtime_owner = Some(owner);
        newer.origin = newer_origin;
        (older, newer)
    }

    #[test]
    fn mid_turn_session_death_keeps_same_process_owner_authoritative() {
        for status in [AgentStatus::Running, AgentStatus::Waiting] {
            let (older, newer) =
                conversation_pair("antigravity", status, Some(SessionOrigin::Fresh));
            assert!(!same_process_conversation_supersedes(&older, &newer));
            assert!(!cleared_conversation_supersedes(&older, &newer));
            assert!(!supersedes(&older, &newer));
        }

        let (mut waiting, newer) = conversation_pair(
            "antigravity",
            AgentStatus::Waiting,
            Some(SessionOrigin::Fresh),
        );
        waiting.context = Some(AgentContext {
            settle: Some(TurnSettle::new(
                waiting.last_activity + std::time::Duration::from_secs(1),
                TurnSettleOutcome::NativeWait,
            )),
            ..AgentContext::new("antigravity", waiting.last_activity)
        });
        assert!(waiting.holds_open_turn());
        assert!(!supersedes(&waiting, &newer));
    }

    #[test]
    fn resting_session_death_still_follows_latest_conversation() {
        let (older, newer) = conversation_pair(
            "antigravity",
            AgentStatus::Success,
            Some(SessionOrigin::Fresh),
        );
        assert!(same_process_conversation_supersedes(&older, &newer));
        assert!(cleared_conversation_supersedes(&older, &newer));
        assert!(supersedes(&older, &newer));
    }

    #[test]
    fn cursor_clear_without_session_start_follows_latest_conversation() {
        let (older, newer) = conversation_pair("cursor", AgentStatus::Success, None);
        assert!(same_process_conversation_supersedes(&older, &newer));
        assert!(supersedes(&older, &newer));
        assert!(!cleared_conversation_supersedes(&older, &newer));
    }

    #[test]
    fn mid_turn_session_death_still_yields_to_a_relaunched_process() {
        let (older, mut newer) = conversation_pair(
            "antigravity",
            AgentStatus::Running,
            Some(SessionOrigin::Fresh),
        );
        newer.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "agy",
            43,
            Some("replacement".to_owned()),
        ));
        assert!(older_yields_pane(&older, &newer));
        assert!(supersedes(&older, &newer));
    }

    #[test]
    fn compacted_continuation_supersedes_resting_or_running_predecessor() {
        for status in [AgentStatus::Success, AgentStatus::Running] {
            let (older, mut newer) =
                conversation_pair("codex", status, Some(SessionOrigin::Forked));
            newer.compacted_from = Some(older.agent_id.clone());

            assert!(compaction_continuation_supersedes(&older, &newer));
            assert!(supersedes(&older, &newer));
        }
    }

    #[test]
    fn compacted_continuation_requires_the_same_agent_instance() {
        let (mut older, mut newer) =
            conversation_pair("codex", AgentStatus::Success, Some(SessionOrigin::Forked));
        assert!(
            !compaction_continuation_supersedes(&older, &newer),
            "a plain fork does not supersede its source"
        );

        newer.compacted_from = Some(older.agent_id.clone());
        newer.runtime_owner.as_mut().unwrap().pid += 1;
        assert!(!compaction_continuation_supersedes(&older, &newer));

        newer.runtime_owner = older.runtime_owner.clone();
        older.pane.as_mut().unwrap().pane_process_start = Some(older.last_activity);
        newer.pane.as_mut().unwrap().pane_process_start = Some(newer.last_activity);
        assert!(!compaction_continuation_supersedes(&older, &newer));
    }

    #[test]
    fn certificate_dead_owner_yields_to_fresh_replacement() {
        for class in [
            TurnErrorClass::PausedOverloaded,
            TurnErrorClass::PausedRateLimit,
            TurnErrorClass::Failed,
        ] {
            let (mut older, newer) =
                conversation_pair("codex", AgentStatus::Running, Some(SessionOrigin::Fresh));
            let at = older.last_activity + std::time::Duration::from_secs(1);
            older.context = Some(AgentContext {
                turn_error: Some(AgentTurnError {
                    class,
                    at,
                    label: None,
                }),
                ..AgentContext::new("codex", at)
            });
            assert!(supersedes(&older, &newer), "{class:?}");
        }

        for outcome in [TurnSettleOutcome::Interrupted, TurnSettleOutcome::Complete] {
            let (mut older, newer) =
                conversation_pair("codex", AgentStatus::Running, Some(SessionOrigin::Fresh));
            let at = older.last_activity + std::time::Duration::from_secs(1);
            older.context = Some(AgentContext {
                settle: Some(TurnSettle::new(at, outcome)),
                ..AgentContext::new("codex", at)
            });
            assert!(supersedes(&older, &newer), "{outcome:?}");
        }

        let (mut parked, newer) =
            conversation_pair("codex", AgentStatus::Running, Some(SessionOrigin::Fresh));
        parked.phase = crate::agents::TurnPhase::Parked;
        assert!(supersedes(&parked, &newer));
    }

    #[test]
    fn stale_certificate_keeps_owner() {
        let (mut older, newer) =
            conversation_pair("codex", AgentStatus::Running, Some(SessionOrigin::Fresh));
        older.context = Some(AgentContext {
            turn_error: Some(AgentTurnError {
                class: TurnErrorClass::PausedOverloaded,
                at: older.last_activity,
                label: None,
            }),
            ..AgentContext::new("codex", older.last_activity)
        });
        assert!(!supersedes(&older, &newer));
    }

    #[test]
    fn rested_follow_latest_owner_requires_the_same_process_incarnation() {
        let (mut older, mut newer) = conversation_pair("antigravity", AgentStatus::Success, None);
        newer.runtime_owner.as_mut().unwrap().process_start = Some("replacement".to_owned());
        assert!(!supersedes(&older, &newer));

        newer.runtime_owner = older.runtime_owner.clone();
        newer.pane.as_mut().unwrap().pane_process_start = Some(newer.last_activity);
        older.pane.as_mut().unwrap().pane_process_start = Some(older.last_activity);
        assert!(!supersedes(&older, &newer));

        newer.pane = older.pane.clone();
        older.runtime_owner.as_mut().unwrap().kind = RuntimeOwnerKind::Daemon;
        newer.runtime_owner.as_mut().unwrap().kind = RuntimeOwnerKind::Daemon;
        assert!(!supersedes(&older, &newer));
    }

    #[test]
    fn rested_fresh_owner_does_not_yield_to_a_fork() {
        let (mut older, newer) =
            conversation_pair("codex", AgentStatus::Running, Some(SessionOrigin::Forked));
        let at = older.last_activity + std::time::Duration::from_secs(1);
        older.context = Some(AgentContext {
            settle: Some(TurnSettle::new(at, TurnSettleOutcome::Interrupted)),
            ..AgentContext::new("codex", at)
        });
        assert!(!supersedes(&older, &newer));
    }
}
