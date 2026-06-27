use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::sidebar::timing::{
    OBSERVE_AGGREGATE_OSC_WINDOW, OBSERVE_ORDER_FLAP_WINDOW, OBSERVE_ROSTER_FLAP_WINDOW,
    OBSERVE_ROW_FLAP_WINDOW, OBSERVE_STATUS_CHURN_WINDOW, OBSERVE_VALUE_OSC_WINDOW, OBSERVE_WARMUP,
};

use super::sig::{FrameSig, RosterRowSig, RosterSig, RowSig, StatusCountSig};
use super::{AnomalyDraft, AnomalyKind, WatchedField, cap_vec};

#[derive(Debug, Default)]
pub struct Observer {
    first_frame_at_ms: Option<u64>,
    prev: Option<FrameSig>,
    roster_empty: Option<RosterEmpty>,
    presence: BTreeMap<String, RowPresence>,
    values: BTreeMap<(String, WatchedField), ValueRing>,
    aggregates: BTreeMap<String, AggregateRing>,
    orders: BTreeMap<String, ValueRing>,
    last_status: BTreeMap<String, String>,
    status_transitions: BTreeMap<String, VecDeque<u64>>,
    last_roster_rows: Vec<RosterRowSig>,
    last_roster_panes_produced_at_ms: Option<u64>,
    pending_roster: Option<RosterSig>,
    pub dropped_msgs: u32,
}

impl Observer {
    pub fn observe(&mut self, sig: FrameSig) -> Vec<AnomalyDraft> {
        if sig.panes_produced_at_ms.is_some() && self.first_frame_at_ms.is_none() {
            self.first_frame_at_ms = Some(sig.at_ms);
        }

        self.observe_roster_update(&sig);

        let mut drafts = Vec::new();
        self.detect_frame_checks(&sig, &mut drafts);

        if self.family_a_enabled(&sig) {
            self.detect_roster_flap(&sig, &mut drafts);
            self.detect_presence(&sig, &mut drafts);
            self.detect_values(&sig, &mut drafts);
            self.detect_status_churn(&sig, &mut drafts);
            self.detect_aggregates(&sig, &mut drafts);
            self.detect_order_flap(&sig, &mut drafts);
        } else {
            self.reset_transient_family_a_edges(&sig);
        }
        self.prune_frame_scoped_detector_state(&sig);

        self.prev = Some(sig);
        if let Some(first) = drafts.first_mut() {
            first.dropped_msgs = std::mem::take(&mut self.dropped_msgs);
        }
        drafts
    }

    pub fn pending_roster_update(&self) -> Option<RosterSig> {
        self.pending_roster.clone()
    }

    pub fn clear_roster_update(&mut self) {
        self.pending_roster = None;
    }

    fn observe_roster_update(&mut self, sig: &FrameSig) {
        let roster = RosterSig::from_frame(sig);
        if roster.rows == self.last_roster_rows
            && roster.panes_produced_at_ms == self.last_roster_panes_produced_at_ms
        {
            return;
        }
        // Writer-side cross-checks compare against this roster. Keep the latest
        // structural change pending until the render loop queues it, so a
        // throttled disappearance cannot leave the writer checking dead rows.
        self.last_roster_rows = roster.rows.clone();
        self.last_roster_panes_produced_at_ms = roster.panes_produced_at_ms;
        self.pending_roster = Some(roster);
    }

    fn family_a_enabled(&self, sig: &FrameSig) -> bool {
        let Some(first) = self.first_frame_at_ms else {
            return false;
        };
        sig.panes_produced_at_ms.is_some()
            && sig.at_ms.saturating_sub(first) >= millis(OBSERVE_WARMUP)
    }

