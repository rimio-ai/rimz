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

/// Carry omissions with liveness or own-pane evidence. A server listing refutes
/// missing siblings in tabs it lists; wholly omitted tabs retain carry protection.
pub(super) fn apply_carry_forward(
    mut fresh: PaneFrame,
    authoritative: bool,
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
    let fresh_views = fresh
        .tabs
        .iter()
        .map(|tab| tab.view_id.clone())
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
        .filter(|(tab, pane)| {
            !authoritative || !fresh_views.contains(&tab.view_id) || own_pane == Some(&pane.pane_id)
        })
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
    now_ms.saturating_sub(carried_since_ms) > timing::PANE_CARRY_TTL.as_millis() as u64
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
                naming: prior_tab.naming.clone(),
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
mod tests;
