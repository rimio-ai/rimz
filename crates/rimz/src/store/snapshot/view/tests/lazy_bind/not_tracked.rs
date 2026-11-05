use super::*;
use crate::agents::{NotTracked, NotTrackedReason};

fn reason() -> NotTracked {
    NotTracked {
        reason: NotTrackedReason::HooksBlocked,
        fix: "enable hooks, then restart".to_owned(),
    }
}

#[test]
fn hook_reason_changes_only_the_row_not_wired_pane_identity() {
    let actual: Vec<_> = [false, true]
        .into_iter()
        .map(|wired| {
            let mut snapshot = room(Vec::new());
            snapshot
                .untracked_kinds
                .insert("qoder".to_owned(), reason());
            if wired {
                snapshot.wired_kinds = vec!["qoder".to_owned()];
            }
            let mut live = pane("term1", "qodercli", "/repo/main");
            live.pane_process_start = Some(ago(60));
            let expected = crate::store::snapshot::process::row_from_process(&live, snapshot.now);
            assert_ne!(
                expected.name, "qoder",
                "the label must differ from the pane kind"
            );
            let snapshot = snapshot.with_live_panes(vec![live], None);
            let rows = rows(&snapshot);
            let row = rows[0];
            (
                row.is_process(),
                row.as_process().and_then(|card| card.not_tracked.clone()),
                row.id == expected.id
                    && row.name == expected.name
                    && row.last_activity == expected.last_activity
                    && row.worktree_path == expected.worktree_path,
                snapshot
                    .agent_panes
                    .iter()
                    .map(|pane| (pane.kind.to_string(), pane.agent_id.clone()))
                    .collect::<Vec<_>>(),
                snapshot
                    .worktree_groups
                    .iter()
                    .all(|group| group.status_counts.is_empty()),
            )
        })
        .collect();
    assert_eq!(
        actual,
        vec![
            (true, Some(reason()), true, vec![], true),
            (
                true,
                Some(reason()),
                true,
                vec![("qoder".to_owned(), None)],
                true
            ),
        ]
    );
}

#[test]
fn paired_session_beats_a_hook_reason() {
    let mut snapshot = room(vec![paneless_codex("sess-1", "/repo/main", 1_000)]);
    snapshot
        .untracked_kinds
        .insert("codex".to_owned(), reason());
    snapshot.wired_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);
    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].id, "sess-1");
    assert_eq!(snapshot.agent_panes[0].agent_id.as_deref(), Some("sess-1"));
}

#[test]
fn hook_blocked_mini_panes_have_rows_without_sessionless_identity() {
    let actual: Vec<_> = ["opencode mini", "opencode mini --standalone"]
        .into_iter()
        .map(|command| {
            let mut snapshot = room(Vec::new());
            snapshot.wired_kinds = vec!["opencode".to_owned()];
            snapshot
                .untracked_kinds
                .insert("opencode".to_owned(), reason());
            let mut live = pane("term1", "opencode", "/repo/main");
            live.foreground_cmdline = Some(command.to_owned());
            let snapshot = snapshot.with_live_panes(vec![live], None);
            let rows = rows(&snapshot);
            assert_eq!(rows.len(), 1, "{command}");
            (
                rows[0]
                    .as_process()
                    .and_then(|card| card.not_tracked.clone()),
                snapshot
                    .agent_panes
                    .iter()
                    .map(|pane| (pane.kind.to_string(), pane.agent_id.clone()))
                    .collect::<Vec<_>>(),
                snapshot
                    .worktree_groups
                    .iter()
                    .all(|group| group.status_counts.is_empty()),
            )
        })
        .collect();
    assert_eq!(
        actual,
        [
            (Some(reason()), vec![], true),
            (Some(reason()), vec![("opencode".to_owned(), None)], true,),
        ]
    );
}

#[test]
fn non_agent_and_floating_panes_do_not_gain_rows_or_identity() {
    let actual: Vec<_> = [false, true]
        .into_iter()
        .map(|floating| {
            let mut snapshot = room(Vec::new());
            snapshot
                .untracked_kinds
                .insert("codex".to_owned(), reason());
            let mut live = pane(
                "term1",
                if floating { "codex" } else { "zsh" },
                "/repo/main",
            );
            live.is_floating = floating;
            let snapshot = snapshot.with_live_panes(vec![live], None);
            (
                rows(&snapshot)
                    .iter()
                    .filter_map(|row| row.as_process())
                    .all(|card| card.not_tracked.is_none()),
                rows(&snapshot).len(),
                snapshot.agent_panes.len(),
            )
        })
        .collect();
    assert_eq!(actual, [(true, 1, 0), (true, 0, 0)]);
}
