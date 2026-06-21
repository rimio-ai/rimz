use super::*;

use crate::agents::AgentStatus;
use crate::ids::{MuxName, PaneId};
use crate::ledger::snapshot::testkit::{AgentStateFx, agent, ago, pane};

fn pane_cmd(raw: &str, view: &str, command: &str, view_name: Option<&str>) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some(view.to_owned()),
        view_kind: Some(crate::ids::ViewKind::Tab),
        view_name: view_name.map(str::to_owned),
        is_focused: false,
        is_floating: false,
        command: Some(command.to_owned()),
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

#[test]
fn dead_stamped_agent_repairs_to_live_cwd_pane() {
    let mut agent = agent("codex", "sess", AgentStatus::Success, 2_000)
        .worktree("/repo/main")
        .in_pane("%7")
        .active_ago(10);
    agent.registered_at = Some(ago(12));
    let live = PaneRef {
        pane_process_start: Some(ago(3_600)),
        ..pane("%0", "codex", "/repo/main")
    };

    let result = compute_lazy_agent_pairings(&[live], &[agent]);

    assert_eq!(
        result
            .pairings
            .get(&PaneId::from_parts(MuxName::Tmux, "%0")),
        Some(&0)
    );
}

#[test]
fn live_stamped_agent_stays_out_of_lazy_pairings() {
    let mut agent = agent("codex", "sess", AgentStatus::Success, 2_000)
        .worktree("/repo/main")
        .in_pane("%0")
        .active_ago(10);
    agent.registered_at = Some(ago(12));
    let live = PaneRef {
        pane_process_start: Some(ago(3_600)),
        ..pane("%0", "codex", "/repo/main")
    };

    let result = compute_lazy_agent_pairings(&[live], &[agent]);

    assert!(
        !result
            .pairings
            .contains_key(&PaneId::from_parts(MuxName::Tmux, "%0"))
    );
}

#[test]
fn lazy_pairing_diagnostics_record_ambiguous_start_proximity_choice() {
    let mut newer = agent("codex", "sess-new", AgentStatus::Running, 2_000)
        .worktree("/repo/main")
        .active_ago(1);
    newer.registered_at = Some(ago(8));
    let old_pane = PaneRef {
        pane_process_start: Some(ago(3_600)),
        ..pane_cmd("terminal_4", "tab_0", "codex", None)
    };
    let new_pane = PaneRef {
        pane_process_start: Some(ago(9)),
        ..pane_cmd("terminal_58", "tab_0", "codex", None)
    };

    let diagnostics = lazy_agent_pairing_diagnostics(&[old_pane, new_pane], &[newer]);

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].method,
        LazyAgentPairingMethod::StartProximity
    );
    assert_eq!(diagnostics[0].selected_pane.raw(), "terminal_58");
    assert_eq!(diagnostics[0].candidates.len(), 2);
}