    fn detect_frame_checks(&self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        self.detect_duplicate_rows(sig, drafts);
        self.detect_status_counts(sig, drafts);
        self.detect_own_view(sig, drafts);
        self.detect_subagents(sig, drafts);
        if sig.panes_produced_at_ms.is_none() && !sig.rows.is_empty() {
            drafts.push(AnomalyDraft::from_sig(
                sig,
                AnomalyKind::FramelessRows {
                    rows: cap_vec(sig.rows.iter().map(|row| row.row_id.clone())),
                },
                None,
            ));
        }
    }

    fn detect_duplicate_rows(&self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        let mut by_row = BTreeMap::<&str, usize>::new();
        let mut by_pane = BTreeMap::<&str, Vec<String>>::new();
        for row in &sig.rows {
            *by_row.entry(&row.row_id).or_default() += 1;
            if let Some(pane_id) = row.pane_id.as_deref() {
                by_pane.entry(pane_id).or_default().push(row.row_id.clone());
            }
        }
        for (row_id, count) in by_row {
            if count > 1 {
                drafts.push(AnomalyDraft::from_sig(
                    sig,
                    AnomalyKind::DuplicateRowId {
                        row_id: row_id.to_owned(),
                        count,
                    },
                    None,
                ));
            }
        }
        for (pane_id, row_ids) in by_pane {
            if row_ids.len() > 1 {
                drafts.push(AnomalyDraft::from_sig(
                    sig,
                    AnomalyKind::DuplicatePaneRows {
                        pane_id: pane_id.to_owned(),
                        row_ids: cap_vec(row_ids),
                    },
                    None,
                ));
            }
        }
    }

    fn detect_status_counts(&self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        let rows_by_group =
            sig.rows
                .iter()
                .fold(BTreeMap::<&str, Vec<&RowSig>>::new(), |mut out, row| {
                    out.entry(&row.group_key).or_default().push(row);
                    out
                });
        for group in &sig.groups {
            let mut tallied = BTreeMap::<String, usize>::new();
            for row in rows_by_group.get(group.key.as_str()).into_iter().flatten() {
                if let Some(status) = row.watched.status.as_ref() {
                    *tallied.entry(status.clone()).or_default() += 1;
                }
            }
            let tallied = tallied
                .into_iter()
                .map(|(status, count)| StatusCountSig { status, count })
                .collect::<Vec<_>>();
            let mut declared = group.status_counts.clone();
            declared.sort();
            let mut tallied_sorted = tallied;
            tallied_sorted.sort();
            let mismatch = if group.hidden_count > 0 {
                tallied_sorted.iter().any(|count| {
                    declared
                        .iter()
                        .find(|declared| declared.status == count.status)
                        .is_none_or(|declared| count.count > declared.count)
                })
            } else {
                declared != tallied_sorted
            };
            if mismatch {
                drafts.push(AnomalyDraft::from_sig(
                    sig,
                    AnomalyKind::StatusCountMismatch {
                        group_key: group.key.clone(),
                        declared,
                        tallied: tallied_sorted,
                    },
                    None,
                ));
            }
        }
    }

    fn detect_own_view(&self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        let Some(own_view) = sig.own_view.as_ref() else {
            return;
        };
        let Some(active) = own_view.active_pane_id.as_ref() else {
            return;
        };
        if !own_view.working_pane_ids.is_empty() && !own_view.working_pane_ids.contains(active) {
            drafts.push(AnomalyDraft::from_sig(
                sig,
                AnomalyKind::OwnViewIncoherent {
                    active_pane_id: active.clone(),
                    working_count: own_view.working_pane_ids.len(),
                },
                None,
            ));
        }
    }

