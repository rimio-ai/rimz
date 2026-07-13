use std::collections::{HashMap, HashSet};

use crate::ids::PaneId;
use crate::sidebar::frame::{CarriedPane, PaneFrame, PaneState, TabFrame};
use crate::sidebar::produce::metrics::PaneRootBinding;
use crate::sidebar::timing;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CarryOutcome {
    pub(super) frame: PaneFrame,
    pub(super) carried: Vec<CarriedPane>,
    pub(super) expired: Vec<ExpiredCarry>,
    pub(super) ambiguous_loss: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExpiredCarry {
    pub(super) pane_id: PaneId,
    pub(super) pid: Option<u32>,
    pub(super) carried_ms: u64,
}

#[derive(Clone, Debug)]
struct CarryDecision<'a> {
    tab: &'a TabFrame,
    pane: &'a PaneState,
    carried: CarriedPane,
}

#[derive(Clone, Copy)]
struct LiveEvidence {
    pid: u32,
    start_ticks: u64,
}

#[derive(Clone, Copy)]
enum Liveness {
    Live(LiveEvidence),
    DeadProven,
    Dead,
    Unknown,
}

struct ClassifiedPane<'a> {
    tab: &'a TabFrame,
    pane: &'a PaneState,
    prior_meta: Option<&'a CarriedPane>,
    verdict: Liveness,
    expired: Option<ExpiredCarry>,
}

struct CarryClassification<'a> {
    panes: Vec<ClassifiedPane<'a>>,
    decisions: HashMap<PaneId, CarryDecision<'a>>,
    confirmed_tabs: HashSet<crate::ids::ViewId>,
}

pub(super) fn apply_carry_forward(
    mut fresh: PaneFrame,
    prior: Option<&PaneFrame>,
    own_pane: Option<&PaneId>,
    bindings: &HashMap<PaneId, PaneRootBinding>,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
    now_ms: u64,
) -> CarryOutcome {
    let Some(prior) = prior else {
        return CarryOutcome {
            frame: fresh,
            carried: Vec::new(),
            expired: Vec::new(),
            ambiguous_loss: false,
        };
    };
    // A fully empty frame is already held by the publish verdict. Carrying here
    // would convert a hard mux failure into apparently-good data and hide the
    // existing empty-frame diagnostic path.
    if fresh.pane_states().next().is_none() {
        return CarryOutcome {
            frame: fresh,
            carried: Vec::new(),
            expired: Vec::new(),
            ambiguous_loss: false,
        };
    }

    let fresh_ids = fresh
        .pane_states()
        .map(|pane| pane.pane_id.clone())
        .collect::<HashSet<_>>();
    let prior_carried = prior
        .carried_panes
        .iter()
        .map(|carried| (carried.pane_id.clone(), carried))
        .collect::<HashMap<_, _>>();
    let missing = prior
        .tabs
        .iter()
        .flat_map(|tab| tab.panes.iter().map(move |pane| (tab, pane)))
        .filter(|(_, pane)| !fresh_ids.contains(&pane.pane_id))
        .collect::<Vec<_>>();

    let mut classification = classify_missing(
        &missing,
        &prior_carried,
        own_pane,
        bindings,
        read_start_ticks,
        now_ms,
    );
    apply_tab_coherence(&mut classification, now_ms);
    let ambiguous_loss = has_ambiguous_loss(&classification);
    let expired = classification
        .panes
        .iter()
        .filter_map(|record| record.expired.clone())
        .collect();

    let mut decisions = classification.decisions.into_values().collect::<Vec<_>>();
    decisions.sort_by_key(|decision| {
        (
            decision.tab.view_id.to_string(),
            decision.pane.pane_id.to_string(),
        )
    });
    let mut carried = decisions
        .iter()
        .map(|decision| decision.carried.clone())
        .collect::<Vec<_>>();
    carried.sort_by_key(|pane| pane.pane_id.to_string());

    for decision in &decisions {
        insert_carried_pane(&mut fresh, decision.tab, decision.pane);
    }
    fresh
        .tabs
        .sort_by(|left, right| left.view_id.cmp(&right.view_id));
    fresh.carried_panes = carried.clone();

    CarryOutcome {
        frame: fresh,
        carried,
        expired,
        ambiguous_loss,
    }
}

