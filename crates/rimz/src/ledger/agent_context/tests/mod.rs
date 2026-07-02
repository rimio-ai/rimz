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
        turn_error: None,
        turn_complete: None,
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

mod merge;
