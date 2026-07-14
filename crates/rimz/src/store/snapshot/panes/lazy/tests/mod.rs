use super::*;

use crate::agents::{AgentState, AgentStatus};
use crate::ids::{MuxName, PaneId};
use crate::store::snapshot::testkit::{AgentStateFx, agent, ago, pane};
use std::collections::{BTreeMap, BTreeSet, HashMap};

mod hook_recovery;

fn pane_cmd(raw: &str, view: &str, command: &str, view_name: Option<&str>) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some(view.to_owned()),
        view_kind: Some(crate::ids::ViewKind::Tab),
        view_name: view_name.map(str::to_owned),
        title: None,
        is_focused: false,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
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
fn wired_non_lazy_pane_synthesizes_idle_row() {
    let pane = pane("term1", "claude", "/repo/main");
    let agents: Vec<AgentState> = Vec::new();
    let pairings = LazyAgentPairingResult {
        pairings: HashMap::new(),
        diagnostics: Vec::new(),
    };
    let bound = BTreeSet::new();
    let default_models = BTreeMap::new();

    assert!(
        agent_pane_for_pane(
            &pane,
            &agents,
            &pairings,
            &bound,
            &[],
            &default_models,
            ago(0),
        )
        .is_none(),
        "unwired panes stay process rows",
    );

    match agent_pane_for_pane(
        &pane,
        &agents,
        &pairings,
        &bound,
        &["claude".to_owned()],
        &default_models,
        ago(0),
    ) {
        Some(AgentPaneRow::Idle(row)) => {
            assert_eq!(row.name, "claude");
            assert_eq!(row.status(), Some(AgentStatus::Idle));
            assert!(row.as_agent().is_some());
        }
        _ => panic!("wired sessionless Claude should synthesize idle row"),
    }
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

#[test]
fn lazy_agent_pairing_uses_wrapper_manifest_worktree_fallback() {
    let session = agent("codex", "sess", AgentStatus::Running, 2_000).worktree("/repo/main");
    let wrapped = PaneRef {
        command: Some("/bin/rimz agents exec codex --worktree-path /repo/main".to_owned()),
        spawn_command: Some("/bin/rimz agents exec codex --worktree-path /repo/main".to_owned()),
        cwd: None,
        ..pane("%0", "codex", "/ignored")
    };

    let result = compute_lazy_agent_pairings(&[wrapped], &[session]);

    assert_eq!(
        result
            .pairings
            .get(&PaneId::from_parts(MuxName::Tmux, "%0")),
        Some(&0)
    );
}
