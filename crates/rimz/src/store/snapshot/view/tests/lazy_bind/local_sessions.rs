use super::*;

use crate::agents::{LocalSessionObservation, LocalSessionProjection, LocalSessionState};
use crate::diag::record::{DiagEvent, LocalSessionBindRejectReason};
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
        projection: LocalSessionProjection::Lifecycle(LocalSessionState {
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
        }),
    }
}

fn lifecycle_state(observation: &mut LocalSessionObservation) -> &mut LocalSessionState {
    let LocalSessionProjection::Lifecycle(state) = &mut observation.projection else {
        panic!("lifecycle projection")
    };
    state
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

fn identity_observation(kind: &str, id: &str, last_secs_ago: i64) -> LocalSessionObservation {
    let mut observation = observation(id, 20, Some(last_secs_ago), Some(20));
    observation.kind = AgentKind::new_unchecked(kind);
    observation.transcript_path = PathBuf::from(format!("/{kind}/{id}.jsonl"));
    observation.projection = LocalSessionProjection::IdentityOnly;
    observation
}

#[test]
fn stock_kiro_session_bootstraps_only_when_a_live_pane_binds() {
    let mut pane = pane("%1", "kiro-cli chat --v3", "/repo/main");
    pane.pane_process_start = Some(ago(21));
    let snapshot = room(Vec::new())
        .with_local_sessions(
            std::slice::from_ref(&pane),
            vec![event_observation("sess-live", 20, 10)],
        )
        .with_live_panes(vec![pane], None);
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(row(&snapshot, "sess-live").name, "kiro");
    assert_eq!(
        rollup_agent(&snapshot, "sess-live").usage.context_pct,
        Some(12)
    );

    let paneless =
        room(Vec::new()).with_local_sessions(&[], vec![event_observation("sess-disk", 20, 10)]);
    assert!(paneless.agents.is_empty());
}

#[test]
fn local_session_binding_normalizes_the_pane_workspace() {
    let mut pane = pane("%1", "kiro-cli chat --v3", "/repo/tmp/../main");
    pane.pane_process_start = Some(ago(21));
    let snapshot = room(Vec::new()).with_local_sessions(
        std::slice::from_ref(&pane),
        vec![event_observation("sess-live", 20, 10)],
    );

    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].agent_id.as_str(), "sess-live");
}

