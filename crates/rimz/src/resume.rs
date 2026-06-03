//! Resume-on-rebirth planning: turn the durable agent rollup into the panes a
//! reborn session re-seeds.
//!
//! When a multiplexer session dies — reboot, server crash, or a Rimz-initiated
//! rebirth of a stuck room — the agents' processes are gone, but the ledger
//! remembers them. This module reads that memory (the audit rollup, which keeps
//! the dead-process agents the runtime projection would expel) and plans one
//! resume pane per prior root agent, so the next birth comes up where the user
//! left off instead of empty.
//!
//! Pure over its inputs: the caller supplies the rollup, the set of cleanly
//! ended sessions, and a worktree-exists predicate, so every filtering rule is
//! unit-tested without a multiplexer or the filesystem. The launcher
//! ([`crate::mux::MuxBackend`]) seeds the resulting [`ResumePane`]s at birth and
//! stays ignorant of agents and the ledger.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::agents::integration_by_name;
use crate::feed::AgentState;
use crate::mux::ResumePane;

/// The default ceiling on agents auto-resumed into one reborn session, so a
/// long-lived workspace cannot fork-bomb a fleet of agent processes on birth.
/// Anything past it is reported, never silently dropped.
pub const DEFAULT_RESUME_MAX: usize = 8;

/// Why a candidate agent was not resumed — surfaced in the start report so a
/// skipped agent stays visible rather than silently lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeSkipReason {
    /// The agent's kind has no resume CLI ([`crate::agents::AgentIntegration::resume_command`]).
    NoResumeSupport,
    /// The agent's worktree no longer exists on disk, so its pane has nowhere to run.
    WorktreeMissing,
    /// Dropped to stay within the resume cap.
    OverCap,
}

/// A candidate that the planner deliberately did not resume, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeSkip {
    pub label: String,
    pub reason: ResumeSkipReason,
}

/// What a reborn session should re-seed, and what it deliberately left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumePlan {
    /// The panes to seed, most-recently-active first (the lead is the focus
    /// target).
    pub panes: Vec<ResumePane>,
    /// Candidates not resumed, each with its reason — the start report names them.
    pub skipped: Vec<ResumeSkip>,
}

impl ResumePlan {
    /// Whether there is nothing to seed — the birth is exactly the bare working
    /// room.
    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }
}

/// Plan the resume seeds for one reborn session from the durable agent rollup.
///
/// `agents` is the audit rollup (dead-process agents intact); `session_name`
/// scopes to this workspace's session; `ended` is the `(kind, agent_id)` set the
/// user closed cleanly ([`crate::ledger::snapshot::agent_tombstones_for_events`]);
/// `max` caps the auto-launched panes; `worktree_exists` decides whether a
/// candidate's worktree is still on disk (production passes `|p| p.is_dir()`).
///
/// A candidate qualifies when it is a root agent (subagents ride their parent),
/// was bound to a pane in *this* session, still carries a session id and a
/// worktree, and was not cleanly ended. A relaunched agent (same kind+worktree)
/// collapses to its newest session, mirroring the sidebar's supersession reap,
/// so resume never doubles it.
pub fn plan_resume(
    agents: &[AgentState],
    session_name: &str,
    ended: &BTreeSet<(String, String)>,
    max: usize,
    worktree_exists: impl Fn(&Path) -> bool,
) -> ResumePlan {
    // Root agents that were bound to a pane in this session, still identified,
    // and not cleanly ended. A subagent is paneless and rides its parent, so it
    // is filtered out here and never resumed standalone.
    let mut candidates: Vec<&AgentState> = agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .filter(|agent| {
            agent
                .worktree_path
                .as_deref()
                .is_some_and(|path| !path.is_empty())
        })
        .filter(|agent| {
            agent
                .pane
                .as_ref()
                .is_some_and(|pane| pane.session_name == session_name)
        })
        .filter(|agent| !ended.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .collect();

    // Most-recently-active first (deterministic on ties), so the newest session
    // wins supersession, the lead pane is the focus target, and the cap keeps
    // the freshest agents.
    candidates.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    });

    let mut seen: HashSet<(String, String, Option<String>)> = HashSet::new();
    let mut plan = ResumePlan::default();
    for agent in candidates {
        // `worktree_path` is `Some(non-empty)` by the filter above.
        let worktree = agent.worktree_path.clone().unwrap_or_default();
        let key = (
            agent.kind.clone(),
            worktree.clone(),
            agent.worktree_branch.clone(),
        );
        // A superseded older relaunch in the same worktree: the sidebar showed
        // only the newest, so resume only the newest.
        if !seen.insert(key) {
            continue;
        }
        let cwd = PathBuf::from(&worktree);
        let label = build_label(&agent.kind, agent.worktree_branch.as_deref(), &cwd);
        if !worktree_exists(&cwd) {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::WorktreeMissing,
            });
            continue;
        }
        let Some(command) = integration_by_name(&agent.kind)
            .ok()
            .and_then(|adapter| adapter.resume_command(&agent.agent_id, &cwd))
        else {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::NoResumeSupport,
            });
            continue;
        };
        if plan.panes.len() >= max {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::OverCap,
            });
            continue;
        }
        plan.panes.push(ResumePane {
            command,
            cwd,
            label,
        });
    }
    plan
}

