use super::*;
use crate::ledger::atomic;
use crate::sidebar::cache::SNAPSHOT_CACHE_TTL;
use crate::sidebar::produce::test_support::pane;

mod cache;
mod fields;

fn frame(panes: Vec<crate::pane::PaneRef>) -> crate::sidebar::frame::PaneFrame {
    crate::sidebar::frame::assemble_frame(panes, 1, "s")
}

fn first(frame: &crate::sidebar::frame::PaneFrame) -> &crate::sidebar::frame::PaneState {
    &frame.tabs[0].panes[0]
}

fn first_mut(
    frame: &mut crate::sidebar::frame::PaneFrame,
) -> &mut crate::sidebar::frame::PaneState {
    &mut frame.tabs[0].panes[0]
}

fn live_row_ids(frame: &crate::sidebar::frame::PaneFrame) -> Vec<String> {
    let workspace = crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    let snapshot =
        crate::SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), jiff::Timestamp::now())
            .with_live_panes(frame.to_pane_refs(), None);
    let mut ids = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter().map(|row| row.id.clone()))
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn write_snapshot_cache(path: &Path, session: &str, produced_at_ms: u64) {
    let cache = crate::sidebar::frame::assemble_frame(Vec::new(), produced_at_ms, session);
    atomic::write_temp_then_rename(path, &cache).expect("write snapshot cache");
}
