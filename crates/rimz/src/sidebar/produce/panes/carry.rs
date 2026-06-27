use std::collections::{HashMap, HashSet};

use crate::ids::PaneId;
use crate::sidebar::frame::{CarriedPane, PaneFrame, PaneState, TabFrame};
use crate::sidebar::produce::metrics::PaneRootBinding;
use crate::sidebar::timing::PANE_CARRY_TTL;

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

    let mut expired = Vec::new();
    let mut decisions: HashMap<PaneId, CarryDecision<'_>> = HashMap::new();
    let mut confirmed_tabs = HashSet::new();

    for (tab, pane) in &missing {
        let prior_meta = prior_carried.get(&pane.pane_id).copied();
        if let Some(expired_meta) = expired_carry(&pane.pane_id, prior_meta, now_ms) {
            expired.push(expired_meta);
            continue;
        }
        let Some((carried, confirmed_pid)) = direct_carry_metadata(
            pane,
            prior_meta,
            own_pane,
            bindings,
            read_start_ticks,
            now_ms,
        ) else {
            continue;
        };
        if confirmed_pid {
            confirmed_tabs.insert(tab.view_id.clone());
        }
        decisions.insert(pane.pane_id.clone(), CarryDecision { tab, pane, carried });
    }

    for (tab, pane) in &missing {
        if decisions.contains_key(&pane.pane_id) || !confirmed_tabs.contains(&tab.view_id) {
            continue;
        }
        let prior_meta = prior_carried.get(&pane.pane_id).copied();
        if prior_meta.is_some_and(|meta| expired_at(meta.carried_since_ms, now_ms)) {
            continue;
        }
        if has_dead_liveness_evidence(pane, prior_meta, bindings, read_start_ticks) {
            continue;
        }
        let carried = CarriedPane {
            pane_id: pane.pane_id.clone(),
            pid: prior_meta.and_then(|meta| meta.pid),
            start_ticks: prior_meta.and_then(|meta| meta.start_ticks),
            carried_since_ms: prior_meta
                .map(|meta| meta.carried_since_ms)
                .unwrap_or(now_ms),
        };
        decisions.insert(pane.pane_id.clone(), CarryDecision { tab, pane, carried });
    }

    let ambiguous_loss = missing.iter().any(|(tab, pane)| {
        !decisions.contains_key(&pane.pane_id)
            && !expired
                .iter()
                .any(|expired| expired.pane_id == pane.pane_id)
            && !has_authoritative_dead_liveness_evidence(
                pane,
                prior_carried.get(&pane.pane_id).copied(),
                bindings,
                read_start_ticks,
            )
            && !(confirmed_tabs.contains(&tab.view_id)
                && has_dead_liveness_evidence(
                    pane,
                    prior_carried.get(&pane.pane_id).copied(),
                    bindings,
                    read_start_ticks,
                ))
    });

    let mut decisions = decisions.into_values().collect::<Vec<_>>();
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

fn direct_carry_metadata(
    pane: &PaneState,
    prior_meta: Option<&CarriedPane>,
    own_pane: Option<&PaneId>,
    bindings: &HashMap<PaneId, PaneRootBinding>,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
    now_ms: u64,
) -> Option<(CarriedPane, bool)> {
    let carried_since_ms = prior_meta
        .map(|meta| meta.carried_since_ms)
        .unwrap_or(now_ms);

    if own_pane.is_some_and(|own| *own == pane.pane_id) {
        let evidence = liveness_evidence(pane, prior_meta, bindings, read_start_ticks);
        return Some((
            CarriedPane {
                pane_id: pane.pane_id.clone(),
                pid: evidence.map(|evidence| evidence.pid).or(pane.current.pid),
                start_ticks: evidence.map(|evidence| evidence.start_ticks),
                carried_since_ms,
            },
            evidence.is_some(),
        ));
    }

    let evidence = liveness_evidence(pane, prior_meta, bindings, read_start_ticks)?;
    Some((
        CarriedPane {
            pane_id: pane.pane_id.clone(),
            pid: Some(evidence.pid),
            start_ticks: Some(evidence.start_ticks),
            carried_since_ms,
        },
        true,
    ))
}

#[derive(Clone, Copy)]
struct LiveEvidence {
    pid: u32,
    start_ticks: u64,
}

