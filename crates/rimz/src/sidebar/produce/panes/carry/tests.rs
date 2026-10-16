use super::*;

use crate::ids::{MuxName, ViewId, ViewKind};
use crate::sidebar::frame::{PaneProcess, PaneState};
use crate::sidebar::produce::test_support::pane;
use crate::sidebar::timing::PANE_CARRY_TTL;

fn frame(raws: &[&str], produced_at_ms: u64) -> PaneFrame {
    let panes = raws
        .iter()
        .map(|raw| same_tab_pane(raw, None))
        .collect::<Vec<_>>();
    crate::sidebar::frame::assemble_frame(panes, produced_at_ms, "s")
}

fn pidded_frame(raws: &[(&str, u32)], produced_at_ms: u64) -> PaneFrame {
    let panes = raws
        .iter()
        .map(|(raw, pid)| {
            let mut pane = same_tab_pane(raw, None);
            pane.pane_pid = Some(*pid);
            pane
        })
        .collect::<Vec<_>>();
    crate::sidebar::frame::assemble_frame(panes, produced_at_ms, "s")
}

fn same_tab_pane(raw: &str, tab: Option<&str>) -> crate::pane::PaneRef {
    let mut pane = pane(raw, Some("zsh"), Some("/repo"));
    pane.view_id = Some(tab.unwrap_or("tab_0").to_owned());
    pane.view_kind = Some(ViewKind::Tab);
    pane
}

fn pane_id(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, raw)
}

#[test]
fn authoritative_whole_tab_omission_carries_live_and_coherent_panes() {
    let mut live = same_tab_pane("terminal_2", Some("tab_9"));
    live.pane_pid = Some(202);
    let pidless = same_tab_pane("terminal_3", Some("tab_9"));
    let prior = crate::sidebar::frame::assemble_frame(vec![live, pidless], 1, "s");
    let fresh = frame(&["terminal_1"], 2);

    let outcome = apply_carry_forward(
        fresh,
        true,
        Some(&prior),
        None,
        &HashMap::new(),
        &|pid| (pid == 202).then_some(9),
        2,
    );

    assert_eq!(outcome.carried.len(), 2);
    assert_eq!(outcome.frame.tabs.len(), 2);
    let tab = outcome
        .frame
        .tabs
        .iter()
        .find(|tab| tab.view_id.as_str() == "tab_9")
        .unwrap();
    assert_eq!(tab.panes, prior.tabs[0].panes);
    assert_eq!(outcome.carried[0].pid, Some(202));
    assert_eq!(outcome.carried[1].pid, None);
    assert!(outcome.expired.is_empty());
    assert!(!outcome.ambiguous_loss);
}

#[test]
fn authoritative_whole_tab_omission_without_liveness_is_ambiguous() {
    for pid in [Some(202), None] {
        let mut missing = same_tab_pane("terminal_2", Some("tab_9"));
        missing.pane_pid = pid;
        let prior = crate::sidebar::frame::assemble_frame(vec![missing], 1, "s");
        let outcome = apply_carry_forward(
            frame(&["terminal_1"], 2),
            true,
            Some(&prior),
            None,
            &HashMap::new(),
            &|_| None,
            2,
        );

        assert!(outcome.ambiguous_loss, "missing tab with pid {pid:?}");
        assert!(outcome.carried.is_empty());
        assert!(outcome.expired.is_empty());
    }
}

#[test]
fn authoritative_omission_refutes_live_and_pidless_siblings() {
    for pid in [Some(202), None] {
        let mut prior = pidded_frame(&[("terminal_1", 101), ("terminal_2", 202)], 1);
        prior
            .pane_states_mut()
            .find(|pane| pane.pane_id == pane_id("terminal_2"))
            .unwrap()
            .current
            .pid = pid;
        prior.carried_panes = vec![CarriedPane {
            pane_id: pane_id("terminal_2"),
            pid: Some(202),
            start_ticks: Some(9),
            carried_since_ms: 1,
        }];
        let fresh = frame(&["terminal_1"], 2);
        let outcome = apply_carry_forward(
            fresh,
            true,
            Some(&prior),
            Some(&pane_id("terminal_1")),
            &HashMap::new(),
            &|_| Some(9),
            2,
        );
        assert_eq!(outcome.frame.pane_states().count(), 1);
        assert!(outcome.carried.is_empty());
        assert!(outcome.expired.is_empty());
        assert!(!outcome.ambiguous_loss);
    }
}

