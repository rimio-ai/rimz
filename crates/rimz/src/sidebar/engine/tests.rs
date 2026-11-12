use std::path::Path;

use super::*;
use crate::agents::AgentStatus;
use crate::sidebar::test_support::{activity_row, pane, worktree_group};

#[test]
fn a_final_fold_records_attention_and_opens_an_unread_episode() {
    let dir = tempfile::TempDir::new().unwrap();
    let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let now = jiff::Timestamp::now();
    let mut row = activity_row(true, Some(AgentStatus::Waiting), now, Path::new("/repo"));
    row.id = "ask".to_owned();
    row.pane = Some(pane("terminal_1", "claude", "/repo"));
    let mut snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), now);
    snapshot.panes_produced_at_ms = Some(1);
    snapshot.worktree_groups = vec![worktree_group(Path::new("/repo"), vec![row])].into();

    let outcome = reconcile_final_fold(&runtime, &mut snapshot);

    assert!(runtime.attention_path().exists());
    assert!(runtime.unread_path().exists());
    assert!(snapshot.worktree_groups[0].rows[0].unread);
    let opened: Vec<_> = outcome
        .opened
        .iter()
        .map(|open| open.row_id.as_str())
        .collect();
    assert_eq!(opened, ["ask"]);
}