fn liveness_evidence(
    pane: &PaneState,
    prior_meta: Option<&CarriedPane>,
    bindings: &HashMap<PaneId, PaneRootBinding>,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
) -> Option<LiveEvidence> {
    if let Some(meta) = prior_meta
        && let (Some(pid), Some(start_ticks)) = (meta.pid, meta.start_ticks)
        && read_start_ticks(pid) == Some(start_ticks)
    {
        return Some(LiveEvidence { pid, start_ticks });
    }
    if let Some(binding) = bindings.get(&pane.pane_id)
        && read_start_ticks(binding.pid) == Some(binding.start_ticks)
    {
        return Some(LiveEvidence {
            pid: binding.pid,
            start_ticks: binding.start_ticks,
        });
    }
    let pid = pane.current.pid?;
    let start_ticks = read_start_ticks(pid)?;
    Some(LiveEvidence { pid, start_ticks })
}

fn has_dead_liveness_evidence(
    pane: &PaneState,
    prior_meta: Option<&CarriedPane>,
    bindings: &HashMap<PaneId, PaneRootBinding>,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
) -> bool {
    if let Some(meta) = prior_meta
        && let (Some(pid), Some(start_ticks)) = (meta.pid, meta.start_ticks)
        && read_start_ticks(pid) != Some(start_ticks)
    {
        return true;
    }
    if let Some(binding) = bindings.get(&pane.pane_id)
        && read_start_ticks(binding.pid) != Some(binding.start_ticks)
    {
        return true;
    }
    pane.current
        .pid
        .is_some_and(|pid| read_start_ticks(pid).is_none())
}

// Only pid-reuse proof explains a missing pane strongly enough to publish the
// loss without the confirmation pull. A plain prior pid that cannot be read may
// be a real exit or a transient `/proc` miss; the carry path still refuses to
// ghost it, but the publish path verifies the omission once before committing.
fn has_authoritative_dead_liveness_evidence(
    pane: &PaneState,
    prior_meta: Option<&CarriedPane>,
    bindings: &HashMap<PaneId, PaneRootBinding>,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
) -> bool {
    if let Some(meta) = prior_meta
        && let (Some(pid), Some(start_ticks)) = (meta.pid, meta.start_ticks)
        && read_start_ticks(pid).is_some_and(|live| live != start_ticks)
    {
        return true;
    }
    if let Some(binding) = bindings.get(&pane.pane_id)
        && read_start_ticks(binding.pid).is_some_and(|live| live != binding.start_ticks)
    {
        return true;
    }
    false
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

fn expired_at(carried_since_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(carried_since_ms) > PANE_CARRY_TTL.as_millis() as u64
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
                active_pane: prior_tab.active_pane.clone(),
                focus_contested: prior_tab.focus_contested,
                panes: Vec::new(),
            });
            fresh.tabs.last_mut().expect("just pushed tab")
        }
    };
    if tab.active_pane.is_none() && prior_tab.active_pane.as_ref() == Some(&prior_pane.pane_id) {
        tab.active_pane = Some(prior_pane.pane_id.clone());
    }
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
    fn previous_carried_since_survives_until_ttl() {
        let mut prior = pidded_frame(&[("terminal_1", 101), ("terminal_2", 202)], 1);
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
            &|pid| (pid == 202).then_some(9),
            1 + PANE_CARRY_TTL.as_millis() as u64,
        );

        assert_eq!(outcome.carried[0].carried_since_ms, 1);
        assert!(outcome.expired.is_empty());
        assert!(!outcome.ambiguous_loss);
    }

    #[test]
    fn carried_pane_expires_after_ttl() {
        let mut prior = pidded_frame(&[("terminal_1", 101), ("terminal_2", 202)], 1);
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
            &|pid| (pid == 202).then_some(9),
            2 + PANE_CARRY_TTL.as_millis() as u64,
        );

        assert_eq!(outcome.frame.pane_states().count(), 1);
        assert!(outcome.carried.is_empty());
        assert_eq!(outcome.expired[0].pane_id, pane_id("terminal_2"));
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
            observed_at_ms: Some(1),
            build: None,
            session_name: "s".to_owned(),
            tabs: vec![TabFrame {
                view_id: ViewId::new_unchecked("tab_9"),
                kind: ViewKind::Tab,
                name: Some("work".to_owned()),
                active_pane: Some(pane_id("terminal_9")),
                focus_contested: false,
                panes: vec![PaneState {
                    pane_id: pane_id("terminal_9"),
                    first_seen_at_ms: Some(1),
                    is_floating: false,
                    current: PaneProcess {
                        pid: Some(909),
                        command: Some("zsh".to_owned()),
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
        assert_eq!(carried_tab.active_pane, Some(pane_id("terminal_9")));
    }
}
