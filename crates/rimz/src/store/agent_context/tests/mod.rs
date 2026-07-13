use super::*;
use crate::ids::WorkspaceId;

fn runtime() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(std::path::Path::new("/tmp/ctx-test"));
    let runtime = RuntimePaths::under(id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

fn ctx(observed_at: Timestamp) -> AgentContext {
    AgentContext {
        source: "claude".to_owned(),
        session_name: None,
        session_preview: None,
        model_id: Some("claude-opus-4-8".to_owned()),
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_opened_by: Vec::new(),
        turn_error: None,
        turn_complete: None,
        plan_proposed: None,
        turn_interrupted: None,
        observed_at,
    }
}

#[test]
fn write_then_read_round_trips() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    let all = read_all(&runtime);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].kind, "claude");
    assert_eq!(all[0].agent_id, "sess-1");
    assert_eq!(all[0].context.model_id.as_deref(), Some("claude-opus-4-8"));
}

#[test]
fn turn_openers_replace_and_survive_statusline_refresh() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    let first = crate::ids::MessageId::parse("msg_0123456789abcdef").unwrap();
    let second = crate::ids::MessageId::parse("msg_123456789abcdef0").unwrap();

    assert!(merge_turn_opened_by(&runtime, "claude", "sess-1", vec![first.clone()]).unwrap());
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    assert_eq!(
        read_one(&runtime, "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by,
        vec![first]
    );

    assert!(merge_turn_opened_by(&runtime, "claude", "sess-1", vec![second.clone()]).unwrap());
    assert_eq!(
        read_one(&runtime, "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by,
        vec![second]
    );
    assert!(merge_turn_opened_by(&runtime, "claude", "sess-1", Vec::new()).unwrap());
    assert!(
        read_one(&runtime, "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by
            .is_empty()
    );
}

#[test]
fn old_record_is_read_liveness_gating_is_the_rollups_job() {
    let (_dir, runtime) = runtime();
    let old = Timestamp::from_second(0).unwrap();
    write(&runtime, "claude", "sess-old", &ctx(old)).unwrap();

    let all = read_all(&runtime);

    assert_eq!(all.len(), 1);
    assert_eq!(all[0].agent_id, "sess-old");
    assert_eq!(all[0].context.observed_at, old);
}

mod merge;