    fn detect_subagents(&self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        let top_level = sig
            .rows
            .iter()
            .map(|row| row.row_id.as_str())
            .collect::<BTreeSet<_>>();
        let mut nested_seen = BTreeSet::<&str>::new();
        for row in &sig.rows {
            for child_id in &row.sub_agent_ids {
                if child_id == &row.row_id {
                    drafts.push(AnomalyDraft::from_sig(
                        sig,
                        AnomalyKind::SubagentTopLevelLeak {
                            agent_id: child_id.clone(),
                        },
                        None,
                    ));
                }
                if top_level.contains(child_id.as_str()) {
                    drafts.push(AnomalyDraft::from_sig(
                        sig,
                        AnomalyKind::SubagentDoubleRender {
                            id: child_id.clone(),
                        },
                        None,
                    ));
                }
                if !nested_seen.insert(child_id.as_str()) {
                    drafts.push(AnomalyDraft::from_sig(
                        sig,
                        AnomalyKind::SubagentDoubleRender {
                            id: child_id.clone(),
                        },
                        None,
                    ));
                }
            }
        }
    }

    fn detect_roster_flap(&mut self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        let window = millis(OBSERVE_ROSTER_FLAP_WINDOW);
        if let Some(empty) = self.roster_empty.take() {
            if sig.at_ms.saturating_sub(empty.empty_at_ms) <= window && !sig.rows.is_empty() {
                drafts.push(AnomalyDraft::from_sig(
                    sig,
                    AnomalyKind::RosterFlap {
                        rows_before: empty.rows_before,
                        empty_at_ms: empty.empty_at_ms,
                        restored_at_ms: sig.at_ms,
                        rows_after: sig.rows.len(),
                    },
                    Some(window),
                ));
            } else if sig.rows.is_empty() {
                self.roster_empty = Some(empty);
            }
        }

        let Some(prev) = self.prev.as_ref() else {
            return;
        };
        if sig.rows.is_empty()
            && !prev.rows.is_empty()
            && sig
                .own_view
                .as_ref()
                .is_some_and(|view| view.sibling_count > 0)
            && !pane_closed_covers_rows(&prev.rows, sig)
        {
            self.roster_empty = Some(RosterEmpty {
                rows_before: prev.rows.len(),
                empty_at_ms: sig.at_ms,
            });
        }
    }

    fn detect_presence(&mut self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        let window = millis(OBSERVE_ROW_FLAP_WINDOW);
        let present_ids = sig
            .rows
            .iter()
            .map(|row| row.row_id.as_str())
            .collect::<BTreeSet<_>>();
        let present_pane_ids = sig
            .rows
            .iter()
            .filter_map(|row| row.pane_id.as_deref())
            .collect::<BTreeSet<_>>();
        let hidden_by_group = hidden_by_group(sig);

        for row in &sig.rows {
            let hidden_now = hidden_by_group
                .get(row.group_key.as_str())
                .copied()
                .unwrap_or(false);
            match self.presence.get_mut(&row.row_id) {
                Some(presence) => {
                    if let Some(gone_at) = presence.gone_at.take()
                        && sig.at_ms.saturating_sub(gone_at) <= window
                        && !presence.closed_justified
                        && !presence.hidden_at_absence
                        && !hidden_now
                    {
                        drafts.push(AnomalyDraft::from_sig(
                            sig,
                            AnomalyKind::RowPresenceFlap {
                                row_id: row.row_id.clone(),
                                pane_id: row.pane_id.clone().or_else(|| presence.pane_id.clone()),
                                gone_at_ms: gone_at,
                                back_at_ms: sig.at_ms,
                            },
                            Some(window),
                        ));
                    }
                    presence.last_seen_at = sig.at_ms;
                    presence.pane_id = row.pane_id.clone();
                    presence.group_key = row.group_key.clone();
                    presence.hidden_last_seen = hidden_now;
                    presence.closed_justified = false;
                    presence.hidden_at_absence = false;
                }
                None => {
                    self.presence.insert(
                        row.row_id.clone(),
                        RowPresence {
                            born_at: sig.at_ms,
                            last_seen_at: sig.at_ms,
                            pane_id: row.pane_id.clone(),
                            group_key: row.group_key.clone(),
                            hidden_last_seen: hidden_now,
                            gone_at: None,
                            hidden_at_absence: false,
                            closed_justified: false,
                            short_lived_emitted: false,
                        },
                    );
                }
            }
        }

        let keys = self.presence.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            if present_ids.contains(key.as_str()) {
                continue;
            }
            let Some(presence) = self.presence.get_mut(&key) else {
                continue;
            };
            if presence.gone_at.is_some() {
                continue;
            }
            let hidden_now = hidden_by_group
                .get(presence.group_key.as_str())
                .copied()
                .unwrap_or(false);
            let closed = presence
                .pane_id
                .as_ref()
                .is_some_and(|pane_id| pane_closed(sig, pane_id));
            // The row id vanished while its pane still backs another row: the
            // pane was rebound to a new identity (e.g. a worktree group re-keys
            // from `branch:<name>` to its path as enumeration catches up), not
            // removed. `group_migration` records that transition; the pane never
            // blinked, so this is not a short-lived row.
            let rebound = presence
                .pane_id
                .as_deref()
                .is_some_and(|pane_id| present_pane_ids.contains(pane_id));
            if sig.at_ms.saturating_sub(presence.born_at) <= window
                && !presence.short_lived_emitted
                && !closed
                && !rebound
                && !presence.hidden_last_seen
                && !hidden_now
            {
                drafts.push(AnomalyDraft::from_sig(
                    sig,
                    AnomalyKind::ShortLivedRow {
                        row_id: key.clone(),
                        pane_id: presence.pane_id.clone(),
                        group_key: presence.group_key.clone(),
                        born_at_ms: presence.born_at,
                        gone_at_ms: sig.at_ms,
                    },
                    Some(window),
                ));
                presence.short_lived_emitted = true;
            }
            presence.gone_at = Some(sig.at_ms);
            presence.hidden_at_absence = hidden_now || presence.hidden_last_seen;
            presence.closed_justified = closed;
        }

