use super::*;
use crate::agents::AgentStatus;
use crate::agents::state::{SoftCompactMark, SoftCompactProgress};
use crate::ids::MuxName;
use crate::store::snapshot::PaneAgent;

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap()
}

fn agent() -> AgentState {
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("claude"),
        "session".into(),
        AgentStatus::Running,
        ts(10),
    );
    agent.turn_started_at = Some(ts(10));
    agent.usage.total_tokens = Some(120_000);
    agent
}

fn snapshot(agent: &AgentState) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        vec![agent.clone()],
        ts(20),
    );
    std::sync::Arc::make_mut(&mut snapshot.agent_panes).push(PaneAgent {
        checkout: None,
        root_lane: false,
        kind: agent.kind.clone(),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(agent.agent_id.clone()),
        pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
        pane_pid: None,
        worktree_path: None,
        worktree_branch: None,
    });
    snapshot
}

fn due(agent: &AgentState) -> Option<SoftCompactStep> {
    due_step(
        &snapshot(agent),
        agent,
        Some(AutoCompact::Tokens(100_000)),
        "continue",
        ts(20),
    )
}

#[test]
fn wrap_up_requires_a_live_participating_open_turn_above_its_line() {
    let candidate = agent();
    assert_eq!(due(&candidate), Some(SoftCompactStep::WrapUp));
    for (name, changed) in [
        ("below threshold", {
            let mut a = candidate.clone();
            a.usage.total_tokens = Some(99_999);
            a
        }),
        ("ended", {
            let mut a = candidate.clone();
            a.ended_at = Some(ts(11));
            a
        }),
        ("resting", {
            let mut a = candidate.clone();
            a.status = AgentStatus::Success;
            a
        }),
        ("waiting", {
            let mut a = candidate.clone();
            a.status = AgentStatus::Waiting;
            a.waiting_since = Some(a.last_activity);
            a
        }),
        ("compacting", {
            let mut a = candidate.clone();
            a.compacting_since = Some(ts(19));
            a
        }),
        ("unprompted compaction", {
            let mut a = candidate.clone();
            a.compacted_awaiting_prompt = Some(ts(19));
            a
        }),
        ("provider child", {
            let mut a = candidate.clone();
            a.parent_agent_id = Some("parent".into());
            a
        }),
        ("unsupported", {
            let mut a = candidate.clone();
            a.kind = AgentKind::new_unchecked("amp");
            a
        }),
    ] {
        assert_eq!(due(&changed), None, "{name}");
    }
    let mut without_pane = snapshot(&candidate);
    std::sync::Arc::make_mut(&mut without_pane.agent_panes).clear();
    assert_eq!(
        due_step(
            &without_pane,
            &candidate,
            Some(AutoCompact::Tokens(100_000)),
            "continue",
            ts(20)
        ),
        None
    );
    assert_eq!(
        due_step(&snapshot(&candidate), &candidate, None, "continue", ts(20)),
        None
    );
    assert_eq!(
        due_step(
            &snapshot(&candidate),
            &candidate,
            Some(AutoCompact::Tokens(100_000)),
            " \n ",
            ts(20)
        ),
        None
    );
    let mut child = candidate;
    child.parent_agent_id = Some("parent".into());
    child.launch_depth = Some(1);
    assert_eq!(due(&child), Some(SoftCompactStep::WrapUp));
}

#[test]
fn wrap_up_rearms_only_in_a_new_compaction_epoch() {
    let mut candidate = agent();
    candidate.soft_compact = Some(SoftCompactMark {
        steered_at: ts(11),
        compactions: Some(0),
        progress: SoftCompactProgress::WrappingUp,
    });
    assert_eq!(due(&candidate), None);
    candidate.compaction_count = 1;
    assert_eq!(due(&candidate), Some(SoftCompactStep::WrapUp));
}

#[test]
fn a_mid_turn_compaction_close_does_not_make_continue_due() {
    let mut candidate = agent();
    candidate.usage.total_tokens = Some(20_000);
    candidate.soft_compact = Some(SoftCompactMark {
        steered_at: ts(11),
        compactions: Some(0),
        progress: SoftCompactProgress::WrappingUp,
    });
    candidate.turn_ended_at = Some(ts(12));
    candidate.compaction_count = 1;
    assert_eq!(
        due(&candidate),
        None,
        "a request close is not a clean turn end"
    );
}

#[test]
fn continue_follows_the_first_later_turn_end_even_if_native_compaction_or_config_changed() {
    let mut candidate = agent();
    candidate.status = AgentStatus::Success;
    candidate.soft_compact = Some(SoftCompactMark {
        steered_at: ts(11),
        compactions: Some(0),
        progress: SoftCompactProgress::WrappingUp,
    });
    candidate.turn_ended_at = Some(ts(11));
    assert_eq!(
        due(&candidate),
        None,
        "the preceding boundary is not the wrap-up's end"
    );
    candidate.turn_ended_at = Some(ts(12));
    candidate.soft_compact.as_mut().unwrap().progress = SoftCompactProgress::TurnEnded;
    candidate.compaction_count = 1;
    candidate.compacted_awaiting_prompt = Some(ts(13));
    candidate.usage.total_tokens = Some(1);
    assert_eq!(
        due_step(&snapshot(&candidate), &candidate, None, "continue", ts(20)),
        Some(SoftCompactStep::Continue)
    );
    candidate.status = AgentStatus::Waiting;
    candidate.waiting_since = Some(candidate.last_activity);
    assert_eq!(due(&candidate), None);
    candidate.status = AgentStatus::Success;
    candidate.soft_compact.as_mut().unwrap().progress = SoftCompactProgress::Continued;
    assert_eq!(due(&candidate), None);
}