#[test]
fn authoritative_own_carry_does_not_restore_pidless_tab_sibling() {
    let mut prior = pidded_frame(&[("terminal_1", 101), ("terminal_2", 202)], 1);
    prior
        .pane_states_mut()
        .find(|pane| pane.pane_id == pane_id("terminal_2"))
        .unwrap()
        .current
        .pid = None;
    let fresh = frame(&["terminal_3"], 2);
    let outcome = apply_carry_forward(
        fresh,
        true,
        Some(&prior),
        Some(&pane_id("terminal_1")),
        &HashMap::new(),
        &|_| Some(9),
        2,
    );
    assert_eq!(outcome.frame.pane_states().count(), 2);
    assert_eq!(outcome.carried.len(), 1);
    assert_eq!(outcome.carried[0].pane_id, pane_id("terminal_1"));
    assert!(outcome.expired.is_empty());
    assert!(!outcome.ambiguous_loss);
}

#[test]
fn live_prior_pid_carries_a_dropped_pane() {
    let prior = pidded_frame(&[("terminal_1", 101), ("terminal_2", 202)], 1);
    let fresh = frame(&["terminal_1"], 2);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        None,
        &HashMap::new(),
        &|pid| (pid == 202).then_some(9),
        2,
    );

    assert_eq!(outcome.frame.pane_states().count(), 2);
    assert_eq!(outcome.carried.len(), 1);
    assert_eq!(outcome.carried[0].pane_id, pane_id("terminal_2"));
    assert_eq!(outcome.carried[0].pid, Some(202));
    assert_eq!(outcome.carried[0].start_ticks, Some(9));
    assert!(!outcome.ambiguous_loss);
}

#[test]
fn unreadable_prior_pid_drops_the_pane_as_ambiguous() {
    let prior = pidded_frame(&[("terminal_1", 101), ("terminal_2", 202)], 1);
    let fresh = frame(&["terminal_1"], 2);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        None,
        &HashMap::new(),
        &|_| None,
        2,
    );

    assert_eq!(outcome.frame.pane_states().count(), 1);
    assert!(outcome.carried.is_empty());
    assert!(outcome.ambiguous_loss);
}

#[test]
fn pidless_drop_without_liveness_evidence_is_ambiguous() {
    let prior = frame(&["terminal_1", "terminal_2"], 1);
    let fresh = frame(&["terminal_1"], 2);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        None,
        &HashMap::new(),
        &|_| None,
        2,
    );

    assert_eq!(outcome.frame.pane_states().count(), 1);
    assert!(outcome.carried.is_empty());
    assert!(outcome.ambiguous_loss);
}

#[test]
fn dead_prior_carried_pid_is_not_ambiguous() {
    let mut prior = frame(&["terminal_1", "terminal_2"], 1);
    prior.carried_panes = vec![CarriedPane {
        pane_id: pane_id("terminal_2"),
        pid: Some(202),
        start_ticks: Some(9),
        carried_since_ms: 1,
    }];
    let fresh = frame(&["terminal_1"], 2);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        None,
        &HashMap::new(),
        &|pid| (pid == 202).then_some(10),
        2,
    );

    assert_eq!(outcome.frame.pane_states().count(), 1);
    assert!(outcome.carried.is_empty());
    assert!(!outcome.ambiguous_loss);
}