fn classify_missing<'a>(
    missing: &[(&'a TabFrame, &'a PaneState)],
    prior_carried: &HashMap<PaneId, &'a CarriedPane>,
    own_pane: Option<&PaneId>,
    bindings: &HashMap<PaneId, PaneRootBinding>,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
    now_ms: u64,
) -> CarryClassification<'a> {
    let mut classification = CarryClassification {
        panes: Vec::with_capacity(missing.len()),
        decisions: HashMap::new(),
        confirmed_tabs: HashSet::new(),
    };
    for &(tab, pane) in missing {
        let prior_meta = prior_carried.get(&pane.pane_id).copied();
        let (record, decision, confirmed) = classify_pane(
            tab,
            pane,
            prior_meta,
            own_pane,
            bindings,
            read_start_ticks,
            now_ms,
        );
        if confirmed {
            classification.confirmed_tabs.insert(tab.view_id.clone());
        }
        if let Some(decision) = decision {
            classification
                .decisions
                .insert(pane.pane_id.clone(), decision);
        }
        classification.panes.push(record);
    }
    classification
}

fn classify_pane<'a>(
    tab: &'a TabFrame,
    pane: &'a PaneState,
    prior_meta: Option<&'a CarriedPane>,
    own_pane: Option<&PaneId>,
    bindings: &HashMap<PaneId, PaneRootBinding>,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
    now_ms: u64,
) -> (ClassifiedPane<'a>, Option<CarryDecision<'a>>, bool) {
    // The producer runs inside its own pane, so that pane is provably alive
    // for as long as we are producing; its carry never expires.
    let is_own_pane = own_pane.is_some_and(|own| *own == pane.pane_id);
    let expired = if is_own_pane {
        None
    } else {
        expired_carry(&pane.pane_id, prior_meta, now_ms)
    };
    if expired.is_some() {
        return (
            ClassifiedPane {
                tab,
                pane,
                prior_meta,
                verdict: Liveness::Unknown,
                expired,
            },
            None,
            false,
        );
    }

    let verdict = scan_evidence(pane, prior_meta, bindings, read_start_ticks);
    let evidence = match verdict {
        Liveness::Live(evidence) => Some(evidence),
        Liveness::DeadProven | Liveness::Dead | Liveness::Unknown => None,
    };
    let carried = direct_carry(pane, prior_meta, evidence, is_own_pane, now_ms);
    (
        ClassifiedPane {
            tab,
            pane,
            prior_meta,
            verdict,
            expired,
        },
        carried.map(|carried| CarryDecision { tab, pane, carried }),
        evidence.is_some(),
    )
}

fn direct_carry(
    pane: &PaneState,
    prior_meta: Option<&CarriedPane>,
    evidence: Option<LiveEvidence>,
    is_own_pane: bool,
    now_ms: u64,
) -> Option<CarriedPane> {
    let carried_since_ms = prior_meta
        .map(|meta| meta.carried_since_ms)
        .unwrap_or(now_ms);
    if is_own_pane {
        return Some(CarriedPane {
            pane_id: pane.pane_id.clone(),
            pid: evidence.map(|evidence| evidence.pid).or(pane.current.pid),
            start_ticks: evidence.map(|evidence| evidence.start_ticks),
            carried_since_ms,
        });
    }
    evidence.map(|evidence| CarriedPane {
        pane_id: pane.pane_id.clone(),
        pid: Some(evidence.pid),
        start_ticks: Some(evidence.start_ticks),
        carried_since_ms,
    })
}

fn apply_tab_coherence(classification: &mut CarryClassification<'_>, now_ms: u64) {
    for record in &classification.panes {
        if classification.decisions.contains_key(&record.pane.pane_id)
            || !classification.confirmed_tabs.contains(&record.tab.view_id)
            || record.expired.is_some()
            || record
                .prior_meta
                .is_some_and(|meta| expired_at(meta.carried_since_ms, now_ms))
            || matches!(record.verdict, Liveness::DeadProven | Liveness::Dead)
        {
            continue;
        }
        let carried = CarriedPane {
            pane_id: record.pane.pane_id.clone(),
            pid: record.prior_meta.and_then(|meta| meta.pid),
            start_ticks: record.prior_meta.and_then(|meta| meta.start_ticks),
            carried_since_ms: record
                .prior_meta
                .map(|meta| meta.carried_since_ms)
                .unwrap_or(now_ms),
        };
        classification.decisions.insert(
            record.pane.pane_id.clone(),
            CarryDecision {
                tab: record.tab,
                pane: record.pane,
                carried,
            },
        );
    }
}

fn has_ambiguous_loss(classification: &CarryClassification<'_>) -> bool {
    classification.panes.iter().any(|record| {
        if classification.decisions.contains_key(&record.pane.pane_id) || record.expired.is_some() {
            return false;
        }
        match record.verdict {
            Liveness::DeadProven => false,
            Liveness::Dead if classification.confirmed_tabs.contains(&record.tab.view_id) => false,
            Liveness::Live(_) | Liveness::Dead | Liveness::Unknown => true,
        }
    })
}