/// A short, view-safe label for a resumed agent: `kind:branch`, falling back to
/// the worktree directory name, then `kind:agent`. Doubles as the Zellij tab /
/// tmux window name and the seed's idempotency key.
fn build_label(kind: &str, branch: Option<&str>, worktree: &Path) -> String {
    let short = branch
        .filter(|branch| !branch.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            worktree
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "agent".to_owned());
    format!("{kind}:{short}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::{AgentStatus, PaneRef, PermissionPosture};
    use crate::ids::{MuxName, PaneId};
    use jiff::Timestamp;

    const SESSION: &str = "rimz-code-query-engine";

    fn pane_in(session: &str, raw: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            session_name: session.to_owned(),
            view_id: None,
            view_kind: None,
            view_name: None,
            is_focused: false,
            client_focused: false,
            command: None,
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
        }
    }

    /// A root agent bound to a pane in `SESSION`, active `secs_ago` seconds back.
    fn agent(
        kind: &str,
        id: &str,
        worktree: &str,
        branch: Option<&str>,
        secs_ago: i64,
    ) -> AgentState {
        let when = Timestamp::now() - std::time::Duration::from_secs(secs_ago.max(0) as u64);
        AgentState {
            agent_id: id.to_owned(),
            kind: kind.to_owned(),
            status: AgentStatus::Idle,
            permission_posture: PermissionPosture::Default,
            pane: Some(pane_in(SESSION, &format!("terminal_{id}"))),
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: Some(worktree.to_owned()),
            worktree_branch: branch.map(ToOwned::to_owned),
            task: None,
            prompt: None,
            model: None,
            effort: None,
            context_pct: None,
            total_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            parked_on_background: false,
            last_seen: when,
            last_activity: when,
        }
    }

    fn no_ended() -> BTreeSet<(String, String)> {
        BTreeSet::new()
    }

    #[test]
    fn resumes_root_agents_in_this_session_most_recent_first() {
        let agents = vec![
            agent("codex", "c1", "/code/query-engine", Some("main"), 30),
            agent(
                "claude",
                "a1",
                "/code/qe-feature",
                Some("feature-migration"),
                5,
            ),
        ];
        let plan = plan_resume(&agents, SESSION, &no_ended(), DEFAULT_RESUME_MAX, |_| true);
        assert!(plan.skipped.is_empty());
        assert_eq!(plan.panes.len(), 2);
        // Most-recently-active leads (the focus target).
        assert_eq!(plan.panes[0].label, "claude:feature-migration");
        assert_eq!(plan.panes[0].command, vec!["claude", "--resume", "a1"]);
        assert_eq!(plan.panes[0].cwd, PathBuf::from("/code/qe-feature"));
        assert_eq!(plan.panes[1].label, "codex:main");
        assert_eq!(plan.panes[1].command, vec!["codex", "resume", "c1"]);
    }

    #[test]
    fn skips_subagents() {
        let mut child = agent("claude", "kid", "/code/query-engine", Some("main"), 1);
        child.parent_agent_id = Some("parent".to_owned());
        let plan = plan_resume(&[child], SESSION, &no_ended(), DEFAULT_RESUME_MAX, |_| true);
        assert!(plan.is_empty());
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn skips_agents_from_another_session() {
        let mut other = agent("claude", "a1", "/code/query-engine", Some("main"), 1);
        other.pane = Some(pane_in("rimz-some-other-room", "terminal_a1"));
        let plan = plan_resume(&[other], SESSION, &no_ended(), DEFAULT_RESUME_MAX, |_| true);
        assert!(plan.is_empty());
    }

    #[test]
    fn skips_paneless_agents() {
        let mut paneless = agent("claude", "a1", "/code/query-engine", Some("main"), 1);
        paneless.pane = None;
        let plan = plan_resume(
            &[paneless],
            SESSION,
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn skips_cleanly_ended_sessions() {
        let agents = vec![agent("claude", "a1", "/code/query-engine", Some("main"), 1)];
        let ended: BTreeSet<(String, String)> = [("claude".to_owned(), "a1".to_owned())]
            .into_iter()
            .collect();
        let plan = plan_resume(&agents, SESSION, &ended, DEFAULT_RESUME_MAX, |_| true);
        assert!(plan.is_empty());
    }

    #[test]
    fn reports_a_missing_worktree() {
        let agents = vec![agent("claude", "a1", "/code/gone", Some("dead-branch"), 1)];
        let plan = plan_resume(&agents, SESSION, &no_ended(), DEFAULT_RESUME_MAX, |_| false);
        assert!(plan.panes.is_empty());
        assert_eq!(
            plan.skipped,
            vec![ResumeSkip {
                label: "claude:dead-branch".to_owned(),
                reason: ResumeSkipReason::WorktreeMissing,
            }]
        );
    }

    #[test]
    fn dedups_a_relaunched_agent_keeping_the_newest() {
        let agents = vec![
            agent("claude", "old", "/code/query-engine", Some("main"), 60),
            agent("claude", "new", "/code/query-engine", Some("main"), 2),
        ];
        let plan = plan_resume(&agents, SESSION, &no_ended(), DEFAULT_RESUME_MAX, |_| true);
        assert_eq!(plan.panes.len(), 1);
        assert_eq!(plan.panes[0].command, vec!["claude", "--resume", "new"]);
        // The superseded relaunch is dropped silently, not reported as a skip.
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn keeps_two_kinds_in_one_worktree() {
        let agents = vec![
            agent("claude", "a1", "/code/query-engine", Some("main"), 5),
            agent("codex", "c1", "/code/query-engine", Some("main"), 5),
        ];
        let plan = plan_resume(&agents, SESSION, &no_ended(), DEFAULT_RESUME_MAX, |_| true);
        assert_eq!(plan.panes.len(), 2);
    }

    #[test]
    fn caps_and_reports_the_overflow() {
        let agents = vec![
            agent("claude", "a1", "/code/wt-1", Some("b1"), 5),
            agent("claude", "a2", "/code/wt-2", Some("b2"), 10),
        ];
        let plan = plan_resume(&agents, SESSION, &no_ended(), 1, |_| true);
        assert_eq!(plan.panes.len(), 1);
        // The freshest survives the cap; the older overflows.
        assert_eq!(plan.panes[0].command, vec!["claude", "--resume", "a1"]);
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].reason, ResumeSkipReason::OverCap);
    }

    #[test]
    fn empty_when_no_agents() {
        let plan = plan_resume(&[], SESSION, &no_ended(), DEFAULT_RESUME_MAX, |_| true);
        assert!(plan.is_empty());
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn labels_fall_back_to_the_worktree_dir_without_a_branch() {
        let agents = vec![agent("codex", "c1", "/code/query-engine", None, 1)];
        let plan = plan_resume(&agents, SESSION, &no_ended(), DEFAULT_RESUME_MAX, |_| true);
        assert_eq!(plan.panes[0].label, "codex:query-engine");
    }
}