        self.presence.retain(|_, presence| {
            presence
                .gone_at
                .is_none_or(|gone_at| sig.at_ms.saturating_sub(gone_at) <= window)
        });
        self.prune_historical_detector_state();
    }

    fn prune_historical_detector_state(&mut self) {
        let retained_rows = self
            .presence
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        self.values
            .retain(|(row_id, _), _| retained_rows.contains(row_id.as_str()));
        self.last_status
            .retain(|row_id, _| retained_rows.contains(row_id.as_str()));
        self.status_transitions
            .retain(|row_id, _| retained_rows.contains(row_id.as_str()));
    }

    fn detect_values(&mut self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        let window = millis(OBSERVE_VALUE_OSC_WINDOW);
        for row in &sig.rows {
            for (field, value) in row.watched.fields() {
                let ring = self.values.entry((row.row_id.clone(), field)).or_default();
                if let Some((_, last)) = ring.samples.back()
                    && last == &value
                {
                    continue;
                }
                ring.samples.push_back((sig.at_ms, value));
                while ring.samples.len() > 3 {
                    ring.samples.pop_front();
                }
                let Some((from, via, back)) = ring.oscillation(window) else {
                    continue;
                };
                drafts.push(AnomalyDraft::from_sig(
                    sig,
                    AnomalyKind::ValueOscillation {
                        row_id: row.row_id.clone(),
                        field,
                        from: value_label(&from.1),
                        via: value_label(&via.1),
                        span_ms: back.0.saturating_sub(from.0),
                    },
                    Some(window),
                ));
            }
        }
    }

    fn detect_status_churn(&mut self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        let window = millis(OBSERVE_STATUS_CHURN_WINDOW);
        for row in &sig.rows {
            let Some(status) = row.watched.status.as_ref() else {
                continue;
            };
            let previous = self.last_status.insert(row.row_id.clone(), status.clone());
            match previous {
                None => continue,
                Some(previous) if previous == *status => continue,
                Some(_) => {}
            }
            let transitions = self
                .status_transitions
                .entry(row.row_id.clone())
                .or_default();
            transitions.push_back(sig.at_ms);
            while transitions
                .front()
                .is_some_and(|at| sig.at_ms.saturating_sub(*at) > window)
            {
                transitions.pop_front();
            }
            if transitions.len() >= 4 {
                drafts.push(AnomalyDraft::from_sig(
                    sig,
                    AnomalyKind::StatusChurn {
                        row_id: row.row_id.clone(),
                        transitions: transitions.len(),
                        window_ms: window,
                    },
                    Some(window),
                ));
            }
        }
    }

    fn detect_aggregates(&mut self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        let window = millis(OBSERVE_AGGREGATE_OSC_WINDOW);
        for aggregate in &sig.aggregates {
            let identity = aggregate.key.identity();
            let ring = self.aggregates.entry(identity).or_default();
            if let Some(last) = ring.samples.back()
                && last.committed == aggregate.committed
            {
                continue;
            }
            ring.samples.push_back(AggregateSample {
                at_ms: sig.at_ms,
                committed: aggregate.committed.clone(),
                pulled: aggregate.pulled.clone(),
            });
            while ring.samples.len() > 3 {
                ring.samples.pop_front();
            }
            let Some((from, via, back)) = ring.oscillation(window) else {
                continue;
            };
            drafts.push(AnomalyDraft::from_sig(
                sig,
                AnomalyKind::AggregateOscillation {
                    aggregate: aggregate.key.clone(),
                    from: aggregate_label(&from.committed),
                    via: aggregate_label(&via.committed),
                    back: aggregate_label(&back.committed),
                    span_ms: back.at_ms.saturating_sub(from.at_ms),
                    pulled_via: Some(aggregate_label(&via.pulled)),
                },
                Some(window),
            ));
        }
    }

    fn detect_order_flap(&mut self, sig: &FrameSig, drafts: &mut Vec<AnomalyDraft>) {
        let window = millis(OBSERVE_ORDER_FLAP_WINDOW);
        for group in &sig.groups {
            let order = serialize_order(&group.render_order);
            let ring = self.orders.entry(group.key.clone()).or_default();
            if let Some((_, last)) = ring.samples.back()
                && last.as_deref() == Some(order.as_str())
            {
                continue;
            }
            ring.samples.push_back((sig.at_ms, Some(order)));
            while ring.samples.len() > 3 {
                ring.samples.pop_front();
            }
            let Some((from, via, back)) = ring.oscillation(window) else {
                continue;
            };
            let order = deserialize_order(&from.1);
            let via_order = deserialize_order(&via.1);
            if order_set(&order) != order_set(&via_order) {
                continue;
            }
            drafts.push(AnomalyDraft::from_sig(
                sig,
                AnomalyKind::OrderFlap {
                    group_key: group.key.clone(),
                    order,
                    via_order,
                    span_ms: back.0.saturating_sub(from.0),
                },
                Some(window),
            ));
        }
    }

    fn reset_transient_family_a_edges(&mut self, sig: &FrameSig) {
        self.roster_empty = None;
        for row in &sig.rows {
            self.presence
                .entry(row.row_id.clone())
                .or_insert(RowPresence {
                    born_at: sig.at_ms,
                    last_seen_at: sig.at_ms,
                    pane_id: row.pane_id.clone(),
                    group_key: row.group_key.clone(),
                    hidden_last_seen: false,
                    gone_at: None,
                    hidden_at_absence: false,
                    closed_justified: false,
                    short_lived_emitted: false,
                });
        }
    }

    fn prune_frame_scoped_detector_state(&mut self, sig: &FrameSig) {
        let aggregates = sig
            .aggregates
            .iter()
            .map(|aggregate| aggregate.key.identity())
            .collect::<BTreeSet<_>>();
        self.aggregates
            .retain(|identity, _| aggregates.contains(identity));
        let groups = sig
            .groups
            .iter()
            .map(|group| group.key.as_str())
            .collect::<BTreeSet<_>>();
        self.orders
            .retain(|group_key, _| groups.contains(group_key.as_str()));
    }
}