#[test]
fn exact_resume_wins_before_fresh_one_to_one_pairing() {
    let mut resumed = pane("%1", "kiro-cli chat --v3", "/repo/main");
    resumed.resumed_session_id = Some(AgentSessionId::from("sess-b"));
    let mut other = pane("%2", "kiro-cli chat --v3", "/repo/main");
    other.pane_process_start = Some(ago(21));
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
    let (snapshot, diagnostics) = snapshot.with_local_sessions_and_diagnostics(
        &[first.clone(), second.clone()],
        vec![event_observation("sess-a", 20, 10)],
    );
    let snapshot = snapshot.with_live_panes(vec![first, second], None);
    assert!(snapshot.agents.is_empty());
    assert!(
        diagnostics.is_empty(),
        "an ambiguous legacy fallback never selected a bind to reject"
    );
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
    assert!(agent.usage.context_pct.is_none());
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
fn evidence_free_local_session_bind_fails_closed_with_diagnostic() {
    let pane = pane("%1", "kiro-cli-chat", "/repo/main");
    let (snapshot, diagnostics) = room(Vec::new()).with_local_sessions_and_diagnostics(
        std::slice::from_ref(&pane),
        vec![event_observation("sess-unproven", 20, 10)],
    );

    assert!(snapshot.agents.is_empty());
    assert_eq!(
        diagnostics,
        [DiagEvent::LocalSessionBindRejected {
            agent_kind: AgentKind::new_unchecked("kiro"),
            agent_session_id: AgentSessionId::from("sess-unproven"),
            pane_id: pane.pane_id,
            reason: LocalSessionBindRejectReason::NoEvidence,
        }]
    );
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
    let projected = lifecycle_state(&mut observation);
    projected.status = AgentStatus::Waiting;
    projected.phase = TurnPhase::Idle;
    projected.native_prompt_detail = Some("Which option?".to_owned());
    projected.waiting_since = Some(waiting_since);

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
    let projected = lifecycle_state(&mut observation);
    projected.status = AgentStatus::Waiting;
    projected.phase = TurnPhase::Idle;
    projected.native_prompt_detail = Some("Which language?".to_owned());
    projected.waiting_since = Some(waiting_since);

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
fn cursor_ask_is_exact_hook_bound_pane_truth_and_clears_transiently() {
    let waiting_since = ago(10);
    let pane = pane("%1", "agent", "/repo/main");
    let mut durable = agent("cursor", "cursor-session", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1");
    durable.transcript_path = Some("/cursor/public/cursor-session.jsonl".to_owned());
    let mut local = observation("cursor-session", 20, Some(10), None);
    local.kind = AgentKind::new_unchecked("cursor");
    local.transcript_path = PathBuf::from("/cursor/public/cursor-session.jsonl");
    let projected = lifecycle_state(&mut local);
    projected.status = AgentStatus::Waiting;
    projected.phase = TurnPhase::Idle;
    projected.native_prompt_detail = Some("Which color?".to_owned());
    projected.waiting_since = Some(waiting_since);

    let waiting = room(vec![durable.clone()])
        .with_local_sessions(std::slice::from_ref(&pane), vec![local.clone()])
        .with_live_panes(vec![pane.clone()], None);
    let agent = rollup_agent(&waiting, "cursor-session");
    assert_eq!(agent.status, AgentStatus::Waiting);
    assert_eq!(agent.task.as_deref(), Some("Which color?"));
    assert_eq!(agent.waiting_since, Some(waiting_since));
    assert!(agent.open_ask.is_none());
    assert_eq!(
        agent.transcript_path.as_deref(),
        Some("/cursor/public/cursor-session.jsonl")
    );

    let unbound = room(Vec::new())
        .with_local_sessions(std::slice::from_ref(&pane), vec![local])
        .with_live_panes(vec![pane.clone()], None);
    assert!(
        unbound.agents.is_empty(),
        "disk history cannot invent a card"
    );

    let cleared = room(vec![durable])
        .with_local_sessions(std::slice::from_ref(&pane), Vec::new())
        .with_live_panes(vec![pane], None);
    assert_eq!(
        rollup_agent(&cleared, "cursor-session").status,
        AgentStatus::Running,
        "a disappeared pending call restores durable hook lifecycle"
    );
}

#[test]
fn hook_bound_prompt_survives_bounded_local_observations_without_prompt_evidence() {
    for status in [AgentStatus::Running, AgentStatus::Success] {
        let pane = pane("%1", "agy", "/repo/main");
        let mut durable = agent("antigravity", "conversation-a", status, 0)
            .worktree("/repo/main")
            .in_pane("%1");
        durable.prompt = Some("sticky hook prompt".to_owned());
        durable.recent_prompts = vec!["sticky hook prompt".to_owned()];
        let mut local = observation("conversation-a", 20, Some(10), None);
        local.kind = AgentKind::new_unchecked("antigravity");
        let projected = lifecycle_state(&mut local);
        projected.status = status;
        projected.phase = if status == AgentStatus::Running {
            TurnPhase::Reasoning
        } else {
            TurnPhase::Idle
        };
        projected.latest_prompt = None;

        let snapshot = room(vec![durable])
            .with_local_sessions(std::slice::from_ref(&pane), vec![local])
            .with_live_panes(vec![pane], None);
        let merged = rollup_agent(&snapshot, "conversation-a");
        assert_eq!(merged.prompt.as_deref(), Some("sticky hook prompt"));
        assert_eq!(merged.recent_prompts, ["sticky hook prompt"]);
    }
}

#[test]
fn concrete_local_prompt_replaces_once_while_new_session_stays_blank() {
    let shared_pane = pane("%1", "agy", "/repo/main");
    let mut durable = agent("antigravity", "conversation-a", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1");
    durable.prompt = Some("older prompt".to_owned());
    durable.recent_prompts = vec!["older prompt".to_owned()];
    let mut local = observation("conversation-a", 20, Some(10), None);
    local.kind = AgentKind::new_unchecked("antigravity");
    lifecycle_state(&mut local).latest_prompt = Some("newer prompt".to_owned());

    let snapshot = room(vec![durable])
        .with_local_sessions(std::slice::from_ref(&shared_pane), vec![local.clone()]);
    let snapshot = snapshot
        .with_local_sessions(std::slice::from_ref(&shared_pane), vec![local])
        .with_live_panes(vec![shared_pane], None);
    let merged = rollup_agent(&snapshot, "conversation-a");
    assert_eq!(merged.prompt.as_deref(), Some("newer prompt"));
    assert_eq!(merged.recent_prompts, ["older prompt", "newer prompt"]);

    let mut blank_pane = pane("%2", "agy", "/repo/main");
    blank_pane.resumed_session_id = Some(AgentSessionId::from("conversation-blank"));
    let mut blank = observation("conversation-blank", 20, Some(10), None);
    blank.kind = AgentKind::new_unchecked("antigravity");
    lifecycle_state(&mut blank).latest_prompt = None;
    let blank_snapshot = room(vec![merged.clone()])
        .with_local_sessions(std::slice::from_ref(&blank_pane), vec![blank]);
    let blank = rollup_agent(&blank_snapshot, "conversation-blank");
    assert!(blank.prompt.is_none());
    assert!(blank.recent_prompts.is_empty());
}

#[test]
fn latest_hook_bound_antigravity_conversation_consumes_the_shared_pane() {
    let pane = pane("%1", "agy", "/repo/main");
    let mut older = agent("antigravity", "conversation-old", AgentStatus::Success, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(120);
    older.registered_at = Some(ago(600));
    let mut newer = agent("antigravity", "conversation-new", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5);
    newer.registered_at = Some(ago(60));

    let mut stale_cache = observation("conversation-old", 600, Some(120), Some(5));
    stale_cache.kind = AgentKind::new_unchecked("antigravity");
    lifecycle_state(&mut stale_cache).latest_prompt = Some("old cached prompt".to_owned());
    let mut current = observation("conversation-new", 60, Some(5), None);
    current.kind = AgentKind::new_unchecked("antigravity");
    lifecycle_state(&mut current).latest_prompt = Some("new prompt".to_owned());

    let snapshot = room(vec![older, newer])
        .with_local_sessions(std::slice::from_ref(&pane), vec![stale_cache, current])
        .with_live_panes(vec![pane], None);
    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "conversation-new");
    assert_eq!(
        rollup_agent(&snapshot, "conversation-new")
            .prompt
            .as_deref(),
        Some("new prompt")
    );
}

#[test]
fn exact_resume_moves_a_co_resident_non_owner_to_its_new_pane() {
    let shared_pane = pane("%1", "agy", "/repo/main");
    let mut resumed_pane = pane("%2", "agy", "/repo/main");
    resumed_pane.resumed_session_id = Some(AgentSessionId::from("conversation-old"));
    let mut old = agent("antigravity", "conversation-old", AgentStatus::Success, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(120);
    old.registered_at = Some(ago(600));
    let mut owner = agent("antigravity", "conversation-owner", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5);
    owner.registered_at = Some(ago(60));
    let mut resumed = observation("conversation-old", 600, Some(10), None);
    resumed.kind = AgentKind::new_unchecked("antigravity");

    let snapshot =
        room(vec![old, owner]).with_local_sessions(&[shared_pane, resumed_pane], vec![resumed]);

    assert_eq!(
        rollup_agent(&snapshot, "conversation-old")
            .pane
            .as_ref()
            .map(|pane| pane.pane_id.raw()),
        Some("%2")
    );
}

#[test]
fn lifecycle_state_newer_than_turn_start_but_older_than_activity_is_rejected() {
    let pane = pane("%1", "agy", "/repo/main");
    let mut durable = agent("antigravity", "conversation-a", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5)
        .turn_started_ago(15);
    durable.prompt = Some("current turn".to_owned());
    let mut stale = observation("conversation-a", 20, Some(10), Some(20));
    stale.kind = AgentKind::new_unchecked("antigravity");
    let projected = lifecycle_state(&mut stale);
    projected.status = AgentStatus::Success;
    projected.phase = TurnPhase::Idle;
    projected.latest_prompt = Some("prior turn".to_owned());

    let snapshot = room(vec![durable])
        .with_local_sessions(std::slice::from_ref(&pane), vec![stale])
        .with_live_panes(vec![pane], None);
    let agent = rollup_agent(&snapshot, "conversation-a");
    assert_eq!(agent.status, AgentStatus::Running);
    assert_eq!(agent.prompt.as_deref(), Some("current turn"));
}

#[test]
fn same_second_lifecycle_state_is_accepted_without_replacing_turn_start() {
    let pane = pane("%1", "agy", "/repo/main");
    let turn_started_at = ago(20);
    let mut durable = agent("antigravity", "conversation-a", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(10);
    durable.turn_started_at = Some(turn_started_at);
    durable.open_ask = Some(crate::agents::OpenAsk {
        id: crate::ids::AskId::parse("ask_0123456789abcdef").unwrap(),
        kind: crate::agents::AskKind::Question,
        detail: Some("durable question".to_owned()),
        native_key: None,
        since: ago(12),
    });
    let mut current = observation("conversation-a", 30, Some(10), Some(30));
    current.kind = AgentKind::new_unchecked("antigravity");
    let projected = lifecycle_state(&mut current);
    projected.status = AgentStatus::Success;
    projected.phase = TurnPhase::Idle;
    projected.latest_prompt = Some("provider prompt".to_owned());

    let snapshot = room(vec![durable])
        .with_local_sessions(std::slice::from_ref(&pane), vec![current])
        .with_live_panes(vec![pane], None);
    let agent = rollup_agent(&snapshot, "conversation-a");
    assert_eq!(agent.status, AgentStatus::Success);
    assert_eq!(agent.prompt.as_deref(), Some("provider prompt"));
    assert_eq!(agent.turn_started_at, Some(turn_started_at));
    assert!(agent.open_ask.is_none());
}

#[test]
fn exact_identity_only_codex_observation_preserves_hook_owned_wait() {
    let pane = pane("%1", "codex", "/repo/main");
    let waiting_since = ago(8);
    let turn_started_at = ago(20);
    let mut durable = agent("codex", "session-a", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5);
    durable.phase = TurnPhase::Idle;
    durable.prompt = Some("choose a database".to_owned());
    durable.turn_started_at = Some(turn_started_at);
    durable.waiting_since = Some(waiting_since);
    durable.open_ask = Some(crate::agents::OpenAsk {
        id: crate::ids::AskId::parse("ask_0123456789abcdef").unwrap(),
        kind: crate::agents::AskKind::Question,
        detail: Some("Which database?".to_owned()),
        native_key: None,
        since: waiting_since,
    });
    let open_ask = durable.open_ask.clone();

    let snapshot = room(vec![durable])
        .with_local_sessions(
            std::slice::from_ref(&pane),
            vec![identity_observation("codex", "session-a", 1)],
        )
        .with_live_panes(vec![pane], None);
    let agent = rollup_agent(&snapshot, "session-a");
    assert_eq!(agent.status, AgentStatus::Waiting);
    assert_eq!(agent.prompt.as_deref(), Some("choose a database"));
    assert_eq!(agent.turn_started_at, Some(turn_started_at));
    assert_eq!(agent.waiting_since, Some(waiting_since));
    assert_eq!(agent.open_ask, open_ask);
}

#[test]
fn newer_identity_only_observation_preserves_running_lifecycle_and_clocks() {
    let pane = pane("%1", "codex", "/repo/main");
    let turn_started_at = ago(20);
    let last_activity = ago(5);
    let mut durable = agent("codex", "session-a", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1");
    durable.phase = TurnPhase::Acting;
    durable.prompt = Some("current prompt".to_owned());
    durable.recent_prompts = vec!["older prompt".to_owned(), "current prompt".to_owned()];
    durable.turn_started_at = Some(turn_started_at);
    durable.last_seen = last_activity;
    durable.last_activity = last_activity;
    durable.usage.context_pct = Some(42);

    let snapshot = room(vec![durable])
        .with_local_sessions(
            std::slice::from_ref(&pane),
            vec![identity_observation("codex", "session-a", 1)],
        )
        .with_live_panes(vec![pane], None);
    let agent = rollup_agent(&snapshot, "session-a");
    assert_eq!(agent.status, AgentStatus::Running);
    assert_eq!(agent.phase, TurnPhase::Acting);
    assert_eq!(agent.prompt.as_deref(), Some("current prompt"));
    assert_eq!(
        agent.recent_prompts,
        ["older prompt".to_owned(), "current prompt".to_owned()]
    );
    assert_eq!(agent.turn_started_at, Some(turn_started_at));
    assert_eq!(agent.last_activity, last_activity);
    assert_eq!(agent.usage.context_pct, Some(42));
}

#[test]
fn identity_only_session_adopts_launch_identity_as_idle() {
    let mut provisional = agent("codex", "launch_abc", AgentStatus::Running, 1).in_pane("%1");
    provisional.name = Some("writer".to_owned());
    provisional.name_explicit = true;
    provisional.profile = Some("codex-yolo".to_owned());
    provisional.role = Some("coder".to_owned());
    provisional.channel = Some("auth".to_owned());
    provisional.prompt = Some("launch placeholder".to_owned());
    provisional.open_ask = Some(crate::agents::OpenAsk {
        id: crate::ids::AskId::parse("ask_0123456789abcdef").unwrap(),
        kind: crate::agents::AskKind::Question,
        detail: None,
        native_key: None,
        since: ago(5),
    });
    let pane = pane("%1", "codex", "/repo/main");
    let snapshot = room(vec![provisional]).with_local_sessions(
        std::slice::from_ref(&pane),
        vec![identity_observation("codex", "session-real", 1)],
    );

    let adopted = rollup_agent(&snapshot, "session-real");
    assert_eq!(adopted.name.as_deref(), Some("writer"));
    assert_eq!(adopted.profile.as_deref(), Some("codex-yolo"));
    assert_eq!(adopted.role.as_deref(), Some("coder"));
    assert_eq!(adopted.channel.as_deref(), Some("auth"));
    assert_eq!(adopted.status, AgentStatus::Idle);
    assert_eq!(adopted.phase, TurnPhase::Idle);
    assert!(adopted.prompt.is_none());
    assert!(adopted.turn_started_at.is_none());
    assert!(adopted.open_ask.is_none());
}

#[test]
fn stale_rollout_cannot_adopt_a_fresh_launch_or_add_session_history() {
    let mut provisional = agent("codex", "launch_abc", AgentStatus::Idle, 1).in_pane("%1");
    provisional.launch_id = Some(provisional.agent_id.clone());
    provisional.registered_at = Some(ago(5));
    let pane = pane("%1", "codex", "/repo/main");

    let (snapshot, diagnostics) = room(vec![provisional]).with_local_sessions_and_diagnostics(
        std::slice::from_ref(&pane),
        vec![identity_observation("codex", "session-old", 10)],
    );
    let snapshot = snapshot.with_live_panes(vec![pane.clone()], None);

    assert!(
        snapshot
            .agents
            .iter()
            .all(|agent| agent.agent_id != "session-old")
    );
    let launch = rollup_agent(&snapshot, "launch_abc");
    assert!(launch.transcript_path.is_none());
    assert!(
        !row(&snapshot, "launch_abc")
            .as_agent()
            .expect("launch agent row")
            .has_session_history()
    );
    assert_eq!(
        diagnostics,
        [DiagEvent::LocalSessionBindRejected {
            agent_kind: AgentKind::new_unchecked("codex"),
            agent_session_id: AgentSessionId::from("session-old"),
            pane_id: pane.pane_id,
            reason: LocalSessionBindRejectReason::StaleLaunchClock,
        }]
    );
}

#[test]
fn launch_clock_admits_a_session_created_after_the_launch() {
    let mut provisional = agent("codex", "launch_abc", AgentStatus::Idle, 1).in_pane("%1");
    provisional.launch_id = Some(provisional.agent_id.clone());
    provisional.registered_at = Some(ago(30));
    let pane = pane("%1", "codex", "/repo/main");

    let (snapshot, diagnostics) = room(vec![provisional]).with_local_sessions_and_diagnostics(
        std::slice::from_ref(&pane),
        vec![identity_observation("codex", "session-current", 1)],
    );

    assert!(diagnostics.is_empty());
    assert_eq!(
        rollup_agent(&snapshot, "session-current")
            .pane
            .as_ref()
            .map(|pane| &pane.pane_id),
        Some(&pane.pane_id)
    );
}

#[test]
fn registered_session_reserves_its_launch_pane_from_other_rollouts() {
    let mut current = agent("codex", "session-current", AgentStatus::Idle, 1).in_pane("%1");
    current.launch_id = Some(AgentSessionId::from("launch_abc"));
    current.registered_at = Some(ago(30));
    let pane = pane("%1", "codex", "/repo/main");

    let (snapshot, diagnostics) = room(vec![current]).with_local_sessions_and_diagnostics(
        std::slice::from_ref(&pane),
        vec![identity_observation("codex", "session-other", 1)],
    );

    assert!(
        snapshot
            .agents
            .iter()
            .all(|agent| agent.agent_id != "session-other")
    );
    assert_eq!(
        diagnostics,
        [DiagEvent::LocalSessionBindRejected {
            agent_kind: AgentKind::new_unchecked("codex"),
            agent_session_id: AgentSessionId::from("session-other"),
            pane_id: pane.pane_id,
            reason: LocalSessionBindRejectReason::PaneReserved,
        }]
    );
}

#[test]
fn exact_old_stamp_beside_a_newer_launch_reports_a_ghost_bind() {
    let mut old = agent("codex", "session-old", AgentStatus::Idle, 0).in_pane("%1");
    old.launch_id = Some(AgentSessionId::from("launch_old"));
    old.registered_at = Some(ago(60));
    let mut fresh = agent("codex", "launch_new", AgentStatus::Idle, 1).in_pane("%1");
    fresh.launch_id = Some(fresh.agent_id.clone());
    fresh.registered_at = Some(ago(5));
    let pane = pane("%1", "codex", "/repo/main");

    let (_, diagnostics) = room(vec![old, fresh]).with_local_sessions_and_diagnostics(
        std::slice::from_ref(&pane),
        vec![identity_observation("codex", "session-old", 10)],
    );

    assert_eq!(
        diagnostics,
        [DiagEvent::GhostSessionBind {
            agent_kind: AgentKind::new_unchecked("codex"),
            agent_session_id: AgentSessionId::from("session-old"),
            pane_id: pane.pane_id,
        }]
    );
}

#[test]
fn ended_session_cannot_adopt_a_provisional_launch_identity() {
    let mut provisional = agent("codex", "launch_abc", AgentStatus::Running, 1).in_pane("%1");
    provisional.name = Some("writer".to_owned());
    provisional.name_explicit = true;
    provisional.role = Some("coder".to_owned());
    provisional.channel = Some("auth".to_owned());
    let pane = pane("%1", "codex", "/repo/main");
    let mut snapshot = room(vec![provisional]);
    snapshot.fenced_sessions.insert((
        AgentKind::new_unchecked("codex"),
        AgentSessionId::from("session-dead"),
    ));

    let snapshot = snapshot.with_local_sessions(
        std::slice::from_ref(&pane),
        vec![identity_observation("codex", "session-dead", 1)],
    );

    assert!(
        snapshot
            .agents
            .iter()
            .all(|agent| agent.agent_id != "session-dead")
    );
    let provisional = rollup_agent(&snapshot, "launch_abc");
    assert_eq!(provisional.name.as_deref(), Some("writer"));
    assert_eq!(provisional.role.as_deref(), Some("coder"));
    assert_eq!(provisional.channel.as_deref(), Some("auth"));
    assert_eq!(provisional.pane.as_ref().unwrap().pane_id.raw(), "%1");
    assert!(
        snapshot
            .agents
            .iter()
            .all(|agent| agent.transcript_path.as_deref() != Some("/codex/session-dead.jsonl"))
    );
}

#[test]
fn ownerless_session_stamped_to_an_absent_pane_cannot_fresh_bind_elsewhere() {
    let durable = agent("codex", "session-a", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%9");
    let pane = pane("%1", "codex", "/repo/main");

    let snapshot = room(vec![durable]).with_local_sessions(
        std::slice::from_ref(&pane),
        vec![identity_observation("codex", "session-a", 1)],
    );

    assert_eq!(
        rollup_agent(&snapshot, "session-a")
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .raw(),
        "%9"
    );
}

#[test]
fn reborn_unstamped_session_can_fresh_bind_with_process_clock() {
    let durable = agent("codex", "session-a", AgentStatus::Running, 0).worktree("/repo/main");
    let mut pane = pane("%1", "codex", "/repo/main");
    pane.pane_process_start = Some(ago(21));

    let snapshot = room(vec![durable]).with_local_sessions(
        std::slice::from_ref(&pane),
        vec![identity_observation("codex", "session-a", 1)],
    );

    assert_eq!(
        rollup_agent(&snapshot, "session-a")
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .raw(),
        "%1"
    );
}

#[test]
fn exact_resume_can_bind_an_ended_session() {
    let mut pane = pane("%1", "codex", "/repo/main");
    pane.resumed_session_id = Some(AgentSessionId::from("session-ended"));
    let mut snapshot = room(Vec::new());
    snapshot.fenced_sessions.insert((
        AgentKind::new_unchecked("codex"),
        AgentSessionId::from("session-ended"),
    ));

    let snapshot = snapshot.with_local_sessions(
        std::slice::from_ref(&pane),
        vec![identity_observation("codex", "session-ended", 1)],
    );

    assert_eq!(
        rollup_agent(&snapshot, "session-ended")
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .raw(),
        "%1"
    );
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
