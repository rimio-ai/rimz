use super::*;

use crate::agents::LocalSessionObservation;
use crate::ids::{AgentKind, AgentSessionId};

fn observation(
    id: &str,
    created_secs_ago: i64,
    event_secs_ago: Option<i64>,
    fresh_binding_secs_ago: Option<i64>,
) -> LocalSessionObservation {
    let event_at = event_secs_ago.map(ago);
    LocalSessionObservation {
        kind: AgentKind::new_unchecked("kiro"),
        session_id: AgentSessionId::from(id),
        workspace: PathBuf::from("/repo/main"),
        transcript_path: PathBuf::from(format!("/kiro/{id}/messages.jsonl")),
        created_at: ago(created_secs_ago),
        fresh_binding_at: fresh_binding_secs_ago.map(ago),
        first_event_at: event_at,
        last_activity: event_at.unwrap_or_else(|| ago(created_secs_ago)),
        status: if event_at.is_some() {
            AgentStatus::Running
        } else {
            AgentStatus::Idle
        },
        phase: if event_at.is_some() {
            TurnPhase::Reasoning
        } else {
            TurnPhase::Idle
        },
        latest_prompt: event_at.map(|_| "ping".to_owned()),
        native_prompt_detail: None,
        waiting_since: None,
        context_pct: event_at.map(|_| 12),
    }
}

fn event_observation(
    id: &str,
    created_secs_ago: i64,
    event_secs_ago: i64,
) -> LocalSessionObservation {
    observation(
        id,
        created_secs_ago,
        Some(event_secs_ago),
        Some(created_secs_ago),
    )
}

fn newborn_observation(id: &str, created_secs_ago: i64) -> LocalSessionObservation {
    observation(id, created_secs_ago, None, Some(created_secs_ago))
}

#[test]
fn stock_kiro_session_bootstraps_only_when_a_live_pane_binds() {
    let pane = pane("%1", "kiro-cli chat --v3", "/repo/main");
    let snapshot = room(Vec::new())
        .with_local_sessions(
            std::slice::from_ref(&pane),
            vec![event_observation("sess-live", 20, 10)],
        )
        .with_live_panes(vec![pane], None);
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(row(&snapshot, "sess-live").name, "kiro");
    assert_eq!(rollup_agent(&snapshot, "sess-live").context_pct, Some(12));

    let paneless =
        room(Vec::new()).with_local_sessions(&[], vec![event_observation("sess-disk", 20, 10)]);
    assert!(paneless.agents.is_empty());
}

#[test]
fn exact_resume_wins_before_fresh_one_to_one_pairing() {
    let mut resumed = pane("%1", "kiro-cli chat --v3", "/repo/main");
    resumed.resumed_session_id = Some(AgentSessionId::from("sess-b"));
    let other = pane("%2", "kiro-cli chat --v3", "/repo/main");
    let observations = vec![
        event_observation("sess-a", 20, 10),
        event_observation("sess-b", 20, 10),
    ];
    let snapshot = room(Vec::new())
        .with_local_sessions(&[resumed.clone(), other.clone()], observations)
        .with_live_panes(vec![resumed, other], None);
    assert_eq!(snapshot.agents.len(), 2);
    assert_eq!(
        rollup_agent(&snapshot, "sess-b")
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .raw(),
        "%1"
    );
}

#[test]
fn ambiguous_fresh_candidates_stay_identityless_idle_agent_rows() {
    let first = pane("%1", "kiro-cli chat --v3", "/repo/main");
    let second = pane("%2", "kiro-cli chat --v3", "/repo/main");
    let mut snapshot = room(Vec::new());
    snapshot.wired_kinds = vec!["kiro".to_owned()];
    let snapshot = snapshot
        .with_local_sessions(
            &[first.clone(), second.clone()],
            vec![event_observation("sess-a", 20, 10)],
        )
        .with_live_panes(vec![first, second], None);
    assert!(snapshot.agents.is_empty());
    assert!(rows(&snapshot).iter().all(|row| row.is_agent()));
    assert!(
        rows(&snapshot)
            .iter()
            .all(|row| row.status() == Some(AgentStatus::Idle))
    );
    assert_eq!(
        rows(&snapshot)
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["tmux:%1", "tmux:%2"]
    );
}