#[derive(Clone, Debug)]
struct RosterEmpty {
    rows_before: usize,
    empty_at_ms: u64,
}

#[derive(Clone, Debug)]
struct RowPresence {
    born_at: u64,
    last_seen_at: u64,
    pane_id: Option<String>,
    group_key: String,
    hidden_last_seen: bool,
    gone_at: Option<u64>,
    hidden_at_absence: bool,
    closed_justified: bool,
    short_lived_emitted: bool,
}

#[derive(Clone, Debug, Default)]
struct ValueRing {
    samples: VecDeque<(u64, Option<String>)>,
}

#[derive(Clone, Debug, Default)]
struct AggregateRing {
    samples: VecDeque<AggregateSample>,
}

#[derive(Clone, Debug)]
struct AggregateSample {
    at_ms: u64,
    committed: Option<String>,
    pulled: Option<String>,
}

type ValueSample = (u64, Option<String>);
type Oscillation<'a> = (&'a ValueSample, &'a ValueSample, &'a ValueSample);

impl ValueRing {
    fn oscillation(&self, window_ms: u64) -> Option<Oscillation<'_>> {
        if self.samples.len() != 3 {
            return None;
        }
        let from = &self.samples[0];
        let via = &self.samples[1];
        let back = &self.samples[2];
        if back.0.saturating_sub(from.0) > window_ms {
            return None;
        }
        match (&from.1, &via.1, &back.1) {
            (Some(from), via, Some(back))
                if from == back && via.as_deref() != Some(from.as_str()) =>
            {
                Some((&self.samples[0], &self.samples[1], &self.samples[2]))
            }
            _ => None,
        }
    }
}

