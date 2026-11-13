use super::*;

use crate::agents::{AgentState, AgentStatus};
use crate::ids::{MuxName, PaneId};
use crate::store::snapshot::panes::{PaneBinder, PaneBindingDisposition, PaneWiring};
use crate::store::snapshot::testkit::{AgentStateFx, agent, ago, pane};
use std::collections::{BTreeMap, BTreeSet, HashMap};

mod hook_recovery;
mod not_tracked;

fn pane_cmd(raw: &str, view: &str, command: &str, view_name: Option<&str>) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some(view.to_owned()),
        view_kind: Some(crate::ids::ViewKind::Tab),
        view_name: view_name.map(str::to_owned),
        title: None,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        hosted_agent_lineage: Vec::new(),
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
    let mut pane = pane("term1", "claude", "/repo/main");
    pane.pane_process_start = None;
    pane.first_seen_at_ms = Some(ago(3600).as_millisecond().try_into().unwrap());
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
            &crate::store::snapshot::panes::PaneWiring {
                kinds: &[],
                default_models: &default_models,
                untracked: &BTreeMap::new(),
                own_lanes: &BTreeMap::new(),
            },
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
        &crate::store::snapshot::panes::PaneWiring {
            kinds: &["claude".to_owned()],
            default_models: &default_models,
            untracked: &BTreeMap::new(),
            own_lanes: &BTreeMap::new(),
        },
        ago(0),
    ) {
        Some(AgentPaneRow::Idle(row)) => {
            assert_eq!(row.name, "claude");
            assert_eq!(row.status(), Some(AgentStatus::Idle));
            assert!(row.as_agent().is_some());
            assert_eq!(row.last_activity, ago(3600));
        }
        _ => panic!("wired sessionless Claude should synthesize idle row"),
    }
}

#[test]
fn untracked_mini_panes_only_have_process_rows() {
    let wired = vec!["opencode".to_owned()];
    let models = BTreeMap::new();
    let pairings = compute_lazy_agent_pairings(&[], &[]);
    let cases = [
        (MuxName::Tmux, "opencode", Some("opencode mini"), false),
        (MuxName::Zellij, "opencode mini", None, false),
        (MuxName::Zellij, "opencode mini --standalone", None, true),
        (MuxName::Tmux, "opencode", None, true),
        (MuxName::Zellij, "opencode run mini", None, true),
        (
            MuxName::Tmux,
            "opencode",
            Some("/usr/bin/opencode --quiet mini"),
            false,
        ),
        (
            MuxName::Tmux,
            "opencode",
            Some("/usr/bin/opencode mini --standalone"),
            true,
        ),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(mux, command, foreground, _)| {
            let pane = PaneRef {
                foreground_cmdline: foreground.map(str::to_owned),
                hosted_agent_kind: Some(AgentKind::new_unchecked("opencode")),
                ..match mux {
                    MuxName::Tmux => pane("%1", command, "/repo/main"),
                    MuxName::Zellij => pane_cmd("terminal_1", "tab_0", command, None),
                }
            };
            let wiring = PaneWiring {
                kinds: &wired,
                default_models: &models,
                untracked: &BTreeMap::new(),
                own_lanes: &BTreeMap::new(),
            };
            let idle = matches!(
                agent_pane_for_pane(&pane, &[], &pairings, &BTreeSet::new(), &wiring, ago(0)),
                Some(AgentPaneRow::Idle(_))
            );
            let frame = row_from_frame_pane(&pane, &wiring, ago(0)).unwrap();
            let mut binder =
                PaneBinder::new(PaneBindingIndex::new(&[]), &pairings, wiring, None, ago(0));
            let process = matches!(binder.resolve(&pane), PaneBindingDisposition::Process);
            (idle, process, frame.as_agent().is_some())
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, _, _, tracked)| (*tracked, !*tracked, *tracked))
        .collect();
    assert_eq!(actual, expected, "{cases:?}");
}

#[test]
fn a_real_mini_session_wins_over_the_process_fallback() {
    let pane = PaneRef {
        foreground_cmdline: Some("opencode mini".to_owned()),
        ..pane("%1", "opencode", "/repo/main")
    };
    let agents =
        vec![agent("opencode", "ses_live", AgentStatus::Running, 2_000).worktree("/repo/main")];
    let pairings = compute_lazy_agent_pairings(std::slice::from_ref(&pane), &agents);
    let row = agent_pane_for_pane(
        &pane,
        &agents,
        &pairings,
        &BTreeSet::new(),
        &PaneWiring {
            kinds: &["opencode".to_owned()],
            default_models: &BTreeMap::new(),
            untracked: &BTreeMap::new(),
            own_lanes: &BTreeMap::new(),
        },
        ago(0),
    );
    assert!(
        matches!(row, Some(AgentPaneRow::Agent(agent)) if agent.agent_id == agents[0].agent_id),
        "{:?}",
        pairings.pairings
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