#[test]
fn newest_session_pairs_with_newest_compatible_pane() {
    let mut first = pane("%1", "kiro-cli chat --v3", "/repo/main");
    first.pane_process_start = Some(ago(100));
    let mut second = pane("%2", "kiro-cli chat --v3", "/repo/main");
    second.pane_process_start = Some(ago(95));
    let observations = vec![
        event_observation("sess-first", 90, 80),
        event_observation("sess-second", 70, 60),
    ];
    let snapshot = room(Vec::new())
        .with_local_sessions(&[first.clone(), second.clone()], observations)
        .with_live_panes(vec![first, second], None);
    assert_eq!(snapshot.agents.len(), 2);
    assert_eq!(
        rollup_agent(&snapshot, "sess-first")
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .raw(),
        "%1"
    );
    assert_eq!(
        rollup_agent(&snapshot, "sess-second")
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .raw(),
        "%2"
    );
}

#[test]
fn recordless_current_session_binds_only_to_its_live_process_incarnation() {
    let mut current_pane = pane("%1", "kiro-cli-chat", "/repo/main");
    current_pane.pane_process_start = Some(ago(21));
    let snapshot = room(Vec::new())
        .with_local_sessions(
            std::slice::from_ref(&current_pane),
            vec![newborn_observation("sess-newborn", 20)],
        )
        .with_live_panes(vec![current_pane], None);

    let agent = rollup_agent(&snapshot, "sess-newborn");
    assert_eq!(agent.status, AgentStatus::Idle);
    assert_eq!(agent.phase, TurnPhase::Idle);
    assert!(agent.turn_started_at.is_none());
    assert!(agent.prompt.is_none());
    assert!(agent.context_pct.is_none());
    assert!(row(&snapshot, "sess-newborn").is_agent());

    let mut stale_pane = pane("%2", "kiro-cli-chat", "/repo/main");
    stale_pane.pane_process_start = Some(ago(19));
    let stale = room(Vec::new())
        .with_local_sessions(
            std::slice::from_ref(&stale_pane),
            vec![newborn_observation("sess-stale", 20)],
        )
        .with_live_panes(vec![stale_pane], None);
    assert!(stale.agents.is_empty());
    assert!(rows(&stale).iter().all(|row| row.is_process()));
}

#[test]
fn recordless_session_without_process_start_stays_a_process_row() {
    let pane = pane("%1", "kiro-cli-chat", "/repo/main");
    let snapshot = room(Vec::new())
        .with_local_sessions(
            std::slice::from_ref(&pane),
            vec![newborn_observation("sess-newborn", 20)],
        )
        .with_live_panes(vec![pane], None);

    assert!(snapshot.agents.is_empty());
    assert!(rows(&snapshot).iter().all(|row| row.is_process()));
}

#[test]
fn exact_resume_binds_an_empty_session_without_fresh_authorization() {
    let mut pane = pane("%1", "kiro-cli-chat", "/repo/main");
    pane.resumed_session_id = Some(AgentSessionId::from("sess-resumed"));
    let observation = observation("sess-resumed", 20, None, None);
    let snapshot = room(Vec::new())
        .with_local_sessions(std::slice::from_ref(&pane), vec![observation])
        .with_live_panes(vec![pane], None);

    assert_eq!(
        rollup_agent(&snapshot, "sess-resumed").status,
        AgentStatus::Idle
    );
}

#[test]
fn equal_process_starts_remain_ambiguous() {
    let mut first = pane("%1", "kiro-cli-chat", "/repo/main");
    first.pane_process_start = Some(ago(30));
    let mut second = pane("%2", "kiro-cli-chat", "/repo/main");
    second.pane_process_start = Some(ago(30));
    let snapshot = room(Vec::new())
        .with_local_sessions(
            &[first.clone(), second.clone()],
            vec![newborn_observation("sess-newborn", 20)],
        )
        .with_live_panes(vec![first, second], None);

    assert!(snapshot.agents.is_empty());
    assert!(rows(&snapshot).iter().all(|row| row.is_process()));
}

#[test]
fn observations_without_fresh_authorization_are_exact_resume_only() {
    let pane = pane("%1", "agy", "/repo/main");
    let mut observation = observation("conversation-a", 20, Some(10), None);
    observation.kind = AgentKind::new_unchecked("antigravity");
    observation.transcript_path = PathBuf::from("/antigravity/conversation-a/transcript.jsonl");
    let snapshot = room(Vec::new())
        .with_local_sessions(std::slice::from_ref(&pane), vec![observation])
        .with_live_panes(vec![pane], None);

    assert!(snapshot.agents.is_empty());
    assert!(rows(&snapshot).iter().all(|row| row.is_process()));
}

