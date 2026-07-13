use super::*;

use crate::agents::LocalSessionObservation;
use crate::ids::{AgentKind, AgentSessionId};

fn observation(id: &str, created_secs_ago: i64, event_secs_ago: i64) -> LocalSessionObservation {
    LocalSessionObservation {
        kind: AgentKind::new_unchecked("kiro"),
        session_id: AgentSessionId::from(id),
        workspace: PathBuf::from("/repo/main"),
        transcript_path: PathBuf::from(format!("/kiro/{id}/messages.jsonl")),
        created_at: ago(created_secs_ago),
        first_event_at: Some(ago(event_secs_ago)),
        last_activity: ago(event_secs_ago - 1),
        status: AgentStatus::Running,
        phase: TurnPhase::Reasoning,
        latest_prompt: Some("ping".to_owned()),
        native_prompt_detail: None,
        waiting_since: None,
        context_pct: Some(12),
    }
}

#[test]
fn stock_kiro_session_bootstraps_only_when_a_live_pane_binds() {
    let pane = pane("%1", "kiro-cli chat --v3", "/repo/main");
    let snapshot = room(Vec::new())
        .with_local_sessions(
            std::slice::from_ref(&pane),
            vec![observation("sess-live", 20, 10)],
        )
        .with_live_panes(vec![pane], None);
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(row(&snapshot, "sess-live").name, "kiro");
    assert_eq!(rollup_agent(&snapshot, "sess-live").context_pct, Some(12));

    let paneless =
        room(Vec::new()).with_local_sessions(&[], vec![observation("sess-disk", 20, 10)]);
    assert!(paneless.agents.is_empty());
}

#[test]
fn exact_resume_wins_before_fresh_one_to_one_pairing() {
    let mut resumed = pane("%1", "kiro-cli chat --v3", "/repo/main");
    resumed.resumed_session_id = Some(AgentSessionId::from("sess-b"));
    let other = pane("%2", "kiro-cli chat --v3", "/repo/main");
    let observations = vec![observation("sess-a", 20, 10), observation("sess-b", 20, 10)];
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
fn ambiguous_fresh_candidates_stay_process_rows() {
    let first = pane("%1", "kiro-cli chat --v3", "/repo/main");
    let second = pane("%2", "kiro-cli chat --v3", "/repo/main");
    let snapshot = room(Vec::new())
        .with_local_sessions(
            &[first.clone(), second.clone()],
            vec![observation("sess-a", 20, 10)],
        )
        .with_live_panes(vec![first, second], None);
    assert!(snapshot.agents.is_empty());
    assert_eq!(
        rows(&snapshot)
            .iter()
            .filter(|row| row.is_process())
            .count(),
        2
    );
}

#[test]
fn creation_times_assign_same_cwd_sessions_one_to_one() {
    let mut first = pane("%1", "kiro-cli chat --v3", "/repo/main");
    first.pane_process_start = Some(ago(100));
    let mut second = pane("%2", "kiro-cli chat --v3", "/repo/main");
    second.pane_process_start = Some(ago(40));
    let observations = vec![
        observation("sess-first", 90, 80),
        observation("sess-second", 30, 20),
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
        .with_local_sessions(&[pane], vec![observation("sess-real", 20, 10)]);
    assert_eq!(snapshot.agents.len(), 1);
    let adopted = rollup_agent(&snapshot, "sess-real");
    assert_eq!(adopted.name.as_deref(), Some("writer"));
    assert_eq!(adopted.profile.as_deref(), Some("kiro-yolo"));
    assert_eq!(adopted.role.as_deref(), Some("coder"));
    assert_eq!(adopted.channel.as_deref(), Some("auth"));
    assert_eq!(adopted.description.as_deref(), Some("migration"));
    assert_eq!(adopted.budget.as_deref(), Some("$5.00"));
}
