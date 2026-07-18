use jiff::Timestamp;

use super::codex_origin_overrides;
use crate::SidebarSnapshot;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::WorkspaceId;

#[test]
fn codex_origin_overrides_read_transcript_and_worktree_from_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("rollout.jsonl");
    let worktree = dir.path().join("repo");
    let now = Timestamp::now();
    let agent = AgentState {
        status: AgentStatus::Running,
        worktree_path: Some(worktree.display().to_string()),
        transcript_path: Some(transcript.display().to_string()),
        ..crate::testkit::agent_state("codex", "codex-1", now)
    };
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(&worktree),
        vec![agent],
        now,
    );

    let overrides = codex_origin_overrides(&snapshot);

    assert_eq!(overrides.get(&transcript), Some(&worktree));
}