#[test]
fn antigravity_transcript_question_projects_a_pane_only_wait() {
    let waiting_since = ago(10);
    let mut pane = pane("%1", "agy", "/repo/main");
    pane.resumed_session_id = Some(AgentSessionId::from("conversation-a"));
    let mut observation = observation("conversation-a", 20, Some(10), None);
    observation.kind = AgentKind::new_unchecked("antigravity");
    observation.transcript_path =
        PathBuf::from("/antigravity/conversation-a/transcript_full.jsonl");
    observation.status = AgentStatus::Waiting;
    observation.phase = TurnPhase::Idle;
    observation.native_prompt_detail = Some("Which option?".to_owned());
    observation.waiting_since = Some(waiting_since);

    let snapshot = room(Vec::new())
        .with_local_sessions(std::slice::from_ref(&pane), vec![observation])
        .with_live_panes(vec![pane], None);
    let agent = rollup_agent(&snapshot, "conversation-a");
    assert_eq!(agent.status, AgentStatus::Waiting);
    assert_eq!(agent.phase, TurnPhase::Idle);
    assert_eq!(agent.task.as_deref(), Some("Which option?"));
    assert_eq!(agent.waiting_since, Some(waiting_since));
    assert!(agent.open_ask.is_none());
    assert_eq!(
        row(&snapshot, "conversation-a").task(),
        Some("Which option?")
    );
}

#[test]
fn hook_bound_antigravity_question_does_not_need_workspace_latest_authorization() {
    let waiting_since = ago(10);
    let pane = pane("%1", "agy", "/repo/main");
    let durable = agent("antigravity", "conversation-a", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1");
    let mut observation = observation("conversation-a", 20, Some(10), None);
    observation.kind = AgentKind::new_unchecked("antigravity");
    observation.transcript_path =
        PathBuf::from("/antigravity/conversation-a/transcript_full.jsonl");
    observation.status = AgentStatus::Waiting;
    observation.phase = TurnPhase::Idle;
    observation.native_prompt_detail = Some("Which language?".to_owned());
    observation.waiting_since = Some(waiting_since);

    let snapshot = room(vec![durable])
        .with_local_sessions(std::slice::from_ref(&pane), vec![observation])
        .with_live_panes(vec![pane], None);
    let agent = rollup_agent(&snapshot, "conversation-a");
    assert_eq!(agent.status, AgentStatus::Waiting);
    assert_eq!(agent.phase, TurnPhase::Idle);
    assert_eq!(agent.task.as_deref(), Some("Which language?"));
    assert_eq!(agent.waiting_since, Some(waiting_since));
    assert!(agent.open_ask.is_none());
    assert_eq!(
        row(&snapshot, "conversation-a").task(),
        Some("Which language?")
    );
}

#[test]
fn stale_hook_bound_transcript_does_not_regress_a_newer_turn() {
    let pane = pane("%1", "agy", "/repo/main");
    let mut durable = agent("antigravity", "conversation-a", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(5);
    durable.prompt = Some("current turn".to_owned());
    let mut stale = observation("conversation-a", 20, Some(10), Some(20));
    stale.kind = AgentKind::new_unchecked("antigravity");
    stale.status = AgentStatus::Success;
    stale.phase = TurnPhase::Idle;
    stale.latest_prompt = Some("prior turn".to_owned());

    let snapshot = room(vec![durable])
        .with_local_sessions(std::slice::from_ref(&pane), vec![stale])
        .with_live_panes(vec![pane], None);
    let agent = rollup_agent(&snapshot, "conversation-a");
    assert_eq!(agent.status, AgentStatus::Running);
    assert_eq!(agent.prompt.as_deref(), Some("current turn"));
}

#[test]
fn provider_session_adopts_provisional_launch_identity() {
    let mut provisional = agent("kiro", "launch_abc", AgentStatus::Idle, 1).in_pane("%1");
    provisional.name = Some("writer".to_owned());
    provisional.name_explicit = true;
    provisional.profile = Some("kiro-yolo".to_owned());
    provisional.role = Some("coder".to_owned());
    provisional.channel = Some("auth".to_owned());
    provisional.description = Some("migration".to_owned());
    provisional.budget = Some("$5.00".to_owned());
    let pane = pane("%1", "kiro-cli chat --v3", "/repo/main");
    let snapshot = room(vec![provisional])
        .with_local_sessions(&[pane], vec![event_observation("sess-real", 20, 10)]);
    assert_eq!(snapshot.agents.len(), 1);
    let adopted = rollup_agent(&snapshot, "sess-real");
    assert_eq!(adopted.name.as_deref(), Some("writer"));
    assert_eq!(adopted.profile.as_deref(), Some("kiro-yolo"));
    assert_eq!(adopted.role.as_deref(), Some("coder"));
    assert_eq!(adopted.channel.as_deref(), Some("auth"));
    assert_eq!(adopted.description.as_deref(), Some("migration"));
    assert_eq!(adopted.budget.as_deref(), Some("$5.00"));
}
