//! Card reductions over the acting view's retained roster and highlight.

use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::sidebar_pane::commands::{CardDirection, CardReply, CardRequest, CardTarget};
use crate::sidebar_pane::render::ViewState;
use crate::sidebar_pane::view::{BodyFilter, card_slots};
use crate::store::snapshot::SidebarSnapshot;

use super::selection::{
    InputEffect, InputOutcome, anchor_selection, begin_or_continue_browse, select_row_in,
    set_make_up_filter,
};

#[derive(Debug)]
pub(in crate::sidebar_pane) struct CardInput {
    pub(in crate::sidebar_pane) answer: std::sync::Mutex<Option<CardReply>>,
    pub(super) request: CardRequest,
    pub(super) reply: std::sync::mpsc::SyncSender<CardReply>,
    pub(super) expires: std::time::Instant,
    pub(super) view: super::views::ViewId,
}

impl PartialEq for CardInput {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for CardInput {}

impl CardInput {
    pub(super) fn check(
        &self,
        views: &super::views::Views,
        pane: &PaneId,
        now: std::time::Instant,
    ) -> Result<(), &'static str> {
        if now >= self.expires {
            return Err("request expired; try again");
        }
        let (view, destination) = views.command_target_for(&self.request)?;
        if view != self.view || destination != pane {
            return Err("view changed while the request was queued; try again");
        }
        Ok(())
    }
}

pub(super) fn reduce(
    request: &CardRequest,
    view: &mut ViewState,
    snapshot: &SidebarSnapshot,
) -> (InputOutcome, CardReply) {
    let mut outcome = InputOutcome::default();
    // A pane-named close is routed to any ready view, not the presser's, so
    // it leaves that view's help and search draft as they are.
    if !matches!(request, CardRequest::CloseTarget { pane: Some(_) }) {
        outcome.redraw = std::mem::take(&mut view.help_visible);
        if view.search_draft.take().is_some() {
            anchor_selection(view, snapshot);
            outcome.redraw = true;
        }
    }
    let target = match request {
        CardRequest::Walk {
            direction,
            worktree,
        } => {
            let roster = view.visible_roster(snapshot);
            let choices: Vec<_> = if *worktree {
                roster
                    .groups()
                    .iter()
                    .filter_map(|group| {
                        let range = group.range();
                        let first = range.clone().find(|index| {
                            roster.row(*index).is_some_and(|row| row.pane.is_some())
                        })?;
                        Some((range, first))
                    })
                    .collect()
            } else {
                roster
                    .rows()
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| row.pane.is_some())
                    .map(|(index, _)| (index..index + 1, index))
                    .collect()
            };
            if choices.is_empty() {
                return (outcome, request.refused("no visible cards"));
            }
            let selected = roster.rows().iter().position(|row| {
                row.pane
                    .as_ref()
                    .is_some_and(|pane| Some(&pane.pane_id) == view.selected_pane.as_ref())
            });
            let current = selected.and_then(|selected| {
                choices
                    .iter()
                    .position(|(range, _)| range.contains(&selected))
            });
            let next = match (current, direction) {
                (Some(current), CardDirection::Next) => (current + 1) % choices.len(),
                (Some(current), CardDirection::Prev) => {
                    (current + choices.len() - 1) % choices.len()
                }
                (None, CardDirection::Next) => 0,
                (None, CardDirection::Prev) => choices.len() - 1,
            };
            roster.pane_at_ordinal(choices[next].1)
        }
        CardRequest::Slot { n } => {
            if !(1..=9).contains(n) {
                return (outcome, request.refused("slot must be between 1 and 9"));
            }
            let slots = card_slots(snapshot);
            let Some(pane) = slots.get(usize::from(n - 1)) else {
                return (
                    outcome,
                    request.refused(format!("slot {n} is empty: {} cards", slots.len())),
                );
            };
            Some((*pane).clone())
        }
        CardRequest::Last => {
            let Some(previous) = &view.previous_card else {
                return (outcome, request.refused("no previous card"));
            };
            if !snapshot.rows().any(|row| {
                row.pane
                    .as_ref()
                    .is_some_and(|pane| pane.pane_id == *previous)
            }) {
                return (outcome, request.refused("previous card left the room"));
            }
            Some(previous.clone())
        }
        CardRequest::FilterNeedsYou { toggle } => {
            if !*toggle && view.make_up_filter.filter == Some(BodyFilter::NeedsYou) {
                return (outcome, request.refused("already showing needs-you"));
            }
            let mut lens = view.make_up_filter.clone();
            if *toggle && lens.filter == Some(BodyFilter::NeedsYou) {
                lens.filter = None;
            } else if BodyFilter::NeedsYou.total(&snapshot.worktree_groups) == 0 {
                return (outcome, request.refused("nothing needs you"));
            } else {
                lens.filter = Some(BodyFilter::NeedsYou);
            }
            if set_make_up_filter(view, snapshot, lens) {
                outcome.redraw = true;
                outcome
                    .effects
                    .push(InputEffect::SyncFilter(view.make_up_filter.clone()));
            }
            return (outcome, CardReply::Done);
        }
        CardRequest::CloseTarget { pane: named } => {
            let wanted = named.as_ref().or(view.selected_pane.as_ref());
            let Some((row, pane)) = snapshot.rows().find_map(|row| {
                let pane = &row.pane.as_ref()?.pane_id;
                (Some(pane) == wanted).then_some((row, pane))
            }) else {
                let reason = match named {
                    Some(pane) => format!("no card owns pane {pane}"),
                    None => "no highlighted card".to_owned(),
                };
                return (outcome, request.refused(reason));
            };
            let target = if row.as_agent().is_some() {
                CardTarget::Agent {
                    kind: AgentKind::new_unchecked(row.name.clone()),
                    agent_id: AgentSessionId::from(row.id.clone()),
                    pane: pane.clone(),
                }
            } else {
                CardTarget::Pane(pane.clone())
            };
            return (outcome, CardReply::CloseTarget { target });
        }
    };
    if let Some(pane) = target {
        let roster = view.visible_roster(snapshot);
        if let Some(index) = roster.rows().iter().position(|row| {
            row.pane
                .as_ref()
                .is_some_and(|candidate| candidate.pane_id == pane)
        }) {
            select_row_in(view, &roster, index);
        } else {
            view.selected_index = None;
            view.selected_pane = Some(pane.clone());
            view.entry_cursor = None;
        }
        begin_or_continue_browse(view);
        outcome.redraw = true;
        outcome.effects.push(InputEffect::Focus(pane));
    }
    (outcome, CardReply::Done)
}