#[test]
fn cached_binding_carries_pidless_zellij_pane() {
    let prior = frame(&["terminal_1", "terminal_2"], 1);
    let fresh = frame(&["terminal_1"], 2);
    let bindings = HashMap::from([(
        pane_id("terminal_2"),
        PaneRootBinding {
            pid: 202,
            start_ticks: 9,
        },
    )]);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        None,
        &bindings,
        &|pid| (pid == 202).then_some(9),
        2,
    );

    assert_eq!(outcome.frame.pane_states().count(), 2);
    assert_eq!(outcome.carried[0].pid, Some(202));
    assert!(!outcome.ambiguous_loss);
}

#[test]
fn start_tick_mismatch_blocks_cached_binding() {
    let prior = frame(&["terminal_1", "terminal_2"], 1);
    let fresh = frame(&["terminal_1"], 2);
    let bindings = HashMap::from([(
        pane_id("terminal_2"),
        PaneRootBinding {
            pid: 202,
            start_ticks: 9,
        },
    )]);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        None,
        &bindings,
        &|pid| (pid == 202).then_some(10),
        2,
    );

    assert_eq!(outcome.frame.pane_states().count(), 1);
    assert!(outcome.carried.is_empty());
    assert!(!outcome.ambiguous_loss);
}

#[test]
fn previous_carried_since_survives_until_ttl_then_expires() {
    let mut prior = pidded_frame(&[("terminal_1", 101), ("terminal_2", 202)], 1);
    prior.carried_panes = vec![CarriedPane {
        pane_id: pane_id("terminal_2"),
        pid: Some(202),
        start_ticks: Some(9),
        carried_since_ms: 1,
    }];
    let fresh = frame(&["terminal_1"], 2);

    let at_ttl = apply_carry_forward(
        fresh.clone(),
        false,
        Some(&prior),
        None,
        &HashMap::new(),
        &|pid| (pid == 202).then_some(9),
        1 + PANE_CARRY_TTL.as_millis() as u64,
    );

    assert_eq!(at_ttl.carried[0].carried_since_ms, 1);
    assert!(at_ttl.expired.is_empty());
    assert!(!at_ttl.ambiguous_loss);

    let expired = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        None,
        &HashMap::new(),
        &|pid| (pid == 202).then_some(9),
        2 + PANE_CARRY_TTL.as_millis() as u64,
    );

    assert_eq!(expired.frame.pane_states().count(), 1);
    assert!(expired.carried.is_empty());
    assert_eq!(expired.expired[0].pane_id, pane_id("terminal_2"));
    assert!(!expired.ambiguous_loss);
}

#[test]
fn own_pane_carry_never_expires() {
    let mut prior = frame(&["terminal_1", "terminal_2"], 1);
    prior.carried_panes = vec![CarriedPane {
        pane_id: pane_id("terminal_1"),
        pid: Some(101),
        start_ticks: Some(9),
        carried_since_ms: 1,
    }];
    let fresh = frame(&["terminal_2"], 2);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        Some(&pane_id("terminal_1")),
        &HashMap::new(),
        &|_| None,
        2 + PANE_CARRY_TTL.as_millis() as u64,
    );

    assert_eq!(outcome.frame.pane_states().count(), 2);
    assert_eq!(outcome.carried[0].pane_id, pane_id("terminal_1"));
    assert!(outcome.expired.is_empty());
    assert!(!outcome.ambiguous_loss);
}

#[test]
fn same_tab_pidless_sibling_carries_for_coherence() {
    let mut prior = pidded_frame(
        &[
            ("terminal_1", 101),
            ("terminal_2", 202),
            ("terminal_3", 303),
        ],
        1,
    );
    prior
        .pane_states_mut()
        .find(|pane| pane.pane_id.raw() == "terminal_3")
        .expect("terminal_3 present")
        .current
        .pid = None;
    let fresh = frame(&["terminal_1"], 2);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        None,
        &HashMap::new(),
        &|pid| (pid == 202).then_some(9),
        2,
    );

    let ids = outcome
        .carried
        .iter()
        .map(|carried| carried.pane_id.raw().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["terminal_2", "terminal_3"]);
    assert_eq!(
        outcome
            .carried
            .iter()
            .find(|carried| carried.pane_id.raw() == "terminal_3")
            .and_then(|carried| carried.pid),
        None
    );
    assert!(!outcome.ambiguous_loss);
}