fn scan_evidence(
    pane: &PaneState,
    prior_meta: Option<&CarriedPane>,
    bindings: &HashMap<PaneId, PaneRootBinding>,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
) -> Liveness {
    let mut dead_proven = false;
    let mut dead = false;
    if let Some(meta) = prior_meta
        && let (Some(pid), Some(start_ticks)) = (meta.pid, meta.start_ticks)
    {
        match read_start_ticks(pid) {
            Some(live_ticks) if live_ticks == start_ticks => {
                return Liveness::Live(LiveEvidence { pid, start_ticks });
            }
            Some(_) => dead_proven = true,
            None => dead = true,
        }
    }
    if let Some(binding) = bindings.get(&pane.pane_id) {
        match read_start_ticks(binding.pid) {
            Some(live_ticks) if live_ticks == binding.start_ticks => {
                return Liveness::Live(LiveEvidence {
                    pid: binding.pid,
                    start_ticks: binding.start_ticks,
                });
            }
            Some(_) => dead_proven = true,
            None => dead = true,
        }
    }
    if let Some(pid) = pane.current.pid {
        if let Some(start_ticks) = read_start_ticks(pid) {
            return Liveness::Live(LiveEvidence { pid, start_ticks });
        }
        dead = true;
    }
    if dead_proven {
        Liveness::DeadProven
    } else if dead {
        Liveness::Dead
    } else {
        Liveness::Unknown
    }
}

fn expired_carry(
    pane_id: &PaneId,
    prior_meta: Option<&CarriedPane>,
    now_ms: u64,
) -> Option<ExpiredCarry> {
    let prior_meta = prior_meta?;
    expired_at(prior_meta.carried_since_ms, now_ms).then(|| ExpiredCarry {
        pane_id: pane_id.clone(),
        pid: prior_meta.pid,
        carried_ms: now_ms.saturating_sub(prior_meta.carried_since_ms),
    })
}

pub(super) fn expired_at(carried_since_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(carried_since_ms) > timing::pane_carry_ttl().as_millis() as u64
}

fn insert_carried_pane(fresh: &mut PaneFrame, prior_tab: &TabFrame, prior_pane: &PaneState) {
    let tab = match fresh
        .tabs
        .iter_mut()
        .find(|tab| tab.view_id == prior_tab.view_id)
    {
        Some(tab) => tab,
        None => {
            fresh.tabs.push(TabFrame {
                view_id: prior_tab.view_id.clone(),
                kind: prior_tab.kind,
                name: prior_tab.name.clone(),
                panes: Vec::new(),
            });
            fresh.tabs.last_mut().expect("just pushed tab")
        }
    };
    if !tab
        .panes
        .iter()
        .any(|pane| pane.pane_id == prior_pane.pane_id)
    {
        tab.panes.push(prior_pane.clone());
    }
}

#[cfg(test)]
mod tests {
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
    fn live_prior_pid_carries_a_dropped_pane() {
        let prior = pidded_frame(&[("terminal_1", 101), ("terminal_2", 202)], 1);
        let fresh = frame(&["terminal_1"], 2);

        let outcome = apply_carry_forward(
            fresh,
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

        let outcome = apply_carry_forward(fresh, Some(&prior), None, &HashMap::new(), &|_| None, 2);

        assert_eq!(outcome.frame.pane_states().count(), 1);
        assert!(outcome.carried.is_empty());
        assert!(outcome.ambiguous_loss);
    }

    #[test]
    fn pidless_drop_without_liveness_evidence_is_ambiguous() {
        let prior = frame(&["terminal_1", "terminal_2"], 1);
        let fresh = frame(&["terminal_1"], 2);

        let outcome = apply_carry_forward(fresh, Some(&prior), None, &HashMap::new(), &|_| None, 2);

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
        let no_prior = apply_carry_forward(fresh, None, None, &HashMap::new(), &|_| None, 1);
        assert!(!no_prior.ambiguous_loss);

        let prior = frame(&["terminal_1"], 1);
        let empty = frame(&[], 2);
        let empty_fresh =
            apply_carry_forward(empty, Some(&prior), None, &HashMap::new(), &|_| None, 2);
        assert!(!empty_fresh.ambiguous_loss);
    }

    #[test]
    fn omitted_tab_is_recreated_from_prior_metadata() {
        let mut prior = PaneFrame {
            produced_at_ms: 1,
            observed_at_ms: 1,
            build: None,
            session_name: "s".to_owned(),
            tabs: vec![TabFrame {
                view_id: ViewId::new_unchecked("tab_9"),
                kind: ViewKind::Tab,
                name: Some("work".to_owned()),
                panes: vec![PaneState {
                    pane_id: pane_id("terminal_9"),
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
            focused_pane: None,
            presence: None,
        };
        let fresh = frame(&["terminal_1"], 2);
        prior.tabs[0].panes[0].current.pid = Some(909);

        let outcome = apply_carry_forward(
            fresh,
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
}