type AggregateOscillation<'a> = (
    &'a AggregateSample,
    &'a AggregateSample,
    &'a AggregateSample,
);

impl AggregateRing {
    fn oscillation(&self, window_ms: u64) -> Option<AggregateOscillation<'_>> {
        if self.samples.len() != 3 {
            return None;
        }
        let from = &self.samples[0];
        let via = &self.samples[1];
        let back = &self.samples[2];
        if back.at_ms.saturating_sub(from.at_ms) > window_ms {
            return None;
        }
        match (&from.committed, &via.committed, &back.committed) {
            (Some(from), via, Some(back))
                if from == back && via.as_deref() != Some(from.as_str()) =>
            {
                Some((&self.samples[0], &self.samples[1], &self.samples[2]))
            }
            _ => None,
        }
    }
}

fn value_label(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "<none>".to_owned())
}

fn aggregate_label(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "0".to_owned())
}

fn serialize_order(order: &[String]) -> String {
    order.join("\u{1f}")
}

fn deserialize_order(value: &Option<String>) -> Vec<String> {
    value
        .as_deref()
        .map(|value| {
            if value.is_empty() {
                Vec::new()
            } else {
                value.split('\u{1f}').map(str::to_owned).collect()
            }
        })
        .unwrap_or_default()
}

fn order_set(order: &[String]) -> BTreeSet<&str> {
    order.iter().map(String::as_str).collect()
}

fn pane_closed_covers_rows(rows: &[RowSig], sig: &FrameSig) -> bool {
    let row_panes = rows
        .iter()
        .filter_map(|row| row.pane_id.as_deref())
        .collect::<Vec<_>>();
    !row_panes.is_empty() && row_panes.iter().all(|pane_id| pane_closed(sig, pane_id))
}

fn pane_closed(sig: &FrameSig, pane_id: &str) -> bool {
    sig.events
        .pane_closed
        .iter()
        .any(|event| event.pane_id == pane_id)
}

fn hidden_by_group(sig: &FrameSig) -> BTreeMap<&str, bool> {
    sig.groups
        .iter()
        .map(|group| (group.key.as_str(), group.hidden_count > 0))
        .collect()
}

fn millis(duration: std::time::Duration) -> u64 {
    duration.as_millis() as u64
}

#[cfg(test)]
mod tests;