#[test]
fn same_tab_dead_sibling_does_not_carry_for_coherence() {
    let prior = pidded_frame(
        &[
            ("terminal_1", 101),
            ("terminal_2", 202),
            ("terminal_3", 303),
        ],
        1,
    );
    let fresh = frame(&["terminal_1"], 2);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        None,
        &HashMap::new(),
        &|pid| (pid == 202).then_some(9),
        2,
    );

    let ids = outcome
        .carried
        .iter()
        .map(|carried| carried.pane_id.raw().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["terminal_2"]);
    assert!(!outcome.ambiguous_loss);
}

#[test]
fn omitted_own_pane_carries_even_without_proc_evidence() {
    let prior = frame(&["terminal_1", "terminal_2"], 1);
    let fresh = frame(&["terminal_2"], 2);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        Some(&pane_id("terminal_1")),
        &HashMap::new(),
        &|_| None,
        2,
    );

    assert_eq!(outcome.frame.pane_states().count(), 2);
    assert_eq!(outcome.carried[0].pane_id, pane_id("terminal_1"));
    assert!(!outcome.ambiguous_loss);
}

#[test]
fn no_prior_and_empty_fresh_are_not_ambiguous() {
    let fresh = frame(&["terminal_1"], 1);
    let no_prior = apply_carry_forward(fresh, false, None, None, &HashMap::new(), &|_| None, 1);
    assert!(!no_prior.ambiguous_loss);

    let prior = frame(&["terminal_1"], 1);
    let empty = frame(&[], 2);
    let empty_fresh = apply_carry_forward(
        empty,
        false,
        Some(&prior),
        None,
        &HashMap::new(),
        &|_| None,
        2,
    );
    assert!(!empty_fresh.ambiguous_loss);
}

#[test]
fn omitted_tab_is_recreated_from_prior_metadata() {
    let mut prior = PaneFrame {
        focus_origin: Default::default(),
        client_view_fresh: false,
        client_sample_withheld: false,
        topology_stamp_ms: None,
        produced_at_ms: 1,
        observed_at_ms: 1,
        build: None,
        session_name: "s".to_owned(),
        tabs: vec![TabFrame {
            view_id: ViewId::new_unchecked("tab_9"),
            kind: ViewKind::Tab,
            name: Some("work".to_owned()),
            naming: Default::default(),
            panes: vec![PaneState {
                pane_id: pane_id("terminal_9"),
                title: None,
                first_seen_at_ms: Some(1),
                hosted_carry_since_ms: None,
                is_floating: false,
                current: PaneProcess {
                    pid: Some(909),
                    command: Some("zsh".to_owned()),
                    foreground_cmdline: None,
                    spawn_command: None,
                    cwd: Some("/repo".to_owned()),
                    started_at: None,
                    hosted_agent_kind: None,
                    hosted_agent_process_start: None,
                    hosted_agent_lineage: Vec::new(),
                    resumed_session_id: None,
                    elevated_agent: None,
                },
                previous: None,
                children: Vec::new(),
                metrics: Default::default(),
            }],
        }],
        carried_panes: Vec::new(),
        viewed_panes: Vec::new(),
        client_views: Vec::new(),
        focused_pane: None,
        presence: None,
    };
    let fresh = frame(&["terminal_1"], 2);
    prior.tabs[0].panes[0].current.pid = Some(909);

    let outcome = apply_carry_forward(
        fresh,
        false,
        Some(&prior),
        None,
        &HashMap::new(),
        &|pid| (pid == 909).then_some(90),
        2,
    );

    let carried_tab = outcome
        .frame
        .tabs
        .iter()
        .find(|tab| tab.view_id.as_str() == "tab_9")
        .expect("prior tab recreated");
    assert_eq!(carried_tab.name.as_deref(), Some("work"));
    assert!(
        carried_tab
            .panes
            .iter()
            .any(|pane| pane.pane_id == pane_id("terminal_9"))
    );
}
